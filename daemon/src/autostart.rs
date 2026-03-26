//! Platform autostart configuration for omny-daemon.
//!
//! - **macOS**: launchd plist at `~/Library/LaunchAgents/co.omnidea.daemon.plist`
//! - **Linux**: systemd user unit at `~/.config/systemd/user/omny-daemon.service`
//! - **Windows**: startup folder shortcut (via VBScript)

use std::path::{Path, PathBuf};

/// Install platform autostart configuration.
///
/// Returns `Ok(path)` with the path to the installed file, or an error message.
pub fn install() -> Result<PathBuf, String> {
    let daemon_bin = find_daemon_binary()?;

    #[cfg(target_os = "macos")]
    {
        install_launchd(&daemon_bin)
    }

    #[cfg(target_os = "linux")]
    {
        install_systemd(&daemon_bin)
    }

    #[cfg(target_os = "windows")]
    {
        install_windows_startup(&daemon_bin)
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = daemon_bin;
        Err("Autostart not supported on this platform".into())
    }
}

/// Remove platform autostart configuration.
///
/// Returns `Ok(path)` with the path that was removed, or an error message.
pub fn uninstall() -> Result<PathBuf, String> {
    #[cfg(target_os = "macos")]
    {
        uninstall_launchd()
    }

    #[cfg(target_os = "linux")]
    {
        uninstall_systemd()
    }

    #[cfg(target_os = "windows")]
    {
        uninstall_windows_startup()
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        Err("Autostart not supported on this platform".into())
    }
}

// ─── Helpers ────────────────────────────────────────────────────────────────

/// Find the omny-daemon binary path.
fn find_daemon_binary() -> Result<PathBuf, String> {
    // Use the currently running binary
    std::env::current_exe().map_err(|e| format!("Could not determine binary path: {e}"))
}

// ─── macOS: launchd ─────────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
const LAUNCHD_LABEL: &str = "co.omnidea.daemon";

#[cfg(target_os = "macos")]
fn launchd_plist_path() -> Result<PathBuf, String> {
    let home = std::env::var("HOME").map_err(|_| "HOME not set".to_string())?;
    Ok(Path::new(&home)
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{LAUNCHD_LABEL}.plist")))
}

#[cfg(target_os = "macos")]
fn install_launchd(daemon_bin: &Path) -> Result<PathBuf, String> {
    let plist_path = launchd_plist_path()?;
    let home = std::env::var("HOME").map_err(|_| "HOME not set".to_string())?;

    // Ensure LaunchAgents directory exists
    if let Some(parent) = plist_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create {}: {e}", parent.display()))?;
    }

    let log_dir = Path::new(&home).join(".omnidea").join("logs");
    std::fs::create_dir_all(&log_dir)
        .map_err(|e| format!("Failed to create log dir: {e}"))?;

    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{bin}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <false/>
    <key>StandardOutPath</key>
    <string>{log_dir}/daemon.log</string>
    <key>StandardErrorPath</key>
    <string>{log_dir}/daemon.log</string>
    <key>EnvironmentVariables</key>
    <dict>
        <key>RUST_LOG</key>
        <string>info</string>
    </dict>
</dict>
</plist>
"#,
        label = LAUNCHD_LABEL,
        bin = daemon_bin.display(),
        log_dir = log_dir.display(),
    );

    std::fs::write(&plist_path, plist)
        .map_err(|e| format!("Failed to write plist: {e}"))?;

    // Load the agent
    let status = std::process::Command::new("launchctl")
        .args(["load", "-w"])
        .arg(&plist_path)
        .status()
        .map_err(|e| format!("launchctl load failed: {e}"))?;

    if !status.success() {
        return Err("launchctl load returned non-zero exit code".into());
    }

    Ok(plist_path)
}

#[cfg(target_os = "macos")]
fn uninstall_launchd() -> Result<PathBuf, String> {
    let plist_path = launchd_plist_path()?;

    if !plist_path.exists() {
        return Err(format!("No plist found at {}", plist_path.display()));
    }

    // Unload the agent
    let _ = std::process::Command::new("launchctl")
        .args(["unload", "-w"])
        .arg(&plist_path)
        .status();

    std::fs::remove_file(&plist_path)
        .map_err(|e| format!("Failed to remove plist: {e}"))?;

    Ok(plist_path)
}

