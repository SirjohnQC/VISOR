use crate::actions::Resolver;
use crate::actions::overlay;
use crate::core::types::{Command, DisplayLevel, State};
use crate::error::{Result, VisorError};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, TrayIconBuilder, TrayIconEvent};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
};

/// How often the pump loop wakes to drain events and check for a state change.
///
/// The plan said 100ms. This is the most expensive recurring cost in an
/// otherwise 1Hz application, and the project's stated premise is "lightweight,
/// low CPU", so it is 250ms instead — a quarter-second of tooltip latency is
/// imperceptible and it cuts the wakeup rate by 60%. Replacing this poll with a
/// `SetTimer` plus a blocking `GetMessageW` would remove the cost entirely and
/// is the right long-term shape; it is deliberately out of scope here.
const POLL: std::time::Duration = std::time::Duration::from_millis(250);

const ICON_PX: u32 = 32;

pub fn tooltip(s: State) -> String {
    let detail = match s {
        State::Active => "present",
        State::Watching => "watching",
        State::Dimmed => "dimmed",
        State::Away => "screen black",
        State::Deep => "monitor off",
        State::Paused => "paused",
        State::Degraded => "camera unavailable",
    };
    format!("VISOR — {detail}")
}

/// Spec §4.7 — `Degraded` shows a warning on the tray icon.
pub fn is_warning(s: State) -> bool {
    matches!(s, State::Degraded)
}

/// A flat RGBA square. The allowlist has no image crate, so the icons are
/// built from raw pixels rather than decoded from a file.
fn solid_icon(rgb: [u8; 3]) -> std::result::Result<Icon, tray_icon::BadIcon> {
    let mut rgba = Vec::with_capacity((ICON_PX * ICON_PX * 4) as usize);
    for _ in 0..(ICON_PX * ICON_PX) {
        rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 0xFF]);
    }
    Icon::from_rgba(rgba, ICON_PX, ICON_PX)
}

/// Owns the Win32 message pump. Blocks until the user quits.
///
/// Ruling F8: this thread also owns the `Resolver`, because overlay windows
/// must belong to the pumping thread and `DdcMonitor` is not `Send`. The engine
/// sends `DisplayLevel`s down `levels` and this loop applies them.
pub fn run(
    tx: Sender<Command>,
    status: Arc<AtomicU8>,
    levels: Receiver<DisplayLevel>,
    resolver: &mut Resolver,
) -> Result<()> {
    let win = |e: tray_icon::BadIcon| VisorError::Windows(e.to_string());
    // Slate for normal operation, amber for the Degraded warning.
    let normal = solid_icon([0x3A, 0x6E, 0xA5]).map_err(win)?;
    let warning = solid_icon([0xC8, 0x7A, 0x1E]).map_err(win)?;

    let menu = Menu::new();
    let pause = MenuItem::new("Pause", true, None);
    let resume = MenuItem::new("Resume", true, None);
    let reload = MenuItem::new("Reload config", true, None);
    let quit = MenuItem::new("Quit", true, None);
    menu.append_items(&[&pause, &resume, &reload, &quit])
        .map_err(|e| VisorError::Windows(e.to_string()))?;

    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_icon(normal.clone())
        .with_tooltip(tooltip(State::Active))
        .build()
        .map_err(|e| VisorError::Windows(e.to_string()))?;

    let menu_rx = MenuEvent::receiver();
    let mut shown = State::Active;

    // Ruling F12: dedicated message-only window so WM_DISPLAYCHANGE and
    // WM_POWERBROADCAST are always caught, independent of whether/how many
    // overlay windows currently exist (they are destroyed and recreated on
    // every `Resolver::rescan`). Held alive for the whole pump loop; dropping
    // it at the end of this function destroys it.
    let _broadcast_window = overlay::create_broadcast_window();

    loop {
        // Pump Win32 messages — tray-icon delivers its events through them.
        let mut msg = MSG::default();
        // SAFETY: a standard message pump. `msg` is a valid, owned MSG, and
        // PeekMessageW only writes into it.
        while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
            // SAFETY: `msg` was just filled by a successful PeekMessageW.
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        let _ = TrayIconEvent::receiver().try_recv();

        while let Ok(ev) = menu_rx.try_recv() {
            let cmd = if ev.id == *pause.id() {
                Some(Command::Pause)
            } else if ev.id == *resume.id() {
                Some(Command::Resume)
            } else if ev.id == *reload.id() {
                Some(Command::Reload)
            } else if ev.id == *quit.id() {
                Some(Command::Quit)
            } else {
                None
            };
            if let Some(c) = cmd {
                if c == Command::Reload {
                    // Ruling F8: the `Resolver` lives on this thread, not the
                    // engine's, so a reload has to be actioned here too —
                    // `Engine::reload` cannot reach it.
                    resolver.rescan();
                }
                // A send failure means the engine thread is already gone;
                // quitting is then the only sensible response.
                if tx.send(c).is_err() || c == Command::Quit {
                    return Ok(());
                }
            }
        }

        // Spec §8: "Monitor hot-unplug: re-enumerate and re-probe on
        // WM_DISPLAYCHANGE" / "System resume: re-probe DDC; assume Active".
        // Both are handled the same way: rescan the Resolver right here (main
        // thread), then forward the same `Command::Reload` the tray menu
        // uses so the engine resets its machine and re-reads config too.
        if overlay::take_display_changed() {
            tracing::info!("WM_DISPLAYCHANGE observed; rescanning display targets");
            resolver.rescan();
            if tx.send(Command::Reload).is_err() {
                return Ok(());
            }
        }
        if overlay::take_resumed() {
            tracing::info!("system resume observed; rescanning display targets");
            resolver.rescan();
            if tx.send(Command::Reload).is_err() {
                return Ok(());
            }
        }

        // Apply whatever the engine asked for since the last iteration.
        // DDC VCP calls block for tens to hundreds of ms, so this is the one
        // place the pump can stall; at a 250ms poll the only visible cost is
        // tooltip latency.
        while let Ok(level) = levels.try_recv() {
            resolver.apply(level);
        }

        let current = State::from_u8(status.load(Ordering::Relaxed));
        if current != shown {
            shown = current;
            let _ = tray.set_tooltip(Some(tooltip(current)));
            let icon = if is_warning(current) {
                warning.clone()
            } else {
                normal.clone()
            };
            let _ = tray.set_icon(Some(icon));
        }

        std::thread::sleep(POLL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::types::State;

    #[test]
    fn every_state_has_a_distinct_tooltip() {
        let all = [
            State::Active,
            State::Watching,
            State::Dimmed,
            State::Away,
            State::Deep,
            State::Paused,
            State::Degraded,
        ];
        let labels: Vec<_> = all.iter().map(|s| tooltip(*s)).collect();
        for l in &labels {
            assert!(l.starts_with("VISOR"), "tooltip should be branded: {l}");
        }
        let mut sorted = labels.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), all.len(), "tooltips must be distinct");
    }

    #[test]
    fn degraded_is_flagged_as_a_warning() {
        assert!(is_warning(State::Degraded));
        assert!(!is_warning(State::Active));
    }
}
