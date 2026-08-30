# Project VISOR — Design

**Date:** 2026-08-30
**Status:** Approved, pending implementation plan
**Repo:** https://github.com/SirjohnQC/VISOR

---

## 1. Purpose

VISOR is a lightweight Windows background application, written in Rust, that
uses the webcam to decide whether the user is physically at the computer, and
dims and then powers down an OLED monitor when they are not.

The behavioural target is a hardware proximity sensor of the kind ASUS ships
for OLED care: ambient, graduated, and requiring no ritual from the user. You
leave, the panel fades and then goes dark. You come back, it is already on.

v1 ships presence detection, display control, a system tray icon, a TOML
config file, and a log. The action layer is a trait from day one so later
integrations (Discord presence, and whatever follows) plug in without a
rewrite, but none of them are built now.

## 2. Core principles

1. **Fail toward the screen staying on.** Every error path resolves to a lit
   display. A background app that blanks a monitor because a driver hiccuped
   is worse than no app.
2. **The camera is shut unless it is needed.** Presence is inferred from input
   activity first; the webcam opens only once input has gone idle. The LED
   tells the literal truth about when VISOR is looking.
3. **No network.** There is no network code in the v1 binary — not disabled,
   absent. "No images leave this machine" is verifiable from the dependency
   tree rather than trusted from a setting.
4. **No framework, no runtime, no bundled model.** Dependencies are justified
   individually.
5. **A false dim is cheap; a false blackout is expensive.** Thresholds are
   asymmetric throughout, and the dim step exists so VISOR can act early
   without acting destructively.

## 3. Process architecture

Two threads. No async runtime.

```
main thread                          engine thread
───────────                          ─────────────
tray icon + Win32 message pump  <--  Status (atomic: state + health)
overlay windows                  -->  Command channel: Pause/Resume/Reload/Quit
                                     |
                                     +- loop { tick -> sense -> step -> apply }
```

The main thread owns the tray icon and the Win32 message pump and blocks on
nothing else. The engine thread owns idle polling, the camera, face detection,
the state machine, and the display actions, and is free to block on a camera
frame for as long as it needs.

Overlay windows must be created on the message-pump thread, so the
black-overlay display strategy sends its request back to main over a second
channel. DDC/CI and the blank-all broadcast have no such constraint and run
inline on the engine thread.

**Rejected alternatives.** A single-threaded tick loop would stall the tray's
message pump on every camera frame. A Tokio-based design buys nothing for what
is fundamentally a 1 Hz poll loop with one blocking sensor, and costs binary
size and debuggability; when Discord arrives it can own a small runtime inside
its own action module rather than infecting the core.

## 4. State machine

The state machine is a **pure function** with no hardware access and no clock
of its own:

```rust
fn step(&mut self, idle: Duration, face: FaceResult, now: Instant)
    -> (State, Vec<Effect>)
```

### 4.1 States

| State | Camera | Display | Meaning |
|---|---|---|---|
| `Active` | closed | full | Input seen within `idle_grace`. The common case. |
| `Watching` | open @ `sample_interval` | full | Input idle, face still being seen. |
| `Dimmed` | open @ `sample_interval` | dim | No face for `dim_after`. |
| `Away` | open @ `away_sample` | off | No face for `away_after`. |
| `Paused` | closed | full | User pause from tray. |
| `Degraded` | closed | full | Camera failed repeatedly. VISOR stands down. |

### 4.2 Sensor input

```rust
enum FaceResult {
    Face { count: u8, largest_ratio: f32 },  // box height / frame height
    NoFace,
    Unknown,                                 // camera or detector error
}
```

A `Face` whose `largest_ratio` is below `min_face_ratio` is downgraded to
`NoFace` by the first step of `step()` — the threshold lives inside the pure
machine, not in the camera adapter, so it is covered by the unit tests. This is
the intent filter: a face at desk distance fills a predictably large share of
the frame, one across the room does not, so someone walking behind the chair
does not count as presence. It costs nothing — the bounding box is already
returned by the detector.

### 4.3 Miss streak

A single `miss_streak_start: Option<Instant>` drives both reduction steps. It
is set on the first `NoFace` and cleared by:

- `face_confirm` (2) **consecutive** `Face` results — one isolated hit amid
  misses does not reset the timer, which is what stops a single false detection
  from indefinitely postponing sleep;
