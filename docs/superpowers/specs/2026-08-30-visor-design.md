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
| `Away` | open @ `away_sample` | black, panel powered | No face for `away_after`. |
| `Deep` | open @ `away_sample` | panel powered off | No face for `deep_after`. |
| `Paused` | closed | full | User pause from tray. |
| `Degraded` | closed | full | Camera failed repeatedly. VISOR stands down. |

**Why `Away` and `Deep` are separate.** On OLED, an opaque black overlay turns
the pixels genuinely off — it captures effectively all of the burn-in benefit
and most of the power benefit **without power-cycling the panel**, and it
restores instantly because waking is just destroying a window. True DPMS
power-off saves the remaining panel electronics but costs a 1-2s wake and a
power cycle.

That distinction matters more than it first appears. LG OLED panels run
pixel-cleaning/compensation cycles *while powered off*, and those cycles are
interrupted when the power state changes mid-cycle. Blanking the panel 75
seconds after every trip to the kettle would power-cycle it many times a day.
Deferring true power-off to `deep_after` means it happens only on absences long
enough for a compensation cycle to actually complete.

(Evidence caveat: LG documents this behaviour for their OLED **televisions**;
the UltraGear monitor firmware is not confirmed to be identical. It is treated
as strong suspicion, not established fact. Two independent reasons support the
same split regardless: it sidesteps the DDC-wake reliability risk of §12 for
the common case, and overlay restore is faster than DPMS wake.)

`away_sample` governs both `Away` and `Deep`. A slower cadence in `Deep` was
considered and rejected — face detection on a downscaled frame costs a few
milliseconds, so the saving is negligible and does not justify another knob.

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

A single `miss_streak_start: Option<Instant>` drives all three reduction steps. It
is set on the first `NoFace` and cleared by:

- `face_confirm` (2) **consecutive** `Face` results — one isolated hit amid
  misses does not reset the timer, which is what stops a single false detection
  from indefinitely postponing sleep;
- a single `Unknown` — cleared immediately rather than after two, because
  `Unknown` is biased toward the fail-safe (§4.7);
- any input.

`dim_after`, `away_after`, and `deep_after` are all measured from it, so they
must be strictly increasing in that order (enforced at config load).

### 4.4 Transitions

```
                     any input, from any reduced state
      +--------------------------------------------------+
      |             [display -> full, close camera]       |
      |                                                   |
  +---v----+  idle >= idle_grace  +----------+            |
  | Active | -------------------> | Watching |            |
  +---^----+   [open camera]      +----+-----+            |
      |                                |                  |
      |                    streak >= dim_after            |
      |                       [display -> dim]            |
      |                                v                  |
      |                          +---------+              |
      |                          | Dimmed  |--------------+
      |                          +----+----+              |
      |                               |                   |
      |                  streak >= away_after             |
      |                     [display -> black]            |
      |                               v                   |
      |                          +--------+               |
      |                          |  Away  |---------------+
      |                          +---+----+               |
      |                              |                    |
      |                  streak >= deep_after             |
      |                      [display -> off]             |
      |                              v                    |
      |                          +--------+               |
      +--------------------------|  Deep  |---------------+
                                 +--------+
```

Restores upward (`Dimmed`, `Away`, or `Deep` -> `Watching`) are triggered by
`wake_confirm` face hits **or** by any input; input additionally closes the
camera and returns to `Active`. Input always wins immediately. All three
reduced states restore to `Full` in one step — there is no walking back up the
ladder rung by rung.

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
enum DisplayLevel {
    Full,
    Dim(u8),   // percent of the user's saved brightness
    Black,     // pixels off, panel still powered
    Off,       // panel powered down (DPMS)
}

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
| `DisplayControl` | per-operation resolver (§6) | recording spy |

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
`GetPhysicalMonitorsFromHMONITOR`), probes each configured target with
`GetVCPFeature(0xD6)` and `GetVCPFeature(0x10)`, and saves the user's current
brightness so restores return their own level rather than a guess.

### 6.1 Mechanism is chosen per operation, not per monitor

A monitor is **not** pinned to a single tier. Each of the four display levels
resolves its own mechanism independently, because a panel can support one and
not another — and can change its mind at runtime.

The case that forces this: **Windows locks DDC/CI brightness while HDR is on.**
`SetVCPFeature(0x10, ...)` silently does nothing, which on an HDR gaming OLED
would mean the entire `Dimmed` state quietly never happens. Meanwhile
`SetVCPFeature(0xD6, ...)` power control keeps working. Per-monitor tiering
cannot express that; per-operation resolution can.

| Level | Preferred | Fallback |
|---|---|---|
| `Dim(p)` | DDC `0x10` = `p%` of saved, **verified by readback** | layered overlay, alpha `100 - p` |
| `Black` | opaque overlay | — (always available) |
| `Off` | DDC `0xD6` = `4` | opaque overlay (degrades to `Black`), then broadcast |
| `Full` | undo whatever is applied | — |

**The readback is the whole mechanism.** After writing `0x10`, VISOR reads it
back; if the value did not take, it falls through to the overlay for that
application and remembers the result. Doing this on *every* dim rather than once
at startup means toggling HDR mid-session simply works, with no HDR-detection
code anywhere in the codebase.

**`Black` deliberately does not use DDC at all.** It is the short-absence
workhorse, so it must not power-cycle the panel (§4.1) and must restore
instantly.

**`Off` uses VCP value 4, never value 5.** Value 5 is a hard power-off many
panels cannot be woken from over DDC.

**Broadcast is quarantined.** `SC_MONITORPOWER` blanks *every* display, so it
is only ever used when a monitor supports nothing else **and** either it is the
only display attached or the user has explicitly set `strategy = "broadcast"`.
It is never chosen automatically in a multi-monitor setup, where it would blank
panels VISOR was told to leave alone.

