//! Editor module — CRDT-based text editing with SequenceRga.
//!
//! Daemon owns the CRDT. TypeScript owns the view. Each open document gets
//! an `EditorSession` with one `SequenceRga` per digit+field pair. All edits
//! flow through the daemon so undo/redo, persistence, and (future) multiplayer
//! are handled in one place.
//!
//! Phase A: single-user MVP. Multiplayer (Phase C) adds Globe broadcast.

use std::collections::HashMap;
use std::sync::Arc;

use equipment::{CallDescriptor, EventDescriptor, ModuleCatalog, PhoneError};
use serde_json::{json, Value};
use uuid::Uuid;
use x::crdt::sequence::{SequenceOp, SequenceRga};

use crate::api_json;
use crate::daemon_module::DaemonModule;
use crate::state::DaemonState;

// ── Types ────────────────────────────────────────────────────────────

/// Key for looking up a specific RGA within a session: (digit_id, field_name).
type FieldKey = (Uuid, String);

/// A single undo/redo entry: the forward ops that were applied, plus the
/// inverse ops needed to reverse them.
#[derive(Debug, Clone)]
pub struct UndoEntry {
    /// Ops as originally applied (for redo).
    pub forward: Vec<SequenceOp>,
    /// Ops that reverse the forward ops (for undo).
    pub inverse: Vec<SequenceOp>,
}

/// In-memory editing session for one open .idea document.
pub struct EditorSession {
    /// One RGA per (digit_id, field_name) pair.
    pub fields: HashMap<FieldKey, SequenceRga>,
    /// Undo stack (most recent at the end).
    pub undo_stack: Vec<UndoEntry>,
    /// Redo stack (most recent at the end).
    pub redo_stack: Vec<UndoEntry>,
    /// Replica ID derived from Crown pubkey.
    pub replica_id: String,
    /// Monotonic version counter, incremented on every edit.
    pub version: u64,
    /// Whether any edits have been made since last save.
    pub dirty: bool,
}

impl EditorSession {
    /// Create a new session for the given replica.
    fn new(replica_id: String) -> Self {
        Self {
            fields: HashMap::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            replica_id,
            version: 0,
            dirty: false,
        }
    }
}

// ── Module ───────────────────────────────────────────────────────────

pub struct EditorModule;

fn err(op: &str, msg: impl ToString) -> PhoneError {
    PhoneError::HandlerFailed { call_id: op.into(), message: msg.to_string() }
}

fn ok_json(v: &Value) -> Result<Vec<u8>, PhoneError> {
    serde_json::to_vec(v).map_err(|e| err("serialize", e))
}

fn guard_vault_unlocked(state: &DaemonState) -> Result<(), PhoneError> {
    let vault = state.vault.lock().unwrap();
    if !vault.is_unlocked() {
        return Err(err("editor", "Vault is locked — unlock identity first"));
    }
    Ok(())
}

/// Extract a string text from a Digit's content field.
/// Returns empty string if content is not a string.
fn digit_text_content(digit: &ideas::Digit) -> String {
    digit.content.as_str().unwrap_or("").to_string()
}

/// Initialize a SequenceRga from existing text, inserting each char sequentially.
fn rga_from_text(replica_id: &str, text: &str) -> SequenceRga {
    let mut rga = SequenceRga::new(replica_id);
    for (i, ch) in text.chars().enumerate() {
        rga.insert_at(i, ch);
    }
    rga
}

impl DaemonModule for EditorModule {
    fn id(&self) -> &str { "editor" }
    fn name(&self) -> &str { "Editor (CRDT)" }
    fn deps(&self) -> &[&str] { &["crown", "vault", "ideas"] }

