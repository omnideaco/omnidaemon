//! Configuration loading for the Omnidea daemon.
//!
//! Loads `DaemonConfig` from `~/.omnidea/config.toml`, creating a default
//! file if one doesn't exist. The config controls Omnibus runtime settings
//! and Tower mode.

use std::path::{Path, PathBuf};

use omnibus::DaemonConfig;
use tower::{TowerConfig, TowerMode};

/// Returns the path to the Omnidea home directory: `~/.omnidea/`.
pub fn omnidea_home() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    Path::new(&home).join(".omnidea")
}

/// Returns the path to the config file: `~/.omnidea/config.toml`.
pub fn config_path() -> PathBuf {
    omnidea_home().join("config.toml")
}

/// Returns the path to the pidfile: `~/.omnidea/daemon.pid`.
pub fn pidfile_path() -> PathBuf {
    omnidea_home().join("daemon.pid")
}

/// Default TOML content for a fresh config file.
const DEFAULT_CONFIG_TOML: &str = r#"# Omnidea Daemon Configuration
# https://omnidea.co

[omnibus]
# Port for the local relay server. 0 = OS-assigned.
port = 4040
# Bind to all interfaces (true = LAN-reachable, false = localhost only).
bind_all = false
# Human-readable device name for mDNS discovery.
device_name = "Omnidea Device"
# Directory for persistent data (Soul, relay DB). Uses ~/.omnidea/data if unset.
# data_dir = "/path/to/data"
# Attempt UPnP port mapping (requires consent).
enable_upnp = false
# Home node URL for persistent sync (optional).
# home_node = "ws://192.168.1.10:4040"

[tower]
# Enable Tower mode to run an always-on network node.
# When enabled, Tower wraps Omnibus with gospel peering, search, and content policy.
# When disabled, the daemon runs standalone Omnibus only.
enabled = false
# Tower mode:
#   "pharos"  — lightweight directory node, gospel records only (Raspberry Pi friendly)
#   "harbor"  — community content node, gospel + member content storage
mode = "pharos"
# Human-readable Tower name (shown in lighthouse announcements).
name = "My Tower"
# Seed relay URLs for gospel peering (connects on startup).
seeds = []
# Community pubkeys this Harbor serves (Harbor mode only, empty = open Harbor).
communities = []
# Public URL for lighthouse announcements (optional, auto-detected if unset).
# public_url = "wss://my-tower.example.com:7777"
# Lighthouse announcement interval in seconds (default: 300 = 5 min).
# announce_interval_secs = 300
# Gospel bilateral sync interval in seconds (default: 60).
# gospel_interval_secs = 60
# Gospel live sync polling interval in seconds (default: 2).
# gospel_live_interval_secs = 2
"#;

/// Convert `DaemonConfig`'s `TowerSection` into a `tower::TowerConfig`.
///
/// The mapping lives in the daemon (not Omnibus) because the daemon knows
/// both the file-format types and the runtime types.
pub fn to_tower_config(daemon_config: &DaemonConfig, data_dir: &Path) -> TowerConfig {
    let mode = match daemon_config.tower.mode.as_str() {
        "harbor" => TowerMode::Harbor,
        "intermediary" => TowerMode::Intermediary,
        _ => TowerMode::Pharos,
    };

    let seed_peers = daemon_config
        .tower
        .seeds
        .iter()
        .filter_map(|s| s.parse().ok())
        .collect();

    let public_url = daemon_config
        .tower
        .public_url
        .as_deref()
        .and_then(|s| s.parse().ok());

    TowerConfig {
        mode,
        name: daemon_config.tower.name.clone(),
        data_dir: data_dir.join("tower"),
        port: daemon_config.omnibus.port,
        bind_all: daemon_config.omnibus.bind_all,
        seed_peers,
        public_url,
        gospel_interval_secs: daemon_config.tower.gospel_interval_secs.unwrap_or(60),
        announce_interval_secs: daemon_config.tower.announce_interval_secs.unwrap_or(300),
        gospel_live_interval_secs: daemon_config.tower.gospel_live_interval_secs.unwrap_or(2),
        communities: daemon_config.tower.communities.clone(),
        ..Default::default()
    }
}

/// Save the current config to disk at `~/.omnidea/config.toml`.
pub fn save_config(config: &DaemonConfig) -> Result<(), String> {
    let path = config_path();
    let toml_str =
        toml::to_string_pretty(config).map_err(|e| format!("serialize config: {e}"))?;
    std::fs::write(&path, toml_str).map_err(|e| format!("write config: {e}"))?;
    Ok(())
}

