//! Pure tablet-pad (ExpressKey) state diffing.
//!
//! The live Windows capture path reads the set of currently pressed button
//! usages from each pad HID report; this module turns two such snapshots into
//! press/release **edges** with stable per-device button indices, with no OS
//! dependency so it is unit-testable anywhere.

/// Diff two pressed-usage snapshots into `(index, pressed)` edges.
///
/// `buttons` is the profile's sorted `(usage_page, usage)` list
/// ([`crate::caps::ProfileCaps::pad_buttons`]); a button's index is its
/// position there, so indices stay stable across sessions for the same
/// device. Usages not present in `buttons` (e.g. a usage the descriptor
/// didn't declare) are ignored. Edges are emitted in `buttons` order,
/// releases and presses alike.
pub fn button_edges(
    buttons: &[(u16, u16)],
    prev: &[(u16, u16)],
    curr: &[(u16, u16)],
) -> Vec<(u8, bool)> {
    let mut edges = Vec::new();
    for (index, usage) in buttons.iter().enumerate() {
        // Indices beyond u8 can't be represented in the wire event; the
        // enumeration path caps the list, but guard anyway.
        let Ok(index) = u8::try_from(index) else {
            break;
        };
        let was = prev.contains(usage);
        let is = curr.contains(usage);
        if was != is {
            edges.push((index, is));
        }
    }
    edges
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUTTONS: &[(u16, u16)] = &[(0x09, 1), (0x09, 2), (0x09, 3), (0x0C, 0xE9)];

    #[test]
    fn press_and_release_produce_edges() {
        // Nothing → button 1 pressed.
        assert_eq!(
            button_edges(BUTTONS, &[], &[(0x09, 2)]),
            vec![(1, true)]
        );
        // Button 1 → released.
        assert_eq!(
            button_edges(BUTTONS, &[(0x09, 2)], &[]),
            vec![(1, false)]
        );
    }

    #[test]
    fn unchanged_state_produces_no_edges() {
        let held = [(0x09, 1), (0x0C, 0xE9)];
        assert!(button_edges(BUTTONS, &held, &held).is_empty());
        assert!(button_edges(BUTTONS, &[], &[]).is_empty());
    }

    #[test]
    fn simultaneous_changes_emit_one_edge_each() {
        // Button 0 released while button 2 and the consumer usage are pressed.
        let edges = button_edges(BUTTONS, &[(0x09, 1)], &[(0x09, 3), (0x0C, 0xE9)]);
        assert_eq!(edges, vec![(0, false), (2, true), (3, true)]);
    }

    #[test]
    fn undeclared_usages_are_ignored() {
        // (0x09, 99) is not in the profile's button list.
        assert!(button_edges(BUTTONS, &[], &[(0x09, 99)]).is_empty());
    }

    #[test]
    fn index_follows_button_list_position() {
        let edges = button_edges(BUTTONS, &[], &[(0x0C, 0xE9)]);
        assert_eq!(edges, vec![(3, true)]);
    }
}
