# VISOR

VISOR is a lightweight Windows background application that uses your webcam
to decide whether you are physically at the computer, then dims and
eventually powers down the display when you are not — the way an ASUS OLED
proximity sensor behaves, but ambient and requiring no ritual. Walk away and
the panel fades and then goes dark; come back and it is already on.

Presence is inferred from keyboard/mouse activity first. The webcam only
opens once input has gone idle, so the camera's LED tells the literal truth
about when VISOR is looking, and it closes again the moment you touch the
keyboard or mouse.

VISOR has no network code at all — not disabled, absent — so nothing it sees
ever leaves the machine.

## The state ladder

| State | Camera | Display | Enters after (default) |
|---|---|---|---|
| `Active` | closed | full | input seen within the last `idle_grace` (30s) |
| `Watching` | open | full | input idle for `idle_grace` (30s), face still seen |
| `Dimmed` | open | dimmed to `dim_level`% of saved brightness | no face for `dim_after` (20s) |
| `Away` | open | black (pixels off, panel still powered) | no face for `away_after` (45s) |
| `Deep` | open | panel powered off (DPMS) | no face for `deep_after` (15m) |
| `Paused` | closed | full | you chose Pause from the tray menu |
| `Degraded` | closed | full | the camera failed 3 times in a row |

With the defaults, standing up dims the panel at roughly 50 seconds after you
last moved the mouse, blacks it at roughly 75 seconds, and powers it down
after about 15 minutes. Any input — or, from a reduced state, a confirmed
face — restores to full brightness immediately; there is no walking back up
the ladder one rung at a time. `Degraded` shows a warning-coloured tray icon
and retries the camera every 5 minutes.

## Config and log

