//! Config module — daemon config get/set/reload.

use std::sync::Arc;
use equipment::{CallDescriptor, ModuleCatalog, PhoneError};
use serde_json::{json, Value};
use crate::daemon_module::DaemonModule;
use crate::state::DaemonState;

pub struct ConfigModule;

fn err(op: &str, msg: impl ToString) -> PhoneError {
    PhoneError::HandlerFailed { call_id: op.into(), message: msg.to_string() }
}
fn ok_json(v: &Value) -> Result<Vec<u8>, PhoneError> {
    serde_json::to_vec(v).map_err(|e| err("serialize", e))
}

impl DaemonModule for ConfigModule {
    fn id(&self) -> &str { "config" }

    fn register(&self, state: &Arc<DaemonState>) {
        let s = state.clone();
        state.phone.register_raw("config.get", move |_data| {
            let config = s.config.lock().map_err(|_| err("config.get", "lock poisoned"))?;
            let v = crate::api_json::daemon_config_json(&config);
            ok_json(&v)
        });

        let s = state.clone();
        state.phone.register_raw("config.set", move |data| {
            let params: Value = serde_json::from_slice(data).unwrap_or(Value::Null);
            let mut config = s.config.lock().map_err(|_| err("config.set", "lock poisoned"))?;

            // DaemonConfig serde IS the config file contract — round-trip through
            // serde is intentional here. We serialize to Value, merge incoming
            // fields, then deserialize back so that serde validates the result.
            if let Some(obj) = params.as_object() {
                let mut config_val = crate::api_json::daemon_config_json(&config);
                if let Some(config_obj) = config_val.as_object_mut() {
                    for (k, v) in obj {
                        config_obj.insert(k.clone(), v.clone());
                    }
                }
                *config = serde_json::from_value(config_val).map_err(|e| err("config.set", e))?;
            }

            crate::config::save_config(&config).map_err(|e| err("config.set", e))?;
            ok_json(&json!({"ok": true}))
        });

        let s = state.clone();
        state.phone.register_raw("config.reload", move |_data| {
            let fresh = crate::config::load_or_create_default();
            let mut config = s.config.lock().map_err(|_| err("config.reload", "lock poisoned"))?;
            *config = fresh;
            ok_json(&json!({"ok": true}))
        });
    }

    fn catalog(&self) -> ModuleCatalog {
        ModuleCatalog::new()
            .with_call(CallDescriptor::new("config.get", "Get daemon config"))
            .with_call(CallDescriptor::new("config.set", "Update config fields"))
            .with_call(CallDescriptor::new("config.reload", "Reload config from disk"))
    }
}
