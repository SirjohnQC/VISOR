//! DDC/CI brightness and power control (spec §6.1, §12).
//!
//! Windows locks DDC/CI brightness while HDR is on: `SetVCPFeature(0x10, ..)`
//! silently does nothing. `set_brightness` always reads the value back after
//! writing so the caller (the mechanism resolver in Task 11) can detect a
//! write that didn't take and fall through to the overlay — the write is
//! never assumed to have succeeded just because the call returned.
//!
//! Every failure path here returns `false`/`None` rather than panicking,
//! per the fail-toward-lit-screen principle (spec §2.1, §8).

use windows::Win32::Devices::Display::{
    DestroyPhysicalMonitors, GetNumberOfPhysicalMonitorsFromHMONITOR,
    GetPhysicalMonitorsFromHMONITOR, GetVCPFeatureAndVCPFeatureReply, PHYSICAL_MONITOR,
    SetVCPFeature,
};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Graphics::Gdi::HMONITOR;

const VCP_BRIGHTNESS: u8 = 0x10;
const VCP_POWER_MODE: u8 = 0xD6;
/// Spec §6.1/§12: value 4 is power off, and is the only value ever written
/// here. Value 5 is a *hard* power-off that many panels cannot be woken from
/// over DDC/CI, requiring a physical power-cycle to recover — it must never
/// be sent. Do not renumber or "simplify" these constants.
const POWER_ON: u32 = 1;
const POWER_OFF: u32 = 4;

/// How far a brightness readback may drift from the requested value and
/// still count as confirmed. Panels round to their own internal step size,
/// so an exact match is not a realistic bar.
const BRIGHTNESS_TOLERANCE: u32 = 5;

/// `percent` of `saved`, clamped to at least 1 so the panel never reads as
/// off — a `Dim` target must never be mistaken for `Black`/`Off`. `percent`
/// is clamped to 100 so an out-of-range config value cannot overshoot.
pub fn scale(saved: u32, percent: u8) -> u32 {
    let v = saved.saturating_mul(percent.min(100) as u32) / 100;
    if saved == 0 { 0 } else { v.max(1) }
}

/// A physical monitor reached over DDC/CI.
///
/// Not `Clone`/`Copy`: it owns exactly one `PHYSICAL_MONITOR` array and
/// `Drop` destroys it exactly once, so there is no path to a double-free of
/// the underlying handle array.
pub struct DdcMonitor {
    monitors: Vec<PHYSICAL_MONITOR>,
    saved: Option<u32>,
}

impl DdcMonitor {
    /// Opens the physical monitor(s) behind `handle` (an `HMONITOR`, carried
    /// as `isize` the way `monitors::MonitorInfo::handle` does) and reads
    /// the current brightness to serve as the restore point.
    ///
    /// Returns `None` only when the handle cannot be resolved to any
    /// physical monitor at all (e.g. it does not support DDC/CI). A monitor
    /// that resolves but does not answer `GetVCPFeature` for brightness
    /// still returns `Some`, with `saved_brightness() == None` — such a
    /// monitor is unusable for `Dim` but may still accept power commands
    /// (spec §6.1).
    pub fn open(handle: isize) -> Option<DdcMonitor> {
        let hmonitor = HMONITOR(handle as *mut core::ffi::c_void);

        let mut count: u32 = 0;
        // SAFETY: `hmonitor` is a handle produced by `monitors::enumerate`
        // from a live `EnumDisplayMonitors` callback, and `count` is a local
        // `u32` that outlives this call for the API to write into.
        let got_count =
            unsafe { GetNumberOfPhysicalMonitorsFromHMONITOR(hmonitor, &mut count) }.is_ok();
        if !got_count || count == 0 {
            return None;
        }

        let mut monitors = vec![PHYSICAL_MONITOR::default(); count as usize];
        // SAFETY: `monitors` has exactly `count` elements, the size
        // `GetNumberOfPhysicalMonitorsFromHMONITOR` just reported for this
        // same handle, matching what `GetPhysicalMonitorsFromHMONITOR`
        // requires of its output slice.
        let got_monitors =
            unsafe { GetPhysicalMonitorsFromHMONITOR(hmonitor, &mut monitors) }.is_ok();
        if !got_monitors {
            return None;
        }

        let mut me = DdcMonitor {
            monitors,
            saved: None,
        };
        me.saved = me.read_vcp(VCP_BRIGHTNESS);
        Some(me)
    }

    /// The underlying DDC/CI handle. `open` guarantees `monitors` is
    /// non-empty, and most displays report exactly one physical monitor per
    /// `HMONITOR`; when more than one is reported (e.g. a clone/mirror
    /// setup) only the first is driven.
    fn handle(&self) -> HANDLE {
        self.monitors[0].hPhysicalMonitor
    }

