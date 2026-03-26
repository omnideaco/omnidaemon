//! Vault module — encrypted storage lifecycle.

use std::sync::Arc;

use equipment::{CallDescriptor, EventDescriptor, ModuleCatalog, PhoneError};
use serde_json::{json, Value};

use crate::daemon_module::DaemonModule;
use crate::state::DaemonState;

pub struct VaultModule;

fn err(op: &str, msg: impl ToString) -> PhoneError {
    PhoneError::HandlerFailed { call_id: op.into(), message: msg.to_string() }
}

fn ok_json(v: &Value) -> Result<Vec<u8>, PhoneError> {
    serde_json::to_vec(v).map_err(|e| err("serialize", e))
}

impl DaemonModule for VaultModule {
    fn id(&self) -> &str { "vault" }
    fn deps(&self) -> &[&str] { &["crown"] }

    fn register(&self, state: &Arc<DaemonState>) {
        let s = state.clone();
        state.phone.register_raw("vault.status", move |_data| {
            let vault = s.vault.lock().unwrap();
            let count = vault.idea_count().unwrap_or(0);
            ok_json(&json!({
                "unlocked": vault.is_unlocked(),
                "idea_count": count,
            }))
        });

        let s = state.clone();
        state.phone.register_raw("vault.lock", move |_data| {
            let mut vault = s.vault.lock().unwrap();
            vault.lock().map_err(|e| err("vault.lock", e))?;
            s.email.send_raw("vault.locked", b"{}");
            ok_json(&json!({"ok": true}))
        });

        let s = state.clone();
        state.phone.register_raw("vault.unlock", move |data| {
            let params: Value = serde_json::from_slice(data).unwrap_or(Value::Null);
            let default_pw = std::env::var("VAULT_PASSWORD").unwrap_or_else(|_| "omnidea-vault-local".into());
            let password = params.get("password").and_then(|v| v.as_str()).unwrap_or(&default_pw);
            let mut vault = s.vault.lock().unwrap();
            let data_dir = s.data_dir.clone();
            vault.unlock(password, data_dir).map_err(|e| err("vault.unlock", e))?;
            s.email.send_raw("vault.unlocked", b"{}");
            ok_json(&json!({"ok": true}))
        });

        let s = state.clone();
        state.phone.register_raw("vault.search", move |data| {
            let params: Value = serde_json::from_slice(data).unwrap_or(Value::Null);
            let query = params.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;
            let vault = s.vault.lock().unwrap();
            let hits = vault.search(query, limit).map_err(|e| err("vault.search", e))?;
            let hits_json: Vec<Value> = hits.iter()
                .map(|h| crate::api_json::search_hit_json(h))
                .collect();
            ok_json(&Value::Array(hits_json))
        });
    }

    fn catalog(&self) -> ModuleCatalog {
        ModuleCatalog::new()
            .with_call(CallDescriptor::new("vault.status", "Vault lock state + idea count"))
            .with_call(CallDescriptor::new("vault.lock", "Lock vault"))
            .with_call(CallDescriptor::new("vault.unlock", "Unlock vault"))
            .with_call(CallDescriptor::new("vault.search", "Full-text search"))
            .with_emitted_event(EventDescriptor::new("vault.locked", "Vault was locked"))
            .with_emitted_event(EventDescriptor::new("vault.unlocked", "Vault was unlocked"))
    }
}
