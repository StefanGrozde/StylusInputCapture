//! Pure HID-report → [`PenSample`] decode (SPEC_BGC §6).
//!
//! The decoder reads usage values through the [`UsageReader`] seam rather than
//! touching the OS directly, so [`decode_report`] is a **pure function**: caps +
//! a reader in, `PenSample` out. The live Windows path supplies a reader backed
//! by `HidP_GetUsageValue`/`HidP_GetUsages` (see [`crate::capture`]); tests
//! supply [`BitfieldReader`], which extracts usages straight from report bytes.
//! Both exercise the *same* mapping/normalization/tool-selection logic.

use tablet_core::{normalize, tilt_from_orientation, PenSample, ToolKind};

use crate::caps::ProfileCaps;
use crate::hid::*;

/// Source of decoded HID usage values for a single report.
///
/// Value usages return the (sign-extended where applicable) integer value;
/// button usages return whether the control is currently asserted.
pub trait UsageReader {
    /// Read a value-type usage, or `None` if it is absent from this report.
    fn value(&self, usage_page: u16, usage: u16) -> Option<i32>;
    /// Read a button-type usage; `false` if absent or not asserted.
    fn button(&self, usage_page: u16, usage: u16) -> bool;
}

/// Decode one HID report into a [`PenSample`] (SPEC_BGC §6.1).
///
/// `caps` provides the logical ranges used for normalization and drives which
/// optional axes/tools are populated. `t_capture_ns` and `serial` are supplied by
/// the caller (host-derived QPC timestamp and the synthesized monotonic serial,
/// SPEC_BGC §5). `t_device_ms`, `z_raw`, `rotation`, and `tool_serial` take their
/// documented defaults (`0`/`None`) — HID pens expose no source for them.
pub fn decode_report<R: UsageReader>(
    reader: &R,
    caps: &ProfileCaps,
    t_capture_ns: u64,
    serial: u32,
) -> PenSample {
    // --- position (Generic Desktop) ---
    let x_raw = reader.value(PAGE_GENERIC_DESKTOP, USAGE_X).unwrap_or(0);
    let y_raw = reader.value(PAGE_GENERIC_DESKTOP, USAGE_Y).unwrap_or(0);
    let x_norm = norm_axis(x_raw, caps.x.as_ref());
    let y_norm = norm_axis(y_raw, caps.y.as_ref());

    // --- pressure (Digitizer) ---
    let pressure_raw = reader
        .value(PAGE_DIGITIZER, USAGE_TIP_PRESSURE)
        .unwrap_or(0)
        .max(0) as u32;
    let pressure_norm = norm_axis(pressure_raw as i32, caps.pressure.as_ref());

    // --- tilt / orientation ---
    // Prefer direct X/Y tilt when the device exposes it; otherwise derive tilt
    // from azimuth/altitude. Angular usages are assumed to be in whole degrees
    // (the common HID convention), so we scale by 10 into the 0.1° fields.
    let (mut azimuth_deci_deg, mut altitude_deci_deg) = (None, None);
    let (tilt_x_deg, tilt_y_deg) = if caps.x_tilt.is_some() || caps.y_tilt.is_some() {
        let tx = caps
            .x_tilt
            .map(|_| reader.value(PAGE_DIGITIZER, USAGE_X_TILT).unwrap_or(0) as f64);
        let ty = caps
            .y_tilt
            .map(|_| reader.value(PAGE_DIGITIZER, USAGE_Y_TILT).unwrap_or(0) as f64);
        (tx, ty)
    } else if caps.azimuth.is_some() && caps.altitude.is_some() {
        let az = reader.value(PAGE_DIGITIZER, USAGE_AZIMUTH).unwrap_or(0) * 10;
        let alt = reader.value(PAGE_DIGITIZER, USAGE_ALTITUDE).unwrap_or(0) * 10;
        azimuth_deci_deg = Some(az);
        altitude_deci_deg = Some(alt);
        let (tx, ty) = tilt_from_orientation(az, alt);
        (Some(tx), Some(ty))
    } else {
        (None, None)
    };

    // --- twist ---
    let twist_deci_deg = caps
        .twist
        .map(|_| reader.value(PAGE_DIGITIZER, USAGE_TWIST).unwrap_or(0) * 10);

    // --- tangent (barrel) pressure ---
    let tangent_pressure_raw = caps
        .tangent_pressure
        .map(|_| reader.value(PAGE_DIGITIZER, USAGE_BARREL_PRESSURE).unwrap_or(0));

    // --- switches → buttons bitmask ---
    let tip = caps.has_tip && reader.button(PAGE_DIGITIZER, USAGE_TIP_SWITCH);
    let barrel = caps.has_barrel && reader.button(PAGE_DIGITIZER, USAGE_BARREL_SWITCH);
    let eraser = caps.has_eraser && reader.button(PAGE_DIGITIZER, USAGE_ERASER);
    let invert = caps.has_invert && reader.button(PAGE_DIGITIZER, USAGE_INVERT);
    let mut buttons = 0u32;
    buttons |= (tip as u32) << BUTTON_BIT_TIP;
    buttons |= (barrel as u32) << BUTTON_BIT_BARREL;
    buttons |= (eraser as u32) << BUTTON_BIT_ERASER;

    // --- proximity ---
    // If the device exposes In-Range, honor it; otherwise a delivered report
    // implies the tool is in range.
    let in_proximity = if caps.has_in_range {
        reader.button(PAGE_DIGITIZER, USAGE_IN_RANGE)
    } else {
        true
    };

    // --- tool kind (SPEC_BGC §6.3) ---
    let tool = if eraser || invert {
        ToolKind::Eraser
    } else if caps.tangent_pressure.is_some() {
        ToolKind::Airbrush
    } else {
        ToolKind::Pen
    };

    PenSample {
        t_capture_ns,
        t_device_ms: 0,
        serial,
        x_raw,
        y_raw,
        z_raw: 0,
        x_norm,
        y_norm,
        pressure_raw,
        pressure_norm,
        tangent_pressure_raw,
        azimuth_deci_deg,
        altitude_deci_deg,
        twist_deci_deg,
        tilt_x_deg,
        tilt_y_deg,
        rotation_deci_deg: None,
        buttons,
        tool,
        tool_serial: 0,
        in_proximity,
        status: 0,
    }
}

