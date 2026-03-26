//! DaemonModule trait — self-registering modules for the daemon.
//!
//! Each Omninet crate adapter implements this trait. The module owns its
//! Phone handler registrations, its Contacts catalog, and its dependencies.
//! Adding a new operation is two steps inside the module — no central file changes.
//!
//! Pattern from Quarry/Tome's Pact: modules self-register during boot,
//! the dispatch table (Phone) is a runtime dictionary that grows as modules register.

use std::sync::Arc;

use equipment::ModuleCatalog;

use crate::state::DaemonState;

/// A self-registering daemon module.
///
/// Each module registers its Phone handlers (and optionally Email subscriptions)
/// during `register()`. The daemon iterates modules at boot — it doesn't know
/// what operations exist. The module owns that knowledge.
pub trait DaemonModule: Send + Sync {
    /// Module identifier (e.g., "crown", "ideas", "vault").
    fn id(&self) -> &str;

    /// Module display name.
    fn name(&self) -> &str { self.id() }

    /// Modules this one depends on (must register first).
    fn deps(&self) -> &[&str] { &[] }

    /// Register all Phone handlers and Email subscriptions.
    ///
    /// Called once during daemon boot, after dependencies have registered.
    /// Phone's `register_raw` replaces any existing handler for the same
    /// call ID — so hand-written modules override auto-generated FFI handlers.
    fn register(&self, state: &Arc<DaemonState>);

    /// Self-describing catalog for Contacts registration.
    ///
    /// Lists all Phone calls this module handles, Email events it emits
    /// and subscribes to, and Communicator channels it supports.
    fn catalog(&self) -> ModuleCatalog;
}
