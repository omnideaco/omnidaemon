//! Daemon lifecycle module — ping, status, stop, version, health.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use equipment::{CallDescriptor, ModuleCatalog, PhoneError};
use serde_json::{json, Value};

use crate::daemon_module::DaemonModule;
use crate::state::DaemonState;

pub struct DaemonOpsModule;

fn ok_json(v: &Value) -> Result<Vec<u8>, PhoneError> {
    serde_json::to_vec(v).map_err(|e| PhoneError::HandlerFailed {
        call_id: "serialize".into(), message: e.to_string(),
    })
}

impl DaemonModule for DaemonOpsModule {
    fn id(&self) -> &str { "daemon" }
    fn name(&self) -> &str { "Daemon Lifecycle" }

    fn register(&self, state: &Arc<DaemonState>) {
        state.phone.register_raw("daemon.ping", |_| ok_json(&json!({"pong": true})));

        let s = state.clone();
        state.phone.register_raw("daemon.status", move |_data| {
            let omnibus = s.omnibus.omnibus();
            let status = omnibus.status();
            let status_json = crate::api_json::omnibus_status_json(&status);
            let has_identity = omnibus.pubkey().is_some();
            let unlocked = !s.crown_locked.load(Ordering::Relaxed);
            let mut result = json!({
                "running": true,
                "pid": std::process::id(),
                "omnibus": status_json,
                "tower_enabled": s.omnibus.tower().is_some(),
                "crown": {
                    "exists": has_identity,
                    "unlocked": has_identity && unlocked,
                    "crown_id": omnibus.pubkey(),
                    "online": has_identity && unlocked,
                },
            });
            if let Some(t) = s.omnibus.tower() {
                let ts = t.status();
                // TowerStatus is not re-exported from the tower crate, so we
                // hand-build the JSON inline. TowerMode IS re-exported and
                // derives Serialize with rename_all="lowercase".
                let mode_val = serde_json::to_value(&ts.mode).unwrap_or(Value::Null);
                result["tower"] = json!({
                    "mode": mode_val,
                    "name": ts.name,
                    "relay_url": ts.relay_url,
                    "relay_port": ts.relay_port,
                    "relay_connections": ts.relay_connections,
                    "has_identity": ts.has_identity,
                    "pubkey": ts.pubkey,
                    "gospel_peers": ts.gospel_peers,
                    "gospel_peer_urls": ts.gospel_peer_urls,
                    "uptime_secs": ts.uptime_secs,
                    "event_count": ts.event_count,
                    "indexed_count": ts.indexed_count,
                    "communities": ts.communities,
                    "federated_communities": ts.federated_communities,
                    "connection_policy": ts.connection_policy,
                    "allowlist_size": ts.allowlist_size,
                    "connections_rejected": ts.connections_rejected,
                });
            }
            ok_json(&result)
        });

        let s = state.clone();
        state.phone.register_raw("daemon.stop", move |_data| {
            log::info!("Received daemon.stop — initiating shutdown");
            s.shutdown.store(true, Ordering::SeqCst);
            ok_json(&json!({"ok": true}))
        });

        let s = state.clone();
        state.phone.register_raw("daemon.version", move |_data| {
            ok_json(&json!({
                "daemon": env!("CARGO_PKG_VERSION"),
                "protocol": "1.0",
                "op_count": s.phone.registered_call_ids().len(),
                "equipment_ready": true,
            }))
        });

        let s = state.clone();
        state.phone.register_raw("daemon.health", move |_data| {
            let omnibus = s.omnibus.omnibus();
            let has_identity = omnibus.pubkey().is_some();
            let unlocked = !s.crown_locked.load(Ordering::Relaxed);
            let vault_unlocked = s.vault.lock().map(|v| v.is_unlocked()).unwrap_or(false);
            ok_json(&json!({
                "healthy": true,
                "equipment_ready": true,
                "vault_unlocked": vault_unlocked,
                "identity_loaded": has_identity,
                "crown_unlocked": has_identity && unlocked,
                "omnibus_running": true,
                "tower_enabled": s.omnibus.tower().is_some(),
            }))
        });
    }

    fn catalog(&self) -> ModuleCatalog {
        ModuleCatalog::new()
            .with_call(CallDescriptor::new("daemon.ping", "Health check"))
            .with_call(CallDescriptor::new("daemon.status", "Full daemon status"))
            .with_call(CallDescriptor::new("daemon.stop", "Initiate shutdown"))
            .with_call(CallDescriptor::new("daemon.version", "Version info"))
            .with_call(CallDescriptor::new("daemon.health", "Comprehensive health"))
    }
}
