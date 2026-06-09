use std::{
    collections::VecDeque,
    io::{self, BufReader, Read},
    net::TcpStream,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use tablet_core::{DeviceCapabilities, PenSample};
use tablet_stream::{Format, FrameReader, Metrics, StreamError, StreamMessage};

use crate::spawn::SpawnedProducer;

/// Where a consumer reads the WCAP stream from.
///
/// `Tcp`/`Pipe` reconnect on EOF/error with exponential backoff; `Stdin`
/// treats EOF as terminal (no reconnect) — see [`run_source`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Stdin,
    Tcp(String),
    Pipe(String),
}

const DEFAULT_DISPLAY_CAPACITY: usize = 4096;

/// Initial backoff before the first reconnect attempt (SPEC_CUI.md §5.3).
const INITIAL_BACKOFF: Duration = Duration::from_millis(250);
/// Backoff is doubled after each failed attempt, capped here.
const MAX_BACKOFF: Duration = Duration::from_secs(3);

/// Compute the next backoff duration: doubles the previous value, capped at
/// [`MAX_BACKOFF`]. Pure and deterministic so it is unit-testable without
/// sleeping (TC3.5 step 6).
fn next_backoff(previous: Duration) -> Duration {
    let doubled = previous.saturating_mul(2);
    if doubled > MAX_BACKOFF {
        MAX_BACKOFF
    } else {
        doubled
    }
}

/// Sleep for `duration`, but wake early (in small slices) to check `stop` so
/// shutdown is prompt even mid-backoff. Keeps the no-spin guarantee: each
/// slice still sleeps a meaningful amount rather than busy-looping.
fn interruptible_sleep(duration: Duration, stop: &AtomicBool) {
    const SLICE: Duration = Duration::from_millis(50);
    let mut remaining = duration;
    while remaining > Duration::ZERO {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let slice = remaining.min(SLICE);
        thread::sleep(slice);
        remaining -= slice;
    }
}

/// Whether a [`Source`] reconnects on EOF/error (`Tcp`/`Pipe`, SPEC_CUI.md
/// §5.3) or treats EOF as terminal (`Stdin`, and the `--spawn` child — which
/// is not a `Source` variant but follows the same stdin-like contract).
fn reconnects(source: &Source) -> bool {
    matches!(source, Source::Tcp(_) | Source::Pipe(_))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionStatus {
    Connecting,
    Connected,
    Disconnected,
}

#[derive(Clone, Debug)]
pub struct SourceState {
    pub status: ConnectionStatus,
    pub latest_capabilities: Option<DeviceCapabilities>,
    pub latest_metrics: Option<Metrics>,
    pub display_dropped: u64,
    pub serial_gaps: u64,
}

impl Default for SourceState {
    fn default() -> Self {
        Self {
            status: ConnectionStatus::Connecting,
            latest_capabilities: None,
            latest_metrics: None,
            display_dropped: 0,
            serial_gaps: 0,
        }
    }
}

#[derive(Clone)]
pub struct SharedSource {
    inner: Arc<Mutex<SharedSourceInner>>,
    capacity: usize,
}

struct SharedSourceInner {
    state: SourceState,
    messages: VecDeque<StreamMessage>,
    /// Sticky flag: set whenever `status` transitions to `Connected`. Used by
    /// the reconnect loop to decide whether to reset the backoff after an
    /// attempt — a connection that was established (however briefly) before
    /// dropping should not be penalized with an ever-growing delay
    /// (SPEC_CUI.md §5.3). Cleared via [`SharedSource::take_connected_flag`]
    /// at the start of each attempt.
    connected_since_check: bool,
}

impl SharedSource {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(SharedSourceInner {
                state: SourceState::default(),
                messages: VecDeque::with_capacity(capacity),
                connected_since_check: false,
            })),
            capacity,
        }
    }

    pub fn state(&self) -> SourceState {
        self.inner
            .lock()
            .expect("source mutex poisoned")
            .state
            .clone()
    }

    pub fn set_status(&self, status: ConnectionStatus) {
        let mut inner = self.inner.lock().expect("source mutex poisoned");
        if status == ConnectionStatus::Connected {
            inner.connected_since_check = true;
        }
        inner.state.status = status;
    }

    /// Read-and-clear the sticky "connected since last check" flag (see
    /// [`SharedSourceInner::connected_since_check`]). The reconnect loop calls
    /// this once per attempt to decide whether to reset the backoff.
    fn take_connected_flag(&self) -> bool {
        let mut inner = self.inner.lock().expect("source mutex poisoned");
        std::mem::take(&mut inner.connected_since_check)
    }

    pub fn drain_messages(&self) -> Vec<StreamMessage> {
        self.inner
            .lock()
            .expect("source mutex poisoned")
            .messages
            .drain(..)
            .collect()
    }
}

