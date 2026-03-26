//! Health module — relay health, store stats, logs.

use std::sync::Arc;
use equipment::{CallDescriptor, ModuleCatalog, PhoneError};
use serde_json::Value;
use crate::daemon_module::DaemonModule;
use crate::state::DaemonState;

pub struct HealthModule;

fn err(op: &str, msg: impl ToString) -> PhoneError {
    PhoneError::HandlerFailed { call_id: op.into(), message: msg.to_string() }
}
fn ok_json(v: &Value) -> Result<Vec<u8>, PhoneError> {
    serde_json::to_vec(v).map_err(|e| err("serialize", e))
}

impl DaemonModule for HealthModule {
    fn id(&self) -> &str { "health" }
    fn deps(&self) -> &[&str] { &["omnibus"] }

    fn register(&self, state: &Arc<DaemonState>) {
        let s = state.clone();
        state.phone.register_raw("health.relay", move |_data| {
            let health = s.omnibus.omnibus().relay_health();
            let v: Vec<Value> = health.iter()
                .map(|h| crate::api_json::relay_health_json(h))
                .collect();
            ok_json(&Value::Array(v))
        });

        let s = state.clone();
        state.phone.register_raw("health.store_stats", move |_data| {
            let stats = s.omnibus.omnibus().store_stats();
            let v = crate::api_json::store_stats_json(&stats);
            ok_json(&v)
        });

        let s = state.clone();
        state.phone.register_raw("health.logs", move |data| {
            let params: Value = serde_json::from_slice(data).unwrap_or(Value::Null);
            let count = params.get("count").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
            let logs = s.omnibus.omnibus().recent_logs(count);
            let logs_json: Vec<Value> = logs.iter()
                .map(|l| crate::api_json::log_entry_json(l))
                .collect();
            ok_json(&Value::Array(logs_json))
        });
    }

    fn catalog(&self) -> ModuleCatalog {
        ModuleCatalog::new()
            .with_call(CallDescriptor::new("health.relay", "Relay health snapshots"))
            .with_call(CallDescriptor::new("health.store_stats", "Event store stats"))
            .with_call(CallDescriptor::new("health.logs", "Recent log entries"))
    }
}
