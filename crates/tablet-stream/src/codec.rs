use crate::error::StreamError;
use crate::message::{
    Format, Metrics, StreamMessage, KIND_CAPABILITIES, KIND_HEARTBEAT, KIND_METRICS,
    KIND_PROXIMITY, KIND_SAMPLE, KIND_TABLET_BUTTON,
};
use serde::{Deserialize, Serialize};
use tablet_core::{DeviceCapabilities, PenSample};

// ---------------------------------------------------------------------------
// Internal serde-friendly mirror types for postcard/JSON round-trips
// ---------------------------------------------------------------------------

/// Thin wrapper used for `Proximity` postcard encoding.
#[derive(Serialize, Deserialize)]
struct ProximityPayload {
    in_range: bool,
    tool_serial: u64,
}

/// Thin wrapper used for `TabletButton` postcard encoding.
#[derive(Serialize, Deserialize)]
struct TabletButtonPayload {
    index: u8,
    pressed: bool,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Encode a `StreamMessage` into a `(kind_byte, payload_bytes)` pair.
///
/// The payload bytes are ready to embed into a binary frame or a JSON line;
/// the caller is responsible for framing (length prefix, kind byte, etc.).
pub fn encode_payload(msg: &StreamMessage, format: Format) -> Result<(u8, Vec<u8>), StreamError> {
    match (msg, format) {
        // ── Postcard ─────────────────────────────────────────────────────
        (StreamMessage::Capabilities(caps), Format::Postcard) => {
            let bytes =
                postcard::to_allocvec(caps).map_err(|e| StreamError::Encode(e.to_string()))?;
            Ok((KIND_CAPABILITIES, bytes))
        }
        (StreamMessage::Sample(sample), Format::Postcard) => {
            let bytes =
                postcard::to_allocvec(sample).map_err(|e| StreamError::Encode(e.to_string()))?;
            Ok((KIND_SAMPLE, bytes))
        }
        (
            StreamMessage::Proximity {
                in_range,
                tool_serial,
            },
            Format::Postcard,
        ) => {
            let payload = ProximityPayload {
                in_range: *in_range,
                tool_serial: *tool_serial,
            };
            let bytes =
                postcard::to_allocvec(&payload).map_err(|e| StreamError::Encode(e.to_string()))?;
            Ok((KIND_PROXIMITY, bytes))
        }
        (StreamMessage::Metrics(metrics), Format::Postcard) => {
            let bytes =
                postcard::to_allocvec(metrics).map_err(|e| StreamError::Encode(e.to_string()))?;
            Ok((KIND_METRICS, bytes))
        }
        (StreamMessage::Heartbeat, Format::Postcard) => Ok((KIND_HEARTBEAT, Vec::new())),
        (StreamMessage::TabletButton { index, pressed }, Format::Postcard) => {
            let payload = TabletButtonPayload {
                index: *index,
                pressed: *pressed,
            };
            let bytes =
                postcard::to_allocvec(&payload).map_err(|e| StreamError::Encode(e.to_string()))?;
            Ok((KIND_TABLET_BUTTON, bytes))
        }

        // ── JSON ──────────────────────────────────────────────────────────
        (StreamMessage::Capabilities(caps), Format::Json) => {
            let bytes = serde_json::to_vec(caps).map_err(|e| StreamError::Encode(e.to_string()))?;
            Ok((KIND_CAPABILITIES, bytes))
        }
        (StreamMessage::Sample(sample), Format::Json) => {
            let bytes =
                serde_json::to_vec(sample).map_err(|e| StreamError::Encode(e.to_string()))?;
            Ok((KIND_SAMPLE, bytes))
        }
        (
            StreamMessage::Proximity {
                in_range,
                tool_serial,
            },
            Format::Json,
        ) => {
            let payload = serde_json::json!({
                "in_range": in_range,
                "tool_serial": tool_serial,
            });
            let bytes =
                serde_json::to_vec(&payload).map_err(|e| StreamError::Encode(e.to_string()))?;
            Ok((KIND_PROXIMITY, bytes))
        }
        (StreamMessage::Metrics(metrics), Format::Json) => {
            let bytes =
                serde_json::to_vec(metrics).map_err(|e| StreamError::Encode(e.to_string()))?;
            Ok((KIND_METRICS, bytes))
        }
        (StreamMessage::Heartbeat, Format::Json) => Ok((KIND_HEARTBEAT, Vec::new())),
        (StreamMessage::TabletButton { index, pressed }, Format::Json) => {
            let payload = serde_json::json!({
                "index": index,
                "pressed": pressed,
            });
            let bytes =
                serde_json::to_vec(&payload).map_err(|e| StreamError::Encode(e.to_string()))?;
            Ok((KIND_TABLET_BUTTON, bytes))
        }
    }
}

/// Decode a payload slice (plus its kind byte) back into a `StreamMessage`.
pub fn decode_payload(
    kind: u8,
    bytes: &[u8],
    format: Format,
) -> Result<StreamMessage, StreamError> {
    match (kind, format) {
        // ── Postcard ─────────────────────────────────────────────────────
        (KIND_CAPABILITIES, Format::Postcard) => {
            let caps: DeviceCapabilities =
                postcard::from_bytes(bytes).map_err(|e| StreamError::Decode(e.to_string()))?;
            Ok(StreamMessage::Capabilities(caps))
        }
        (KIND_SAMPLE, Format::Postcard) => {
            let sample: PenSample =
                postcard::from_bytes(bytes).map_err(|e| StreamError::Decode(e.to_string()))?;
            Ok(StreamMessage::Sample(sample))
        }
        (KIND_PROXIMITY, Format::Postcard) => {
            let p: ProximityPayload =
                postcard::from_bytes(bytes).map_err(|e| StreamError::Decode(e.to_string()))?;
            Ok(StreamMessage::Proximity {
                in_range: p.in_range,
                tool_serial: p.tool_serial,
            })
        }
        (KIND_METRICS, Format::Postcard) => {
            let metrics: Metrics =
                postcard::from_bytes(bytes).map_err(|e| StreamError::Decode(e.to_string()))?;
            Ok(StreamMessage::Metrics(metrics))
        }
        (KIND_HEARTBEAT, Format::Postcard) => Ok(StreamMessage::Heartbeat),
        (KIND_TABLET_BUTTON, Format::Postcard) => {
            let p: TabletButtonPayload =
                postcard::from_bytes(bytes).map_err(|e| StreamError::Decode(e.to_string()))?;
            Ok(StreamMessage::TabletButton {
                index: p.index,
                pressed: p.pressed,
            })
        }

        // ── JSON ──────────────────────────────────────────────────────────
        (KIND_CAPABILITIES, Format::Json) => {
            let caps: DeviceCapabilities =
                serde_json::from_slice(bytes).map_err(|e| StreamError::Decode(e.to_string()))?;
            Ok(StreamMessage::Capabilities(caps))
        }
        (KIND_SAMPLE, Format::Json) => {
            let sample: PenSample =
                serde_json::from_slice(bytes).map_err(|e| StreamError::Decode(e.to_string()))?;
            Ok(StreamMessage::Sample(sample))
        }
        (KIND_PROXIMITY, Format::Json) => {
            #[derive(Deserialize)]
            struct P {
                in_range: bool,
                tool_serial: u64,
            }
            let p: P =
                serde_json::from_slice(bytes).map_err(|e| StreamError::Decode(e.to_string()))?;
            Ok(StreamMessage::Proximity {
                in_range: p.in_range,
                tool_serial: p.tool_serial,
            })
        }
        (KIND_METRICS, Format::Json) => {
            let metrics: Metrics =
                serde_json::from_slice(bytes).map_err(|e| StreamError::Decode(e.to_string()))?;
            Ok(StreamMessage::Metrics(metrics))
        }
        (KIND_HEARTBEAT, Format::Json) => Ok(StreamMessage::Heartbeat),
        (KIND_TABLET_BUTTON, Format::Json) => {
            let p: TabletButtonPayload =
                serde_json::from_slice(bytes).map_err(|e| StreamError::Decode(e.to_string()))?;
            Ok(StreamMessage::TabletButton {
                index: p.index,
                pressed: p.pressed,
            })
        }

        // ── Unknown kind ─────────────────────────────────────────────────
        (k, _) => Err(StreamError::UnknownKind(k)),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tablet_core::{AxisInfo, AxisUnit, DeviceCapabilities, PenSample, ToolKind};

    fn make_caps() -> DeviceCapabilities {
        let unsupported = AxisInfo {
            min: 0,
            max: 0,
            resolution: 0.0,
            unit: AxisUnit::None,
            supported: false,
        };
        DeviceCapabilities {
            device_name: "Test Tablet".to_string(),
            driver_version: "1.2.3".to_string(),
            x: AxisInfo {
                min: 0,
                max: 60960,
                resolution: 100.0,
                unit: AxisUnit::Inch,
                supported: true,
            },
            y: AxisInfo {
                min: 0,
                max: 45720,
                resolution: 100.0,
                unit: AxisUnit::Inch,
                supported: true,
            },
            z: unsupported,
            pressure: AxisInfo {
                min: 0,
                max: 1023,
                resolution: 1.0,
                unit: AxisUnit::None,
                supported: true,
            },
            tangent_pressure: unsupported,
            azimuth: unsupported,
            altitude: unsupported,
            twist: unsupported,
            rotation: unsupported,
            max_packet_rate_hz: 200,
            queue_size: 1024,
        }
    }

    fn make_sample(serial: u32) -> PenSample {
        PenSample {
            t_capture_ns: serial as u64 * 5_000_000,
            t_device_ms: serial,
            serial,
            x_raw: 1000,
            y_raw: 2000,
            z_raw: 0,
            x_norm: 0.5,
            y_norm: 0.5,
            pressure_raw: 512,
            pressure_norm: 0.5,
            tangent_pressure_raw: None,
            azimuth_deci_deg: None,
            altitude_deci_deg: None,
            twist_deci_deg: None,
            tilt_x_deg: None,
            tilt_y_deg: None,
            rotation_deci_deg: None,
            buttons: 0,
            tool: ToolKind::Pen,
            tool_serial: 1,
            in_proximity: true,
            status: 0,
        }
    }

    fn make_metrics() -> Metrics {
        Metrics {
            packets_per_sec: 200.0,
            dropped: 0,
            queue_depth: 5,
            actual_rate_hz: 200,
            requested_rate_hz: 200,
            connected_clients: 1,
        }
    }

    fn roundtrip(msg: &StreamMessage, format: Format) {
        let (kind, payload) = encode_payload(msg, format).expect("encode failed");
        let decoded = decode_payload(kind, &payload, format).expect("decode failed");
        // Compare by re-encoding (since StreamMessage doesn't implement PartialEq)
        let (kind2, payload2) = encode_payload(&decoded, format).expect("re-encode failed");
        assert_eq!(kind, kind2, "kind mismatch for format {format:?}");
        assert_eq!(payload, payload2, "payload mismatch for format {format:?}");
    }

    #[test]
    fn postcard_pen_sample_roundtrip() {
        roundtrip(&StreamMessage::Sample(make_sample(42)), Format::Postcard);
    }

    #[test]
    fn postcard_capabilities_roundtrip() {
        roundtrip(&StreamMessage::Capabilities(make_caps()), Format::Postcard);
    }

    #[test]
    fn postcard_metrics_roundtrip() {
        roundtrip(&StreamMessage::Metrics(make_metrics()), Format::Postcard);
    }

    #[test]
    fn postcard_proximity_roundtrip() {
        roundtrip(
            &StreamMessage::Proximity {
                in_range: true,
                tool_serial: 99,
            },
            Format::Postcard,
        );
    }

    #[test]
    fn postcard_heartbeat_roundtrip() {
        roundtrip(&StreamMessage::Heartbeat, Format::Postcard);
    }

    #[test]
    fn json_pen_sample_roundtrip() {
        roundtrip(&StreamMessage::Sample(make_sample(7)), Format::Json);
    }

    #[test]
    fn json_capabilities_roundtrip() {
        roundtrip(&StreamMessage::Capabilities(make_caps()), Format::Json);
    }

    #[test]
    fn json_metrics_roundtrip() {
        roundtrip(&StreamMessage::Metrics(make_metrics()), Format::Json);
    }

    #[test]
    fn postcard_tablet_button_roundtrip() {
        roundtrip(
            &StreamMessage::TabletButton {
                index: 3,
                pressed: true,
            },
            Format::Postcard,
        );
    }

    #[test]
    fn json_tablet_button_roundtrip() {
        roundtrip(
            &StreamMessage::TabletButton {
                index: 7,
                pressed: false,
            },
            Format::Json,
        );
    }

    #[test]
    fn unknown_kind_returns_error() {
        let err = decode_payload(0xFF, &[], Format::Postcard).unwrap_err();
        assert!(
            matches!(err, StreamError::UnknownKind(0xFF)),
            "expected UnknownKind, got {err:?}"
        );
    }
}
