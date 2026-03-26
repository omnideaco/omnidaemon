//! Daemon status polling and connection management.
//!
//! Runs on a background thread, periodically querying the daemon for
//! Omnibus and Tower status. Sends status snapshots to the main thread
//! for menu updates and animation control.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use omny_client::DaemonClient;
use serde_json::Value;

/// How often to poll daemon status (seconds).
const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Snapshot of daemon/omnibus/tower status sent to the main thread.
#[derive(Debug, Clone)]
pub struct StatusSnapshot {
    /// Whether the daemon is reachable.
    pub daemon_connected: bool,
    /// Omnibus status (if available).
    pub omnibus: Option<OmnibusStatus>,
    /// Tower status (if available).
    pub tower: Option<TowerStatus>,
}

/// Omnibus node runtime status.
#[derive(Debug, Clone)]
pub struct OmnibusStatus {
    pub running: bool,
    pub port: Option<u16>,
    pub peers: Option<u32>,
    pub events: Option<u64>,
}

/// Tower service status.
#[derive(Debug, Clone)]
pub struct TowerStatus {
    pub running: bool,
    pub name: Option<String>,
}

/// Commands sent from the main thread to the status thread.
#[derive(Debug)]
pub enum StatusCommand {
    /// Request a fresh status poll immediately.
    PollNow,
    /// Tell the daemon to start Omnibus.
    Start,
    /// Tell the daemon to stop Omnibus.
    Stop,
    /// Tell the daemon to restart Omnibus.
    Restart,
    /// Shut down the status thread.
    Shutdown,
}

/// Messages sent from the status thread to the main thread.
#[derive(Debug)]
pub enum StatusMessage {
    /// Fresh status snapshot.
    Status(StatusSnapshot),
    /// An action completed (start/stop/restart) with optional error.
    ActionResult {
        action: &'static str,
        error: Option<String>,
    },
}

impl StatusSnapshot {
    /// A disconnected snapshot (daemon not reachable).
    pub fn disconnected() -> Self {
        Self {
            daemon_connected: false,
            omnibus: None,
            tower: None,
        }
    }
}

/// Spawn the background status polling thread.
///
/// Returns a sender for commands and a receiver for status messages.
pub fn spawn_status_thread() -> (mpsc::Sender<StatusCommand>, mpsc::Receiver<StatusMessage>) {
    let (cmd_tx, cmd_rx) = mpsc::channel::<StatusCommand>();
    let (msg_tx, msg_rx) = mpsc::channel::<StatusMessage>();

    std::thread::Builder::new()
        .name("omny-tray-status".to_string())
        .spawn(move || {
            status_loop(cmd_rx, msg_tx);
        })
        .expect("failed to spawn status thread");

    (cmd_tx, msg_rx)
}

