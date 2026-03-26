//! Ideas module — content CRUD.
//!
//! Overrides auto-generated FFI handlers for idea operations that
//! compose Ideas + Vault + Hall across multiple crates.
//! All operations go Rust → Rust with zero FFI boundaries.
//!
//! Uses `crate::api_json` for all outbound serialization (x::Value unwrapping,
//! header flattening, field renaming). Inbound save uses load-merge-write:
//! loads the existing package, applies TS changes, writes back via Hall.

use std::collections::HashSet;
use std::sync::Arc;

use equipment::{CallDescriptor, EventDescriptor, ModuleCatalog, PhoneError};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::api_json;
use crate::daemon_module::DaemonModule;
use crate::modifiers;
use crate::state::DaemonState;

pub struct IdeasModule;

fn err(op: &str, msg: impl ToString) -> PhoneError {
    PhoneError::HandlerFailed { call_id: op.into(), message: msg.to_string() }
}

fn ok_json(v: &Value) -> Result<Vec<u8>, PhoneError> {
    serde_json::to_vec(v).map_err(|e| err("serialize", e))
}

fn guard_vault_unlocked(state: &DaemonState) -> Result<(), PhoneError> {
    let vault = state.vault.lock().unwrap();
    if !vault.is_unlocked() {
        return Err(err("idea", "Vault is locked — unlock identity first"));
    }
    Ok(())
}

impl DaemonModule for IdeasModule {
    fn id(&self) -> &str { "ideas" }
    fn name(&self) -> &str { "Ideas Content" }
    fn deps(&self) -> &[&str] { &["crown", "vault"] }

