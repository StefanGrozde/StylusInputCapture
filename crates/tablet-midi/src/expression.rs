//! Expression snapshots and HSV trail-color mapping for HUD visualization.
//!
//! Pure data + math — no egui or MIDI I/O. The HUD captures an
//! [`ExpressionSnapshot`] after each [`MpeEngine::process`](crate::MpeEngine::process)
//! call and maps it to RGB via [`TrailColorScheme`].

use serde::{Deserialize, Serialize};

use tablet_process::ProcessedSample;

use crate::engine::MpeEngine;
use crate::event::PITCH_BEND_CENTER;
use crate::mapping::MidiMapping;

/// Normalized MPE expression axes at one sample instant.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ExpressionSnapshot {
    /// Pitch bend in `[-1, 1]` (0 = center).
    pub bend_norm: f32,
    /// CC74 timbre in `[0, 1]`.
    pub cc74_norm: f32,
    /// Channel pressure in `[0, 1]`.
    pub pressure_norm: f32,
    /// Optional tilt CC in `[0, 1]` (center 64 → 0.5).
    pub tilt_norm: Option<f32>,
}

impl ExpressionSnapshot {
    /// Capture expression for trail color from pen position relative to the
    /// note strike. Hue uses vertical displacement in normalized surface space
    /// (full height → ±1), not the MIDI pitch-bend value — the latter is
    /// scaled by the 48-st MPE range so small `y_bend_semitones` moves barely
    /// shift hue when read from `last_bend()`.
    pub fn from_performance(
        engine: &MpeEngine,
        sample: &ProcessedSample,
        mapping: &MidiMapping,
    ) -> Option<Self> {
        let (x_origin, y_origin) = engine.strike_origin()?;
        let bend_norm = (y_origin - sample.y).clamp(-1.0, 1.0) as f32;
        let cc74_norm = if mapping.x_to_cc74 {
            (0.5 + (sample.x - x_origin)).clamp(0.0, 1.0) as f32
        } else {
            engine
                .last_cc74()
                .map(seven_bit_frac)
                .unwrap_or(0.5)
        };
        let pressure_norm = if mapping.pressure_to_channel_pressure {
            sample.pressure.clamp(0.0, 1.0) as f32
        } else {
            engine
                .last_pressure()
                .map(seven_bit_frac)
                .unwrap_or(0.0)
        };
        let tilt_norm = engine.last_tilt_cc().map(seven_bit_frac);
        Some(Self {
            bend_norm,
            cc74_norm,
            pressure_norm,
            tilt_norm,
        })
    }

    /// Capture the engine's last-emitted MIDI expression (for meters / debug).
    pub fn from_engine(engine: &MpeEngine) -> Option<Self> {
        if engine.active_note().is_none() {
            return None;
        }
        let bend_norm = engine
            .last_bend()
            .map(|b| (f32::from(b) - f32::from(PITCH_BEND_CENTER)) / 8191.0)
            .unwrap_or(0.0)
            .clamp(-1.0, 1.0);
        let cc74_norm = engine
            .last_cc74()
            .map(seven_bit_frac)
            .unwrap_or(0.5);
        let pressure_norm = engine
            .last_pressure()
            .map(seven_bit_frac)
            .unwrap_or(0.0);
        let tilt_norm = engine.last_tilt_cc().map(seven_bit_frac);
        Some(Self {
            bend_norm,
            cc74_norm,
            pressure_norm,
            tilt_norm,
        })
    }