/// Main status polling loop. Runs until a Shutdown command is received.
fn status_loop(cmd_rx: mpsc::Receiver<StatusCommand>, msg_tx: mpsc::Sender<StatusMessage>) {
    let mut client: Option<DaemonClient> = None;
    let mut last_poll = Instant::now() - POLL_INTERVAL; // poll immediately on start

    loop {
        // Check for commands (non-blocking)
        match cmd_rx.try_recv() {
            Ok(StatusCommand::Shutdown) => {
                log::info!("Status thread shutting down");
                return;
            }
            Ok(StatusCommand::PollNow) => {
                last_poll = Instant::now() - POLL_INTERVAL; // force immediate poll
            }
            Ok(StatusCommand::Start) => {
                handle_action(&client, "start", |c| c.omnibus_start(), &msg_tx);
                last_poll = Instant::now() - POLL_INTERVAL; // poll after action
            }
            Ok(StatusCommand::Stop) => {
                handle_action(&client, "stop", |c| c.omnibus_stop(), &msg_tx);
                last_poll = Instant::now() - POLL_INTERVAL;
            }
            Ok(StatusCommand::Restart) => {
                handle_action(&client, "restart", |c| c.omnibus_restart(), &msg_tx);
                last_poll = Instant::now() - POLL_INTERVAL;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                log::info!("Command channel closed, shutting down");
                return;
            }
        }

        // Time to poll?
        if last_poll.elapsed() >= POLL_INTERVAL {
            last_poll = Instant::now();

            // Try to connect if we don't have a client
            if client.is_none() {
                match DaemonClient::connect() {
                    Ok(c) => {
                        log::info!("Connected to daemon");
                        client = Some(c);
                    }
                    Err(e) => {
                        log::debug!("Daemon not reachable: {e}");
                        let _ = msg_tx.send(StatusMessage::Status(StatusSnapshot::disconnected()));
                        // Sleep a bit before next attempt
                        std::thread::sleep(Duration::from_millis(100));
                        continue;
                    }
                }
            }

            if let Some(ref c) = client {
                let snapshot = poll_status(c);
                if !snapshot.daemon_connected {
                    // Lost connection, drop client so we reconnect next cycle
                    log::warn!("Lost connection to daemon");
                    client = None;
                }
                let _ = msg_tx.send(StatusMessage::Status(snapshot));
            }
        }

        // Sleep briefly to avoid busy-spinning
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Poll the daemon for current status.
fn poll_status(client: &DaemonClient) -> StatusSnapshot {
    let omnibus = match client.omnibus_status() {
        Ok(val) => Some(parse_omnibus_status(&val)),
        Err(e) => {
            log::debug!("omnibus.status failed: {e}");
            // If we get a disconnect, signal it
            if matches!(e, omny_client::ClientError::Disconnected | omny_client::ClientError::Timeout) {
                return StatusSnapshot::disconnected();
            }
            None
        }
    };

    let tower = match client.tower_status() {
        Ok(val) => Some(parse_tower_status(&val)),
        Err(e) => {
            log::debug!("tower.status failed: {e}");
            None
        }
    };

    StatusSnapshot {
        daemon_connected: true,
        omnibus,
        tower,
    }
}

/// Parse the JSON response from `omnibus.status` into typed status.
///
/// `omnibus.status` returns OmnibusStatus fields directly:
/// `relay_port`, `has_identity`, `discovered_peers`, `relay_connections`, etc.
/// If we got a response at all, Omnibus is running.
fn parse_omnibus_status(val: &Value) -> OmnibusStatus {
    OmnibusStatus {
        running: true, // If we got a response, it's running
        port: val
            .get("relay_port")
            .and_then(|v| v.as_u64())
            .map(|p| p as u16),
        peers: val
            .get("discovered_peers")
            .and_then(|v| v.as_u64())
            .map(|p| p as u32),
        events: val
            .get("relay_connections")
            .and_then(|v| v.as_u64()),
    }
}

/// Parse the JSON response from `tower.status` into typed status.
fn parse_tower_status(val: &Value) -> TowerStatus {
    TowerStatus {
        running: val.get("running").and_then(|v| v.as_bool()).unwrap_or(false),
        name: val
            .get("name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
    }
}

/// Execute a daemon action and send the result to the main thread.
fn handle_action<F>(
    client: &Option<DaemonClient>,
    action: &'static str,
    f: F,
    msg_tx: &mpsc::Sender<StatusMessage>,
) where
    F: FnOnce(&DaemonClient) -> Result<Value, omny_client::ClientError>,
{
    let result = match client {
        Some(c) => match f(c) {
            Ok(_) => {
                log::info!("{action} succeeded");
                None
            }
            Err(e) => {
                log::error!("{action} failed: {e}");
                Some(format!("{e}"))
            }
        },
        None => {
            log::warn!("{action} requested but not connected to daemon");
            Some("not connected to daemon".to_string())
        }
    };

    let _ = msg_tx.send(StatusMessage::ActionResult {
        action,
        error: result,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_parse_omnibus_status_running() {
        let val = json!({
            "relay_port": 4869,
            "discovered_peers": 3,
            "relay_connections": 1247,
            "has_identity": true
        });
        let status = parse_omnibus_status(&val);
        assert!(status.running);
        assert_eq!(status.port, Some(4869));
        assert_eq!(status.peers, Some(3));
        assert_eq!(status.events, Some(1247));
    }

    #[test]
    fn test_parse_omnibus_status_minimal() {
        // Any response means running
        let val = json!({});
        let status = parse_omnibus_status(&val);
        assert!(status.running);
        assert_eq!(status.port, None);
        assert_eq!(status.peers, None);
        assert_eq!(status.events, None);
    }

    #[test]
    fn test_parse_tower_status_running() {
        let val = json!({"running": true, "name": "Harbor"});
        let status = parse_tower_status(&val);
        assert!(status.running);
        assert_eq!(status.name.as_deref(), Some("Harbor"));
    }

    #[test]
    fn test_parse_tower_status_disabled() {
        let val = json!({"running": false});
        let status = parse_tower_status(&val);
        assert!(!status.running);
        assert_eq!(status.name, None);
    }

    #[test]
    fn test_disconnected_snapshot() {
        let snap = StatusSnapshot::disconnected();
        assert!(!snap.daemon_connected);
        assert!(snap.omnibus.is_none());
        assert!(snap.tower.is_none());
    }
}