### 6.2 Restoring to `Full`

Order matters, because a panel that has just come out of DPMS will reject DDC
traffic for a moment:

1. Destroy any overlay window (instant, and the only step needed from `Black`).
2. If the panel was `Off`: `SetVCPFeature(0xD6, 1)`, plus a `SC_MONITORPOWER -1`
   broadcast as belt-and-braces — a panel that ignores DDC-wake is the one
   failure a user cannot work around themselves.
3. Restore brightness with `SetVCPFeature(0x10, saved)`, retried with backoff
   for up to 2s, since the panel may not accept it immediately after waking.
   Skipped entirely if the dim was done by overlay, in which case step 1
   already restored it.

### 6.3 Degradation

A runtime failure demotes that *operation* on that monitor to its fallback and
logs once, so a panel that turns flaky mid-session degrades rather than
breaking. Mechanism choices are re-probed on `WM_DISPLAYCHANGE` and on system
resume.

## 7. Configuration

TOML at `%APPDATA%\VISOR\config.toml`, written with defaults on first run and
reloaded from a tray menu item. No file watcher.

```toml
[presence]
idle_grace      = "30s"   # input idle before the camera opens
sample_interval = "2s"    # frame cadence in Watching / Dimmed
dim_after       = "20s"   # no-face streak before dimming
away_after      = "45s"   # no-face streak before going black (panel stays on)
deep_after      = "15m"   # no-face streak before true power-off
away_sample     = "1s"    # frame cadence in Away/Deep — governs wake latency
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

Standing up dims the panel at roughly 50s, blacks it at roughly 75s, and powers
it down after about 15 minutes.

`hold_awake_while_present` asserts `SetThreadExecutionState(ES_DISPLAY_REQUIRED)`
while the user is present, making VISOR the single authority on when the screen
is on and allowing the Windows timeout to be set very short as a safety net.
**It defaults to off.** Until presence detection has proven itself on real
hardware, a false positive would mean a screen that never sleeps — exactly
backwards for a panel we are trying to protect. Revisit after real use.

Validation at load: `dim_after < away_after < deep_after`, all durations
positive, `min_face_ratio` in `(0, 1)`, `dim_level` in `1..=99`. A config that
fails validation falls back to defaults and logs loudly rather than refusing to
start.

## 8. Failure handling

| Failure | Response |
|---|---|
| Camera open fails | `Unknown`; 3 consecutive -> `Degraded` |
| Detector error | `Unknown`; same escalation |
| Config parse/validation error | Defaults, log at `error`, keep running |
| DDC dim write does not read back | Fall through to overlay dim, log once |
| DDC power call fails | Demote that operation to overlay, log once |
| All mechanisms for a level fail | Log once, remain in current state |
| Monitor hot-unplug | Re-enumerate and re-probe on `WM_DISPLAYCHANGE` |
| System resume | Re-probe DDC; assume `Active` |

## 9. Testing

**Unit — the state machine.** This is where the real coverage goes. Fake clock,
scripted idle durations, scripted `FaceResult`s, spy `DisplayControl`. Every
transition, plus specifically: `Unknown` never causing a step down; hysteresis
absorbing a single dropped frame; a miss streak reset by one late hit; wake
probation expiring back to `Away`; wake probation cleared by a second hit;
`min_face_ratio` downgrading a distant face; `Degraded` restoring the display
from `Deep`; the full ladder `Watching -> Dimmed -> Away -> Deep` on one
unbroken streak; restore to `Full` in a single step from each of the three
reduced states; pause and resume from every state.

**Integration — Windows adapters.** `#[ignore]` by default, run on demand
against real hardware since they cannot run in CI: DDC probe/off/wake
round-trip, brightness save/restore, the `0x10` readback correctly reporting
failure with HDR on, mechanism resolution picking overlay-dim under HDR while
keeping DDC-off, monitor enumeration, camera open/close.

**Manual smoke checklist.** Walk away and return. Walk away with a video
playing. Sit down without touching input. Have someone walk past behind the
chair. Unplug the webcam mid-session. Sleep and resume the machine. Unplug a
monitor while dimmed. **Toggle Windows HDR on and off while dimmed** — the dim
must survive the transition by switching mechanism. Leave for 15+ minutes and
confirm the panel actually powers down and comes back.

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
   │  ├─ mod.rs         DisplayControl trait + resolver
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
5. **Display control** — overlay first (it is the `Black` workhorse and has no
   hardware dependency, so it makes VISOR useful on any monitor immediately),
   then DDC for `Dim` and `Off`, then the quarantined broadcast fallback. First
   point VISOR does the thing it exists for.
6. **Polish** — `Degraded`, tray status, first-run defaults,
   `WM_DISPLAYCHANGE` handling.

## 12. Known risks

**DDC/CI wake reliability varies by panel and cannot be predicted from here.**
Largely defused by the `Away`/`Deep` split — DDC power-off now only happens on
long absences, and the common case never leaves the overlay. Milestone 5 still
opens with a throwaway probe against the real monitor; if it cannot be woken
reliably, `Deep` is disabled by setting `deep_after` to infinity and VISOR
still delivers everything except the last few watts.

**The reference hardware is an LG UltraGear OLED (GX7).** Two of its
characteristics shaped the design and should be re-checked on other panels:
DDC brightness is locked while Windows HDR is on (handled by the readback in
§6.1), and pixel-cleaning cycles run while powered off (handled by the
`Away`/`Deep` split in §4.1). Neither assumption is load-bearing — a panel that
behaves differently simply resolves to different mechanisms.

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
