//! Gospel module — gospel registry dump/save.

use std::sync::Arc;
use equipment::{CallDescriptor, ModuleCatalog, PhoneError};
use serde_json::{json, Value};
use crate::daemon_module::DaemonModule;
use crate::state::DaemonState;

pub struct GospelModule;

fn err(op: &str, msg: impl ToString) -> PhoneError {
    PhoneError::HandlerFailed { call_id: op.into(), message: msg.to_string() }
}
fn ok_json(v: &Value) -> Result<Vec<u8>, PhoneError> {
    serde_json::to_vec(v).map_err(|e| err("serialize", e))
}

impl DaemonModule for GospelModule {
    fn id(&self) -> &str { "gospel" }
    fn deps(&self) -> &[&str] { &["omnibus"] }

    fn register(&self, state: &Arc<DaemonState>) {
        let s = state.clone();
        state.phone.register_raw("gospel.dump", move |_data| {
            let omnibus = s.omnibus.omnibus();
            match omnibus.gospel_registry() {
                Some(registry) => {
                    let events = registry.all_events();
                    let events_json: Vec<Value> = events.iter()
                        .map(|e| crate::api_json::omni_event_json(e))
                        .collect();
                    ok_json(&Value::Array(events_json))
                }
                None => ok_json(&Value::Array(vec![])),
            }
        });

        let s = state.clone();
        state.phone.register_raw("gospel.save", move |_data| {
            let omnibus = s.omnibus.omnibus();
            omnibus.save_gospel();
            ok_json(&json!({"ok": true}))
        });
    }

    fn catalog(&self) -> ModuleCatalog {
        ModuleCatalog::new()
            .with_call(CallDescriptor::new("gospel.dump", "Dump gospel registry"))
            .with_call(CallDescriptor::new("gospel.save", "Persist gospel to disk"))
    }
}
