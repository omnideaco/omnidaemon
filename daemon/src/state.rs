//! DaemonState — shared state for all daemon modules and IPC handlers.
//!
//! Holds the full Equipment stack (Phone, Email, Contacts, Pager, Communicator)
//! plus protocol state (Omnibus, Vault, Crown lock, config, etc.).
//!
//! Wrapped in `Arc<DaemonState>` and shared across all handler closures.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use equipment::{Communicator, Contacts, Email, Pager, Phone};
use omnibus::{DaemonConfig, Omnibus};
use uuid::Uuid;
use vault::Vault;

use crate::modules::editor_mod::EditorSession;

/// How Omnibus is accessed — either through Tower (which owns Omnibus)
/// or as a standalone instance.
pub enum OmnibusRef {
    /// Tower mode: Tower owns Omnibus internally.
    Tower(Arc<tower::Tower>),
    /// Standalone mode: daemon owns Omnibus directly.
    Standalone(Arc<Omnibus>),
}

impl OmnibusRef {
    /// Get a reference to the Omnibus instance, regardless of mode.
    pub fn omnibus(&self) -> &Omnibus {
        match self {
            OmnibusRef::Tower(t) => t.omnibus(),
            OmnibusRef::Standalone(o) => o,
        }
    }

    /// Get the Tower if in Tower mode.
    pub fn tower(&self) -> Option<&tower::Tower> {
        match self {
            OmnibusRef::Tower(t) => Some(t),
            OmnibusRef::Standalone(_) => None,
        }
    }
}

/// All shared state for the daemon.
///
/// Every module and handler gets an `Arc<DaemonState>`. The Equipment actors
/// provide the communication backbone: Phone for RPC dispatch, Email for
/// lifecycle events, Contacts for module registry, Pager for notifications,
/// Communicator for future real-time sessions.
pub struct DaemonState {
    // ── Equipment actors ────────────────────────────────────────
    /// RPC dispatch backbone. All operations are Phone handlers.
    pub phone: Phone,
    /// Pub/sub for lifecycle events and modifier observers.
    pub email: Email,
    /// Module registry with self-describing catalogs.
    pub contacts: Contacts,
    /// System notification queue.
    pub pager: Pager,
    /// Real-time session management (future).
    pub communicator: Communicator,

    // ── Protocol state ──────────────────────────────────────────
    /// Omnibus node runtime (identity, networking, relay).
    pub omnibus: OmnibusRef,
    /// Encrypted storage (manifest, content keys, collectives).
    pub vault: Mutex<Vault>,
    /// Crown lock state. When true, sensitive identity ops are gated.
    pub crown_locked: AtomicBool,
    /// Shutdown flag. When true, the daemon exits.
    pub shutdown: AtomicBool,
    /// Ready flag. When true, Equipment is fully registered and dispatch works.
    /// IPC server accepts connections before this, but dispatch() waits on it.
    pub ready: AtomicBool,
    /// Condvar to wake dispatch threads waiting on `ready`.
    pub ready_signal: Condvar,
    /// Mutex paired with ready_signal (Condvar requires a Mutex).
    pub ready_mutex: Mutex<()>,
    /// Mutable daemon config for get/set/reload.
    pub config: Mutex<DaemonConfig>,
    /// Data directory (e.g., ~/.omnidea/data).
    pub data_dir: PathBuf,
    /// Auth token for IPC connections.
    pub auth_token: String,

    // ── Editor state ─────────────────────────────────────────────
    /// Open editor sessions, keyed by idea UUID.
    pub editor_sessions: Mutex<HashMap<Uuid, EditorSession>>,
}

impl DaemonState {
    /// Create a new DaemonState with fresh Equipment actors.
    pub fn new(
        omnibus: OmnibusRef,
        data_dir: PathBuf,
        config: DaemonConfig,
        auth_token: String,
    ) -> Self {
        Self {
            phone: Phone::new(),
            email: Email::new(),
            contacts: Contacts::new(),
            pager: Pager::new(),
            communicator: Communicator::new(),
            omnibus,
            vault: Mutex::new(Vault::new()),
            crown_locked: AtomicBool::new(true),
            shutdown: AtomicBool::new(false),
            ready: AtomicBool::new(false),
            ready_signal: Condvar::new(),
            ready_mutex: Mutex::new(()),
            config: Mutex::new(config),
            data_dir,
            auth_token,
            editor_sessions: Mutex::new(HashMap::new()),
        }
    }
}

impl DaemonState {
    /// Signal that Equipment is fully registered and dispatch can proceed.
    pub fn mark_ready(&self) {
        self.ready.store(true, Ordering::SeqCst);
        self.ready_signal.notify_all();
    }

    /// Block until Equipment is ready (or shutdown). Returns false on shutdown.
    pub fn wait_ready(&self) -> bool {
        if self.ready.load(Ordering::Relaxed) {
            return true;
        }
        let mut guard = self.ready_mutex.lock().unwrap_or_else(|e| e.into_inner());
        while !self.ready.load(Ordering::Relaxed) {
            if self.shutdown.load(Ordering::Relaxed) {
                return false;
            }
            let (g, timeout) = self.ready_signal
                .wait_timeout(guard, std::time::Duration::from_secs(30))
                .unwrap();
            guard = g;
            if timeout.timed_out() {
                return false;
            }
        }
        true
    }
}

/// Ensure the Vault is unlocked. Called after crown.create and crown.unlock.
///
/// Uses VAULT_PASSWORD env var (default: "omnidea-vault-local") — Crown-derived key later.
pub fn ensure_vault_unlocked(state: &DaemonState) {
    let mut vault = state.vault.lock().unwrap_or_else(|e| e.into_inner());
    if !vault.is_unlocked() {
        let data_dir = state.data_dir.clone();
        let password = std::env::var("VAULT_PASSWORD").unwrap_or_else(|_| "omnidea-vault-local".into());
        match vault.unlock(&password, data_dir) {
            Ok(()) => log::info!("Vault unlocked"),
            Err(e) => log::warn!("Vault unlock failed: {e}"),
        }
    }
}
