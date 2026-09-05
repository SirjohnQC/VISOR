# VISOR tuning window — design specification

The reference this window is built against. Colour tokens live in
`src/ui/theme.rs` and are not repeated here; everything else that governs the
window is below, because it existed nowhere but a conversation transcript
until this file.

Canvas of the five states:
<https://claude.ai/code/artifact/6d5132ce-c86b-4250-b9f3-da3dd85b7423>

---

## 1. The governing rule

**Instrument, not app.**

> The only saturated pixels in the window are the ones that carry the
> measurement.

No brand accent, no coloured buttons, no gradient hero. Wordmark, buttons,
labels and diagnostics are all neutral grey. Chroma is spent exclusively on the
face-ratio state, the Degraded warning, and the focus ring. That is what makes
a glance work: if there is colour on screen it is telling you about your face
ratio, and nothing else ever will.

Three consequences that come from the product rather than from taste:

1. **The preview renders in luminance.** It says *we are measuring shape, not
   looking at you*, it makes any webcam in any lighting look deliberate, and it
   guarantees the detection overlay's colour is unambiguous. No toggle.
2. **The timing rail is a picture of the light.** The bar's fill brightness
   literally *is* the screen brightness at that moment on the timeline. Drag
   `dim_level` and the segment visibly darkens. The setting and its depiction
   are the same object.
3. **The video plate stays dark in every theme.** Video on a light ground
   glares and the letterbox bars read as a bug.

---

## 2. Type scale

Segoe UI Variable Text → Segoe UI → system UI. Display optical size ≥ 28px,
Text 13–20px, Small ≤ 12px. Nothing needs downloading.

| Level | Size / Line | Weight | Tracking | Use |
|---|---|---|---|---|
| Numeral | 34 / 36 | 300 | −1% | The live `0.24 / 0.15` fraction |
| Title | 20 / 26 | 600 | 0 | State name |
| Body | 13 / 18 | 400 | 0 | Verdict sentence, buttons |
| Body strong | 13 / 18 | 600 | 0 | Inline values, the `20%` readout |
| Caption | 12 / 16 | 400 | 0 | Sub-line, diagnostics, chips |
| Micro | 10.5 / 13 | 400 | +2% | Ticks, marker names, units |
| Section | 11 / 14 | 600 | +8% caps | "SEQUENCE", "DISPLAY" |
| Wordmark | 13 / 16 | 700 | +14% caps | "VISOR" |

**Two non-negotiables.** Tabular figures (`DWRITE_FONT_FEATURE_TAG_TABULAR_
FIGURES`) on every number that changes — without them the 34px ratio visibly
wobbles as digit widths swap and the calm-instrument premise dies in the first
second. And uniform line spacing per format, so rows land on the grid
regardless of font fallback.

Cache exactly 8 `IDWriteTextFormat` objects for the window's life.

---

## 3. Layout

420 × 696 logical px, **not resizable** — a deliberate cost decision: a fixed
window means no scrollbar control ever has to exist and every position is a
constant. Margin `M = 22`; content column x 22 → 398. Vertical rhythm on 4.
Radii: plate 10, buttons 6, rail track 6, gauge track 10, chips pill.

Hairlines are 1 *device* pixel: stroke at `1/scale` DIP offset by `0.5/scale`,
or they blur at 125%/150%.

| y | h | Block |
|---|---|---|
| 0 | 40 | Title bar (hairline at 39.5, full bleed) |
| 52 | 44 | Status band |
| 108 | 240 | Preview plate (320w) + gauge column (44w at x354) |
| 360 | 48 | Readout (numeral 360–390, verdict 392–408) |
| 424 | 16 | Section label "SEQUENCE" |
| 448 | 86 | Sequence rail block |
| 542 | 28 | Dim-level row |
| 586 | 16 | Section label "DISPLAY" |
| 606 | 36 | Diagnostics (two 18px lines) |
| 654 | 1 | Hairline, full bleed |
| 656 | 40 | Footer actions (buttons 28 tall at 662–690) |

Rail block internal lanes, relative to y448: marker labels 0–14, handle lane
14–28 (centres y21), track 28–56, axis ticks 56–70, summary 72–86.

---

## 4. Control inventory

Six types. That is the budget. Explicitly **not** introduced: scrollbar, text
input, dropdown, checkbox/toggle, tabs, accordion, hover tooltips (every hover
explanation is instead a permanent inline caption).

