//! Integration tests for the Equipment-based daemon.
//!
//! Tests exercise the full stack: Phone handlers + Rust crates + temp directory.
//! No sockets, no IPC, no daemon process — just DaemonState + Phone.call_raw().

use std::sync::Arc;

use equipment::{ModuleInfo, ModuleType};
use omnibus::{DaemonConfig, Omnibus, OmnibusConfig};
use serde_json::{json, Value};

// Import daemon internals
use omny_daemon::state::{DaemonState, OmnibusRef, ensure_vault_unlocked};

// Re-export the modules we need
mod helpers {
    use super::*;
    use std::path::Path;

    /// Create a fully-initialized DaemonState for testing.
    ///
    /// Starts Omnibus on a random port in a temp directory,
    /// registers all FFI ops + override modules, wires observers.
    pub fn test_state(dir: &Path) -> Arc<DaemonState> {
        let omnibus_config = OmnibusConfig {
            data_dir: Some(dir.to_path_buf()),
            device_name: format!("test-{}", std::process::id()),
            port: 0, // OS-assigned
            bind_all: false,
            ..Default::default()
        };

        let omnibus = Arc::new(Omnibus::start(omnibus_config).expect("omnibus should start"));

        let daemon_config: DaemonConfig =
            toml::from_str("[omnibus]\n").expect("default config");

        let state = Arc::new(DaemonState::new(
            OmnibusRef::Standalone(omnibus),
            dir.to_path_buf(),
            daemon_config,
            "test-token".into(),
        ));

        // Phase 1: Auto-register FFI ops from C header
        omny_daemon::ffi_ops::register_all(&state.phone);

        // Phase 2: Hand-written modules override complex ops
        let modules = omny_daemon::modules::all_modules();
        for module in &modules {
            module.register(&state);
            let info = ModuleInfo::new(module.id(), module.name(), ModuleType::Source)
                .with_dependencies(module.deps().iter().map(|s| s.to_string()).collect())
                .with_catalog(module.catalog());
            state.contacts.register(info).ok();
        }

        // Phase 3: Wire modifier observers
        omny_daemon::modifiers::wire_observers(&state);

        // Mark ready (in tests, everything is synchronous)
        state.mark_ready();

        state
    }

    /// Call a Phone handler and parse the JSON response.
    pub fn call(state: &DaemonState, method: &str, params: Value) -> Result<Value, String> {
        let input = serde_json::to_vec(&params).unwrap();
        match state.phone.call_raw(method, &input) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).map_err(|e| format!("parse response: {e}"))
            }
            Err(e) => Err(format!("{e}")),
        }
    }
}

// ── Crown Lifecycle Tests ───────────────────────────────────────────

#[test]
fn crown_state_no_identity() {
    let dir = tempfile::tempdir().unwrap();
    let state = helpers::test_state(dir.path());

    let result = helpers::call(&state, "crown.state", json!({})).unwrap();
    assert_eq!(result["exists"], false);
    assert_eq!(result["unlocked"], false);
}

#[test]
fn crown_create_and_state() {
    let dir = tempfile::tempdir().unwrap();
    let state = helpers::test_state(dir.path());

    // Create identity
    let result = helpers::call(&state, "crown.create", json!({"name": "Test User"})).unwrap();
    assert!(result["crown_id"].is_string(), "should return crown_id");

    // State should show exists + unlocked
    let state_result = helpers::call(&state, "crown.state", json!({})).unwrap();
    assert_eq!(state_result["exists"], true);
    assert_eq!(state_result["unlocked"], true);
}

#[test]
fn crown_lock_unlock_cycle() {
    let dir = tempfile::tempdir().unwrap();
    let state = helpers::test_state(dir.path());

    // Create identity
    helpers::call(&state, "crown.create", json!({"name": "Lock Test"})).unwrap();

    // Lock
    let lock_result = helpers::call(&state, "crown.lock", json!({})).unwrap();
    assert_eq!(lock_result["locked"], true);

    // State should show locked
    let state_result = helpers::call(&state, "crown.state", json!({})).unwrap();
    assert_eq!(state_result["unlocked"], false);

    // Unlock
    let unlock_result = helpers::call(&state, "crown.unlock", json!({})).unwrap();
    assert_eq!(unlock_result["unlocked"], true);

    // State should show unlocked
    let state_result = helpers::call(&state, "crown.state", json!({})).unwrap();
    assert_eq!(state_result["unlocked"], true);
}

