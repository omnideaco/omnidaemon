//! Crown module — identity lifecycle.
//!
//! Overrides auto-generated FFI handlers for crown operations that
//! compose Omnibus + Vault across crate boundaries.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use equipment::{CallDescriptor, EventDescriptor, ModuleCatalog, PhoneError};
use serde_json::{json, Value};

use crate::daemon_module::DaemonModule;
use crate::modifiers;
use crate::state::{DaemonState, ensure_vault_unlocked};

pub struct CrownModule;

fn err(op: &str, msg: impl ToString) -> PhoneError {
    PhoneError::HandlerFailed { call_id: op.into(), message: msg.to_string() }
}

fn ok_json(v: &Value) -> Result<Vec<u8>, PhoneError> {
    serde_json::to_vec(v).map_err(|e| PhoneError::HandlerFailed {
        call_id: "serialize".into(), message: e.to_string(),
    })
}

impl DaemonModule for CrownModule {
    fn id(&self) -> &str { "crown" }
    fn name(&self) -> &str { "Crown Identity" }
    fn deps(&self) -> &[&str] { &["omnibus"] }

    fn register(&self, state: &Arc<DaemonState>) {
        // ── crown.state ─────────────────────────────────────────
        let s = state.clone();
        state.phone.register_raw("crown.state", move |_data| {
            let omnibus = s.omnibus.omnibus();
            let has_identity = omnibus.pubkey().is_some();
            let unlocked = !s.crown_locked.load(Ordering::Relaxed);
            ok_json(&json!({
                "exists": has_identity,
                "unlocked": has_identity && unlocked,
                "crown_id": omnibus.pubkey(),
                "display_name": omnibus.profile_json().and_then(|j| {
                    serde_json::from_str::<Value>(&j).ok()?.get("display_name")?.as_str().map(String::from)
                }),
                "online": has_identity && unlocked,
                "has_avatar": false,
            }))
        });

        // ── crown.create ────────────────────────────────────────
        let s = state.clone();
        state.phone.register_raw("crown.create", move |data| {
            let params: Value = serde_json::from_slice(data).unwrap_or(Value::Null);
            let name = params.get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("Anonymous");

            let omnibus = s.omnibus.omnibus();
            let crown_id = omnibus.create_identity(name)
                .map_err(|e| err("crown.create", e))?;

            s.crown_locked.store(false, Ordering::Relaxed);
            ensure_vault_unlocked(&s);

            // Post-modifier: emit event for Yoke + Pager observers
            let event = serde_json::to_vec(&json!({"crown_id": &crown_id})).unwrap_or_default();
            s.email.send_raw("crown.created", &event);

            ok_json(&json!({ "crown_id": crown_id }))
        });

        // ── crown.unlock ────────────────────────────────────────
        let s = state.clone();
        state.phone.register_raw("crown.unlock", move |_data| {
            let omnibus = s.omnibus.omnibus();
            if omnibus.pubkey().is_none() {
                return Err(err("crown.unlock", "No identity to unlock"));
            }
            s.crown_locked.store(false, Ordering::Relaxed);
            ensure_vault_unlocked(&s);

            s.email.send_raw("crown.unlocked", b"{}");
            ok_json(&json!({ "ok": true, "unlocked": true }))
        });

        // ── crown.lock ──────────────────────────────────────────
        let s = state.clone();
        state.phone.register_raw("crown.lock", move |_data| {
            s.crown_locked.store(true, Ordering::Relaxed);
            s.email.send_raw("crown.locked", b"{}");
            ok_json(&json!({ "ok": true, "locked": true }))
        });

        // ── crown.profile ───────────────────────────────────────
        let s = state.clone();
        state.phone.register_raw("crown.profile", move |_data| {
            if s.crown_locked.load(Ordering::Relaxed) {
                return Err(err("crown.profile", "Crown is locked"));
            }
            let omnibus = s.omnibus.omnibus();
            match omnibus.profile_json() {
                Some(json_str) => {
                    let v: Value = serde_json::from_str(&json_str)
                        .map_err(|e| err("crown.profile", e))?;
                    ok_json(&v)
                }
                None => ok_json(&Value::Null),
            }
        });

        // ── crown.update_profile ────────────────────────────────
        let s = state.clone();
        state.phone.register_raw("crown.update_profile", move |data| {
            if s.crown_locked.load(Ordering::Relaxed) {
                return Err(err("crown.update_profile", "Crown is locked"));
            }
            let params: Value = serde_json::from_slice(data).unwrap_or(Value::Null);
            let name = params.get("display_name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let omnibus = s.omnibus.omnibus();
            omnibus.update_display_name(name)
                .map_err(|e| err("crown.update_profile", e))?;
            ok_json(&json!({ "ok": true }))
        });

        // ── crown.set_status ────────────────────────────────────
        let s = state.clone();
        state.phone.register_raw("crown.set_status", move |data| {
            if s.crown_locked.load(Ordering::Relaxed) {
                return Err(err("crown.set_status", "Crown is locked"));
            }
            let params: Value = serde_json::from_slice(data).unwrap_or(Value::Null);
            let online = params.get("online").and_then(|v| v.as_bool()).unwrap_or(true);
            ok_json(&json!({ "ok": true, "online": online }))
        });

        // ── crown.import ────────────────────────────────────────
        state.phone.register_raw("crown.import", |_data| {
            Err(err("crown.import", "Import not yet implemented"))
        });

        // ── crown.delete ────────────────────────────────────────
        let s = state.clone();
        state.phone.register_raw("crown.delete", move |_data| {
            modifiers::polity_check("crown.delete")?;
            if s.crown_locked.load(Ordering::Relaxed) {
                return Err(err("crown.delete", "Crown is locked"));
            }
            Err(err("crown.delete", "Delete not yet implemented"))
        });

        // ── crown.avatar ────────────────────────────────────────
        state.phone.register_raw("crown.avatar", |_data| {
            ok_json(&Value::Null)
        });

        // ── identity.* aliases (backward compat) ────────────────
        let s = state.clone();
        state.phone.register_raw("identity.create", move |data| {
            s.phone.call_raw("crown.create", data)
        });

        let s = state.clone();
        state.phone.register_raw("identity.load", move |data| {
            let params: Value = serde_json::from_slice(data).unwrap_or(Value::Null);
            let path = params.get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| err("identity.load", "missing 'path'"))?;
            let omnibus = s.omnibus.omnibus();
            let crown_id = omnibus.load_identity(path)
                .map_err(|e| err("identity.load", e))?;
            ensure_vault_unlocked(&s);
            ok_json(&json!({ "crown_id": crown_id }))
        });

        let s = state.clone();
        state.phone.register_raw("identity.profile", move |data| {
            s.phone.call_raw("crown.profile", data)
        });

        let s = state.clone();
        state.phone.register_raw("identity.pubkey", move |_data| {
            ok_json(&json!(s.omnibus.omnibus().pubkey()))
        });

        let s = state.clone();
        state.phone.register_raw("identity.update_name", move |data| {
            let params: Value = serde_json::from_slice(data).unwrap_or(Value::Null);
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let omnibus = s.omnibus.omnibus();
            if s.crown_locked.load(Ordering::Relaxed) {
                return Err(err("identity.update_name", "Crown is locked"));
            }
            omnibus.update_display_name(name)
                .map_err(|e| err("identity.update_name", e))?;
            ok_json(&json!({ "ok": true }))
        });
    }