#[derive(Debug, Default)]
pub struct IngestState {
    last_serial: Option<u32>,
}

/// Owns the reader thread and the means to stop it cleanly.
///
/// Dropping the handle (or calling [`SourceHandle::stop`] explicitly, e.g.
/// from the app's `on_exit` hook) sets the shared `stop` flag; the reader
/// thread observes it between retries/backoff sleeps and exits promptly
/// (SPEC_CUI.md §5.3, TC3.5 step 6). If a producer was spawned via
/// `--spawn`, it is killed and reaped here too so no orphan process remains.
pub struct SourceHandle {
    shared: SharedSource,
    stop: Arc<AtomicBool>,
    spawned: Option<Arc<Mutex<SpawnedProducer>>>,
    _thread: JoinHandle<()>,
}

impl SourceHandle {
    pub fn spawn(source: Source, format: Format) -> Self {
        Self::spawn_with_capacity(source, format, DEFAULT_DISPLAY_CAPACITY)
    }

    pub fn spawn_with_capacity(source: Source, format: Format, capacity: usize) -> Self {
        let shared = SharedSource::new(capacity);
        let stop = Arc::new(AtomicBool::new(false));
        let thread_shared = shared.clone();
        let thread_stop = Arc::clone(&stop);
        let thread = thread::spawn(move || run_source(source, format, thread_shared, thread_stop));

        Self {
            shared,
            stop,
            spawned: None,
            _thread: thread,
        }
    }

    /// Spawn `tablet-cli` as a child producer (`--spawn`, SPEC_CUI.md §5.2)
    /// and feed its stdout into the reader path as a postcard stream — the
    /// format is forced to postcard regardless of `--format` (TC3.5 Part A).
    /// EOF on the child's stdout is terminal, mirroring `Stdin` (no
    /// reconnect): once the producer exits there is nothing to reconnect to.
    pub fn spawn_child_producer(producer: SpawnedProducer) -> io::Result<Self> {
        Self::spawn_child_producer_with_capacity(producer, DEFAULT_DISPLAY_CAPACITY)
    }

    pub fn spawn_child_producer_with_capacity(
        mut producer: SpawnedProducer,
        capacity: usize,
    ) -> io::Result<Self> {
        let stdout = producer
            .take_stdout()
            .ok_or_else(|| io::Error::other("spawned tablet-cli produced no stdout pipe"))?;

        let shared = SharedSource::new(capacity);
        let stop = Arc::new(AtomicBool::new(false));
        let spawned = Arc::new(Mutex::new(producer));

        let thread_shared = shared.clone();
        let thread_stop = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            thread_shared.set_status(ConnectionStatus::Connecting);
            // Spawned-child EOF is terminal (no reconnect), exactly like
            // `Stdin` (SPEC_CUI.md §5.3, TC3.5 Part B). Format is forced to
            // postcard — that's what `spawn_producer` launches `tablet-cli` with.
            let _ = ingest_reader(stdout, Format::Postcard, thread_shared.clone());
            thread_shared.set_status(ConnectionStatus::Disconnected);
            let _ = &thread_stop; // no retries for a terminal source
        });

        Ok(Self {
            shared,
            stop,
            spawned: Some(spawned),
            _thread: thread,
        })
    }

    pub fn state(&self) -> SourceState {
        self.shared.state()
    }

    pub fn drain_messages(&self) -> Vec<StreamMessage> {
        self.shared.drain_messages()
    }

    /// Signal the reader thread to stop (it checks this flag between retries
    /// and backoff sleeps — see [`interruptible_sleep`]) and, if a producer
    /// was spawned, kill and reap it so no orphan process remains. Idempotent;
    /// safe to call from `on_exit` and to rely on via `Drop`.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(spawned) = &self.spawned {
            if let Ok(mut producer) = spawned.lock() {
                producer.kill_and_wait();
            }
        }
    }
}

