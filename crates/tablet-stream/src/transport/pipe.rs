//! Windows named pipe transport.
//!
//! All `unsafe` code in this file is isolated to the FFI calls to `CreateNamedPipeA`,
//! `ConnectNamedPipe`, and `WriteFile`.  Each unsafe block is documented with
//! the invariants that make it sound:
//!
//! - Handles are validated against `INVALID_HANDLE_VALUE` before use.
//! - `WriteFile` is called only with a valid, open handle obtained from
//!   `ConnectNamedPipe`.
//! - The `PipeHandle` wrapper owns the handle and closes it in `Drop` via
//!   `CloseHandle`.

#![cfg(windows)]

use std::ffi::CString;
use std::io::{self, Write};
use std::sync::{
    atomic::{AtomicU32, Ordering},
    Arc,
};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{WriteFile, PIPE_ACCESS_DUPLEX};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeA, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};

use super::{Transport, TransportWriter};

// ---------------------------------------------------------------------------
// PipeHandle — RAII wrapper around a Win32 HANDLE
// ---------------------------------------------------------------------------

/// Owns a Win32 named pipe handle; closes it automatically on drop.
///
/// # Safety invariant
/// `handle` is always either `INVALID_HANDLE_VALUE` (sentinel for "closed")
/// or a valid handle obtained from `CreateNamedPipeA` / `ConnectNamedPipe`.
pub struct PipeHandle {
    handle: HANDLE,
    count: Arc<AtomicU32>,
}

impl Write for PipeHandle {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "pipe handle is invalid",
            ));
        }
        let mut written: u32 = 0;
        // SAFETY: `self.handle` is a valid, connected pipe handle (invariant
        // maintained by `NamedPipeTransport::next_client`).  `buf.as_ptr()` is
        // valid for `buf.len()` bytes.  `written` is a valid output pointer.
        let ok = unsafe {
            WriteFile(
                self.handle,
                buf.as_ptr(),
                buf.len() as u32,
                &mut written,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(written as usize)
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for PipeHandle {
    fn drop(&mut self) {
        if self.handle != INVALID_HANDLE_VALUE {
            // SAFETY: handle is valid and owned exclusively by this struct.
            unsafe { CloseHandle(self.handle) };
            self.handle = INVALID_HANDLE_VALUE;
            self.count.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

// SAFETY: HANDLE is a raw pointer-sized integer, but we use it as an opaque
// OS resource identifier; the OS serializes concurrent access to the pipe.
unsafe impl Send for PipeHandle {}

// ---------------------------------------------------------------------------
// NamedPipeTransport
// ---------------------------------------------------------------------------

/// A transport that accepts connections on a Windows named pipe.
///
/// The pipe is created on-demand in `next_client()`, which blocks until a
/// client connects.  Callers that want non-blocking behavior should call
/// `next_client()` from a dedicated thread.
#[cfg(windows)]
pub struct NamedPipeTransport {
    /// The pipe name passed to the constructor (without the `\\.\pipe\` prefix).
    name: String,
    client_count: Arc<AtomicU32>,
}

#[cfg(windows)]
impl NamedPipeTransport {
    /// Create a new transport for `\\.\pipe\{name}`.
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            client_count: Arc::new(AtomicU32::new(0)),
        }
    }

    /// Full Win32 pipe path, e.g. `\\.\pipe\wacom-capture\0`.
    fn pipe_path(&self) -> CString {
        let full = format!(r"\\.\pipe\{}", self.name);
        // CString::new panics only on interior NUL bytes, which we don't have.
        CString::new(full).expect("pipe name must not contain NUL bytes")
    }
}

#[cfg(windows)]
impl Transport for NamedPipeTransport {
    /// Create a pipe instance and block until one client connects.
    ///
    /// This is blocking by design; callers should run it in a dedicated thread
    /// if they need concurrency.  Returns `None` if the pipe could not be
    /// created.
    fn next_client(&mut self) -> Option<Box<dyn TransportWriter>> {
        let path = self.pipe_path();

        // SAFETY: `path.as_ptr()` is a valid NUL-terminated C string for the
        // lifetime of the call.  All other arguments are plain integer constants
        // or 0/NULL.  The returned handle is checked against INVALID_HANDLE_VALUE.
        let handle = unsafe {
            CreateNamedPipeA(
                path.as_ptr() as *const u8,
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                PIPE_UNLIMITED_INSTANCES,
                65536, // out-buffer size
                65536, // in-buffer size
                0,     // default timeout
                std::ptr::null(),
            )
        };

        if handle == INVALID_HANDLE_VALUE {
            return None;
        }

        // Block until a client connects.
        // SAFETY: `handle` is a valid pipe handle from `CreateNamedPipeA`.
        let connected = unsafe { ConnectNamedPipe(handle, std::ptr::null_mut()) };

        // `ConnectNamedPipe` returns 0 on failure, but also returns 0 (with
        // `ERROR_PIPE_CONNECTED`) when the client connected between `Create...`
        // and `Connect...`.  We treat both as success.
        if connected == 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() != Some(535 /* ERROR_PIPE_CONNECTED */) {
                // SAFETY: handle is valid.
                unsafe { CloseHandle(handle) };
                return None;
            }
        }

        self.client_count.fetch_add(1, Ordering::Relaxed);
        Some(Box::new(PipeHandle {
            handle,
            count: Arc::clone(&self.client_count),
        }))
    }

    fn connected_clients(&self) -> u32 {
        self.client_count.load(Ordering::Relaxed)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[cfg(windows)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn named_pipe_write_read() {
        use std::thread;

        let pipe_name = "tablet-stream-test-pipe-12345";
        let mut transport = NamedPipeTransport::new(pipe_name);

        // Server: accept a client in a background thread.
        let server_handle = thread::spawn(move || {
            let mut client = transport.next_client().expect("failed to get client");
            client.write_all(b"hello").expect("write failed");
        });

        // Client: open the pipe and read back.
        let path = format!(r"\\.\pipe\{pipe_name}");
        // Give the server thread a moment to call CreateNamedPipe.
        std::thread::sleep(std::time::Duration::from_millis(50));

        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .expect("failed to open named pipe as client");

        let mut buf = [0u8; 5];
        file.read_exact(&mut buf).expect("read failed");

        server_handle.join().expect("server thread panicked");
        assert_eq!(&buf, b"hello");
    }
}
