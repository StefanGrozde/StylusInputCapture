use serde::{Deserialize, Serialize};
use tablet_core::{DeviceCapabilities, PenSample};

/// Frame-kind discriminant constants (u8), as defined in SPEC_1.md §7.3.
pub const KIND_CAPABILITIES: u8 = 0x01;
pub const KIND_SAMPLE: u8 = 0x02;
pub const KIND_PROXIMITY: u8 = 0x03;
pub const KIND_METRICS: u8 = 0x04;
pub const KIND_HEARTBEAT: u8 = 0x05;
pub const KIND_TABLET_BUTTON: u8 = 0x06;

/// Wire format selector.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Format {
    /// `postcard` binary encoding — compact, allocation-light, preferred for
    /// real-time streaming.
    Postcard,
    /// JSONL text encoding — one JSON object per line, human-readable.
    Json,
}

/// Periodic runtime metrics emitted as a `KIND_METRICS` frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Metrics {
    /// Samples delivered to the transport per second.
    pub packets_per_sec: f64,
    /// Cumulative samples dropped due to ring-buffer overflow.
    pub dropped: u64,
    /// Current ring-buffer occupancy.
    pub queue_depth: u32,
    /// Actual negotiated Wintab packet rate.
    pub actual_rate_hz: u32,
    /// Rate requested by the caller.
    pub requested_rate_hz: u32,
    /// Number of currently connected transport clients.
    pub connected_clients: u32,
}

/// Every message type that can appear in a stream, one variant per kind byte.
#[derive(Debug, Clone)]
pub enum StreamMessage {
    /// Device capability descriptor — first message in every session.
    Capabilities(DeviceCapabilities),
    /// A single decoded pen sample.
    Sample(PenSample),
    /// Pen proximity change (enter / leave sensing area).
    Proximity {
        /// `true` if the tool entered range; `false` if it left.
        in_range: bool,
        /// Physical pen serial associated with the event.
        tool_serial: u64,
    },
    /// Periodic metrics snapshot.
    Metrics(Metrics),
    /// Keepalive / no-op; sent periodically when no samples arrive.
    Heartbeat,
    /// A physical tablet button (ExpressKey) changed state. `index` is the
    /// backend-assigned stable per-device ordinal (see
    /// `tablet_core::SampleEvent::TabletButton`).
    TabletButton {
        /// Stable per-device button ordinal.
        index: u8,
        /// `true` on press, `false` on release.
        pressed: bool,
    },
}