/// Normalize a raw axis value against its logical range, or `0.0` if the axis is
/// absent.
fn norm_axis(raw: i32, range: Option<&crate::caps::AxisRange>) -> f64 {
    match range {
        Some(r) => normalize(raw as i64, r.min as i64, r.max as i64),
        None => 0.0,
    }
}

// ---------------------------------------------------------------------------
// Byte-driven reader (pure) — used by unit tests and as the reference for the
// HID bit layout the live reader reproduces via HidP.
// ---------------------------------------------------------------------------

/// Bit layout of one usage within a HID report (offset from the start of the
/// report data, i.e. *including* any leading report-ID byte).
#[derive(Copy, Clone, Debug)]
pub struct FieldLayout {
    /// HID usage page.
    pub usage_page: u16,
    /// HID usage.
    pub usage: u16,
    /// Absolute bit offset from the start of the report buffer.
    pub bit_offset: u32,
    /// Field width in bits.
    pub bit_size: u16,
    /// Whether the field is two's-complement signed.
    pub signed: bool,
}

impl FieldLayout {
    /// Convenience constructor for an unsigned field.
    pub fn unsigned(usage_page: u16, usage: u16, bit_offset: u32, bit_size: u16) -> Self {
        Self {
            usage_page,
            usage,
            bit_offset,
            bit_size,
            signed: false,
        }
    }

    /// Convenience constructor for a signed field.
    pub fn signed(usage_page: u16, usage: u16, bit_offset: u32, bit_size: u16) -> Self {
        Self {
            usage_page,
            usage,
            bit_offset,
            bit_size,
            signed: true,
        }
    }
}

