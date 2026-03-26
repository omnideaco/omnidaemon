//! Omnibus module — Omnibus lifecycle.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use equipment::{CallDescriptor, ModuleCatalog, PhoneError};
use serde_json::{json, Value};
use crate::daemon_module::DaemonModule;
use crate::state::DaemonState;

pub struct OmnibusModule;

fn err(op: &str, msg: impl ToString) -> PhoneError {
    PhoneError::HandlerFailed { call_id: op.into(), message: msg.to_string() }
}
fn ok_json(v: &Value) -> Result<Vec<u8>, PhoneError> {
    serde_json::to_vec(v).map_err(|e| err("serialize", e))
}

impl DaemonModule for OmnibusModule {
    fn id(&self) -> &str { "omnibus" }

    fn register(&self, state: &Arc<DaemonState>) {
        state.phone.register_raw("omnibus.start", |_data| {
            ok_json(&json!({"ok": true, "message": "Omnibus is already running"}))
        });

        let s = state.clone();
        state.phone.register_raw("omnibus.stop", move |_data| {
            log::info!("Received omnibus.stop — stopping daemon");
            s.shutdown.store(true, Ordering::SeqCst);
            ok_json(&json!({"ok": true}))
        });

        let s = state.clone();
        state.phone.register_raw("omnibus.restart", move |_data| {
            log::info!("Received omnibus.restart — stopping daemon (will restart on next connect)");
            s.shutdown.store(true, Ordering::SeqCst);
            ok_json(&json!({"ok": true}))
        });

        let s = state.clone();
        state.phone.register_raw("omnibus.status", move |_data| {
            let status = s.omnibus.omnibus().status();
            let v = crate::api_json::omnibus_status_json(&status);
            ok_json(&v)
        });
    }

    fn catalog(&self) -> ModuleCatalog {
        ModuleCatalog::new()
            .with_call(CallDescriptor::new("omnibus.start", "Start Omnibus (no-op)"))
            .with_call(CallDescriptor::new("omnibus.stop", "Stop daemon"))
            .with_call(CallDescriptor::new("omnibus.restart", "Restart daemon"))
            .with_call(CallDescriptor::new("omnibus.status", "Omnibus status"))
    }
}
