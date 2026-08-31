use crate::actions::DisplayControl;
use crate::actions::monitors::{self};
use crate::core::types::DisplayLevel;
use std::sync::OnceLock;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::CreateSolidBrush;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, LWA_ALPHA, RegisterClassW, SW_HIDE,
    SW_SHOWNOACTIVATE, SetLayeredWindowAttributes, ShowWindow, WNDCLASSW, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
#[cfg(test)]
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
};
use windows::core::PCWSTR;

const CLASS_NAME: &str = "VISOR.OverlayWindow";

/// How opaque the black overlay must be to represent `level`.
pub fn alpha_for(level: DisplayLevel) -> u8 {
    match level {
        DisplayLevel::Full => 0,
        DisplayLevel::Dim(p) => {
            let p = p.min(100) as u16;
            (((100 - p) * 255) / 100) as u8
        }
        DisplayLevel::Black | DisplayLevel::Off => 255,
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Standard passthrough — the overlay never customizes message handling, so
/// every message goes straight to the default procedure.
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: `hwnd`/`wparam`/`lparam` are handed to us unchanged by the
    // Windows message dispatcher and passed straight through; we never
    // interpret them ourselves.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// A null instance handle. These overlay windows load no icons or resources
/// from a module, so there is nothing `GetModuleHandleW` would buy us — a
/// null `HINSTANCE` is valid here and keeps this file off the
/// `Win32_System_LibraryLoader` feature the brief did not ask for.
fn no_hinstance() -> HINSTANCE {
    HINSTANCE(std::ptr::null_mut())
}

fn register_class_once() {
    static REGISTERED: OnceLock<()> = OnceLock::new();
    REGISTERED.get_or_init(|| {
        let name = wide(CLASS_NAME);
        // SAFETY: `CreateSolidBrush` with a valid COLORREF cannot fail in
        // practice; if it somehow returned a null brush the class would just
        // paint with no background brush, which is not correctness-critical
        // here — `SetLayeredWindowAttributes` is what actually darkens the
        // screen.
        let brush = unsafe { CreateSolidBrush(COLORREF(0)) };
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: no_hinstance(),
            hbrBackground: brush,
            lpszClassName: PCWSTR(name.as_ptr()),
            ..Default::default()
        };
        // SAFETY: `wc` is a fully initialized `WNDCLASSW`, and `name` (the
        // buffer backing `lpszClassName`) is still alive for this whole call.
        let atom = unsafe { RegisterClassW(&wc) };
        if atom == 0 {
            tracing::error!("RegisterClassW failed for the VISOR overlay window class");
        }
    });
}

/// Creates one topmost, click-through, layered popup window covering `rect`.
/// Returns `None` (after logging) rather than panicking — fail toward the
/// screen staying on: a monitor we can't cover is skipped, not fatal.
fn create_window(rect: RECT) -> Option<isize> {
    register_class_once();
    let class_name = wide(CLASS_NAME);
    let ex_style =
        WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW;
    // SAFETY: `class_name` is a valid null-terminated UTF-16 buffer alive for
    // the duration of this call. No window text, parent, menu, or creation
    // param are needed for a topmost overlay popup.
    let hwnd = unsafe {
        CreateWindowExW(
            ex_style,
            PCWSTR(class_name.as_ptr()),
            PCWSTR::null(),
            WS_POPUP,
            rect.left,
            rect.top,
            rect.right - rect.left,
            rect.bottom - rect.top,
            None,
            None,
            no_hinstance(),
            None,
        )
    };
    match hwnd {
        Ok(h) => Some(h.0 as isize),
        Err(e) => {
            tracing::warn!(error = %e, "CreateWindowExW failed for an overlay monitor; skipping it");
            None
        }
    }
}

/// One layered overlay window. Kept as `isize` rather than `HWND` so
/// `OverlayControl` stays `Send` (it is only ever driven from the thread that
/// created it, per the contract on `OverlayControl` itself).
struct OverlayWindow {
    handle: isize,
}

impl OverlayWindow {
    fn hwnd(&self) -> HWND {
        HWND(self.handle as *mut core::ffi::c_void)
    }

    fn set_alpha(&self, alpha: u8) {
        if alpha == 0 {
            // Hide rather than destroy — instant restore is the entire
            // reason `Away` uses the overlay rather than DPMS.
            // SAFETY: `self.hwnd()` was created by `create_window` and has
            // not been destroyed while this `OverlayWindow` is alive.
            unsafe {
                let _ = ShowWindow(self.hwnd(), SW_HIDE);
            }
            return;
        }
        // SAFETY: `self.hwnd()` is a live window created with
        // `WS_EX_LAYERED`, so `SetLayeredWindowAttributes` is valid to call
        // on it.
        unsafe {
            if let Err(e) = SetLayeredWindowAttributes(self.hwnd(), COLORREF(0), alpha, LWA_ALPHA) {
                tracing::warn!(error = %e, "SetLayeredWindowAttributes failed on an overlay window");
            }
            let _ = ShowWindow(self.hwnd(), SW_SHOWNOACTIVATE);
        }
    }
}

impl Drop for OverlayWindow {
    fn drop(&mut self) {
        // SAFETY: `self.hwnd()` was created by `create_window` and is
        // destroyed at most once, here.
        unsafe {
            let _ = DestroyWindow(self.hwnd());
        }
    }
}

/// Layered black overlays, one per target monitor.
///
/// **Must be constructed and driven from the thread that owns the message
/// pump.** These are top-level windows; on a thread that never pumps, any
/// system-wide `SendMessage` broadcast (`WM_SETTINGCHANGE`, `WM_DISPLAYCHANGE`)
/// blocks until timeout — stalling the sender, not just VISOR. Task 11 routes
/// creation to the pump thread; until then `main` keeps using `SpyDisplay`.
pub struct OverlayControl {
    windows: Vec<OverlayWindow>,
}

impl OverlayControl {
    /// Overlay windows for every monitor (spec §7: empty `targets` = all).
    pub fn new() -> Self {
        Self::for_targets(&[])
    }

    /// Overlay windows for the monitors spec §7 `display.targets` selects.
    pub fn for_targets(targets: &[String]) -> Self {
        let all = monitors::enumerate();
        let chosen = monitors::select(all, targets);
        let mut windows = Vec::new();
        for m in &chosen {
            match create_window(m.rect) {
                Some(handle) => windows.push(OverlayWindow { handle }),
                None => {
                    // Fail toward the screen staying on: a partial overlay
                    // beats a panic.
                    tracing::warn!(
                        monitor = %m.description,
                        "failed to create an overlay window for this monitor; continuing without it"
                    );
                }
            }
        }
        Self { windows }
    }
}

impl Default for OverlayControl {
    fn default() -> Self {
        Self::new()
    }
}

impl DisplayControl for OverlayControl {
    fn apply(&mut self, level: DisplayLevel) {
        let alpha = alpha_for(level);
        for w in &self.windows {
            w.set_alpha(alpha);
        }
    }
}

/// Pumps this thread's message queue until `dur` elapses. For tests and other
/// short-lived callers only — production overlay driving happens on the real
/// message-pump thread (see the `OverlayControl` doc comment / ruling F7).
#[cfg(test)]
fn pump_for(dur: std::time::Duration) {
    let deadline = std::time::Instant::now() + dur;
    while std::time::Instant::now() < deadline {
        let mut msg = MSG::default();
        // SAFETY: standard message pump; `msg` is a valid, owned MSG and
        // `PeekMessageW` only writes into it.
        while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
            // SAFETY: `msg` was just filled by a successful `PeekMessageW`.
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alpha_for_dim_is_proportional_to_the_missing_brightness() {
        // Dim(100) means full brightness, so no darkening at all.
        assert_eq!(alpha_for(DisplayLevel::Dim(100)), 0);
        // Dim(20) means 80% darkened.
        assert_eq!(alpha_for(DisplayLevel::Dim(20)), 204);
        // Black and Off both mean fully opaque.
        assert_eq!(alpha_for(DisplayLevel::Black), 255);
        assert_eq!(alpha_for(DisplayLevel::Off), 255);
        assert_eq!(alpha_for(DisplayLevel::Full), 0);
    }

    #[test]
    fn enumerate_finds_at_least_one_monitor() {
        let ms = crate::actions::monitors::enumerate();
        assert!(!ms.is_empty(), "a running desktop must have a monitor");
        for m in &ms {
            assert!(m.rect.right > m.rect.left);
            assert!(m.rect.bottom > m.rect.top);
        }
    }

    #[test]
    fn empty_targets_means_every_monitor() {
        use crate::actions::monitors::{MonitorInfo, select};
        let all = vec![
            MonitorInfo {
                handle: 1,
                rect: Default::default(),
                description: r"\\.\DISPLAY1".into(),
            },
            MonitorInfo {
                handle: 2,
                rect: Default::default(),
                description: r"\\.\DISPLAY2".into(),
            },
        ];
        assert_eq!(
            select(all.clone(), &[]).len(),
            2,
            "empty targets = all monitors"
        );
        let one = select(all, &["DISPLAY2".to_string()]);
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].handle, 2);
    }

    /// Run explicitly: `cargo test --lib overlay_is_visible_by_eye -- --ignored --nocapture`
    #[test]
    #[ignore = "manual: requires a desktop session and a human to look at the screen"]
    fn overlay_is_visible_by_eye() {
        let mut o = OverlayControl::new();
        for (level, note) in [
            (DisplayLevel::Dim(20), "should be heavily dimmed"),
            (DisplayLevel::Black, "should be fully black"),
            (DisplayLevel::Full, "should be back to normal"),
        ] {
            println!("applying {level:?} — {note}");
            o.apply(level);
            pump_for(std::time::Duration::from_secs(3));
        }
    }
}