    fn register(&self, state: &Arc<DaemonState>) {
        // ── idea.create ─────────────────────────────────────────
        let s = state.clone();
        state.phone.register_raw("idea.create", move |data| {
            modifiers::polity_check("idea.create")?;
            guard_vault_unlocked(&s)?;

            let params: Value = serde_json::from_slice(data).unwrap_or(Value::Null);
            let digit_type = params.get("type").and_then(|v| v.as_str()).unwrap_or("text");
            let title = params.get("title").and_then(|v| v.as_str()).unwrap_or("Untitled");
            let content_str = params.get("content").and_then(|v| v.as_str()).unwrap_or("");

            // Get author pubkey from Omnibus
            let omnibus = s.omnibus.omnibus();
            let author = omnibus.pubkey_hex()
                .ok_or_else(|| err("idea.create", "No identity — create one first"))?;

            // Create root digit
            let content_value = x::Value::String(content_str.to_string());
            let root_digit = ideas::Digit::new(digit_type.to_string(), content_value, author.clone())
                .map_err(|e| err("idea.create", e))?;

            // Create header with internal key slot (local encryption)
            let key_slot = ideas::header::KeySlot::Internal(ideas::header::InternalKeySlot {
                key_id: "local".to_string(),
                wrapped_key: String::new(),
            });
            let mut header = ideas::Header::create(
                author.clone(),
                String::new(), // signature (empty for local)
                root_digit.id(),
                key_slot,
            );

            // Mark Babel as enabled in the header
            header.babel.enabled = true;
            header.babel.vocabulary_seed = Some("vault-derived".to_string());

            // Build package path
            let mut vault = s.vault.lock().unwrap();
            let personal = vault.personal_path().map_err(|e| err("idea.create", e))?;
            std::fs::create_dir_all(&personal).ok();
            let idea_path = personal.join(format!("{}.idea", header.id));

            // Create package
            let root_id = header.content.root_digit_id;
            let package = ideas::IdeaPackage::new(idea_path.clone(), header.clone(), root_digit);

            // Get content key + vocab seed from Vault and write encrypted via Hall
            let content_key = vault.content_key(&header.id)
                .map_err(|e| err("idea.create", e))?;
            let vocab_seed = vault.vocabulary_seed()
                .map_err(|e| err("idea.create", e))?;
            hall::scribe::write(&package, content_key.expose(), Some(vocab_seed.expose()))
                .map_err(|e| err("idea.create", e))?;

            // Register in manifest
            let mut entry = vault::ManifestEntry::from_header(&header, idea_path.to_string_lossy().to_string());
            entry.title = Some(title.to_string());
            entry.extended_type = Some(digit_type.to_string());
            vault.register_idea(entry.clone())
                .map_err(|e| err("idea.create", e))?;

            // Post-modifier: emit event
            let result = api_json::manifest_entry_json(&entry, Some(&root_id));
            let result_bytes = serde_json::to_vec(&result).unwrap_or_default();
            s.email.send_raw("idea.created", &result_bytes);

            ok_json(&result)
        });

        // ── idea.list ───────────────────────────────────────────
        let s = state.clone();
        state.phone.register_raw("idea.list", move |data| {
            guard_vault_unlocked(&s)?;

            let params: Value = serde_json::from_slice(data).unwrap_or(Value::Null);

            let mut filter = vault::IdeaFilter::new();
            if let Some(et) = params.get("extended_type").and_then(|v| v.as_str()) {
                filter = filter.extended_type(et);
            }
            if let Some(tc) = params.get("title_contains").and_then(|v| v.as_str()) {
                filter = filter.title_contains(tc);
            }
            if let Some(cr) = params.get("creator").and_then(|v| v.as_str()) {
                filter = filter.creator(cr);
            }

            let vault = s.vault.lock().unwrap();
            let entries = vault.list_ideas(&filter)
                .map_err(|e| err("idea.list", e))?;

            let entries_json: Vec<Value> = entries.iter()
                .map(|e| api_json::manifest_entry_json(e, None))
                .collect();

            ok_json(&Value::Array(entries_json))
        });

        // ── idea.load ───────────────────────────────────────────
        let s = state.clone();
        state.phone.register_raw("idea.load", move |data| {
            guard_vault_unlocked(&s)?;

            let params: Value = serde_json::from_slice(data).unwrap_or(Value::Null);
            let id_str = params.get("id").and_then(|v| v.as_str())
                .ok_or_else(|| err("idea.load", "missing 'id'"))?;
            let id = Uuid::parse_str(id_str)
                .map_err(|e| err("idea.load", format!("invalid UUID: {e}")))?;

            let mut vault = s.vault.lock().unwrap();
            let entry = vault.get_idea(&id)
                .map_err(|e| err("idea.load", e))?
                .ok_or_else(|| err("idea.load", "idea not found"))?;

            let path = std::path::PathBuf::from(&entry.path);
            let content_key = vault.content_key(&id)
                .map_err(|e| err("idea.load", e))?;

            let vocab_seed = vault.vocabulary_seed()
                .map_err(|e| err("idea.load", e))?;
            let read_result = hall::scholar::read(&path, content_key.expose(), Some(vocab_seed.expose()))
                .map_err(|e| err("idea.load", e))?;

            // Hand-built JSON: flattens header, unwraps x::Value
            let package_json = api_json::idea_package_json(&read_result.value);
            ok_json(&package_json)
        });

        // ── idea.save ───────────────────────────────────────────
        //
        // Load-merge-write pattern: loads the existing package from disk,
        // applies changes from the TS client, writes back via Hall, and
        // updates the manifest. Never deserializes the TS package directly
        // into IdeaPackage (format mismatch: x::Value wrapping, missing
        // fields, private Digit fields).
        let s = state.clone();
        state.phone.register_raw("idea.save", move |data| {
            modifiers::polity_check("idea.save")?;
            guard_vault_unlocked(&s)?;

            let params: Value = serde_json::from_slice(data)
                .map_err(|e| err("idea.save", e))?;
            let id_str = params.get("id").and_then(|v| v.as_str())
                .ok_or_else(|| err("idea.save", "missing 'id'"))?;
            let id = Uuid::parse_str(id_str)
                .map_err(|e| err("idea.save", format!("invalid UUID: {e}")))?;

            let incoming_pkg = params.get("package")
                .ok_or_else(|| err("idea.save", "missing 'package'"))?;

            let mut vault = s.vault.lock().unwrap();

            // Clone entry to release the immutable borrow (we need &mut later)
            let entry = vault.get_idea(&id)
                .map_err(|e| err("idea.save", e))?
                .ok_or_else(|| err("idea.save", "idea not found"))?
                .clone();

            let path = std::path::PathBuf::from(&entry.path);
            let content_key = vault.content_key(&id)
                .map_err(|e| err("idea.save", e))?;

            // Load existing package from disk
            let vocab_seed = vault.vocabulary_seed()
                .map_err(|e| err("idea.save", e))?;
            let read_result = hall::scholar::read(&path, content_key.expose(), Some(vocab_seed.expose()))
                .map_err(|e| err("idea.save", e))?;
            let mut package = read_result.value;

            // Merge incoming digits
            if let Some(digits_obj) = incoming_pkg.get("digits").and_then(|d| d.as_object()) {
                let now = chrono::Utc::now();
                let author = package.header.creator.public_key.clone();

                for (digit_id_str, digit_json) in digits_obj {
                    let digit_id = Uuid::parse_str(digit_id_str)
                        .map_err(|e| err("idea.save", format!("invalid digit UUID: {e}")))?;

                    let content = digit_json.get("content")
                        .map(api_json::json_to_x)
                        .unwrap_or(x::Value::String(String::new()));
                    let properties = digit_json.get("properties")
                        .and_then(|p| p.as_object())
                        .map(api_json::json_to_x_props)
                        .unwrap_or_default();
                    let children = digit_json.get("children")
                        .and_then(|c| c.as_array())
                        .map(|arr| arr.iter()
                            .filter_map(|v| v.as_str().and_then(|s| Uuid::parse_str(s).ok()))
                            .collect::<Vec<_>>());

                    if let Some(existing) = package.digits.get_mut(&digit_id) {
                        // Update existing digit's pub fields
                        existing.content = content;
                        existing.properties = properties;
                        existing.children = children;
                        existing.modified = now;
                    } else {
                        // New digit — construct via serde round-trip (id is private)
                        let dtype = digit_json.get("type").and_then(|v| v.as_str())
                            .unwrap_or("paragraph");
                        let digit_serde = json!({
                            "id": digit_id_str,
                            "type": dtype,
                            "content": serde_json::to_value(&content).unwrap_or(json!({"string": ""})),
                            "properties": serde_json::to_value(&properties).unwrap_or(json!({})),
                            "children": children.as_ref().map(|c|
                                c.iter().map(|u| u.to_string()).collect::<Vec<_>>()),
                            "created": now.to_rfc3339(),
                            "modified": now.to_rfc3339(),
                            "author": &author,
                            "vector": {},
                            "tombstone": false,
                        });
                        let new_digit: ideas::Digit = serde_json::from_value(digit_serde)
                            .map_err(|e| err("idea.save", format!("failed to create digit: {e}")))?;
                        package.digits.insert(digit_id, new_digit);
                    }
                }

                // Remove digits the client deleted (not in incoming set)
                let incoming_ids: HashSet<Uuid> = digits_obj.keys()
                    .filter_map(|k| Uuid::parse_str(k).ok())
                    .collect();
                let root_id = package.header.content.root_digit_id;
                package.digits.retain(|id, _| incoming_ids.contains(id) || *id == root_id);
            }

            // Update header modified time + ensure Babel is enabled
            package.header.modified = chrono::Utc::now();
            package.header.babel.enabled = true;
            package.header.babel.vocabulary_seed = Some("vault-derived".to_string());

            // Write back to disk via Hall (Babel + AES-256-GCM)
            hall::scribe::write(&package, content_key.expose(), Some(vocab_seed.expose()))
                .map_err(|e| err("idea.save", e))?;

            // Update manifest entry (title from root digit properties, modified time)
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
                .map_err(|e| err("idea.save", e))?;

            // Post-modifier: emit event
            let event = serde_json::to_vec(&json!({"id": id_str})).unwrap_or_default();
            s.email.send_raw("idea.saved", &event);

            ok_json(&json!({ "ok": true }))
        });

        // ── idea.delete ─────────────────────────────────────────
        let s = state.clone();
        state.phone.register_raw("idea.delete", move |data| {
            modifiers::polity_check("idea.delete")?;
            guard_vault_unlocked(&s)?;

            let params: Value = serde_json::from_slice(data).unwrap_or(Value::Null);
            let id_str = params.get("id").and_then(|v| v.as_str())
                .ok_or_else(|| err("idea.delete", "missing 'id'"))?;
            let id = Uuid::parse_str(id_str)
                .map_err(|e| err("idea.delete", format!("invalid UUID: {e}")))?;

            let mut vault = s.vault.lock().unwrap();

            // Get path before removing from manifest
            if let Ok(Some(entry)) = vault.get_idea(&id) {
                let path = std::path::PathBuf::from(&entry.path);
                if path.exists() {
                    std::fs::remove_dir_all(&path).ok();
                }
            }

            vault.unregister_idea(&id)
                .map_err(|e| err("idea.delete", e))?;

            // Post-modifier: emit event
            let event = serde_json::to_vec(&json!({"id": id_str})).unwrap_or_default();
            s.email.send_raw("idea.deleted", &event);

            ok_json(&json!({ "ok": true }))
        });

        // ── idea.search ─────────────────────────────────────────
        let s = state.clone();
        state.phone.register_raw("idea.search", move |data| {
            guard_vault_unlocked(&s)?;

            let params: Value = serde_json::from_slice(data).unwrap_or(Value::Null);
            let query = params.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;

            let vault = s.vault.lock().unwrap();
            let hits = vault.search(query, limit)
                .map_err(|e| err("idea.search", e))?;

            let hits_json: Vec<Value> = hits.iter()
                .map(|h| api_json::search_hit_json(h))
                .collect();

            ok_json(&Value::Array(hits_json))
        });
    }

    fn catalog(&self) -> ModuleCatalog {
        ModuleCatalog::new()
            .with_call(CallDescriptor::new("idea.create", "Create a new .idea"))
            .with_call(CallDescriptor::new("idea.list", "List ideas (filtered)"))
            .with_call(CallDescriptor::new("idea.load", "Load an .idea package"))
            .with_call(CallDescriptor::new("idea.save", "Save an .idea package"))
            .with_call(CallDescriptor::new("idea.delete", "Delete an .idea"))
            .with_call(CallDescriptor::new("idea.search", "Full-text search"))
            .with_emitted_event(EventDescriptor::new("idea.created", "Idea was created"))
            .with_emitted_event(EventDescriptor::new("idea.saved", "Idea was saved"))
            .with_emitted_event(EventDescriptor::new("idea.deleted", "Idea was deleted"))
    }
}