Both live in `%APPDATA%\VISOR\`:

- `%APPDATA%\VISOR\config.toml` — written with defaults the first time VISOR
  runs. Edit it, then choose **Reload config** from the tray menu (or trigger
  a display change / system resume — see below) to pick up the change without
  restarting.
- `%APPDATA%\VISORisor.log.YYYY-MM-DD` — the log, rotated daily, so look
  for today's date rather than a bare `visor.log`. Verbosity is controlled by
  `[log] level` in the config (`"info"` by default). Setting it to `"debug"`
  adds one line per camera probe showing exactly what the detector saw, which
  is the fastest way to answer "why didn't it dim?".

A config that fails to parse or fails validation (for example
`dim_after >= away_after`, an out-of-range `dim_level`, or a non-positive
duration) falls back to the built-in defaults and logs the failure at `error`
— VISOR keeps running rather than refusing to start.

### Enabling `hold_awake_while_present`

Set, in `config.toml`:

```toml
[display]
hold_awake_while_present = true
```

When on, VISOR asserts `SetThreadExecutionState(ES_DISPLAY_REQUIRED)` for as
long as you are `Active` or `Watching`, making VISOR the single authority on
when the screen sleeps — which lets you set Windows' own screen-off timeout
very short as a safety net, since VISOR is what's actually deciding when the
panel goes dark.

**It defaults to off.** Presence detection has not yet been proven on real
hardware over long sessions, and a false "you're still here" would mean a
screen that never sleeps — exactly the wrong failure direction for a panel
VISOR exists to protect. Turn it on once you trust your own setup.

## Your first run

Start `visor.exe`. Nothing visible happens except a tray icon — that is
correct; VISOR does nothing at all while you are using the machine.

Open today's log and read the first three lines. They tell you everything
about how VISOR will behave on your hardware:

```
INFO visor: VISOR starting cfg=Config { ... }
INFO visor::actions: display target monitor=\\.\DISPLAY1 ddc=false brightness=false
INFO visor::ui::tray: tray icon created; VISOR is running
```

`ddc` and `brightness` are the ones that matter. See **How your monitor is
driven** below for what each combination means.

Then choose **Check camera** from the tray menu while sitting normally. That
is the one thing worth doing before you trust VISOR to dim on you, because a
camera that cannot see you fails silently and looks exactly like working
correctly until your screen goes dark in your face.

A sensible first session: leave the defaults, work as usual, and check the log
afterwards for `state` lines. If VISOR dimmed while you were sitting there,
`Check camera` will tell you why in one sentence.

## How your monitor is driven

VISOR picks a mechanism per operation, not per monitor, because the three
things it wants to do have different best answers (spec §6.1):

| Log line | Dimming | Powering off (`Deep`) |
|---|---|---|
| `ddc=true brightness=true` | real backlight dimming over DDC/CI | DDC/CI power command |
| `ddc=true brightness=false` | black overlay at partial opacity | DDC/CI power command |
| `ddc=false` | black overlay at partial opacity | `SC_MONITORPOWER` broadcast |

`brightness=false` on a DDC-capable monitor almost always means **Windows HDR
is on** — Windows silently ignores DDC brightness writes while it is. VISOR
detects this by reading the value back after every write rather than trusting
the call, so it falls through to the overlay in the same tick and you never
see a missed dim.

Overlay dimming is not backlight dimming: it lays a partially transparent
black window over the screen. On an OLED that is nearly as good, since the
pixels themselves are what draw power and what burn in.

DDC/CI is probed when VISOR starts and again on every display change or
**Reload config**. It is occasionally flaky — a driver can refuse
`GetPhysicalMonitorsFromHMONITOR` for a while, particularly after a VISOR was
killed outright rather than quit — and a probe that fails means VISOR spends
that run in overlay mode. If the log says `ddc=false` on a monitor you know
speaks DDC/CI, **Reload config** re-probes without restarting.

`SC_MONITORPOWER` is a Windows-wide broadcast, so VISOR **refuses to use it
automatically when more than one monitor is attached** — it would blank all of
them. With several monitors and no DDC, `Deep` degrades to the same black
overlay as `Away`. Set `strategy = "broadcast"` in `[display]` to override
that if blanking everything is what you want.

### The pointer on a black screen

While the overlay is *dimming* it is click-through: you may well still be at
the desk, and it must not eat your clicks. Once it goes fully black it stops
being click-through, because owning hit-testing is the only way to stop
Windows drawing a mouse pointer on top of an otherwise black screen — the
cursor is composited above every window, so no amount of painting can cover
it.

That means a fully black overlay does swallow mouse clicks. Moving the mouse
is itself what ends `Away`, so the window is gone within a tick (`away_sample`,
1s by default); the exposure is the first click of a return, and only if you
click without moving first.

## Checking that the camera can actually see you

Choose **Check camera** from the tray menu. VISOR samples ten frames over about
two seconds and puts the verdict in the tray tooltip (and the log), where it
stays for twelve seconds.

This exists because every way presence detection can fail looks identical from
the outside — a covered lens, a camera pointed at the ceiling, another app
holding the device, or simply sitting further away than `min_face_ratio`
allows all produce exactly one visible symptom: VISOR dims on you as though the
room were empty. The check tells the four apart:

| Verdict | Meaning |
|---|---|
| *Camera sees you* | A face was detected and clears `min_face_ratio`. |
| *Face seen but too small* | It can see you, but at a smaller share of the frame than `min_face_ratio` requires, so the state machine counts you as away. The message suggests a specific lower value to put in the config. |
| *Camera works, but no face was detected* | Frames are arriving; nothing that looks like a face is in them. Check the aim and the lighting. |
| *Camera unavailable* | No usable frame at all — covered, unplugged, in use by another app, or blocked by Windows camera privacy settings. |

Use it to tune `min_face_ratio` for how you actually sit: run it from your
normal working position, and if it reports *too small*, either move closer or
take the suggested value.

Checking from `Active` opens the camera only for the length of the check and
closes it again afterwards, so it does not weaken the rule that the lens stays
shut while you are present.

## Monitor requirements: DDC/CI

Dimming and true power-off go through DDC/CI (VCP feature codes over the
monitor's data channel) when the monitor supports it and confirms the write.
**DDC/CI must be enabled in the monitor's on-screen menu** — it ships off on
many panels. If it's off, or the monitor doesn't support it, or a DDC call
isn't confirmed, VISOR automatically falls back to a black overlay window
instead: pixels-off rather than true power-off. `Black` (the `Away` state)
always uses the overlay regardless of DDC support, since an overlay restores
instantly and never power-cycles the panel.

## HDR and brightness

A monitor with Windows HDR enabled will often silently ignore or misreport a
DDC brightness (VCP `0x10`) write. VISOR does not trust the write blindly: it
reads the brightness back immediately after setting it, and if the readback
doesn't confirm the new value, it treats that monitor as not
brightness-capable for this operation and falls through to the black-overlay
dim instead — in the same call, so you never see a missed dim. This is
re-evaluated on every dim, so toggling HDR off later (letting DDC brightness
work again) is picked up the next time VISOR dims, without a restart.

## Display changes and system resume

VISOR reacts to two Windows broadcasts:

- **`WM_DISPLAYCHANGE`** (a monitor was connected, disconnected, or changed
  mode) — VISOR re-enumerates monitors and rebuilds its DDC handles.
- **`WM_POWERBROADCAST` / `PBT_APMRESUMEAUTOMATIC`** (the system resumed from
  sleep) — VISOR re-probes DDC and resets to `Active`.

Both are handled the same way under the hood: they re-scan the display
targets and reset the engine exactly as **Reload config** does. There is no
file watcher — reloading only ever happens from the tray menu, a display
change, or a resume.

## Running the hardware tests

Most of VISOR's behaviour is covered by the unit test suite (`cargo test
--lib`), which runs against fakes and needs no hardware. A handful of
integration tests instead exercise real Windows adapters and are `#[ignore]`d
by default because they cannot run in CI:

```
cargo test --test hardware -- --ignored --nocapture --test-threads=1
```

This runs, in order: a face-detection test that needs a webcam with you in
front of it; a DDC brightness save/round-trip test that needs a DDC/CI-capable
monitor; and a DDC power off/on round-trip test.

**Warning:** the power round-trip test (`power_off_and_on_round_trips`)
physically turns your monitor off via DDC/CI and then tries to turn it back
on five seconds later. If your monitor doesn't reliably wake from a DDC power
command, this test can leave the panel dark with no VISOR-side way to
recover it — you would need to fix it at the monitor (power button / input
select). Run it only when you can physically reach the monitor, and only on
a monitor you don't mind testing this on. `--test-threads=1` keeps these
tests from clobbering each other's monitor handles.

There is also one ignored, eyes-on test in the library itself
(`cargo test --lib overlay_is_visible_by_eye -- --ignored --nocapture`) that
cycles the overlay through dim, black, and full so a human can visually
confirm it.

## What VISOR does not do (yet)

VISOR's action layer is a trait so future integrations (for example, Discord
presence) can plug in without a rewrite, but nothing beyond display control
and the tray icon is built in v1. There is no file watcher for the config —
reload is manual (tray menu) or triggered by the display-change/resume
broadcasts above.
