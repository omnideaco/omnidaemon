//! End-to-end tests for the daemon over IPC.
//!
//! Unlike integration.rs (which tests Phone handlers in-process), these tests
//! start a real IPC server on a temp socket and connect via DaemonClient.
//! This exercises the full wire protocol: serialization, auth handshake,
//! request/response framing, and error codes.

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use equipment::{ModuleInfo, ModuleType};
use omnibus::{DaemonConfig, Omnibus, OmnibusConfig};
use omny_client::{ClientType, DaemonClient};
use serde_json::json;

use omny_daemon::server::IpcServer;
use omny_daemon::state::{DaemonState, OmnibusRef};

// ── Test Harness ─────────────────────────────────────────────────────

/// A running daemon with IPC server in a temp directory.
struct TestDaemon {
    state: Arc<DaemonState>,
    socket_path: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

impl TestDaemon {
    /// Start a test daemon on a temp socket.
    fn start() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket_path = dir.path().join("test-daemon.sock");

        let omnibus_config = OmnibusConfig {
            data_dir: Some(dir.path().to_path_buf()),
            device_name: format!("e2e-test-{}", std::process::id()),
            port: 0,
            bind_all: false,
            ..Default::default()
        };

        let omnibus = Arc::new(Omnibus::start(omnibus_config).expect("omnibus should start"));

        let daemon_config: DaemonConfig =
            toml::from_str("[omnibus]\n").expect("default config");

        // Use a known auth token for testing.
        let auth_token = "e2e-test-token-0123456789abcdef".to_string();

        // Write the auth token to the temp dir so the client can read it.
        let auth_path = dir.path().join("auth.token");
        std::fs::write(&auth_path, &auth_token).expect("write auth token");

        let state = Arc::new(DaemonState::new(
            OmnibusRef::Standalone(omnibus),
            dir.path().to_path_buf(),
            daemon_config,
            auth_token,
        ));

        // Register everything
        omny_daemon::ffi_ops::register_all(&state.phone);
        let modules = omny_daemon::modules::all_modules();
        for module in &modules {
            module.register(&state);
            let info = ModuleInfo::new(module.id(), module.name(), ModuleType::Source)
                .with_dependencies(module.deps().iter().map(|s| s.to_string()).collect())
                .with_catalog(module.catalog());
            state.contacts.register(info).ok();
        }
        omny_daemon::modifiers::wire_observers(&state);
        state.mark_ready();

        // Start IPC server in background thread
        let ipc = Arc::new(IpcServer::new(state.clone(), socket_path.clone()));
        let ipc_clone = Arc::clone(&ipc);
        thread::Builder::new()
            .name("e2e-ipc".into())
            .spawn(move || {
                let _ = ipc_clone.run();
            })
            .expect("spawn IPC");

        // Wait for socket to appear
        for _ in 0..50 {
            if socket_path.exists() {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert!(socket_path.exists(), "socket should exist after start");

        TestDaemon {
            state,
            socket_path,
            _dir: dir,
        }
    }

    /// Connect a client to this daemon.
    fn connect(&self) -> DaemonClient {
        // The client normally reads auth token from ~/.omnidea/auth.token,
        // but we need to supply it manually for the temp daemon.
        // Use connect_to_as which does handshake with token from disk.
        // We'll cheat: write our token where the client expects it.
        let home_auth = omny_client::auth_token_path();
        let original_token = std::fs::read_to_string(&home_auth).ok();

        // Temporarily write our test token
        if let Some(parent) = home_auth.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&home_auth, &self.state.auth_token).expect("write temp auth");

        let client = DaemonClient::connect_to_as(
            &self.socket_path,
            ClientType::Cli,
            None,
        )
        .expect("connect to test daemon");

        // Restore original token
        if let Some(orig) = original_token {
            std::fs::write(&home_auth, orig).ok();
        } else {
            std::fs::remove_file(&home_auth).ok();
        }

        client
    }

