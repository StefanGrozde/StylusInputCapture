use std::sync::{Arc, Mutex};

use tablet_core::{SampleEvent, TabletBackend};
use tablet_stream::{
    Format, FrameReader, FrameWriter, Metrics, MockBackend, MockConfig, StreamMessage,
};

#[test]
fn mock_serial_gaps_are_reported_in_metrics_frame() {
    const SAMPLE_COUNT: u32 = 100;
    const GAP_EVERY: u32 = 10;
    const EXPECTED_GAPS: u64 = 9;

    let mut backend = MockBackend::new(MockConfig {
        sample_count: SAMPLE_COUNT,
        rate_hz: 1_000_000,
        gap_every: Some(GAP_EVERY),
        tablet_button_every: None,
    });

    let events = Arc::new(Mutex::new(Vec::new()));
    let sink_events = Arc::clone(&events);
    backend
        .start(Box::new(move |event| {
            sink_events.lock().unwrap().push(event);
        }))
        .unwrap();
    backend.stop().unwrap();

    let events = events.lock().unwrap();
    let samples: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            SampleEvent::Sample(sample) => Some(*sample),
            _ => None,
        })
        .collect();

    assert_eq!(samples.len(), SAMPLE_COUNT as usize);

    let detected_gaps: u64 = samples
        .windows(2)
        .map(|pair| {
            pair[1]
                .serial
                .saturating_sub(pair[0].serial)
                .saturating_sub(1) as u64
        })
        .sum();

    assert!(detected_gaps > 0);
    assert_eq!(detected_gaps, EXPECTED_GAPS);

    let mut encoded = Vec::new();
    let mut writer = FrameWriter::new(&mut encoded, Format::Postcard);
    writer.write_header().unwrap();
    for event in events.iter().cloned() {
        writer
            .write_message(&stream_message_from_event(event))
            .unwrap();
    }
    writer
        .write_message(&StreamMessage::Metrics(Metrics {
            packets_per_sec: samples.len() as f64,
            dropped: detected_gaps,
            queue_depth: 0,
            actual_rate_hz: 200,
            requested_rate_hz: 200,
            connected_clients: 1,
        }))
        .unwrap();
    drop(writer);

    let mut reader = FrameReader::new_from_header(std::io::Cursor::new(encoded)).unwrap();
    let metrics = loop {
        match reader.read_message().unwrap() {
            StreamMessage::Metrics(metrics) => break metrics,
            _ => continue,
        }
    };

    assert_eq!(metrics.dropped, EXPECTED_GAPS);
    assert_eq!(metrics.dropped, detected_gaps);
}

fn stream_message_from_event(event: SampleEvent) -> StreamMessage {
    match event {
        SampleEvent::Capabilities(caps) => StreamMessage::Capabilities(caps),
        SampleEvent::Sample(sample) => StreamMessage::Sample(sample),
        SampleEvent::Proximity {
            in_range,
            tool_serial,
        } => StreamMessage::Proximity {
            in_range,
            tool_serial,
        },
        SampleEvent::TabletButton { index, pressed } => {
            StreamMessage::TabletButton { index, pressed }
        }
    }
}
