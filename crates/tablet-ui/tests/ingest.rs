use std::io::Cursor;

use tablet_consumer::{ingest_reader, ConnectionStatus, SharedSource};
use tablet_core::{AxisInfo, AxisUnit, DeviceCapabilities, PenSample, ToolKind};
use tablet_stream::{Format, FrameWriter, Metrics, StreamMessage};

fn axis(supported: bool) -> AxisInfo {
    AxisInfo {
        min: 0,
        max: 1000,
        resolution: 100.0,
        unit: AxisUnit::None,
        supported,
    }
}

fn caps() -> DeviceCapabilities {
    let unsupported = axis(false);
    DeviceCapabilities {
        device_name: "Ingest Test Tablet".to_owned(),
        driver_version: "1.0".to_owned(),
        x: axis(true),
        y: axis(true),
        z: unsupported,
        pressure: axis(true),
        tangent_pressure: unsupported,
        azimuth: unsupported,
        altitude: unsupported,
        twist: unsupported,
        rotation: unsupported,
        max_packet_rate_hz: 200,
        queue_size: 128,
    }
}

fn sample(serial: u32) -> PenSample {
    PenSample {
        t_capture_ns: u64::from(serial) * 5_000_000,
        t_device_ms: serial,
        serial,
        x_raw: 100,
        y_raw: 200,
        z_raw: 0,
        x_norm: 0.1,
        y_norm: 0.2,
        pressure_raw: 300,
        pressure_norm: 0.3,
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

fn metrics() -> Metrics {
    Metrics {
        packets_per_sec: 199.0,
        dropped: 7,
        queue_depth: 4,
        actual_rate_hz: 200,
        requested_rate_hz: 250,
        connected_clients: 1,
    }
}

fn encode(messages: &[StreamMessage], format: Format) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut writer = FrameWriter::new(&mut bytes, format);
    writer.write_header().unwrap();
    for message in messages {
        writer.write_message(message).unwrap();
    }
    drop(writer);
    bytes
}

fn ingest(messages: Vec<StreamMessage>, format: Format, capacity: usize) -> SharedSource {
    let shared = SharedSource::new(capacity);
    let bytes = encode(&messages, format);
    let state = ingest_reader(Cursor::new(bytes), format, shared.clone()).unwrap();

    assert_eq!(state.status, ConnectionStatus::Disconnected);
    shared
}

#[test]
fn postcard_ingestion_captures_caps_metrics_samples_gaps_and_eof() {
    let messages = vec![
        StreamMessage::Capabilities(caps()),
        StreamMessage::Sample(sample(1)),
        StreamMessage::Sample(sample(2)),
        StreamMessage::Sample(sample(5)),
        StreamMessage::Metrics(metrics()),
    ];

    let shared = ingest(messages, Format::Postcard, 16);
    let state = shared.state();
    let drained = shared.drain_messages();
    let sample_count = drained
        .iter()
        .filter(|message| matches!(message, StreamMessage::Sample(_)))
        .count();

    assert_eq!(state.status, ConnectionStatus::Disconnected);
    assert_eq!(
        state.latest_capabilities.unwrap().device_name,
        "Ingest Test Tablet"
    );
    assert_eq!(state.latest_metrics.unwrap().dropped, 7);
    assert_eq!(state.serial_gaps, 2);
    assert_eq!(state.display_dropped, 0);
    assert_eq!(sample_count, 3);
}

#[test]
fn json_ingestion_uses_json_reader_path() {
    let messages = vec![
        StreamMessage::Capabilities(caps()),
        StreamMessage::Sample(sample(10)),
        StreamMessage::Sample(sample(11)),
        StreamMessage::Heartbeat,
    ];

    let shared = ingest(messages, Format::Json, 16);
    let state = shared.state();
    let drained = shared.drain_messages();

    assert_eq!(state.status, ConnectionStatus::Disconnected);
    assert_eq!(state.serial_gaps, 0);
    assert_eq!(state.latest_capabilities.unwrap().queue_size, 128);
    assert_eq!(drained.len(), 4);
}

#[test]
fn overflow_drops_oldest_and_increments_display_dropped() {
    let messages = vec![
        StreamMessage::Sample(sample(1)),
        StreamMessage::Sample(sample(2)),
        StreamMessage::Sample(sample(3)),
        StreamMessage::Sample(sample(4)),
    ];

    let shared = ingest(messages, Format::Postcard, 2);
    let state = shared.state();
    let drained = shared.drain_messages();
    let serials: Vec<u32> = drained
        .into_iter()
        .filter_map(|message| match message {
            StreamMessage::Sample(sample) => Some(sample.serial),
            _ => None,
        })
        .collect();

    assert_eq!(state.display_dropped, 2);
    assert_eq!(serials, [3, 4]);
}