- a single `Unknown` — cleared immediately rather than after two, because
  `Unknown` is biased toward the fail-safe (§4.7);
- any input.

`dim_after` and `away_after` are both measured from it, so `away_after` must be
greater than `dim_after` (enforced at config load).

### 4.4 Transitions

```
                     any input
      +--------------------------------------------+
      |                                            |
  +---v----+  idle >= idle_grace  +----------+     |
  | Active | -------------------> | Watching |     |
  +---^----+   [open camera]      +----+-----+     |
      |                                |           |
      |                    streak >= dim_after     |
      |                       [display -> dim]     |
      |                                v           |
      |                          +---------+       |
      |                          | Dimmed  |-------+
      |                          +----+----+       |
      |                               |            |
      |                  streak >= away_after      |
      |                      [display -> off]      |
      |                               v            |
      |                          +--------+        |
      +--------------------------|  Away  |--------+
              [display -> full,  +--------+
               close camera]
```

Restores upward (`Dimmed -> Watching`, `Away -> Watching`) are triggered by
`wake_confirm` face hits **or** by any input. Input always wins immediately.

### 4.5 Asymmetric thresholds

Going down requires `face_confirm` (2) consecutive hits to *stay* awake and a
full uninterrupted `away_after` of misses to step down. Coming up requires only
`wake_confirm` (1) hit, because the cost of a wrong wake is a panel that lights
briefly, while the cost of a wrong sleep is a screen dying mid-use.

Worst-case camera-only wake latency is therefore ~1s, which sits below the 1-2s
a panel takes to physically light from DPMS-off. Driving it lower buys nothing
perceptible.

### 4.6 Wake probation

`wake_confirm = 1` means a single spurious detection could restore the display
and then hold it lit for a further `away_after`. So a restore triggered by the
**camera** (not by input) enters probation: if no second `Face` arrives within
`wake_probation`, and no input arrives, the machine returns to the state it
came from and re-applies that state's display level. A real user produces a
second hit within a second or two; a one-frame artifact does not.

The miss streak is **preserved** across probation rather than reset, so an
expired probation resumes the original timeline instead of granting a fresh
`away_after`. Probation is confirmed — and the streak finally cleared — by a
second `Face` (satisfying `face_confirm`) or by any input.

Input-triggered restores never enter probation.

### 4.7 Fail-safe semantics of `Unknown`

`Unknown` — a camera or detector error — breaks the miss streak, so it can
never cause a step down. It does **not** trigger a wake from `Away`, because a
failing sensor is not evidence of return.

Three consecutive `Unknown` results move the machine to `Degraded`, which
**restores the display to full**, closes the camera, and shows a warning on the
tray icon. This is the correct fail-safe: once VISOR cannot tell whether the
user is present, leaving their screen dark is the harmful outcome. The camera
is retried every 5 minutes; a success returns to `Active`.

### 4.8 Effects

```rust
enum DisplayLevel { Full, Dim(u8), Off }

enum Effect {
    OpenCamera,
    CloseCamera,
    SetSampleInterval(Duration),
    SetDisplay(DisplayLevel),
    SetAwakeHold(bool),
}
```

`SetDisplay` is declarative — it names the desired level and lets the display
chain work out how to reach it from wherever the panel currently is. This
avoids a combinatorial set of wake/dim/restore effects.

## 5. Components

Every hardware boundary is a trait, so the entire behaviour of VISOR is
testable with a fake clock and no hardware.

| Trait | Real implementation | Test double |
|---|---|---|
| `Clock` | `Instant::now` | manual advance |
| `IdleSource` | `GetLastInputInfo` | scripted durations |
| `Camera` | WinRT `MediaCapture` + `FaceAnalysis` | scripted `FaceResult`s |
| `DisplayControl` | tiered chain (§6) | recording spy |

### 5.1 Vision

Capture and detection both come from Windows itself — `Windows.Media.Capture`
for frames and `Windows.Media.FaceAnalysis` for detection — reached through the
`windows` crate that VISOR already needs for DDC/CI, the tray, and idle
detection. The entire vision path therefore costs zero additional
dependencies, no bundled model, and no extra DLL.

`FaceAnalysis` is a frontal detector. That is acceptable here, because "facing
the screen" is precisely the question being asked. If it proves too strict at
sharp angles in real use, the first remedy is raising `away_after`, and the
escape hatch is an ONNX detector (YuNet/UltraFace) behind the same `Camera`
trait.

