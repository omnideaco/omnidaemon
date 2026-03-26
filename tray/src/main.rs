//! omny-tray -- Omnidea system tray / menu bar app.
//!
//! Shows Omnibus status with an animated spinning pinwheel icon.
//! Communicates with `omny-daemon` via the `omnidea-client` IPC library.

mod animation;
mod icon;
mod status;

use std::time::{Duration, Instant};

use tray_icon::menu::{Menu, MenuEvent, MenuItem, MenuId, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};
use winit::application::ApplicationHandler;
use winit::event::StartCause;
use winit::event_loop::{ActiveEventLoop, EventLoop};

use animation::AnimationController;
use icon::IconFrames;
use status::{StatusCommand, StatusMessage, StatusSnapshot};

// ─── Menu item IDs ─────────────────────────────────────────────────────────

const ID_STATUS_OMNIBUS: &str = "status_omnibus";
const ID_STATUS_PORT: &str = "status_port";
const ID_STATUS_PEERS: &str = "status_peers";
const ID_STATUS_EVENTS: &str = "status_events";
const ID_STATUS_TOWER: &str = "status_tower";
const ID_START: &str = "action_start";
const ID_STOP: &str = "action_stop";
const ID_RESTART: &str = "action_restart";
const ID_SETTINGS: &str = "action_settings";
const ID_QUIT_TRAY: &str = "action_quit_tray";
const ID_STOP_AND_QUIT: &str = "action_stop_and_quit";

/// Interval between animation frame updates when spinning.
const ANIMATION_TICK: Duration = Duration::from_millis(80);

/// Interval between event loop wake-ups for checking status messages.
const EVENT_LOOP_TICK: Duration = Duration::from_millis(100);

// ─── User events ───────────────────────────────────────────────────────────

/// Custom events forwarded into the winit event loop.
#[derive(Debug)]
enum UserEvent {
    TrayIcon(TrayIconEvent),
    Menu(MenuEvent),
}

// ─── Menu items (held for later mutation) ──────────────────────────────────

/// Handles to menu items we need to update at runtime.
struct MenuItems {
    omnibus_status: MenuItem,
    port: MenuItem,
    peers: MenuItem,
    events: MenuItem,
    tower_status: MenuItem,
    start: MenuItem,
    stop: MenuItem,
    restart: MenuItem,
}

// ─── Application ───────────────────────────────────────────────────────────

struct App {
    /// The system tray icon handle.
    tray: Option<TrayIcon>,
    /// Pre-rendered icon frames.
    frames: IconFrames,
    /// Animation state machine.
    animation: AnimationController,
    /// Last time we advanced the animation.
    last_animation_tick: Instant,
    /// Handles to mutable menu items.
    menu_items: Option<MenuItems>,
    /// Sender for commands to the status thread.
    cmd_tx: std::sync::mpsc::Sender<StatusCommand>,
    /// Receiver for status messages from the background thread.
    msg_rx: std::sync::mpsc::Receiver<StatusMessage>,
    /// Last known status snapshot.
    last_status: StatusSnapshot,
}

impl App {
    fn new(
        frames: IconFrames,
        cmd_tx: std::sync::mpsc::Sender<StatusCommand>,
        msg_rx: std::sync::mpsc::Receiver<StatusMessage>,
    ) -> Self {
        let frame_count = frames.spin_frames.len();
        Self {
            tray: None,
            frames,
            animation: AnimationController::new(frame_count),
            last_animation_tick: Instant::now(),
            menu_items: None,
            cmd_tx,
            msg_rx,
            last_status: StatusSnapshot::disconnected(),
        }
    }

    /// Build the tray icon with menu. Called once during event loop init.
    fn build_tray(&mut self) {
        let (menu, items) = build_menu();

        let icon = Icon::from_rgba(
            self.frames.static_frame.clone(),
            self.frames.width,
            self.frames.height,
        )
        .expect("valid icon data");

        let tray = TrayIconBuilder::new()
            .with_icon(icon)
            .with_tooltip("Omnidea")
            .with_menu(Box::new(menu))
            .with_icon_as_template(true) // macOS: adapts to light/dark menu bar
            .build()
            .expect("failed to build tray icon");

        self.tray = Some(tray);
        self.menu_items = Some(items);
    }

