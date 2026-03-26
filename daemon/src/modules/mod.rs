//! Self-registering daemon modules.
//!
//! Each module implements `DaemonModule` and registers its Phone handlers
//! during boot. Modules are listed here in dependency order.

mod crown_mod;
mod daemon_mod;
pub mod editor_mod;
mod ideas_mod;
mod vault_mod;
mod config_mod;
mod network_mod;
mod discovery_mod;
mod health_mod;
mod gospel_mod;
mod tower_mod;
mod omnibus_mod;
mod op_mod;
mod events_mod;

use crate::daemon_module::DaemonModule;

/// All daemon modules in dependency order.
///
/// Modules that depend on others come later in the list.
/// The boot sequence iterates this list, calling `register()` on each.
pub fn all_modules() -> Vec<Box<dyn DaemonModule>> {
    vec![
        // Core infrastructure (no deps)
        Box::new(daemon_mod::DaemonOpsModule),
        Box::new(config_mod::ConfigModule),
        Box::new(omnibus_mod::OmnibusModule),
        Box::new(tower_mod::TowerModule),
        Box::new(events_mod::EventsModule),

        // Identity + storage (dep on omnibus)
        Box::new(crown_mod::CrownModule),
        Box::new(vault_mod::VaultModule),

        // Content (dep on crown + vault)
        Box::new(ideas_mod::IdeasModule),

        // Editor (dep on crown + vault + ideas)
        Box::new(editor_mod::EditorModule),

        // Network (dep on omnibus)
        Box::new(network_mod::NetworkModule),
        Box::new(discovery_mod::DiscoveryModule),
        Box::new(health_mod::HealthModule),
        Box::new(gospel_mod::GospelModule),

        // Meta (dep on phone — must be last)
        Box::new(op_mod::OpModule),
    ]
}
