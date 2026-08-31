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

    // Real idle source, but the camera and display are still fakes: real
    // vision (Task 9-ish) and real display control (later tasks) are not
    // wired up yet. An empty script makes `FakeCamera` report `NoFace`
    // forever, which is enough to walk the ladder in the log.
    let idle: Arc<dyn visor::sense::idle::IdleSource + Sync> = Arc::new(Win32Idle::new());
    let camera = Box::new(FakeCamera::new(Vec::new()));
    let display = Box::new(SpyDisplay::new());
    let engine = Engine::new(cfg, idle, camera, display);

    // The tray icon and the real command channel arrive in Task 8. For now
    // there is no producer of `Command`s, so we just hold `tx` alive for the
    // lifetime of `main` -- dropping it would make `rx.recv_timeout` see
    // `Disconnected` on the engine thread's very first timeout and return
    // immediately, ending the loop before anything is observable in the log.
    let (_tx, rx) = mpsc::channel();
    let status = Arc::new(AtomicU8::new(0));

    let handle = thread::spawn(move || engine.run(rx, status));

    // Keep `main` alive so the engine thread keeps ticking. The Win32
    // message pump that should own this thread lands in Task 8; until then,
    // blocking on the engine thread's join handle is the simplest way to
    // keep the process running (it will not return, since nothing ever
    // sends `Command::Quit`).
    handle.join().expect("engine thread panicked");
}
