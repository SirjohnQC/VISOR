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

use crate::sense::preview::PreviewFrame;
use crate::ui::theme::{Palette, Rgb, Theme, palette};
use std::cell::Cell;
use std::cell::RefCell;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
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
    DWRITE_TEXT_ALIGNMENT, DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING,
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat,
};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, PAINTSTRUCT};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GWLP_USERDATA, GetWindowLongPtrW, HMENU,
    IDC_ARROW, LoadCursorW, RegisterClassW, SW_HIDE, SW_SHOW, SetWindowLongPtrW, ShowWindow,
    WM_CLOSE, WM_DESTROY, WM_LBUTTONDOWN, WM_NCCREATE, WM_PAINT, WNDCLASSW, WS_CAPTION,
    WS_EX_APPWINDOW, WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU,
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
/// The in-plate preview toggle. One rect, used by BOTH the painter and the
/// hit test -- two copies of these numbers is how a button ends up drawn in
/// one place and clickable in another.
const PREVIEW_BTN: (f32, f32, f32, f32) = (119.0, 280.0, 245.0, 312.0);

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

/// The eight cached text formats, one per level of the type scale.
struct Fonts {
    numeral: IDWriteTextFormat,
    title: IDWriteTextFormat,
    body: IDWriteTextFormat,
    body_strong: IDWriteTextFormat,
    caption: IDWriteTextFormat,
    micro: IDWriteTextFormat,
    section: IDWriteTextFormat,
    wordmark: IDWriteTextFormat,
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
            wordmark: make(13.0, 700)?,
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
    /// Set by a click, drained by the pump. The window cannot talk to the
    /// engine itself -- it does not own the command channel -- so it leaves a
    /// note and `ui::tray` posts it.
    pending_preview: Cell<Option<bool>>,
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
            pending_preview: Cell::new(None),
        });

        let class = wide(CLASS_NAME);
        let title = wide("VISOR");
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
                WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
                windows::Win32::UI::WindowsAndMessaging::CW_USEDEFAULT,
                windows::Win32::UI::WindowsAndMessaging::CW_USEDEFAULT,
                WIN_W as i32,
                WIN_H as i32,
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
    pub fn set_frame(&self, frame: PreviewFrame) {
        *self.frame.borrow_mut() = Some(frame);
        if self.is_visible() {
            self.invalidate();
        }
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
    fn on_click(&self, x: f32, y: f32) {
        if hit(PREVIEW_BTN, x, y) {
            self.pending_preview.set(Some(!self.preview_on.get()));
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

            let text =
                |s: &str, f: &IDWriteTextFormat, r_: D2D_RECT_F, b: &ID2D1SolidColorBrush| {
                    let w = wide(s);
                    // The trailing NUL is not part of the string to draw.
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

            // ---- title bar ------------------------------------------------
            text(
                "V I S O R",
                &r.fonts.wordmark,
                rect(MARGIN, 12.0, 200.0, 32.0),
                &b_t1,
            );
            t.FillRectangle(&rect(0.0, 39.0, WIN_W, 40.0), &b_hair);

            // ---- status band ----------------------------------------------
            let dot = D2D1_ROUNDED_RECT {
                rect: rect(MARGIN, 62.0, MARGIN + 8.0, 70.0),
                radiusX: 4.0,
                radiusY: 4.0,
            };
            t.FillRoundedRectangle(&dot, &b_t1);
            text(
                "Watching",
                &r.fonts.title,
                rect(38.0, 53.0, 300.0, 80.0),
                &b_t1,
            );
            text(
                "Camera open, watching for absence.",
                &r.fonts.caption,
                rect(MARGIN, 79.0, CONTENT_R, 96.0),
                &b_t2,
            );

            // ---- preview plate --------------------------------------------
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

            let frame = self.frame.borrow();
            if let Some(f) = frame.as_ref().filter(|f| f.width > 0 && f.height > 0) {
                // Letterbox, never crop. `largest_ratio` is FaceBox.Height over
                // the FRAME height, so cropping to fill would change the
                // effective height and the ratio drawn would stop matching the
                // ratio measured -- the one thing this window must not do.
                let (pw, ph) = (PLATE.2 - PLATE.0, PLATE.3 - PLATE.1);
                let scale = (pw / f.width as f32).min(ph / f.height as f32);
                let (dw, dh) = (f.width as f32 * scale, f.height as f32 * scale);
                let (dx, dy) = (PLATE.0 + (pw - dw) / 2.0, PLATE.1 + (ph - dh) / 2.0);

                if let Some(bmp) = self.upload(t, f) {
                    t.DrawBitmap(
                        &bmp,
                        Some(&rect(dx, dy, dx + dw, dy + dh)),
                        1.0,
                        D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                        None,
                    );
                }

                // Corner brackets, not a rectangle: brackets read as a
                // viewfinder, a closed box reads as surveillance.
                if let Some(b_good) = brush(p.good) {
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
                                &b_good,
                                2.0,
                                None,
                            );
                            t.DrawLine(
                                D2D_POINT_2F { x: cx, y: cy },
                                D2D_POINT_2F {
                                    x: cx,
                                    y: cy + arm * sy,
                                },
                                &b_good,
                                2.0,
                                None,
                            );
                        }
                    }
                }

                // The scrim carrying the promise that none of this is acted on.
                if let Some(b_scrim) = brush(Rgb(0, 0, 0)) {
                    t.FillRectangle(&rect(PLATE.0 + 1.0, 322.0, PLATE.2 - 1.0, 347.0), &b_scrim);
                }
                text(
                    "Tuning — VISOR will not dim while the preview is on.",
                    &r.fonts.micro,
                    rect(PLATE.0 + 10.0, 328.0, PLATE.2, 344.0),
                    &b_t2,
                );
            } else {
                text(
                    "Camera is closed",
                    &r.fonts.body,
                    rect(PLATE.0 + 100.0, 196.0, PLATE.2, 220.0),
                    &b_t2,
                );
                text(
                    "VISOR opens it only after 30 s without keyboard or mouse. Nothing it sees ever leaves this PC.",
                    &r.fonts.caption,
                    rect(PLATE.0 + 50.0, 222.0, PLATE.2 - 50.0, 274.0),
                    &b_t3,
                );
            }

            // The preview toggle, drawn from the same rect the hit test uses.
            let btn = D2D1_ROUNDED_RECT {
                rect: rect(PREVIEW_BTN.0, PREVIEW_BTN.1, PREVIEW_BTN.2, PREVIEW_BTN.3),
                radiusX: 6.0,
                radiusY: 6.0,
            };
            t.DrawRoundedRectangle(&btn, &b_strong, 1.0, None);
            text(
                if self.preview_on.get() {
                    "Turn off preview"
                } else {
                    "Turn on preview"
                },
                &r.fonts.body,
                rect(
                    PREVIEW_BTN.0 + 14.0,
                    PREVIEW_BTN.1 + 5.0,
                    PREVIEW_BTN.2,
                    PREVIEW_BTN.3,
                ),
                &b_t1,
            );

            // ---- gauge column ---------------------------------------------
            let track = D2D1_ROUNDED_RECT {
                rect: rect(362.0, 138.0, 382.0, 318.0),
                radiusX: 10.0,
                radiusY: 10.0,
            };
            t.FillRoundedRectangle(&track, &b_well);
            t.DrawRoundedRectangle(&track, &b_hair, 1.0, None);
            // the user's line
            t.FillRectangle(&rect(354.0, 273.0, 398.0, 274.5), &b_t1);

            // ---- readout ---------------------------------------------------
            text(
                "\u{2014} / 0.15",
                &r.fonts.numeral,
                rect(MARGIN, 353.0, CONTENT_R, 392.0),
                &b_t3,
            );
            text(
                "Turn on the preview to check what VISOR can see.",
                &r.fonts.body,
                rect(MARGIN, 388.0, CONTENT_R, 408.0),
                &b_t2,
            );

            // ---- sequence ---------------------------------------------------
            text(
                "S E Q U E N C E",
                &r.fonts.section,
                rect(MARGIN, 424.0, CONTENT_R, 440.0),
                &b_t3,
            );
            self.draw_rail(t, p, &b_hair, &b_strong, &b_t3);

            // ---- dim level ---------------------------------------------------
            text(
                "Dim to",
                &r.fonts.caption,
                rect(MARGIN, 548.0, 74.0, 566.0),
                &b_t2,
            );
            align(&r.fonts.body_strong, DWRITE_TEXT_ALIGNMENT_TRAILING);
            text(
                "20%",
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
            text(
                "Probing\u{2026}",
                &r.fonts.caption,
                rect(MARGIN, 606.0, CONTENT_R, 624.0),
                &b_t2,
            );

            // ---- footer -------------------------------------------------------
            t.FillRectangle(&rect(0.0, 654.0, WIN_W, 655.0), &b_hair);
            for (label, x, w) in [("Pause", MARGIN, 76.0), ("Reload config", 106.0, 106.0)] {
                let b = D2D1_ROUNDED_RECT {
                    rect: rect(x, 662.0, x + w, 690.0),
                    radiusX: 6.0,
                    radiusY: 6.0,
                };
                t.DrawRoundedRectangle(&b, &b_strong, 1.0, None);
                align(&r.fonts.body, DWRITE_TEXT_ALIGNMENT_LEADING);
                text(
                    label,
                    &r.fonts.body,
                    rect(x + 12.0, 667.0, x + w, 688.0),
                    &b_t2,
                );
            }
        }
    }

    /// The sequence rail: the segmented track whose fill brightness *is* the
    /// screen brightness at that point on the timeline.
    ///
    /// # Safety
    /// Must be called inside a draw pass.
    unsafe fn draw_rail(
        &self,
        t: &ID2D1HwndRenderTarget,
        p: &Palette,
        hair: &ID2D1SolidColorBrush,
        strong: &ID2D1SolidColorBrush,
        _t3: &ID2D1SolidColorBrush,
    ) {
        // SAFETY: caller guarantees a live draw pass.
        unsafe {
            let outline = D2D1_ROUNDED_RECT {
                rect: rect(MARGIN, 476.0, CONTENT_R, 504.0),
                radiusX: 6.0,
                radiusY: 6.0,
            };
            // Rounded outer ends with square inner seams: clip to the rounded
            // track, then fill plain rectangles inside it. Direct2D has no
            // per-corner radius, and this is the cheapest way to get one.
            if let Ok(geo) = self.d2d.CreateRoundedRectangleGeometry(&outline) {
                let layer = t.CreateLayer(None).ok();
                if let Some(layer) = layer {
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
                        if let Ok(b) = t.CreateSolidColorBrush(&colour(c), None) {
                            t.FillRectangle(&rect(x0, 477.0, x1, 503.0), &b);
                        }
                    };
                    // Defaults from config: camera opens at 30s, dim +20s,
                    // black +45s, off +15m -- laid out on the spec's log axis.
                    seg(MARGIN, 128.0, p.level_full);
                    seg(128.0, 156.0, p.level_full);
                    seg(156.0, 178.0, crate::ui::theme::dim_fill(p, 20));
                    seg(178.0, CONTENT_R, p.level_black);
                    t.PopLayer();
                }
            }
            t.DrawRoundedRectangle(&outline, hair, 1.0, None);
            // the "powered down" tail gets an outline rather than a colour
            t.FillRectangle(&rect(316.0, 477.0, 317.0, 503.0), strong);
        }
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
        let (l, t, r, b) = PREVIEW_BTN;
        assert!(hit(PREVIEW_BTN, (l + r) / 2.0, (t + b) / 2.0), "centre");
        assert!(hit(PREVIEW_BTN, l, t), "top-left corner is inside");
        assert!(!hit(PREVIEW_BTN, r, b), "bottom-right is exclusive");
        assert!(!hit(PREVIEW_BTN, l - 1.0, (t + b) / 2.0));
        // And it sits within the plate it is drawn on.
        assert!(l >= PLATE.0 && r <= PLATE.2 && t >= PLATE.1 && b <= PLATE.3);
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

        let (l, t, r, b) = PREVIEW_BTN;
        w.on_click((l + r) / 2.0, (t + b) / 2.0);
        assert_eq!(w.take_preview_request(), Some(true));
        assert_eq!(w.take_preview_request(), None, "draining consumes it");

        // Nothing changed until the pump says so.
        w.set_preview_state(true);
        w.on_click((l + r) / 2.0, (t + b) / 2.0);
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
}
