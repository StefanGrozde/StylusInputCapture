//! End-to-end integration tests for the tablet-stream crate.
//!
//! These tests cover the full pipeline: MockBackend -> FrameWriter -> wire
//! bytes -> FrameReader, as well as the TCP transport round-trip and JSONL
//! format verification.

use std::io::Cursor;
use std::sync::{Arc, Mutex};

use tablet_core::{AxisInfo, AxisUnit, DeviceCapabilities, PenSample, SampleEvent, ToolKind};
use tablet_stream::{
    framing::{FrameReader, FrameWriter, MAGIC, PROTOCOL_VERSION},
    message::{Format, Metrics, StreamMessage},
    mock::{MockBackend, MockConfig},
    transport::tcp::TcpTransport,
    transport::Transport,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_caps() -> DeviceCapabilities {
    let unsupported = AxisInfo {
        min: 0,
        max: 0,
        resolution: 0.0,
        unit: AxisUnit::None,
        supported: false,
    };
    DeviceCapabilities {
        device_name: "Integration Test Tablet".to_string(),
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
        x_raw: 1000 + serial as i32 * 10,
        y_raw: 2000 + serial as i32 * 10,
        z_raw: 0,
        x_norm: serial as f64 * 0.01,
        y_norm: serial as f64 * 0.01,
        pressure_raw: serial * 10,
        pressure_norm: serial as f64 * 0.01,
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

// ---------------------------------------------------------------------------
// Test A: in-memory FrameWriter/FrameReader round-trip (postcard)
// ---------------------------------------------------------------------------

/// Verify that writing a structured sequence of messages to a Vec<u8> and
/// reading them back produces the same types in the same order.
#[test]
fn test_a_in_memory_roundtrip_postcard() {
    let caps = make_caps();
    let samples: Vec<PenSample> = (1..=5).map(make_sample).collect();
    let metrics = Metrics {
        packets_per_sec: 200.0,
        dropped: 0,
        queue_depth: 3,
        actual_rate_hz: 200,
        requested_rate_hz: 200,
        connected_clients: 1,
    };

    // ── Write ─────────────────────────────────────────────────────────────
    let mut buf = Vec::<u8>::new();
    let mut writer = FrameWriter::new(&mut buf, Format::Postcard);
    writer.write_header().unwrap();
    writer
        .write_message(&StreamMessage::Capabilities(caps.clone()))
        .unwrap();
    for s in &samples {
        writer.write_message(&StreamMessage::Sample(*s)).unwrap();
    }
    writer
        .write_message(&StreamMessage::Metrics(metrics))
        .unwrap();
    writer.write_message(&StreamMessage::Heartbeat).unwrap();
    drop(writer);

    // ── Validate binary header layout ─────────────────────────────────────
    assert_eq!(&buf[0..4], &MAGIC, "magic mismatch");
    assert_eq!(
        u16::from_le_bytes([buf[4], buf[5]]),
        PROTOCOL_VERSION,
        "version mismatch"
    );
    assert_eq!(buf[6], 0u8, "format byte must be 0 for postcard");

    // ── Read back ─────────────────────────────────────────────────────────
    let cursor = Cursor::new(buf);
    let mut reader = FrameReader::new_from_header(cursor).unwrap();

    let msg0 = reader.read_message().unwrap();
    assert!(
        matches!(msg0, StreamMessage::Capabilities(_)),
        "expected Capabilities first"
    );

    let mut last_serial = 0u32;
    for i in 0..5 {
        let msg = reader.read_message().unwrap();
        if let StreamMessage::Sample(s) = msg {
            assert!(
                s.serial > last_serial || last_serial == 0,
                "serial must be increasing: {} then {}",
                last_serial,
                s.serial
            );
            assert_eq!(s.serial, i + 1, "serial must match index");
            last_serial = s.serial;
        } else {
            panic!("expected Sample at position {i}, got something else");
        }
    }

    assert!(
        matches!(reader.read_message().unwrap(), StreamMessage::Metrics(_)),
        "expected Metrics after samples"
    );
    assert!(
        matches!(reader.read_message().unwrap(), StreamMessage::Heartbeat),
        "expected Heartbeat at end"
    );
}

// ---------------------------------------------------------------------------
// Test B: TCP round-trip
// ---------------------------------------------------------------------------

/// Bind an ephemeral TCP port, connect a client, write Capabilities + 3
/// Samples from server to client, verify via FrameReader on the client side.
#[test]
fn test_b_tcp_roundtrip() {
    use std::net::TcpStream;
    use std::thread;

    // Bind to an ephemeral port.
    let mut transport = TcpTransport::bind("127.0.0.1:0").expect("failed to bind TCP");
    let addr = transport.local_addr().expect("failed to get local addr");

    // Client thread: connect and read back the frames.
    let client_thread = thread::spawn(move || {
        // Small retry loop in case the server hasn't called accept yet.
        let stream = loop {
            match TcpStream::connect(addr) {
                Ok(s) => break s,
                Err(_) => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            }
        };

        let mut reader =
            FrameReader::new_from_header(stream).expect("client: failed to read handshake header");

        let msg0 = reader.read_message().expect("client: read Capabilities");
        assert!(
            matches!(msg0, StreamMessage::Capabilities(_)),
            "client: expected Capabilities first"
        );

        let mut prev_serial = 0u32;
        for i in 0..3 {
            let msg = reader.read_message().expect("client: read Sample");
            if let StreamMessage::Sample(s) = msg {
                assert!(
                    s.serial >= prev_serial,
                    "client: serial out of order at sample {i}"
                );
                prev_serial = s.serial;
            } else {
                panic!("client: expected Sample at index {i}");
            }
        }
    });

    // Server: accept the client.
    let client_writer = loop {
        if let Some(w) = transport.next_client() {
            break w;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    };

    let mut writer = FrameWriter::new(client_writer, Format::Postcard);
    writer.write_header().unwrap();
    writer
        .write_message(&StreamMessage::Capabilities(make_caps()))
        .unwrap();
    for i in 1..=3u32 {
        writer
            .write_message(&StreamMessage::Sample(make_sample(i)))
            .unwrap();
    }
    drop(writer); // close the connection

    client_thread.join().expect("client thread panicked");
}

// ---------------------------------------------------------------------------
// Test C: JSONL format round-trip
// ---------------------------------------------------------------------------

/// Write messages in JSONL format to a buffer, split by newlines, and verify
/// that each line is valid JSON with a `"kind"` field and expected payload
/// fields.
#[test]
fn test_c_jsonl_format_roundtrip() {
    let caps = make_caps();
    let sample = make_sample(7);
    let metrics = Metrics {
        packets_per_sec: 150.0,
        dropped: 0,
        queue_depth: 2,
        actual_rate_hz: 200,
        requested_rate_hz: 200,
        connected_clients: 1,
    };

    // ── Write ─────────────────────────────────────────────────────────────
    let mut buf = Vec::<u8>::new();
    let mut writer = FrameWriter::new(&mut buf, Format::Json);
    // Note: write_header is a no-op for JSON but shouldn't panic.
    writer.write_header().unwrap();
    writer
        .write_message(&StreamMessage::Capabilities(caps))
        .unwrap();
    writer
        .write_message(&StreamMessage::Sample(sample))
        .unwrap();
    writer
        .write_message(&StreamMessage::Metrics(metrics))
        .unwrap();
    writer.write_message(&StreamMessage::Heartbeat).unwrap();
    drop(writer);

    // ── Parse lines ───────────────────────────────────────────────────────
    let text = std::str::from_utf8(&buf).expect("output must be UTF-8");
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();

    assert_eq!(lines.len(), 4, "expected 4 JSONL lines");

    // Verify kind fields.
    let kind_strings: Vec<String> = lines
        .iter()
        .map(|line| {
            let v: serde_json::Value =
                serde_json::from_str(line).expect("each line must be valid JSON");
            v["kind"]
                .as_str()
                .expect("must have 'kind' field")
                .to_string()
        })
        .collect();

    assert_eq!(kind_strings[0], "capabilities");
    assert_eq!(kind_strings[1], "sample");
    assert_eq!(kind_strings[2], "metrics");
    assert_eq!(kind_strings[3], "heartbeat");

    // Verify that the sample line has the `serial` field.
    let sample_json: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert!(
        sample_json.get("serial").is_some(),
        "sample JSON must contain 'serial' field"
    );
    assert_eq!(
        sample_json["serial"],
        serde_json::json!(7u32),
        "serial must be 7"
    );

    // Verify capabilities line has device_name.
    let caps_json: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert!(
        caps_json.get("device_name").is_some(),
        "capabilities JSON must contain 'device_name'"
    );

    // Verify metrics line has all 6 required fields.
    let metrics_json: serde_json::Value = serde_json::from_str(lines[2]).unwrap();
    for field in &[
        "packets_per_sec",
        "dropped",
        "queue_depth",
        "actual_rate_hz",
        "requested_rate_hz",
        "connected_clients",
    ] {
        assert!(
            metrics_json.get(field).is_some(),
            "metrics JSON must contain field '{field}'"
        );
    }

    // ── Also verify round-trip via FrameReader ─────────────────────────────
    // Re-write and read back using the JSONL reader path.
    let mut buf2 = Vec::<u8>::new();
    let mut writer2 = FrameWriter::new(&mut buf2, Format::Json);
    writer2
        .write_message(&StreamMessage::Capabilities(make_caps()))
        .unwrap();
    writer2
        .write_message(&StreamMessage::Sample(make_sample(7)))
        .unwrap();
    writer2.write_message(&StreamMessage::Heartbeat).unwrap();
    drop(writer2);

    // Use internal FrameReader with Json format (no binary header).
    use std::io::BufReader;
    let mut reader =
        tablet_stream::framing::FrameReader::new_json(BufReader::new(Cursor::new(buf2)));
    let m0 = reader.read_message().unwrap();
    let m1 = reader.read_message().unwrap();
    let m2 = reader.read_message().unwrap();
    assert!(matches!(m0, StreamMessage::Capabilities(_)));
    assert!(matches!(m1, StreamMessage::Sample(_)));
    assert!(matches!(m2, StreamMessage::Heartbeat));
}

// ---------------------------------------------------------------------------
// Test D: MockBackend drives a postcard stream end-to-end
// ---------------------------------------------------------------------------

/// Use MockBackend to generate events, encode them with FrameWriter, then
/// decode with FrameReader, asserting capabilities-first and increasing serials.
#[test]
fn test_d_mock_backend_end_to_end() {
    use tablet_core::TabletBackend;

    let config = MockConfig {
        sample_count: 10,
        rate_hz: 50_000, // Very fast for tests — ~20µs per sample
        gap_every: None,
    };
    let mut backend = MockBackend::new(config);

    let events: Arc<Mutex<Vec<SampleEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let events_clone = Arc::clone(&events);

    backend
        .start(Box::new(move |ev| {
            events_clone.lock().unwrap().push(ev);
        }))
        .unwrap();
    backend.stop().unwrap();

    let collected = events.lock().unwrap();
    assert!(
        !collected.is_empty(),
        "MockBackend must emit at least one event"
    );

    // ── Encode into a buffer ───────────────────────────────────────────────
    let mut buf = Vec::<u8>::new();
    let mut writer = FrameWriter::new(&mut buf, Format::Postcard);
    writer.write_header().unwrap();

    for ev in collected.iter() {
        let msg = match ev {
            SampleEvent::Capabilities(c) => StreamMessage::Capabilities(c.clone()),
            SampleEvent::Sample(s) => StreamMessage::Sample(*s),
            SampleEvent::Proximity {
                in_range,
                tool_serial,
            } => StreamMessage::Proximity {
                in_range: *in_range,
                tool_serial: *tool_serial,
            },
        };
        writer.write_message(&msg).unwrap();
    }
    drop(writer);

    // ── Decode ────────────────────────────────────────────────────────────
    let mut reader = FrameReader::new_from_header(Cursor::new(buf)).unwrap();

    let first = reader.read_message().unwrap();
    assert!(
        matches!(first, StreamMessage::Capabilities(_)),
        "first decoded message must be Capabilities"
    );

    let mut prev_serial = 0u32;
    let mut sample_count = 0;
    for _ in 0..(collected.len() - 1) {
        let msg = reader.read_message().unwrap();
        if let StreamMessage::Sample(s) = msg {
            assert!(
                s.serial > prev_serial || prev_serial == 0,
                "serials must be increasing: {prev_serial} then {}",
                s.serial
            );
            prev_serial = s.serial;
            sample_count += 1;
        }
    }

    assert_eq!(sample_count, 10, "must decode exactly 10 samples");
}
