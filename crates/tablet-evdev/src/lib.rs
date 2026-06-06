// tablet-evdev: future Linux evdev/libinput backend stub.
//
// On non-Linux targets this crate compiles to an empty library.

#[cfg(all(target_os = "linux", feature = "backend-evdev"))]
use tablet_core::{BackendError, DeviceCapabilities, SampleEvent, TabletBackend};

/// Linux evdev/libinput backend placeholder.
///
/// This validates the `TabletBackend` trait shape behind the `backend-evdev`
/// feature without touching real devices.
#[cfg(all(target_os = "linux", feature = "backend-evdev"))]
#[derive(Debug, Default)]
pub struct EvdevBackend;

#[cfg(all(target_os = "linux", feature = "backend-evdev"))]
impl EvdevBackend {
    pub fn new() -> Self {
        Self
    }
}

#[cfg(all(target_os = "linux", feature = "backend-evdev"))]
impl TabletBackend for EvdevBackend {
    fn capabilities(&self) -> Result<DeviceCapabilities, BackendError> {
        Err(BackendError::NotImplemented { backend: "evdev" })
    }

    fn start(&mut self, _sink: Box<dyn FnMut(SampleEvent) + Send>) -> Result<(), BackendError> {
        Err(BackendError::NotImplemented { backend: "evdev" })
    }

    fn stop(&mut self) -> Result<(), BackendError> {
        Err(BackendError::NotImplemented { backend: "evdev" })
    }
}
