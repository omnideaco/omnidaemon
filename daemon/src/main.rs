//! # omny-daemon
//!
//! Headless service that owns the Omnibus runtime and serves IPC requests
//! over a Unix domain socket at `~/.omnidea/daemon.sock`.
//!
//! When Tower mode is enabled in the config, the daemon starts Tower
//! (which owns Omnibus internally) instead of standalone Omnibus.
//!
//! ## Usage
//!
//! ```text
//! omny-daemon              # run in foreground
//! omny-daemon --daemon     # daemonize (double-fork, detach, log to file)
//! omny-daemon status       # query running daemon status
//! omny-daemon stop         # stop running daemon
//! omny-daemon install      # install platform autostart
//! omny-daemon uninstall    # remove platform autostart
//! ```

mod autostart;

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use omnibus::Omnibus;

use omny_daemon::auth;
use omny_daemon::config::{self, load_or_create_default, omnidea_home, pidfile_path, to_tower_config};
use omny_daemon::ffi_ops;
use omny_daemon::modifiers;
use omny_daemon::modules;
use omny_daemon::server::IpcServer;
use omny_daemon::state;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(|s| s.as_str());

    match command {
        Some("status") => cmd_status(),
        Some("stop") => cmd_stop(),
        Some("install") => cmd_install(),
        Some("uninstall") => cmd_uninstall(),
        Some("--daemon") => cmd_daemonize(),
        Some("--help" | "-h") => print_usage(),
        Some(unknown) => {
            eprintln!("Unknown command: {unknown}");
            print_usage();
            std::process::exit(1);
        }
        None => run_foreground(),
    }
}

/// Print usage information.
fn print_usage() {
    println!(
        "omny-daemon — Omnidea node service\n\
         \n\
         Usage:\n\
         \x20 omny-daemon              Run in foreground\n\
         \x20 omny-daemon --daemon     Daemonize (background, log to ~/.omnidea/daemon.log)\n\
         \x20 omny-daemon status       Query running daemon status\n\
         \x20 omny-daemon stop         Stop running daemon\n\
         \x20 omny-daemon install      Install platform autostart\n\
         \x20 omny-daemon uninstall    Remove platform autostart\n\
         \x20 omny-daemon --help       Show this message"
    );
}

/// Install platform autostart configuration.
fn cmd_install() {
    match autostart::install() {
        Ok(path) => {
            println!("Autostart installed: {}", path.display());
            println!("omny-daemon will start automatically on login.");
        }
        Err(e) => {
            eprintln!("Failed to install autostart: {e}");
            std::process::exit(1);
        }
    }
}

/// Remove platform autostart configuration.
fn cmd_uninstall() {
    match autostart::uninstall() {
        Ok(path) => {
            println!("Autostart removed: {}", path.display());
            println!("omny-daemon will no longer start on login.");
        }
        Err(e) => {
            eprintln!("Failed to remove autostart: {e}");
            std::process::exit(1);
        }
    }
}

