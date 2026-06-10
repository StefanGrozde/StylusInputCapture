//! The HUD's MIDI output boundary.
//!
//! This is the *only* place the app touches a real MIDI driver. The mapping
//! logic in `tablet-midi` is pure and emits [`MidiEvent`]s; here we open a
//! `midir` connection (an existing port or a freshly created virtual one) and
//! forward `event.to_bytes()`.
//!
//! The whole backend is behind the `midi-out` cargo feature. With the feature
//! off, [`MidiOut`] compiles to no-ops so the crate builds on systems without a
//! MIDI driver/dev-headers (e.g. Linux without ALSA) — the GUI still runs, it
//! just can't emit notes.

use tablet_midi::{MidiEvent, MidiMapping};

/// Build the MPE setup messages a receiver needs to enter MPE mode for the
/// configured zone: per-member-channel pitch-bend range (RPN 0) and the MPE
/// Configuration Message (RPN 6 on the master channel = member count).
///
/// Pure, so it is unit-testable without any MIDI driver.
pub fn mpe_init_events(m: &MidiMapping) -> Vec<MidiEvent> {
    let mut events = Vec::new();
    let bend_range = m.mpe.pitch_bend_range_semitones.round().clamp(0.0, 96.0) as u8;

    // MCM: a zone's members are always the channels adjacent to the master,
    // so advertise enough of them to cover every channel notes go out on —
    // `member_count()` would leave a raised `member_low..=member_high` range
    // outside the zone the receiver was configured for.
    push_rpn(&mut events, m.mpe.master_channel, 0x00, 0x06, m.mpe.zone_member_count());

    // Per-member pitch-bend sensitivity (RPN 0,0 = semitones), on every
    // channel in the advertised zone, not just the ones notes are sent on.
    for channel in m.mpe.zone_member_range() {
        push_rpn(&mut events, channel, 0x00, 0x00, bend_range);
    }
    events
}

/// Push an RPN write (`CC101=msb`, `CC100=lsb`, `CC6=data`, then RPN-null) for
/// `channel`.
fn push_rpn(out: &mut Vec<MidiEvent>, channel: u8, rpn_msb: u8, rpn_lsb: u8, data_msb: u8) {
    let cc = |controller, value| MidiEvent::ControlChange {
        channel,
        controller,
        value,
    };
    out.push(cc(101, rpn_msb));
    out.push(cc(100, rpn_lsb));
    out.push(cc(6, data_msb));
    // RPN null (101/100 = 127) so subsequent data entries aren't misattributed.
    out.push(cc(101, 127));
    out.push(cc(100, 127));
}

#[cfg(feature = "midi-out")]
mod backend {
    use super::*;
    use midir::{MidiOutput, MidiOutputConnection};

    const CLIENT: &str = "tablet-hud";
    const PORT_NAME: &str = "tablet-hud out";

    /// A live (or not-yet-connected) MIDI output.
    pub struct MidiOut {
        conn: Option<MidiOutputConnection>,
        label: Option<String>,
    }

    impl MidiOut {
        pub fn new() -> Self {
            Self {
                conn: None,
                label: None,
            }
        }

        /// Names of the currently available output ports, in `connect_index`
        /// order.
        pub fn list_ports() -> Vec<String> {
            match MidiOutput::new(CLIENT) {
                Ok(out) => out
                    .ports()
                    .iter()
                    .map(|p| out.port_name(p).unwrap_or_else(|_| "<unknown>".to_owned()))
                    .collect(),
                Err(_) => Vec::new(),
            }
        }

        /// Index of the first output port whose name contains `substring`.
        pub fn find_port_index_by_name(substring: &str) -> Option<usize> {
            Self::list_ports()
                .iter()
                .position(|name| name.contains(substring))
        }

        /// Can this platform create virtual ports? (unix: yes; Windows: no.)
        pub fn virtual_supported() -> bool {
            cfg!(unix)
        }

        /// Connect to the existing output port at `index` (per [`list_ports`]).
        pub fn connect_index(&mut self, index: usize) -> Result<(), String> {
            let out = MidiOutput::new(CLIENT).map_err(|e| e.to_string())?;
            let ports = out.ports();
            let port = ports
                .get(index)
                .ok_or_else(|| format!("no MIDI output port at index {index}"))?;
            let label = out
                .port_name(port)
                .unwrap_or_else(|_| "<unknown>".to_owned());
            let conn = out.connect(port, PORT_NAME).map_err(|e| e.to_string())?;
            self.conn = Some(conn);
            self.label = Some(label);
            Ok(())
        }