Frames live in one buffer, are handed to the detector, and are dropped. Nothing
is written to disk, encoded, or crosses a process boundary. The detector
returns a count and a bounding-box ratio, not an image.

### 5.2 Idle detection

`GetLastInputInfo`, polled once per second. This is what keeps the camera shut
in the common case: while the user is typing, presence is known without
looking.

## 6. Display control

At startup VISOR enumerates monitors (`EnumDisplayMonitors` ->
`GetPhysicalMonitorsFromHMONITOR`) and probes each configured target with
`GetVCPFeature(0xD6)`. Each monitor is pinned to the best tier that answered.

**Tier 1 — DDC/CI.** Talks to the panel over its own control channel.

- `Full`: `SetVCPFeature(0xD6, 1)`, then `SetVCPFeature(0x10, saved_brightness)`
- `Dim(p)`: `SetVCPFeature(0x10, p% of saved_brightness)`
- `Off`: `SetVCPFeature(0xD6, 4)`

Brightness (VCP `0x10`) is read once at startup and saved, so restores return
the panel to the user's own level rather than a guess.

**Value 4 is used for off, never value 5.** Value 5 is a hard power-off that
many panels cannot be woken from over DDC.

**Tier 2 — black overlay.** A borderless topmost
`WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW` window sized to the monitor's rect,
created on the message-pump thread.

- `Full`: destroy the window
- `Dim(p)`: layered window, alpha proportional to `100 - p`
- `Off`: opaque black

On OLED, black pixels are genuinely off, so this recovers most of the power and
burn-in benefit without any hardware dependency.

**Tier 3 — blank-all broadcast.** `SC_MONITORPOWER`, global and blunt.

- `Full`: broadcast `-1`
- `Dim(p)`: unsupported — no-op, panel stays full
- `Off`: broadcast `2`

Monitors on this tier skip `Dimmed` entirely and step straight from `Watching`
to `Away`.

**Degradation.** A runtime failure demotes that monitor one tier and logs once,
so a panel that turns flaky mid-session degrades rather than breaking.

**Wake belt-and-braces.** When restoring a DDC monitor to `Full`, VISOR also
issues a `SC_MONITORPOWER -1` broadcast. A stubborn panel ignoring DDC-wake is
the one failure mode a user cannot work around themselves.

## 7. Configuration

TOML at `%APPDATA%\VISOR\config.toml`, written with defaults on first run and
reloaded from a tray menu item. No file watcher.

```toml
[presence]
idle_grace      = "30s"   # input idle before the camera opens
sample_interval = "2s"    # frame cadence in Watching / Dimmed
dim_after       = "20s"   # no-face streak before dimming
away_after      = "45s"   # no-face streak before powering off
away_sample     = "1s"    # frame cadence in Away — governs wake latency
face_confirm    = 2       # consecutive hits to stay awake
wake_confirm    = 1       # hits to restore from Dimmed or Away
wake_probation  = "10s"   # camera-only restore must be confirmed within this
min_face_ratio  = 0.15    # face height / frame height; smaller = not at the desk

[camera]
device = ""               # empty = system default

[display]
targets   = []            # empty = all monitors; else match on description
strategy  = "auto"        # auto | ddc | overlay | broadcast
dim_level = 20            # percent of saved brightness
hold_awake_while_present = false

[log]
level = "info"
```

Standing up dims the panel at roughly 50s and blacks it at roughly 75s.

`hold_awake_while_present` asserts `SetThreadExecutionState(ES_DISPLAY_REQUIRED)`
while the user is present, making VISOR the single authority on when the screen
is on and allowing the Windows timeout to be set very short as a safety net.
**It defaults to off.** Until presence detection has proven itself on real
hardware, a false positive would mean a screen that never sleeps — exactly
backwards for a panel we are trying to protect. Revisit after real use.

Validation at load: `away_after > dim_after`, all durations positive,
`min_face_ratio` in `(0, 1)`, `dim_level` in `1..=99`. A config that fails
validation falls back to defaults and logs loudly rather than refusing to start.

## 8. Failure handling