/// A [`UsageReader`] that extracts usages directly from raw report bytes given an
/// explicit bit layout. HID packs fields little-endian, LSB first.
pub struct BitfieldReader<'a> {
    report: &'a [u8],
    values: &'a [FieldLayout],
    buttons: &'a [FieldLayout],
}

impl<'a> BitfieldReader<'a> {
    /// Build a reader over `report` with the given value- and button-field layouts.
    pub fn new(report: &'a [u8], values: &'a [FieldLayout], buttons: &'a [FieldLayout]) -> Self {
        Self {
            report,
            values,
            buttons,
        }
    }
}

impl UsageReader for BitfieldReader<'_> {
    fn value(&self, usage_page: u16, usage: u16) -> Option<i32> {
        let f = self
            .values
            .iter()
            .find(|f| f.usage_page == usage_page && f.usage == usage)?;
        let raw = extract_bits(self.report, f.bit_offset, f.bit_size);
        Some(if f.signed {
            sign_extend(raw, f.bit_size)
        } else {
            raw as i32
        })
    }

    fn button(&self, usage_page: u16, usage: u16) -> bool {
        self.buttons
            .iter()
            .find(|f| f.usage_page == usage_page && f.usage == usage)
            .map(|f| extract_bits(self.report, f.bit_offset, f.bit_size.max(1)) != 0)
            .unwrap_or(false)
    }
}

/// Extract `bit_size` bits starting at `bit_offset` (LSB-first) from `report`.
/// Bits past the end of the buffer read as zero.
fn extract_bits(report: &[u8], bit_offset: u32, bit_size: u16) -> u32 {
    let mut val: u32 = 0;
    for i in 0..bit_size as u32 {
        let bit = bit_offset + i;
        let byte = (bit / 8) as usize;
        let shift = bit % 8;
        if byte < report.len() && (report[byte] >> shift) & 1 != 0 {
            val |= 1 << i;
        }
    }
    val
}

