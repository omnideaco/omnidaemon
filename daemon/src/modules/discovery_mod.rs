//! Discovery module — peer discovery.

use std::sync::Arc;
use equipment::{CallDescriptor, ModuleCatalog, PhoneError};
use serde_json::{json, Value};
use crate::daemon_module::DaemonModule;
use crate::state::DaemonState;

pub struct DiscoveryModule;

fn err(op: &str, msg: impl ToString) -> PhoneError {
    PhoneError::HandlerFailed { call_id: op.into(), message: msg.to_string() }
}
fn ok_json(v: &Value) -> Result<Vec<u8>, PhoneError> {
    serde_json::to_vec(v).map_err(|e| err("serialize", e))
}

impl DaemonModule for DiscoveryModule {
    fn id(&self) -> &str { "discovery" }
    fn deps(&self) -> &[&str] { &["omnibus"] }

    fn register(&self, state: &Arc<DaemonState>) {
        let s = state.clone();
        state.phone.register_raw("discovery.peers", move |_data| {
            let peers = s.omnibus.omnibus().peers();
            // LocalPeer may not impl Serialize — format manually
            let peers_json: Vec<Value> = peers.iter()
                .map(|p| json!({"name": format!("{:?}", p)}))
                .collect();
            ok_json(&Value::Array(peers_json))
        });

        let s = state.clone();
        state.phone.register_raw("discovery.peer_count", move |_data| {
            ok_json(&json!({"count": s.omnibus.omnibus().peers().len()}))
        });

        let s = state.clone();
        state.phone.register_raw("discovery.connect_discovered", move |_data| {
            s.omnibus.omnibus().connect_discovered_peers();
            ok_json(&json!({"ok": true}))
        });
    }

    fn catalog(&self) -> ModuleCatalog {
        ModuleCatalog::new()
            .with_call(CallDescriptor::new("discovery.peers", "List discovered peers"))
            .with_call(CallDescriptor::new("discovery.peer_count", "Count discovered peers"))
            .with_call(CallDescriptor::new("discovery.connect_discovered", "Connect to all discovered"))
    }
}
