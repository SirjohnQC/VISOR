//! The tuning window: a real top-level Win32 window, every pixel painted by us.
//!
//! **Pump-thread only.** This is a top-level window, so it belongs to the
//! thread that runs the message loop — the same rule (ruling F7) that put the
//! overlay windows there. It is created, drawn and destroyed from `ui::tray`'s
//! loop and never crosses a thread boundary.
//!
//! Rendering is Direct2D + DirectWrite, which cost no new dependencies: both
//! are feature flags on the `windows` crate already in the tree. That is what
//! lets a zero-dependency window still have antialiased geometry and real
//! typography instead of looking like a 2005 properties dialog.
//!
//! The render target is an `ID2D1HwndRenderTarget` rather than a device
//! context on a DXGI swap chain. The swap chain would buy per-monitor DPI v2
//! and partial presents (worth having for the 4 Hz playhead); the Hwnd target
//! is a fraction of the code and correct for a window this size. Swapping it
//! later touches only `Renderer::create`.

use crate::core::types::State;
use crate::sense::preview::PreviewFrame;
use crate::ui::controls::{Axis, Scale, TIME_SNAPS, clamp_above, clamp_below, snap};
use crate::ui::settings::{self, Settings};
use crate::ui::signal::{
    CameraStatus, Confirmation, Envelope, SMOOTH_ALPHA, SignalState, classify, quantise, smooth,
    suggested,
};
use crate::ui::theme::{Palette, Rgb, Theme, palette};
use std::cell::Cell;
use std::cell::RefCell;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_POINT_2F, D2D_RECT_F, D2D_SIZE_U, D2D1_ALPHA_MODE_IGNORE, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_BITMAP_PROPERTIES, D2D1_DRAW_TEXT_OPTIONS_NONE,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_HWND_RENDER_TARGET_PROPERTIES,
    D2D1_PRESENT_OPTIONS_NONE, D2D1_RENDER_TARGET_PROPERTIES, D2D1_ROUNDED_RECT, D2D1CreateFactory,
    ID2D1Bitmap, ID2D1Factory, ID2D1HwndRenderTarget, ID2D1SolidColorBrush,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT, DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_NEAR,
    DWRITE_TEXT_ALIGNMENT, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING,
    DWRITE_TEXT_ALIGNMENT_TRAILING, DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat,
};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, PAINTSTRUCT};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRectEx, CreateWindowExW, DefWindowProcW, DestroyWindow, GWLP_USERDATA,
    GetWindowLongPtrW, HMENU, IDC_ARROW, LoadCursorW, RegisterClassW, SW_HIDE, SW_SHOW,
    SetWindowLongPtrW, ShowWindow, WM_CLOSE, WM_DESTROY, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEMOVE, WM_NCCREATE, WM_PAINT, WM_SIZE, WNDCLASSW, WS_CAPTION, WS_EX_APPWINDOW,
    WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU,
};
use windows::core::PCWSTR;

/// Logical size from the design spec. Fixed, deliberately: a window that never
/// resizes needs no scrollbar control and every coordinate is a constant.
pub const WIN_W: f32 = 420.0;
pub const WIN_H: f32 = 696.0;
const MARGIN: f32 = 22.0;
const CONTENT_R: f32 = WIN_W - MARGIN;

const CLASS_NAME: &str = "VISOR.TuningWindow";

/// The preview plate.
const PLATE: (f32, f32, f32, f32) = (MARGIN, 108.0, 342.0, 348.0);
/// The preview toggle, in its two positions. One rect each, used by BOTH the
/// painter and the hit test -- two copies of these numbers is how a button
/// ends up drawn in one place and clickable in another.
///
/// With the camera shut the button is the call to action, so it sits in the
/// middle of an empty plate. Once video is running the middle of the plate is
/// the user's face, so it moves to the top corner where the picture is almost
/// always ceiling or wall.
const PREVIEW_BTN_IDLE: (f32, f32, f32, f32) = (120.0, 268.0, 244.0, 300.0);
const PREVIEW_BTN_LIVE: (f32, f32, f32, f32) = (240.0, 116.0, 336.0, 140.0);

/// Footer navigation. Right-aligned on the instrument page so it never crowds
/// the actions, left-aligned on the settings page because "back" belongs where
/// the eye starts a line.
const SETTINGS_BTN: (f32, f32, f32, f32) = (312.0, 662.0, CONTENT_R, 690.0);
const BACK_BTN: (f32, f32, f32, f32) = (MARGIN, 662.0, 88.0, 690.0);

/// Which page the window is showing.
///
/// Not tabs: spec §4 bans those, and rightly — the instrument face must not
/// grow chrome for a page most people open once. This is one ghost button and
/// a swap, and the window never changes size, so the fixed-layout premise (and
/// with it the absence of any scrollbar) survives intact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Instrument,
    Settings,
}

/// What the window is told about the rest of VISOR each tick.
///
/// The window derives nothing it can be handed: the threshold and the state
/// come from the same config and machine everything else uses, so the number
/// on screen cannot drift from the number being acted on.
#[derive(Debug, Clone)]
pub struct WindowStatus {
    pub state: State,
    pub threshold: f32,
    pub dim_level: u8,
    pub monitor: String,
    pub ddc: bool,
    pub brightness_confirmed: bool,
    pub idle_grace: f32,
    pub dim_after: f32,
    pub away_after: f32,
    pub deep_after: f32,
}

impl Default for WindowStatus {
    fn default() -> Self {
        Self {
            state: State::Active,
            threshold: 0.15,
            dim_level: 20,
            monitor: String::new(),
            ddc: false,
            brightness_confirmed: false,
            idle_grace: 30.0,
            dim_after: 20.0,
            away_after: 45.0,
            deep_after: 900.0,
        }
    }
}

/// The six values the window edits, in the units the axes work in: a ratio,
/// a percentage, and four durations in seconds.
///
/// Held separately from [`WindowStatus`] because these are what the user is
/// currently dragging, and a status push arriving mid-drag must not yank the
/// handle out from under them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Editable {
    pub threshold: f32,
    pub dim_level: u8,
    pub idle_grace: f32,
    pub dim_after: f32,
    pub away_after: f32,
    pub deep_after: f32,
}

/// What the mouse currently owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grab {
    Threshold,
    DimLevel,
    IdleGrace,
    DimAfter,
    AwayAfter,
    DeepAfter,
}

/// The sequence rail's pixel run. Inset from the track so a handle at either
/// extreme is not half outside it.
const RAIL_LO: f32 = 29.0;
const RAIL_HI: f32 = 391.0;
/// Handle lane and track, for hit-testing.
const RAIL_TOP: f32 = 455.0;
const RAIL_BOTTOM: f32 = 505.0;
/// Smallest gap between two ladder thresholds, in seconds.
const LADDER_GAP: f32 = 5.0;

fn rail_axis() -> Axis {
    Axis {
        lo: RAIL_LO,
        hi: RAIL_HI,
        scale: Scale::Log {
            t0: 5.0,
            t1: 3600.0,
        },
    }
}

fn dim_axis() -> Axis {
    Axis {
        lo: 74.0,
        hi: 330.0,
        scale: Scale::Linear {
            min: 1.0,
            max: 99.0,
        },
    }
}

fn gauge_axis(top: f32, bottom: f32) -> Axis {
    Axis {
        lo: bottom,
        hi: top,
        scale: Scale::Linear {
            min: 0.0,
            max: GAUGE_TOP,
        },
    }
}