    /// Process all pending status messages from the background thread.
    fn drain_status_messages(&mut self) {
        while let Ok(msg) = self.msg_rx.try_recv() {
            match msg {
                StatusMessage::Status(snapshot) => {
                    self.apply_status(snapshot);
                }
                StatusMessage::ActionResult { action, error } => {
                    if let Some(ref err) = error {
                        log::error!("Action '{action}' failed: {err}");
                    } else {
                        log::info!("Action '{action}' completed");
                    }
                    // Trigger immediate re-poll to reflect the change
                    let _ = self.cmd_tx.send(StatusCommand::PollNow);
                }
            }
        }
    }

    /// Apply a status snapshot: update menu text and animation state.
    fn apply_status(&mut self, snapshot: StatusSnapshot) {
        let items = match self.menu_items {
            Some(ref items) => items,
            None => return,
        };

        if !snapshot.daemon_connected {
            items.omnibus_status.set_text("Daemon: Not Running");
            items.port.set_text("  --");
            items.peers.set_text("  --");
            items.events.set_text("  --");
            items.tower_status.set_text("Tower: --");
            items.start.set_enabled(false);
            items.stop.set_enabled(false);
            items.restart.set_enabled(false);
            self.animation.stop_spinning();
        } else {
            // Omnibus
            if let Some(ref omnibus) = snapshot.omnibus {
                if omnibus.running {
                    items.omnibus_status.set_text("Omnibus: Running");
                    items
                        .port
                        .set_text(format!("  Port: {}", omnibus.port.unwrap_or(0)));
                    items
                        .peers
                        .set_text(format!("  Peers: {}", omnibus.peers.unwrap_or(0)));
                    items.events.set_text(format!(
                        "  Events: {}",
                        format_number(omnibus.events.unwrap_or(0))
                    ));
                    items.start.set_enabled(false);
                    items.stop.set_enabled(true);
                    items.restart.set_enabled(true);
                    self.animation.stop_spinning();
                } else {
                    items.omnibus_status.set_text("Omnibus: Stopped");
                    items.port.set_text("  --");
                    items.peers.set_text("  --");
                    items.events.set_text("  --");
                    items.start.set_enabled(true);
                    items.stop.set_enabled(false);
                    items.restart.set_enabled(false);
                    self.animation.stop_spinning();
                }
            } else {
                items.omnibus_status.set_text("Omnibus: Unknown");
                items.start.set_enabled(true);
                items.stop.set_enabled(false);
                items.restart.set_enabled(false);
            }

            // Tower
            if let Some(ref tower) = snapshot.tower {
                if tower.running {
                    let name = tower.name.as_deref().unwrap_or("Active");
                    items.tower_status.set_text(format!("Tower: {name}"));
                } else {
                    items.tower_status.set_text("Tower: Disabled");
                }
            } else {
                items.tower_status.set_text("Tower: --");
            }
        }

        // Update the icon if animation stopped
        if !self.animation.is_spinning() {
            self.set_icon_frame_static();
        }

        self.last_status = snapshot;
    }

    /// Set the tray icon to the static (non-rotated) frame.
    fn set_icon_frame_static(&self) {
        if let Some(ref tray) = self.tray {
            if let Ok(icon) = Icon::from_rgba(
                self.frames.static_frame.clone(),
                self.frames.width,
                self.frames.height,
            ) {
                let _ = tray.set_icon(Some(icon));
            }
        }
    }

    /// Set the tray icon to a specific spin frame.
    fn set_icon_frame(&self, frame_index: usize) {
        if let Some(ref tray) = self.tray {
            if let Some(frame_data) = self.frames.spin_frames.get(frame_index) {
                if let Ok(icon) =
                    Icon::from_rgba(frame_data.clone(), self.frames.width, self.frames.height)
                {
                    let _ = tray.set_icon(Some(icon));
                }
            }
        }
    }

    /// Advance the animation if enough time has passed.
    fn tick_animation(&mut self) {
        if !self.animation.is_spinning() {
            return;
        }
        if self.last_animation_tick.elapsed() >= ANIMATION_TICK {
            self.last_animation_tick = Instant::now();
            if let Some(frame_index) = self.animation.tick() {
                self.set_icon_frame(frame_index);
            }
        }
    }

