use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::Duration,
    time::Instant,
};

use rtrb::{Consumer, Producer, RingBuffer};
use tablet_core::{BackendError, SampleEvent, TabletBackend};
use tablet_stream::{
    transport::{stdout::StdoutTransport, tcp::TcpTransport, Transport, TransportWriter},
    FrameWriter, MockBackend, MockConfig, StreamError, StreamMessage,
};
use thiserror::Error;
use tracing::{debug, info, warn};

use crate::config::{Config, Transport as ConfigTransport};
use crate::metrics::{MetricsCounters, MetricsEmitter};

type EventConsumer = Arc<Mutex<Consumer<SampleEvent>>>;

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("backend error: {0}")]
    Backend(#[from] BackendError),
    #[error("stream error: {0}")]
    Stream(#[from] StreamError),
    #[error("streaming thread panicked")]
    StreamingThreadPanicked,
    #[error("failed to install Ctrl-C handler: {0}")]
    CtrlC(#[from] ctrlc::Error),
    #[cfg(not(windows))]
    #[error("named pipe transport is only available on Windows")]
    PipeUnsupported,
}

pub fn run(config: Config) -> Result<(), RuntimeError> {
    let queue_size = config.capture.queue_size as usize;
    let (producer, consumer) = RingBuffer::<SampleEvent>::new(queue_size);
    let consumer = Arc::new(Mutex::new(consumer));
    let dropped = Arc::new(AtomicU64::new(0));
    let counters = MetricsCounters::new(Arc::clone(&dropped));
    let shutdown = Arc::new(AtomicBool::new(false));
    install_shutdown_handler(Arc::clone(&shutdown))?;

    let streaming = spawn_streaming_thread(
        config.clone(),
        Arc::clone(&consumer),
        counters.clone(),
        Arc::clone(&shutdown),
    );

    let mut backend = build_backend(&config);
    let sink_consumer = Arc::clone(&consumer);
    let sink_counters = counters.clone();
    let mut producer = producer;
    backend.start(Box::new(move |event| {
        enqueue_event(&mut producer, &sink_consumer, &sink_counters, event);
    }))?;

    wait_for_backend(&mut *backend, &shutdown)?;
    shutdown.store(true, Ordering::Release);

    streaming
        .join()
        .map_err(|_| RuntimeError::StreamingThreadPanicked)??;

    let dropped = dropped.load(Ordering::Relaxed);
    if dropped > 0 {
        warn!(dropped, "events dropped while streaming");
    }

    Ok(())
}

fn install_shutdown_handler(shutdown: Arc<AtomicBool>) -> Result<(), RuntimeError> {
    ctrlc::set_handler(move || {
        shutdown.store(true, Ordering::Release);
    })?;
    Ok(())
}

fn build_backend(config: &Config) -> Box<dyn TabletBackend> {
    // Used by the Wintab and mock branches; the Raw Input branch needs no config.
    let _ = &config;

    // Raw Input is the default Windows capture path (SPEC_BGC): focus-independent,
    // so capture continues while another app (a DAW) owns the foreground.
    #[cfg(all(windows, feature = "backend-rawinput"))]
    {
        return Box::new(tablet_rawinput::RawInputBackend::new());
    }

    // Wintab is opt-in fallback only and cannot capture in the background.
    #[cfg(all(windows, feature = "backend-wintab", not(feature = "backend-rawinput")))]
    {
        return Box::new(
            tablet_wintab::WintabBackend::new()
                .with_flip_y(config.capture.flip_y)
                .with_queue_size(config.capture.queue_size as i32),
        );
    }

    #[allow(unreachable_code)]
    Box::new(MockBackend::new(MockConfig {
        sample_count: 100,
        rate_hz: config.capture.requested_rate_hz,
        gap_every: None,
        tablet_button_every: None,
    }))
}

// Real threaded backends (Raw Input / Wintab) capture until shutdown is
// requested, then are stopped.
#[cfg(all(windows, any(feature = "backend-rawinput", feature = "backend-wintab")))]
fn wait_for_backend(
    backend: &mut dyn TabletBackend,
    shutdown: &AtomicBool,
) -> Result<(), BackendError> {
    while !shutdown.load(Ordering::Acquire) {
        thread::sleep(Duration::from_millis(250));
    }
    backend.stop()
}

#[cfg(not(all(windows, any(feature = "backend-rawinput", feature = "backend-wintab"))))]
fn wait_for_backend(
    backend: &mut dyn TabletBackend,
    _shutdown: &AtomicBool,
) -> Result<(), BackendError> {
    backend.stop()
}

fn spawn_streaming_thread(
    config: Config,
    consumer: EventConsumer,
    counters: MetricsCounters,
    shutdown: Arc<AtomicBool>,
) -> JoinHandle<Result<(), RuntimeError>> {
    thread::Builder::new()
        .name("tablet-streaming".to_string())
        .spawn(move || stream_events(config, consumer, counters, shutdown))
        .expect("failed to spawn streaming thread")
}

fn stream_events(
    config: Config,
    consumer: EventConsumer,
    counters: MetricsCounters,
    shutdown: Arc<AtomicBool>,
) -> Result<(), RuntimeError> {
    let mut transport = open_transport(&config)?;
    let mut clients: Vec<FrameWriter<Box<dyn TransportWriter>>> = Vec::new();
    let format = config.output.format.into();
    let metrics_interval = Duration::from_millis(config.telemetry.metrics_interval_ms);
    let mut metrics_emitter =
        MetricsEmitter::new(counters.clone(), metrics_interval, Instant::now());
    let requested_rate_hz = config.capture.requested_rate_hz;
    let mut actual_rate_hz = requested_rate_hz;
    let mut last_sample_serial = None;

    loop {
        while let Some(writer) = transport.next_client() {
            let mut client = FrameWriter::new(writer, format);
            client.write_header()?;
            clients.push(client);
        }

        let mut progressed = false;
        while let Some(event) = pop_event_counted(&consumer, &counters) {
            progressed = true;
            if let SampleEvent::Capabilities(caps) = &event {
                actual_rate_hz = caps.max_packet_rate_hz;
            }
            if let SampleEvent::Sample(sample) = &event {
                counters.increment_packets_processed();
                warn_on_serial_gap(&mut last_sample_serial, sample.serial);
            }
            let message = stream_message_from_event(event);
            write_to_clients(&mut clients, &message)?;
        }

        if metrics_emitter.due(Instant::now()) {
            emit_metrics(
                &mut metrics_emitter,
                &mut clients,
                Instant::now(),
                actual_rate_hz,
                requested_rate_hz,
                transport.connected_clients(),
            )?;
        }

        if shutdown.load(Ordering::Acquire) && !progressed && counters.queue_depth() == 0 {
            break;
        }

        if !progressed {
            thread::sleep(Duration::from_millis(1));
        }
    }

    if !metrics_emitter.has_emitted() {
        emit_metrics(
            &mut metrics_emitter,
            &mut clients,
            Instant::now(),
            actual_rate_hz,
            requested_rate_hz,
            transport.connected_clients(),
        )?;
    }

    debug!("streaming thread stopped");
    Ok(())
}

fn emit_metrics(
    metrics_emitter: &mut MetricsEmitter,
    clients: &mut Vec<FrameWriter<Box<dyn TransportWriter>>>,
    now: Instant,
    actual_rate_hz: u32,
    requested_rate_hz: u32,
    connected_clients: u32,
) -> Result<(), StreamError> {
    let ring_dropped = metrics_emitter.dropped_since_last_emit();
    if ring_dropped > 0 {
        warn!(dropped = ring_dropped, "ring-dropped events detected");
    }

    let metrics =
        metrics_emitter.snapshot(now, actual_rate_hz, requested_rate_hz, connected_clients);
    info!(
        packets_per_sec = metrics.packets_per_sec,
        dropped = metrics.dropped,
        queue_depth = metrics.queue_depth,
        actual_rate_hz = metrics.actual_rate_hz,
        requested_rate_hz = metrics.requested_rate_hz,
        connected_clients = metrics.connected_clients,
        "stream metrics"
    );
    write_to_clients(clients, &StreamMessage::Metrics(metrics))
}

fn open_transport(config: &Config) -> Result<Box<dyn Transport>, RuntimeError> {
    match config.output.transport {
        ConfigTransport::Stdout => Ok(Box::new(StdoutTransport::new())),
        ConfigTransport::Tcp => Ok(Box::new(TcpTransport::bind(&config.output.tcp_addr)?)),
        ConfigTransport::Pipe => open_pipe_transport(&config.output.pipe_name),
    }
}

#[cfg(windows)]
fn open_pipe_transport(pipe_name: &str) -> Result<Box<dyn Transport>, RuntimeError> {
    Ok(Box::new(
        tablet_stream::transport::pipe::NamedPipeTransport::new(pipe_name),
    ))
}

#[cfg(not(windows))]
fn open_pipe_transport(_pipe_name: &str) -> Result<Box<dyn Transport>, RuntimeError> {
    Err(RuntimeError::PipeUnsupported)
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

fn warn_on_serial_gap(last_serial: &mut Option<u32>, serial: u32) {
    if let Some(last) = *last_serial {
        let delta = serial.wrapping_sub(last);
        if delta > 1 {
            warn!(
                previous_serial = last,
                serial,
                gap_size = delta - 1,
                "sample serial gap detected"
            );
        }
    }
    *last_serial = Some(serial);
}

fn write_to_clients(
    clients: &mut Vec<FrameWriter<Box<dyn TransportWriter>>>,
    message: &StreamMessage,
) -> Result<(), StreamError> {
    let mut first_error = None;
    clients.retain_mut(|client| match client.write_message(message) {
        Ok(()) => true,
        Err(error) => {
            if first_error.is_none() {
                first_error = Some(error);
            }
            false
        }
    });

    if let Some(error) = first_error {
        warn!("dropping transport client after write failure: {error}");
    }
    Ok(())
}

fn enqueue_event(
    producer: &mut Producer<SampleEvent>,
    consumer: &EventConsumer,
    counters: &MetricsCounters,
    event: SampleEvent,
) {
    match producer.push(event) {
        Ok(()) => {
            counters.increment_queue_depth();
        }
        Err(rtrb::PushError::Full(event)) => {
            counters.dropped.fetch_add(1, Ordering::Relaxed);
            if let Ok(mut consumer) = consumer.try_lock() {
                if consumer.pop().is_ok() {
                    counters.decrement_queue_depth();
                }
            }
            if producer.push(event).is_ok() {
                counters.increment_queue_depth();
            } else {
                counters.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

fn pop_event(consumer: &EventConsumer) -> Option<SampleEvent> {
    consumer.lock().ok()?.pop().ok()
}

fn pop_event_counted(consumer: &EventConsumer, counters: &MetricsCounters) -> Option<SampleEvent> {
    let event = pop_event(consumer)?;
    counters.decrement_queue_depth();
    Some(event)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tablet_core::{AxisInfo, AxisUnit, DeviceCapabilities};

    fn capabilities_event(name: &str) -> SampleEvent {
        let unsupported = AxisInfo {
            min: 0,
            max: 0,
            resolution: 0.0,
            unit: AxisUnit::None,
            supported: false,
        };
        SampleEvent::Capabilities(DeviceCapabilities {
            device_name: name.to_string(),
            driver_version: "test".to_string(),
            x: unsupported.clone(),
            y: unsupported.clone(),
            z: unsupported.clone(),
            pressure: unsupported.clone(),
            tangent_pressure: unsupported.clone(),
            azimuth: unsupported.clone(),
            altitude: unsupported.clone(),
            twist: unsupported.clone(),
            rotation: unsupported,
            max_packet_rate_hz: 200,
            queue_size: 2,
        })
    }

    #[test]
    fn full_queue_discards_oldest_when_consumer_is_available() {
        let (mut producer, consumer) = RingBuffer::<SampleEvent>::new(2);
        let consumer = Arc::new(Mutex::new(consumer));
        let dropped = Arc::new(AtomicU64::new(0));
        let counters = MetricsCounters::new(Arc::clone(&dropped));

        enqueue_event(
            &mut producer,
            &consumer,
            &counters,
            capabilities_event("one"),
        );
        enqueue_event(
            &mut producer,
            &consumer,
            &counters,
            capabilities_event("two"),
        );
        enqueue_event(
            &mut producer,
            &consumer,
            &counters,
            capabilities_event("three"),
        );

        assert_eq!(dropped.load(Ordering::Relaxed), 1);
        assert_eq!(counters.queue_depth(), 2);
        let first = pop_event(&consumer).expect("second event remains");
        let second = pop_event(&consumer).expect("third event remains");

        match first {
            SampleEvent::Capabilities(caps) => assert_eq!(caps.device_name, "two"),
            _ => panic!("unexpected event"),
        }
        match second {
            SampleEvent::Capabilities(caps) => assert_eq!(caps.device_name, "three"),
            _ => panic!("unexpected event"),
        }
    }
}
