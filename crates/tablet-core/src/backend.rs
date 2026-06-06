use crate::{BackendError, DeviceCapabilities, PenSample};

/// An event emitted by a `TabletBackend` via the sink callback.
///
/// The stream layer serializes the inner payload types (`PenSample`,
/// `DeviceCapabilities`) separately; `SampleEvent` itself is not serialized.
#[derive(Debug, Clone)]
pub enum SampleEvent {
    /// Full device capability descriptor — emitted once at session start and
    /// whenever `WT_INFOCHANGE` signals a driver/device state change.
    Capabilities(DeviceCapabilities),

    /// A single decoded pen packet.
    Sample(PenSample),

    /// Proximity change: the tool entered or left the sensing area.
    Proximity {
        /// `true` if the tool is now in range; `false` if it left.
        in_range: bool,
        /// Physical pen serial number associated with the event.
        tool_serial: u64,
    },
}

/// The single abstraction every platform backend implements.
///
/// All backends must be `Send` so they can be moved to a background thread.
/// The `start` sink is invoked on the capture thread and must be cheap
/// (e.g. a ring-buffer push) — no blocking I/O inside the sink.
pub trait TabletBackend: Send {
    /// Enumerate connected devices and return their capability descriptor.
    ///
    /// This is the source for the `Capabilities` handshake frame.
    fn capabilities(&self) -> Result<DeviceCapabilities, BackendError>;

    /// Begin capture.
    ///
    /// Decoded samples are delivered via the `sink` callback, which is invoked
    /// on the capture thread. The sink must be cheap (e.g. a ring push).
    fn start(&mut self, sink: Box<dyn FnMut(SampleEvent) + Send>) -> Result<(), BackendError>;

    /// Stop capture and release all OS resources (context, window, thread).
    fn stop(&mut self) -> Result<(), BackendError>;
}
