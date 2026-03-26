//! IPC server for the Omnidea daemon.
//!
//! Dispatches all operations through Equipment's Phone. The 500-line match
//! statement has been replaced by a single `phone.call_raw()`.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::state::DaemonState;
use crate::transport::{PlatformListener, PlatformStream};

use equipment::PhoneError;
use omnibus::Omnibus;
use omny_client::{ClientType, Handshake, HandshakeResponse, PushEvent, Request, Response, RpcError};
use serde_json::{json, Value};

/// IPC server — dispatches all operations through Equipment's Phone.
pub struct IpcServer {
    state: Arc<DaemonState>,
    socket_path: PathBuf,
}

impl IpcServer {
    pub fn new(state: Arc<DaemonState>, socket_path: PathBuf) -> Self {
        Self { state, socket_path }
    }

    /// Start listening for client connections. Blocks until shutdown.
    pub fn run(&self) -> std::io::Result<()> {
        let listener = PlatformListener::bind(&self.socket_path)?;

        while !self.state.shutdown.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok(Some(stream)) => {
                    let state = Arc::clone(&self.state);
                    thread::Builder::new()
                        .name("ipc-client".into())
                        .spawn(move || {
                            if let Err(e) = handle_client(stream, &state) {
                                log::debug!("Client disconnected: {e}");
                            }
                        })
                        .ok();
                }
                Ok(None) => thread::sleep(Duration::from_millis(100)),
                Err(e) => {
                    log::error!("Accept error: {e}");
                    thread::sleep(Duration::from_millis(100));
                }
            }
        }

        log::info!("IPC server shutting down");
        Ok(())
    }

    /// Clean up the socket/pipe on shutdown.
    pub fn cleanup(&self) {
        PlatformListener::cleanup(&self.socket_path);
    }
}

// ── Client Handler ──────────────────────────────────────────────────

fn handle_client(
    stream: PlatformStream,
    state: &Arc<DaemonState>,
) -> std::io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(300)))?;

    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;

    // Authenticate
    let client_type = match handle_handshake(&mut reader, &mut writer, &state.auth_token) {
        Ok(ct) => {
            log::debug!("Client authenticated as {ct}");
            ct
        }
        Err(_) => {
            log::debug!("Client connected without handshake (legacy mode)");
            ClientType::Cli
        }
    };

    let subscribed = Arc::new(AtomicBool::new(false));

    for line in reader.lines() {
        if state.shutdown.load(Ordering::Relaxed) {
            break;
        }

        let line = match line {
            Ok(l) => l,
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => continue,
            Err(e) => return Err(e),
        };

        if line.is_empty() {
            continue;
        }

        let request: Request = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                log::warn!("Invalid request JSON: {e}");
                write_response(&mut writer, &Response {
                    id: 0, result: None,
                    error: Some(RpcError { code: -32700, message: format!("parse error: {e}") }),
                })?;
                continue;
            }
        };

        // Permission gate
        if let Err(deny) = check_permission(&client_type, &request.method) {
            write_response(&mut writer, &Response {
                id: request.id, result: None, error: Some(deny),
            })?;
            continue;
        }

        // Dispatch through Equipment's Phone
        let response = match dispatch(&request.method, &request.params, state) {
            Ok(result) => Response { id: request.id, result: Some(result), error: None },
            Err(e) => Response { id: request.id, result: None, error: Some(e) },
        };

        write_response(&mut writer, &response)?;

        // Event forwarding
        if request.method == "events.subscribe"
            && response.error.is_none()
            && !subscribed.swap(true, Ordering::SeqCst)
        {
            start_event_forwarder(
                state.omnibus.omnibus(),
                writer.try_clone()?,
                Arc::clone(&subscribed),
                Arc::clone(state),
            );
        }

        if request.method == "daemon.stop" && response.error.is_none() {
            break;
        }
    }

    Ok(())
}

// ── Dispatch ────────────────────────────────────────────────────────

/// Dispatch an RPC request through Equipment's Phone.
///
/// This replaces the 500-line match statement. All operations are Phone handlers:
/// ~484 auto-registered from divinity_ffi.h + ~40 hand-written Rust-native overrides.
///
/// Waits for the daemon to be fully ready before dispatching. This allows the
/// IPC server to accept connections during boot — clients connect instantly and
/// their first call blocks briefly until Equipment registration completes.
fn dispatch(
    method: &str,
    params: &Value,
    state: &Arc<DaemonState>,
) -> Result<Value, RpcError> {
    // Wait for Equipment to be fully registered before dispatching.
    if !state.wait_ready() {
        return Err(rpc_err(-3, "daemon is shutting down"));
    }

    let input = serde_json::to_vec(params)
        .map_err(|e| rpc_err(-1, format!("serialize: {e}")))?;

    match state.phone.call_raw(method, &input) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| rpc_err(-1, format!("deserialize response: {e}"))),
        Err(PhoneError::NoHandler(_)) => Err(rpc_err(-32601, format!("unknown method: {method}"))),
        Err(PhoneError::HandlerFailed { message, .. }) => Err(rpc_err(-1, message)),
        Err(PhoneError::Serialization { source, .. }) => Err(rpc_err(-2, source.to_string())),
    }
}

