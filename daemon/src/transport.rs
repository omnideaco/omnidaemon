//! Platform-abstracted IPC transport for the Omnidea daemon.
//!
//! On Unix (macOS/Linux): Unix domain socket (`~/.omnidea/daemon.sock`).
//! On Windows: Named Pipe (`\\.\pipe\omnidea-daemon`).
//!
//! Both provide the same security properties: local-only, filesystem-protected,
//! no network exposure.

use std::io::{self, Read, Write};
use std::path::Path;
use std::time::Duration;

// ─── Platform stream wrapper ───────────────────────────────────────────────

/// A platform-abstracted bidirectional stream (one client connection).
///
/// On Unix this wraps `UnixStream`. On Windows this wraps a Named Pipe handle.
pub enum PlatformStream {
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixStream),
    #[cfg(windows)]
    Pipe(PipeStream),
}

impl PlatformStream {
    /// Clone the stream for separate reader/writer threads.
    pub fn try_clone(&self) -> io::Result<Self> {
        match self {
            #[cfg(unix)]
            PlatformStream::Unix(s) => Ok(PlatformStream::Unix(s.try_clone()?)),
            #[cfg(windows)]
            PlatformStream::Pipe(p) => p.try_clone().map(PlatformStream::Pipe),
        }
    }

    /// Set the read timeout on this stream.
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        match self {
            #[cfg(unix)]
            PlatformStream::Unix(s) => s.set_read_timeout(timeout),
            #[cfg(windows)]
            PlatformStream::Pipe(p) => p.set_read_timeout(timeout),
        }
    }

    /// Set non-blocking mode.
    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        match self {
            #[cfg(unix)]
            PlatformStream::Unix(s) => s.set_nonblocking(nonblocking),
            #[cfg(windows)]
            PlatformStream::Pipe(p) => p.set_nonblocking(nonblocking),
        }
    }

    /// Get the peer's process ID (if supported by the platform).
    ///
    /// Returns `Some(pid)` on platforms that support peer credential lookup.
    pub fn peer_pid(&self) -> Option<u32> {
        match self {
            #[cfg(target_os = "macos")]
            PlatformStream::Unix(s) => get_peer_pid_macos(s),
            #[cfg(target_os = "linux")]
            PlatformStream::Unix(s) => get_peer_pid_linux(s),
            #[cfg(all(unix, not(target_os = "macos"), not(target_os = "linux")))]
            PlatformStream::Unix(_) => None,
            #[cfg(windows)]
            PlatformStream::Pipe(p) => p.peer_pid(),
        }
    }
}

impl Read for PlatformStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            #[cfg(unix)]
            PlatformStream::Unix(s) => s.read(buf),
            #[cfg(windows)]
            PlatformStream::Pipe(p) => p.read(buf),
        }
    }
}

impl Write for PlatformStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            #[cfg(unix)]
            PlatformStream::Unix(s) => s.write(buf),
            #[cfg(windows)]
            PlatformStream::Pipe(p) => p.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            #[cfg(unix)]
            PlatformStream::Unix(s) => s.flush(),
            #[cfg(windows)]
            PlatformStream::Pipe(p) => p.flush(),
        }
    }
}

// ─── Platform listener wrapper ─────────────────────────────────────────────

/// A platform-abstracted connection listener.
///
/// On Unix this wraps `UnixListener`. On Windows this creates Named Pipes.
pub enum PlatformListener {
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixListener),
    #[cfg(windows)]
    Pipe(PipeListener),
}

impl PlatformListener {
    /// Bind to the given path, removing any stale socket/pipe.
    ///
    /// On Unix: creates a Unix domain socket at `path`.
    /// On Windows: `path` is ignored — creates `\\.\pipe\omnidea-daemon`.
    pub fn bind(path: &Path) -> io::Result<Self> {
        #[cfg(unix)]
        {
            // Remove stale socket from a crashed daemon.
            if path.exists() {
                log::info!("Removing stale socket at {}", path.display());
                std::fs::remove_file(path)?;
            }

            let listener = std::os::unix::net::UnixListener::bind(path)?;

            // Set socket file permissions to owner-only (0600).
            set_socket_permissions(path)?;

            listener.set_nonblocking(true)?;
            log::info!("IPC server listening on {}", path.display());
            Ok(PlatformListener::Unix(listener))
        }

        #[cfg(windows)]
        {
            let _ = path; // Windows uses a fixed pipe name.
            PipeListener::bind().map(PlatformListener::Pipe)
        }
    }

