use thiserror::Error;

/// Errors that a `TabletBackend` implementation can surface.
#[derive(Debug, Error)]
pub enum BackendError {
    /// The Wintab32.dll driver was not found or the interface is not live.
    ///
    /// Install the Wacom tablet driver from https://www.wacom.com/en-us/support/product-support/drivers
    /// and retry.
    #[error(
        "Wacom Wintab driver not found or not live. \
        Please install the Wacom tablet driver from https://www.wacom.com/en-us/support/product-support/drivers"
    )]
    DriverMissing,

    /// No tablet device was enumerated by the driver.
    #[error("No tablet device found. Ensure your Wacom tablet is connected and powered on.")]
    NoDevice,

    /// WTOpen returned a null context handle.
    #[error(
        "Failed to open a Wintab capture context (WTOpen returned null). \
        The driver may be in use by another application."
    )]
    ContextOpenFailed,

    /// A requested axis or field is not available on this device/driver.
    #[error("The requested axis or field '{field}' is not supported by this device.")]
    UnsupportedField {
        /// The name of the unsupported field/axis.
        field: String,
    },

    /// A backend exists behind a feature gate, but its implementation is not ready yet.
    #[error("The {backend} backend is not implemented yet.")]
    NotImplemented {
        /// The backend name, such as "evdev" or "macOS".
        backend: &'static str,
    },

    /// A transport-level I/O failure occurred.
    #[error("Transport I/O error: {0}")]
    Transport(String),
}
