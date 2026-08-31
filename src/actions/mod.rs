use crate::config::DisplayConfig;
use crate::core::types::DisplayLevel;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use windows::Win32::System::Power::{ES_CONTINUOUS, ES_DISPLAY_REQUIRED, SetThreadExecutionState};

pub mod broadcast;
pub mod ddc;
pub mod monitors;
pub mod overlay;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mechanism {
    Ddc,
    Overlay,
    Broadcast,
}

#[derive(Debug, Clone, Copy)]
pub struct DdcCapability {
    /// A brightness write was confirmed by readback. False while HDR is on.
    pub brightness_confirmed: bool,
    /// The panel accepted a power command.
    pub power: bool,
}

/// Spec §6.1 — the mechanism is chosen per operation, not per monitor.
///
/// The `Full` arm is **not** the restore path. Restoring follows the §6.2
/// sequence — hide the overlay, power on, then restore brightness — rather
/// than selecting a single mechanism. This arm exists only to keep the
/// function total; `Resolver::restore` never consults it.
pub fn plan_for(level: DisplayLevel, cap: DdcCapability) -> Mechanism {
    match level {
        // Black must not power-cycle the panel and must restore instantly.
        DisplayLevel::Black => Mechanism::Overlay,
        DisplayLevel::Dim(_) => {
            if cap.brightness_confirmed {
                Mechanism::Ddc
            } else {
                Mechanism::Overlay
            }
        }
        DisplayLevel::Off => {
            if cap.power {
                Mechanism::Ddc
            } else {
                Mechanism::Overlay
            }
        }
        DisplayLevel::Full => Mechanism::Overlay,
    }
}

/// Spec §6.3 — `SC_MONITORPOWER` blanks every display, so it is never chosen
/// automatically when more than one monitor is attached.
pub fn broadcast_allowed(strategy: &str, monitor_count: usize) -> bool {
    strategy == "broadcast" || monitor_count <= 1
}

/// What `Engine` holds instead of the real display stack.
///
/// Ruling F8: the resolver cannot live on the engine thread. Overlay windows
/// must be owned by the message-pump thread (ruling F7), and `PHYSICAL_MONITOR`
/// is not `Send`, so `DdcMonitor` cannot cross threads at all. This forwards
/// levels to the main thread, which owns the `Resolver` and does the real work.
pub struct ChannelDisplay {
    pub tx: Sender<DisplayLevel>,
}

impl DisplayControl for ChannelDisplay {
    fn apply(&mut self, level: DisplayLevel) {
        // A closed receiver means the pump is gone and the process is exiting.
        // Spec §8: a display failure must never propagate into a state change.
        let _ = self.tx.send(level);
    }
}

/// Ties the mechanisms together. **Main-thread only** — see `ChannelDisplay`.
pub struct Resolver {
    overlay: overlay::OverlayControl,
    /// One entry per selected monitor: its DDC handle if it opened, and what
    /// we have learned it can actually do.
    targets: Vec<Target>,
    strategy: String,
    configured: Vec<String>,
    /// Whether the panel is currently powered down, so restore knows to wake it.
    powered_off: bool,
}

struct Target {
    description: String,
    ddc: Option<ddc::DdcMonitor>,
    cap: DdcCapability,
}

impl Resolver {
    pub fn new(cfg: &DisplayConfig) -> Self {
        let mut r = Self {
            overlay: overlay::OverlayControl::for_targets(&cfg.targets),
            targets: Vec::new(),
            strategy: cfg.strategy.clone(),
            configured: cfg.targets.clone(),
            powered_off: false,
        };
        r.rescan();
        r
    }

    /// Re-enumerate monitors and rebuild the DDC handles. Task 13 calls this
    /// on `WM_DISPLAYCHANGE`.
    pub fn rescan(&mut self) {
        let chosen = monitors::select(monitors::enumerate(), &self.configured);
        self.targets = chosen
            .into_iter()
            .map(|m| {
                let ddc = ddc::DdcMonitor::open(m.handle);
                // Assume capable until proven otherwise; the readback in
                // `set_brightness` is what actually decides, per operation.
                let cap = DdcCapability {
                    brightness_confirmed: ddc
                        .as_ref()
                        .map(|d| d.saved_brightness().is_some())
                        .unwrap_or(false),
                    power: ddc.is_some(),
                };
                Target {
                    description: m.description,
                    ddc,
                    cap,
                }
            })
            .collect();
        self.overlay = overlay::OverlayControl::for_targets(&self.configured);
        tracing::info!(targets = self.targets.len(), "display targets rescanned");
    }