    /// Shut down the daemon.
    fn shutdown(&self) {
        self.state
            .shutdown
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        self.shutdown();
        // Give the IPC thread a moment to exit
        thread::sleep(Duration::from_millis(100));
    }
}

// ── Tests ────────────────────────────────────────────────────────────

#[test]
fn e2e_daemon_ping() {
    let daemon = TestDaemon::start();
    let client = daemon.connect();

    let result = client.call("daemon.ping", json!({})).unwrap();
    assert_eq!(result["pong"], true);
}

#[test]
fn e2e_daemon_version() {
    let daemon = TestDaemon::start();
    let client = daemon.connect();

    let result = client.call("daemon.version", json!({})).unwrap();
    assert!(result["daemon"].is_string(), "should have daemon version");
    assert!(result["op_count"].as_u64().unwrap() > 400, "should have 400+ ops");
    assert_eq!(result["equipment_ready"], true);
}

#[test]
fn e2e_daemon_health() {
    let daemon = TestDaemon::start();
    let client = daemon.connect();

    let result = client.call("daemon.health", json!({})).unwrap();
    assert_eq!(result["healthy"], true);
    assert_eq!(result["equipment_ready"], true);
    assert_eq!(result["omnibus_running"], true);
}

#[test]
fn e2e_crown_lifecycle() {
    let daemon = TestDaemon::start();
    let client = daemon.connect();

    // 1. No identity yet
    let state = client.call("crown.state", json!({})).unwrap();
    assert_eq!(state["exists"], false);
    assert_eq!(state["unlocked"], false);

    // 2. Create identity
    let created = client.call("crown.create", json!({"name": "E2E User"})).unwrap();
    let crown_id = created["crown_id"].as_str().expect("should return crown_id");
    assert!(!crown_id.is_empty());

    // 3. State shows exists + unlocked
    let state = client.call("crown.state", json!({})).unwrap();
    assert_eq!(state["exists"], true);
    assert_eq!(state["unlocked"], true);
    assert_eq!(state["crown_id"], crown_id);

    // 4. Lock
    let locked = client.call("crown.lock", json!({})).unwrap();
    assert_eq!(locked["locked"], true);

    let state = client.call("crown.state", json!({})).unwrap();
    assert_eq!(state["unlocked"], false);

    // 5. Profile should fail when locked
    let profile_err = client.call("crown.profile", json!({}));
    assert!(profile_err.is_err(), "crown.profile should fail when locked");

    // 6. Unlock
    let unlocked = client.call("crown.unlock", json!({})).unwrap();
    assert_eq!(unlocked["unlocked"], true);

    let state = client.call("crown.state", json!({})).unwrap();
    assert_eq!(state["unlocked"], true);
}

#[test]
fn e2e_idea_crud() {
    let daemon = TestDaemon::start();
    let client = daemon.connect();

    // Setup: create identity
    client.call("crown.create", json!({"name": "CRUD Tester"})).unwrap();

    // 1. Create
    let created = client.call("idea.create", json!({
        "title": "E2E Note",
        "type": "text",
        "content": "Hello from the E2E test!"
    })).unwrap();
    let idea_id = created["id"].as_str().expect("should return id");
    assert!(!idea_id.is_empty());
    assert_eq!(created["title"], "E2E Note");

    // 2. List — should contain our note
    let list = client.call("idea.list", json!({})).unwrap();
    let entries = list.as_array().expect("should be array");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["id"], idea_id);
    assert_eq!(entries[0]["title"], "E2E Note");

    // 3. Load — should return the full package
    let loaded = client.call("idea.load", json!({"id": idea_id})).unwrap();
    assert!(loaded.is_object(), "should return IdeaPackage object");

    // 4. Create a second note
    let created2 = client.call("idea.create", json!({
        "title": "Second Note",
        "type": "text",
        "content": "Second entry"
    })).unwrap();
    let idea2_id = created2["id"].as_str().unwrap();

    // 5. List should have 2
    let list = client.call("idea.list", json!({})).unwrap();
    assert_eq!(list.as_array().unwrap().len(), 2);

    // 6. Delete the first
    let deleted = client.call("idea.delete", json!({"id": idea_id})).unwrap();
    assert_eq!(deleted["ok"], true);

    // 7. List should have 1
    let list = client.call("idea.list", json!({})).unwrap();
    let entries = list.as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["id"], idea2_id);
}

#[test]
fn e2e_idea_requires_identity() {
    let daemon = TestDaemon::start();
    let client = daemon.connect();

    // No identity — idea.create should fail
    let result = client.call("idea.create", json!({"title": "Fail", "type": "text"}));
    assert!(result.is_err(), "idea.create should fail without identity");
}

#[test]
fn e2e_idea_list_filter() {
    let daemon = TestDaemon::start();
    let client = daemon.connect();

    client.call("crown.create", json!({"name": "Filter Tester"})).unwrap();

    // Create notes with different titles
    client.call("idea.create", json!({"title": "Alpha Note", "type": "text"})).unwrap();
    client.call("idea.create", json!({"title": "Beta Note", "type": "text"})).unwrap();
    client.call("idea.create", json!({"title": "Alpha Document", "type": "text"})).unwrap();

    // Filter by title_contains
    let alpha = client.call("idea.list", json!({"title_contains": "Alpha"})).unwrap();
    assert_eq!(alpha.as_array().unwrap().len(), 2);

    let beta = client.call("idea.list", json!({"title_contains": "Beta"})).unwrap();
    assert_eq!(beta.as_array().unwrap().len(), 1);
}