#[test]
fn crown_profile_requires_unlock() {
    let dir = tempfile::tempdir().unwrap();
    let state = helpers::test_state(dir.path());

    // No identity — profile should fail
    let result = helpers::call(&state, "crown.profile", json!({}));
    assert!(result.is_err());
}

// ── Idea CRUD Tests ─────────────────────────────────────────────────

#[test]
fn idea_create_requires_identity() {
    let dir = tempfile::tempdir().unwrap();
    let state = helpers::test_state(dir.path());

    // No identity — create should fail (vault locked)
    let result = helpers::call(&state, "idea.create", json!({"title": "Test", "type": "text"}));
    assert!(result.is_err(), "idea.create should fail without identity");
}

#[test]
fn idea_create_and_list() {
    let dir = tempfile::tempdir().unwrap();
    let state = helpers::test_state(dir.path());

    // Create identity + unlock vault
    helpers::call(&state, "crown.create", json!({"name": "Idea Test"})).unwrap();

    // Create an idea
    let create_result = helpers::call(&state, "idea.create", json!({
        "title": "My First Note",
        "type": "text",
        "content": "Hello, Omninet!"
    })).unwrap();

    assert!(create_result["id"].is_string(), "should return idea id");
    assert_eq!(create_result["title"], "My First Note");

    // List ideas
    let list_result = helpers::call(&state, "idea.list", json!({})).unwrap();
    let entries = list_result.as_array().expect("should be array");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["title"], "My First Note");
}

#[test]
fn idea_create_load_save_delete() {
    let dir = tempfile::tempdir().unwrap();
    let state = helpers::test_state(dir.path());

    helpers::call(&state, "crown.create", json!({"name": "CRUD Test"})).unwrap();

    // Create
    let create_result = helpers::call(&state, "idea.create", json!({
        "title": "CRUD Note",
        "type": "text",
        "content": "Original content"
    })).unwrap();
    let idea_id = create_result["id"].as_str().unwrap().to_string();

    // Load
    let load_result = helpers::call(&state, "idea.load", json!({"id": &idea_id})).unwrap();
    assert!(load_result.is_object(), "should return IdeaPackage");

    // Delete
    let delete_result = helpers::call(&state, "idea.delete", json!({"id": &idea_id})).unwrap();
    assert_eq!(delete_result["ok"], true);

    // List should be empty
    let list_result = helpers::call(&state, "idea.list", json!({})).unwrap();
    let entries = list_result.as_array().expect("should be array");
    assert_eq!(entries.len(), 0);
}

#[test]
fn idea_list_with_filter() {
    let dir = tempfile::tempdir().unwrap();
    let state = helpers::test_state(dir.path());

    helpers::call(&state, "crown.create", json!({"name": "Filter Test"})).unwrap();

    // Create two ideas with different types
    helpers::call(&state, "idea.create", json!({"title": "Note A", "type": "text"})).unwrap();
    helpers::call(&state, "idea.create", json!({"title": "Note B", "type": "text"})).unwrap();

    // List all
    let all = helpers::call(&state, "idea.list", json!({})).unwrap();
    assert_eq!(all.as_array().unwrap().len(), 2);

    // List filtered by extended_type
    let filtered = helpers::call(&state, "idea.list", json!({"extended_type": "text"})).unwrap();
    assert_eq!(filtered.as_array().unwrap().len(), 2);

    // List filtered by title_contains
    let search = helpers::call(&state, "idea.list", json!({"title_contains": "Note A"})).unwrap();
    assert_eq!(search.as_array().unwrap().len(), 1);
}

// ── Vault Tests ─────────────────────────────────────────────────────

#[test]
fn vault_status_locked_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let state = helpers::test_state(dir.path());

    let result = helpers::call(&state, "vault.status", json!({})).unwrap();
    assert_eq!(result["unlocked"], false);
}

#[test]
fn vault_unlocks_on_crown_create() {
    let dir = tempfile::tempdir().unwrap();
    let state = helpers::test_state(dir.path());

    helpers::call(&state, "crown.create", json!({"name": "Vault Test"})).unwrap();

    let result = helpers::call(&state, "vault.status", json!({})).unwrap();
    assert_eq!(result["unlocked"], true);
}

// ── Equipment Stack Tests ───────────────────────────────────────────

