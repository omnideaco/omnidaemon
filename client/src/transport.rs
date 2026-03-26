//! Platform-abstracted IPC transport for the Omnidea client.
//!
//! Provides a unified `ClientStream` that works on all platforms:
//! - Unix (macOS/Linux): Unix domain socket
//! - Windows: Named Pipe

use std::io::{self, Read, Write};
use std::path::Path;
use std::time::Duration;

// ─── Client stream ─────────────────────────────────────────────────────────

/// A platform-abstracted bidirectional stream for client connections.
pub enum ClientStream {
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixStream),
    #[cfg(windows)]
    Pipe(WindowsPipeStream),
}

impl ClientStream {
    /// Connect to the daemon at the given path.
    ///
    /// On Unix: connects to the Unix domain socket at `path`.
    /// On Windows: `path` is ignored — connects to `\\.\pipe\omnidea-daemon`.
    pub fn connect(path: &Path) -> io::Result<Self> {
        #[cfg(unix)]
        {
            let stream = std::os::unix::net::UnixStream::connect(path)?;
            stream.set_read_timeout(Some(Duration::from_secs(10)))?;
            Ok(ClientStream::Unix(stream))
        }

        #[cfg(windows)]
        {
            let _ = path;
            WindowsPipeStream::connect().map(ClientStream::Pipe)
        }
    }

    /// Clone the stream for separate reader/writer threads.
    pub fn try_clone(&self) -> io::Result<Self> {
        match self {
            #[cfg(unix)]
            ClientStream::Unix(s) => Ok(ClientStream::Unix(s.try_clone()?)),
            #[cfg(windows)]
            ClientStream::Pipe(p) => p.try_clone().map(ClientStream::Pipe),
        }
    }

    /// Set the read timeout.
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        match self {
            #[cfg(unix)]
            ClientStream::Unix(s) => s.set_read_timeout(timeout),
            #[cfg(windows)]
            ClientStream::Pipe(p) => p.set_read_timeout(timeout),
        }
    }
}

impl Read for ClientStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            #[cfg(unix)]
            ClientStream::Unix(s) => s.read(buf),
            #[cfg(windows)]
            ClientStream::Pipe(p) => p.read(buf),
        }
    }
}

impl Write for ClientStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            #[cfg(unix)]
            ClientStream::Unix(s) => s.write(buf),
            #[cfg(windows)]
            ClientStream::Pipe(p) => p.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            #[cfg(unix)]
            ClientStream::Unix(s) => s.flush(),
            #[cfg(windows)]
            ClientStream::Pipe(p) => p.flush(),
        }
    }
}

// ─── Windows Named Pipe client ────────────────────────────────────────────

#[cfg(windows)]
pub struct WindowsPipeStream {
    handle: windows_sys::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
unsafe impl Send for WindowsPipeStream {}

#[cfg(windows)]
impl WindowsPipeStream {
    const PIPE_NAME: &str = r"\\.\pipe\omnidea-daemon";

    pub fn connect() -> io::Result<Self> {
        use windows_sys::Win32::Foundation::*;
        use windows_sys::Win32::Storage::FileSystem::*;

        let pipe_name_wide: Vec<u16> = Self::PIPE_NAME
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

        // Switch to blocking byte-mode reads.
        use windows_sys::Win32::System::Pipes::*;
        let mode = PIPE_READMODE_BYTE | PIPE_WAIT;
        unsafe { SetNamedPipeHandleState(handle, &mode, std::ptr::null(), std::ptr::null()) };

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
                0,
                DUPLICATE_SAME_ACCESS,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { handle: new_handle })
    }

    pub fn set_read_timeout(&self, _timeout: Option<Duration>) -> io::Result<()> {
        Ok(()) // Timeouts handled at higher level for Named Pipes.
    }
}

#[cfg(windows)]
impl Read for WindowsPipeStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
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
            return Err(io::Error::last_os_error());
        }
        Ok(bytes_read as usize)
    }
}

#[cfg(windows)]
impl Write for WindowsPipeStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
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

#[cfg(windows)]
impl Drop for WindowsPipeStream {
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.handle) };
    }
}