    fn register(&self, state: &Arc<DaemonState>) {
        // ── editor.open ──────────────────────────────────────────
        let s = state.clone();
        state.phone.register_raw("editor.open", move |data| {
            guard_vault_unlocked(&s)?;

            let params: Value = serde_json::from_slice(data).unwrap_or(Value::Null);
            let id_str = params.get("id").and_then(|v| v.as_str())
                .ok_or_else(|| err("editor.open", "missing 'id'"))?;
            let id = Uuid::parse_str(id_str)
                .map_err(|e| err("editor.open", format!("invalid UUID: {e}")))?;

            // Get replica_id from Crown pubkey
            let omnibus = s.omnibus.omnibus();
            let replica_id = omnibus.pubkey_hex()
                .ok_or_else(|| err("editor.open", "No identity — create one first"))?;

            // Load the .idea package from disk.
            // IMPORTANT: Release vault lock BEFORE acquiring sessions lock.
            // save_session locks sessions→vault, so we must not lock vault→sessions
            // (ABBA deadlock). Vault is only needed for key derivation + read.
            let (package, requested_field) = {
                let mut vault = s.vault.lock().unwrap();
                let entry = vault.get_idea(&id)
                    .map_err(|e| err("editor.open", e))?
                    .ok_or_else(|| err("editor.open", "idea not found"))?
                    .clone();

                let path = std::path::PathBuf::from(&entry.path);
                let content_key = vault.content_key(&id)
                    .map_err(|e| err("editor.open", e))?;
                let vocab_seed = vault.vocabulary_seed()
                    .map_err(|e| err("editor.open", e))?;
                let read_result = hall::scholar::read(
                    &path, content_key.expose(), Some(vocab_seed.expose()),
                ).map_err(|e| err("editor.open", e))?;

                let field = params.get("field").and_then(|v| v.as_str())
                    .unwrap_or("content").to_string();

                (read_result.value, field)
                // vault lock released here
            };

            // Build an EditorSession with one RGA per digit's text content
            let mut session = EditorSession::new(replica_id.clone());
            let mut fields_json = serde_json::Map::new();

            for (digit_id, digit) in &package.digits {
                let text = digit_text_content(digit);
                let field_name = requested_field.clone();
                let rga = rga_from_text(&replica_id, &text);

                fields_json.insert(
                    digit_id.to_string(),
                    json!({
                        "field": &field_name,
                        "text": &text,
                        "type": digit.digit_type(),
                    }),
                );

                session.fields.insert((*digit_id, field_name), rga);
            }

            // Store session (vault already released — no deadlock risk)
            let mut sessions = s.editor_sessions.lock().unwrap();
            sessions.insert(id, session);

            ok_json(&json!({
                "id": id_str,
                "fields": Value::Object(fields_json),
                "version": 0,
            }))
        });

        // ── editor.edit ──────────────────────────────────────────
        let s = state.clone();
        state.phone.register_raw("editor.edit", move |data| {
            let params: Value = serde_json::from_slice(data).unwrap_or(Value::Null);
            let id_str = params.get("id").and_then(|v| v.as_str())
                .ok_or_else(|| err("editor.edit", "missing 'id'"))?;
            let id = Uuid::parse_str(id_str)
                .map_err(|e| err("editor.edit", format!("invalid UUID: {e}")))?;

            let digit_id_str = params.get("digit_id").and_then(|v| v.as_str())
                .ok_or_else(|| err("editor.edit", "missing 'digit_id'"))?;
            let digit_id = Uuid::parse_str(digit_id_str)
                .map_err(|e| err("editor.edit", format!("invalid digit UUID: {e}")))?;

            let field = params.get("field").and_then(|v| v.as_str())
                .unwrap_or("content");
            let position = params.get("position").and_then(|v| v.as_u64())
                .ok_or_else(|| err("editor.edit", "missing 'position'"))? as usize;

            // Optional: chars to insert and count to delete
            let insert_text = params.get("insert").and_then(|v| v.as_str()).unwrap_or("");
            let delete_count = params.get("delete").and_then(|v| v.as_u64()).unwrap_or(0) as usize;

            let mut sessions = s.editor_sessions.lock().unwrap();
            let session = sessions.get_mut(&id)
                .ok_or_else(|| err("editor.edit", "no open session for this idea"))?;

            let key = (digit_id, field.to_string());
            let rga = session.fields.get_mut(&key)
                .ok_or_else(|| err("editor.edit", "field not found in session"))?;

            let mut forward_ops = Vec::new();
            let mut inverse_ops = Vec::new();

            // 1. Delete characters (from position, backwards so indices stay valid)
            for _i in 0..delete_count {
                // Capture the character for inverse (re-insert)
                if let Some(ch_id) = rga.position_to_id(position).cloned() {
                    let ch = rga.text().chars().nth(position).unwrap_or('\0');
                    if let Some(op) = rga.delete_at(position) {
                        // Inverse of delete = insert at the same position
                        // We record the insert_at position for the inverse
                        inverse_ops.push(SequenceOp::Insert {
                            id: ch_id,
                            value: ch,
                            after: if position == 0 {
                                None
                            } else {
                                rga.position_to_id(position - 1).cloned()
                            },
                        });
                        forward_ops.push(op);
                    }
                }
            }

            // 2. Insert characters
            for (i, ch) in insert_text.chars().enumerate() {
                let ins_pos = position + i;
                let op = rga.insert_at(ins_pos, ch);
                // Inverse of insert = delete by the ID we just created
                if let SequenceOp::Insert { ref id, .. } = op {
                    inverse_ops.push(SequenceOp::Delete { id: id.clone() });
                }
                forward_ops.push(op);
            }

            // 3. Record undo entry, clear redo stack
            if !forward_ops.is_empty() {
                // Inverse ops must be applied in reverse order
                inverse_ops.reverse();
                session.undo_stack.push(UndoEntry {
                    forward: forward_ops.clone(),
                    inverse: inverse_ops,
                });
                session.redo_stack.clear();
                session.version += 1;
                session.dirty = true;
            }

            let text = rga.text();
            let version = session.version;
            let can_undo = !session.undo_stack.is_empty();
            let can_redo = !session.redo_stack.is_empty();
            let ops_json = api_json::sequence_ops_json(&forward_ops);

            // Emit editor.changed event
            let event = serde_json::to_vec(&json!({
                "id": id_str,
                "digit_id": digit_id_str,
                "field": field,
                "version": version,
            })).unwrap_or_default();
            s.email.send_raw("editor.changed", &event);

            ok_json(&json!({
                "ops": ops_json,
                "text": text,
                "version": version,
                "can_undo": can_undo,
                "can_redo": can_redo,
            }))
        });

        // ── editor.undo ──────────────────────────────────────────
        let s = state.clone();
        state.phone.register_raw("editor.undo", move |data| {
            let params: Value = serde_json::from_slice(data).unwrap_or(Value::Null);
            let id_str = params.get("id").and_then(|v| v.as_str())
                .ok_or_else(|| err("editor.undo", "missing 'id'"))?;
            let id = Uuid::parse_str(id_str)
                .map_err(|e| err("editor.undo", format!("invalid UUID: {e}")))?;

            let mut sessions = s.editor_sessions.lock().unwrap();
            let session = sessions.get_mut(&id)
                .ok_or_else(|| err("editor.undo", "no open session for this idea"))?;

            let entry = session.undo_stack.pop()
                .ok_or_else(|| err("editor.undo", "nothing to undo"))?;

            // Apply inverse ops to all affected RGAs
            for op in &entry.inverse {
                // Find which field this op affects by checking all RGAs
                for rga in session.fields.values_mut() {
                    rga.apply(op);
                }
            }

            // Push to redo stack
            session.redo_stack.push(entry);
            session.version += 1;
            session.dirty = true;

            // Collect all field texts
            let fields_json = collect_field_texts(&session.fields);
            let can_undo = !session.undo_stack.is_empty();
            let can_redo = !session.redo_stack.is_empty();

            ok_json(&json!({
                "fields": Value::Object(fields_json),
                "version": session.version,
                "can_undo": can_undo,
                "can_redo": can_redo,
            }))
        });

        // ── editor.redo ──────────────────────────────────────────
        let s = state.clone();
        state.phone.register_raw("editor.redo", move |data| {
            let params: Value = serde_json::from_slice(data).unwrap_or(Value::Null);
            let id_str = params.get("id").and_then(|v| v.as_str())
                .ok_or_else(|| err("editor.redo", "missing 'id'"))?;
            let id = Uuid::parse_str(id_str)
                .map_err(|e| err("editor.redo", format!("invalid UUID: {e}")))?;

            let mut sessions = s.editor_sessions.lock().unwrap();
            let session = sessions.get_mut(&id)
                .ok_or_else(|| err("editor.redo", "no open session for this idea"))?;

            let entry = session.redo_stack.pop()
                .ok_or_else(|| err("editor.redo", "nothing to redo"))?;

            // Re-apply forward ops
            for op in &entry.forward {
                for rga in session.fields.values_mut() {
                    rga.apply(op);
                }
            }

            // Push back to undo stack
            session.undo_stack.push(entry);
            session.version += 1;
            session.dirty = true;

            let fields_json = collect_field_texts(&session.fields);
            let can_undo = !session.undo_stack.is_empty();
            let can_redo = !session.redo_stack.is_empty();

            ok_json(&json!({
                "fields": Value::Object(fields_json),
                "version": session.version,
                "can_undo": can_undo,
                "can_redo": can_redo,
            }))
        });

        // ── editor.format (stub for Phase A) ─────────────────────
        state.phone.register_raw("editor.format", move |data| {
            let params: Value = serde_json::from_slice(data).unwrap_or(Value::Null);
            let id_str = params.get("id").and_then(|v| v.as_str())
                .ok_or_else(|| err("editor.format", "missing 'id'"))?;

            // Phase A stub — formatting is Phase B
            ok_json(&json!({
                "id": id_str,
                "ok": true,
                "note": "formatting is Phase B — not yet implemented",
            }))
        });

        // ── editor.cursor (stub for Phase A) ─────────────────────
        state.phone.register_raw("editor.cursor", move |data| {
            let params: Value = serde_json::from_slice(data).unwrap_or(Value::Null);
            let id_str = params.get("id").and_then(|v| v.as_str())
                .ok_or_else(|| err("editor.cursor", "missing 'id'"))?;

            // Phase A stub — cursor sharing is Phase C
            ok_json(&json!({
                "id": id_str,
                "ok": true,
                "note": "cursor sharing is Phase C — not yet implemented",
            }))
        });

        // ── editor.save ──────────────────────────────────────────
        // Delegates to save_session (single implementation, correct lock ordering).
        let s = state.clone();
        state.phone.register_raw("editor.save", move |data| {
            let params: Value = serde_json::from_slice(data).unwrap_or(Value::Null);
            let id_str = params.get("id").and_then(|v| v.as_str())
                .ok_or_else(|| err("editor.save", "missing 'id'"))?;
            let id = Uuid::parse_str(id_str)
                .map_err(|e| err("editor.save", format!("invalid UUID: {e}")))?;

            // Check dirty flag before doing the full save
            {
                let sessions = s.editor_sessions.lock()
                    .unwrap_or_else(|e| e.into_inner());
                let is_dirty = sessions.get(&id).map(|s| s.dirty).unwrap_or(false);
                if !is_dirty {
                    return ok_json(&json!({ "ok": true, "saved": false }));
                }
            }

            save_session(&s, &id, id_str)?;
            ok_json(&json!({ "ok": true, "saved": true }))
        });

        // ── editor.close ─────────────────────────────────────────
        let s = state.clone();
        state.phone.register_raw("editor.close", move |data| {
            let params: Value = serde_json::from_slice(data).unwrap_or(Value::Null);
            let id_str = params.get("id").and_then(|v| v.as_str())
                .ok_or_else(|| err("editor.close", "missing 'id'"))?;
            let id = Uuid::parse_str(id_str)
                .map_err(|e| err("editor.close", format!("invalid UUID: {e}")))?;

            // Save if dirty before closing
            {
                let sessions = s.editor_sessions.lock()
                    .unwrap_or_else(|e| e.into_inner());
                let is_dirty = sessions.get(&id).map(|s| s.dirty).unwrap_or(false);
                if is_dirty {
                    drop(sessions);
                    save_session(&s, &id, id_str)?;
                }
            }

            // Remove session from memory
            let mut sessions = s.editor_sessions.lock()
                .unwrap_or_else(|e| e.into_inner());
            sessions.remove(&id);

            // Emit event
            let event = serde_json::to_vec(&json!({"id": id_str})).unwrap_or_default();
            s.email.send_raw("editor.closed", &event);

            ok_json(&json!({ "ok": true }))
        });

        // ── Auto-save loop ─────────────────────────────────────
        // Daemon owns persistence. Every 3 seconds, flush any dirty sessions
        // to disk via Hall. Emits editor.saved events so the UI can react.
        let s = state.clone();
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(std::time::Duration::from_secs(3));

                // Exit if daemon is shutting down
                if s.shutdown.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }

                // Collect dirty session IDs (lock briefly, then release)
                let dirty_ids: Vec<(Uuid, String)> = {
                    let sessions = s.editor_sessions.lock()
                        .unwrap_or_else(|e| e.into_inner());
                    sessions.iter()
                        .filter(|(_, session)| session.dirty)
                        .map(|(id, _)| (*id, id.to_string()))
                        .collect()
                };

                // Save each dirty session
                if !dirty_ids.is_empty() {
                    log::info!("editor auto-save: {} dirty session(s)", dirty_ids.len());
                }
                for (id, id_str) in &dirty_ids {
                    match save_session(&s, id, id_str) {
                        Ok(()) => {}
                        Err(e) => log::warn!("editor auto-save failed for {}: {:?}", id_str, e),
                    }
                }
            }
        });
    }

    fn catalog(&self) -> ModuleCatalog {
        ModuleCatalog::new()
            .with_call(CallDescriptor::new("editor.open", "Open a .idea for editing"))
            .with_call(CallDescriptor::new("editor.edit", "Insert/delete characters"))
            .with_call(CallDescriptor::new("editor.undo", "Undo last edit"))
            .with_call(CallDescriptor::new("editor.redo", "Redo last undone edit"))
            .with_call(CallDescriptor::new("editor.format", "Apply formatting (Phase B)"))
            .with_call(CallDescriptor::new("editor.cursor", "Update cursor position (Phase C)"))
            .with_call(CallDescriptor::new("editor.save", "Save RGA state to .idea"))
            .with_call(CallDescriptor::new("editor.close", "Save and close editor session"))
            .with_emitted_event(EventDescriptor::new("editor.changed", "Editor content changed"))
            .with_emitted_event(EventDescriptor::new("editor.saved", "Editor saved to disk"))
            .with_emitted_event(EventDescriptor::new("editor.closed", "Editor session closed"))
    }
}

