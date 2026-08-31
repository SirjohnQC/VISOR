use crate::actions::DisplayControl;
use crate::actions::monitors::{self};
use crate::core::types::DisplayLevel;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::CreateSolidBrush;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, HWND_MESSAGE, LWA_ALPHA,
    PBT_APMRESUMEAUTOMATIC, RegisterClassW, SW_HIDE, SW_SHOWNOACTIVATE, SetLayeredWindowAttributes,
    ShowWindow, WINDOW_EX_STYLE, WINDOW_STYLE, WM_DISPLAYCHANGE, WM_POWERBROADCAST, WNDCLASSW,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
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

/// Set by the WndProc when Windows delivers `WM_DISPLAYCHANGE` (a monitor was
/// added, removed, or its mode changed). Cleared by `take_display_changed`.
static DISPLAY_CHANGED: AtomicBool = AtomicBool::new(false);

/// Set by the WndProc on `WM_POWERBROADCAST` / `PBT_APMRESUMEAUTOMATIC`
/// (the system resumed from sleep). Cleared by `take_resumed`.
static RESUMED: AtomicBool = AtomicBool::new(false);

/// Ruling F12: `PeekMessageW` in the tray pump only drains this *thread's*
/// queue, but `WM_DISPLAYCHANGE`/`WM_POWERBROADCAST` are delivered to a
/// *window*. The tray icon's window belongs to `tray-icon`'s own private
/// WndProc, which we do not control, so this overlay/broadcast window class's
/// WndProc — on the same thread — is where these must be caught instead.
/// Read-and-clear each iteration of the pump loop; see `ui::tray::run`.
pub fn take_display_changed() -> bool {
    DISPLAY_CHANGED.swap(false, Ordering::AcqRel)
}

/// Read-and-clear counterpart to `DISPLAY_CHANGED` for system resume.
pub fn take_resumed() -> bool {
    RESUMED.swap(false, Ordering::AcqRel)
}

/// Passthrough for everything except the two broadcasts VISOR cares about.
///
/// A WndProc must stay fast and must not block: for both messages this only
/// sets a flag and falls through to `DefWindowProcW`. It never rescans
/// monitors or sends a command itself — that work happens on the pump's next
/// iteration (same thread, but outside the message dispatcher's call stack).
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_DISPLAYCHANGE => {
            DISPLAY_CHANGED.store(true, Ordering::Release);
        }
        WM_POWERBROADCAST if wparam.0 as u32 == PBT_APMRESUMEAUTOMATIC => {
            RESUMED.store(true, Ordering::Release);
        }
        _ => {}
    }
    // SAFETY: `hwnd`/`wparam`/`lparam` are handed to us unchanged by the
    // Windows message dispatcher and passed straight through; we never
    // interpret them ourselves beyond the read-only match above.
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

/// A hidden, message-only window that exists solely to receive
/// `WM_DISPLAYCHANGE` and `WM_POWERBROADCAST` (ruling F12).
///
/// Overlay windows are not a reliable place to catch these: `Resolver::rescan`
/// destroys and recreates the whole set on every call, and if configured
/// `display.targets` matches zero monitors there may be no overlay window at
/// all. This window is independent of that lifecycle — created once when the
/// pump starts and kept alive for the whole run — so the broadcasts are
/// always caught regardless of how many (or how few) overlay windows exist at
/// any given moment.
///
/// Returns `None` (after logging) rather than panicking, matching
/// `create_window`'s fail-toward-the-screen-staying-on stance: if this
/// window can't be created, VISOR still runs, it just won't react to
/// display-change/resume broadcasts until the next tick's own state check.
pub(crate) fn create_broadcast_window() -> Option<OverlayWindow> {
    register_class_once();
    let class_name = wide(CLASS_NAME);
    // SAFETY: `class_name` is a valid null-terminated UTF-16 buffer alive for
    // the duration of this call. `HWND_MESSAGE` marks this as a message-only
    // window, so it needs no size, position, or visible style — it is never
    // shown and never receives anything but messages.
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(class_name.as_ptr()),
            PCWSTR::null(),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            None,
            no_hinstance(),
            None,
        )
    };
    match hwnd {
        Ok(h) => Some(OverlayWindow {
            handle: h.0 as isize,
        }),
        Err(e) => {
            tracing::error!(
                error = %e,
                "failed to create the message-only broadcast window; \
                 WM_DISPLAYCHANGE/WM_POWERBROADCAST will not be observed"
            );
            None
        }
    }
}

/// One layered overlay window. Kept as `isize` rather than `HWND` so
/// `OverlayControl` stays `Send` (it is only ever driven from the thread that
/// created it, per the contract on `OverlayControl` itself).
///
/// `pub(crate)` rather than private: `create_broadcast_window` below also
/// returns one, so the pump thread (`ui::tray::run`) can hold it alive for as
/// long as it needs a window to receive broadcasts, without exposing the
/// Win32 handle itself outside this module.
pub(crate) struct OverlayWindow {
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
    fn an_overlay_window_is_created_for_every_monitor() {
        // The visual test completing "without panicking" proves nothing: the
        // creation failure path deliberately logs and carries on, so a run in
        // which every CreateWindowExW failed looks identical. This asserts the
        // windows genuinely exist.
        let expected = crate::actions::monitors::enumerate().len();
        assert!(expected > 0, "a running desktop must have a monitor");

        let o = OverlayControl::new();
        assert_eq!(
            o.windows.len(),
            expected,
            "every enumerated monitor must get an overlay window"
        );
        // Dropping `o` destroys them; if DestroyWindow were wrong this would
        // leak a topmost black window over the desktop, which is loud enough
        // to notice.
    }

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