    /// Handle a menu item click by its ID.
    fn handle_menu_event(&mut self, id: &MenuId, event_loop: &ActiveEventLoop) {
        let id_str = id.as_ref();

        match id_str {
            s if s == ID_START => {
                log::info!("User requested: start");
                self.animation.start_spinning();
                let _ = self.cmd_tx.send(StatusCommand::Start);
            }
            s if s == ID_STOP => {
                log::info!("User requested: stop");
                let _ = self.cmd_tx.send(StatusCommand::Stop);
            }
            s if s == ID_RESTART => {
                log::info!("User requested: restart");
                self.animation.start_spinning();
                let _ = self.cmd_tx.send(StatusCommand::Restart);
            }
            s if s == ID_SETTINGS => {
                open_settings();
            }
            s if s == ID_QUIT_TRAY => {
                log::info!("Quitting tray");
                event_loop.exit();
            }
            s if s == ID_STOP_AND_QUIT => {
                log::info!("Stopping daemon and quitting tray");
                let _ = self.cmd_tx.send(StatusCommand::Stop);
                let _ = self.cmd_tx.send(StatusCommand::Shutdown);
                event_loop.exit();
            }
            _ => {
                log::debug!("Unhandled menu item: {id_str}");
            }
        }
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, _event_loop: &ActiveEventLoop) {}

    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _window_id: winit::window::WindowId,
        _event: winit::event::WindowEvent,
    ) {
    }

    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        // Create the tray icon on first init (must happen after event loop starts on macOS)
        if cause == StartCause::Init {
            #[cfg(not(target_os = "linux"))]
            {
                self.build_tray();
            }

            #[cfg(target_os = "macos")]
            {
                // Set Accessory policy AFTER build_tray() — winit's event loop
                // init sets Regular policy, so we must override it here.
                use objc2_app_kit::NSApplication;
                use objc2_app_kit::NSApplicationActivationPolicy;
                use objc2_foundation::MainThreadMarker;
                let mtm = MainThreadMarker::new().expect("must be on main thread");
                let app = NSApplication::sharedApplication(mtm);
                app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

                // Wake the run loop so the icon appears immediately.
                use objc2_core_foundation::CFRunLoop;
                let rl = CFRunLoop::main().unwrap();
                rl.wake_up();
            }
        }

        // Set the control flow to wake up periodically
        let tick = if self.animation.is_spinning() {
            ANIMATION_TICK
        } else {
            EVENT_LOOP_TICK
        };
        event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(
            Instant::now() + tick,
        ));

        // Process status updates from the background thread
        self.drain_status_messages();

        // Advance animation
        self.tick_animation();
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Menu(event) => {
                self.handle_menu_event(event.id(), event_loop);
            }
            UserEvent::TrayIcon(_event) => {
                // Could handle click/double-click on the icon itself
            }
        }
    }
}

// ─── Menu construction ─────────────────────────────────────────────────────

/// Build the tray context menu and return handles to mutable items.
fn build_menu() -> (Menu, MenuItems) {
    let menu = Menu::new();

    // Status display items (disabled -- informational only)
    let omnibus_status =
        MenuItem::with_id(ID_STATUS_OMNIBUS, "Omnibus: Connecting...", false, None);
    let port = MenuItem::with_id(ID_STATUS_PORT, "  --", false, None);
    let peers = MenuItem::with_id(ID_STATUS_PEERS, "  --", false, None);
    let events = MenuItem::with_id(ID_STATUS_EVENTS, "  --", false, None);

    let tower_status = MenuItem::with_id(ID_STATUS_TOWER, "Tower: --", false, None);

    // Action items
    let start = MenuItem::with_id(ID_START, "Start", false, None);
    let stop = MenuItem::with_id(ID_STOP, "Stop", false, None);
    let restart = MenuItem::with_id(ID_RESTART, "Restart", false, None);
    let settings = MenuItem::with_id(ID_SETTINGS, "Settings...", true, None);
    let quit_tray = MenuItem::with_id(ID_QUIT_TRAY, "Quit Tray", true, None);
    let stop_and_quit =
        MenuItem::with_id(ID_STOP_AND_QUIT, "Stop Daemon && Quit", true, None);

    let _ = menu.append_items(&[
        &omnibus_status,
        &port,
        &peers,
        &events,
        &PredefinedMenuItem::separator(),
        &tower_status,
        &PredefinedMenuItem::separator(),
        &start,
        &stop,
        &restart,
        &PredefinedMenuItem::separator(),
        &settings,
        &PredefinedMenuItem::separator(),
        &quit_tray,
        &stop_and_quit,
    ]);

    let items = MenuItems {
        omnibus_status,
        port,
        peers,
        events,
        tower_status,
        start,
        stop,
        restart,
    };

    (menu, items)
}