        /// Create a virtual output port (unix only) DAWs can subscribe to.
        #[cfg(unix)]
        pub fn connect_virtual(&mut self) -> Result<(), String> {
            use midir::os::unix::VirtualOutput;
            let out = MidiOutput::new(CLIENT).map_err(|e| e.to_string())?;
            let conn = out.create_virtual(PORT_NAME).map_err(|e| e.to_string())?;
            self.conn = Some(conn);
            self.label = Some(format!("virtual: {PORT_NAME}"));
            Ok(())
        }

        #[cfg(not(unix))]
        pub fn connect_virtual(&mut self) -> Result<(), String> {
            Err("virtual MIDI ports aren't supported on this platform — install a \
                 loopback driver (e.g. loopMIDI) and connect to its port instead"
                .to_owned())
        }

        pub fn disconnect(&mut self) {
            if let Some(conn) = self.conn.take() {
                conn.close();
            }
            self.label = None;
        }

        pub fn is_connected(&self) -> bool {
            self.conn.is_some()
        }

        pub fn port_label(&self) -> Option<&str> {
            self.label.as_deref()
        }

        /// Send one event (best-effort: a send error is dropped — the next
        /// sample will resend the changed controller value).
        pub fn send(&mut self, event: &MidiEvent) {
            if let Some(conn) = &mut self.conn {
                let _ = conn.send(event.to_bytes().as_slice());
            }
        }
    }
}

#[cfg(not(feature = "midi-out"))]
mod backend {
    use super::*;

    const DISABLED: &str = "MIDI output disabled (built without the `midi-out` feature)";

    /// No-op stand-in used when the crate is built without `midi-out`.
    pub struct MidiOut;

    impl MidiOut {
        pub fn new() -> Self {
            Self
        }
        pub fn list_ports() -> Vec<String> {
            Vec::new()
        }
        pub fn find_port_index_by_name(_substring: &str) -> Option<usize> {
            None
        }
        pub fn virtual_supported() -> bool {
            false
        }
        pub fn connect_index(&mut self, _index: usize) -> Result<(), String> {
            Err(DISABLED.to_owned())
        }
        pub fn connect_virtual(&mut self) -> Result<(), String> {
            Err(DISABLED.to_owned())
        }
        pub fn disconnect(&mut self) {}
        pub fn is_connected(&self) -> bool {
            false
        }
        pub fn port_label(&self) -> Option<&str> {
            None
        }
        pub fn send(&mut self, _event: &MidiEvent) {}
    }
}

pub use backend::MidiOut;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mpe_init_sets_mcm_and_per_member_bend_range() {
        let m = MidiMapping::default(); // master 0, members 1..=15
        let events = mpe_init_events(&m);

        // MCM on master channel: CC6 carries the member count (15).
        assert!(events.iter().any(|e| matches!(
            e,
            MidiEvent::ControlChange {
                channel: 0,
                controller: 6,
                value: 15
            }
        )));

        // Every member channel gets a pitch-bend-range data entry (CC6 = 48).
        for ch in 1..=15u8 {
            assert!(
                events.iter().any(|e| matches!(
                    e,
                    MidiEvent::ControlChange { channel, controller: 6, value: 48 } if *channel == ch
                )),
                "missing bend-range RPN for channel {ch}"
            );
        }
    }

    #[test]
    fn raised_member_low_still_advertises_a_zone_covering_emitted_channels() {
        // Members 5..=8 with master 0: notes are emitted on channels 5..=8, so
        // the MCM must claim 8 members (zone 1..=8), not 4 (zone 1..=4).
        let mut m = MidiMapping::default();
        m.mpe.member_low = 5;
        m.mpe.member_high = 8;
        let events = mpe_init_events(&m);

        assert!(events.iter().any(|e| matches!(
            e,
            MidiEvent::ControlChange {
                channel: 0,
                controller: 6,
                value: 8
            }
        )));

        // The whole advertised zone gets the bend-range RPN.
        for ch in 1..=8u8 {
            assert!(
                events.iter().any(|e| matches!(
                    e,
                    MidiEvent::ControlChange { channel, controller: 6, value: 48 } if *channel == ch
                )),
                "missing bend-range RPN for channel {ch}"
            );
        }
    }
}