fn hit(r: (f32, f32, f32, f32), x: f32, y: f32) -> bool {
    x >= r.0 && x < r.2 && y >= r.1 && y < r.3
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn colour(c: Rgb) -> D2D1_COLOR_F {
    let (r, g, b) = c.to_f32();
    D2D1_COLOR_F { r, g, b, a: 1.0 }
}

fn rect(l: f32, t: f32, r: f32, b: f32) -> D2D_RECT_F {
    D2D_RECT_F {
        left: l,
        top: t,
        right: r,
        bottom: b,
    }
}

/// The cached text formats, one per level of the type scale.
///
/// No wordmark level: the native title bar already says VISOR, and a second
/// wordmark drawn under it was brand noise occupying the one row that had a
/// genuine use -- the camera status, which has to be visible at a glance and
/// has to agree with the hardware LED.
struct Fonts {
    numeral: IDWriteTextFormat,
    title: IDWriteTextFormat,
    body: IDWriteTextFormat,
    body_strong: IDWriteTextFormat,
    caption: IDWriteTextFormat,
    micro: IDWriteTextFormat,
    section: IDWriteTextFormat,
}

impl Fonts {
    /// The axis ticks and marker labels are the only users of `micro`, and
    /// they arrive with the rail control. Kept in the cache now so the type
    /// scale is defined in one place rather than growing a straggler later.
    #[allow(dead_code)]
    fn micro(&self) -> &IDWriteTextFormat {
        &self.micro
    }
}

impl Fonts {
    fn create(dw: &IDWriteFactory) -> windows::core::Result<Fonts> {
        // Segoe UI Variable ships with Windows 11; DirectWrite falls back on
        // its own if it is missing, so no download and no bundled face.
        let make = |size: f32, weight: u16| -> windows::core::Result<IDWriteTextFormat> {
            let f = unsafe {
                dw.CreateTextFormat(
                    PCWSTR(wide("Segoe UI Variable Text").as_ptr()),
                    None,
                    DWRITE_FONT_WEIGHT(weight as i32),
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size,
                    PCWSTR(wide("en-us").as_ptr()),
                )?
            };
            unsafe {
                f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?;
            }
            Ok(f)
        };
        Ok(Fonts {
            numeral: make(34.0, 300)?,
            title: make(20.0, 600)?,
            body: make(13.0, 400)?,
            body_strong: make(13.0, 600)?,
            caption: make(12.0, 400)?,
            micro: make(10.5, 400)?,
            section: make(11.0, 600)?,
        })
    }
}

/// Direct2D resources. Recreated wholesale if the device is ever lost.
struct Renderer {
    target: ID2D1HwndRenderTarget,
    fonts: Fonts,
}

impl Renderer {
    fn create(
        d2d: &ID2D1Factory,
        dw: &IDWriteFactory,
        hwnd: HWND,
        w: u32,
        h: u32,
    ) -> windows::core::Result<Renderer> {
        let props = D2D1_RENDER_TARGET_PROPERTIES::default();
        let hwnd_props = D2D1_HWND_RENDER_TARGET_PROPERTIES {
            hwnd,
            pixelSize: windows::Win32::Graphics::Direct2D::Common::D2D_SIZE_U {
                width: w,
                height: h,
            },
            presentOptions: D2D1_PRESENT_OPTIONS_NONE,
        };
        let target = unsafe { d2d.CreateHwndRenderTarget(&props, &hwnd_props)? };
        Ok(Renderer {
            target,
            fonts: Fonts::create(dw)?,
        })
    }
}

/// Everything the window needs to draw itself.
///
/// Owned by the window via `GWLP_USERDATA`; `RefCell` because the WndProc is
/// re-entered by Windows and cannot hold a `&mut` across a call into it.
pub struct TuningWindow {
    hwnd: HWND,
    d2d: ID2D1Factory,
    dwrite: IDWriteFactory,
    renderer: RefCell<Option<Renderer>>,
    palette: RefCell<Palette>,
    visible: RefCell<bool>,
    /// The most recent preview frame, and the D2D bitmap it was uploaded into.
    frame: RefCell<Option<PreviewFrame>>,
    bitmap: RefCell<Option<(ID2D1Bitmap, u32, u32)>>,
    /// Scratch BGRA buffer, reused so a 15fps preview does not allocate
    /// 1.2 MB per frame.
    bgra: RefCell<Vec<u8>>,
    preview_on: Cell<bool>,
    status: RefCell<WindowStatus>,
    /// The rolling min/max the verdict is judged on. Lives here rather than in
    /// the engine because it is a property of what the user is being shown,
    /// not of what VISOR is deciding.
    envelope: RefCell<Envelope>,
    confirmation: RefCell<Confirmation>,
    smoothed: Cell<Option<f32>>,
    shown: Cell<Option<f32>>,
    /// What the user is editing. Seeded from status, then owned by the window
    /// until it is saved.
    edits: Cell<Editable>,
    grab: Cell<Option<Grab>>,
    /// Set on drag release; the pump drains it and writes config.toml.
    dirty: Cell<bool>,
    /// The gauge's pixel extent from the last paint. The gauge follows the
    /// video, which is only known once a frame has been drawn, so hit-testing
    /// has to read what the painter last decided rather than guess.
    gauge_extent: Cell<(f32, f32)>,
    /// Set by a click, drained by the pump. The window cannot talk to the
    /// engine itself -- it does not own the command channel -- so it leaves a
    /// note and `ui::tray` posts it.
    pending_preview: Cell<Option<bool>>,
    /// Which page is showing, and the eight values the second one edits.
    /// Same discipline as `edits`: owned by the window from the click until
    /// the pump has written it, so a status push cannot revert it in between.
    page: Cell<Page>,
    settings: Cell<Settings>,
    settings_dirty: Cell<bool>,
}

impl TuningWindow {
    /// Create the window, hidden. Returns `None` (after logging) rather than
    /// panicking: VISOR's job is dimming, and it must keep doing it even if
    /// the window cannot be built.
    pub fn create(theme: Theme) -> Option<Box<TuningWindow>> {
        match Self::try_create(theme) {
            Ok(w) => Some(w),
            Err(e) => {
                tracing::error!(error = %e, "could not create the tuning window");
                None
            }
        }
    }

    fn try_create(theme: Theme) -> windows::core::Result<Box<TuningWindow>> {
        // SAFETY: standard factory creation; both calls either return a live
        // interface or an error, and neither takes a pointer we own.
        let d2d: ID2D1Factory =
            unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)? };
        let dwrite: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };

        register_class();

        let mut me = Box::new(TuningWindow {
            hwnd: HWND(std::ptr::null_mut()),
            d2d,
            dwrite,
            renderer: RefCell::new(None),
            palette: RefCell::new(palette(theme)),
            visible: RefCell::new(false),
            frame: RefCell::new(None),
            bitmap: RefCell::new(None),
            bgra: RefCell::new(Vec::new()),
            preview_on: Cell::new(false),
            status: RefCell::new(WindowStatus::default()),
            envelope: RefCell::new(Envelope::new()),
            confirmation: RefCell::new(Confirmation::new()),
            smoothed: Cell::new(None),
            shown: Cell::new(None),
            edits: Cell::new(Editable {
                threshold: 0.15,
                dim_level: 20,
                idle_grace: 30.0,
                dim_after: 20.0,
                away_after: 45.0,
                deep_after: 900.0,
            }),
            grab: Cell::new(None),
            dirty: Cell::new(false),
            gauge_extent: Cell::new((PLATE.1, PLATE.3)),
            pending_preview: Cell::new(None),
            page: Cell::new(Page::Instrument),
            // Seeded from defaults and overwritten by the pump's first
            // `set_settings`. The page cannot be reached without a click, and
            // a click cannot happen before the window is visible, so nothing
            // is ever drawn from this placeholder.
            settings: Cell::new(Settings::from_config(&crate::config::Config::default())),
            settings_dirty: Cell::new(false),
        });

        let class = wide(CLASS_NAME);
        let title = wide("VISOR");

        // CreateWindowExW takes the OUTER size, but every coordinate in the
        // design is a client coordinate. Passing 420x696 straight in gives a
        // client area smaller by the caption and borders -- roughly 32px of
        // the bottom, which is the whole footer -- and silently squeezes the
        // layout. Ask Windows how big the frame needs to be instead.
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        let mut outer = RECT {
            left: 0,
            top: 0,
            right: WIN_W as i32,
            bottom: WIN_H as i32,
        };
        // SAFETY: `outer` is a local RECT the call only writes into.
        unsafe {
            let _ = AdjustWindowRectEx(&mut outer, style, false, WS_EX_APPWINDOW);
        }
        // The box is passed as the creation param so the WndProc can find it
        // from the very first message; see `wndproc`'s WM_NCCREATE arm.
        let ptr: *mut TuningWindow = &mut *me;
        // SAFETY: `class`/`title` are null-terminated and outlive the call.
        // `ptr` stays valid because `me` is boxed and returned to the caller,
        // who keeps it alive for as long as the window exists.
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_APPWINDOW,
                PCWSTR(class.as_ptr()),
                PCWSTR(title.as_ptr()),
                // No WS_THICKFRAME and no WS_MAXIMIZEBOX: the layout is a
                // table of constants, so the window does not resize.
                style,
                windows::Win32::UI::WindowsAndMessaging::CW_USEDEFAULT,
                windows::Win32::UI::WindowsAndMessaging::CW_USEDEFAULT,
                outer.right - outer.left,
                outer.bottom - outer.top,
                None,
                HMENU(std::ptr::null_mut()),
                None,
                Some(ptr as *const core::ffi::c_void),
            )?
        };
        me.hwnd = hwnd;
        Ok(me)
    }

    pub fn set_theme(&self, theme: Theme) {
        *self.palette.borrow_mut() = palette(theme);
        self.invalidate();
    }

    pub fn is_visible(&self) -> bool {
        *self.visible.borrow()
    }

    /// A preview frame from the engine.
    ///
    /// This is also where the measurement is taken: the envelope gets the RAW
    /// ratio (its job is to catch the dips smoothing would hide) while the
    /// number on screen gets the smoothed, dead-banded one.
    pub fn set_frame(&self, frame: PreviewFrame) {
        let now = std::time::Instant::now();
        match frame.largest_ratio() {
            Some(r) => {
                self.envelope.borrow_mut().push(now, r);
                let sm = smooth(self.smoothed.get(), r, SMOOTH_ALPHA);
                self.smoothed.set(Some(sm));
                self.shown.set(Some(quantise(self.shown.get(), sm)));
            }
            None => {
                // No face is not a reading of zero -- recording one would drag
                // the envelope minimum to the floor and make every threshold
                // look unsafe. Just let the window age out.
                self.envelope.borrow_mut().expire(now);
                self.smoothed.set(None);
                self.shown.set(None);
            }
        }
        *self.frame.borrow_mut() = Some(frame);
        if self.is_visible() {
            self.invalidate();
        }
    }

    /// State, threshold and display diagnostics, pushed by the pump each tick.
    pub fn set_status(&self, status: WindowStatus) {
        let changed = {
            let cur = self.status.borrow();
            (cur.threshold - status.threshold).abs() > f32::EPSILON
                || cur.state != status.state
                || cur.ddc != status.ddc
                || cur.monitor != status.monitor
                || cur.dim_level != status.dim_level
        };
        if changed {
            // Never reseed while the user has hold of something, or the handle
            // jumps out from under the mouse. Nor while there are unsaved
            // edits: the pump is about to write them, and the config it is
            // pushing is the one they replace.
            if self.grab.get().is_none() && !self.dirty.get() {
                self.edits.set(Editable {
                    threshold: status.threshold,
                    dim_level: status.dim_level,
                    idle_grace: status.idle_grace,
                    dim_after: status.dim_after,
                    away_after: status.away_after,
                    deep_after: status.deep_after,
                });
            }
            *self.status.borrow_mut() = status;
            if self.is_visible() {
                self.invalidate();
            }
        }
    }

    /// The current edits, and whether they need saving. Drained by the pump.
    pub fn take_edits(&self) -> Option<Editable> {
        if self.dirty.replace(false) {
            Some(self.edits.get())
        } else {
            None
        }
    }

    /// Push the settings page's values in from config.
    ///
    /// Refused while a click is still waiting to be saved: the pump pushes
    /// every 250ms from a `cfg` it only re-reads on Reload, so adopting one
    /// mid-flight would put the just-clicked segment back where it was and the
    /// click would look like it bounced.
    pub fn set_settings(&self, s: Settings) {
        if self.settings_dirty.get() || self.settings.get() == s {
            return;
        }
        self.settings.set(s);
        if self.is_visible() && self.page.get() == Page::Settings {
            self.invalidate();
        }
    }

    /// The settings page's values, if a click changed them. Drained by the pump.
    pub fn take_settings(&self) -> Option<Settings> {
        if self.settings_dirty.replace(false) {
            Some(self.settings.get())
        } else {
            None
        }
    }

    /// Which handle, if any, is under this point.
    ///
    /// Ordered deliberately: the rail markers are tested before the track they
    /// sit on, so grabbing a handle never registers as a click on the bar
    /// underneath it.
    fn hit_handle(&self, x: f32, y: f32) -> Option<Grab> {
        let e = self.edits.get();
        let (gt, gb) = self.gauge_extent.get();

        // Gauge threshold: a wide band, because the grip is only 10px tall and
        // the spec asks for a 44px target.
        let ty = gauge_axis(gt, gb).pixel_of(e.threshold);
        if (350.0..=400.0).contains(&x) && (y - ty).abs() <= 14.0 {
            return Some(Grab::Threshold);
        }

        if (RAIL_TOP..=RAIL_BOTTOM).contains(&y) {
            let a = rail_axis();
            let mut best: Option<(f32, Grab)> = None;
            for (value, which) in [
                (e.idle_grace, Grab::IdleGrace),
                (e.idle_grace + e.dim_after, Grab::DimAfter),
                (e.idle_grace + e.away_after, Grab::AwayAfter),
                (e.idle_grace + e.deep_after, Grab::DeepAfter),
            ] {
                let d = (a.pixel_of(value) - x).abs();
                if d <= 11.0 && best.is_none_or(|(bd, _)| d < bd) {
                    best = Some((d, which));
                }
            }
            if let Some((_, which)) = best {
                return Some(which);
            }
        }

        if (68.0..=336.0).contains(&x) && (542.0..=570.0).contains(&y) {
            return Some(Grab::DimLevel);
        }
        None
    }

    /// Move whatever is held to follow the pointer.
    fn drag_to(&self, x: f32, y: f32) {
        let Some(grab) = self.grab.get() else { return };
        let mut e = self.edits.get();
        let a = rail_axis();

        match grab {
            Grab::Threshold => {
                let (gt, gb) = self.gauge_extent.get();
                // Clamped to what the gauge can actually show. Below 0.02 is
                // noise, and above 0.60 is off the top of the scale.
                e.threshold = gauge_axis(gt, gb).value_at(y).clamp(0.02, GAUGE_TOP);
            }
            Grab::DimLevel => {
                let v = dim_axis().value_at(x).round().clamp(1.0, 99.0);
                // 5% stops, because nobody wants to hunt for 37%.
                e.dim_level = ((v / 5.0).round() * 5.0).clamp(1.0, 99.0) as u8;
            }
            Grab::IdleGrace => {
                // Markers 2-4 are stored RELATIVE to this one, so moving it
                // slides the whole sequence rigidly. That falls out of the
                // representation rather than needing to be arranged.
                e.idle_grace = snap(a.value_at(x), &TIME_SNAPS, &a, 7.0).clamp(5.0, 3600.0);
            }
            Grab::DimAfter | Grab::AwayAfter | Grab::DeepAfter => {
                let absolute = snap(a.value_at(x), &TIME_SNAPS, &a, 7.0);
                let rel = (absolute - e.idle_grace).max(1.0);
                match grab {
                    Grab::DimAfter => {
                        e.dim_after = clamp_below(rel, e.away_after, LADDER_GAP).max(1.0)
                    }
                    Grab::AwayAfter => {
                        e.away_after = clamp_above(rel, e.dim_after, LADDER_GAP);
                        e.away_after = clamp_below(e.away_after, e.deep_after, LADDER_GAP);
                    }
                    _ => e.deep_after = clamp_above(rel, e.away_after, LADDER_GAP),
                }
            }
        }
        self.edits.set(e);
        self.invalidate();
    }

    fn begin_drag(&self, grab: Grab) {
        self.grab.set(Some(grab));
        // SAFETY: capture is released on WM_LBUTTONUP; `self.hwnd` is live.
        unsafe {
            SetCapture(self.hwnd);
        }
    }

    fn end_drag(&self) {
        if self.grab.take().is_some() {
            self.dirty.set(true);
            // A new line deserves a fresh verdict: the old confirmation was
            // about a threshold that no longer exists.
            self.envelope.borrow_mut().clear();
            self.confirmation
                .borrow_mut()
                .restart(std::time::Instant::now());
            // SAFETY: paired with the SetCapture in `begin_drag`.
            unsafe {
                let _ = ReleaseCapture();
            }
            self.invalidate();
        }
    }

    /// The measurement, as the window will draw it.
    fn signal(&self) -> (SignalState, Option<f32>, Option<f32>) {
        let threshold = self.status.borrow().threshold;
        let cam = if !self.preview_on.get() {
            CameraStatus::Closed
        } else if self.frame.borrow().is_some() {
            CameraStatus::Live
        } else {
            CameraStatus::Unavailable
        };
        let low = self.envelope.borrow().min();
        (classify(cam, low, threshold), self.shown.get(), low)
    }

    /// A click asked to turn the preview on or off; `None` if nothing is
    /// waiting. Drained by the pump, which owns the command channel.
    pub fn take_preview_request(&self) -> Option<bool> {
        self.pending_preview.take()
    }

    /// Told by the pump what actually happened, so the button label and the
    /// engine can never disagree.
    pub fn set_preview_state(&self, on: bool) {
        self.preview_on.set(on);
        if !on {
            *self.frame.borrow_mut() = None;
            self.envelope.borrow_mut().clear();
            self.smoothed.set(None);
            self.shown.set(None);
            self.confirmation.borrow_mut().cancel();
        } else {
            // A fresh look starts a fresh confirmation.
            self.envelope.borrow_mut().clear();
            self.confirmation
                .borrow_mut()
                .restart(std::time::Instant::now());
        }
        self.invalidate();
    }

    pub fn show(&self) {
        *self.visible.borrow_mut() = true;
        // SAFETY: `self.hwnd` is live for as long as this struct is.
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOW);
        }
        self.invalidate();
    }

    pub fn hide(&self) {
        *self.visible.borrow_mut() = false;
        // Never leave the camera held open behind an invisible window. The
        // request goes through the pump like any other, so the engine and the
        // LED agree with what the user can see.
        if self.preview_on.get() {
            self.pending_preview.set(Some(false));
        }
        // SAFETY: as above.
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }

    pub fn toggle(&self) {
        if self.is_visible() {
            self.hide()
        } else {
            self.show()
        }
    }

    /// Route a click. The only live control so far is the preview toggle;
    /// the markers land next and will hang off the same test.
    fn preview_btn(&self) -> (f32, f32, f32, f32) {
        if self.preview_on.get() {
            PREVIEW_BTN_LIVE
        } else {
            PREVIEW_BTN_IDLE
        }
    }

    fn on_click(&self, x: f32, y: f32) {
        if self.page.get() == Page::Settings {
            if hit(BACK_BTN, x, y) {
                self.page.set(Page::Instrument);
                self.invalidate();
            } else if let Some((setting, index)) = settings::hit(x, y) {
                let mut s = self.settings.get();
                setting.apply(&mut s, index);
                // Only mark dirty if something moved: clicking the segment
                // that is already lit should not cost a file write and a
                // reload of the whole engine.
                if s != self.settings.get() {
                    self.settings.set(s);
                    self.settings_dirty.set(true);
                    self.invalidate();
                }
            }
            return;
        }
        if hit(SETTINGS_BTN, x, y) {
            self.page.set(Page::Settings);
            self.invalidate();
            return;
        }
        if hit(self.preview_btn(), x, y) {
            self.pending_preview.set(Some(!self.preview_on.get()));
            return;
        }
        if let Some(g) = self.hit_handle(x, y) {
            self.begin_drag(g);
        }
    }

    /// Upload the newest luminance frame into a reusable D2D bitmap.
    ///
    /// The frame is one byte per pixel and D2D wants four, so it is expanded
    /// into a scratch buffer kept between frames. At 15fps a fresh 1.2 MB
    /// allocation per frame would be the most expensive thing in an otherwise
    /// 1 Hz program.
    fn upload(&self, t: &ID2D1HwndRenderTarget, f: &PreviewFrame) -> Option<ID2D1Bitmap> {
        let (w, h) = (f.width, f.height);
        if f.luma.len() < (w * h) as usize {
            return None;
        }
        let mut bgra = self.bgra.borrow_mut();
        bgra.resize((w * h * 4) as usize, 0);
        for (i, &l) in f.luma.iter().enumerate().take((w * h) as usize) {
            let o = i * 4;
            bgra[o] = l;
            bgra[o + 1] = l;
            bgra[o + 2] = l;
            bgra[o + 3] = 0xFF;
        }

        let mut slot = self.bitmap.borrow_mut();
        let same = matches!(slot.as_ref(), Some((_, bw, bh)) if *bw == w && *bh == h);
        if !same {
            let props = D2D1_BITMAP_PROPERTIES {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_IGNORE,
                },
                dpiX: 96.0,
                dpiY: 96.0,
            };
            // SAFETY: `bgra` holds exactly w*h*4 bytes and outlives the call.
            let made = unsafe {
                t.CreateBitmap(
                    D2D_SIZE_U {
                        width: w,
                        height: h,
                    },
                    Some(bgra.as_ptr() as *const core::ffi::c_void),
                    w * 4,
                    &props,
                )
            };
            *slot = made.ok().map(|b| (b, w, h));
        } else if let Some((b, _, _)) = slot.as_ref() {
            // SAFETY: the bitmap is w*h and `bgra` holds exactly that many pixels.
            unsafe {
                let _ = b.CopyFromMemory(None, bgra.as_ptr() as *const core::ffi::c_void, w * 4);
            }
        }
        slot.as_ref().map(|(b, _, _)| b.clone())
    }

    fn invalidate(&self) {
        // SAFETY: `self.hwnd` is live; a null rect invalidates the whole
        // client area.
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::InvalidateRect(self.hwnd, None, false);
        }
    }

    fn paint(&self) {
        let mut slot = self.renderer.borrow_mut();
        if slot.is_none() {
            match Renderer::create(
                &self.d2d,
                &self.dwrite,
                self.hwnd,
                WIN_W as u32,
                WIN_H as u32,
            ) {
                Ok(r) => *slot = Some(r),
                Err(e) => {
                    tracing::error!(error = %e, "could not create the render target");
                    return;
                }
            }
        }
        let Some(r) = slot.as_ref() else { return };
        let p = *self.palette.borrow();

        // SAFETY: a standard Direct2D draw pass. Every call below targets the
        // render target created above and is bracketed by Begin/EndDraw.
        unsafe {
            r.target.BeginDraw();
            r.target.Clear(Some(&colour(p.bg)));
            self.draw_chrome(r, &p);
            // A lost device shows up here; dropping the renderer makes the
            // next paint rebuild it rather than drawing into a dead target.
            if r.target.EndDraw(None, None).is_err() {
                drop(slot);
                *self.renderer.borrow_mut() = None;
            }
        }
    }

    /// # Safety
    /// Must be called between `BeginDraw` and `EndDraw` on `r.target`.
    unsafe fn draw_chrome(&self, r: &Renderer, p: &Palette) {
        let t = &r.target;
        let st = self.status.borrow().clone();
        let (signal, shown, low) = self.signal();

        // SAFETY: the caller guarantees we are inside a draw pass.
        unsafe {
            let brush = |c: Rgb| -> Option<ID2D1SolidColorBrush> {
                t.CreateSolidColorBrush(&colour(c), None).ok()
            };
            let (Some(b_t1), Some(b_t2), Some(b_t3), Some(b_hair), Some(b_well), Some(b_strong)) = (
                brush(p.t1),
                brush(p.t2),
                brush(p.t3),
                brush(p.hair),
                brush(p.well),
                brush(p.strong),
            ) else {
                return;
            };
            // Chroma is spent only on the measurement. This is the one place
            // in the window that picks a saturated colour at all.
            let accent = match signal {
                SignalState::Good => p.good,
                SignalState::Marginal => p.marginal,
                SignalState::Below => p.below,
                SignalState::NoFace => p.no_signal,
                SignalState::Unavailable => p.dead,
            };
            let Some(b_accent) = brush(accent) else {
                return;
            };

            let text =
                |s: &str, f: &IDWriteTextFormat, r_: D2D_RECT_F, b: &ID2D1SolidColorBrush| {
                    let w = wide(s);
                    t.DrawText(
                        &w[..w.len() - 1],
                        f,
                        &r_,
                        b,
                        D2D1_DRAW_TEXT_OPTIONS_NONE,
                        DWRITE_MEASURING_MODE_NATURAL,
                    );
                };
            let align = |f: &IDWriteTextFormat, a: DWRITE_TEXT_ALIGNMENT| {
                let _ = f.SetTextAlignment(a);
            };

            // ---- top row: camera status ------------------------------------
            // The native caption already says VISOR; a second wordmark under it
            // was just brand noise. This row carries the one fact worth the
            // space, and it agrees with the hardware LED at all times.
            let (cam_label, cam_dot) = if self.preview_on.get() {
                ("Camera on \u{00B7} local only", p.good)
            } else {
                ("Camera off", p.dead)
            };
            if let Some(b_dot) = brush(cam_dot) {
                let dot = D2D1_ROUNDED_RECT {
                    rect: rect(MARGIN, 17.0, MARGIN + 7.0, 24.0),
                    radiusX: 3.5,
                    radiusY: 3.5,
                };
                t.FillRoundedRectangle(&dot, &b_dot);
            }
            text(
                cam_label,
                &r.fonts.caption,
                rect(MARGIN + 14.0, 13.0, CONTENT_R, 32.0),
                &b_t2,
            );
            t.FillRectangle(&rect(0.0, 39.0, WIN_W, 40.0), &b_hair);

            // The camera row above is the one thing both pages need; below the
            // hairline they share nothing, so the second page takes over here.
            if self.page.get() == Page::Settings {
                self.draw_settings_page(
                    t, &r.fonts, &b_t1, &b_t2, &b_t3, &b_hair, &b_well, &b_strong,
                );
                return;
            }

            // ---- status band ------------------------------------------------
            let (name, sub) = state_copy(st.state, st.dim_level);
            let dot_colour = match st.state {
                State::Active => p.good,
                State::Degraded => p.warn_text,
                State::Paused => p.t3,
                _ => p.t1,
            };
            if let Some(b_dot) = brush(dot_colour) {
                let dot = D2D1_ROUNDED_RECT {
                    rect: rect(MARGIN, 62.0, MARGIN + 8.0, 70.0),
                    radiusX: 4.0,
                    radiusY: 4.0,
                };
                t.FillRoundedRectangle(&dot, &b_dot);
            }
            let b_name = if st.state == State::Degraded {
                brush(p.warn_text).unwrap_or_else(|| b_t1.clone())
            } else {
                b_t1.clone()
            };
            text(name, &r.fonts.title, rect(38.0, 53.0, 320.0, 80.0), &b_name);
            text(
                &sub,
                &r.fonts.caption,
                rect(MARGIN, 80.0, CONTENT_R, 100.0),
                &b_t2,
            );

            // ---- plate --------------------------------------------------------
            let plate = D2D1_ROUNDED_RECT {
                rect: rect(PLATE.0, PLATE.1, PLATE.2, PLATE.3),
                radiusX: 10.0,
                radiusY: 10.0,
            };
            if let Some(b_plate) = brush(p.plate) {
                t.FillRoundedRectangle(&plate, &b_plate);
            }
            t.DrawRoundedRectangle(&plate, &b_hair, 1.0, None);
            align(&r.fonts.body, DWRITE_TEXT_ALIGNMENT_LEADING);

            // The gauge is aligned to the VIDEO, not the plate: with a
            // letterboxed frame the two differ, and the caliper-to-gauge link
            // is only honest if a height on the left means the same height on
            // the right.
            let mut extent = (PLATE.1, PLATE.3);

            let frame = self.frame.borrow();
            if let Some(f) = frame.as_ref().filter(|f| f.width > 0 && f.height > 0) {
                let (pw, ph) = (PLATE.2 - PLATE.0, PLATE.3 - PLATE.1);
                let scale = (pw / f.width as f32).min(ph / f.height as f32);
                let (dw, dh) = (f.width as f32 * scale, f.height as f32 * scale);
                let (dx, dy) = (PLATE.0 + (pw - dw) / 2.0, PLATE.1 + (ph - dh) / 2.0);
                extent = (dy, dy + dh);

                if let Some(bmp) = self.upload(t, f) {
                    t.DrawBitmap(
                        &bmp,
                        Some(&rect(dx, dy, dx + dw, dy + dh)),
                        1.0,
                        D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                        None,
                    );
                }

                for fb in &f.faces {
                    let (bx, by) = (dx + fb.x as f32 * scale, dy + fb.y as f32 * scale);
                    let (bw2, bh2) = (fb.w as f32 * scale, fb.h as f32 * scale);
                    let arm = (bh2 / 4.0).clamp(4.0, 18.0);
                    for (cx, cy, sx, sy) in [
                        (bx, by, 1.0f32, 1.0f32),
                        (bx + bw2, by, -1.0, 1.0),
                        (bx, by + bh2, 1.0, -1.0),
                        (bx + bw2, by + bh2, -1.0, -1.0),
                    ] {
                        t.DrawLine(
                            D2D_POINT_2F { x: cx, y: cy },
                            D2D_POINT_2F {
                                x: cx + arm * sx,
                                y: cy,
                            },
                            &b_accent,
                            2.0,
                            None,
                        );
                        t.DrawLine(
                            D2D_POINT_2F { x: cx, y: cy },
                            D2D_POINT_2F {
                                x: cx,
                                y: cy + arm * sy,
                            },
                            &b_accent,
                            2.0,
                            None,
                        );
                    }
                }

                if let Some(b_scrim) = brush(Rgb(0, 0, 0)) {
                    t.FillRectangle(&rect(PLATE.0 + 1.0, 323.0, PLATE.2 - 1.0, 347.0), &b_scrim);
                }
                text(
                    "Tuning \u{2014} VISOR will not dim while the preview is on.",
                    &r.fonts.micro,
                    rect(PLATE.0 + 10.0, 329.0, PLATE.2, 345.0),
                    &b_t2,
                );
            } else {
                text(
                    "Camera is closed",
                    &r.fonts.body,
                    rect(PLATE.0 + 104.0, 196.0, PLATE.2, 220.0),
                    &b_t2,
                );
                text(
                    "VISOR opens it only after your keyboard and mouse go idle. Nothing it sees ever leaves this PC.",
                    &r.fonts.caption,
                    rect(PLATE.0 + 40.0, 222.0, PLATE.2 - 40.0, 262.0),
                    &b_t3,
                );
            }

            let btn = self.preview_btn();
            let btn_r = D2D1_ROUNDED_RECT {
                rect: rect(btn.0, btn.1, btn.2, btn.3),
                radiusX: 6.0,
                radiusY: 6.0,
            };
            if self.preview_on.get() {
                // Over live video the button needs its own ground or it is
                // unreadable against whatever happens to be behind it.
                if let Some(b_bg) = brush(p.surface) {
                    t.FillRoundedRectangle(&btn_r, &b_bg);
                }
            }
            t.DrawRoundedRectangle(&btn_r, &b_strong, 1.0, None);
            align(&r.fonts.caption, DWRITE_TEXT_ALIGNMENT_LEADING);
            text(
                if self.preview_on.get() {
                    "Turn off preview"
                } else {
                    "Turn on preview"
                },
                &r.fonts.caption,
                rect(btn.0 + 12.0, btn.1 + 4.0, btn.2, btn.3),
                &b_t1,
            );
            drop(frame);

            // ---- gauge --------------------------------------------------------
            let (gt, gb) = extent;
            // Remember it: the gauge follows the video, so hit-testing has to
            // read what the painter last decided rather than guess.
            self.gauge_extent.set((gt, gb));
            let ed = self.edits.get();
            let gauge = gauge_axis(gt, gb);
            let y_of = |v: f32| gauge.pixel_of(v);

            let track = D2D1_ROUNDED_RECT {
                rect: rect(362.0, gt, 382.0, gb),
                radiusX: 10.0,
                radiusY: 10.0,
            };
            t.FillRoundedRectangle(&track, &b_well);
            t.DrawRoundedRectangle(&track, &b_hair, 1.0, None);

            if let (Some(lo), Some(hi)) = (low, self.envelope.borrow().max())
                && let Ok(band) = t.CreateSolidColorBrush(
                    &D2D1_COLOR_F {
                        a: 0.28,
                        ..colour(accent)
                    },
                    None,
                )
            {
                t.FillRectangle(&rect(358.0, y_of(hi), 386.0, y_of(lo)), &band);
            }

            if let Some(v) = shown {
                t.FillRectangle(&rect(363.0, y_of(v), 381.0, gb - 1.0), &b_accent);
            }
            // The envelope minimum gets its own tick: it is the number that
            // actually decides whether VISOR will dim on you.
            if let Some(lo) = low {
                t.FillRectangle(
                    &rect(358.0, y_of(lo) - 0.5, 386.0, y_of(lo) + 0.5),
                    &b_accent,
                );
            }

            // The user's line. Neutral: it is not a measurement.
            let ty = y_of(ed.threshold);
            t.FillRectangle(&rect(354.0, ty - 0.75, 398.0, ty + 0.75), &b_t1);
            let grip = D2D1_ROUNDED_RECT {
                rect: rect(356.0, ty - 5.0, 388.0, ty + 5.0),
                radiusX: 3.0,
                radiusY: 3.0,
            };
            t.FillRoundedRectangle(&grip, &b_t1);

            // ---- readout ------------------------------------------------------
            let measured = match shown {
                Some(v) => format!("{v:.2}"),
                None => "\u{2014}".to_string(),
            };
            text(
                &measured,
                &r.fonts.numeral,
                rect(MARGIN, 353.0, 140.0, 392.0),
                &b_accent,
            );
            text(
                &format!("/ {:.2}", ed.threshold),
                &r.fonts.numeral,
                rect(100.0, 353.0, CONTENT_R, 392.0),
                &b_t3,
            );
            text(
                &verdict(signal, low, ed.threshold),
                &r.fonts.body,
                rect(MARGIN, 392.0, CONTENT_R, 414.0),
                &b_t2,
            );

            // ---- sequence -----------------------------------------------------
            text(
                "S E Q U E N C E",
                &r.fonts.section,
                rect(MARGIN, 424.0, CONTENT_R, 440.0),
                &b_t3,
            );
            self.draw_rail(t, p, &b_hair, &b_strong, &b_t1, &b_t3, ed);

            // ---- dim level ----------------------------------------------------
            text(
                "Dim to",
                &r.fonts.caption,
                rect(MARGIN, 548.0, 74.0, 566.0),
                &b_t2,
            );
            let bar = D2D1_ROUNDED_RECT {
                rect: rect(74.0, 552.0, 330.0, 560.0),
                radiusX: 4.0,
                radiusY: 4.0,
            };
            if let Some(b_dim) = brush(crate::ui::theme::dim_fill(p, ed.dim_level)) {
                t.FillRoundedRectangle(&bar, &b_dim);
            }
            t.DrawRoundedRectangle(&bar, &b_hair, 1.0, None);
            let dx = dim_axis().pixel_of(ed.dim_level as f32);
            let dh = D2D1_ROUNDED_RECT {
                rect: rect(dx - 6.0, 549.0, dx + 6.0, 563.0),
                radiusX: 6.0,
                radiusY: 6.0,
            };
            t.FillRoundedRectangle(&dh, &b_t1);
            align(&r.fonts.body_strong, DWRITE_TEXT_ALIGNMENT_TRAILING);
            text(
                &format!("{}%", ed.dim_level),
                &r.fonts.body_strong,
                rect(300.0, 547.0, CONTENT_R, 566.0),
                &b_t1,
            );
            align(&r.fonts.body_strong, DWRITE_TEXT_ALIGNMENT_LEADING);

            // ---- display ------------------------------------------------------
            text(
                "D I S P L A Y",
                &r.fonts.section,
                rect(MARGIN, 586.0, CONTENT_R, 602.0),
                &b_t3,
            );
            let (mech, how) = diagnostics(&st);
            text(
                &mech,
                &r.fonts.caption,
                rect(MARGIN, 604.0, CONTENT_R, 622.0),
                &b_t2,
            );
            text(
                &how,
                &r.fonts.caption,
                rect(MARGIN, 622.0, CONTENT_R, 650.0),
                &b_t3,
            );

            // ---- footer -------------------------------------------------------
            t.FillRectangle(&rect(0.0, 654.0, WIN_W, 655.0), &b_hair);
            let pause_label = if st.state == State::Paused {
                "Resume"
            } else {
                "Pause"
            };
            for (label, x, w) in [(pause_label, MARGIN, 76.0), ("Reload config", 106.0, 106.0)] {
                let b = D2D1_ROUNDED_RECT {
                    rect: rect(x, 662.0, x + w, 690.0),
                    radiusX: 6.0,
                    radiusY: 6.0,
                };
                t.DrawRoundedRectangle(&b, &b_strong, 1.0, None);
                text(
                    label,
                    &r.fonts.body,
                    rect(x + 12.0, 667.0, x + w, 688.0),
                    &b_t2,
                );
            }
            // The way to the second page. One ghost button, right-aligned so
            // it never crowds the two actions.
            let more = D2D1_ROUNDED_RECT {
                rect: rect(
                    SETTINGS_BTN.0,
                    SETTINGS_BTN.1,
                    SETTINGS_BTN.2,
                    SETTINGS_BTN.3,
                ),
                radiusX: 6.0,
                radiusY: 6.0,
            };
            t.DrawRoundedRectangle(&more, &b_strong, 1.0, None);
            text(
                "Settings \u{2192}",
                &r.fonts.body,
                rect(SETTINGS_BTN.0 + 12.0, 667.0, SETTINGS_BTN.2, 688.0),
                &b_t2,
            );
        }
    }

    /// The settings page: the values that used to live in TOML and nowhere
    /// else, one segmented choice per row.
    ///
    /// Draws nothing saturated. That is not restraint for its own sake — the
    /// governing rule is that the only chroma in this window carries the
    /// measurement, and none of these eight settings is a measurement. A lit
    /// segment is told apart from an unlit one by fill and text weight, which
    /// is also what makes the page legible in high contrast.
    ///
    /// Walks `settings::BLOCKS`, the same table `settings::hit` walks. Two
    /// copies of these coordinates is how a control ends up drawn in one place
    /// and clickable in another.
    ///
    /// # Safety
    /// Must be called inside a draw pass.
    #[allow(clippy::too_many_arguments)]
    unsafe fn draw_settings_page(
        &self,
        t: &ID2D1HwndRenderTarget,
        fonts: &Fonts,
        b_t1: &ID2D1SolidColorBrush,
        b_t2: &ID2D1SolidColorBrush,
        b_t3: &ID2D1SolidColorBrush,
        b_hair: &ID2D1SolidColorBrush,
        b_well: &ID2D1SolidColorBrush,
        b_strong: &ID2D1SolidColorBrush,
    ) {
        let s = self.settings.get();

        // SAFETY: the caller guarantees we are inside a draw pass.
        unsafe {
            let text =
                |v: &str, f: &IDWriteTextFormat, r_: D2D_RECT_F, b: &ID2D1SolidColorBrush| {
                    let w = wide(v);
                    t.DrawText(
                        &w[..w.len() - 1],
                        f,
                        &r_,
                        b,
                        D2D1_DRAW_TEXT_OPTIONS_NONE,
                        DWRITE_MEASURING_MODE_NATURAL,
                    );
                };
            let pill = |r_: D2D_RECT_F| D2D1_ROUNDED_RECT {
                rect: r_,
                radiusX: 6.0,
                radiusY: 6.0,
            };
            // Option labels are centred in their segment. The formats are
            // cached for the window's life and shared with page one, so this is
            // flipped and put back the way `draw_chrome` does it -- leaving it
            // centred would silently re-align every Body run on the instrument.
            let align = |f: &IDWriteTextFormat, a: DWRITE_TEXT_ALIGNMENT| {
                let _ = f.SetTextAlignment(a);
            };
            align(&fonts.body, DWRITE_TEXT_ALIGNMENT_CENTER);
            align(&fonts.body_strong, DWRITE_TEXT_ALIGNMENT_CENTER);

            for block in &settings::BLOCKS {
                match block {
                    settings::Block::Section { y, label } => {
                        text(
                            label,
                            &fonts.section,
                            rect(settings::LEFT, *y, settings::RIGHT, y + settings::SECTION_H),
                            b_t3,
                        );
                    }
                    settings::Block::Row(row) => {
                        let selected = row.setting.selected(&s);
                        text(
                            row.label,
                            &fonts.body_strong,
                            rect(
                                settings::LEFT,
                                row.y,
                                settings::RIGHT,
                                row.y + settings::LABEL_H,
                            ),
                            b_t1,
                        );
                        // A value the page does not offer lights no segment, so
                        // the caption has to carry what IS in force or the row
                        // reads as broken rather than as hand-tuned.
                        let caption = match selected {
                            Some(_) => row.caption.to_string(),
                            None => format!(
                                "{} \u{2014} config says {}",
                                row.caption,
                                row.setting.current(&s)
                            ),
                        };
                        text(
                            &caption,
                            &fonts.caption,
                            rect(
                                settings::LEFT,
                                row.y + settings::CAPTION_DY,
                                settings::RIGHT,
                                row.y + settings::CAPTION_DY + settings::CAPTION_H,
                            ),
                            b_t3,
                        );

                        for (i, option) in row.options.iter().enumerate() {
                            let (l, top, r_, bot) = settings::segment_rect(row, i);
                            let seg = pill(rect(l, top, r_, bot));
                            let on = selected == Some(i);
                            t.FillRoundedRectangle(&seg, if on { b_strong } else { b_well });
                            if on {
                                t.DrawRoundedRectangle(&seg, b_hair, 1.0, None);
                            }
                            text(
                                option,
                                if on { &fonts.body_strong } else { &fonts.body },
                                rect(l, top + 4.0, r_, bot),
                                if on { b_t1 } else { b_t2 },
                            );
                        }
                    }
                }
            }

            align(&fonts.body, DWRITE_TEXT_ALIGNMENT_LEADING);
            align(&fonts.body_strong, DWRITE_TEXT_ALIGNMENT_LEADING);

            // ---- footer -------------------------------------------------------
            t.FillRectangle(
                &rect(
                    0.0,
                    settings::FOOTER_HAIRLINE,
                    WIN_W,
                    settings::FOOTER_HAIRLINE + 1.0,
                ),
                b_hair,
            );
            let back = pill(rect(BACK_BTN.0, BACK_BTN.1, BACK_BTN.2, BACK_BTN.3));
            t.DrawRoundedRectangle(&back, b_strong, 1.0, None);
            text(
                "\u{2190} Back",
                &fonts.body,
                rect(BACK_BTN.0 + 12.0, BACK_BTN.1 + 5.0, BACK_BTN.2, BACK_BTN.3),
                b_t2,
            );
            text(
                "Saved to config.toml as you click.",
                &fonts.caption,
                rect(100.0, BACK_BTN.1 + 7.0, CONTENT_R, BACK_BTN.3),
                b_t3,
            );
        }
    }

    /// The sequence rail: one axis, four markers, and a segmented track whose
    /// fill brightness *is* the screen brightness at that point on the
    /// timeline.
    ///
    /// # Safety
    /// Must be called inside a draw pass.
    #[allow(clippy::too_many_arguments)]
    unsafe fn draw_rail(
        &self,
        t: &ID2D1HwndRenderTarget,
        p: &Palette,
        hair: &ID2D1SolidColorBrush,
        strong: &ID2D1SolidColorBrush,
        handle: &ID2D1SolidColorBrush,
        label: &ID2D1SolidColorBrush,
        e: Editable,
    ) {
        let a = rail_axis();
        let x_idle = a.pixel_of(e.idle_grace);
        let x_dim = a.pixel_of(e.idle_grace + e.dim_after);
        let x_away = a.pixel_of(e.idle_grace + e.away_after);
        let x_deep = a.pixel_of(e.idle_grace + e.deep_after);

        // SAFETY: caller guarantees a live draw pass.
        unsafe {
            let outline = D2D1_ROUNDED_RECT {
                rect: rect(MARGIN, 476.0, CONTENT_R, 504.0),
                radiusX: 6.0,
                radiusY: 6.0,
            };
            if let Ok(geo) = self.d2d.CreateRoundedRectangleGeometry(&outline)
                && let Ok(layer) = t.CreateLayer(None)
            {
                let params = windows::Win32::Graphics::Direct2D::D2D1_LAYER_PARAMETERS {
                    contentBounds: outline.rect,
                    geometricMask: std::mem::ManuallyDrop::new(Some(geo.into())),
                    maskAntialiasMode:
                        windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                    maskTransform: windows::Foundation::Numerics::Matrix3x2::identity(),
                    opacity: 1.0,
                    opacityBrush: std::mem::ManuallyDrop::new(None),
                    layerOptions: windows::Win32::Graphics::Direct2D::D2D1_LAYER_OPTIONS_NONE,
                };
                t.PushLayer(&params, &layer);

                let seg = |x0: f32, x1: f32, c: Rgb| {
                    if x1 > x0
                        && let Ok(b) = t.CreateSolidColorBrush(&colour(c), None)
                    {
                        t.FillRectangle(&rect(x0, 477.0, x1, 503.0), &b);
                    }
                };
                seg(MARGIN, x_dim, p.level_full);
                seg(x_dim, x_away, crate::ui::theme::dim_fill(p, e.dim_level));
                seg(x_away, CONTENT_R, p.level_black);
                t.PopLayer();
            }
            t.DrawRoundedRectangle(&outline, hair, 1.0, None);

            // The powered-down tail: an outline and a label rather than a
            // colour, so "black" and "off" are told apart without chroma.
            t.FillRectangle(&rect(x_deep, 477.0, x_deep + 1.0, 503.0), strong);

            // The camera-opens dot sits on the track's top edge.
            if let Ok(b) = t.CreateSolidColorBrush(&colour(p.good), None) {
                let dot = D2D1_ROUNDED_RECT {
                    rect: rect(x_idle - 3.0, 474.0, x_idle + 3.0, 480.0),
                    radiusX: 3.0,
                    radiusY: 3.0,
                };
                t.FillRoundedRectangle(&dot, &b);
            }

            for x in [x_idle, x_dim, x_away, x_deep] {
                let h = D2D1_ROUNDED_RECT {
                    rect: rect(x - 7.0, 462.0, x + 7.0, 476.0),
                    radiusX: 7.0,
                    radiusY: 7.0,
                };
                t.FillRoundedRectangle(&h, handle);
            }
            let _ = label;
        }
    }
}

