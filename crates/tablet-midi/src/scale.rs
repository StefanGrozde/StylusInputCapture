//! Musical scales and position→pitch quantization.
//!
//! The HUD maps horizontal pen position to pitch. [`quantize`] turns a
//! normalized X in `[0, 1]` into a continuous MIDI pitch across a configured
//! span, snaps it to the nearest note in the selected [`ScaleKind`] + key, and
//! reports the leftover offset in semitones so the engine can drive MPE
//! pitch-bend for micro-expression / glide.

use serde::{Deserialize, Serialize};

/// A musical scale, expressed as semitone offsets from its root.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScaleKind {
    Chromatic,
    Major,
    NaturalMinor,
    MajorPentatonic,
    MinorPentatonic,
    Blues,
    Dorian,
    Mixolydian,
}

impl ScaleKind {
    /// Semitone offsets (0–11) of the scale degrees from the root.
    pub fn intervals(self) -> &'static [u8] {
        match self {
            ScaleKind::Chromatic => &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
            ScaleKind::Major => &[0, 2, 4, 5, 7, 9, 11],
            ScaleKind::NaturalMinor => &[0, 2, 3, 5, 7, 8, 10],
            ScaleKind::MajorPentatonic => &[0, 2, 4, 7, 9],
            ScaleKind::MinorPentatonic => &[0, 3, 5, 7, 10],
            ScaleKind::Blues => &[0, 3, 5, 6, 7, 10],
            ScaleKind::Dorian => &[0, 2, 3, 5, 7, 9, 10],
            ScaleKind::Mixolydian => &[0, 2, 4, 5, 7, 9, 10],
        }
    }

    /// Human-readable label for UI menus.
    pub fn label(self) -> &'static str {
        match self {
            ScaleKind::Chromatic => "Chromatic",
            ScaleKind::Major => "Major",
            ScaleKind::NaturalMinor => "Natural minor",
            ScaleKind::MajorPentatonic => "Major pentatonic",
            ScaleKind::MinorPentatonic => "Minor pentatonic",
            ScaleKind::Blues => "Blues",
            ScaleKind::Dorian => "Dorian",
            ScaleKind::Mixolydian => "Mixolydian",
        }
    }

    /// All variants, for populating UI menus.
    pub const ALL: [ScaleKind; 8] = [
        ScaleKind::Chromatic,
        ScaleKind::Major,
        ScaleKind::NaturalMinor,
        ScaleKind::MajorPentatonic,
        ScaleKind::MinorPentatonic,
        ScaleKind::Blues,
        ScaleKind::Dorian,
        ScaleKind::Mixolydian,
    ];
}

/// Pitch-class names indexed 0=C .. 11=B.
pub const PITCH_CLASS_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

/// The snapped pitch for a position, plus the residual to the exact position.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct QuantizedPitch {
    /// Nearest in-scale MIDI note (0–127).
    pub note: u8,
    /// Continuous (un-snapped) pitch in MIDI semitones, before rounding.
    pub continuous: f64,
    /// `continuous - note`, in semitones — the offset to drive as pitch-bend.
    pub bend_semitones: f64,
}

/// Is `note` a member of `scale` rooted at pitch-class `key` (0–11)?
fn in_scale(note: u8, key: u8, intervals: &[u8]) -> bool {
    let pc = (i32::from(note) - i32::from(key)).rem_euclid(12) as u8;
    intervals.contains(&pc)
}

/// Map `x ∈ [0, 1]` to a pitch over `span_notes` semitones starting at
/// `low_note`, snapped to the nearest note in `scale`/`key`.
///
/// `key` is a pitch class 0–11 (0 = C). The returned `bend_semitones` is the
/// signed distance from the snapped note to the exact pointed pitch.
pub fn quantize(x: f64, low_note: u8, span_notes: u8, scale: ScaleKind, key: u8) -> QuantizedPitch {
    let continuous = f64::from(low_note) + x.clamp(0.0, 1.0) * f64::from(span_notes);
    let intervals = scale.intervals();
    let key = key % 12;
    let base = continuous.round() as i32;

    // Search outward from the rounded note for the nearest in-scale pitch.
    // A full chromatic scale matches at distance 0; the widest gap in any
    // supported scale is < 12, so 12 steps always finds a match.
    let mut best = base.clamp(0, 127);
    for d in 0..=12 {
        let lo = base - d;
        let hi = base + d;
        if (0..=127).contains(&lo) && in_scale(lo as u8, key, intervals) {
            best = lo;
            break;
        }
        if (0..=127).contains(&hi) && in_scale(hi as u8, key, intervals) {
            best = hi;
            break;
        }
    }

    let note = best.clamp(0, 127) as u8;
    QuantizedPitch {
        note,
        continuous,
        bend_semitones: continuous - f64::from(note),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chromatic_snaps_to_rounded_note() {
        // span 12 semitones from C3 (48). x=0 → 48, x=1 → 60.
        assert_eq!(quantize(0.0, 48, 12, ScaleKind::Chromatic, 0).note, 48);
        assert_eq!(quantize(1.0, 48, 12, ScaleKind::Chromatic, 0).note, 60);
        // Midpoint → 54 (F#3).
        assert_eq!(quantize(0.5, 48, 12, ScaleKind::Chromatic, 0).note, 54);
    }

    #[test]
    fn major_scale_skips_non_scale_notes() {
        // C major (key 0): note 49 (C#) is out of scale and must snap to a
        // neighbour (C=48 or D=50), never to 49 itself.
        let q = quantize(1.0 / 12.0, 48, 12, ScaleKind::Major, 0);
        assert!(q.note == 48 || q.note == 50);
        assert_ne!(q.note, 49);
        // The reported note is always in-scale.
        assert!(in_scale(q.note, 0, ScaleKind::Major.intervals()));
    }

    #[test]
    fn bend_is_residual_to_exact_position() {
        // Continuous pitch = 48 + 0.5/12 * 12 ... pick a value with known residual.
        // x such that continuous = 50.3 → low 48, span 12 → x = 2.3/12.
        let q = quantize(2.3 / 12.0, 48, 12, ScaleKind::Chromatic, 0);
        assert!((q.continuous - 50.3).abs() < 1e-9);
        assert_eq!(q.note, 50);
        assert!((q.bend_semitones - 0.3).abs() < 1e-9);
    }

    #[test]
    fn quantized_note_always_in_scale_across_range() {
        for kind in ScaleKind::ALL {
            for key in 0..12u8 {
                for i in 0..=100 {
                    let x = f64::from(i) / 100.0;
                    let q = quantize(x, 36, 48, kind, key);
                    assert!(
                        in_scale(q.note, key, kind.intervals()),
                        "{kind:?} key {key}: note {} not in scale",
                        q.note
                    );
                }
            }
        }
    }

    #[test]
    fn out_of_unit_x_is_clamped() {
        assert_eq!(quantize(-1.0, 48, 12, ScaleKind::Chromatic, 0).note, 48);
        assert_eq!(quantize(2.0, 48, 12, ScaleKind::Chromatic, 0).note, 60);
    }
}
