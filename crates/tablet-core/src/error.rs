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

    /// No tablet device was enumerated.
    ///
    /// Under the Raw Input backend this also fires when no HID **digitizer**
    /// collection is present — most often because "Use Windows Ink" is turned
    /// off in Wacom Tablet Properties (SPEC_BGC §9).
    #[error(
        "No tablet device found. Ensure your Wacom tablet is connected and powered on, \
        and that \"Use Windows Ink\" is enabled in Wacom Tablet Properties \
        (required for the pen to present as a standard HID digitizer)."
    )]
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

    /// `RegisterRawInputDevices` failed (Raw Input backend, SPEC_BGC §4).
    ///
    /// Carries the underlying Win32 error code for diagnostics.
    #[error("Failed to register for Raw Input (Win32 error {win32_error}).")]
    RawInputRegistrationFailed {
        /// The `GetLastError` value captured at the failure site.
        win32_error: u32,
    },

    /// A transport-level I/O failure occurred.
    #[error("Transport I/O error: {0}")]
    Transport(String),
}
