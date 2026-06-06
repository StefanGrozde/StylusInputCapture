// tablet-stream: wire formats, framing/handshake, and transports

pub mod codec;
pub mod error;
pub mod framing;
pub mod message;
pub mod mock;
pub mod transport;

// Convenience re-exports for the most commonly used types.
pub use codec::{decode_payload, encode_payload};
pub use error::StreamError;
pub use framing::{FrameReader, FrameWriter, MAGIC, PROTOCOL_VERSION};
pub use message::{
    Format, Metrics, StreamMessage, KIND_CAPABILITIES, KIND_HEARTBEAT, KIND_METRICS,
    KIND_PROXIMITY, KIND_SAMPLE,
};
pub use mock::{MockBackend, MockConfig};
pub use transport::{Transport, TransportWriter};