/// The top of the gauge's scale. Real faces live in 0.05..0.45; 0.60 gives
/// headroom without wasting two thirds of the column.
const GAUGE_TOP: f32 = 0.60;

/// Name and sub-line for a state. Says the consequence, never the mechanism.
fn state_copy(state: State, dim_level: u8) -> (&'static str, String) {
    match state {
        State::Active => ("Active", "You are here \u{2014} camera closed.".into()),
        State::Watching => ("Watching", "Camera open, watching for absence.".into()),
        State::Dimmed => ("Dimmed", format!("Screen at {dim_level}%.")),
        State::Away => ("Away", "Screen black, panel still powered.".into()),
        State::Deep => ("Deep", "Monitor powered down.".into()),
        State::Paused => ("Paused", "Nothing will dim until you resume.".into()),
        State::Degraded => ("Degraded", "Camera failed \u{2014} dimming is off.".into()),
    }
}

/// One sentence saying what the measurement means for the user.
fn verdict(signal: SignalState, low: Option<f32>, threshold: f32) -> String {
    match (signal, low) {
        (SignalState::Good, Some(lo)) => {
            let head = ((lo / threshold - 1.0) * 100.0).round() as i32;
            format!("Clear by {head}% \u{2014} lowest reading {lo:.2}.")
        }
        (SignalState::Marginal, Some(lo)) => {
            let head = ((lo / threshold - 1.0) * 100.0).round() as i32;
            format!(
                "Only {head}% above the line \u{2014} one lean back and VISOR will think you left. Try {:.2}.",
                suggested(lo)
            )
        }
        (SignalState::Below, Some(lo)) => format!(
            "Too small \u{2014} VISOR would treat you as away. Try min_face_ratio {:.2}.",
            suggested(lo)
        ),
        (SignalState::NoFace, _) => {
            "Camera is running but sees no face. Check the angle and the lighting.".into()
        }
        (SignalState::Unavailable, _) => "Turn on the preview to check what VISOR can see.".into(),
        _ => String::new(),
    }
}