**Page one — the instrument** edits the six values that fail silently:
`min_face_ratio`, `idle_grace`, `dim_after`, `away_after`, `deep_after`,
`dim_level`. Nothing else ever goes on this face.

**Page two — settings** edits the other eight: `sample_interval`,
`away_sample`, `face_confirm`, `wake_confirm`, `wake_probation`,
`hold_awake_while_present`, `strategy`, `theme`.

This revises an earlier ruling in this file, which said adding
`hold_awake_while_present` would cost a whole toggle control for one boolean,
so don't. The objection was right and the conclusion was wrong: the cost is not
one toggle, it is a *page*, and a page pays for all eight at once. Three things
made it affordable, and all three are load-bearing —

1. **The window never resizes.** Page two is 420×696 like page one, so the
   fixed-layout premise survives and no scrollbar is needed. The size is what
   the ban on scrollbars actually rests on; the page count is not.
2. **The instrument face gains one ghost button**, in the footer, which is a
   C4 that already existed. No tab strip, no accordion — those were banned
   because they put chrome on the instrument, and this puts none there.
3. **Every setting on page two is a small closed set of sensible values.**
   Nobody wants `sample_interval = 2.7s`. So all eight are one control, C6,
   and there is no dropdown, checkbox or radio anywhere: three banned types
   replaced by one, with nothing hidden behind a collapsed menu.

Implemented in `src/ui/settings.rs` — pure, like `controls.rs`, and holding the
one table that both the painter and the hit test walk.

### C0 — Marker (shared primitive)

Both the rail and the gauge are built on one draggable marker: shared
hit-testing, capture, keyboard and animation. Write it once.

- **Rest** — fill `t1`, 2px contour of `bg` so it reads over any track fill.
- **Hover** — grows 2px over 90ms; cursor `IDC_SIZEWE` / `IDC_SIZENS`.
- **Active** — stem 2px → 3px, value bubble above; mouse captured; ESC cancels
  and restores the pre-drag value.
- **Focus** — 2px ring, 3px offset, **only when keyboard focus is visible**
  (track `WM_UPDATEUISTATE` / `UISF_HIDEFOCUS` so a click leaves no ring).
- **Disabled** — fill `t4`, no hit.

Hit rect 44×44 logical regardless of drawn size. Arrows = one snap step,
Ctrl+arrow = fine, Home/End = axis extremes, PageUp/Down = 5 steps.

### C1 — Rail
Horizontal axis carrying 1–4 markers. Parameterised by scale (log or linear),
snap set, ticks, optional segment painter. Two instances: the sequence rail and
dim-level. Hovering the *track* shows a ghost line with a Micro label — a free
preview of where a click would land; clicking animates the nearest marker there
over 180ms.

### C2 — Gauge
The vertical face-ratio instrument. One instance. See §5.

### C3 — Plate
The video surface with overlays. Non-interactive except one embedded C4.

### C4 — GhostButton
Text-only, 28 tall (20 in-plate), padding 12, radius 6. Rest: no fill, no
border. Hover: fill `hair`. Active: fill `strong`, **no offset** — this is a
calm UI. Danger variant (Quit only): hover border and text go `danger`.

### C5 — Chip
Non-interactive pill, height 20, radius 10, optional 6px leading dot. Camera
status, display mechanism, forced-strategy note.

### C6 — Choice
A row of 2–4 segments sharing the content column evenly, 24 tall, radius 6,
6px gaps. Unselected: `well` fill, Body, `t2`. Selected: `strong` fill, a 1px
`hair` outline, Body strong, `t1`. Eight instances, all on page two, and the
only control type that page has.

No chroma, by the governing rule — none of these eight settings is a
measurement. Selection is carried by fill *and* text weight so it survives high
contrast, where a fill difference alone would not.

**A value the config holds that the row does not offer lights nothing.** Not
the nearest neighbour: showing `2s` selected while the file says `4s` would be
a lie, and clicking to "fix" it would overwrite something a user typed on
purpose. The caption instead gains ` — config says 4s`, so a hand-tuned row
reads as hand-tuned rather than as broken.

Clicking the segment already lit does nothing at all. Every other click writes
`config.toml` immediately and posts a `Reload`; there is no Apply button,
because a settings page with an unsaved state is a settings page that can lose
your change.