    fn catalog(&self) -> ModuleCatalog {
        ModuleCatalog::new()
            .with_call(CallDescriptor::new("crown.state", "Get identity state"))
            .with_call(CallDescriptor::new("crown.create", "Create new identity"))
            .with_call(CallDescriptor::new("crown.unlock", "Unlock identity"))
            .with_call(CallDescriptor::new("crown.lock", "Lock identity"))
            .with_call(CallDescriptor::new("crown.profile", "Get profile"))
            .with_call(CallDescriptor::new("crown.update_profile", "Update profile"))
            .with_call(CallDescriptor::new("crown.set_status", "Set online status"))
            .with_call(CallDescriptor::new("crown.import", "Import from recovery phrase"))
            .with_call(CallDescriptor::new("crown.delete", "Delete identity"))
            .with_call(CallDescriptor::new("crown.avatar", "Get avatar"))
            .with_call(CallDescriptor::new("identity.create", "Create identity (alias)"))
            .with_call(CallDescriptor::new("identity.load", "Load identity from path"))
            .with_call(CallDescriptor::new("identity.profile", "Get profile (alias)"))
            .with_call(CallDescriptor::new("identity.pubkey", "Get public key"))
            .with_call(CallDescriptor::new("identity.update_name", "Update display name"))
            .with_emitted_event(EventDescriptor::new("crown.created", "Identity was created"))
            .with_emitted_event(EventDescriptor::new("crown.unlocked", "Identity was unlocked"))
            .with_emitted_event(EventDescriptor::new("crown.locked", "Identity was locked"))
    }
}
