//! # omnidea-client
//!
//! Shared IPC client library for communicating with the Omnidea daemon (`omny-daemon`).
//!
//! Uses line-delimited JSON over a platform-abstracted IPC transport:
//! - Unix (macOS/Linux): Unix domain socket (`~/.omnidea/daemon.sock`)
//! - Windows: Named Pipe (`\\.\pipe\omnidea-daemon`)
//!
//! The client is fully synchronous — no async runtime required — so it works in any
//! context including Beryllium's non-tokio event loop.
//!
//! ## Protocol
//!
//! **Request** (client -> daemon):
//! ```json
//! {"id": 1, "method": "omnibus.status", "params": {}}
//! ```
//!
//! **Response** (daemon -> client):
//! ```json
//! {"id": 1, "result": {"running": true, "port": 4869}}
//! ```
//!
//! **Error** (daemon -> client):
//! ```json
//! {"id": 2, "error": {"code": -1, "message": "Omnibus not running"}}
//! ```
//!
//! **Push event** (daemon -> client, no id):
//! ```json
//! {"event": "peer.connected", "data": {"pubkey": "abc..."}}
//! ```

use std::collections::HashMap;
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub mod transport;

// ─── Auth + handshake types ─────────────────────────────────────────────────

/// Client type sent during the IPC handshake.
///
/// Determines the client's permission level:
/// - `beryllium`: Full trust (the browser process).
/// - `tray`: Full trust (the menu bar tray app).
/// - `cli`: Full trust (the CLI tool).
/// - `program`: Sandboxed (a TypeScript program running in a WebView).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientType {
    Beryllium,
    Tray,
    Cli,
    Program,
}

impl std::fmt::Display for ClientType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientType::Beryllium => write!(f, "beryllium"),
            ClientType::Tray => write!(f, "tray"),
            ClientType::Cli => write!(f, "cli"),
            ClientType::Program => write!(f, "program"),
        }
    }
}

/// Handshake message sent by the client as the first message on a connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Handshake {
    /// The hex-encoded auth token read from `~/.omnidea/auth.token`.
    pub auth: String,
    /// What kind of client this is (determines permissions).
    pub client_type: ClientType,
    /// Optional program identifier (e.g., "tome", "courier"). Only for `Program` clients.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub program_id: Option<String>,
}

/// Handshake response from the daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandshakeResponse {
    /// Whether authentication succeeded.
    pub auth: String, // "ok" or "denied"
    /// A unique session ID for this connection.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// The client type acknowledged by the daemon.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_type: Option<ClientType>,
}

/// Returns the path to the auth token file: `~/.omnidea/auth.token`.
pub fn auth_token_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".omnidea").join("auth.token")
}

/// Read the auth token from disk.
pub fn read_auth_token() -> Result<String, std::io::Error> {
    let path = auth_token_path();
    let token = std::fs::read_to_string(path)?;
    Ok(token.trim().to_string())
}

// ─── Protocol types ─────────────────────────────────────────────────────────

/// Request from client to daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

/// Successful or error response from daemon to client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

/// Structured RPC error returned inside a [`Response`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RPC error {}: {}", self.code, self.message)
    }
}

impl std::error::Error for RpcError {}

/// Server-initiated push event (no request id).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushEvent {
    pub event: String,
    #[serde(default)]
    pub data: Value,
}

/// Any message the server can send: either a [`Response`] or a [`PushEvent`].
///
/// Uses `#[serde(untagged)]` — responses are distinguished by having an `id` field.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ServerMessage {
    Response(Response),
    Event(PushEvent),
}

// ─── Error type ─────────────────────────────────────────────────────────────

/// Errors that can occur when communicating with the daemon.
#[derive(Debug)]
pub enum ClientError {
    /// Could not connect to daemon socket.
    ConnectionFailed(std::io::Error),
    /// Socket read/write error.
    Io(std::io::Error),
    /// JSON serialization/deserialization error.
    Json(serde_json::Error),
    /// Daemon returned an error response.
    Rpc(RpcError),
    /// Response timeout (10 seconds by default).
    Timeout,
    /// Daemon disconnected unexpectedly.
    Disconnected,
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::ConnectionFailed(e) => write!(f, "connection failed: {e}"),
            ClientError::Io(e) => write!(f, "I/O error: {e}"),
            ClientError::Json(e) => write!(f, "JSON error: {e}"),
            ClientError::Rpc(e) => write!(f, "{e}"),
            ClientError::Timeout => write!(f, "response timeout"),
            ClientError::Disconnected => write!(f, "daemon disconnected"),
        }
    }
}