    /// Blend two snapshots (e.g. segment endpoints).
    pub fn lerp(self, other: Self, t: f32) -> Self {
        let t = t.clamp(0.0, 1.0);
        let lerp_f = |a: f32, b: f32| a + (b - a) * t;
        let tilt_norm = match (self.tilt_norm, other.tilt_norm) {
            (Some(a), Some(b)) => Some(lerp_f(a, b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };
        Self {
            bend_norm: lerp_f(self.bend_norm, other.bend_norm),
            cc74_norm: lerp_f(self.cc74_norm, other.cc74_norm),
            pressure_norm: lerp_f(self.pressure_norm, other.pressure_norm),
            tilt_norm,
        }
    }

    /// Map this snapshot to an sRGB color via composite HSV encoding.
    pub fn to_rgb(self, scheme: &TrailColorScheme) -> Rgb {
        let mut hue = scheme.neutral_hue_deg + self.bend_norm * scheme.bend_hue_span_deg;
        if let Some(tilt) = self.tilt_norm {
            hue += (tilt - 0.5) * 2.0 * scheme.tilt_hue_offset_deg;
        }
        hue = hue.rem_euclid(360.0);

        // CC74 center (64) → mid saturation; extremes → muted / vivid.
        let sat_dist = (self.cc74_norm - 0.5).abs() * 2.0;
        let saturation = scheme.saturation_min
            + (scheme.saturation_max - scheme.saturation_min) * sat_dist;

        let value = scheme.min_value
            + (1.0 - scheme.min_value) * self.pressure_norm.clamp(0.0, 1.0);

        hsv_to_rgb(hue, saturation.clamp(0.0, 1.0), value.clamp(0.0, 1.0))
    }
}

/// App-level trail color configuration (persisted in HUD settings).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrailColorScheme {
    /// When false, the HUD falls back to the fixed accent trail color.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Show the HSV legend strip in the meters panel.
    #[serde(default = "default_true")]
    pub show_legend: bool,
    /// Hue at neutral pitch bend (~cyan accent).
    #[serde(default = "default_neutral_hue")]
    pub neutral_hue_deg: f32,
    /// Hue span for full ±1 bend (warm up, cool down).
    #[serde(default = "default_bend_span")]
    pub bend_hue_span_deg: f32,
    /// Minimum HSV value so faint pressure strokes stay visible.
    #[serde(default = "default_min_value")]
    pub min_value: f32,
    /// Saturation at CC74 center (neutral timbre).
    #[serde(default = "default_saturation_min")]
    pub saturation_min: f32,
    /// Saturation at CC74 extremes.
    #[serde(default = "default_saturation_max")]
    pub saturation_max: f32,
    /// Optional hue offset from tilt CC (±degrees at extremes).
    #[serde(default = "default_tilt_offset")]
    pub tilt_hue_offset_deg: f32,
}

fn default_true() -> bool {
    true
}

fn default_neutral_hue() -> f32 {
    190.0
}

fn default_bend_span() -> f32 {
    60.0
}

fn default_min_value() -> f32 {
    0.35
}

fn default_saturation_min() -> f32 {
    0.35
}

fn default_saturation_max() -> f32 {
    1.0
}

fn default_tilt_offset() -> f32 {
    15.0
}

impl Default for TrailColorScheme {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            show_legend: default_true(),
            neutral_hue_deg: default_neutral_hue(),
            bend_hue_span_deg: default_bend_span(),
            min_value: default_min_value(),
            saturation_min: default_saturation_min(),
            saturation_max: default_saturation_max(),
            tilt_hue_offset_deg: default_tilt_offset(),
        }
    }
}

/// sRGB triple in `0..=255`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

fn seven_bit_frac(v: u8) -> f32 {
    f32::from(v) / 127.0
}