// ─── Linux: systemd ─────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
const SYSTEMD_SERVICE_NAME: &str = "omny-daemon.service";

#[cfg(target_os = "linux")]
fn systemd_unit_path() -> Result<PathBuf, String> {
    let home = std::env::var("HOME").map_err(|_| "HOME not set".to_string())?;
    Ok(Path::new(&home)
        .join(".config")
        .join("systemd")
        .join("user")
        .join(SYSTEMD_SERVICE_NAME))
}

#[cfg(target_os = "linux")]
fn install_systemd(daemon_bin: &Path) -> Result<PathBuf, String> {
    let unit_path = systemd_unit_path()?;

    // Ensure directory exists
    if let Some(parent) = unit_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create {}: {e}", parent.display()))?;
    }

    let unit = format!(
        r#"[Unit]
Description=Omnidea Daemon — Omninet node service
After=network.target

[Service]
Type=simple
ExecStart={bin}
Restart=on-failure
RestartSec=5
Environment=RUST_LOG=info

[Install]
WantedBy=default.target
"#,
        bin = daemon_bin.display(),
    );

    std::fs::write(&unit_path, unit)
        .map_err(|e| format!("Failed to write unit file: {e}"))?;

    // Reload systemd and enable the service
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status();

    let status = std::process::Command::new("systemctl")
        .args(["--user", "enable", "--now", SYSTEMD_SERVICE_NAME])
        .status()
        .map_err(|e| format!("systemctl enable failed: {e}"))?;

    if !status.success() {
        return Err("systemctl enable returned non-zero exit code".into());
    }

    Ok(unit_path)
}

#[cfg(target_os = "linux")]
fn uninstall_systemd() -> Result<PathBuf, String> {
    let unit_path = systemd_unit_path()?;

    if !unit_path.exists() {
        return Err(format!("No unit file found at {}", unit_path.display()));
    }

    // Stop and disable the service
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "disable", "--now", SYSTEMD_SERVICE_NAME])
        .status();

    std::fs::remove_file(&unit_path)
        .map_err(|e| format!("Failed to remove unit file: {e}"))?;

    // Reload systemd
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status();

    Ok(unit_path)
}

// ─── Windows: Startup folder ────────────────────────────────────────────────

#[cfg(target_os = "windows")]
fn windows_startup_path() -> Result<PathBuf, String> {
    let appdata = std::env::var("APPDATA")
        .map_err(|_| "APPDATA not set".to_string())?;
    Ok(Path::new(&appdata)
        .join("Microsoft")
        .join("Windows")
        .join("Start Menu")
        .join("Programs")
        .join("Startup")
        .join("omny-daemon.vbs"))
}

#[cfg(target_os = "windows")]
fn install_windows_startup(daemon_bin: &Path) -> Result<PathBuf, String> {
    let startup_path = windows_startup_path()?;

    // VBScript that runs the daemon hidden (no console window)
    let vbs = format!(
        r#"Set WshShell = CreateObject("WScript.Shell")
WshShell.Run """{bin}""", 0, False
"#,
        bin = daemon_bin.display(),
    );

    std::fs::write(&startup_path, vbs)
        .map_err(|e| format!("Failed to write startup script: {e}"))?;

    Ok(startup_path)
}

#[cfg(target_os = "windows")]
fn uninstall_windows_startup() -> Result<PathBuf, String> {
    let startup_path = windows_startup_path()?;

    if !startup_path.exists() {
        return Err(format!("No startup script found at {}", startup_path.display()));
    }

    std::fs::remove_file(&startup_path)
        .map_err(|e| format!("Failed to remove startup script: {e}"))?;

    Ok(startup_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_daemon_binary_returns_path() {
        let bin = find_daemon_binary().unwrap();
        assert!(bin.exists());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn launchd_plist_path_is_valid() {
        let path = launchd_plist_path().unwrap();
        assert!(path.ends_with("co.omnidea.daemon.plist"));
        assert!(path.to_string_lossy().contains("LaunchAgents"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn systemd_unit_path_is_valid() {
        let path = systemd_unit_path().unwrap();
        assert!(path.ends_with("omny-daemon.service"));
        assert!(path.to_string_lossy().contains("systemd/user"));
    }
}