### Page two layout

Same 420×696, same title bar, same footer hairline at y654. Content runs y52 →
y654 as three sections of rows. A row is 58 tall: label 0–15, caption 16–31,
segments 34–58. Sections are 16 tall with 6px under them; rows are 6px apart,
14px before the next section.

| y | Block |
|---|---|
| 52 | `W A T C H I N G` |
| 74 / 138 / 202 | `sample_interval` · `away_sample` · `face_confirm` |
| 274 | `W A K I N G` |
| 296 / 360 / 424 | `wake_confirm` · `wake_probation` · `hold_awake_while_present` |
| 496 | `D I S P L A Y` |
| 518 / 582 | `strategy` · `theme` |
| 662 | `← Back`, and a Caption saying the page saves as you click |

Every one of those numbers lives once, in `settings::BLOCKS`, which the painter
and the hit test both walk. A layout test proves the blocks neither overlap nor
run past the hairline — with no scrollbar, a row that does not fit is a row
that is simply unreachable.

**Two settings take effect on the pump thread, not in the engine**: `theme` is
the window's own palette, and `strategy` belongs to the `Resolver`, which
ruling F8 keeps on this side of the channel. `Resolver::reconfigure` exists for
the second one — a plain `rescan` re-probes with the strategy the `Resolver`
was *built* with, so the setting would have saved and then done nothing until
the next start.

---

## 5. The face-ratio interaction

### The plate
**Letterboxed, never cropped.** This is correctness, not aesthetics:
`largest_ratio` is `FaceBox.Height / frame.PixelHeight`, so cropping a 16:9
frame into a 4:3 plate changes the effective frame height and the ratio drawn
stops matching the ratio measured.

Overlay is **four corner brackets**, not a rectangle — brackets read as
viewfinder, a closed rectangle reads as surveillance, which matters for a
product whose pitch is privacy. Each stroke gets a 1px black 50% contour first
so it survives a blown-out wall behind the user's head. Plus a **caliper** on
the box's left edge spanning exactly the box height, with no number on it: it
exists purely to make the eye connect "this height" to the gauge 12px away.

**Smoothing** is the difference between instrument and twitchy:
- drawn box: α = 0.35 per detection
- displayed numeral: α = 0.25, then hundredths with a one-hundredth dead band
- envelope: **raw** values — its job is to catch the dips smoothing hides

Rates while visible: frames 15 fps, detection 5 Hz. Both stop when hidden.

### The gauge
44 wide, aligned to the **video extent**, not the plate, so the mapping stays
honest. Linear 0.00 bottom → 0.60 top (real faces live in 0.05–0.45; 0.60 gives
headroom without wasting the column). Ticks every 0.10, stronger at 0.30, **no
numeric labels** — the numbers live in the readout 20px away.

Elements bottom-to-top: fill to the measured value; a 2px instant cap plus a
triangle pointing in; a 28-wide envelope band at 28% alpha behind the fill
spanning min–max; a full-opacity tick at the envelope **minimum** with a Micro
`low 0.19` label — *this is the number the user should be watching*, and the
design says so by giving it its own tick; and the threshold marker, a 1.5px
neutral line with a 32×10 grip handle.

### Reading measured against threshold
One 34px fraction: `0.24 / 0.15`, left number in the state colour, `/` and
right in `t3`. A fraction is right because the question is literally a
comparison. Below it, one Body sentence saying the **consequence**, never the
mechanism.

| State | Gauge | Plate | Verdict |
|---|---|---|---|
| **Good** (env min ≥ thr × 1.15) | solid `good` fill | brackets `good` | `● Clear by 43% — no dips below in 14 s.` |
| **Marginal** (≥ thr, < ×1.15) | solid `marginal` | brackets `marginal` | `Only 6% above the line — one lean back and VISOR will think you left.` + `Use 0.13` |
| **Below** (measured < thr) | fill to measured; the gap measured→threshold filled with a 45° hatch | brackets `below` **plus a dashed ghost rectangle at the required size**, Micro `needed` | `Too small — VISOR would treat you as away.` + `Use 0.11` |
| **No face** | empty track; dotted line at last known height fading over 3s | video, no brackets, Micro `no face` | `Camera is running but sees no face. Check the angle and the lighting.` |
| **Unavailable** | track `dead`, threshold still draggable at 60% | slashed-camera glyph + reason + `Retry camera` | (reason lives in the plate) |