/// Standard HSV (H in degrees, S/V in 0..1) → sRGB.
fn hsv_to_rgb(h: f32, s: f32, v: f32) -> Rgb {
    if s <= f32::EPSILON {
        let c = (v * 255.0).round() as u8;
        return Rgb {
            r: c,
            g: c,
            b: c,
        };
    }
    let h = (h / 60.0).rem_euclid(6.0);
    let i = h.floor() as i32;
    let f = h - i as f32;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    let (rf, gf, bf) = match i {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    Rgb {
        r: (rf * 255.0).round() as u8,
        g: (gf * 255.0).round() as u8,
        b: (bf * 255.0).round() as u8,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapping::NoteMode;
    use crate::scale::ScaleKind;
    use crate::{MidiMapping, MpeEngine};
    use tablet_core::{PenSample, ToolKind};
    use tablet_process::ProcessedSample;

    fn processed(x: f64, y: f64, pressure: f64, active: bool) -> ProcessedSample {
        ProcessedSample {
            raw: PenSample {
                t_capture_ns: 0,
                t_device_ms: 0,
                serial: 0,
                x_raw: 0,
                y_raw: 0,
                z_raw: 0,
                x_norm: x,
                y_norm: y,
                pressure_raw: 0,
                pressure_norm: pressure,
                tangent_pressure_raw: None,
                azimuth_deci_deg: None,
                altitude_deci_deg: None,
                twist_deci_deg: None,
                tilt_x_deg: None,
                tilt_y_deg: None,
                rotation_deci_deg: None,
                buttons: 0,
                tool: ToolKind::Pen,
                tool_serial: 0,
                in_proximity: active,
                status: 0,
            },
            x,
            y,
            x_filtered: None,
            y_filtered: None,
            pressure,
            active,
            tilt_x: None,
            tilt_y: None,
            twist: None,
            out_of_range: false,
        }
    }

    fn test_mapping() -> MidiMapping {
        let mut m = MidiMapping::default();
        m.scale = ScaleKind::Chromatic;
        m.mode = NoteMode::Hold;
        m.vibrato.enabled_by_default = true;
        m
    }

    fn snapshot(bend: f32, cc74: f32, pressure: f32) -> ExpressionSnapshot {
        ExpressionSnapshot {
            bend_norm: bend,
            cc74_norm: cc74,
            pressure_norm: pressure,
            tilt_norm: None,
        }
    }

    #[test]
    fn from_engine_none_without_voice() {
        let engine = MpeEngine::new();
        assert!(ExpressionSnapshot::from_engine(&engine).is_none());
    }

    #[test]
    fn neutral_bend_uses_neutral_hue() {
        let scheme = TrailColorScheme::default();
        let snap = snapshot(0.0, 0.5, 0.5);
        let rgb_center = snap.to_rgb(&scheme);
        let snap_up = snapshot(1.0, 0.5, 0.5);
        let rgb_up = snap_up.to_rgb(&scheme);
        let snap_down = snapshot(-1.0, 0.5, 0.5);
        let rgb_down = snap_down.to_rgb(&scheme);
        assert_ne!(rgb_center, rgb_up);
        assert_ne!(rgb_center, rgb_down);
        assert_ne!(rgb_up, rgb_down);
    }

    #[test]
    fn cc74_extremes_change_saturation() {
        let scheme = TrailColorScheme::default();
        let mid = snapshot(0.0, 0.5, 0.8).to_rgb(&scheme);
        let low = snapshot(0.0, 0.0, 0.8).to_rgb(&scheme);
        let high = snapshot(0.0, 1.0, 0.8).to_rgb(&scheme);
        assert_ne!(mid, low);
        assert_ne!(mid, high);
    }

    #[test]
    fn pressure_controls_brightness() {
        let scheme = TrailColorScheme::default();
        let dim = snapshot(0.0, 0.5, 0.0).to_rgb(&scheme);
        let bright = snapshot(0.0, 0.5, 1.0).to_rgb(&scheme);
        let dim_lum = u32::from(dim.r) + u32::from(dim.g) + u32::from(dim.b);
        let bright_lum = u32::from(bright.r) + u32::from(bright.g) + u32::from(bright.b);
        assert!(bright_lum > dim_lum);
    }

    #[test]
    fn min_value_floor_keeps_zero_pressure_visible() {
        let scheme = TrailColorScheme::default();
        let rgb = snapshot(0.0, 0.5, 0.0).to_rgb(&scheme);
        let lum = u32::from(rgb.r) + u32::from(rgb.g) + u32::from(rgb.b);
        assert!(lum > 0);
    }

    #[test]
    fn lerp_interpolates_axes() {
        let a = snapshot(0.0, 0.0, 0.0);
        let b = snapshot(1.0, 1.0, 1.0);
        let mid = a.lerp(b, 0.5);
        assert!((mid.bend_norm - 0.5).abs() < 1e-5);
        assert!((mid.cc74_norm - 0.5).abs() < 1e-5);
        assert!((mid.pressure_norm - 0.5).abs() < 1e-5);
    }

    #[test]
    fn trail_color_scheme_defaults() {
        let scheme = TrailColorScheme::default();
        assert!(scheme.enabled);
        assert!(scheme.show_legend);
        assert_eq!(scheme.neutral_hue_deg, 190.0);
    }

    #[test]
    fn from_performance_tracks_vertical_displacement_for_hue() {
        let mut engine = MpeEngine::new();
        let mapping = test_mapping();
        let mut out = Vec::new();
        let strike = processed(0.5, 0.5, 0.8, true);
        engine.process(&strike, &mapping, &mut out);
        out.clear();

        let dragged = processed(0.5, 0.25, 0.8, true);
        engine.process(&dragged, &mapping, &mut out);

        let midi = ExpressionSnapshot::from_engine(&engine).unwrap();
        let perf = ExpressionSnapshot::from_performance(&engine, &dragged, &mapping).unwrap();

        assert!((perf.bend_norm - 0.25).abs() < 1e-5);
        assert!(
            perf.bend_norm.abs() > midi.bend_norm.abs() * 3.0,
            "MIDI bend_norm ({}) is too small for visible hue; performance ({}) should be larger",
            midi.bend_norm,
            perf.bend_norm
        );
    }

    #[test]
    fn from_performance_tracks_horizontal_displacement_for_cc74() {
        let mut engine = MpeEngine::new();
        let mapping = test_mapping();
        let mut out = Vec::new();
        let strike = processed(0.5, 0.5, 0.8, true);
        engine.process(&strike, &mapping, &mut out);
        out.clear();

        let dragged = processed(0.75, 0.5, 0.8, true);
        engine.process(&dragged, &mapping, &mut out);

        let perf = ExpressionSnapshot::from_performance(&engine, &dragged, &mapping).unwrap();
        assert!((perf.cc74_norm - 0.75).abs() < 1e-5);
    }
}