| Failure | Response |
|---|---|
| Camera open fails | `Unknown`; 3 consecutive -> `Degraded` |
| Detector error | `Unknown`; same escalation |
| Config parse/validation error | Defaults, log at `error`, keep running |
| DDC call fails | Demote that monitor one tier, log once |
| All display tiers fail | Log once, remain in current state |
| Monitor hot-unplug | Re-enumerate and re-probe on `WM_DISPLAYCHANGE` |
| System resume | Re-probe DDC; assume `Active` |

## 9. Testing

**Unit — the state machine.** This is where the real coverage goes. Fake clock,
scripted idle durations, scripted `FaceResult`s, spy `DisplayControl`. Every
transition, plus specifically: `Unknown` never causing a step down; hysteresis
absorbing a single dropped frame; a miss streak reset by one late hit; wake
probation expiring back to `Away`; wake probation cleared by a second hit;
`min_face_ratio` downgrading a distant face; `Degraded` restoring the display
from `Away`; pause and resume from every state.

**Integration — Windows adapters.** `#[ignore]` by default, run on demand
against real hardware since they cannot run in CI: DDC probe/sleep/wake
round-trip, brightness save/restore, monitor enumeration, camera open/close.

**Manual smoke checklist.** Walk away and return. Walk away with a video
playing. Sit down without touching input. Have someone walk past behind the
chair. Unplug the webcam mid-session. Sleep and resume the machine. Unplug a
monitor while dimmed.

## 10. Layout

The crate lives at the repository root; there is no nested `visor/` directory.

```
VISOR/
├─ Cargo.toml
├─ docs/superpowers/specs/
└─ src/
   ├─ main.rs           tray, message pump, wiring
   ├─ config.rs
   ├─ logging.rs
   ├─ core/
   │  ├─ machine.rs     the pure state machine
   │  ├─ engine.rs      the tick loop
   │  └─ types.rs       State, Effect, FaceResult, DisplayLevel
   ├─ sense/
   │  ├─ idle.rs        GetLastInputInfo
   │  └─ camera.rs      MediaCapture + FaceAnalysis
   ├─ actions/
   │  ├─ mod.rs         DisplayControl trait + tiered chain
   │  ├─ ddc.rs
   │  ├─ overlay.rs
   │  └─ broadcast.rs
   └─ ui/tray.rs
```

**Dependencies:** `windows`, `serde` + `toml`, `tracing` +
`tracing-subscriber`, `thiserror`, and a tray implementation (the `tray-icon`
crate, or roughly 150 lines of hand-rolled `Shell_NotifyIconW`; decided at
milestone 1).

## 11. Build order

1. **Skeleton** — config, logging, and a tray icon that starts, sits there, and
   quits cleanly.
2. **State machine** — pure, fully tested against fakes. No hardware.
3. **Idle + engine loop** — correct transitions with a stub camera. First point
   the behaviour is observable in the log.
4. **Camera + face detection** — real presence.
5. **Display chain** — DDC first, then overlay, then broadcast. First point
   VISOR does the thing it exists for.
6. **Polish** — `Degraded`, tray status, first-run defaults,
   `WM_DISPLAYCHANGE` handling.

## 12. Known risks

**DDC/CI wake reliability varies by panel and cannot be predicted from here.**
Milestone 5 opens with a throwaway probe against the actual monitor before
committing to DDC as the primary tier. If it cannot be woken reliably, the
overlay tier is promoted to default and DDC becomes opt-in.

**`FaceAnalysis` is frontal-only** and may lose the user at a sharp angle. First
remedy is raising `away_after`; escape hatch is an ONNX detector behind the
same trait.

**VISOR cannot match a hardware proximity sensor's reaction time on the leaving
side.** A dedicated sensor is always on and effectively free; VISOR gates the
camera behind `idle_grace` to keep the webcam shut and the CPU quiet. Coming
back is fast either way (~1s, below the panel's own wake time). `idle_grace` is
the lever if the user wants to trade camera-on time for a faster blank.

## 13. Deliberately excluded from v1

Settings GUI (TOML is hand-edited). Discord presence. Gesture-based wake — the
cheapest confirmed-intent gesture is touching the mouse, which already wakes
instantly, and hand tracking would cost the model weight this design exists to
avoid; `min_face_ratio` captures the underlying intent problem passively.
Motion pre-trigger for cheaper high-rate sampling — a real optimization, but one
to justify with a measurement rather than a guess. Non-Windows platforms.