#[test]
fn op_list_returns_all_handlers() {
    let dir = tempfile::tempdir().unwrap();
    let state = helpers::test_state(dir.path());

    let result = helpers::call(&state, "op.list", json!({})).unwrap();
    let ops = result.as_array().expect("should be array");

    // Should have 484 FFI + ~40 overrides = ~524 total
    assert!(ops.len() > 400, "should have 400+ registered ops, got {}", ops.len());

    // Key ops should exist
    let op_names: Vec<&str> = ops.iter().filter_map(|v| v.as_str()).collect();
    assert!(op_names.contains(&"crown.create"), "missing crown.create");
    assert!(op_names.contains(&"idea.create"), "missing idea.create");
    assert!(op_names.contains(&"daemon.ping"), "missing daemon.ping");
    assert!(op_names.contains(&"vault.status"), "missing vault.status");
}

#[test]
fn op_has_known_and_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let state = helpers::test_state(dir.path());

    let known = helpers::call(&state, "op.has", json!({"op": "idea.create"})).unwrap();
    assert_eq!(known["exists"], true);

    let unknown = helpers::call(&state, "op.has", json!({"op": "nonexistent.op"})).unwrap();
    assert_eq!(unknown["exists"], false);
}

#[test]
fn op_count_matches_list() {
    let dir = tempfile::tempdir().unwrap();
    let state = helpers::test_state(dir.path());

    let count = helpers::call(&state, "op.count", json!({})).unwrap();
    let list = helpers::call(&state, "op.list", json!({})).unwrap();

    assert_eq!(
        count["count"].as_u64().unwrap(),
        list.as_array().unwrap().len() as u64
    );
}

#[test]
fn contacts_has_registered_modules() {
    let dir = tempfile::tempdir().unwrap();
    let state = helpers::test_state(dir.path());

    let module_ids = state.contacts.registered_module_ids();
    assert!(module_ids.contains(&"crown".to_string()), "missing crown module");
    assert!(module_ids.contains(&"ideas".to_string()), "missing ideas module");
    assert!(module_ids.contains(&"vault".to_string()), "missing vault module");
    assert!(module_ids.contains(&"daemon".to_string()), "missing daemon module");
}

#[test]
fn contacts_who_handles() {
    let dir = tempfile::tempdir().unwrap();
    let state = helpers::test_state(dir.path());

    assert_eq!(
        state.contacts.who_handles("crown.create"),
        Some("crown".to_string())
    );
    assert_eq!(
        state.contacts.who_handles("idea.create"),
        Some("ideas".to_string())
    );
}

// ── Daemon Ops Tests ────────────────────────────────────────────────

#[test]
fn daemon_ping() {
    let dir = tempfile::tempdir().unwrap();
    let state = helpers::test_state(dir.path());

    let result = helpers::call(&state, "daemon.ping", json!({})).unwrap();
    assert_eq!(result["pong"], true);
}

#[test]
fn daemon_version() {
    let dir = tempfile::tempdir().unwrap();
    let state = helpers::test_state(dir.path());

    let result = helpers::call(&state, "daemon.version", json!({})).unwrap();
    assert!(result["daemon"].is_string());
    assert!(result["op_count"].as_u64().unwrap() > 400);
    assert_eq!(result["equipment_ready"], true);
}

#[test]
fn daemon_health() {
    let dir = tempfile::tempdir().unwrap();
    let state = helpers::test_state(dir.path());

    let result = helpers::call(&state, "daemon.health", json!({})).unwrap();
    assert_eq!(result["healthy"], true);
    assert_eq!(result["equipment_ready"], true);
    assert_eq!(result["omnibus_running"], true);
}

// ── Identity Alias Tests ────────────────────────────────────────────

#[test]
fn identity_aliases_work() {
    let dir = tempfile::tempdir().unwrap();
    let state = helpers::test_state(dir.path());

    // identity.create should work like crown.create
    let result = helpers::call(&state, "identity.create", json!({"name": "Alias Test"})).unwrap();
    assert!(result["crown_id"].is_string());

    // identity.pubkey should return the pubkey
    let pubkey = helpers::call(&state, "identity.pubkey", json!({})).unwrap();
    assert!(pubkey.is_string());
}

// ── Unknown Method Test ─────────────────────────────────────────────

#[test]
fn unknown_method_returns_error() {
    let dir = tempfile::tempdir().unwrap();
    let state = helpers::test_state(dir.path());

    let result = helpers::call(&state, "completely.nonexistent", json!({}));
    assert!(result.is_err());
}