impl std::error::Error for ClientError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ClientError::ConnectionFailed(e) | ClientError::Io(e) => Some(e),
            ClientError::Json(e) => Some(e),
            ClientError::Rpc(e) => Some(e),
            _ => None,
        }
    }
}

impl From<serde_json::Error> for ClientError {
    fn from(e: serde_json::Error) -> Self {
        ClientError::Json(e)
    }
}

// ─── Socket path ────────────────────────────────────────────────────────────

/// Returns the default daemon socket path for the current platform.
///
/// - macOS/Linux: `~/.omnidea/daemon.sock`
/// - Windows: `\\.\pipe\omnidea-daemon`
pub fn default_socket_path() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(r"\\.\pipe\omnidea-daemon")
    } else {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        PathBuf::from(home).join(".omnidea").join("daemon.sock")
    }
}

// ─── Pending call infrastructure ────────────────────────────────────────────

/// A one-shot channel for delivering a single response to a waiting caller.
struct PendingCall {
    response: Mutex<Option<Response>>,
    ready: Condvar,
}

impl PendingCall {
    fn new() -> Self {
        Self {
            response: Mutex::new(None),
            ready: Condvar::new(),
        }
    }

    /// Block until a response arrives or timeout expires.
    fn wait(&self, timeout: Duration) -> Result<Response, ClientError> {
        let mut guard = self.response.lock().map_err(|_| ClientError::Disconnected)?;
        let result = self
            .ready
            .wait_timeout_while(guard, timeout, |resp| resp.is_none())
            .map_err(|_| ClientError::Disconnected)?;
        guard = result.0;
        if result.1.timed_out() {
            return Err(ClientError::Timeout);
        }
        guard.take().ok_or(ClientError::Disconnected)
    }

    /// Deliver a response, waking the waiter.
    fn deliver(&self, response: Response) {
        if let Ok(mut guard) = self.response.lock() {
            *guard = Some(response);
            self.ready.notify_one();
        }
    }
}

// ─── DaemonClient ───────────────────────────────────────────────────────────

/// Synchronous IPC client for the Omnidea daemon.
///
/// Connects to the daemon's Unix socket and provides a blocking RPC interface.
/// A background reader thread dispatches responses to waiting callers and
/// forwards push events to subscribers.
///
/// # Thread Safety
///
/// `DaemonClient` is `Send + Sync`. Multiple threads can call methods concurrently.
/// Writes are serialized via an internal mutex. Reads are handled by a dedicated
/// background thread.
pub struct DaemonClient {
    /// Monotonically increasing request ID.
    next_id: AtomicU64,
    /// Serialized writer access.
    writer: Mutex<BufWriter<Box<dyn Write + Send>>>,
    /// Pending calls awaiting responses, keyed by request ID.
    pending: Arc<Mutex<HashMap<u64, Arc<PendingCall>>>>,
    /// Receiver handed out by `subscribe_events`. Only one subscriber at a time.
    event_rx: Mutex<Option<mpsc::Receiver<PushEvent>>>,
    /// Response timeout duration.
    timeout: Duration,
    /// Socket path for reconnection.
    socket_path: PathBuf,
    /// Client type for reconnection handshake.
    client_type: ClientType,
    /// Program ID for reconnection handshake (if Program client).
    program_id: Option<String>,
}

impl std::fmt::Debug for DaemonClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaemonClient")
            .field("next_id", &self.next_id)
            .field("timeout", &self.timeout)
            .field("client_type", &self.client_type)
            .field("socket_path", &self.socket_path)
            .finish_non_exhaustive()
    }
}

impl DaemonClient {
    /// Connect to the daemon at the default socket path.
    ///
    /// Performs authentication using the token from `~/.omnidea/auth.token`
    /// and identifies as the given `client_type`.
    pub fn connect_as(client_type: ClientType) -> Result<Self, ClientError> {
        Self::connect_to_as(&default_socket_path(), client_type, None)
    }

    /// Connect to the daemon, identifying as a specific program.
    pub fn connect_as_program(program_id: &str) -> Result<Self, ClientError> {
        Self::connect_to_as(
            &default_socket_path(),
            ClientType::Program,
            Some(program_id.to_string()),
        )
    }

    /// Backward-compatible connect (defaults to CLI client type).
    ///
    /// Used by existing code (omny-daemon status/stop, tray) that doesn't
    /// yet pass a client type. Falls back gracefully if no auth token exists.
    pub fn connect() -> Result<Self, ClientError> {
        Self::connect_to(&default_socket_path())
    }

    /// Backward-compatible connect to a specific path (defaults to CLI).
    pub fn connect_to(path: &Path) -> Result<Self, ClientError> {
        Self::connect_to_as(path, ClientType::Cli, None)
    }