    fn read_vcp(&self, code: u8) -> Option<u32> {
        let mut current: u32 = 0;
        // SAFETY: `handle()` is a live physical-monitor HANDLE owned by this
        // `DdcMonitor` until `Drop` runs; `current` is a local `u32` the
        // call writes into. The optional out-params are omitted with `None`.
        let ok = unsafe {
            GetVCPFeatureAndVCPFeatureReply(self.handle(), code, None, &mut current, None)
        };
        // This API returns a raw Win32 BOOL as `i32` rather than a wrapped
        // `windows_core::Result`: nonzero means success.
        if ok != 0 { Some(current) } else { None }
    }

    fn write_vcp(&self, code: u8, value: u32) -> bool {
        // SAFETY: `handle()` is a live physical-monitor HANDLE owned by this
        // `DdcMonitor` until `Drop` runs.
        let ok = unsafe { SetVCPFeature(self.handle(), code, value) };
        // Also a raw BOOL-as-`i32`: nonzero means success.
        ok != 0
    }

    /// The brightness saved when this monitor was opened, if it answered.
    pub fn saved_brightness(&self) -> Option<u32> {
        self.saved
    }

    /// Sets brightness to `percent` of the saved value, then reads it back
    /// and reports whether the readback landed within tolerance of the
    /// target. This is the whole mechanism spec §6.1 relies on to detect
    /// Windows HDR silently locking DDC brightness: the write is never
    /// assumed to have taken just because the call returned success.
    ///
    /// Returns `false` (never panics) if brightness was not readable at
    /// `open`, if the write fails, or if the readback fails or disagrees.
    pub fn set_brightness(&mut self, percent: u8) -> bool {
        let Some(saved) = self.saved else {
            return false;
        };
        let target = scale(saved, percent);
        if !self.write_vcp(VCP_BRIGHTNESS, target) {
            return false;
        }
        match self.read_vcp(VCP_BRIGHTNESS) {
            Some(actual) => actual.abs_diff(target) <= BRIGHTNESS_TOLERANCE,
            None => false,
        }
    }

    /// Writes the brightness saved at `open` time back to the panel.
    /// Returns `false` if brightness was never readable at `open`.
    pub fn restore_brightness(&mut self) -> bool {
        let Some(saved) = self.saved else {
            return false;
        };
        if !self.write_vcp(VCP_BRIGHTNESS, saved) {
            return false;
        }
        // Read back, exactly as `set_brightness` does. The asymmetry used to
        // run the other way: this returned whether the WRITE call succeeded,
        // so a panel that accepted the write without acting on it -- which is
        // precisely the Windows HDR failure mode `set_brightness` exists to
        // catch -- reported a restore that never happened. Everything
        // downstream now depends on this answer being about the panel's state
        // rather than about the call's return value: `rescan` decides whether
        // it may adopt what it reads based on it.
        match self.read_vcp(VCP_BRIGHTNESS) {
            Some(actual) => actual.abs_diff(saved) <= BRIGHTNESS_TOLERANCE,
            None => false,
        }
    }

    /// Replace the restore point with one carried over from a previous handle.
    ///
    /// Used only by `Resolver::rescan`, when a restore did not confirm and the
    /// value this handle read at `open` is therefore the dim rather than the
    /// user's brightness. See `brightness_to_keep`.
    pub fn adopt_saved(&mut self, value: Option<u32>) {
        self.saved = value;
    }

    /// Powers the panel on or off. Only ever writes VCP value 1 (on) or 4
    /// (off) — never 5, the hard power-off (spec §6.1/§12).
    pub fn set_power(&mut self, on: bool) -> bool {
        self.write_vcp(VCP_POWER_MODE, if on { POWER_ON } else { POWER_OFF })
    }
}

impl Drop for DdcMonitor {
    fn drop(&mut self) {
        // SAFETY: `self.monitors` is exactly the array `open` obtained from
        // `GetPhysicalMonitorsFromHMONITOR`, destroyed here exactly once.
        // `DdcMonitor` is not `Clone`, so no other owner of this array can
        // exist to race or repeat this call.
        let _ = unsafe { DestroyPhysicalMonitors(&self.monitors) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dim_percent_scales_against_saved_brightness() {
        assert_eq!(scale(80, 20), 16);
        assert_eq!(scale(50, 100), 50);
        assert_eq!(scale(0, 50), 0);
        assert_eq!(scale(100, 1), 1);
    }

    #[test]
    fn a_dim_target_never_rounds_down_to_a_dark_panel() {
        // 1% of 10 is 0 by integer division; the floor keeps the panel lit so
        // Dim can never be mistaken for Off.
        assert_eq!(scale(10, 1), 1);
        // But a monitor genuinely reporting zero stays zero.
        assert_eq!(scale(0, 100), 0);
        // And percent is clamped, so an out-of-range value cannot overshoot.
        assert_eq!(scale(50, 200), 50);
    }
}