impl Drop for SourceHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Reader-thread entry point: connects (or reconnects, for `Tcp`/`Pipe`) and
/// ingests until either the source is terminal (EOF on `Stdin`) or `stop` is
/// signalled. Connection-state transitions (`Connecting`/`Connected`/
/// `Disconnected`) are surfaced through `shared` at every step (SPEC_CUI.md §5.3).
fn run_source(source: Source, format: Format, shared: SharedSource, stop: Arc<AtomicBool>) {
    let retry = reconnects(&source);
    let mut backoff = INITIAL_BACKOFF;

    loop {
        if stop.load(Ordering::Relaxed) {
            shared.set_status(ConnectionStatus::Disconnected);
            return;
        }

        shared.set_status(ConnectionStatus::Connecting);
        let _ = shared.take_connected_flag(); // clear any stale flag before this attempt

        let result = match open_source(source.clone()) {
            Ok(reader) => ingest_reader(reader, format, shared.clone()).map(|_| ()),
            Err(error) => Err(error),
        };

        let did_connect = shared.take_connected_flag();
        shared.set_status(ConnectionStatus::Disconnected);

        if !retry || stop.load(Ordering::Relaxed) {
            // Terminal: `Stdin` (and anything else that doesn't reconnect)
            // stops here regardless of success/failure — EOF is the end.
            let _ = result;
            return;
        }

        // A connection that was actually established resets the backoff —
        // only a string of *failed* attempts should grow the delay
        // (SPEC_CUI.md §5.3).
        backoff = if did_connect { INITIAL_BACKOFF } else { backoff };

        // Tcp/Pipe: wait with exponential backoff (capped), then retry —
        // checking `stop` so we never spin and shut down promptly.
        interruptible_sleep(backoff, &stop);
        backoff = next_backoff(backoff);
    }
}

fn open_source(source: Source) -> Result<Box<dyn Read + Send>, StreamError> {
    match source {
        Source::Stdin => Ok(Box::new(io::stdin())),
        Source::Tcp(addr) => Ok(Box::new(TcpStream::connect(addr)?)),
        Source::Pipe(name) => open_pipe(name),
    }
}

#[cfg(windows)]
fn open_pipe(name: String) -> Result<Box<dyn Read + Send>, StreamError> {
    let path = format!(r"\\.\pipe\{name}");
    Ok(Box::new(std::fs::OpenOptions::new().read(true).open(path)?))
}

#[cfg(not(windows))]
fn open_pipe(name: String) -> Result<Box<dyn Read + Send>, StreamError> {
    Err(StreamError::Io(io::Error::new(
        io::ErrorKind::Unsupported,
        format!("named pipes are only supported on Windows: {name}"),
    )))
}

pub fn ingest_reader<R: Read>(
    reader: R,
    format: Format,
    shared: SharedSource,
) -> Result<SourceState, StreamError> {
    let result = match format {
        Format::Postcard => {
            let mut reader = FrameReader::new_from_header(reader)?;
            shared.set_status(ConnectionStatus::Connected);
            ingest_loop(&mut reader, &shared)
        }
        Format::Json => {
            let mut reader = FrameReader::new_json(BufReader::new(reader));
            shared.set_status(ConnectionStatus::Connected);
            ingest_loop(&mut reader, &shared)
        }
    };

    shared.set_status(ConnectionStatus::Disconnected);

    match result {
        Ok(()) | Err(StreamError::TruncatedFrame) => Ok(shared.state()),
        Err(error) => Err(error),
    }
}