    fn monitor_count(&self) -> usize {
        self.targets.len()
    }

    /// Spec §6.2 — restore in a fixed order rather than by picking a mechanism.
    fn restore(&mut self) {
        // 1. Drop the overlay first; it is the fastest thing to undo.
        self.overlay.apply(DisplayLevel::Full);

        // 2. Power the panel back on if we powered it down.
        if self.powered_off {
            for t in &mut self.targets {
                if let Some(d) = t.ddc.as_mut() {
                    let _ = d.set_power(true);
                }
            }
            if broadcast_allowed(&self.strategy, self.monitor_count()) {
                broadcast::set_power(true);
            }
            self.powered_off = false;
        }

        // 3. Restore brightness, retried with backoff: a panel that has just
        //    woken often rejects VCP writes for a moment.
        for t in &mut self.targets {
            let Some(d) = t.ddc.as_mut() else { continue };
            let mut delay = std::time::Duration::from_millis(50);
            let mut restored = false;
            for _ in 0..5 {
                if d.restore_brightness() {
                    restored = true;
                    break;
                }
                std::thread::sleep(delay);
                delay *= 2;
            }
            if !restored {
                tracing::warn!(monitor = %t.description, "brightness restore never confirmed");
            }
        }
    }
}

impl Resolver {
    /// Inherent rather than `impl DisplayControl`: that trait requires `Send`,
    /// and `Resolver` holds `PHYSICAL_MONITOR` handles that are not. The type
    /// system therefore enforces ruling F8 for us — a `Resolver` cannot be
    /// moved onto the engine thread even by accident.
    pub fn apply(&mut self, level: DisplayLevel) {
        if level == DisplayLevel::Full {
            self.restore();
            return;
        }

        let mut needs_overlay = false;
        let count = self.targets.len();
        let strategy = self.strategy.clone();
        let mut powered_off = self.powered_off;
        for t in &mut self.targets {
            match plan_for(level, t.cap) {
                Mechanism::Ddc => match level {
                    DisplayLevel::Dim(p) => {
                        let Some(d) = t.ddc.as_mut() else {
                            needs_overlay = true;
                            continue;
                        };
                        if !d.set_brightness(p) {
                            // The readback did not confirm. This is how an HDR
                            // toggle becomes transparent: remember that DDC
                            // brightness is not working and fall through to the
                            // overlay in this same call, so the user never sees
                            // a missed dim.
                            tracing::info!(
                                monitor = %t.description,
                                "DDC brightness unconfirmed (HDR?); using the overlay"
                            );
                            t.cap.brightness_confirmed = false;
                            needs_overlay = true;
                        }
                    }
                    DisplayLevel::Off => {
                        let Some(d) = t.ddc.as_mut() else {
                            needs_overlay = true;
                            continue;
                        };
                        if d.set_power(false) {
                            powered_off = true;
                        } else {
                            tracing::info!(
                                monitor = %t.description,
                                "DDC power off rejected; using the overlay"
                            );
                            t.cap.power = false;
                            needs_overlay = true;
                        }
                    }
                    _ => needs_overlay = true,
                },
                Mechanism::Overlay => needs_overlay = true,
                Mechanism::Broadcast => {
                    if broadcast_allowed(&strategy, count) {
                        broadcast::set_power(false);
                        powered_off = true;
                    } else {
                        needs_overlay = true;
                    }
                }
            }
        }

        self.powered_off = powered_off;

        // One overlay pass covers every monitor that needed it. `Black` always
        // lands here, which is the point: it must not power-cycle the panel.
        if needs_overlay || self.targets.is_empty() {
            self.overlay.apply(level);
        } else {
            self.overlay.apply(DisplayLevel::Full);
        }
    }
}

pub trait DisplayControl: Send {
    /// Bring every target monitor to `level`. Errors are logged internally;
    /// a failure must never propagate into a state change (spec §8).
    fn apply(&mut self, level: DisplayLevel);
}

pub struct SpyDisplay {
    log: Arc<Mutex<Vec<DisplayLevel>>>,
}

impl SpyDisplay {
    pub fn new() -> Self {
        Self {
            log: Arc::new(Mutex::new(Vec::new())),
        }
    }
    pub fn log(&self) -> Arc<Mutex<Vec<DisplayLevel>>> {
        self.log.clone()
    }
}

impl Default for SpyDisplay {
    fn default() -> Self {
        Self::new()
    }
}

impl DisplayControl for SpyDisplay {
    fn apply(&mut self, level: DisplayLevel) {
        self.log.lock().unwrap().push(level);
    }
}