/// Two's-complement sign-extend a `bit_size`-bit value to `i32`.
fn sign_extend(val: u32, bit_size: u16) -> i32 {
    if bit_size == 0 || bit_size >= 32 {
        return val as i32;
    }
    let shift = 32 - bit_size as u32;
    ((val << shift) as i32) >> shift
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caps::AxisRange;

    /// A typical pen profile: X/Y 16-bit, pressure 16-bit, tip/barrel/eraser/
    /// invert/in-range switches, signed X/Y tilt.
    fn pen_profile() -> ProfileCaps {
        ProfileCaps {
            device_name: "Test Pen".into(),
            driver_version: "hid".into(),
            x: Some(AxisRange::new(0, 30000)),
            y: Some(AxisRange::new(0, 20000)),
            pressure: Some(AxisRange::new(0, 8191)),
            x_tilt: Some(AxisRange::new(-90, 90)),
            y_tilt: Some(AxisRange::new(-90, 90)),
            has_in_range: true,
            has_tip: true,
            has_barrel: true,
            has_eraser: true,
            has_invert: true,
            ..Default::default()
        }
    }

    // Layout used by the byte-level tests. Report is unnumbered (no leading ID).
    //   bytes 0..2  : X  (u16 LE) @ bit 0
    //   bytes 2..4  : Y  (u16 LE) @ bit 16
    //   bytes 4..6  : pressure (u16 LE) @ bit 32
    //   byte  6     : X tilt (i8) @ bit 48
    //   byte  7     : Y tilt (i8) @ bit 56
    //   byte  8     : switch bitmap @ bit 64: tip=b0, barrel=b1, eraser=b2,
    //                 invert=b3, in-range=b4
    fn value_layout() -> Vec<FieldLayout> {
        vec![
            FieldLayout::unsigned(PAGE_GENERIC_DESKTOP, USAGE_X, 0, 16),
            FieldLayout::unsigned(PAGE_GENERIC_DESKTOP, USAGE_Y, 16, 16),
            FieldLayout::unsigned(PAGE_DIGITIZER, USAGE_TIP_PRESSURE, 32, 16),
            FieldLayout::signed(PAGE_DIGITIZER, USAGE_X_TILT, 48, 8),
            FieldLayout::signed(PAGE_DIGITIZER, USAGE_Y_TILT, 56, 8),
        ]
    }

    fn button_layout() -> Vec<FieldLayout> {
        vec![
            FieldLayout::unsigned(PAGE_DIGITIZER, USAGE_TIP_SWITCH, 64, 1),
            FieldLayout::unsigned(PAGE_DIGITIZER, USAGE_BARREL_SWITCH, 65, 1),
            FieldLayout::unsigned(PAGE_DIGITIZER, USAGE_ERASER, 66, 1),
            FieldLayout::unsigned(PAGE_DIGITIZER, USAGE_INVERT, 67, 1),
            FieldLayout::unsigned(PAGE_DIGITIZER, USAGE_IN_RANGE, 68, 1),
        ]
    }

    fn report(x: u16, y: u16, p: u16, tx: i8, ty: i8, switches: u8) -> [u8; 9] {
        [
            x as u8,
            (x >> 8) as u8,
            y as u8,
            (y >> 8) as u8,
            p as u8,
            (p >> 8) as u8,
            tx as u8,
            ty as u8,
            switches,
        ]
    }

    #[test]
    fn decodes_pen_position_pressure_and_tilt() {
        let caps = pen_profile();
        let values = value_layout();
        let buttons = button_layout();
        // tip down (b0) + in-range (b4) => 0b0001_0001 = 0x11
        let bytes = report(15000, 5000, 4096, -30, 45, 0x11);
        let reader = BitfieldReader::new(&bytes, &values, &buttons);

        let s = decode_report(&reader, &caps, 123, 7);

        assert_eq!(s.x_raw, 15000);
        assert_eq!(s.y_raw, 5000);
        assert_eq!(s.pressure_raw, 4096);
        assert!((s.x_norm - 0.5).abs() < 1e-9);
        assert!((s.y_norm - 0.25).abs() < 1e-9);
        assert!((s.pressure_norm - 0.5).abs() < 1e-3);
        assert_eq!(s.tilt_x_deg, Some(-30.0));
        assert_eq!(s.tilt_y_deg, Some(45.0));
        assert_eq!(s.serial, 7);
        assert_eq!(s.t_capture_ns, 123);
        assert_eq!(s.t_device_ms, 0);
        assert_eq!(s.z_raw, 0);
        assert_eq!(s.rotation_deci_deg, None);
        assert_eq!(s.tool_serial, 0);
    }

    #[test]
    fn pen_tip_down_sets_buttons_and_proximity_and_toolkind() {
        let caps = pen_profile();
        let values = value_layout();
        let buttons = button_layout();
        // tip (b0) + barrel (b1) + in-range (b4) = 0x13
        let bytes = report(0, 0, 0, 0, 0, 0x13);
        let reader = BitfieldReader::new(&bytes, &values, &buttons);

        let s = decode_report(&reader, &caps, 0, 0);

        assert_eq!(s.buttons & (1 << BUTTON_BIT_TIP), 1 << BUTTON_BIT_TIP);
        assert_eq!(s.buttons & (1 << BUTTON_BIT_BARREL), 1 << BUTTON_BIT_BARREL);
        assert_eq!(s.buttons & (1 << BUTTON_BIT_ERASER), 0);
        assert!(s.in_proximity);
        assert_eq!(s.tool, ToolKind::Pen);
    }

    #[test]
    fn eraser_switch_selects_eraser_toolkind() {
        let caps = pen_profile();
        let values = value_layout();
        let buttons = button_layout();
        // eraser (b2) + in-range (b4) = 0x14
        let bytes = report(100, 200, 0, 0, 0, 0x14);
        let reader = BitfieldReader::new(&bytes, &values, &buttons);

        let s = decode_report(&reader, &caps, 0, 0);

        assert_eq!(s.tool, ToolKind::Eraser);
        assert_eq!(s.buttons & (1 << BUTTON_BIT_ERASER), 1 << BUTTON_BIT_ERASER);
    }

    #[test]
    fn invert_usage_selects_eraser_toolkind() {
        let caps = pen_profile();
        let values = value_layout();
        let buttons = button_layout();
        // invert (b3) + in-range (b4) = 0x18
        let bytes = report(0, 0, 0, 0, 0, 0x18);
        let reader = BitfieldReader::new(&bytes, &values, &buttons);

        let s = decode_report(&reader, &caps, 0, 0);
        assert_eq!(s.tool, ToolKind::Eraser);
    }

    #[test]
    fn out_of_range_clears_proximity() {
        let caps = pen_profile();
        let values = value_layout();
        let buttons = button_layout();
        // no bits set => in-range bit clear
        let bytes = report(0, 0, 0, 0, 0, 0x00);
        let reader = BitfieldReader::new(&bytes, &values, &buttons);

        let s = decode_report(&reader, &caps, 0, 0);
        assert!(!s.in_proximity);
    }

    #[test]
    fn missing_axes_take_defaults() {
        // Minimal profile: only X/Y, no pressure/tilt/switches.
        let caps = ProfileCaps {
            x: Some(AxisRange::new(0, 1000)),
            y: Some(AxisRange::new(0, 1000)),
            ..Default::default()
        };
        let values = vec![
            FieldLayout::unsigned(PAGE_GENERIC_DESKTOP, USAGE_X, 0, 16),
            FieldLayout::unsigned(PAGE_GENERIC_DESKTOP, USAGE_Y, 16, 16),
        ];
        let bytes: [u8; 4] = [232, 3, 244, 1]; // X=1000, Y=500
        let reader = BitfieldReader::new(&bytes, &values, &[]);

        let s = decode_report(&reader, &caps, 0, 0);
        assert_eq!(s.x_raw, 1000);
        assert!((s.x_norm - 1.0).abs() < 1e-9);
        assert_eq!(s.pressure_raw, 0);
        assert_eq!(s.tilt_x_deg, None);
        assert_eq!(s.twist_deci_deg, None);
        assert_eq!(s.tangent_pressure_raw, None);
        // No In-Range usage => a delivered report implies proximity.
        assert!(s.in_proximity);
        assert_eq!(s.tool, ToolKind::Pen);
    }

    #[test]
    fn azimuth_altitude_derive_tilt_when_no_direct_tilt() {
        // Device reports orientation, not X/Y tilt.
        let caps = ProfileCaps {
            x: Some(AxisRange::new(0, 1000)),
            y: Some(AxisRange::new(0, 1000)),
            azimuth: Some(AxisRange::new(0, 359)),
            altitude: Some(AxisRange::new(0, 90)),
            ..Default::default()
        };
        // azimuth=0°, altitude=45°  → tilt_x≈45°, tilt_y≈0°
        let values = vec![
            FieldLayout::unsigned(PAGE_DIGITIZER, USAGE_AZIMUTH, 0, 16),
            FieldLayout::unsigned(PAGE_DIGITIZER, USAGE_ALTITUDE, 16, 16),
        ];
        let bytes: [u8; 4] = [0, 0, 45, 0];
        let reader = BitfieldReader::new(&bytes, &values, &[]);

        let s = decode_report(&reader, &caps, 0, 0);
        assert_eq!(s.azimuth_deci_deg, Some(0));
        assert_eq!(s.altitude_deci_deg, Some(450));
        let tx = s.tilt_x_deg.unwrap();
        let ty = s.tilt_y_deg.unwrap();
        assert!((tx - 45.0).abs() < 1e-6, "tilt_x={tx}");
        assert!(ty.abs() < 1e-6, "tilt_y={ty}");
    }
}
