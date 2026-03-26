//! Op module — query the Phone + Contacts registries.

use std::sync::Arc;
use equipment::{CallDescriptor, ModuleCatalog, PhoneError};
use serde_json::{json, Value};
use crate::daemon_module::DaemonModule;
use crate::state::DaemonState;

pub struct OpModule;

fn err(op: &str, msg: impl ToString) -> PhoneError {
    PhoneError::HandlerFailed { call_id: op.into(), message: msg.to_string() }
}
fn ok_json(v: &Value) -> Result<Vec<u8>, PhoneError> {
    serde_json::to_vec(v).map_err(|e| err("serialize", e))
}

impl DaemonModule for OpModule {
    fn id(&self) -> &str { "op" }

    fn register(&self, state: &Arc<DaemonState>) {
        let s = state.clone();
        state.phone.register_raw("op.list", move |_data| {
            let ids = s.phone.registered_call_ids();
            ok_json(&Value::Array(ids.into_iter().map(Value::String).collect()))
        });

        let s = state.clone();
        state.phone.register_raw("op.has", move |data| {
            let params: Value = serde_json::from_slice(data).unwrap_or(Value::Null);
            let name = params.get("op").and_then(|v| v.as_str()).unwrap_or("");
            ok_json(&json!({"exists": s.phone.has_handler(name)}))
        });

        let s = state.clone();
        state.phone.register_raw("op.count", move |_data| {
            ok_json(&json!({"count": s.phone.registered_call_ids().len()}))
        });
    }

    fn catalog(&self) -> ModuleCatalog {
        ModuleCatalog::new()
            .with_call(CallDescriptor::new("op.list", "List all registered operations"))
            .with_call(CallDescriptor::new("op.has", "Check if operation exists"))
            .with_call(CallDescriptor::new("op.count", "Count registered operations"))
    }
}
