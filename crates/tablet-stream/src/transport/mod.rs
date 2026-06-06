//! Transport abstractions: stdout, TCP, named pipe (Windows).
//!
//! Every transport yields a stream of `Write + Send` client handles; the
//! streaming layer creates a `FrameWriter` per handle, writes the handshake
//! header, and then pumps messages into it.

pub mod stdout;
pub mod tcp;

#[cfg(windows)]
pub mod pipe;

// ---------------------------------------------------------------------------
// Traits
// ---------------------------------------------------------------------------

/// Any `Write + Send` type automatically implements this marker trait.
///
/// Used as the trait object type for transport clients so the streaming layer
/// can treat stdout, TCP streams, and named pipe handles uniformly.
pub trait TransportWriter: std::io::Write + Send {}
impl<T: std::io::Write + Send> TransportWriter for T {}

/// A source of new transport clients.
///
/// The streaming layer calls `next_client()` in a polling loop; when it
/// returns `Some`, the caller is responsible for performing the handshake and
/// then writing frames.
pub trait Transport: Send {
    /// Accept the next waiting client, if any.
    ///
    /// Returns `None` when no client is immediately available (non-blocking
    /// semantics).  The caller should yield and retry.
    fn next_client(&mut self) -> Option<Box<dyn TransportWriter>>;

    /// Number of clients currently connected.
    fn connected_clients(&self) -> u32;
}