// ── Helpers ──────────────────────────────────────────────────────────

/// Collect all field texts from the session into a JSON map.
fn collect_field_texts(fields: &HashMap<FieldKey, SequenceRga>) -> serde_json::Map<String, Value> {
    let mut result = serde_json::Map::new();
    for ((digit_id, field_name), rga) in fields {
        let key = format!("{}:{}", digit_id, field_name);
        result.insert(key, json!({
            "digit_id": digit_id.to_string(),
            "field": field_name,
            "text": rga.text(),
        }));
    }
    result
}

/// Save a session's RGA state to disk (load-merge-write). Extracted for
/// reuse by both editor.save, editor.close, and the auto-save loop.
///
/// Lock order: sessions → vault (same as editor.save RPC handler).
/// editor.open releases vault before acquiring sessions to avoid ABBA deadlock.
fn save_session(state: &DaemonState, id: &Uuid, id_str: &str) -> Result<(), PhoneError> {
    guard_vault_unlocked(state)?;

    // Use unwrap_or_else to survive poisoned mutex (matches auto-save loop pattern).
    // A poisoned mutex means a handler panicked while holding the lock — the data
    // inside may be inconsistent, but crashing the auto-save thread is worse.
    let mut sessions = state.editor_sessions.lock()
        .unwrap_or_else(|e| e.into_inner());
    let session = sessions.get_mut(id)
        .ok_or_else(|| err("editor.save", "no open session for this idea"))?;

    if !session.dirty {
        return Ok(());
    }

    log::info!("editor.save: saving session {} (version {})", id_str, session.version);

    // Snapshot RGA text while holding sessions lock, then release it.
    // This minimizes the time we hold both locks and lets edits continue
    // while we do disk I/O.
    // Snapshot ALL field RGA texts — the field_name ("body", "content", etc.)
    // is whatever the TypeScript requested when opening the session. Every
    // field maps to digit.content regardless of its logical name.
    let field_texts: Vec<(Uuid, String, String)> = session.fields.iter()
        .map(|((digit_id, field_name), rga)| (*digit_id, field_name.clone(), rga.text()))
        .collect();

    let field_count = field_texts.len();
    // Release sessions lock before acquiring vault (consistent with editor.open)
    drop(sessions);

    let mut vault = state.vault.lock().unwrap();
    let entry = vault.get_idea(id)
        .map_err(|e| err("editor.save", e))?
        .ok_or_else(|| err("editor.save", "idea not found"))?
        .clone();

    let path = std::path::PathBuf::from(&entry.path);
    let content_key = vault.content_key(id)
        .map_err(|e| err("editor.save", e))?;
    let vocab_seed = vault.vocabulary_seed()
        .map_err(|e| err("editor.save", e))?;

    log::info!("editor.save: reading package from {}", path.display());

    let read_result = hall::scholar::read(
        &path, content_key.expose(), Some(vocab_seed.expose()),
    ).map_err(|e| err("editor.save", e))?;

    if read_result.has_warnings() {
        for w in &read_result.warnings {
            log::warn!("editor.save: Hall read warning for {}: {}", id_str, w);
        }
    }

    let mut package = read_result.value;
    let disk_digit_count = package.digits.len();

    // Update each digit's content from the RGA text snapshot
    let mut updated_count = 0usize;
    for (digit_id, _field_name, text) in &field_texts {
        if let Some(digit) = package.digits.get_mut(digit_id) {
            digit.content = x::Value::String(text.clone());
            digit.modified = chrono::Utc::now();
            updated_count += 1;
        } else {
            log::warn!(
                "editor.save: digit {} not found in package on disk (session has it, disk doesn't)",
                digit_id
            );
        }
    }

    log::info!(
        "editor.save: updated {}/{} digits ({} on disk) for {}",
        updated_count, field_count, disk_digit_count, id_str
    );

    if updated_count == 0 && field_count > 0 {
        log::warn!(
            "editor.save: NO digits updated for {} — RGA fields exist but no matching digits on disk!",
            id_str
        );
    }

    package.header.modified = chrono::Utc::now();
    package.header.babel.enabled = true;
    package.header.babel.vocabulary_seed = Some("vault-derived".to_string());

    let bytes = hall::scribe::write(&package, content_key.expose(), Some(vocab_seed.expose()))
        .map_err(|e| err("editor.save", e))?;

    log::info!("editor.save: wrote {} bytes to {}", bytes, path.display());

    let root_id = package.header.content.root_digit_id;
    let mut updated_entry = entry;
    updated_entry.modified_at = chrono::Utc::now();
    if let Some(root) = package.digits.get(&root_id) {
        if let Some(title_val) = root.properties.get("title") {
            if let Some(title_str) = title_val.as_str() {
                updated_entry.title = Some(title_str.to_string());
            }
        }
    }
    vault.register_idea(updated_entry)
        .map_err(|e| err("editor.save", e))?;

    // Re-acquire sessions to clear dirty flag
    let mut sessions = state.editor_sessions.lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(session) = sessions.get_mut(id) {
        session.dirty = false;
    }

    let event = serde_json::to_vec(&json!({"id": id_str})).unwrap_or_default();
    state.email.send_raw("editor.saved", &event);

    log::info!("editor.save: session {} saved successfully", id_str);

    Ok(())
}
