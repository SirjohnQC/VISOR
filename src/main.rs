use std::sync::Arc;
use std::sync::atomic::AtomicU8;
use std::sync::mpsc;
use std::thread;

use visor::actions::SpyDisplay;
use visor::config::Config;
use visor::core::engine::Engine;
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
    let display = Box::new(SpyDisplay::new());
    let engine = Engine::new(cfg, idle, camera, display);

    let (tx, rx) = mpsc::channel();
    let status = Arc::new(AtomicU8::new(0));

    let handle = thread::spawn({
        let status = Arc::clone(&status);
        move || engine.run(rx, status)
    });

    // The message pump must own the main thread; the engine ticks on the
    // spawned one.
    let outcome = visor::ui::tray::run(tx, status);
    if let Err(e) = &outcome {
        tracing::error!(error = %e, "tray failed");
    }

    // Join before exiting, so the engine's shutdown path -- which applies
    // DisplayLevel::Full -- actually completes. Exiting with the panel dark is
    // the one outcome this whole program exists to prevent, so this join is
    // load-bearing, not tidiness. `tray::run` has dropped its `Sender` by now,
    // so the engine sees either the Quit it was sent or a Disconnected; both
    // restore the display.
    if handle.join().is_err() {
        tracing::error!("engine thread panicked");
    }
    tracing::info!("VISOR stopped");
}