The **deficit hatch** is the best thing in this design: it draws the exact
quantity of the problem as an area, on the same scale as the measurement, next
to a dashed box showing that same shortfall in physical terms. Two views of one
quantity, side by side.

### Confirmation
After any threshold change the envelope resets and a 10s window opens: the
threshold line goes amber, a 2px underline grows under the verdict, and the
copy reads `Checking — move around, lean back, turn your head.` **A dip below
the threshold resets it to zero.** You cannot confirm a bad setting by sitting
perfectly still, because the thing being tested is whether you can *move*
without VISOR losing you. On completion: `● Confirmed — no dips below in 10 s.
Lowest 0.19.`

Implemented in `src/ui/signal.rs`.

### The camera-closed state
`min_face_ratio` cannot be tuned if touching the mouse closes the camera, and
the machine closes it whenever `idle < idle_grace` — exactly the situation a
user dragging a slider is in. Solved with a `preview_hold` flag owned by the
window:

- **Opening the window does NOT open the camera.** Privacy default. The plate
  says `Camera is closed`, `VISOR opens it only after 30 s without keyboard or
  mouse.`, and offers `Turn on preview`.
- While held: camera stays open regardless of input, and **VISOR does not act
  on what it sees** — no dimming, no transitions.
- A 24-tall scrim on the plate's bottom edge, always visible while held:
  `Tuning — VISOR will not dim while the preview is on. Nothing leaves this PC.`
- The title-bar chip tracks reality always, agreeing with the hardware LED.
- Force-cleared when the window hides. No path leaves the camera held open
  behind an invisible window.

---

## 6. The timing ladder

**The insight it rests on** (verified in `machine.rs`): `rung_for(streak)`
compares **one** miss-streak duration against all three thresholds. They are
three marks on a single timeline, not sequential countdowns — which also makes
`dim < black < off` a geometric fact rather than a validation error.

So: **one rail, one axis — "time since your last keypress" — four markers.**

- Marker 1 is `idle_grace`, absolute, labelled `camera opens`.
- Markers 2–4 are `dim_after` / `away_after` / `deep_after`, drawn at absolute
  positions `idle_grace + value` but **edited and labelled as relative values**,
  matching the TOML exactly.
- Dragging marker 1 **translates 2–4 rigidly**, because they are relative.
- Markers 2–4 hard-clamp against each other at one snap step. No push, no
  cascade: simpler, and never surprising.

### Log axis

```
t(p) = 5 · 720^p     p ∈ [0,1]  →  5 s … 60 min
p(t) = ln(t/5) / ln(720)
x    = 29 + p × 362        (7px inset each end so handles never clip)
```

One pixel is 1.8% of the value everywhere — nobody wants 1-second precision at
15 minutes, everybody wants it at 20 seconds.

Defaults: `idle_grace` 30s → x128; `dim_after` 20s (abs 50s) → x156;
`away_after` 45s (abs 75s) → x178; `deep_after` 15m (abs 930s) → x316.

**Snap set** (on the relative value): 5, 10, 15, 20, 30, 45s; 1, 2, 3, 5, 10,
15, 20, 30, 45, 60m. Magnet 7px. Ctrl bypasses; free values quantise to 1s
below 2m and 15s above.

**Ticks**: `5s` x29, `10s` x67, `30s` x128, `1m` x166, `5m` x254, `15m` x315,
`1h` x391.

**Handle collision**: at defaults dim and away are 22px apart and visually
overlap, which is *true*; drag capture means a grabbed marker stays grabbed.
The value *labels* collide-resolve instead, collapsing to `dim 20s → black 45s`.

### The segmented track

| Segment | Fill |
|---|---|
| 22 → x_idle | `level_full` — screen on, camera closed |
| x_idle → x_dim | `level_full` + 6% white, with a `good` dot on the top edge at x_idle — camera **open** |
| x_dim → x_away | `dim_fill(dim_level)` |
| x_away → x_deep | `level_black` |
| x_deep → 398 | `level_black` + 1px outline + 45° 8% hatch + Micro `off` — distinguishes "black" from "powered down" without colour |

**Playhead**: 2px line at the current elapsed time with a triangle above, shown
only in Watching/Dimmed/Away/Deep, updated at 4 Hz via a dirty rect on the
y462–504 strip only. Do not repaint the window at 4 Hz when the camera is shut.

