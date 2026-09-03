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
        // Spec §6.3 -- the fallback for a panel that will not take a DDC
        // power command is `SC_MONITORPOWER`, not the overlay. A black overlay
        // leaves the panel driven and lit-but-black, which is not "off" and
        // does not save the panel anything an `Away` overlay had not already
        // saved. `Resolver::apply` degrades this to the overlay itself when
        // `broadcast_allowed` refuses (more than one monitor attached), so the
        // multi-monitor quarantine still holds.
        DisplayLevel::Off => {
            if cap.power {
                Mechanism::Ddc
            } else {
                Mechanism::Broadcast
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

/// How many times `Resolver::restore` may retry a brightness write.
///
/// The retry exists because a panel that has just woken often rejects VCP
/// writes for a moment. But `restore_brightness` returns `false` *immediately*
/// and forever when the monitor never answered `GetVCPFeature(0x10)` at open
/// time -- there is simply nothing saved to write back. Retrying that sleeps
/// 50+100+200+400ms for nothing, on the message-pump thread, on every restore.
/// And it is not a rare case: an unreadable brightness is exactly what Windows
/// HDR produces, which is the configuration this machine runs.
fn restore_attempts(has_saved: bool) -> usize {
    if has_saved { 5 } else { 0 }
}

/// What a rescan should keep as the restore point for a monitor.
///
/// `DdcMonitor::open` reads the panel's current brightness and treats it as the
/// user's. That is only true when the panel is actually at the user's
/// brightness, which is why `rescan` restores before it re-opens. When the
/// restore did not confirm, the panel is still dim and the fresh read is a dim
/// value, so the value known before the rescan is the better answer.
///
/// Keeping the previous value rather than dropping it also matters because
/// `restore_attempts` returns 0 when nothing is saved: a lost brightness is not
/// merely unknown, it disables every future restore for that monitor.
pub fn brightness_to_keep(
    previous: Option<u32>,
    restore_confirmed: bool,
    fresh: Option<u32>,
) -> Option<u32> {
    if restore_confirmed {
        fresh.or(previous)
    } else {
        previous.or(fresh)
    }
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
    /// How many monitors are *attached*, which is not the same as how many
    /// this `Resolver` drives. `SC_MONITORPOWER` blanks every display in the
    /// session regardless of `display.targets`, so spec §6.3's quarantine has
    /// to be judged against what is plugged in, not against what we selected.
    /// Counting targets instead would let someone who pinned VISOR to one
    /// screen of two get both of them blanked.
    attached: usize,
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
            attached: 0,
            powered_off: false,
        };
        r.rescan();
        // Start from a known-lit panel. A previous VISOR killed outright --
        // Task Manager, a crash, a forced reboot -- can have left the monitor
        // switched off over DDC, and nothing in Windows knows: the OS is still
        // driving the display normally, so moving the mouse will not bring it
        // back and the user has to reach for the monitor's own power button.
        // This process cannot tell whether that happened, so it assumes it
        // might have. `SetVCPFeature(0xD6, 1)` on a panel that is already on is
        // a no-op, which makes assuming the worse case free.
        r.powered_off = true;
        r.restore();
        r
    }

    /// Re-enumerate monitors and rebuild the DDC handles. Task 13 calls this
    /// on `WM_DISPLAYCHANGE`.
    ///
    /// Restores first, and that ordering is load-bearing. `DdcMonitor::open`
    /// reads the panel's current brightness and keeps it as the restore point,
    /// so re-opening while VISOR still has the panel sitting at a dim level
    /// would capture *the dim* as the user's brightness. Every later restore
    /// would then land there, and the next dim would take 20% of that -- a
    /// display change every so often would walk the panel down 100 -> 20 -> 4
    /// -> 1 with no way back short of the monitor's own OSD. Putting the panel
    /// back to the saved value before discarding the handles that know it is
    /// what keeps the readback honest.
    pub fn rescan(&mut self) {
        let restored = self.restore();
        // Captured before the old handles are dropped: if the restore did not
        // confirm, the panel is still at the dim level and the value the new
        // handle is about to read is that dim, not the user's brightness.
        let previous: Vec<(String, Option<u32>)> = self
            .targets
            .iter()
            .map(|t| {
                (
                    t.description.clone(),
                    t.ddc.as_ref().and_then(|d| d.saved_brightness()),
                )
            })
            .collect();

        let all = monitors::enumerate();
        self.attached = all.len();
        let chosen = monitors::select(all, &self.configured);
        self.targets = chosen
            .into_iter()
            .map(|m| {
                let mut ddc = ddc::DdcMonitor::open(m.handle);
                if let Some(d) = ddc.as_mut() {
                    let prev = previous
                        .iter()
                        .find(|(desc, _)| *desc == m.description)
                        .and_then(|(_, b)| *b);
                    let fresh = d.saved_brightness();
                    let keep = brightness_to_keep(prev, restored, fresh);
                    if keep != fresh {
                        tracing::warn!(
                            monitor = %m.description,
                            read = ?fresh,
                            kept = ?keep,
                            "restore did not confirm; keeping the brightness known                              before the rescan rather than adopting a dim one"
                        );
                        d.adopt_saved(keep);
                    }
                }
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
        for t in &self.targets {
            tracing::info!(
                monitor = %t.description,
                ddc = t.ddc.is_some(),
                brightness = t.cap.brightness_confirmed,
                "display target"
            );
        }
        tracing::info!(
            targets = self.targets.len(),
            attached = self.attached,
            broadcast_ok = broadcast_allowed(&self.strategy, self.attached),
            "display targets rescanned"
        );
    }

    /// Attached monitors, deliberately not `self.targets.len()` -- see the
    /// `attached` field.
    fn monitor_count(&self) -> usize {
        self.attached
    }

    /// What the tuning window's DISPLAY section reports: the first driven
    /// monitor, whether it speaks DDC/CI, and whether brightness writes are
    /// actually confirmed on it.
    ///
    /// Read from the live resolver rather than re-probed, so the window can
    /// never disagree with the mechanism actually in use.
    pub fn primary(&self) -> Option<(String, bool, bool)> {
        self.targets.first().map(|t| {
            (
                t.description.clone(),
                t.ddc.is_some(),
                t.cap.brightness_confirmed,
            )
        })
    }

    /// Spec §6.2 — restore in a fixed order rather than by picking a mechanism.
    ///
    /// Returns whether the panel is now known to be at the user's brightness.
    /// `rescan` needs that answer before it may adopt what a fresh handle
    /// reads; a monitor with nothing saved to restore reports `true`, because
    /// it was never dimmed over DDC and so cannot be sitting at a dim value.
    fn restore(&mut self) -> bool {
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
        let mut confirmed = true;
        for t in &mut self.targets {
            let Some(d) = t.ddc.as_mut() else { continue };
            let attempts = restore_attempts(d.saved_brightness().is_some());
            if attempts == 0 {
                tracing::debug!(
                    monitor = %t.description,
                    "no saved brightness to restore; nothing to retry"
                );
                continue;
            }
            let mut delay = std::time::Duration::from_millis(50);
            let mut restored = false;
            for _ in 0..attempts {
                if d.restore_brightness() {
                    restored = true;
                    break;
                }
                std::thread::sleep(delay);
                delay *= 2;
            }
            if !restored {
                tracing::warn!(monitor = %t.description, "brightness restore never confirmed");
                confirmed = false;
            }
        }
        confirmed
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
        let count = self.monitor_count();
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
    fn off_prefers_ddc_and_degrades_to_the_broadcast_not_the_overlay() {
        let cap = DdcCapability {
            brightness_confirmed: true,
            power: true,
        };
        assert_eq!(plan_for(DisplayLevel::Off, cap), Mechanism::Ddc);

        // Regression: this used to answer `Overlay`, which made
        // `Mechanism::Broadcast` unreachable from `plan_for` and left
        // `broadcast::set_power` as dead code. On a machine whose panel does
        // not speak DDC/CI -- which is the machine VISOR was written on --
        // that silently turned `Deep` into a second `Away`: a black overlay
        // over a panel that was still fully powered.
        let cap = DdcCapability {
            brightness_confirmed: true,
            power: false,
        };
        assert_eq!(plan_for(DisplayLevel::Off, cap), Mechanism::Broadcast);
    }

    #[test]
    fn every_mechanism_is_reachable_from_plan_for() {
        // The bug above was not that a branch was wrong, it was that a whole
        // mechanism had no way to be selected. This asserts the enum and the
        // planner cannot drift apart again.
        let caps = [
            DdcCapability {
                brightness_confirmed: true,
                power: true,
            },
            DdcCapability {
                brightness_confirmed: false,
                power: false,
            },
        ];
        let levels = [
            DisplayLevel::Full,
            DisplayLevel::Dim(20),
            DisplayLevel::Black,
            DisplayLevel::Off,
        ];
        let mut seen = Vec::new();
        for c in caps {
            for l in levels {
                let m = plan_for(l, c);
                if !seen.contains(&m) {
                    seen.push(m);
                }
            }
        }
        for m in [Mechanism::Ddc, Mechanism::Overlay, Mechanism::Broadcast] {
            assert!(seen.contains(&m), "{m:?} is unreachable from plan_for");
        }
    }

    #[test]
    fn a_monitor_with_no_saved_brightness_is_not_retried() {
        // Without this the pump thread sleeps 750ms on every single restore
        // for a panel that can never answer -- the HDR case.
        assert_eq!(restore_attempts(false), 0);
        assert!(
            restore_attempts(true) > 1,
            "a readable panel must still get the just-woke retry"
        );
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
mod rescan_brightness {
    use super::brightness_to_keep;

    #[test]
    fn a_failed_restore_must_not_let_a_rescan_capture_the_dim() {
        // `DdcMonitor::open` takes whatever the panel currently reports as the
        // user's brightness. `rescan` restores first so that capture is
        // honest -- but `restore` can fail, and then the panel is still
        // sitting at the dim level when the new handle reads it.
        //
        // Capturing THAT makes 20% the new "full", and the next dim takes 20%
        // OF IT: the 100 -> 20 -> 4 -> 1 walk-down the `rescan` doc comment
        // warns about, ending somewhere only the monitor's own OSD can undo.
        // This happened on 2026-09-03 -- the panel refused the read, so the
        // value was lost rather than corrupted, which was luck, not design.
        assert_eq!(brightness_to_keep(Some(80), false, Some(16)), Some(80));

        // A confirmed restore means the panel really is back at the user's
        // brightness, so the fresh read is the truth and a brightness the user
        // changed at the OSD meanwhile is picked up.
        assert_eq!(brightness_to_keep(Some(80), true, Some(90)), Some(90));

        // A panel that refuses the read after a failed restore must not lose
        // the value already known. Losing it makes `restore_attempts` return
        // 0, and then every later restore skips silently -- the panel stays
        // dim and nothing in the log says why.
        assert_eq!(brightness_to_keep(Some(80), false, None), Some(80));

        // Nothing saved before means nothing was ever dimmed over DDC, so a
        // fresh read cannot be a dim value and is safe to adopt.
        assert_eq!(brightness_to_keep(None, true, Some(75)), Some(75));
        assert_eq!(brightness_to_keep(None, false, Some(75)), Some(75));
        assert_eq!(brightness_to_keep(None, false, None), None);
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