/// The two diagnostic lines. A fact with a consequence, not a warning.
fn diagnostics(st: &WindowStatus) -> (String, String) {
    let monitor = if st.monitor.is_empty() {
        "no monitor selected".to_string()
    } else {
        st.monitor.clone()
    };
    if st.ddc && st.brightness_confirmed {
        (
            format!("[DDC/CI]  {monitor}"),
            format!("Backlight dimming to {}%.", st.dim_level),
        )
    } else if st.ddc {
        (
            format!("[DDC/CI]  {monitor}"),
            "Brightness writes are not confirmed (HDR?) \u{2014} dimming uses a black overlay."
                .into(),
        )
    } else {
        (
            format!("[Overlay]  {monitor}"),
            "No DDC/CI on this monitor \u{2014} VISOR covers the screen instead of dimming the backlight.".into(),
        )
    }
}

impl Drop for TuningWindow {
    fn drop(&mut self) {
        if !self.hwnd.0.is_null() {
            // SAFETY: destroyed exactly once, here.
            unsafe {
                let _ = DestroyWindow(self.hwnd);
            }
        }
    }
}

fn register_class() {
    use std::sync::OnceLock;
    static REGISTERED: OnceLock<()> = OnceLock::new();
    REGISTERED.get_or_init(|| {
        let name = wide(CLASS_NAME);
        // SAFETY: `name` outlives the call; the cursor is a system resource.
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            lpszClassName: PCWSTR(name.as_ptr()),
            hCursor: unsafe { LoadCursorW(None, IDC_ARROW).unwrap_or_default() },
            ..Default::default()
        };
        // SAFETY: `wc` is fully initialised and `name` is still alive.
        if unsafe { RegisterClassW(&wc) } == 0 {
            tracing::error!("RegisterClassW failed for the tuning window class");
        }
    });
}

