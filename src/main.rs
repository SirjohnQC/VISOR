use std::sync::Arc;
use std::sync::atomic::AtomicU8;
use std::sync::mpsc;
use std::thread;

use visor::actions::{ChannelDisplay, Resolver};
use visor::config::Config;
use visor::core::engine::Engine;
use visor::core::types::DisplayLevel;
use visor::sense::camera::FakeCamera;
use visor::sense::idle::Win32Idle;

fn main() {
    let path = Config::default_path();
    Config::write_defaults_if_missing(&path);

    // Load once with defaults so we know what level to log at, then re-read
    // through the logger so parse failures are actually recorded.
    let bootstrap = Config::load_or_default(&path);
    let _guard = match visor::logging::init(&bootstrap.log.level) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("visor: could not initialise logging: {e}");
            return;
        }
    };

    let cfg = Config::load_or_default(&path);
    tracing::info!(?cfg, "VISOR starting");

    // Real idle source and a real tray, but the camera and display are still
    // fakes: real vision and real display control land in later tasks. An
    // empty script makes `FakeCamera` report `NoFace` forever, which is
    // enough to walk the ladder in the log.
    let idle: Arc<dyn visor::sense::idle::IdleSource + Sync> = Arc::new(Win32Idle::new());
    let camera = Box::new(FakeCamera::new(Vec::new()));

    // Ruling F8: the real display stack lives on THIS thread. Overlay windows
    // must belong to the thread that pumps messages, and PHYSICAL_MONITOR is
    // not Send, so `Resolver` cannot cross a thread boundary at all -- the type
    // system enforces that for us. The engine gets a channel instead.
    let mut resolver = Resolver::new(&cfg.display);
    let (level_tx, level_rx) = mpsc::channel();
    let display = Box::new(ChannelDisplay { tx: level_tx });
    let engine = Engine::new(cfg, idle, camera, display);

    let (tx, rx) = mpsc::channel();
    let status = Arc::new(AtomicU8::new(0));

    let handle = thread::spawn({
        let status = Arc::clone(&status);
        move || engine.run(rx, status)
    });

    // The message pump must own the main thread; the engine ticks on the
    // spawned one.
    if let Err(e) = visor::ui::tray::run(tx, status, level_rx, &mut resolver) {
        tracing::error!(error = %e, "tray failed");
    }

    // Join so the engine stops ticking before we restore. `tray::run` has
    // dropped its Sender by now, so the engine sees either the Quit it was sent
    // or a Disconnected; both end the loop.
    if handle.join().is_err() {
        tracing::error!("engine thread panicked");
    }

    // Ruling F9. The engine's own shutdown sends Full down the level channel,
    // but nobody is left to drain it -- tray::run has returned and this thread
    // is what would have applied it. For the overlay that would be survivable,
    // since process exit destroys the windows. For DDC it is not: a panel
    // powered off with SetVCPFeature(0xD6, 4) STAYS OFF after we exit, leaving
    // the user in the dark with no VISOR running to fix it.
    //
    // So restore here, unconditionally, on the thread that owns the hardware --
    // even if the tray errored or the engine panicked. This is the last line of
    // defence for the one property the whole program exists to guarantee.
    resolver.apply(DisplayLevel::Full);
    tracing::info!("VISOR stopped");
}
