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
- `%APPDATA%\VISOR\` also holds the log file. Its verbosity is controlled by
  `[log] level` in the config (`"info"` by default).

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
