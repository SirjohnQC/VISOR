use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};

pub trait IdleSource: Send {
    /// How long since the last keyboard or mouse input.
    fn idle_for(&self) -> Duration;
}

pub struct Win32Idle;

impl Win32Idle {
    pub fn new() -> Self {
        Self
    }
}

impl Default for Win32Idle {
    fn default() -> Self {
        Self::new()
    }
}

impl IdleSource for Win32Idle {
    fn idle_for(&self) -> Duration {
        let mut info = LASTINPUTINFO {
            cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
            dwTime: 0,
        };
        // SAFETY: `info` is a correctly sized, initialised LASTINPUTINFO.
        let ok = unsafe { GetLastInputInfo(&mut info) };
        if !ok.as_bool() {
            // Fail toward presence (spec §2.1): report zero idle so VISOR
            // never blanks a screen because this call failed.
            return Duration::ZERO;
        }
        // Both values are 32-bit millisecond tick counts that wrap roughly
        // every 49 days; wrapping_sub gives the correct delta across a wrap.
        // SAFETY: GetTickCount takes no arguments and has no preconditions.
        let now = unsafe { GetTickCount() };
        Duration::from_millis(now.wrapping_sub(info.dwTime) as u64)
    }
}

/// Test double used by the engine tests in later tasks.
pub struct FakeIdle(AtomicU64);

impl FakeIdle {
    pub fn new(d: Duration) -> Self {
        Self(AtomicU64::new(d.as_millis() as u64))
    }
    pub fn set(&self, d: Duration) {
        self.0.store(d.as_millis() as u64, Ordering::Relaxed);
    }
}

impl IdleSource for FakeIdle {
    fn idle_for(&self) -> Duration {
        Duration::from_millis(self.0.load(Ordering::Relaxed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_idle_returns_what_it_was_given() {
        let f = FakeIdle::new(Duration::from_secs(42));
        assert_eq!(f.idle_for(), Duration::from_secs(42));
        f.set(Duration::from_secs(1));
        assert_eq!(f.idle_for(), Duration::from_secs(1));
    }

    #[test]
    fn win32_idle_reports_something_plausible() {
        // We cannot script real input, but the value must be finite and not
        // absurd — this catches a tick-arithmetic bug that returns ~49 days.
        let idle = Win32Idle::new().idle_for();
        assert!(
            idle < Duration::from_secs(60 * 60 * 24),
            "implausible idle time: {idle:?}"
        );
    }
}
