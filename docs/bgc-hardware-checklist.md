# Background Pen Capture — hardware validation checklist (B7)

The defining acceptance test for the Raw Input rebuild (SPEC_BGC §2, §12). Run on
**Windows + a real Wacom tablet**, with **"Use Windows Ink" ON** in Wacom Tablet
Properties (SPEC_BGC §9). This is a manual checklist; record results inline.

## Setup

1. Connect the Wacom tablet; confirm the driver is installed and **"Use Windows
   Ink" is ON**.
2. Build: `cargo build` (Raw Input is the default Windows backend).
3. Start the capturer streaming over TCP so a separate consumer can read it:
   ```powershell
   cargo run -p tablet-cli -- --transport tcp
   ```
   Expect a log line: `raw-input capture thread entering message loop`.
4. In another terminal, start the visualizer/consumer (`tablet-ui`) pointed at the
   same TCP address.

| Env field | Value |
|-----------|-------|
| Date / tester | |
| Tablet model | |
| Wacom driver version | |
| Windows build | |

---

## 1. Background capture (the defining test)

- [ ] Give a **DAW (or any non-capture app)** the foreground focus.
- [ ] Draw on the tablet. Confirm `tablet-ui` shows **live samples** while the DAW
      stays foreground (the capturer never gets focus).
- [ ] `packets_per_sec` in the capturer's metrics is non-zero while drawing.

> Result:

## 2. Axis fidelity

- [ ] **X/Y** track at full logical resolution (raw values span the device's
      logical min/max, far finer than screen pixels).
- [ ] **Pressure** spans its full range from light touch to hard press.
- [ ] **X/Y tilt** changes as the pen is tilted (or tilt derived from
      azimuth/altitude on devices that report orientation).
- [ ] **Twist** changes when the pen is rotated (if the device supports it).
- [ ] **Tip / barrel / eraser switches** map to `buttons` bits 0 / 1 / 2.
- [ ] Flipping to the **eraser** end yields `ToolKind::Eraser`.

> Result:

## 3. Proximity & hot-plug

- [ ] Lifting the pen out of range / bringing it back emits `Proximity` events
      with the correct `in_range`.
- [ ] **Unplug** the tablet → on the active tool, a leaving-proximity event fires;
      the cache entry is dropped.
- [ ] **Replug** the tablet → `Capabilities` is re-emitted (device arrival).

> Result:

## 4. Windows-Ink-OFF regression

- [ ] Turn **"Use Windows Ink" OFF** in Wacom Tablet Properties.
- [ ] Restart the capturer. Confirm it fails with the **actionable diagnostic**
      (`No tablet device found … enable "Use Windows Ink" …`) — **no silent stall,
      no panic**.
- [ ] Turn Windows Ink back ON; confirm capture resumes.

> Result:

## 5. Latency / loss sanity

- [ ] Sustained drawing shows **no growth in `dropped`** under normal load.
- [ ] Responsiveness feels immediate (capture→transport target < ~2 ms;
      `GetRawInputBuffer` batches under bursts).

> Result:

---

## Acceptance summary

- [ ] Live samples received while a different app is foreground.
- [ ] All supported axes/switches read correctly; eraser → `Eraser`.
- [ ] Proximity and hot-plug events fire; capabilities re-emit on arrival.
- [ ] Windows-Ink-off produces the diagnostic, not a silent stall or panic.