    /// Accept a new client connection.
    ///
    /// Returns `Ok(Some(stream))` on success, `Ok(None)` if no connection is
    /// pending (non-blocking), or `Err` on failure.
    pub fn accept(&self) -> io::Result<Option<PlatformStream>> {
        match self {
            #[cfg(unix)]
            PlatformListener::Unix(l) => match l.accept() {
                Ok((stream, _addr)) => Ok(Some(PlatformStream::Unix(stream))),
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => Ok(None),
                Err(e) => Err(e),
            },
            #[cfg(windows)]
            PlatformListener::Pipe(p) => p.accept(),
        }
    }

    /// Clean up the socket/pipe on shutdown.
    pub fn cleanup(path: &Path) {
        #[cfg(unix)]
        {
            if path.exists() {
                if let Err(e) = std::fs::remove_file(path) {
                    log::warn!("Failed to remove socket file: {e}");
                }
            }
        }
        #[cfg(windows)]
        {
            let _ = path; // Named pipes are cleaned up by the OS when the last handle closes.
        }
    }
}

// ─── Client-side connect ───────────────────────────────────────────────────

/// Connect to the daemon as a client.
///
/// On Unix: connects to the Unix domain socket at `path`.
/// On Windows: connects to the `\\.\pipe\omnidea-daemon` Named Pipe.
pub fn connect(path: &Path) -> io::Result<PlatformStream> {
    #[cfg(unix)]
    {
        let stream = std::os::unix::net::UnixStream::connect(path)?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        Ok(PlatformStream::Unix(stream))
    }

    #[cfg(windows)]
    {
        let _ = path;
        PipeStream::connect().map(PlatformStream::Pipe)
    }
}

// ─── Unix peer credentials ────────────────────────────────────────────────

#[cfg(target_os = "macos")]
fn get_peer_pid_macos(stream: &std::os::unix::net::UnixStream) -> Option<u32> {
    use std::os::unix::io::AsRawFd;
    let fd = stream.as_raw_fd();
    let mut pid: libc::pid_t = 0;
    let mut pid_size = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
    // LOCAL_PEERPID = 0x002 on macOS
    let ret = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_LOCAL,
            0x002, // LOCAL_PEERPID
            &mut pid as *mut _ as *mut libc::c_void,
            &mut pid_size,
        )
    };
    if ret == 0 && pid > 0 {
        Some(pid as u32)
    } else {
        None
    }
}

#[cfg(target_os = "linux")]
fn get_peer_pid_linux(stream: &std::os::unix::net::UnixStream) -> Option<u32> {
    use std::os::unix::io::AsRawFd;
    let fd = stream.as_raw_fd();

    #[repr(C)]
    struct UcredLinux {
        pid: libc::pid_t,
        uid: libc::uid_t,
        gid: libc::gid_t,
    }

    let mut cred = UcredLinux {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut cred_size = std::mem::size_of::<UcredLinux>() as libc::socklen_t;

    let ret = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut _ as *mut libc::c_void,
            &mut cred_size,
        )
    };
    if ret == 0 && cred.pid > 0 {
        Some(cred.pid as u32)
    } else {
        None
    }
}

// ─── Unix socket permissions ──────────────────────────────────────────────

#[cfg(unix)]
fn set_socket_permissions(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::Permissions::from_mode(0o600);
    std::fs::set_permissions(path, perms)
}

// ─── Windows Named Pipe implementation ────────────────────────────────────

#[cfg(windows)]
mod windows_pipe {
    use std::io::{self, Read, Write};
    use std::time::Duration;

    const PIPE_NAME: &str = r"\\.\pipe\omnidea-daemon";

    /// Server-side Named Pipe listener.
    pub struct PipeListener {
        // Windows Named Pipes create a new pipe instance per connection.
        // The listener just holds the pipe name.
        _private: (),
    }

    impl PipeListener {
        pub fn bind() -> io::Result<Self> {
            log::info!("IPC server listening on {PIPE_NAME}");
            Ok(Self { _private: () })
        }

        pub fn accept(&self) -> io::Result<Option<super::PlatformStream>> {
            use std::os::windows::io::FromRawHandle;
            use windows_sys::Win32::Foundation::*;
            use windows_sys::Win32::Storage::FileSystem::*;
            use windows_sys::Win32::System::Pipes::*;

            // Create a new pipe instance for this connection.
            let pipe_name_wide: Vec<u16> = PIPE_NAME
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();

            let handle = unsafe {
                CreateNamedPipeW(
                    pipe_name_wide.as_ptr(),
                    PIPE_ACCESS_DUPLEX,
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_NOWAIT,
                    PIPE_UNLIMITED_INSTANCES,
                    65536,
                    65536,
                    0,
                    std::ptr::null_mut(),
                )
            };

            if handle == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }

            // Try to connect a client (non-blocking).
            let connected = unsafe { ConnectNamedPipe(handle, std::ptr::null_mut()) };
            if connected == 0 {
                let err = io::Error::last_os_error();
                match err.raw_os_error() {
                    Some(ERROR_PIPE_CONNECTED) => {
                        // Client already connected — proceed.
                    }
                    Some(ERROR_PIPE_LISTENING) => {
                        // No client yet — clean up and return None.
                        unsafe { CloseHandle(handle) };
                        return Ok(None);
                    }
                    _ => {
                        unsafe { CloseHandle(handle) };
                        return Err(err);
                    }
                }
            }

            Ok(Some(super::PlatformStream::Pipe(PipeStream { handle })))
        }
    }

    /// A connected Named Pipe stream.
    pub struct PipeStream {
        handle: windows_sys::Win32::Foundation::HANDLE,
    }

    // SAFETY: Named Pipe handles can be sent between threads.
    unsafe impl Send for PipeStream {}

    impl PipeStream {
        pub fn connect() -> io::Result<Self> {
            use windows_sys::Win32::Foundation::*;
            use windows_sys::Win32::Storage::FileSystem::*;

            let pipe_name_wide: Vec<u16> = PIPE_NAME
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();

            let handle = unsafe {
                CreateFileW(
                    pipe_name_wide.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    0,
                    std::ptr::null_mut(),
                    OPEN_EXISTING,
                    0,
                    0,
                )
            };

            if handle == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }

            Ok(Self { handle })
        }

        pub fn try_clone(&self) -> io::Result<Self> {
            use windows_sys::Win32::Foundation::*;

            let process = unsafe { GetCurrentProcess() };
            let mut new_handle: HANDLE = 0;
            let ok = unsafe {
                DuplicateHandle(
                    process,
                    self.handle,
                    process,
                    &mut new_handle,
                    0,
                    0, // not inheritable
                    DUPLICATE_SAME_ACCESS,
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self {
                handle: new_handle,
            })
        }

        pub fn set_read_timeout(&self, _timeout: Option<Duration>) -> io::Result<()> {
            // Named Pipes use PIPE_NOWAIT for non-blocking.
            // Timeouts can be emulated via WaitForSingleObject before reads.
            // For now, the blocking/timeout behavior is handled at a higher level.
            Ok(())
        }

        pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
            use windows_sys::Win32::System::Pipes::*;
            let mode = if nonblocking {
                PIPE_READMODE_BYTE | PIPE_NOWAIT
            } else {
                PIPE_READMODE_BYTE | PIPE_WAIT
            };
            let ok = unsafe { SetNamedPipeHandleState(self.handle, &mode, std::ptr::null(), std::ptr::null()) };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }

        pub fn peer_pid(&self) -> Option<u32> {
            use windows_sys::Win32::System::Pipes::*;
            let mut pid: u32 = 0;
            let ok = unsafe { GetNamedPipeClientProcessId(self.handle, &mut pid) };
            if ok != 0 && pid > 0 {
                Some(pid)
            } else {
                None
            }
        }
    }

    impl Read for PipeStream {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            use windows_sys::Win32::Foundation::*;
            use windows_sys::Win32::Storage::FileSystem::*;
            let mut bytes_read: u32 = 0;
            let ok = unsafe {
                ReadFile(
                    self.handle,
                    buf.as_mut_ptr() as *mut _,
                    buf.len() as u32,
                    &mut bytes_read,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                let err = io::Error::last_os_error();
                if err.raw_os_error() == Some(ERROR_NO_DATA as i32) {
                    return Err(io::Error::from(io::ErrorKind::WouldBlock));
                }
                return Err(err);
            }
            Ok(bytes_read as usize)
        }
    }

    impl Write for PipeStream {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            use windows_sys::Win32::Foundation::*;
            use windows_sys::Win32::Storage::FileSystem::*;
            let mut bytes_written: u32 = 0;
            let ok = unsafe {
                WriteFile(
                    self.handle,
                    buf.as_ptr() as *const _,
                    buf.len() as u32,
                    &mut bytes_written,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(bytes_written as usize)
        }

        fn flush(&mut self) -> io::Result<()> {
            use windows_sys::Win32::Storage::FileSystem::*;
            let ok = unsafe { FlushFileBuffers(self.handle) };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
    }

    impl Drop for PipeStream {
        fn drop(&mut self) {
            unsafe { windows_sys::Win32::Foundation::CloseHandle(self.handle) };
        }
    }
}

#[cfg(windows)]
pub use windows_pipe::{PipeListener, PipeStream};
