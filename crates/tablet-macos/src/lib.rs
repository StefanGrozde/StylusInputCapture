// tablet-macos: future macOS tablet backend stub.
//
// On non-macOS targets this crate compiles to an empty library.

#[cfg(all(target_os = "macos", feature = "backend-macos"))]
use tablet_core::{BackendError, DeviceCapabilities, SampleEvent, TabletBackend};

/// macOS NSEvent tablet backend placeholder.
///
/// This validates the `TabletBackend` trait shape behind the `backend-macos`
/// feature without touching real devices.
#[cfg(all(target_os = "macos", feature = "backend-macos"))]
#[derive(Debug, Default)]
pub struct MacosBackend;

#[cfg(all(target_os = "macos", feature = "backend-macos"))]
impl MacosBackend {
    pub fn new() -> Self {
        Self
    }
}

#[cfg(all(target_os = "macos", feature = "backend-macos"))]
impl TabletBackend for MacosBackend {
    fn capabilities(&self) -> Result<DeviceCapabilities, BackendError> {
        Err(BackendError::NotImplemented { backend: "macOS" })
    }

    fn start(&mut self, _sink: Box<dyn FnMut(SampleEvent) + Send>) -> Result<(), BackendError> {
        Err(BackendError::NotImplemented { backend: "macOS" })
    }

    fn stop(&mut self) -> Result<(), BackendError> {
        Err(BackendError::NotImplemented { backend: "macOS" })
    }
}
