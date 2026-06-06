// tablet-core: platform-agnostic types and traits
// No OS-specific dependencies allowed in this crate.

pub mod backend;
pub mod capabilities;
pub mod error;
pub mod normalize;
pub mod sample;

pub use backend::{SampleEvent, TabletBackend};
pub use capabilities::{AxisInfo, AxisUnit, DeviceCapabilities};
pub use error::BackendError;
pub use normalize::{normalize, tilt_from_orientation};
pub use sample::{PenSample, ToolKind};
