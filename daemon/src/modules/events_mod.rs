//! Events module — subscribe/unsubscribe for push events.
//!
//! The actual event forwarding is connection-scoped (in handle_client).
//! These handlers just return success.

use std::sync::Arc;
use equipment::{CallDescriptor, ModuleCatalog, PhoneError};
use serde_json::{json, Value};
use crate::daemon_module::DaemonModule;
use crate::state::DaemonState;

pub struct EventsModule;

fn ok_json(v: &Value) -> Result<Vec<u8>, PhoneError> {
    serde_json::to_vec(v).map_err(|e| PhoneError::HandlerFailed {
        call_id: "serialize".into(), message: e.to_string(),
    })
}

impl DaemonModule for EventsModule {
    fn id(&self) -> &str { "events" }

    fn register(&self, _state: &Arc<DaemonState>) {
        _state.phone.register_raw("events.subscribe", |_data| {
            ok_json(&json!({"ok": true, "subscribed": true}))
        });

        _state.phone.register_raw("events.unsubscribe", |_data| {
            ok_json(&json!({"ok": true, "unsubscribed": true}))
        });
    }

    fn catalog(&self) -> ModuleCatalog {
        ModuleCatalog::new()
            .with_call(CallDescriptor::new("events.subscribe", "Subscribe to push events"))
            .with_call(CallDescriptor::new("events.unsubscribe", "Unsubscribe from push events"))
    }
}
