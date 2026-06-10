//! Pad-grid geometry and note layout — the Push-style playing surface.
//!
//! Pure functions shared by the [`MpeEngine`](crate::MpeEngine) (hit-testing
//! and pitch) and the HUD (drawing), so the pads you see are exactly the pads
//! that sound. All positions are normalized `[0, 1]` coordinates from the
//! capture stream, with Y growing **downward** (screen/digitizer convention);
//! pad rows count **upward** from the bottom of the surface, like a Push.

use crate::mapping::{GridConfig, MidiMapping};

/// One pad of the grid. `row` 0 is the **bottom** row, `col` 0 the left column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pad {
    pub row: u8,
    pub col: u8,
}

/// The pad under a normalized `(x, y)` position (clamped to the surface).
pub fn pad_at(x: f64, y: f64, grid: &GridConfig) -> Pad {
    let cols = grid.cols.max(1);
    let rows = grid.rows.max(1);
    let col = (x.clamp(0.0, 1.0) * f64::from(cols)) as u8;
    // Stream Y grows downward; row 0 is the bottom row.
    let row_top = (y.clamp(0.0, 1.0) * f64::from(rows)) as u8;
    Pad {
        row: rows - 1 - row_top.min(rows - 1),
        col: col.min(cols - 1),
    }
}

/// Position within the pad under `(x, y)`, each component in `[0, 1]`.
///
/// `fx` grows rightward; `fy` grows **upward** (1 at the pad's top edge), so it
/// can feed CC74 "slide" directly.
pub fn pad_fraction(x: f64, y: f64, grid: &GridConfig) -> (f64, f64) {
    let fx = (x.clamp(0.0, 1.0) * f64::from(grid.cols.max(1))).fract();
    let fy_down = (y.clamp(0.0, 1.0) * f64::from(grid.rows.max(1))).fract();
    // The bottom/right surface edges land exactly on a cell boundary
    // (fract = 0); they belong to the edge cell, so 0 there is already right.
    (fx, 1.0 - fy_down)
}

/// The MIDI note a pad sounds, or `None` if it falls off the top of the MIDI
/// range.
///
/// The pad's scale-degree index is `row * row_interval_degrees + col`, walked
/// up the mapping's scale from `low_note`'s degree. `low_note` itself is first
/// snapped *up* to the nearest in-scale note so the bottom-left pad is always
/// in scale.
pub fn pad_note(pad: Pad, m: &MidiMapping) -> Option<u8> {
    let degree_index =
        u32::from(pad.row) * u32::from(m.grid.row_interval_degrees.max(1)) + u32::from(pad.col);
    let intervals = m.scale.intervals();
    let key = m.key % 12;

    // Snap low_note up to the first in-scale note at or above it.
    let mut base = i32::from(m.low_note);
    while base <= 127 && !in_scale(base, key, intervals) {
        base += 1;
    }
    if base > 127 {
        return None;
    }

    // Walk `degree_index` scale degrees up from `base`. Degrees per octave is
    // the interval count, so jump whole octaves first, then step.
    let per_octave = intervals.len() as u32;
    let base_pc = (base - i32::from(key)).rem_euclid(12) as u8;
    let base_pos = intervals.iter().position(|&i| i == base_pc)?;
    let total = base_pos as u32 + degree_index;
    let octaves = total / per_octave;
    let step = (total % per_octave) as usize;
    let base_octave_root = base - i32::from(intervals[base_pos]);
    let note = base_octave_root + (octaves as i32) * 12 + i32::from(intervals[step]);
    (0..=127).contains(&note).then_some(note as u8)
}

/// Is this pad's note the scale root (for HUD accent coloring)?
pub fn is_root(pad: Pad, m: &MidiMapping) -> bool {
    pad_note(pad, m)
        .map(|n| (i32::from(n) - i32::from(m.key % 12)).rem_euclid(12) == 0)
        .unwrap_or(false)
}