#[test]
fn e2e_vault_locked_until_identity() {
    let daemon = TestDaemon::start();
    let client = daemon.connect();

    // Vault should be locked initially
    let status = client.call("vault.status", json!({})).unwrap();
    assert_eq!(status["unlocked"], false);

    // Create identity — vault should unlock
    client.call("crown.create", json!({"name": "Vault Tester"})).unwrap();

    let status = client.call("vault.status", json!({})).unwrap();
    assert_eq!(status["unlocked"], true);
}

#[test]
fn e2e_op_list() {
    let daemon = TestDaemon::start();
    let client = daemon.connect();

    let list = client.call("op.list", json!({})).unwrap();
    let ops = list.as_array().expect("should be array");
    assert!(ops.len() > 400, "should have 400+ ops, got {}", ops.len());

    // Spot check key ops
    let op_names: Vec<&str> = ops.iter().filter_map(|v| v.as_str()).collect();
    assert!(op_names.contains(&"crown.create"));
    assert!(op_names.contains(&"idea.create"));
    assert!(op_names.contains(&"daemon.ping"));
}

#[test]
fn e2e_unknown_method() {
    let daemon = TestDaemon::start();
    let client = daemon.connect();

    let result = client.call("totally.fake.method", json!({}));
    assert!(result.is_err(), "unknown method should return error");
    if let Err(omny_client::ClientError::Rpc(rpc_err)) = result {
        assert_eq!(rpc_err.code, -32601, "should be method-not-found code");
    } else {
        panic!("expected RpcError, got: {:?}", result);
    }
}

#[test]
fn e2e_identity_aliases() {
    let daemon = TestDaemon::start();
    let client = daemon.connect();

    // identity.create → crown.create
    let created = client.call("identity.create", json!({"name": "Alias Test"})).unwrap();
    assert!(created["crown_id"].is_string());

    // identity.pubkey → returns pubkey
    let pubkey = client.call("identity.pubkey", json!({})).unwrap();
    assert!(pubkey.is_string());
}

#[test]
fn e2e_multiple_clients() {
    let daemon = TestDaemon::start();

    // Connect two clients simultaneously
    let client1 = daemon.connect();
    let client2 = daemon.connect();

    // Both should work
    let ping1 = client1.call("daemon.ping", json!({})).unwrap();
    let ping2 = client2.call("daemon.ping", json!({})).unwrap();

    assert_eq!(ping1["pong"], true);
    assert_eq!(ping2["pong"], true);

    // State changes from one client visible to the other
    client1.call("crown.create", json!({"name": "Multi-Client"})).unwrap();

    let state = client2.call("crown.state", json!({})).unwrap();
    assert_eq!(state["exists"], true);
    assert_eq!(state["unlocked"], true);
}

#[test]
fn e2e_full_tome_workflow() {
    // Simulates what Tome (the note-taking program) does end-to-end:
    // 1. Check identity state
    // 2. Create identity (if needed)
    // 3. Create a note
    // 4. Load it back
    // 5. List all notes
    // 6. Delete it
    // 7. Verify it's gone

    let daemon = TestDaemon::start();
    let client = daemon.connect();

    // Step 1: Check state (what the SDK does on startup)
    let state = client.call("crown.state", json!({})).unwrap();
    assert_eq!(state["exists"], false);

    // Step 2: Create identity (crown setup flow)
    let created = client.call("crown.create", json!({"name": "Tome User"})).unwrap();
    let _crown_id = created["crown_id"].as_str().unwrap();

    // Step 3: Create a note (what Tome does when user clicks "New Note")
    let note = client.call("idea.create", json!({
        "title": "My First Tome Note",
        "type": "text",
        "content": "This is a note created by Tome."
    })).unwrap();
    let note_id = note["id"].as_str().unwrap();

    // Step 4: Load it back (what Tome does when user opens a note)
    let loaded = client.call("idea.load", json!({"id": note_id})).unwrap();
    assert!(loaded.is_object());

    // Step 5: List all notes (what Tome shows in the sidebar)
    let list = client.call("idea.list", json!({})).unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["title"], "My First Tome Note");

    // Step 6: Delete (user deletes the note)
    let deleted = client.call("idea.delete", json!({"id": note_id})).unwrap();
    assert_eq!(deleted["ok"], true);

    // Step 7: Verify it's gone
    let list = client.call("idea.list", json!({})).unwrap();
    assert_eq!(list.as_array().unwrap().len(), 0);
}
