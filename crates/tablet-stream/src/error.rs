/// Errors that can occur while encoding, decoding, framing, or transporting
/// stream messages.
#[derive(Debug, thiserror::Error)]
pub enum StreamError {
    #[error("encode failed: {0}")]
    Encode(String),

    #[error("decode failed: {0}")]
    Decode(String),

    #[error("unknown frame kind: {0:#04x}")]
    UnknownKind(u8),

    #[error("bad magic: expected WCAP")]
    BadMagic,

    #[error("version mismatch: got {got}, expected {expected}")]
    VersionMismatch { got: u16, expected: u16 },

    #[error("truncated frame")]
    TruncatedFrame,

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