// ─── Platform actions ──────────────────────────────────────────────────────

/// Open Omny browser to the settings page.
///
/// Tries to activate the running Omny app. If Omny is hidden (window
/// close just hides), this brings it back. The user navigates to
/// `omny://system/settings` from there.
fn open_settings() {
    log::info!("Opening Omny for settings");

    // Try to find and activate the Omny (omnishell) process.
    // On macOS, try the app bundle first, then the binary directly.
    #[cfg(target_os = "macos")]
    {
        // Try activating via bundle ID (packaged builds)
        let bundle_result = std::process::Command::new("open")
            .args(["-b", "co.omnidea.omny"])
            .spawn();

        if bundle_result.is_err() {
            // Fallback: try to find omnishell next to us or in dev paths
            let candidates = find_omny_binary();
            for path in &candidates {
                if std::path::Path::new(path).exists() {
                    let _ = std::process::Command::new("open").arg(path).spawn();
                    return;
                }
            }
            log::warn!("Could not find Omny binary to open settings");
        }
    }
    #[cfg(target_os = "linux")]
    {
        let candidates = find_omny_binary();
        for path in &candidates {
            if std::path::Path::new(path).exists() {
                let _ = std::process::Command::new(path).spawn();
                return;
            }
        }
        log::warn!("Could not find Omny binary to open settings");
    }
}

/// Find the omnishell binary in common locations.
fn find_omny_binary() -> Vec<String> {
    let mut candidates = Vec::new();

    // Next to the tray binary (packaged builds)
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(format!("{}/omnishell", dir.display()));
        }
    }

    // Dev build paths
    #[cfg(debug_assertions)]
    {
        if let Ok(manifest) = std::env::var("CARGO_MANIFEST_DIR") {
            candidates.push(format!(
                "{}/../../omnishell/target/debug/omnishell",
                manifest
            ));
        }
        // Dev fallback: check common workspace layout
        candidates.push(format!(
            "{}/Developer/Omny/omnishell/target/debug/omnishell",
            std::env::var("HOME").unwrap_or_default()
        ));
    }

    candidates
}

// ─── Helpers ───────────────────────────────────────────────────────────────

/// Format a number with comma separators (e.g., 1247 -> "1,247").
fn format_number(n: u64) -> String {
    let s = n.to_string();
    let mut result = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(ch);
    }
    result.chars().rev().collect()
}

// ─── Entry point ───────────────────────────────────────────────────────────

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .init();

    log::info!("omny-tray starting");

    // Render icon frames from the embedded SVG
    let frames = IconFrames::render().expect("failed to render icon frames");
    log::info!(
        "Rendered {} animation frames at {}x{}",
        frames.spin_frames.len(),
        frames.width,
        frames.height,
    );

    // Spawn background status polling thread
    let (cmd_tx, msg_rx) = status::spawn_status_thread();

    // Build the event loop with custom user events
    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .expect("failed to build event loop");

    // Forward tray and menu events into the event loop
    let proxy = event_loop.create_proxy();
    TrayIconEvent::set_event_handler(Some(move |event| {
        let _ = proxy.send_event(UserEvent::TrayIcon(event));
    }));
    let proxy = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some(move |event| {
        let _ = proxy.send_event(UserEvent::Menu(event));
    }));

    // Run
    let mut app = App::new(frames, cmd_tx, msg_rx);
    event_loop.run_app(&mut app).expect("event loop error");

    log::info!("omny-tray exiting");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_number_zero() {
        assert_eq!(format_number(0), "0");
    }

    #[test]
    fn test_format_number_small() {
        assert_eq!(format_number(42), "42");
        assert_eq!(format_number(999), "999");
    }

    #[test]
    fn test_format_number_thousands() {
        assert_eq!(format_number(1_000), "1,000");
        assert_eq!(format_number(1_247), "1,247");
        assert_eq!(format_number(999_999), "999,999");
    }

    #[test]
    fn test_format_number_millions() {
        assert_eq!(format_number(1_000_000), "1,000,000");
        assert_eq!(format_number(12_345_678), "12,345,678");
    }
}