    /// Connect to the daemon at a specific socket path with client identification.
    pub fn connect_to_as(
        path: &Path,
        client_type: ClientType,
        program_id: Option<String>,
    ) -> Result<Self, ClientError> {
        let mut stream =
            transport::ClientStream::connect(path).map_err(ClientError::ConnectionFailed)?;

        // Perform handshake: send auth token + client type as the first message.
        let handshake_result = Self::perform_handshake(&mut stream, client_type.clone(), program_id.clone());

        // If handshake fails because no auth token exists, proceed anyway.
        // The daemon will accept unauthenticated connections during the
        // transition period, but will log a warning.
        if let Err(ref e) = handshake_result {
            log::debug!("Handshake skipped or failed: {e} — connecting without auth");
        }

        let reader_stream = stream.try_clone().map_err(ClientError::Io)?;
        let writer_stream = stream;

        let writer: Box<dyn Write + Send> = Box::new(writer_stream);
        let pending: Arc<Mutex<HashMap<u64, Arc<PendingCall>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let (event_tx, event_rx) = mpsc::channel();

        let client = DaemonClient {
            next_id: AtomicU64::new(1),
            writer: Mutex::new(BufWriter::new(writer)),
            pending: Arc::clone(&pending),
            event_rx: Mutex::new(Some(event_rx)),
            timeout: Duration::from_secs(10),
            socket_path: path.to_path_buf(),
            client_type: client_type,
            program_id: program_id,
        };

        // Spawn reader thread
        let reader_pending = Arc::clone(&pending);
        thread::Builder::new()
            .name("omnidea-client-reader".to_string())
            .spawn(move || {
                Self::reader_loop(reader_stream, reader_pending, event_tx);
            })
            .map_err(ClientError::Io)?;

        Ok(client)
    }

    /// Perform the authentication handshake on a raw stream.
    fn perform_handshake(
        stream: &mut transport::ClientStream,
        client_type: ClientType,
        program_id: Option<String>,
    ) -> Result<HandshakeResponse, ClientError> {
        // Read the auth token from disk.
        let token = read_auth_token().map_err(ClientError::Io)?;

        let handshake = Handshake {
            auth: token,
            client_type,
            program_id,
        };

        // Send handshake as a single JSON line.
        let mut line = serde_json::to_string(&handshake)?;
        line.push('\n');
        stream.write_all(line.as_bytes()).map_err(ClientError::Io)?;
        stream.flush().map_err(ClientError::Io)?;

        // Read the response line.
        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            match stream.read(&mut byte) {
                Ok(0) => return Err(ClientError::Disconnected),
                Ok(_) => {
                    if byte[0] == b'\n' {
                        break;
                    }
                    buf.push(byte[0]);
                }
                Err(e) => return Err(ClientError::Io(e)),
            }
        }

        let response: HandshakeResponse = serde_json::from_slice(&buf)?;

        if response.auth != "ok" {
            return Err(ClientError::Rpc(RpcError {
                code: -10,
                message: "Authentication denied".to_string(),
            }));
        }

        Ok(response)
    }

    /// Reconnect to the daemon after a disconnect.
    ///
    /// Creates a fresh connection, performs handshake, and replaces the
    /// internal writer and reader. Existing pending calls will fail with
    /// `Disconnected` (the old reader thread exits when the stream closes).
    pub fn reconnect(&self) -> Result<(), ClientError> {
        let mut stream = transport::ClientStream::connect(&self.socket_path)
            .map_err(ClientError::ConnectionFailed)?;

        // Re-authenticate.
        let _ = Self::perform_handshake(
            &mut stream,
            self.client_type.clone(),
            self.program_id.clone(),
        );

        let reader_stream = stream.try_clone().map_err(ClientError::Io)?;
        let writer_stream = stream;

        // Replace the writer.
        {
            let mut writer = self.writer.lock().map_err(|_| ClientError::Disconnected)?;
            *writer = BufWriter::new(Box::new(writer_stream));
        }

        // New event channel.
        let (event_tx, event_rx) = mpsc::channel();
        {
            let mut rx_slot = self.event_rx.lock().map_err(|_| ClientError::Disconnected)?;
            *rx_slot = Some(event_rx);
        }

        // Spawn new reader thread.
        let reader_pending = Arc::clone(&self.pending);
        thread::Builder::new()
            .name("omnidea-client-reader".to_string())
            .spawn(move || {
                Self::reader_loop(reader_stream, reader_pending, event_tx);
            })
            .map_err(ClientError::Io)?;

        log::info!("Reconnected to daemon");
        Ok(())
    }

    /// Call with one automatic reconnect attempt on disconnect.
    pub fn call_with_retry(&self, method: &str, params: Value) -> Result<Value, ClientError> {
        match self.call(method, params.clone()) {
            Err(ClientError::Disconnected) | Err(ClientError::Timeout) => {
                log::debug!("Call to {method} failed, attempting reconnect");
                self.reconnect()?;
                self.call(method, params)
            }
            other => other,
        }
    }

    /// Background reader loop: reads lines from the stream, dispatches responses
    /// to pending callers, and forwards push events to the event channel.
    fn reader_loop(
        stream: transport::ClientStream,
        pending: Arc<Mutex<HashMap<u64, Arc<PendingCall>>>>,
        event_tx: mpsc::Sender<PushEvent>,
    ) {
        let reader = BufReader::new(stream);
        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
                Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => continue,
                Err(_) => break, // Connection closed or fatal error
            };

            if line.is_empty() {
                continue;
            }

            // Try to parse as a server message
            match serde_json::from_str::<ServerMessage>(&line) {
                Ok(ServerMessage::Response(resp)) => {
                    let id = resp.id;
                    if let Ok(mut map) = pending.lock() {
                        if let Some(call) = map.remove(&id) {
                            call.deliver(resp);
                        } else {
                            log::warn!("received response for unknown request id {id}");
                        }
                    }
                }
                Ok(ServerMessage::Event(event)) => {
                    // Best-effort delivery; if nobody is listening, drop it
                    let _ = event_tx.send(event);
                }
                Err(e) => {
                    log::warn!("failed to parse server message: {e} (line: {line})");
                }
            }
        }

        // Connection closed — wake all pending callers so they get Disconnected
        if let Ok(mut map) = pending.lock() {
            for (_, call) in map.drain() {
                // Deliver nothing — the waiter will see None and return Disconnected
                call.ready.notify_one();
            }
        }
    }
}