fn ingest_loop<R: Read>(
    reader: &mut FrameReader<R>,
    shared: &SharedSource,
) -> Result<(), StreamError> {
    let mut ingest_state = IngestState::default();

    loop {
        let message = reader.read_message()?;
        handle_message(message, &mut ingest_state, shared);
    }
}

pub fn handle_message(
    message: StreamMessage,
    ingest_state: &mut IngestState,
    shared: &SharedSource,
) {
    match &message {
        StreamMessage::Capabilities(caps) => {
            shared
                .inner
                .lock()
                .expect("source mutex poisoned")
                .state
                .latest_capabilities = Some(caps.clone());
        }
        StreamMessage::Metrics(metrics) => {
            shared
                .inner
                .lock()
                .expect("source mutex poisoned")
                .state
                .latest_metrics = Some(metrics.clone());
        }
        StreamMessage::Sample(sample) => record_serial_gap(*sample, ingest_state, shared),
        StreamMessage::Proximity { .. } | StreamMessage::Heartbeat => {}
    }

    push_message(shared, message);
}

fn record_serial_gap(sample: PenSample, ingest_state: &mut IngestState, shared: &SharedSource) {
    if let Some(previous) = ingest_state.last_serial {
        let jump = sample.serial.saturating_sub(previous);
        if jump > 1 {
            let missing = u64::from(jump - 1);
            shared
                .inner
                .lock()
                .expect("source mutex poisoned")
                .state
                .serial_gaps += missing;
        }
    }
    ingest_state.last_serial = Some(sample.serial);
}

fn push_message(shared: &SharedSource, message: StreamMessage) {
    let mut inner = shared.inner.lock().expect("source mutex poisoned");
    if inner.messages.len() == shared.capacity {
        inner.messages.pop_front();
        inner.state.display_dropped += 1;
    }
    inner.messages.push_back(message);
}

#[cfg(test)]
mod reconnect_tests {
    use super::*;

    #[test]
    fn next_backoff_doubles_from_initial() {
        assert_eq!(next_backoff(INITIAL_BACKOFF), Duration::from_millis(500));
        assert_eq!(next_backoff(Duration::from_millis(500)), Duration::from_secs(1));
        assert_eq!(next_backoff(Duration::from_secs(1)), Duration::from_secs(2));
    }

    #[test]
    fn next_backoff_caps_at_max() {
        assert_eq!(next_backoff(Duration::from_secs(2)), MAX_BACKOFF);
        assert_eq!(next_backoff(MAX_BACKOFF), MAX_BACKOFF);
        assert_eq!(next_backoff(Duration::from_secs(100)), MAX_BACKOFF);
    }

    #[test]
    fn next_backoff_never_exceeds_cap_from_any_start() {
        let mut backoff = INITIAL_BACKOFF;
        for _ in 0..20 {
            backoff = next_backoff(backoff);
            assert!(backoff <= MAX_BACKOFF);
        }
        assert_eq!(backoff, MAX_BACKOFF);
    }

    #[test]
    fn tcp_and_pipe_sources_reconnect_but_stdin_does_not() {
        assert!(reconnects(&Source::Tcp("127.0.0.1:9123".to_owned())));
        assert!(reconnects(&Source::Pipe("wacom-capture".to_owned())));
        assert!(!reconnects(&Source::Stdin));
    }

    #[test]
    fn interruptible_sleep_returns_promptly_when_stop_is_set() {
        let stop = AtomicBool::new(true);
        let started = std::time::Instant::now();
        interruptible_sleep(Duration::from_secs(10), &stop);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "sleep should have been interrupted almost immediately"
        );
    }

    #[test]
    fn shared_source_connected_flag_is_sticky_until_taken() {
        let shared = SharedSource::new(8);
        assert!(!shared.take_connected_flag());

        shared.set_status(ConnectionStatus::Connecting);
        assert!(!shared.take_connected_flag());

        shared.set_status(ConnectionStatus::Connected);
        shared.set_status(ConnectionStatus::Disconnected);
        // The flag survives the transition back to Disconnected...
        assert!(shared.take_connected_flag());
        // ...and is cleared once taken.
        assert!(!shared.take_connected_flag());
    }
}
