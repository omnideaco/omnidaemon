> **[Omnidea](https://github.com/omnideaco/omnidea)** / **[Omny](https://github.com/omnideaco/omny)** / **Omnidaemon** · For AI-assisted development, see [Omny CLAUDE.md](https://github.com/omnideaco/omny/blob/main/CLAUDE.md).

# omnidaemon

[![License: AGPL-3.0](https://img.shields.io/badge/License-AGPL--3.0-blue.svg)](LICENSE.md) [![GitHub Stars](https://img.shields.io/github/stars/omnideaco/omnidaemon)](https://github.com/omnideaco/omnidaemon/stargazers) [![Governed by the Covenant](https://img.shields.io/badge/Governed_by-The_Covenant-gold.svg)](https://github.com/omnideaco/covenant) ![Rust](https://img.shields.io/badge/Rust-000000?logo=rust&logoColor=white)

Part of [Omnidea](https://github.com/omnideaco/omnidea). For the full stack: `git clone --recursive https://github.com/omnideaco/omnidea.git`

The single source of truth for all Omninet operations. A headless Rust service that owns identity (Crown), storage (Vault), networking (Omnibus/Tower), and the operation pipeline (Equipment). Every client -- the browser, the tray, CLI tools, and TypeScript programs -- connects to this daemon over IPC rather than calling protocol crates directly.

## Three Binaries

| Binary | Crate | Purpose |
|--------|-------|---------|
| `omny-daemon` | `daemon/` | The headless service. Owns all state, listens for IPC connections, dispatches operations through Equipment. |
| `omny-tray` | `tray/` | System tray / menu bar app. Shows Omnibus status with an animated pinwheel icon. Connects to the daemon via IPC to display status and issue start/stop/restart commands. |
| `omny-client` | `client/` | Shared IPC client library (not a standalone binary). Used by omnishell, omny-tray, and any Rust code that needs to talk to the daemon. Fully synchronous -- no async runtime required. |

## How It Works

```
omnishell (Beryllium)       omny-tray         CLI tools         Programs (JS)
        |                      |                  |                   |
        └──────────────────────┼──────────────────┼───────────────────┘
                               |
                        omny-client (IPC)
                               |
                 Unix Socket / Named Pipe  (authenticated)
                               |
                        omny-daemon
                  ┌────────────┼────────────────┐
               Crown        Omnibus          Equipment
             (identity)    (network)     (Phone + Email + Contacts)
                  └────────────┼────────────────┘
                       29 Rust protocol crates
```

### IPC Protocol

Line-delimited JSON over a platform-abstracted transport:
- **Unix (macOS/Linux):** Unix domain socket at `~/.omnidea/daemon.sock`
- **Windows:** Named Pipe at `\\.\pipe\omnidea-daemon`

Both are local-only with filesystem-protected permissions (socket is `0600`).

### Authentication

On startup, the daemon generates a 32-byte random token and writes it to `~/.omnidea/auth.token` with owner-only permissions. Clients read this token and present it as the first message on any new connection (the handshake). Client types (`beryllium`, `tray`, `cli`, `program`) determine permission levels -- programs are sandboxed and cannot call restricted methods like `daemon.stop` or `crown.delete`.

### Operation Dispatch

All operations are dispatched through Equipment's Phone (an in-process RPC backbone):

1. **Auto-registered FFI ops** -- `build.rs` parses the Omninet C header (`divinity_ffi.h`) and auto-registers ~484 simple pass-through operations at boot.
2. **Hand-written modules** -- 14 Rust-native modules override complex operations with richer logic (Crown lifecycle, Vault encryption, Ideas CRUD, Tower management, editor sessions, etc.).
3. **Modifier observers** -- Yoke (history tracking) and other cross-cutting concerns wire into Email pub/sub to observe operations without coupling to them.

The daemon starts accepting IPC connections immediately on boot. If a client's first RPC arrives before Equipment registration completes, the call blocks briefly until the daemon is ready -- no errors, no retries needed.

## Daemon Modules

Each module implements `DaemonModule` and self-registers its Phone handlers at boot:

| Module | What It Handles |
|--------|----------------|
| `crown_mod` | Identity creation, loading, profile updates, recovery phrase |
| `vault_mod` | Encrypted storage unlock, lock, status |
| `ideas_mod` | Content CRUD (create, read, update, delete Ideas) |
| `editor_mod` | CRDT editor sessions with auto-save |
| `omnibus_mod` | Network runtime status, start, stop |
| `tower_mod` | Always-on relay node management |
| `gospel_mod` | Peer content synchronization |
| `network_mod` | Event publishing to the Omnibus network |
| `discovery_mod` | Peer and content discovery |
| `health_mod` | Network health monitoring |
| `events_mod` | Push event subscriptions |
| `daemon_mod` | Daemon lifecycle (ping, status, stop, shutdown) |
| `config_mod` | Runtime configuration get/set/reload |
| `op_mod` | Operation registry queries (list ops, capabilities) |

## Configuration

Config file at `~/.omnidea/config.toml` (created with defaults on first run):

```toml
[omnibus]
port = 4040                    # Local relay port (0 = OS-assigned)
bind_all = false               # true = LAN-reachable, false = localhost only
device_name = "Omnidea Device" # mDNS discovery name

[tower]
enabled = false                # Enable always-on relay node mode
```

## Usage

```bash
omny-daemon              # Run in foreground
omny-daemon --daemon     # Daemonize (double-fork, log to ~/.omnidea/daemon.log)
omny-daemon status       # Query running daemon status
omny-daemon stop         # Stop running daemon
omny-daemon install      # Install platform autostart (launch agent / systemd)
omny-daemon uninstall    # Remove platform autostart
```

## Building

Requires [Omninet](https://github.com/omnideaco/omninet) as a sibling directory -- the daemon depends on Omninet crates via path deps in `Cargo.toml`.

```bash
# From the omnidaemon directory:
cargo build              # Build all three crates (daemon, tray, client)
cargo run -p omny-daemon # Run the daemon
cargo run -p omny-tray   # Run the tray app
cargo test               # Run tests
```

### Path Dependencies

The daemon depends on these Omninet crates (resolved via relative paths):

- `omnibus` -- Network runtime (Crown, Globe)
- `tower` -- Always-on relay node
- `equipment` -- Phone, Email, Contacts, Pager, Communicator
- `ideas`, `vault`, `hall`, `sentinal` -- Content pipeline
- `globe` -- Event deserialization
- `polity`, `yoke`, `x` -- Cross-cutting concerns

## API Reference

See [API.md](API.md) for the full IPC protocol specification including authentication, request/response format, all methods, error codes, and push events.

## License

Licensed under the [Omninet Covenant License](LICENSE.md) (AGPL-3.0 foundation + Covenant alignment).