impl DaemonClient {
    /// Send a request and wait for the matching response (blocking).
    ///
    /// Returns the `result` value on success, or a [`ClientError::Rpc`] if the
    /// daemon returned an error.
    pub fn call(&self, method: &str, params: Value) -> Result<Value, ClientError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);

        let request = Request {
            id,
            method: method.to_string(),
            params,
        };

        // Register pending call before writing (avoid race with reader thread)
        let pending_call = Arc::new(PendingCall::new());
        {
            let mut map = self.pending.lock().map_err(|_| ClientError::Disconnected)?;
            map.insert(id, Arc::clone(&pending_call));
        }

        // Serialize and send
        let mut line = serde_json::to_string(&request)?;
        line.push('\n');

        {
            let mut writer = self.writer.lock().map_err(|_| ClientError::Disconnected)?;
            writer.write_all(line.as_bytes()).map_err(ClientError::Io)?;
            writer.flush().map_err(ClientError::Io)?;
        }

        // Wait for response
        let response = match pending_call.wait(self.timeout) {
            Ok(resp) => resp,
            Err(e) => {
                // Clean up on timeout/disconnect
                if let Ok(mut map) = self.pending.lock() {
                    map.remove(&id);
                }
                return Err(e);
            }
        };

        // Check for error response
        if let Some(rpc_err) = response.error {
            return Err(ClientError::Rpc(rpc_err));
        }

        Ok(response.result.unwrap_or(Value::Null))
    }

    /// Subscribe to push events from the daemon.
    ///
    /// Returns a receiver channel that yields [`PushEvent`] values as the daemon
    /// sends them. Only one subscriber is supported — calling this again returns
    /// an error.
    pub fn subscribe_events(&self) -> Result<mpsc::Receiver<PushEvent>, ClientError> {
        let mut rx_slot = self.event_rx.lock().map_err(|_| ClientError::Disconnected)?;
        rx_slot.take().ok_or(ClientError::Disconnected)
    }

    // ─── Convenience methods ────────────────────────────────────────────

    /// Query the daemon's own status.
    pub fn daemon_status(&self) -> Result<Value, ClientError> {
        self.call("daemon.status", Value::Object(Default::default()))
    }

    /// Ping the daemon (lightweight health check).
    pub fn daemon_ping(&self) -> Result<Value, ClientError> {
        self.call("daemon.ping", serde_json::json!({}))
    }

    /// Get daemon version, protocol version, and operation count.
    pub fn daemon_version(&self) -> Result<Value, ClientError> {
        self.call("daemon.version", serde_json::json!({}))
    }

    /// Comprehensive health check (orchestrator, vault, identity, network).
    pub fn daemon_health(&self) -> Result<Value, ClientError> {
        self.call("daemon.health", serde_json::json!({}))
    }

    // ─── Operation dispatch ─────────────────────────────────────────

    /// Run a single operation by name (convenience over pipeline_run).
    pub fn op_run(&self, op: &str, input: Value) -> Result<Value, ClientError> {
        self.call("op.run", serde_json::json!({ "op": op, "input": input }))
    }

    /// List all available operations.
    pub fn op_list(&self) -> Result<Value, ClientError> {
        self.call("op.list", serde_json::json!({}))
    }

    /// Check if an operation exists.
    pub fn op_has(&self, op: &str) -> Result<Value, ClientError> {
        self.call("op.has", serde_json::json!({ "op": op }))
    }

    /// Get the total count of registered operations.
    pub fn op_count(&self) -> Result<Value, ClientError> {
        self.call("op.count", serde_json::json!({}))
    }

    /// Start the Omnibus node runtime.
    pub fn omnibus_start(&self) -> Result<Value, ClientError> {
        self.call("omnibus.start", Value::Object(Default::default()))
    }

    /// Stop the Omnibus node runtime.
    pub fn omnibus_stop(&self) -> Result<Value, ClientError> {
        self.call("omnibus.stop", Value::Object(Default::default()))
    }

    /// Restart the Omnibus node runtime.
    pub fn omnibus_restart(&self) -> Result<Value, ClientError> {
        self.call("omnibus.restart", Value::Object(Default::default()))
    }

    /// Query the Omnibus node status.
    pub fn omnibus_status(&self) -> Result<Value, ClientError> {
        self.call("omnibus.status", Value::Object(Default::default()))
    }

    /// Start the Tower service.
    pub fn tower_start(&self) -> Result<Value, ClientError> {
        self.call("tower.start", Value::Object(Default::default()))
    }

    /// Stop the Tower service.
    pub fn tower_stop(&self) -> Result<Value, ClientError> {
        self.call("tower.stop", Value::Object(Default::default()))
    }

    /// Query the Tower service status.
    pub fn tower_status(&self) -> Result<Value, ClientError> {
        self.call("tower.status", Value::Object(Default::default()))
    }

    /// Create a new identity with the given display name.
    pub fn identity_create(&self, name: &str) -> Result<Value, ClientError> {
        self.call(
            "identity.create",
            serde_json::json!({ "name": name }),
        )
    }

    /// Get the current identity profile.
    pub fn identity_profile(&self) -> Result<Value, ClientError> {
        self.call("identity.profile", Value::Object(Default::default()))
    }

    /// Get the current identity's public key.
    pub fn identity_pubkey(&self) -> Result<Value, ClientError> {
        self.call("identity.pubkey", Value::Object(Default::default()))
    }

    // ─── Crown methods ───────────────────────────────────────────────

    /// Query the current Crown identity state.
    ///
    /// Returns `{ exists, unlocked, crown_id, display_name, online, has_avatar }`.
    /// Always works, even when locked.
    pub fn crown_state(&self) -> Result<Value, ClientError> {
        self.call("crown.state", serde_json::json!({}))
    }

    /// Create a new Crown identity with the given display name.
    ///
    /// Automatically unlocks the Crown on success.
    pub fn crown_create(&self, name: &str) -> Result<Value, ClientError> {
        self.call("crown.create", serde_json::json!({ "name": name }))
    }

    /// Unlock the Crown for the current session.
    pub fn crown_unlock(&self) -> Result<Value, ClientError> {
        self.call("crown.unlock", serde_json::json!({}))
    }

    /// Lock the Crown, gating sensitive operations.
    pub fn crown_lock(&self) -> Result<Value, ClientError> {
        self.call("crown.lock", serde_json::json!({}))
    }

    /// Get the current Crown profile. Requires unlocked Crown.
    pub fn crown_profile(&self) -> Result<Value, ClientError> {
        self.call("crown.profile", serde_json::json!({}))
    }

    /// Update the Crown profile display name. Requires unlocked Crown.
    pub fn crown_update_profile(&self, display_name: &str) -> Result<Value, ClientError> {
        self.call(
            "crown.update_profile",
            serde_json::json!({ "display_name": display_name }),
        )
    }

    /// Set online/offline status. Requires unlocked Crown.
    pub fn crown_set_status(&self, online: bool) -> Result<Value, ClientError> {
        self.call(
            "crown.set_status",
            serde_json::json!({ "online": online }),
        )
    }

    /// Get the Crown avatar (base64 or null). Works even when locked.
    pub fn crown_avatar(&self) -> Result<Value, ClientError> {
        self.call("crown.avatar", serde_json::json!({}))
    }

    // ─── Network methods ────────────────────────────────────────────

    /// Post content to the network.
    pub fn network_post(&self, content: &str) -> Result<Value, ClientError> {
        self.call(
            "network.post",
            serde_json::json!({ "content": content }),
        )
    }

    /// Publish a raw event (as JSON) to the network.
    pub fn network_publish(&self, event_json: &str) -> Result<Value, ClientError> {
        self.call(
            "network.publish",
            serde_json::json!({ "event": event_json }),
        )
    }

    /// List connected peers.
    pub fn discovery_peers(&self) -> Result<Value, ClientError> {
        self.call("discovery.peers", Value::Object(Default::default()))
    }

    /// Get the count of connected peers.
    pub fn discovery_peer_count(&self) -> Result<Value, ClientError> {
        self.call("discovery.peer_count", Value::Object(Default::default()))
    }

    /// Check relay health.
    pub fn health_relay(&self) -> Result<Value, ClientError> {
        self.call("health.relay", Value::Object(Default::default()))
    }

    /// Get storage statistics.
    pub fn health_store_stats(&self) -> Result<Value, ClientError> {
        self.call("health.store_stats", Value::Object(Default::default()))
    }

    /// Get recent daemon log entries.
    pub fn health_logs(&self, count: u32) -> Result<Value, ClientError> {
        self.call(
            "health.logs",
            serde_json::json!({ "count": count }),
        )
    }

    /// Dump the gospel registry (for debugging).
    pub fn gospel_dump(&self) -> Result<Value, ClientError> {
        self.call("gospel.dump", Value::Object(Default::default()))
    }

    /// Get the current daemon configuration.
    pub fn config_get(&self) -> Result<Value, ClientError> {
        self.call("config.get", Value::Object(Default::default()))
    }

    /// Set a single config field.
    ///
    /// Returns `{"ok": true, "needs_restart": bool}`.
    pub fn config_set(&self, section: &str, key: &str, value: Value) -> Result<Value, ClientError> {
        self.call(
            "config.set",
            serde_json::json!({
                "section": section,
                "key": key,
                "value": value,
            }),
        )
    }

    /// Set multiple config fields at once.
    ///
    /// `updates` should be a nested object like `{"omnibus": {"port": 5050}, "tower": {"enabled": true}}`.
    /// Returns `{"ok": true, "needs_restart": bool}`.
    pub fn config_set_updates(&self, updates: Value) -> Result<Value, ClientError> {
        self.call("config.set", serde_json::json!({ "updates": updates }))
    }

    /// Reload config from disk.
    pub fn config_reload(&self) -> Result<Value, ClientError> {
        self.call("config.reload", Value::Object(Default::default()))
    }

    // ─── Pipeline methods ────────────────────────────────────────────

    /// Execute a pipeline of protocol operations.
    ///
    /// `pipeline_json` should be a JSON string with the pipeline spec:
    /// ```json
    /// { "source": "bridge", "steps": [{ "id": "s0", "op": "ideas.create_digit", "input": {...} }] }
    /// ```
    pub fn pipeline_run(&self, pipeline_json: &str) -> Result<Value, ClientError> {
        self.call(
            "pipeline.run",
            serde_json::json!({ "pipeline": pipeline_json }),
        )
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_request_serialize() {
        let req = Request {
            id: 1,
            method: "omnibus.status".to_string(),
            params: json!({}),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"id\":1"));
        assert!(json.contains("\"method\":\"omnibus.status\""));
    }

    #[test]
    fn test_request_roundtrip() {
        let req = Request {
            id: 42,
            method: "identity.create".to_string(),
            params: json!({"name": "Alice"}),
        };
        let json = serde_json::to_string(&req).unwrap();
        let parsed: Request = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, 42);
        assert_eq!(parsed.method, "identity.create");
        assert_eq!(parsed.params["name"], "Alice");
    }

    #[test]
    fn test_request_default_params() {
        let json = r#"{"id": 1, "method": "daemon.status"}"#;
        let req: Request = serde_json::from_str(json).unwrap();
        assert_eq!(req.id, 1);
        assert_eq!(req.method, "daemon.status");
        assert_eq!(req.params, Value::Null);
    }

    #[test]
    fn test_response_success_serialize() {
        let resp = Response {
            id: 1,
            result: Some(json!({"running": true, "port": 4869})),
            error: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"result\""));
        assert!(!json.contains("\"error\""), "error field should be skipped when None");
    }

    #[test]
    fn test_response_error_serialize() {
        let resp = Response {
            id: 2,
            result: None,
            error: Some(RpcError {
                code: -1,
                message: "Omnibus not running".to_string(),
            }),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(!json.contains("\"result\""), "result field should be skipped when None");
        assert!(json.contains("\"error\""));
        assert!(json.contains("Omnibus not running"));
    }

    #[test]
    fn test_response_success_roundtrip() {
        let resp = Response {
            id: 7,
            result: Some(json!({"peers": 3, "uptime": 3600})),
            error: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: Response = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, 7);
        assert_eq!(parsed.result.unwrap()["peers"], 3);
        assert!(parsed.error.is_none());
    }

    #[test]
    fn test_response_error_roundtrip() {
        let resp = Response {
            id: 3,
            result: None,
            error: Some(RpcError {
                code: -42,
                message: "not found".to_string(),
            }),
        };
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: Response = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, 3);
        assert!(parsed.result.is_none());
        let err = parsed.error.unwrap();
        assert_eq!(err.code, -42);
        assert_eq!(err.message, "not found");
    }

    #[test]
    fn test_push_event_serialize() {
        let event = PushEvent {
            event: "peer.connected".to_string(),
            data: json!({"pubkey": "abc123"}),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"event\":\"peer.connected\""));
        assert!(json.contains("\"pubkey\":\"abc123\""));
    }

    #[test]
    fn test_push_event_roundtrip() {
        let event = PushEvent {
            event: "omnibus.stopped".to_string(),
            data: json!({"reason": "manual"}),
        };
        let json = serde_json::to_string(&event).unwrap();
        let parsed: PushEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.event, "omnibus.stopped");
        assert_eq!(parsed.data["reason"], "manual");
    }

    #[test]
    fn test_push_event_default_data() {
        let json = r#"{"event": "heartbeat"}"#;
        let event: PushEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.event, "heartbeat");
        assert_eq!(event.data, Value::Null);
    }

    #[test]
    fn test_server_message_response_variant() {
        let json = r#"{"id": 1, "result": {"ok": true}}"#;
        let msg: ServerMessage = serde_json::from_str(json).unwrap();
        match msg {
            ServerMessage::Response(resp) => {
                assert_eq!(resp.id, 1);
                assert_eq!(resp.result.unwrap()["ok"], true);
            }
            ServerMessage::Event(_) => panic!("expected Response, got Event"),
        }
    }

    #[test]
    fn test_server_message_error_variant() {
        let json = r#"{"id": 2, "error": {"code": -1, "message": "fail"}}"#;
        let msg: ServerMessage = serde_json::from_str(json).unwrap();
        match msg {
            ServerMessage::Response(resp) => {
                assert_eq!(resp.id, 2);
                assert!(resp.result.is_none());
                let err = resp.error.unwrap();
                assert_eq!(err.code, -1);
            }
            ServerMessage::Event(_) => panic!("expected Response, got Event"),
        }
    }

    #[test]
    fn test_server_message_event_variant() {
        let json = r#"{"event": "peer.disconnected", "data": {"pubkey": "xyz"}}"#;
        let msg: ServerMessage = serde_json::from_str(json).unwrap();
        match msg {
            ServerMessage::Event(event) => {
                assert_eq!(event.event, "peer.disconnected");
                assert_eq!(event.data["pubkey"], "xyz");
            }
            ServerMessage::Response(_) => panic!("expected Event, got Response"),
        }
    }

    #[test]
    fn test_rpc_error_display() {
        let err = RpcError {
            code: -5,
            message: "something went wrong".to_string(),
        };
        assert_eq!(format!("{err}"), "RPC error -5: something went wrong");
    }

    #[test]
    fn test_client_error_display() {
        assert_eq!(format!("{}", ClientError::Timeout), "response timeout");
        assert_eq!(format!("{}", ClientError::Disconnected), "daemon disconnected");

        let rpc = ClientError::Rpc(RpcError {
            code: -1,
            message: "test".to_string(),
        });
        assert_eq!(format!("{rpc}"), "RPC error -1: test");
    }

    #[test]
    fn test_default_socket_path() {
        let path = default_socket_path();
        // On Unix, should end with daemon.sock
        #[cfg(unix)]
        assert!(
            path.ends_with("daemon.sock"),
            "expected path ending with daemon.sock, got {path:?}"
        );
    }

    #[test]
    fn test_line_delimited_protocol_format() {
        // Verify that serialized messages produce single-line JSON suitable for
        // line-delimited protocol
        let req = Request {
            id: 1,
            method: "test".to_string(),
            params: json!({"key": "value with spaces"}),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains('\n'), "serialized JSON must not contain newlines");

        let resp = Response {
            id: 1,
            result: Some(json!({"nested": {"deep": true}})),
            error: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(!json.contains('\n'), "serialized JSON must not contain newlines");

        let event = PushEvent {
            event: "test".to_string(),
            data: json!({"list": [1, 2, 3]}),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(!json.contains('\n'), "serialized JSON must not contain newlines");
    }

    #[cfg(unix)]
    mod integration {
        use super::super::*;
        use serde_json::json;
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;
        use std::path::PathBuf;

        use std::sync::atomic::AtomicU64 as TestCounter;

        static TEST_ID: TestCounter = TestCounter::new(0);

        /// Creates a temp socket path and a mock server that echoes back responses.
        fn setup_mock_server() -> (PathBuf, thread::JoinHandle<()>) {
            let id = TEST_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "omnidea-test-{}-{}",
                std::process::id(),
                id
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let sock_path = dir.join("test.sock");

            // Remove stale socket
            let _ = std::fs::remove_file(&sock_path);

            let listener = UnixListener::bind(&sock_path).unwrap();
            let path = sock_path.clone();

            let handle = thread::spawn(move || {
                if let Ok((stream, _)) = listener.accept() {
                    let reader = BufReader::new(stream.try_clone().unwrap());
                    let mut writer = stream;

                    for line in reader.lines() {
                        let line = match line {
                            Ok(l) => l,
                            Err(_) => break,
                        };
                        if line.is_empty() {
                            continue;
                        }

                        // Parse request, send back a simple response
                        if let Ok(req) = serde_json::from_str::<Request>(&line) {
                            let resp = Response {
                                id: req.id,
                                result: Some(json!({"method": req.method, "echo": true})),
                                error: None,
                            };
                            let mut resp_json = serde_json::to_string(&resp).unwrap();
                            resp_json.push('\n');
                            if writer.write_all(resp_json.as_bytes()).is_err() {
                                break;
                            }
                            let _ = writer.flush();
                        }
                    }
                }
            });

            (path, handle)
        }

        #[test]
        fn test_connect_and_call() {
            let (sock_path, _server) = setup_mock_server();

            // Give the server a moment to bind
            thread::sleep(Duration::from_millis(50));

            let client = DaemonClient::connect_to(&sock_path).unwrap();
            let result = client.call("omnibus.status", json!({})).unwrap();

            assert_eq!(result["method"], "omnibus.status");
            assert_eq!(result["echo"], true);

            // Cleanup
            let _ = std::fs::remove_file(&sock_path);
        }

        #[test]
        fn test_multiple_calls_sequential() {
            let (sock_path, _server) = setup_mock_server();
            thread::sleep(Duration::from_millis(50));

            let client = DaemonClient::connect_to(&sock_path).unwrap();

            let r1 = client.call("daemon.status", json!({})).unwrap();
            assert_eq!(r1["method"], "daemon.status");

            let r2 = client.call("tower.status", json!({})).unwrap();
            assert_eq!(r2["method"], "tower.status");

            let r3 = client.call("identity.pubkey", json!({})).unwrap();
            assert_eq!(r3["method"], "identity.pubkey");

            let _ = std::fs::remove_file(&sock_path);
        }

        #[test]
        fn test_convenience_methods() {
            let (sock_path, _server) = setup_mock_server();
            thread::sleep(Duration::from_millis(50));

            let client = DaemonClient::connect_to(&sock_path).unwrap();

            let r = client.daemon_status().unwrap();
            assert_eq!(r["method"], "daemon.status");

            let r = client.omnibus_status().unwrap();
            assert_eq!(r["method"], "omnibus.status");

            let _ = std::fs::remove_file(&sock_path);
        }

        #[test]
        fn test_connection_failed() {
            let result = DaemonClient::connect_to(Path::new("/tmp/nonexistent-daemon.sock"));
            assert!(result.is_err());
            match result.unwrap_err() {
                ClientError::ConnectionFailed(_) => {} // expected
                other => panic!("expected ConnectionFailed, got: {other}"),
            }
        }

        #[test]
        fn test_push_events() {
            let dir = std::env::temp_dir().join(format!("omnidea-event-test-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let sock_path = dir.join("test.sock");
            let _ = std::fs::remove_file(&sock_path);

            let listener = UnixListener::bind(&sock_path).unwrap();

            let _server = thread::spawn(move || {
                if let Ok((stream, _)) = listener.accept() {
                    let mut writer = stream;

                    // Send a push event
                    let event = PushEvent {
                        event: "peer.connected".to_string(),
                        data: json!({"pubkey": "abc123"}),
                    };
                    let mut event_json = serde_json::to_string(&event).unwrap();
                    event_json.push('\n');
                    let _ = writer.write_all(event_json.as_bytes());
                    let _ = writer.flush();

                    // Keep connection open briefly
                    thread::sleep(Duration::from_millis(200));
                }
            });

            thread::sleep(Duration::from_millis(50));

            let client = DaemonClient::connect_to(&sock_path).unwrap();
            let rx = client.subscribe_events().unwrap();

            // Should receive the push event
            let event = rx.recv_timeout(Duration::from_secs(2)).unwrap();
            assert_eq!(event.event, "peer.connected");
            assert_eq!(event.data["pubkey"], "abc123");

            let _ = std::fs::remove_file(&sock_path);
        }
    }
}
