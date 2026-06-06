//! Mock `TabletBackend` for testing without hardware.
//!
//! `MockBackend` synthesizes a deterministic stream of `SampleEvent`s that
//! can be used to drive end-to-end transport/framing tests on any OS.  It
//! intentionally contains **no `cfg(windows)`** so it compiles everywhere.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use tablet_core::{
    normalize, AxisInfo, AxisUnit, BackendError, DeviceCapabilities, PenSample, SampleEvent,
    TabletBackend, ToolKind,
};

// ---------------------------------------------------------------------------
// MockConfig
// ---------------------------------------------------------------------------

/// Configuration for `MockBackend`.
#[derive(Debug, Clone)]
pub struct MockConfig {
    /// Total samples to emit before the thread exits naturally.
    /// `0` means run until `stop()` is called.
    pub sample_count: u32,
    /// Nominal delay between samples in Hz (samples per second).
    pub rate_hz: u32,
    /// If `Some(n)`, increment the serial by 2 every `n`th sample to simulate
    /// a packet-drop / serial discontinuity.
    pub gap_every: Option<u32>,
}

impl Default for MockConfig {
    fn default() -> Self {
        Self {
            sample_count: 100,
            rate_hz: 200,
            gap_every: None,
        }
    }
}

// ---------------------------------------------------------------------------
// MockBackend
// ---------------------------------------------------------------------------

/// A `TabletBackend` that emits a synthetic, deterministic stream of samples.
///
/// Useful for unit/integration tests without a physical Wacom tablet.
pub struct MockBackend {
    config: MockConfig,
    stop_flag: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl MockBackend {
    /// Create a new `MockBackend` with the given configuration.
    pub fn new(config: MockConfig) -> Self {
        Self {
            config,
            stop_flag: Arc::new(AtomicBool::new(false)),
            handle: None,
        }
    }

    /// Build the fixed `DeviceCapabilities` the mock advertises.
    fn mock_caps() -> DeviceCapabilities {
        let unsupported = AxisInfo {
            min: 0,
            max: 0,
            resolution: 0.0,
            unit: AxisUnit::None,
            supported: false,
        };
        DeviceCapabilities {
            device_name: "Mock Tablet".to_string(),
            driver_version: "0.0.0".to_string(),
            x: AxisInfo {
                min: 0,
                max: 60960,
                resolution: 100.0,
                unit: AxisUnit::Inch,
                supported: true,
            },
            y: AxisInfo {
                min: 0,
                max: 60960,
                resolution: 100.0,
                unit: AxisUnit::Inch,
                supported: true,
            },
            z: unsupported,
            pressure: AxisInfo {
                min: 0,
                max: 1023,
                resolution: 1.0,
                unit: AxisUnit::None,
                supported: true,
            },
            tangent_pressure: unsupported,
            azimuth: unsupported,
            altitude: unsupported,
            twist: unsupported,
            rotation: unsupported,
            max_packet_rate_hz: 200,
            queue_size: 1024,
        }
    }
}

impl TabletBackend for MockBackend {
    fn capabilities(&self) -> Result<DeviceCapabilities, BackendError> {
        Ok(Self::mock_caps())
    }

    /// Spawn the capture thread and begin emitting `SampleEvent`s.
    ///
    /// The first event is always `SampleEvent::Capabilities`, followed by
    /// `SampleEvent::Sample` messages with monotonically increasing serials and
    /// timestamps.  When `MockConfig::gap_every = Some(n)`, every nth sample
    /// skips a serial number to simulate a hardware drop.
    fn start(&mut self, mut sink: Box<dyn FnMut(SampleEvent) + Send>) -> Result<(), BackendError> {
        let config = self.config.clone();
        let stop_flag = Arc::clone(&self.stop_flag);
        self.stop_flag.store(false, Ordering::SeqCst);

        let caps = Self::mock_caps();
        let sleep_us = if config.rate_hz > 0 {
            1_000_000 / config.rate_hz as u64
        } else {
            5_000
        };

        let handle = thread::spawn(move || {
            // Emit capabilities first.
            sink(SampleEvent::Capabilities(caps.clone()));

            let max_x = caps.x.max as i32;
            let max_y = caps.y.max as i32;
            let max_pressure = caps.pressure.max as u32;
            let pressure_min = caps.pressure.min;
            let pressure_max = caps.pressure.max;
            let x_min = caps.x.min;
            let x_max = caps.x.max;
            let y_min = caps.y.min;
            let y_max = caps.y.max;

            let mut serial: u32 = 0;
            let mut t_ns: u64 = 0;
            let mut idx: u32 = 0;

            loop {
                // When sample_count is 0, run indefinitely until stop().
                // When sample_count > 0, run exactly that many samples regardless
                // of the stop flag — stop() will join after natural completion.
                if config.sample_count > 0 {
                    if idx >= config.sample_count {
                        break;
                    }
                } else if stop_flag.load(Ordering::Relaxed) {
                    break;
                }

                // Advance serial (by 2 on gap samples to simulate drop).
                let gap = config
                    .gap_every
                    .map(|n| n > 0 && idx > 0 && idx % n == 0)
                    .unwrap_or(false);
                serial = serial.wrapping_add(if gap { 2 } else { 1 });
                t_ns = t_ns.wrapping_add(5_000_000);

                // Parametric stroke: x follows a half-sine wave; y is linear.
                let phase =
                    (idx as f64) / (config.sample_count.max(1) as f64) * std::f64::consts::PI;
                let x_raw = ((phase.sin() * max_x as f64) as i32).clamp(0, max_x);
                let y_raw =
                    ((idx as f64 / config.sample_count.max(1) as f64) * max_y as f64) as i32;

                // Pressure ramps up then down.
                let p_norm = if phase <= std::f64::consts::FRAC_PI_2 {
                    phase / std::f64::consts::FRAC_PI_2
                } else {
                    (std::f64::consts::PI - phase) / std::f64::consts::FRAC_PI_2
                };
                let pressure_raw = (p_norm * max_pressure as f64) as u32;

                let sample = PenSample {
                    t_capture_ns: t_ns,
                    t_device_ms: idx,
                    serial,
                    x_raw,
                    y_raw,
                    z_raw: 0,
                    x_norm: normalize(x_raw as i64, x_min, x_max),
                    y_norm: normalize(y_raw as i64, y_min, y_max),
                    pressure_raw,
                    pressure_norm: normalize(pressure_raw as i64, pressure_min, pressure_max),
                    tangent_pressure_raw: None,
                    azimuth_deci_deg: None,
                    altitude_deci_deg: None,
                    twist_deci_deg: None,
                    tilt_x_deg: None,
                    tilt_y_deg: None,
                    rotation_deci_deg: None,
                    buttons: 0,
                    tool: ToolKind::Pen,
                    tool_serial: 1,
                    in_proximity: true,
                    status: 0,
                };

                sink(SampleEvent::Sample(sample));

                idx += 1;
                thread::sleep(Duration::from_micros(sleep_us));
            }
        });

        self.handle = Some(handle);
        Ok(())
    }