**Summary line**: `Dims 50 s after you stop typing · black at 1:15 · off at 15:30`

**Suspended** (Paused, Degraded): rail to 45% opacity, playhead hidden,
Degraded additionally hatched. Two treatments, not a caption.

### dim_level
Second Rail instance. Linear 1–99, 8px track, fill is a left-to-right sRGB
gradient from `level_black` to `level_full` — you pick a brightness by pointing
at it. Snap 5%, Ctrl for 1%.

---

## 7. State mapping

| `State` | Dot | Name | Sub-line |
|---|---|---|---|
| `Active` | `good` | Active | `You're here — camera closed.` |
| `Watching` | `t1` | Watching | `Camera open, watching for absence.` |
| `Dimmed` | `t2` | Dimmed | `Screen at 20%.` |
| `Away` | `t2` | Away | `Screen black, panel still powered.` |
| `Deep` | `t2` | Deep | `Monitor powered down.` |
| `Paused` | `t3` | Paused | `Nothing will dim until you resume.` |
| `Degraded` | `warn_text` | Degraded | `Camera failed — dimming is off.` |

**The Degraded band without a layout shift**: in Degraded the plate is dead
anyway, so the warning band occupies the plate's top 56px, inside its rounded
corners. Zero layout change, maximum prominence, and it lands exactly where the
user is already looking. Contents: triangle glyph, `Camera failed 3 times —
dimming is off`, `VISOR will not dim or blank while it cannot see. Retrying in
4:12.` (counting down `DEGRADED_RETRY`), and `Retry now`.

---

## 8. Diagnostics

Two Caption lines, deliberately quiet.

```
[DDC/CI]  LG UltraGear GX7 (\\.\DISPLAY1)
Backlight dimming, 20% · strategy = auto
```

Overlay fallback takes `warn_border` / `warn_text` on the chip only; the body
text stays `t2` — this is a fact with a consequence, not a warning. Only
Degraded gets warning chroma. When `strategy` is not `auto`, append
` · forced in config` in `t3` so a confusing forced mode is never invisible.

---

## 9. Motion

Nothing exceeds 240ms.

| Thing | Spec |
|---|---|
| Gauge fill **height** | **No animation** — it is a live measurement; tweening it would be a lie |
| Gauge fill **colour** | 240ms cross-fade |
| Threshold jump | 180ms ease-out cubic |
| Confirmation underline | 10s linear |
| Playhead | 4 Hz step, no tween |
| Hover | 90ms |
| Marker drag | 1:1, no smoothing |

Honour `SPI_GETCLIENTAREAANIMATION`: when off, cross-fades become snaps. The
confirmation underline stays — it is information, not decoration.

---

## 10. Accessibility

- **Colour is never the sole carrier** of ratio state: the deficit hatch, the
  ghost box, the em-dash numeral, the confirmed dot and the verdict sentence
  each independently say which state you are in. Matters especially because
  good/marginal/below are green/amber/coral.
- Focus rings only after keyboard input (`UISF_HIDEFOCUS`).
- Tab order: preview button → gauge threshold → camera-opens → dim → black →
  off → dim level → Pause/Resume → Reload → Quit → close.
- **UIA**: answer `WM_GETOBJECT` with a provider exposing the four markers with
  `RangeValuePattern` and the status band as a live region. Without it a screen
  reader sees an empty rectangle.
- High contrast: map tokens to `GetSysColor`, draw fills as outlines, keep the
  plate as-is.

---

## 11. Implementation notes

- Currently `ID2D1HwndRenderTarget`. A device context on a flip-model DXGI swap
  chain would buy per-monitor DPI v2 and clean partial presents for the 4 Hz
  playhead; swapping touches only `Renderer::create`.
- Rounded track segments: `PushLayer` with a rounded-rect geometry mask, fill
  plain rectangles inside, `PopLayer`. Direct2D has no per-corner radius.
- Repaint budget: camera live → 15 fps, invalidating plate + gauge + readout
  only. Camera closed → 4 Hz, playhead strip only. Idle → event-driven. The
  window must cost nothing when nothing moves; that is the product's premise.
- **Two things that bite if skipped**: tabular figures (the numeral wobbles),
  and letterboxing instead of cropping (the displayed ratio stops matching the
  measured one).
