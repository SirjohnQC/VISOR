use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    HWND_BROADCAST, SC_MONITORPOWER, SendMessageW, WM_SYSCOMMAND,
};

const MONITOR_ON: isize = -1;
const MONITOR_OFF: isize = 2;

/// Spec §6.3 — the quarantined mechanism of last resort.
///
/// `SendMessageW` to `HWND_BROADCAST` blocks until every top-level window in
/// the session responds or times out, and it is called from the same thread
/// that services VISOR's own windows. Callers must gate this behind
/// [`super::broadcast_allowed`], which is what keeps it off the common path.
pub fn set_power(on: bool) {
    // SAFETY: a broadcast WM_SYSCOMMAND with well-formed parameters; no
    // out-parameters and no pointers are passed.
    unsafe {
        SendMessageW(
            HWND_BROADCAST,
            WM_SYSCOMMAND,
            WPARAM(SC_MONITORPOWER as usize),
            LPARAM(if on { MONITOR_ON } else { MONITOR_OFF }),
        );
    }
}
