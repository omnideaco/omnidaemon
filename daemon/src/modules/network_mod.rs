//! Network module — post, publish, connect, set_home.

use std::sync::Arc;
use equipment::{CallDescriptor, ModuleCatalog, PhoneError};
use serde_json::{json, Value};
use crate::daemon_module::DaemonModule;
use crate::state::DaemonState;

pub struct NetworkModule;

fn err(op: &str, msg: impl ToString) -> PhoneError {
    PhoneError::HandlerFailed { call_id: op.into(), message: msg.to_string() }
}
fn ok_json(v: &Value) -> Result<Vec<u8>, PhoneError> {
    serde_json::to_vec(v).map_err(|e| err("serialize", e))
}

impl DaemonModule for NetworkModule {
    fn id(&self) -> &str { "network" }
    fn deps(&self) -> &[&str] { &["omnibus"] }

    fn register(&self, state: &Arc<DaemonState>) {
        let s = state.clone();
        state.phone.register_raw("network.post", move |data| {
            let params: Value = serde_json::from_slice(data).unwrap_or(Value::Null);
            let content = params.get("content").and_then(|v| v.as_str()).unwrap_or("");
            let omnibus = s.omnibus.omnibus();
            match omnibus.post(content) {
                Ok(event) => {
                    let v = crate::api_json::omni_event_json(&event);
                    ok_json(&v)
                }
                Err(e) => Err(err("network.post", e)),
            }
        });

        let s = state.clone();
        // OmniEvent serde IS the wire protocol contract — deserializing inbound
        // events via serde is intentional here. Do not replace with hand-built JSON.
        state.phone.register_raw("network.publish", move |data| {
            let params: Value = serde_json::from_slice(data).unwrap_or(Value::Null);
            let event_json = params.get("event").and_then(|v| v.as_str()).unwrap_or("{}");
            let event: globe::event::OmniEvent = serde_json::from_str(event_json)
                .map_err(|e| err("network.publish", format!("invalid event: {e}")))?;
            let omnibus = s.omnibus.omnibus();
            omnibus.publish(event).map_err(|e| err("network.publish", e))?;
            ok_json(&json!({"ok": true}))
        });

        let s = state.clone();
        state.phone.register_raw("network.connect_relay", move |data| {
            let params: Value = serde_json::from_slice(data).unwrap_or(Value::Null);
            let url = params.get("url").and_then(|v| v.as_str())
                .ok_or_else(|| err("network.connect_relay", "missing 'url'"))?;
            let omnibus = s.omnibus.omnibus();
            omnibus.connect_relay(url).map_err(|e| err("network.connect_relay", e))?;
            ok_json(&json!({"ok": true}))
        });

        let s = state.clone();
        state.phone.register_raw("network.set_home", move |data| {
            let params: Value = serde_json::from_slice(data).unwrap_or(Value::Null);
            let url = params.get("url").and_then(|v| v.as_str())
                .ok_or_else(|| err("network.set_home", "missing 'url'"))?;
            let omnibus = s.omnibus.omnibus();
            omnibus.set_home_node(url).map_err(|e| err("network.set_home", e))?;
            ok_json(&json!({"ok": true}))
        });
    }

    fn catalog(&self) -> ModuleCatalog {
        ModuleCatalog::new()
            .with_call(CallDescriptor::new("network.post", "Post content"))
            .with_call(CallDescriptor::new("network.publish", "Publish event"))
            .with_call(CallDescriptor::new("network.connect_relay", "Connect to relay"))
            .with_call(CallDescriptor::new("network.set_home", "Set home node"))
    }
}
