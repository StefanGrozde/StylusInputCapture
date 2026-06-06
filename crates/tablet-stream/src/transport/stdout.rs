//! Stdout transport — writes to stdout, suitable for piping.

use std::io;

use super::{Transport, TransportWriter};

/// A transport that yields stdout as its one and only client.
///
/// `next_client()` returns `Some(stdout)` once, then `None` forever, since
/// stdout is a singleton.
pub struct StdoutTransport {
    given: bool,
}

impl StdoutTransport {
    /// Create a new `StdoutTransport`.
    pub fn new() -> Self {
        Self { given: false }
    }
}

impl Default for StdoutTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl Transport for StdoutTransport {
    fn next_client(&mut self) -> Option<Box<dyn TransportWriter>> {
        if !self.given {
            self.given = true;
            Some(Box::new(io::stdout()))
        } else {
            None
        }
    }

    fn connected_clients(&self) -> u32 {
        if self.given {
            1
        } else {
            0
        }
    }
}