/// Daemonize the process using the classic double-fork pattern, then run the
/// daemon server in the background.
///
/// 1. First fork — parent prints PID and exits.
/// 2. `setsid()` — new session, detached from terminal.
/// 3. Second fork — session leader exits so the grandchild can never reacquire
///    a controlling terminal.
/// 4. Redirect stdin/stdout/stderr to /dev/null (logging goes to file).
/// 5. Call `run_foreground()` which handles pidfile, IPC, Equipment, etc.
#[cfg(unix)]
fn cmd_daemonize() {
    use std::fs::OpenOptions;
    use std::os::unix::io::IntoRawFd;

    // Ensure ~/.omnidea/ exists before forking so both parent and child agree
    // on directory state.
    let home = omnidea_home();
    if let Err(e) = std::fs::create_dir_all(&home) {
        eprintln!("Failed to create {}: {e}", home.display());
        std::process::exit(1);
    }

    // ── First fork ──────────────────────────────────────────────────────
    let pid = unsafe { libc::fork() };
    match pid {
        -1 => {
            eprintln!("fork() failed: {}", std::io::Error::last_os_error());
            std::process::exit(1);
        }
        0 => {
            // Child continues below.
        }
        _child_pid => {
            // Parent — report and exit.
            // The actual daemon PID won't be known until after the second fork,
            // so we just confirm that daemonization started. The pidfile will
            // contain the final PID once the daemon is running.
            println!("omny-daemon daemonizing (pidfile: {})", pidfile_path().display());
            std::process::exit(0);
        }
    }

    // ── New session ─────────────────────────────────────────────────────
    if unsafe { libc::setsid() } == -1 {
        eprintln!("setsid() failed: {}", std::io::Error::last_os_error());
        std::process::exit(1);
    }

    // ── Second fork ─────────────────────────────────────────────────────
    let pid2 = unsafe { libc::fork() };
    match pid2 {
        -1 => {
            eprintln!("second fork() failed: {}", std::io::Error::last_os_error());
            std::process::exit(1);
        }
        0 => {
            // Grandchild continues below.
        }
        _grandchild_pid => {
            // Session leader exits.
            std::process::exit(0);
        }
    }

    // ── Redirect stdio ──────────────────────────────────────────────────
    // Open /dev/null for stdin.
    let devnull = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/null")
        .expect("/dev/null must be openable");
    let devnull_fd = devnull.into_raw_fd();

    unsafe {
        libc::dup2(devnull_fd, libc::STDIN_FILENO);
        libc::dup2(devnull_fd, libc::STDOUT_FILENO);
        libc::dup2(devnull_fd, libc::STDERR_FILENO);
        if devnull_fd > libc::STDERR_FILENO {
            libc::close(devnull_fd);
        }
    }

    // ── Set up file-based logging ───────────────────────────────────────
    // Override env_logger's default target to write to daemon.log.
    let log_path = home.join("daemon.log");
    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .expect("failed to open daemon.log");

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_secs()
        .target(env_logger::Target::Pipe(Box::new(log_file)))
        .init();

    log::info!("omny-daemon daemonized (pid {})", std::process::id());

    // ── Run the daemon ──────────────────────────────────────────────────
    // run_foreground_inner() does everything except init logging (we already did).
    run_foreground_inner();
}

#[cfg(not(unix))]
fn cmd_daemonize() {
    eprintln!("Daemonization is only supported on Unix platforms.");
    eprintln!("Use 'omny-daemon' (foreground) or your platform's service manager.");
    std::process::exit(1);
}

/// Connect to the running daemon and print its status.
fn cmd_status() {
    match omny_client::DaemonClient::connect() {
        Ok(client) => match client.daemon_status() {
            Ok(status) => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&status)
                        .unwrap_or_else(|_| format!("{status:?}"))
                );
            }
            Err(e) => {
                eprintln!("Failed to get daemon status: {e}");
                std::process::exit(1);
            }
        },
        Err(e) => {
            eprintln!("Could not connect to daemon: {e}");
            eprintln!("Is omny-daemon running?");
            std::process::exit(1);
        }
    }
}

/// Connect to the running daemon and tell it to stop.
fn cmd_stop() {
    match omny_client::DaemonClient::connect() {
        Ok(client) => match client.call("daemon.stop", serde_json::json!({})) {
            Ok(_) => {
                println!("Daemon stopping.");
            }
            Err(e) => {
                eprintln!("Failed to stop daemon: {e}");
                std::process::exit(1);
            }
        },
        Err(e) => {
            eprintln!("Could not connect to daemon: {e}");
            eprintln!("Is omny-daemon running?");
            std::process::exit(1);
        }
    }
}

/// Run the daemon in the foreground (with stderr logging).
fn run_foreground() {
    // Initialize logging to stderr (foreground mode).
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_secs()
        .init();

    log::info!("omny-daemon starting (foreground)");
    run_foreground_inner();
}