// ── Permission ──────────────────────────────────────────────────────

const RESTRICTED_METHODS: &[&str] = &[
    "daemon.stop", "daemon.shutdown",
    "crown.delete", "crown.import",
    "config.set", "config.reload",
    "omnibus.stop", "omnibus.restart",
    "tower.start", "tower.stop",
];

fn check_permission(client_type: &ClientType, method: &str) -> Result<(), RpcError> {
    match client_type {
        ClientType::Beryllium | ClientType::Tray | ClientType::Cli => Ok(()),
        ClientType::Program => {
            if RESTRICTED_METHODS.contains(&method) {
                Err(rpc_err(-5, format!("Permission denied: {method}")))
            } else {
                Ok(())
            }
        }
    }
}

// ── Handshake ───────────────────────────────────────────────────────

fn handle_handshake(
    reader: &mut BufReader<PlatformStream>,
    writer: &mut PlatformStream,
    auth_token: &str,
) -> Result<ClientType, String> {
    let mut first_line = String::new();
    reader
        .read_line(&mut first_line)
        .map_err(|e| format!("read error: {e}"))?;

    let handshake: Handshake =
        serde_json::from_str(first_line.trim()).map_err(|e| format!("parse: {e}"))?;

    if !crate::auth::verify_token(auth_token, &handshake.auth) {
        let _ = write_response(
            writer,
            &Response {
                id: 0,
                result: None,
                error: Some(rpc_err(-6, "Authentication failed")),
            },
        );
        return Err("bad token".into());
    }

    let session_id = format!("{:016x}", rand_u64());
    let resp = HandshakeResponse {
        auth: "ok".into(),
        session_id: Some(session_id),
        client_type: Some(handshake.client_type.clone()),
    };

    let mut json = serde_json::to_string(&resp).map_err(|e| format!("serialize: {e}"))?;
    json.push('\n');
    writer
        .write_all(json.as_bytes())
        .map_err(|e| format!("write: {e}"))?;
    writer.flush().map_err(|e| format!("flush: {e}"))?;

    Ok(handshake.client_type)
}

fn rand_u64() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    t ^ (std::process::id() as u64).wrapping_mul(0x9E3779B97F4A7C15)
}

// ── Wire helpers ────────────────────────────────────────────────────

fn write_response(writer: &mut PlatformStream, response: &Response) -> std::io::Result<()> {
    let mut json = serde_json::to_string(response)
        .map_err(|e| std::io::Error::other(format!("serialize error: {e}")))?;
    json.push('\n');
    writer.write_all(json.as_bytes())?;
    writer.flush()
}

fn write_push_event(writer: &mut PlatformStream, event: &PushEvent) -> std::io::Result<()> {
    let mut json = serde_json::to_string(event)
        .map_err(|e| std::io::Error::other(format!("serialize error: {e}")))?;
    json.push('\n');
    writer.write_all(json.as_bytes())?;
    writer.flush()
}

fn start_event_forwarder(
    omnibus: &Omnibus,
    mut writer: PlatformStream,
    subscribed: Arc<AtomicBool>,
    state: Arc<DaemonState>,
) {
    let mut rx = omnibus.subscribe_events();

    thread::Builder::new()
        .name("ipc-events".into())
        .spawn(move || {
            while subscribed.load(Ordering::Relaxed) && !state.shutdown.load(Ordering::Relaxed) {
                match rx.try_recv() {
                    Ok(omnibus_event) => {
                        let push = omnibus_event_to_push(&omnibus_event);
                        if write_push_event(&mut writer, &push).is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {
                        thread::sleep(Duration::from_millis(50));
                    }
                    Err(tokio::sync::broadcast::error::TryRecvError::Lagged(n)) => {
                        log::warn!("Event forwarder lagged, dropped {n} events");
                    }
                    Err(tokio::sync::broadcast::error::TryRecvError::Closed) => {
                        break;
                    }
                }
            }
        })
        .ok();
}

fn omnibus_event_to_push(event: &omnibus::OmnibusEvent) -> PushEvent {
    match event {
        omnibus::OmnibusEvent::Started => PushEvent {
            event: "omnibus.started".into(),
            data: json!({}),
        },
        omnibus::OmnibusEvent::Stopped => PushEvent {
            event: "omnibus.stopped".into(),
            data: json!({}),
        },
        omnibus::OmnibusEvent::PeerConnected { pubkey } => PushEvent {
            event: "peer.connected".into(),
            data: json!({ "pubkey": pubkey }),
        },
        omnibus::OmnibusEvent::PeerDisconnected { pubkey } => PushEvent {
            event: "peer.disconnected".into(),
            data: json!({ "pubkey": pubkey }),
        },
        omnibus::OmnibusEvent::EventReceived { event_json } => PushEvent {
            event: "event.received".into(),
            data: json!({ "event": event_json }),
        },
        omnibus::OmnibusEvent::HealthChanged { score } => PushEvent {
            event: "health.changed".into(),
            data: json!({ "score": score }),
        },
    }
}

fn rpc_err(code: i32, message: impl Into<String>) -> RpcError {
    RpcError { code, message: message.into() }
}
