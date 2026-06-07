use std::io::{BufRead, BufReader, Read, Write};

use crate::codec::{decode_payload, encode_payload};
use crate::error::StreamError;
use crate::message::{
    Format, StreamMessage, KIND_CAPABILITIES, KIND_HEARTBEAT, KIND_METRICS, KIND_PROXIMITY,
    KIND_SAMPLE,
};

// ---------------------------------------------------------------------------
// Protocol constants
// ---------------------------------------------------------------------------

/// Four-byte magic that starts every binary stream.
pub const MAGIC: [u8; 4] = *b"WCAP";

/// Current protocol version (u16 LE, placed after the magic).
pub const PROTOCOL_VERSION: u16 = 1;

/// Format byte value for `Format::Postcard`.
const FORMAT_BYTE_POSTCARD: u8 = 0;
/// Format byte value for `Format::Json`.
const FORMAT_BYTE_JSON: u8 = 1;

// ---------------------------------------------------------------------------
// FrameWriter
// ---------------------------------------------------------------------------

/// Writes framed `StreamMessage`s to an underlying `Write` sink.
///
/// Binary mode:
/// ```text
/// Header:  [MAGIC 4B][u16 LE version][u8 format]   (7 bytes)
/// Frame:   [u32 LE payload_len][u8 kind][payload]
/// ```
///
/// JSONL mode: no binary header; each message is a single newline-terminated
/// JSON object with an additional `"kind"` field.
pub struct FrameWriter<W: Write> {
    inner: W,
    format: Format,
    header_written: bool,
}

impl<W: Write> FrameWriter<W> {
    /// Create a new `FrameWriter`.  Call `write_header` before `write_message`.
    pub fn new(inner: W, format: Format) -> Self {
        Self {
            inner,
            format,
            header_written: false,
        }
    }

    /// Write the protocol header.
    ///
    /// - **Binary mode:** `[MAGIC][u16 LE version][u8 format]` — 7 bytes.
    /// - **JSONL mode:** no-op (JSON streams carry no binary preamble).
    pub fn write_header(&mut self) -> Result<(), StreamError> {
        self.header_written = true;
        if self.format == Format::Postcard {
            self.inner.write_all(&MAGIC)?;
            self.inner.write_all(&PROTOCOL_VERSION.to_le_bytes())?;
            self.inner.write_all(&[FORMAT_BYTE_POSTCARD])?;
        }
        // JSONL: nothing to write for the header.
        Ok(())
    }