    /// Signal the capture thread to stop and wait for it to join.
    fn stop(&mut self) -> Result<(), BackendError> {
        self.stop_flag.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn mock_emits_capabilities_then_samples() {
        let config = MockConfig {
            sample_count: 10,
            rate_hz: 10_000, // fast for tests
            gap_every: None,
        };
        let mut backend = MockBackend::new(config);

        let events: Arc<Mutex<Vec<SampleEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let events_clone = Arc::clone(&events);

        backend
            .start(Box::new(move |ev| {
                events_clone.lock().unwrap().push(ev);
            }))
            .unwrap();

        backend.stop().unwrap();

        let collected = events.lock().unwrap();
        assert!(
            !collected.is_empty(),
            "should have collected at least one event"
        );
        assert!(
            matches!(collected[0], SampleEvent::Capabilities(_)),
            "first event must be Capabilities"
        );

        // All remaining events should be Samples.
        for ev in collected.iter().skip(1) {
            assert!(
                matches!(ev, SampleEvent::Sample(_)),
                "expected Sample, got something else"
            );
        }

        // Extract samples and verify increasing serials.
        let samples: Vec<u32> = collected
            .iter()
            .skip(1)
            .filter_map(|ev| {
                if let SampleEvent::Sample(s) = ev {
                    Some(s.serial)
                } else {
                    None
                }
            })
            .collect();

        assert_eq!(samples.len(), 10, "expected 10 samples");
        for window in samples.windows(2) {
            assert!(
                window[1] > window[0],
                "serials must be strictly increasing; got {} then {}",
                window[0],
                window[1]
            );
        }
    }

    #[test]
    fn mock_gap_injection_creates_discontinuities() {
        let config = MockConfig {
            sample_count: 10,
            rate_hz: 10_000,
            gap_every: Some(3), // inject a gap every 3rd sample (idx 3, 6, 9)
        };
        let mut backend = MockBackend::new(config);

        let events: Arc<Mutex<Vec<SampleEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let events_clone = Arc::clone(&events);

        backend
            .start(Box::new(move |ev| {
                events_clone.lock().unwrap().push(ev);
            }))
            .unwrap();

        backend.stop().unwrap();

        let collected = events.lock().unwrap();
        let serials: Vec<u32> = collected
            .iter()
            .skip(1)
            .filter_map(|ev| {
                if let SampleEvent::Sample(s) = ev {
                    Some(s.serial)
                } else {
                    None
                }
            })
            .collect();

        // There must be at least one gap (serial diff > 1).
        let has_gap = serials.windows(2).any(|w| w[1].wrapping_sub(w[0]) > 1);
        assert!(
            has_gap,
            "gap injection must produce at least one serial discontinuity; serials: {serials:?}"
        );
    }

    #[test]
    fn mock_capabilities_returns_correct_device_name() {
        let backend = MockBackend::new(MockConfig::default());
        let caps = backend.capabilities().unwrap();
        assert_eq!(caps.device_name, "Mock Tablet");
        assert_eq!(caps.driver_version, "0.0.0");
        assert!(caps.x.supported);
        assert!(caps.y.supported);
        assert!(caps.pressure.supported);
    }
}