/// # Safety
/// Called by Windows with valid message parameters.
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    // SAFETY: the pointer stored in GWLP_USERDATA is the `Box<TuningWindow>`
    // handed to CreateWindowExW, which the caller keeps alive for as long as
    // the window exists.
    unsafe {
        if msg == WM_NCCREATE {
            let cs = l.0 as *const windows::Win32::UI::WindowsAndMessaging::CREATESTRUCTW;
            if !cs.is_null() {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, (*cs).lpCreateParams as isize);
            }
            return DefWindowProcW(hwnd, msg, w, l);
        }
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const TuningWindow;
        let this = ptr.as_ref();

        match msg {
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let _ = BeginPaint(hwnd, &mut ps);
                if let Some(this) = this {
                    this.paint();
                }
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            // Closing hides to the tray; VISOR keeps running. Quitting is a
            // separate, deliberate act from the tray menu or the footer.
            // The client can still change size under us (DPI moves, a theme
            // change). A render target left at the old size stretches or
            // clips everything, so follow it.
            WM_SIZE => {
                if let Some(this) = this
                    && let Some(r) = this.renderer.borrow().as_ref()
                {
                    let w = (l.0 & 0xFFFF) as u32;
                    let h = ((l.0 >> 16) & 0xFFFF) as u32;
                    if w > 0 && h > 0 {
                        let _ = r.target.Resize(&D2D_SIZE_U {
                            width: w,
                            height: h,
                        });
                    }
                }
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                if let Some(this) = this {
                    let x = (l.0 & 0xFFFF) as i16 as f32;
                    let y = ((l.0 >> 16) & 0xFFFF) as i16 as f32;
                    this.drag_to(x, y);
                }
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                if let Some(this) = this {
                    this.end_drag();
                }
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                if let Some(this) = this {
                    let x = (l.0 & 0xFFFF) as i16 as f32;
                    let y = ((l.0 >> 16) & 0xFFFF) as i16 as f32;
                    this.on_click(x, y);
                }
                LRESULT(0)
            }
            WM_CLOSE => {
                if let Some(this) = this {
                    this.hide();
                }
                LRESULT(0)
            }
            WM_DESTROY => LRESULT(0),
            _ => DefWindowProcW(hwnd, msg, w, l),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_is_the_size_the_design_specifies() {
        assert_eq!((WIN_W, WIN_H), (420.0, 696.0));
    }

    /// Run explicitly:
    /// `cargo test --lib tuning_window_is_visible_by_eye -- --ignored --nocapture`
    #[test]
    #[ignore = "manual: shows the window and needs a human to look at it"]
    fn tuning_window_is_visible_by_eye() {
        use windows::Win32::UI::WindowsAndMessaging::{
            DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
        };
        let themes = [Theme::Dark, Theme::Oled, Theme::Light];
        let Some(w) = TuningWindow::create(Theme::Dark) else {
            panic!("could not create the tuning window");
        };
        w.show();
        for theme in themes {
            println!("showing {theme:?} for 4s");
            w.set_theme(theme);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
            while std::time::Instant::now() < deadline {
                let mut msg = MSG::default();
                // SAFETY: a standard pump; `msg` is a valid owned MSG.
                while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
                    // SAFETY: `msg` was just filled by a successful PeekMessageW.
                    unsafe {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(16));
            }
        }
        w.hide();
    }

    fn frame(w: u32, h: u32, face_h: u32) -> PreviewFrame {
        PreviewFrame {
            width: w,
            height: h,
            luma: (0..(w * h)).map(|i| (i % 255) as u8).collect(),
            faces: vec![crate::sense::preview::FaceBox {
                x: w / 4,
                y: h / 4,
                w: face_h / 2,
                h: face_h,
            }],
        }
    }

    #[test]
    fn the_button_is_clickable_where_it_is_drawn() {
        // One rect feeds both the painter and the hit test. Two copies of
        // these numbers is exactly how a button ends up drawn in one place
        // and clickable in another.
        let (l, t, r, b) = PREVIEW_BTN_IDLE;
        assert!(
            hit(PREVIEW_BTN_IDLE, (l + r) / 2.0, (t + b) / 2.0),
            "centre"
        );
        assert!(hit(PREVIEW_BTN_IDLE, l, t), "top-left corner is inside");
        assert!(!hit(PREVIEW_BTN_IDLE, r, b), "bottom-right is exclusive");
        assert!(!hit(PREVIEW_BTN_IDLE, l - 1.0, (t + b) / 2.0));
        // Both positions sit within the plate they are drawn on.
        for (bl, bt, br, bb) in [PREVIEW_BTN_IDLE, PREVIEW_BTN_LIVE] {
            assert!(
                bl >= PLATE.0 && br <= PLATE.2 && bt >= PLATE.1 && bb <= PLATE.3,
                "button {bl},{bt},{br},{bb} escapes the plate"
            );
        }
        // The live position must clear the middle of the plate, or it lands on
        // the face it is supposed to let you see.
        let (_, _, _, live_bottom) = PREVIEW_BTN_LIVE;
        assert!(
            live_bottom < (PLATE.1 + PLATE.3) / 2.0,
            "the live button must stay out of the centre of the frame"
        );
    }

    #[test]
    fn clicking_the_button_asks_the_engine_rather_than_acting() {
        // The window does not own the camera or the command channel. It
        // leaves a note; the pump posts it. That is what keeps the label, the
        // engine and the camera LED from ever disagreeing.
        let Some(w) = TuningWindow::create(Theme::Dark) else {
            panic!("could not create the tuning window");
        };
        assert_eq!(w.take_preview_request(), None, "nothing pending at rest");

        let (l, t, r, b) = PREVIEW_BTN_IDLE;
        w.on_click((l + r) / 2.0, (t + b) / 2.0);
        assert_eq!(w.take_preview_request(), Some(true));
        assert_eq!(w.take_preview_request(), None, "draining consumes it");

        // Once live the button MOVES off the middle of the plate, because the
        // middle of the plate is now the user's face. The old spot must go
        // dead and the new one must work.
        w.set_preview_state(true);
        w.on_click((l + r) / 2.0, (t + b) / 2.0);
        assert_eq!(
            w.take_preview_request(),
            None,
            "the idle position must not stay clickable once the button moved"
        );
        let (l2, t2, r2, b2) = PREVIEW_BTN_LIVE;
        w.on_click((l2 + r2) / 2.0, (t2 + b2) / 2.0);
        assert_eq!(w.take_preview_request(), Some(false), "it toggles");
    }

    #[test]
    fn a_click_outside_the_button_does_nothing() {
        let Some(w) = TuningWindow::create(Theme::Dark) else {
            panic!("could not create the tuning window");
        };
        w.on_click(5.0, 5.0);
        w.on_click(WIN_W - 5.0, WIN_H - 5.0);
        assert_eq!(w.take_preview_request(), None);
    }

    #[test]
    fn hiding_the_window_gives_the_camera_back() {
        // The one rule that must not have an exception: no path may leave the
        // lens held open behind a window nobody can see.
        let Some(w) = TuningWindow::create(Theme::Dark) else {
            panic!("could not create the tuning window");
        };
        w.set_preview_state(true);
        let _ = w.take_preview_request();
        w.hide();
        assert_eq!(
            w.take_preview_request(),
            Some(false),
            "hiding must release the camera"
        );
    }

    #[test]
    fn hiding_a_window_that_never_held_the_camera_asks_for_nothing() {
        let Some(w) = TuningWindow::create(Theme::Dark) else {
            panic!("could not create the tuning window");
        };
        w.hide();
        assert_eq!(w.take_preview_request(), None);
    }

    #[test]
    fn a_frame_is_drawn_letterboxed_without_crashing() {
        // Exercises the whole upload path -- luminance expanded to BGRA, the
        // bitmap created then reused, the brackets scaled onto it -- at two
        // aspect ratios, because the letterbox arithmetic is where cropping
        // would silently creep back in.
        let Some(w) = TuningWindow::create(Theme::Dark) else {
            panic!("could not create the tuning window");
        };
        w.set_preview_state(true);
        w.set_frame(frame(640, 480, 120)); // 4:3, fills the plate
        w.paint();
        assert!(w.bitmap.borrow().is_some(), "a bitmap should be cached");

        w.set_frame(frame(640, 360, 90)); // 16:9, letterboxed
        w.paint();
        let cached = w.bitmap.borrow().as_ref().map(|(_, bw, bh)| (*bw, *bh));
        assert_eq!(
            cached,
            Some((640, 360)),
            "the bitmap follows the frame size"
        );

        // Same size again must reuse rather than recreate.
        w.set_frame(frame(640, 360, 60));
        w.paint();
        assert_eq!(
            w.bitmap.borrow().as_ref().map(|(_, bw, bh)| (*bw, *bh)),
            Some((640, 360))
        );
    }

    #[test]
    fn a_malformed_frame_is_refused_rather_than_read_past() {
        let Some(w) = TuningWindow::create(Theme::Dark) else {
            panic!("could not create the tuning window");
        };
        w.set_preview_state(true);
        w.set_frame(PreviewFrame {
            width: 64,
            height: 64,
            luma: vec![7; 10], // far too short for what it claims
            faces: Vec::new(),
        });
        w.paint(); // must not panic or read out of bounds
        assert!(w.bitmap.borrow().is_none(), "nothing should be uploaded");
    }

    #[test]
    fn turning_the_preview_off_drops_the_last_frame() {
        // Otherwise the plate keeps showing a picture of the user beside a
        // chip that says the camera is off.
        let Some(w) = TuningWindow::create(Theme::Dark) else {
            panic!("could not create the tuning window");
        };
        w.set_preview_state(true);
        w.set_frame(frame(64, 48, 12));
        assert!(w.frame.borrow().is_some());
        w.set_preview_state(false);
        assert!(w.frame.borrow().is_none(), "a stale frame is not a preview");
    }

    fn win() -> Box<TuningWindow> {
        match TuningWindow::create(Theme::Dark) {
            Some(w) => w,
            None => panic!("could not create the tuning window"),
        }
    }

    #[test]
    fn the_threshold_follows_the_pointer_and_survives_release() {
        let w = win();
        let (gt, gb) = w.gauge_extent.get();
        let axis = gauge_axis(gt, gb);
        let start = w.edits.get().threshold;

        // Grab it where it is drawn, then drag upward (a smaller pixel y).
        let y = axis.pixel_of(start);
        assert_eq!(w.hit_handle(370.0, y), Some(Grab::Threshold));
        w.on_click(370.0, y);
        w.drag_to(370.0, y - 40.0);

        let moved = w.edits.get().threshold;
        assert!(moved > start, "dragging up must raise the line: {moved}");
        assert!(w.take_edits().is_none(), "not saved until released");
        w.end_drag();
        assert_eq!(w.take_edits().map(|e| e.threshold), Some(moved));
        assert!(w.take_edits().is_none(), "draining consumes it");
    }

    #[test]
    fn the_threshold_cannot_leave_the_scale() {
        let w = win();
        let (gt, gb) = w.gauge_extent.get();
        w.on_click(370.0, gauge_axis(gt, gb).pixel_of(w.edits.get().threshold));
        w.drag_to(370.0, gb + 500.0);
        assert!(w.edits.get().threshold >= 0.02, "0.0 would match noise");
        w.drag_to(370.0, gt - 500.0);
        assert!(
            w.edits.get().threshold <= GAUGE_TOP,
            "off the top of the gauge"
        );
    }

    #[test]
    fn moving_the_camera_marker_slides_the_whole_sequence() {
        // The three ladder values are stored RELATIVE to idle_grace, so this
        // falls out of the representation. Asserting it pins the behaviour
        // against someone later "fixing" them to be absolute.
        let w = win();
        let before = w.edits.get();
        let a = rail_axis();

        let x = a.pixel_of(before.idle_grace);
        assert_eq!(w.hit_handle(x, 469.0), Some(Grab::IdleGrace));
        w.on_click(x, 469.0);
        w.drag_to(a.pixel_of(60.0), 469.0);
        w.end_drag();

        let after = w.edits.get();
        assert!(after.idle_grace > before.idle_grace, "the camera moved");
        assert_eq!(
            after.dim_after, before.dim_after,
            "relative values untouched"
        );
        assert_eq!(after.away_after, before.away_after);
        assert_eq!(after.deep_after, before.deep_after);
    }

    #[test]
    fn a_ladder_marker_stops_dead_against_its_neighbour() {
        // Hard clamp, not cascade: dragging dim past black must not silently
        // shove black further out. You went to change one number.
        let w = win();
        let before = w.edits.get();
        let a = rail_axis();

        let x = a.pixel_of(before.idle_grace + before.dim_after);
        assert_eq!(w.hit_handle(x, 469.0), Some(Grab::DimAfter));
        w.on_click(x, 469.0);
        // Aim far past black.
        w.drag_to(a.pixel_of(before.idle_grace + before.deep_after), 469.0);
        w.end_drag();

        let after = w.edits.get();
        assert!(after.dim_after < after.away_after, "order must hold");
        assert_eq!(
            after.away_after, before.away_after,
            "the neighbour must not have been pushed"
        );
    }

    #[test]
    fn a_status_push_does_not_yank_the_handle_out_of_your_hand() {
        // The pump pushes status every 250ms. Mid-drag that would reseat the
        // value from config and the handle would snap back under the mouse.
        let w = win();
        let (gt, gb) = w.gauge_extent.get();
        let y = gauge_axis(gt, gb).pixel_of(w.edits.get().threshold);
        w.on_click(370.0, y);
        w.drag_to(370.0, y - 30.0);
        let held = w.edits.get().threshold;

        w.set_status(WindowStatus {
            threshold: 0.42,
            dim_level: 77,
            ..WindowStatus::default()
        });
        assert_eq!(w.edits.get().threshold, held, "the drag wins");

        // And after release, unsaved edits still win until they are drained.
        w.end_drag();
        w.set_status(WindowStatus {
            threshold: 0.42,
            monitor: "x".into(),
            ..WindowStatus::default()
        });
        assert_eq!(w.edits.get().threshold, held, "unsaved edits are not stale");

        // Once drained, a push seeds normally again.
        let _ = w.take_edits();
        w.set_status(WindowStatus {
            threshold: 0.42,
            monitor: "y".into(),
            ..WindowStatus::default()
        });
        assert_eq!(w.edits.get().threshold, 0.42);
    }

    #[test]
    fn the_dim_slider_snaps_to_fives() {
        // Nobody wants to hunt for 37%.
        let w = win();
        w.on_click(dim_axis().pixel_of(20.0), 556.0);
        w.drag_to(dim_axis().pixel_of(63.0), 556.0);
        let v = w.edits.get().dim_level;
        assert_eq!(v % 5, 0, "snapped to a five: {v}");
        assert!((60..=65).contains(&v), "near where it was dragged: {v}");
        w.end_drag();
        assert_eq!(w.take_edits().map(|e| e.dim_level), Some(v));
    }

    #[test]
    fn clicking_empty_space_grabs_nothing() {
        let w = win();
        assert_eq!(w.hit_handle(200.0, 420.0), None, "between sections");
        assert_eq!(w.hit_handle(10.0, 690.0), None, "the footer");
        w.on_click(200.0, 420.0);
        w.drag_to(200.0, 300.0); // a drag with nothing held must do nothing
        w.end_drag();
        assert!(w.take_edits().is_none(), "nothing was edited");
    }

    #[test]
    fn painting_does_not_crash() {
        // The paint path -- render target creation, eight text formats, the
        // clipped rail layer -- had never executed before this test. All of it
        // fails softly by design (VISOR must keep dimming), so without this a
        // window that draws nothing at all looks exactly like a working one.
        let Some(w) = TuningWindow::create(Theme::Oled) else {
            panic!("could not create the tuning window");
        };
        w.paint();
        w.set_theme(Theme::Light);
        w.paint();
        assert!(
            w.renderer.borrow().is_some(),
            "a successful paint must leave a live render target behind"
        );
    }

    #[test]
    fn a_tuning_window_can_be_created_and_starts_hidden() {
        // Creation is the part that fails on a bad window class or a missing
        // D2D factory, and it fails silently by design (VISOR must keep
        // dimming), so a test is the only thing that would notice.
        let Some(w) = TuningWindow::create(Theme::Dark) else {
            panic!("could not create the tuning window");
        };
        assert!(!w.is_visible(), "must not appear until asked for");
        assert!(!w.hwnd.0.is_null(), "a real HWND was created");
    }

    // ---- the settings page -------------------------------------------------

    /// Centre of the option `index` in the row for `setting`, in window
    /// coordinates. Uses the same table the painter and the hit test use, so a
    /// test cannot pass by clicking somewhere nothing is drawn.
    fn segment_centre(setting: settings::Setting, index: usize) -> (f32, f32) {
        let row = settings::rows()
            .find(|r| r.setting == setting)
            .expect("the page must have a row for this setting");
        let (l, t, r, b) = settings::segment_rect(row, index);
        ((l + r) / 2.0, (t + b) / 2.0)
    }

    #[test]
    fn the_footer_button_opens_the_settings_page_and_back_returns() {
        let w = win();
        assert_eq!(w.page.get(), Page::Instrument);
        w.on_click(
            (SETTINGS_BTN.0 + SETTINGS_BTN.2) / 2.0,
            (SETTINGS_BTN.1 + SETTINGS_BTN.3) / 2.0,
        );
        assert_eq!(w.page.get(), Page::Settings);
        w.on_click(
            (BACK_BTN.0 + BACK_BTN.2) / 2.0,
            (BACK_BTN.1 + BACK_BTN.3) / 2.0,
        );
        assert_eq!(w.page.get(), Page::Instrument);
    }

    #[test]
    fn clicking_a_segment_changes_that_setting_and_asks_for_a_save() {
        let w = win();
        w.page.set(Page::Settings);
        let before = w.settings.get();
        assert!(w.take_settings().is_none(), "nothing to save yet");

        // Default face_confirm is 2, which is index 1; move it to 4.
        let (x, y) = segment_centre(settings::Setting::FaceConfirm, 3);
        w.on_click(x, y);

        let saved = w.take_settings().expect("a click must ask for a save");
        assert_eq!(saved.face_confirm, 4);
        assert_eq!(
            Settings {
                face_confirm: before.face_confirm,
                ..saved
            },
            before,
            "a click must move exactly one setting"
        );
        assert!(w.take_settings().is_none(), "draining consumes it");
    }

    #[test]
    fn clicking_the_segment_already_lit_asks_for_nothing() {
        // Otherwise every stray click on the current value costs a file write
        // and a full engine reload.
        let w = win();
        w.page.set(Page::Settings);
        let (x, y) = segment_centre(settings::Setting::FaceConfirm, 1);
        w.on_click(x, y);
        assert!(w.take_settings().is_none());
    }

    #[test]
    fn a_config_push_cannot_revert_a_click_that_is_not_yet_saved() {
        // The pump pushes every 250ms from a `cfg` it only re-reads on Reload,
        // so the push that lands between the click and the save still carries
        // the OLD value. Adopting it would make the click look like it bounced.
        let w = win();
        w.page.set(Page::Settings);
        let stale = w.settings.get();
        let (x, y) = segment_centre(settings::Setting::WakeConfirm, 2);
        w.on_click(x, y);

        w.set_settings(stale);
        assert_eq!(w.settings.get().wake_confirm, 3, "the click must survive");
        assert_eq!(w.take_settings().map(|s| s.wake_confirm), Some(3));

        // Once drained, a push is welcome again.
        w.set_settings(stale);
        assert_eq!(w.settings.get(), stale);
    }

    #[test]
    fn the_instrument_controls_are_dead_while_the_settings_page_shows() {
        // The rail and the gauge are still hit-testable arithmetic; only the
        // page check stops a click at a marker's coordinates from grabbing it
        // through a page that does not draw it.
        let w = win();
        let a = rail_axis();
        let (x, y) = (a.pixel_of(w.edits.get().idle_grace), RAIL_TOP + 4.0);
        assert_eq!(w.hit_handle(x, y), Some(Grab::IdleGrace));

        w.page.set(Page::Settings);
        w.on_click(x, y);
        assert!(
            w.grab.get().is_none(),
            "the sequence rail is not on this page and must not be grabbable"
        );
    }

    #[test]
    fn painting_the_settings_page_does_not_crash() {
        // Same reason as `painting_does_not_crash`: this walks a table of
        // eleven blocks through DirectWrite and Direct2D, and nothing else
        // would notice a bad rect until it was on screen.
        let w = win();
        w.page.set(Page::Settings);
        w.paint();
        assert!(w.renderer.borrow().is_some());
    }

    #[test]
    fn a_theme_click_writes_a_theme_the_window_can_read_back() {
        // `theme` is the one setting that round-trips through a String, and it
        // is the window's own palette on the other side of that trip.
        let w = win();
        w.page.set(Page::Settings);
        let (x, y) = segment_centre(settings::Setting::Theme, 2);
        w.on_click(x, y);
        let s = w.take_settings().expect("a theme click must save");
        let mut cfg = crate::config::Config::default();
        s.write_into(&mut cfg);
        assert_eq!(Theme::parse(&cfg.ui.theme), Theme::Oled);
        assert!(cfg.validate().is_ok());
    }
}