    /// Encode and write a single `StreamMessage`.
    ///
    /// Calls `write_header` automatically on the first call if it has not been
    /// called yet.
    pub fn write_message(&mut self, msg: &StreamMessage) -> Result<(), StreamError> {
        if !self.header_written {
            self.write_header()?;
        }

        match self.format {
            Format::Postcard => {
                let (kind, payload) = encode_payload(msg, Format::Postcard)?;
                let len = payload.len() as u32;
                self.inner.write_all(&len.to_le_bytes())?;
                self.inner.write_all(&[kind])?;
                self.inner.write_all(&payload)?;
            }
            Format::Json => {
                let line = encode_jsonl(msg)?;
                self.inner.write_all(&line)?;
                self.inner.write_all(b"\n")?;
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// JSONL helpers
// ---------------------------------------------------------------------------

/// Kind discriminant strings used in the `"kind"` JSON field.
fn kind_str(msg: &StreamMessage) -> &'static str {
    match msg {
        StreamMessage::Capabilities(_) => "capabilities",
        StreamMessage::Sample(_) => "sample",
        StreamMessage::Proximity { .. } => "proximity",
        StreamMessage::Metrics(_) => "metrics",
        StreamMessage::Heartbeat => "heartbeat",
    }
}

/// Serialize a `StreamMessage` as a flat JSON object with a `"kind"` key.
///
/// Strategy: serialize the payload struct to a `serde_json::Value`, then
/// inject the `"kind"` field and re-serialize to bytes.
fn encode_jsonl(msg: &StreamMessage) -> Result<Vec<u8>, StreamError> {
    use serde_json::Value;

    let kind = kind_str(msg);

    // Get the payload as a JSON `Value`.
    let (_, payload_bytes) = encode_payload(msg, Format::Json)?;

    let mut map = if payload_bytes.is_empty() {
        // Heartbeat — no payload fields.
        serde_json::Map::new()
    } else {
        let v: Value = serde_json::from_slice(&payload_bytes)
            .map_err(|e| StreamError::Encode(e.to_string()))?;
        match v {
            Value::Object(m) => m,
            other => {
                // Wrap non-object payloads under a "data" key.
                let mut m = serde_json::Map::new();
                m.insert("data".to_string(), other);
                m
            }
        }
    };

    map.insert("kind".to_string(), Value::String(kind.to_string()));

    serde_json::to_vec(&Value::Object(map)).map_err(|e| StreamError::Encode(e.to_string()))
}

// ---------------------------------------------------------------------------
// FrameReader
// ---------------------------------------------------------------------------

/// Reads framed `StreamMessage`s from an underlying `Read` source.
#[allow(dead_code)]
pub struct FrameReader<R: Read> {
    inner: BufReader<R>,
    format: Format,
}

impl<R: Read> FrameReader<R> {
    /// Read and validate the protocol header, then return a ready `FrameReader`.
    ///
    /// Reads exactly 7 bytes in binary mode:
    /// `[MAGIC 4B][u16 LE version][u8 format]`.
    ///
    /// Returns `StreamError::BadMagic` or `StreamError::VersionMismatch` on
    /// protocol violations.
    pub fn new_from_header(inner: R) -> Result<Self, StreamError> {
        let mut buf_reader = BufReader::new(inner);

        let mut header = [0u8; 7];
        buf_reader
            .read_exact(&mut header)
            .map_err(|_| StreamError::TruncatedFrame)?;

        // Validate magic.
        if header[0..4] != MAGIC {
            return Err(StreamError::BadMagic);
        }

        // Validate version.
        let got_ver = u16::from_le_bytes([header[4], header[5]]);
        if got_ver != PROTOCOL_VERSION {
            return Err(StreamError::VersionMismatch {
                got: got_ver,
                expected: PROTOCOL_VERSION,
            });
        }

        // Read format byte.
        let format = match header[6] {
            FORMAT_BYTE_POSTCARD => Format::Postcard,
            FORMAT_BYTE_JSON => Format::Json,
            other => {
                return Err(StreamError::Decode(format!(
                    "unknown format byte: {other:#04x}"
                )))
            }
        };

        Ok(Self {
            inner: buf_reader,
            format,
        })
    }

    /// Create a `FrameReader` that reads JSONL (no binary header) directly
    /// from an already-`BufRead` source.
    ///
    /// This is used when the writer is in `Format::Json` mode and skipped
    /// writing a binary header.
    pub fn new_json(inner: BufReader<R>) -> Self {
        Self {
            inner,
            format: Format::Json,
        }
    }

    /// Read the next `StreamMessage` from the stream.
    ///
    /// - **Binary mode:** reads `u32 LE len`, `u8 kind`, `len` payload bytes.
    /// - **JSONL mode:** reads one newline-terminated line and parses as JSON.
    pub fn read_message(&mut self) -> Result<StreamMessage, StreamError> {
        match self.format {
            Format::Postcard => {
                // Read 4-byte length prefix.
                let mut len_buf = [0u8; 4];
                self.inner
                    .read_exact(&mut len_buf)
                    .map_err(|_| StreamError::TruncatedFrame)?;
                let payload_len = u32::from_le_bytes(len_buf) as usize;

                // Read 1-byte kind.
                let mut kind_buf = [0u8; 1];
                self.inner
                    .read_exact(&mut kind_buf)
                    .map_err(|_| StreamError::TruncatedFrame)?;
                let kind = kind_buf[0];

                // Read payload.
                let mut payload = vec![0u8; payload_len];
                self.inner
                    .read_exact(&mut payload)
                    .map_err(|_| StreamError::TruncatedFrame)?;

                decode_payload(kind, &payload, Format::Postcard)
            }
            Format::Json => {
                let mut line = String::new();
                let n = self.inner.read_line(&mut line).map_err(StreamError::Io)?;
                if n == 0 {
                    return Err(StreamError::TruncatedFrame);
                }
                decode_jsonl(line.trim_end_matches('\n'))
            }
        }
    }
}

/// Parse one JSONL line back into a `StreamMessage`.
fn decode_jsonl(line: &str) -> Result<StreamMessage, StreamError> {
    use serde_json::Value;

    let mut map: serde_json::Map<String, Value> =
        serde_json::from_str(line).map_err(|e| StreamError::Decode(e.to_string()))?;

    let kind_val = map
        .remove("kind")
        .ok_or_else(|| StreamError::Decode("missing 'kind' field".to_string()))?;
    let kind_str = kind_val
        .as_str()
        .ok_or_else(|| StreamError::Decode("'kind' must be a string".to_string()))?;

    let kind_byte = match kind_str {
        "capabilities" => KIND_CAPABILITIES,
        "sample" => KIND_SAMPLE,
        "proximity" => KIND_PROXIMITY,
        "metrics" => KIND_METRICS,
        "heartbeat" => KIND_HEARTBEAT,
        other => return Err(StreamError::Decode(format!("unknown kind string: {other}"))),
    };

    // Re-serialize the remaining fields as the payload bytes for the JSON decoder.
    let payload_bytes =
        serde_json::to_vec(&Value::Object(map)).map_err(|e| StreamError::Decode(e.to_string()))?;

    decode_payload(kind_byte, &payload_bytes, Format::Json)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Metrics;
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
            driver_version: "1.0.0".to_string(),
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
            x_raw: 1234,
            y_raw: 5678,
            z_raw: 0,
            x_norm: 0.3,
            y_norm: 0.7,
            pressure_raw: 256,
            pressure_norm: 0.25,
            tangent_pressure_raw: None,
            azimuth_deci_deg: None,
            altitude_deci_deg: None,
            twist_deci_deg: None,
            tilt_x_deg: None,
            tilt_y_deg: None,
            rotation_deci_deg: None,
            buttons: 0,
            tool: ToolKind::Pen,
            tool_serial: 42,
            in_proximity: true,
            status: 0,
        }
    }

    fn make_metrics() -> Metrics {
        Metrics {
            packets_per_sec: 195.3,
            dropped: 2,
            queue_depth: 8,
            actual_rate_hz: 200,
            requested_rate_hz: 200,
            connected_clients: 1,
        }
    }

    // Helper: write header + messages to Vec<u8>, then read back.
    fn roundtrip_messages(msgs: Vec<StreamMessage>, format: Format) -> Vec<StreamMessage> {
        let mut buf = Vec::<u8>::new();

        // Write
        let mut writer = FrameWriter::new(&mut buf, format);
        writer.write_header().unwrap();
        for m in &msgs {
            writer.write_message(m).unwrap();
        }
        drop(writer);

        // Read back
        let cursor = std::io::Cursor::new(buf);

        // For JSON we don't write a binary header, so we can't use
        // new_from_header. Use a direct reader instead.
        if format == Format::Json {
            let mut reader = FrameReader {
                inner: BufReader::new(cursor),
                format: Format::Json,
            };
            (0..msgs.len())
                .map(|_| reader.read_message().unwrap())
                .collect()
        } else {
            let mut reader = FrameReader::new_from_header(cursor).unwrap();
            (0..msgs.len())
                .map(|_| reader.read_message().unwrap())
                .collect()
        }
    }

    #[test]
    fn binary_write_then_read_three_frames() {
        let msgs = vec![
            StreamMessage::Capabilities(make_caps()),
            StreamMessage::Sample(make_sample(1)),
            StreamMessage::Metrics(make_metrics()),
        ];
        let decoded = roundtrip_messages(msgs, Format::Postcard);
        assert!(matches!(decoded[0], StreamMessage::Capabilities(_)));
        assert!(matches!(decoded[1], StreamMessage::Sample(_)));
        assert!(matches!(decoded[2], StreamMessage::Metrics(_)));
    }

    #[test]
    fn json_write_then_read_three_frames() {
        let msgs = vec![
            StreamMessage::Capabilities(make_caps()),
            StreamMessage::Sample(make_sample(1)),
            StreamMessage::Heartbeat,
        ];
        let decoded = roundtrip_messages(msgs, Format::Json);
        assert!(matches!(decoded[0], StreamMessage::Capabilities(_)));
        assert!(matches!(decoded[1], StreamMessage::Sample(_)));
        assert!(matches!(decoded[2], StreamMessage::Heartbeat));
    }

    #[test]
    fn bad_magic_returns_error() {
        let data = b"NOPE\x01\x00\x00";
        let result = FrameReader::new_from_header(std::io::Cursor::new(data));
        let err = result.err().expect("expected an error for bad magic");
        assert!(
            matches!(err, StreamError::BadMagic),
            "expected BadMagic, got {err:?}"
        );
    }

    #[test]
    fn version_mismatch_returns_error() {
        // Magic OK, but version = 2 instead of 1.
        let mut data = vec![];
        data.extend_from_slice(b"WCAP");
        data.extend_from_slice(&2u16.to_le_bytes());
        data.push(FORMAT_BYTE_POSTCARD);
        let result = FrameReader::new_from_header(std::io::Cursor::new(data));
        let err = result
            .err()
            .expect("expected an error for version mismatch");
        assert!(
            matches!(
                err,
                StreamError::VersionMismatch {
                    got: 2,
                    expected: 1
                }
            ),
            "expected VersionMismatch, got {err:?}"
        );
    }

    #[test]
    fn binary_header_is_exactly_7_bytes() {
        let mut buf = Vec::<u8>::new();
        let mut writer = FrameWriter::new(&mut buf, Format::Postcard);
        writer.write_header().unwrap();
        assert_eq!(buf.len(), 7, "header must be 7 bytes");
        assert_eq!(&buf[0..4], b"WCAP");
        assert_eq!(u16::from_le_bytes([buf[4], buf[5]]), PROTOCOL_VERSION);
        assert_eq!(buf[6], FORMAT_BYTE_POSTCARD);
    }

    #[test]
    fn jsonl_has_kind_field() {
        let mut buf = Vec::<u8>::new();
        let mut writer = FrameWriter::new(&mut buf, Format::Json);
        writer
            .write_message(&StreamMessage::Sample(make_sample(5)))
            .unwrap();
        let line = std::str::from_utf8(&buf).unwrap().trim();
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(v["kind"], "sample");
    }
}
