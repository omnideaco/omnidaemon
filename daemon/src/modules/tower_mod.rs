//! Tower module — Tower status.

use std::sync::Arc;
use equipment::{CallDescriptor, ModuleCatalog, PhoneError};
use serde_json::{json, Value};
use crate::daemon_module::DaemonModule;
use crate::state::DaemonState;

pub struct TowerModule;

fn err(op: &str, msg: impl ToString) -> PhoneError {
    PhoneError::HandlerFailed { call_id: op.into(), message: msg.to_string() }
}
fn ok_json(v: &Value) -> Result<Vec<u8>, PhoneError> {
    serde_json::to_vec(v).map_err(|e| err("serialize", e))
}

impl DaemonModule for TowerModule {
    fn id(&self) -> &str { "tower" }

    fn register(&self, state: &Arc<DaemonState>) {
        let s = state.clone();
        state.phone.register_raw("tower.status", move |_data| {
            match s.omnibus.tower() {
                Some(t) => {
                    let ts = t.status();
                    // TowerStatus is not re-exported from the tower crate, so we
                    // hand-build the JSON inline. TowerMode IS re-exported and
                    // derives Serialize with rename_all="lowercase".
                    let mode_val = serde_json::to_value(&ts.mode).unwrap_or(Value::Null);
                    let v = json!({
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
                    ok_json(&v)
                }
                None => ok_json(&json!({"enabled": false})),
            }
        });

        state.phone.register_raw("tower.start", |_data| {
            Err(err("tower.start", "Tower requires config change + daemon restart"))
        });

        state.phone.register_raw("tower.stop", |_data| {
            Err(err("tower.stop", "Tower requires config change + daemon restart"))
        });
    }

    fn catalog(&self) -> ModuleCatalog {
        ModuleCatalog::new()
            .with_call(CallDescriptor::new("tower.status", "Tower status"))
            .with_call(CallDescriptor::new("tower.start", "Start Tower"))
            .with_call(CallDescriptor::new("tower.stop", "Stop Tower"))
    }
}
