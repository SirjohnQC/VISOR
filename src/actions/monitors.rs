use windows::Win32::Foundation::{BOOL, LPARAM, RECT, TRUE};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFOEXW,
};

#[derive(Debug, Clone)]
pub struct MonitorInfo {
    /// Kept as `isize` rather than `HMONITOR` so `MonitorInfo` stays `Send`.
    pub handle: isize,
    pub rect: RECT,
    pub description: String,
}

unsafe extern "system" fn cb(h: HMONITOR, _dc: HDC, _r: *mut RECT, data: LPARAM) -> BOOL {
    // SAFETY: `data` was set by `enumerate` below to point at a live
    // `Vec<MonitorInfo>` that outlives this whole `EnumDisplayMonitors` call.
    let out = unsafe { &mut *(data.0 as *mut Vec<MonitorInfo>) };
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    // SAFETY: `h` is a valid HMONITOR handed to us by EnumDisplayMonitors, and
    // cbSize declares the EX layout, which is what `info` is.
    let ok = unsafe { GetMonitorInfoW(h, &mut info.monitorInfo as *mut _) };
    if ok.as_bool() {
        let name = String::from_utf16_lossy(&info.szDevice)
            .trim_end_matches('\0')
            .to_string();
        out.push(MonitorInfo {
            handle: h.0 as isize,
            rect: info.monitorInfo.rcMonitor,
            description: name,
        });
    } else {
        // Fail toward the screen staying on: a monitor we can't describe is
        // logged and skipped rather than aborting the whole enumeration.
        tracing::warn!("GetMonitorInfoW failed for a monitor; skipping it");
    }
    TRUE
}

pub fn enumerate() -> Vec<MonitorInfo> {
    let mut out: Vec<MonitorInfo> = Vec::new();
    // SAFETY: `cb` matches the `MONITORENUMPROC` signature, and `out` is a
    // local that outlives this synchronous call, so the raw pointer we pass
    // through `LPARAM` stays valid for the callback's whole lifetime.
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(cb),
            LPARAM(&mut out as *mut Vec<MonitorInfo> as isize),
        );
    }
    out
}

/// Spec §7 `display.targets` — empty means every monitor.
pub fn select(all: Vec<MonitorInfo>, targets: &[String]) -> Vec<MonitorInfo> {
    if targets.is_empty() {
        return all;
    }
    all.into_iter()
        .filter(|m| targets.iter().any(|t| m.description.contains(t.as_str())))
        .collect()
}