/// Load config from disk, or create a default one if it doesn't exist.
///
/// If the config file is missing, writes a sensible default to
/// `~/.omnidea/config.toml` and returns the parsed result. If the file
/// exists but fails to parse, logs a warning and falls back to defaults.
pub fn load_or_create_default() -> DaemonConfig {
    let path = config_path();

    if path.exists() {
        match DaemonConfig::load(&path) {
            Ok(config) => return config,
            Err(e) => {
                log::warn!(
                    "Failed to load config from {}: {e}. Using defaults.",
                    path.display()
                );
            }
        }
    } else {
        // Create default config file.
        let home = omnidea_home();
        if let Err(e) = std::fs::create_dir_all(&home) {
            log::warn!("Could not create {}: {e}", home.display());
        }

        if let Err(e) = std::fs::write(&path, DEFAULT_CONFIG_TOML) {
            log::warn!(
                "Could not write default config to {}: {e}",
                path.display()
            );
        } else {
            log::info!("Created default config at {}", path.display());
        }
    }

    // Parse the default TOML string directly.
    toml::from_str(DEFAULT_CONFIG_TOML).unwrap_or_else(|e| {
        log::error!("BUG: default config TOML failed to parse: {e}");
        // Absolute fallback — construct manually.
        toml::from_str("[omnibus]\n").expect("minimal TOML must parse")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_omnidea_home_is_under_home_dir() {
        let home = omnidea_home();
        assert!(home.ends_with(".omnidea"));
    }

    #[test]
    fn test_config_path_ends_with_toml() {
        let path = config_path();
        assert!(path.ends_with("config.toml"));
    }

    #[test]
    fn test_pidfile_path_ends_with_pid() {
        let path = pidfile_path();
        assert!(path.ends_with("daemon.pid"));
    }

    #[test]
    fn test_default_config_toml_parses() {
        let config: DaemonConfig = toml::from_str(DEFAULT_CONFIG_TOML).unwrap();
        assert_eq!(config.omnibus.port, 4040);
        assert!(!config.omnibus.bind_all);
        assert_eq!(config.omnibus.device_name, "Omnidea Device");
        assert!(!config.tower.enabled);
        assert_eq!(config.tower.mode, "pharos");
    }

    #[test]
    fn test_load_or_create_default_returns_valid_config() {
        // This test runs against whatever state ~/.omnidea/config.toml is in.
        // It should never panic.
        let config = load_or_create_default();
        // Just verify the config loaded without panicking.
        // device_name should be non-empty.
        assert!(!config.omnibus.device_name.is_empty());
    }

    #[test]
    fn test_to_tower_config_pharos() {
        let daemon_config: DaemonConfig = toml::from_str(
            r#"
[omnibus]
port = 5050
bind_all = true
device_name = "Test"

[tower]
enabled = true
mode = "pharos"
name = "Test Pharos"
seeds = ["ws://seed1.example.com"]
"#,
        )
        .unwrap();

        let data_dir = std::env::temp_dir().join("test_tower_config");
        let tower_config = to_tower_config(&daemon_config, &data_dir);

        assert_eq!(tower_config.mode, TowerMode::Pharos);
        assert_eq!(tower_config.name, "Test Pharos");
        assert_eq!(tower_config.data_dir, data_dir.join("tower"));
        assert_eq!(tower_config.port, 5050);
        assert!(tower_config.bind_all);
        assert_eq!(tower_config.seed_peers.len(), 1);
        assert_eq!(tower_config.gospel_interval_secs, 60);
        assert_eq!(tower_config.announce_interval_secs, 300);
        assert_eq!(tower_config.gospel_live_interval_secs, 2);
    }

    #[test]
    fn test_to_tower_config_harbor() {
        let daemon_config: DaemonConfig = toml::from_str(
            r#"
[omnibus]
port = 7777

[tower]
enabled = true
mode = "harbor"
name = "Community Harbor"
communities = ["pubkey_a", "pubkey_b"]
announce_interval_secs = 120
gospel_interval_secs = 30
gospel_live_interval_secs = 5
public_url = "wss://harbor.example.com"
"#,
        )
        .unwrap();

        let data_dir = std::env::temp_dir().join("test_tower_harbor");
        let tower_config = to_tower_config(&daemon_config, &data_dir);

        assert_eq!(tower_config.mode, TowerMode::Harbor);
        assert_eq!(tower_config.name, "Community Harbor");
        assert_eq!(tower_config.communities, vec!["pubkey_a", "pubkey_b"]);
        assert_eq!(tower_config.announce_interval_secs, 120);
        assert_eq!(tower_config.gospel_interval_secs, 30);
        assert_eq!(tower_config.gospel_live_interval_secs, 5);
        assert!(tower_config.public_url.is_some());
    }

    #[test]
    fn test_to_tower_config_unknown_mode_defaults_to_pharos() {
        let daemon_config: DaemonConfig = toml::from_str(
            r#"
[omnibus]

[tower]
mode = "unknown_mode"
"#,
        )
        .unwrap();

        let data_dir = std::env::temp_dir();
        let tower_config = to_tower_config(&daemon_config, &data_dir);
        assert_eq!(tower_config.mode, TowerMode::Pharos);
    }

    #[test]
    fn test_to_tower_config_invalid_seed_skipped() {
        let daemon_config: DaemonConfig = toml::from_str(
            r#"
[omnibus]

[tower]
seeds = ["ws://valid.example.com", "not a url at all"]
"#,
        )
        .unwrap();

        let data_dir = std::env::temp_dir();
        let tower_config = to_tower_config(&daemon_config, &data_dir);
        // Only valid URL is included.
        assert_eq!(tower_config.seed_peers.len(), 1);
    }

    #[test]
    fn test_save_config_roundtrip() {
        // Parse default, save, reload — should be equivalent.
        let config: DaemonConfig = toml::from_str(DEFAULT_CONFIG_TOML).unwrap();
        let toml_str = toml::to_string_pretty(&config).unwrap();
        let reloaded: DaemonConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(config.omnibus.port, reloaded.omnibus.port);
        assert_eq!(config.omnibus.bind_all, reloaded.omnibus.bind_all);
        assert_eq!(config.tower.enabled, reloaded.tower.enabled);
        assert_eq!(config.tower.mode, reloaded.tower.mode);
    }
}