fn in_scale(note: i32, key: u8, intervals: &[u8]) -> bool {
    let pc = (note - i32::from(key)).rem_euclid(12) as u8;
    intervals.contains(&pc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapping::MidiMapping;
    use crate::scale::ScaleKind;

    fn mapping(scale: ScaleKind) -> MidiMapping {
        let mut m = MidiMapping::default();
        m.scale = scale;
        m.key = 0;
        m.low_note = 48; // C3
        m
    }

    #[test]
    fn pad_at_corners_and_clamping() {
        let g = GridConfig::default(); // 8x8
        assert_eq!(pad_at(0.0, 1.0, &g), Pad { row: 0, col: 0 }); // bottom-left
        assert_eq!(pad_at(0.999, 0.0, &g), Pad { row: 7, col: 7 }); // top-right
        // Exact 1.0 edges belong to the last cell, out-of-range clamps.
        assert_eq!(pad_at(1.0, 1.0, &g), Pad { row: 0, col: 7 });
        assert_eq!(pad_at(2.0, -1.0, &g), Pad { row: 7, col: 7 });
    }

    #[test]
    fn pad_fraction_is_within_pad_and_fy_grows_upward() {
        let g = GridConfig::default(); // 8x8 → pad size 0.125
        // Center of the bottom-left pad.
        let (fx, fy) = pad_fraction(0.0625, 1.0 - 0.0625, &g);
        assert!((fx - 0.5).abs() < 1e-9);
        assert!((fy - 0.5).abs() < 1e-9);
        // Moving up (smaller stream Y) raises fy.
        let (_, fy_hi) = pad_fraction(0.0625, 1.0 - 0.11, &g);
        assert!(fy_hi > fy);
    }

    #[test]
    fn chromatic_layout_steps_by_semitone_and_row_interval() {
        let mut m = mapping(ScaleKind::Chromatic);
        m.grid.row_interval_degrees = 5; // chromatic "in 4ths"
        assert_eq!(pad_note(Pad { row: 0, col: 0 }, &m), Some(48));
        assert_eq!(pad_note(Pad { row: 0, col: 1 }, &m), Some(49));
        assert_eq!(pad_note(Pad { row: 1, col: 0 }, &m), Some(53)); // +5 st
    }

    #[test]
    fn major_layout_walks_scale_degrees() {
        let m = mapping(ScaleKind::Major); // C major from C3, rows in 3 degrees
        let row0: Vec<_> = (0..7)
            .map(|c| pad_note(Pad { row: 0, col: c }, &m).unwrap())
            .collect();
        assert_eq!(row0, vec![48, 50, 52, 53, 55, 57, 59]); // C D E F G A B
        // Row 1 starts 3 degrees up: F3 (53) — a 4th, like Push.
        assert_eq!(pad_note(Pad { row: 1, col: 0 }, &m), Some(53));
        // Octave wrap: col 7 of row 0 is C4.
        assert_eq!(pad_note(Pad { row: 0, col: 7 }, &m), Some(60));
    }

    #[test]
    fn out_of_scale_low_note_snaps_up() {
        let mut m = mapping(ScaleKind::Major);
        m.low_note = 49; // C#3, not in C major → snaps to D3 (50)
        assert_eq!(pad_note(Pad { row: 0, col: 0 }, &m), Some(50));
    }

    #[test]
    fn notes_off_the_top_are_none() {
        let mut m = mapping(ScaleKind::Chromatic);
        m.low_note = 120;
        assert_eq!(pad_note(Pad { row: 0, col: 7 }, &m), Some(127));
        assert_eq!(pad_note(Pad { row: 1, col: 7 }, &m), None);
    }

    #[test]
    fn root_pads_are_flagged() {
        let m = mapping(ScaleKind::Major);
        assert!(is_root(Pad { row: 0, col: 0 }, &m)); // C3
        assert!(!is_root(Pad { row: 0, col: 1 }, &m)); // D3
        assert!(is_root(Pad { row: 0, col: 7 }, &m)); // C4
    }
}
