//! TCP transport — listens on a local TCP socket, one `FrameWriter` per client.

use std::io;
use std::net::{TcpListener, TcpStream};
use std::sync::{
    atomic::{AtomicU32, Ordering},
    Arc,
};

use crate::error::StreamError;

use super::{Transport, TransportWriter};

/// A transport that accepts TCP connections on a local socket.
///
/// The listener is set to non-blocking mode so `next_client()` returns
/// immediately when no client is waiting.
pub struct TcpTransport {
    listener: TcpListener,
    client_count: Arc<AtomicU32>,
}

impl TcpTransport {
    /// Bind the TCP listener to `addr` (e.g. `"127.0.0.1:9123"`).
    ///
    /// Use `"127.0.0.1:0"` for an OS-assigned ephemeral port (useful in tests).
    pub fn bind(addr: &str) -> Result<Self, StreamError> {
        let listener = TcpListener::bind(addr)?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener,
            client_count: Arc::new(AtomicU32::new(0)),
        })
    }

    /// Return the local address the listener is bound to.
    pub fn local_addr(&self) -> io::Result<std::net::SocketAddr> {
        self.listener.local_addr()
    }
}

impl Transport for TcpTransport {
    /// Accept the next waiting TCP client (non-blocking).
    ///
    /// Returns `None` when no client is queued (`WouldBlock`).  On success,
    /// increments the client counter and returns the `TcpStream`.
    fn next_client(&mut self) -> Option<Box<dyn TransportWriter>> {
        match self.listener.accept() {
            Ok((stream, _addr)) => {
                self.client_count.fetch_add(1, Ordering::Relaxed);
                Some(Box::new(ClientStream {
                    stream,
                    count: Arc::clone(&self.client_count),
                }))
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => None,
            Err(_) => None,
        }
    }

    fn connected_clients(&self) -> u32 {
        self.client_count.load(Ordering::Relaxed)
    }
}

// ---------------------------------------------------------------------------
// Wrapper that decrements client_count on drop
// ---------------------------------------------------------------------------

struct ClientStream {
    stream: TcpStream,
    count: Arc<AtomicU32>,
}

impl io::Write for ClientStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.stream.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

impl Drop for ClientStream {
    fn drop(&mut self) {
        self.count.fetch_sub(1, Ordering::Relaxed);
    }
}