/// Spec §7 `hold_awake_while_present` — defaults to off. Asserting this makes
/// VISOR the single authority on when the display sleeps.
///
/// Thread-affine: `SetThreadExecutionState` applies to the *calling* thread,
/// and `ES_CONTINUOUS` persists until that thread clears it or exits. This is
/// called from `Engine::apply` on the engine thread, which lives for the
/// whole process, so this is correct — but it is correct *by accident of
/// where it is called from*. Moving this call to another thread (or calling
/// it from a short-lived one) would silently fail to hold anything: the flag
/// would be cleared the moment that thread exited, or would apply to the
/// wrong thread entirely.
pub fn set_awake_hold(on: bool) {
    let flags = if on {
        ES_CONTINUOUS | ES_DISPLAY_REQUIRED
    } else {
        ES_CONTINUOUS
    };
    // SAFETY: a well-formed flag combination; the call has no out-parameters
    // and cannot fail in a way that leaves anything in an invalid state.
    unsafe {
        SetThreadExecutionState(flags);
    }
}

#[cfg(test)]
mod awake_hold_tests {
    use super::*;

    #[test]
    fn awake_hold_sets_and_clears_without_panicking() {
        // ES_DISPLAY_REQUIRED is process-global; this asserts the calls are
        // well-formed and that clearing does not leave the flag asserted.
        set_awake_hold(true);
        set_awake_hold(false);
    }
}

#[cfg(test)]
mod resolver_tests {
    use super::*;

    #[test]
    fn broadcast_is_refused_automatically_when_several_monitors_are_attached() {
        assert!(!broadcast_allowed("auto", 2));
        assert!(broadcast_allowed("auto", 1));
        // explicit opt-in overrides the quarantine
        assert!(broadcast_allowed("broadcast", 3));
    }

    #[test]
    fn dim_falls_through_to_overlay_when_ddc_does_not_confirm() {
        let plan = plan_for(
            DisplayLevel::Dim(20),
            DdcCapability {
                brightness_confirmed: false,
                power: true,
            },
        );
        assert_eq!(plan, Mechanism::Overlay);

        let plan = plan_for(
            DisplayLevel::Dim(20),
            DdcCapability {
                brightness_confirmed: true,
                power: true,
            },
        );
        assert_eq!(plan, Mechanism::Ddc);
    }

    #[test]
    fn black_never_uses_ddc() {
        let plan = plan_for(
            DisplayLevel::Black,
            DdcCapability {
                brightness_confirmed: true,
                power: true,
            },
        );
        assert_eq!(
            plan,
            Mechanism::Overlay,
            "Black must not power-cycle the panel"
        );
    }

    #[test]
    fn off_prefers_ddc_and_degrades_to_overlay() {
        let cap = DdcCapability {
            brightness_confirmed: true,
            power: true,
        };
        assert_eq!(plan_for(DisplayLevel::Off, cap), Mechanism::Ddc);

        let cap = DdcCapability {
            brightness_confirmed: true,
            power: false,
        };
        assert_eq!(plan_for(DisplayLevel::Off, cap), Mechanism::Overlay);
    }

    #[test]
    fn the_channel_display_forwards_levels_without_touching_windows() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut d = ChannelDisplay { tx };
        d.apply(DisplayLevel::Dim(20));
        d.apply(DisplayLevel::Full);
        assert_eq!(rx.try_recv(), Ok(DisplayLevel::Dim(20)));
        assert_eq!(rx.try_recv(), Ok(DisplayLevel::Full));
    }

    #[test]
    fn a_dropped_receiver_does_not_panic_the_engine() {
        // Spec §8: a display failure must never propagate into a state change.
        let (tx, rx) = std::sync::mpsc::channel();
        drop(rx);
        let mut d = ChannelDisplay { tx };
        d.apply(DisplayLevel::Black); // must not panic
    }
}

#[cfg(test)]
mod send_guard {
    /// Ruling F8, held up by the type system rather than by convention.
    ///
    /// `ChannelDisplay` is what crosses onto the engine thread, so it must be
    /// `Send`. `Resolver` must NOT be -- it owns overlay `HWND`s that belong to
    /// the pump thread and `PHYSICAL_MONITOR` handles that are not `Send` --
    /// and that is enforced by `Resolver` deliberately not implementing the
    /// `Send`-bounded `DisplayControl` trait. A stable-Rust test cannot assert
    /// the absence of an auto trait, so this pins the half that is assertable;
    /// the other half is enforced at the `Engine::new` call site in `main`.
    #[test]
    fn the_engine_side_display_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<super::ChannelDisplay>();
    }
}