/// Core daemon logic shared by foreground and daemonized modes.
///
/// Assumes logging is already initialized by the caller.
fn run_foreground_inner() {
    // Install panic hook — clean up auth token on crash.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log::error!("PANIC: {info}");
        auth::remove_token();
        default_hook(info);
    }));

    // Check for an existing daemon.
    let pidfile = pidfile_path();
    if pidfile.exists() {
        if let Ok(contents) = std::fs::read_to_string(&pidfile) {
            if let Ok(pid) = contents.trim().parse::<u32>() {
                if is_process_alive(pid) {
                    log::error!("Another daemon is already running (pid {pid})");
                    eprintln!("Another daemon is already running (pid {pid}).");
                    eprintln!("Run 'omny-daemon stop' first, or delete {}", pidfile.display());
                    std::process::exit(1);
                }
                log::info!("Stale pidfile found (pid {pid} is dead), cleaning up");
            }
        }
        std::fs::remove_file(&pidfile).ok();
    }

    // Load config.
    let daemon_config = load_or_create_default();
    log::info!(
        "Config: port={}, bind_all={}, device={}, tower.enabled={}",
        daemon_config.omnibus.port,
        daemon_config.omnibus.bind_all,
        daemon_config.omnibus.device_name,
        daemon_config.tower.enabled,
    );

    // Determine data directory.
    let data_dir = daemon_config
        .omnibus
        .data_dir
        .clone()
        .unwrap_or_else(|| omnidea_home().join("data"));

    // Ensure data directory exists.
    if let Err(e) = std::fs::create_dir_all(&data_dir) {
        log::error!("Failed to create data directory {}: {e}", data_dir.display());
        std::process::exit(1);
    }

    // Start either Tower (which owns Omnibus) or standalone Omnibus.
    let tower_arc: Option<Arc<tower::Tower>>;
    let standalone_omnibus: Option<Arc<Omnibus>>;

    if daemon_config.tower.enabled {
        // ── Tower mode ──────────────────────────────────────────────
        log::info!("Tower mode enabled — starting Tower (which owns Omnibus)");

        let tower_config = to_tower_config(&daemon_config, &data_dir);
        log::info!(
            "Tower config: mode={}, name={}, port={}, seeds={}",
            tower_config.mode,
            tower_config.name,
            tower_config.port,
            tower_config.seed_peers.len(),
        );

        // Ensure Tower data directory exists.
        if let Err(e) = std::fs::create_dir_all(&tower_config.data_dir) {
            log::error!(
                "Failed to create Tower data directory {}: {e}",
                tower_config.data_dir.display()
            );
            std::process::exit(1);
        }

        match tower::Tower::start(tower_config) {
            Ok(t) => {
                log::info!(
                    "Tower started on port {} as {}",
                    t.port(),
                    t.status().mode
                );

                // Initial announcement.
                if let Err(e) = t.announce() {
                    log::warn!("Initial Tower announcement failed: {e}");
                }

                let arc = Arc::new(t);
                tower_arc = Some(Arc::clone(&arc));
                standalone_omnibus = None;
            }
            Err(e) => {
                log::error!("Failed to start Tower: {e}");
                std::process::exit(1);
            }
        }
    } else {
        // ── Standalone Omnibus mode ────────────────────────────────
        let mut omnibus_config = daemon_config.to_omnibus_config();
        if omnibus_config.data_dir.is_none() {
            omnibus_config.data_dir = Some(data_dir.clone());
        }

        match Omnibus::start(omnibus_config) {
            Ok(o) => {
                log::info!("Omnibus started on port {}", o.port());

                // Try to load existing identity.
                if let Some(ref dd) = o.config().data_dir {
                    let data_dir_str = dd.to_string_lossy();
                    match o.load_identity(&data_dir_str) {
                        Ok(crown_id) => log::info!("Loaded identity: {crown_id}"),
                        Err(_) => {
                            log::info!("No existing identity found (create one via IPC)")
                        }
                    }
                }

                standalone_omnibus = Some(Arc::new(o));
                tower_arc = None;
            }
            Err(e) => {
                log::error!("Failed to start Omnibus: {e}");
                std::process::exit(1);
            }
        }
    }

    // Write pidfile.
    let pid = std::process::id();
    if let Err(e) = std::fs::write(&pidfile, pid.to_string()) {
        log::warn!("Could not write pidfile: {e}");
    }

    // Generate auth token for IPC authentication.
    let auth_token = auth::generate_token();
    match auth::write_token(&auth_token) {
        Ok(path) => log::info!("Auth token written to {}", path.display()),
        Err(e) => log::warn!("Could not write auth token (clients may fail to auth): {e}"),
    }

    // ── Equipment boot ──────────────────────────────────────────
    // Build the OmnibusRef (wraps Tower or standalone Omnibus).
    let omnibus_ref = if let Some(ref t) = tower_arc {
        state::OmnibusRef::Tower(Arc::clone(t))
    } else {
        state::OmnibusRef::Standalone(Arc::clone(standalone_omnibus.as_ref().unwrap()))
    };

    // Create DaemonState with full Equipment stack.
    let state = Arc::new(state::DaemonState::new(
        omnibus_ref,
        data_dir.clone(),
        daemon_config,
        auth_token,
    ));

    // Set up Ctrl-C handler using state.shutdown.
    {
        let shutdown_ref = Arc::clone(&state);
        if let Err(e) = ctrlc::set_handler(move || {
            log::info!("Received signal, shutting down...");
            shutdown_ref.shutdown.store(true, Ordering::SeqCst);
        }) {
            log::warn!("Could not set signal handler: {e}");
        }
    }

    // ── Start IPC server EARLY ──────────────────────────────────
    // Clients can connect and authenticate immediately. Their first
    // RPC call will block in dispatch() until Equipment is ready.
    let socket_path = omny_client::default_socket_path();
    let ipc = Arc::new(IpcServer::new(state.clone(), socket_path));

    let ipc_thread = {
        let ipc = Arc::clone(&ipc);
        std::thread::Builder::new()
            .name("ipc-server".into())
            .spawn(move || {
                if let Err(e) = ipc.run() {
                    log::error!("IPC server error: {e}");
                }
            })
            .expect("failed to spawn IPC server thread")
    };

    log::info!("IPC server listening (dispatch will wait for Equipment)");

    // Phase 1: Auto-register all FFI ops from C header (~484 simple ops).
    ffi_ops::register_all(&state.phone);

    // Phase 2: Hand-written modules override complex ops with Rust-native code.
    let all_modules = modules::all_modules();
    for module in &all_modules {
        module.register(&state);
        let info = equipment::ModuleInfo::new(module.id(), module.name(), equipment::ModuleType::Source)
            .with_dependencies(module.deps().iter().map(|s| s.to_string()).collect())
            .with_catalog(module.catalog());
        state.contacts.register(info).ok();
    }

    // Phase 3: Wire modifier observers (Yoke, Quest, Pager via Email).
    modifiers::wire_observers(&state);

    // Unlock vault if identity already exists.
    if state.omnibus.omnibus().pubkey().is_some() {
        state::ensure_vault_unlocked(&state);
    }

    // Signal that Equipment is ready — unblocks any waiting dispatch calls.
    state.mark_ready();

    log::info!(
        "Equipment ready: {} ops registered, {} modules",
        state.phone.registered_call_ids().len(),
        state.contacts.registered_module_ids().len(),
    );

    // Start watchdog heartbeat thread — writes a timestamp every 30 seconds.
    // The tray or monitoring tools can detect a hung daemon by checking staleness.
    let heartbeat_path = config::omnidea_home().join("daemon.heartbeat");
    {
        let state_ref = Arc::clone(&state);
        let heartbeat_path = heartbeat_path.clone();
        std::thread::Builder::new()
            .name("watchdog".into())
            .spawn(move || {
                while !state_ref.shutdown.load(Ordering::Relaxed) {
                    let ts = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    let _ = std::fs::write(&heartbeat_path, ts.to_string());
                    std::thread::sleep(Duration::from_secs(30));
                }
            })
            .ok();
    }

    // Start Tower main loop thread if Tower mode is active.
    let tower_thread = if let Some(ref tower) = tower_arc {
        let tower_ref = Arc::clone(tower);
        let state_for_tower = Arc::clone(&state);
        let announce_secs = {
            let cfg = state.config.lock().unwrap();
            cfg.tower.announce_interval_secs.unwrap_or(300)
        };
        let gospel_secs = {
            let cfg = state.config.lock().unwrap();
            cfg.tower.gospel_interval_secs.unwrap_or(60)
        };
        let live_secs = {
            let cfg = state.config.lock().unwrap();
            cfg.tower.gospel_live_interval_secs.unwrap_or(2)
        };

        Some(
            std::thread::Builder::new()
                .name("tower-loop".into())
                .spawn(move || {
                    tower_ref.omnibus().runtime().block_on(async {
                        let mut announce_tick =
                            tokio::time::interval(Duration::from_secs(announce_secs));
                        announce_tick.tick().await; // consume immediate tick

                        let mut gospel_tick =
                            tokio::time::interval(Duration::from_secs(gospel_secs));
                        gospel_tick.tick().await;

                        let mut live_tick =
                            tokio::time::interval(Duration::from_secs(live_secs));
                        live_tick.tick().await;

                        loop {
                            if state_for_tower.shutdown.load(Ordering::Relaxed) {
                                break;
                            }
                            tokio::select! {
                                _ = announce_tick.tick() => {
                                    // announce() → omnibus.publish() → runtime.block_on()
                                    // We're already inside block_on(), so offload to a
                                    // blocking thread to avoid nested-runtime panic.
                                    let t = tower_ref.clone();
                                    match tokio::task::spawn_blocking(move || t.announce()).await {
                                        Ok(Err(e)) => log::warn!("Tower announcement failed: {e}"),
                                        Err(e) => log::warn!("Tower announce task panicked: {e}"),
                                        _ => {}
                                    }
                                }
                                _ = gospel_tick.tick() => {
                                    tower_ref.run_gospel_cycle().await;
                                }
                                _ = live_tick.tick() => {
                                    // process_live_events() may call block_on() internally
                                    let t = tower_ref.clone();
                                    let _ = tokio::task::spawn_blocking(move || {
                                        t.process_live_events();
                                    }).await;
                                }
                            }
                        }
                    });
                    log::info!("Tower loop stopped");
                })
                .expect("failed to spawn tower-loop thread"),
        )
    } else {
        None
    };

    // Block until shutdown.
    while !state.shutdown.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(200));
    }

    log::info!("Shutting down...");

    // Shut down registered modules (Contacts tracks order).
    state.contacts.shutdown_all();

    // Stop the node runtime.
    if let Some(ref omnibus) = standalone_omnibus {
        omnibus.stop();
        log::info!("Omnibus stopped");
    }
    if let Some(ref tower) = tower_arc {
        tower.omnibus().stop();
        log::info!("Tower stopped");
    }

    // Clean up IPC socket.
    ipc.cleanup();

    // Remove auth token.
    auth::remove_token();

    // Remove heartbeat file.
    std::fs::remove_file(&heartbeat_path).ok();

    // Remove pidfile.
    std::fs::remove_file(&pidfile).ok();

    // Wait for threads to finish.
    if let Some(t) = tower_thread {
        t.join().ok();
    }
    ipc_thread.join().ok();

    log::info!("omny-daemon stopped");
}

/// Check if a process with the given PID is alive.
///
/// Uses `kill -0` which sends no signal but checks process existence.
fn is_process_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
