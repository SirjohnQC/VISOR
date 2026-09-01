use crate::actions::DisplayControl;
use crate::config::Config;
use crate::core::check::{CameraVerdict, camera_verdict};
use crate::core::machine::Machine;
use crate::core::types::{Command, DisplayLevel, Effect, FaceResult, State};
use crate::sense::camera::Camera;
use crate::sense::idle::IdleSource;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

impl State {
    pub fn as_u8(self) -> u8 {
        match self {
            State::Active => 0,
            State::Watching => 1,
            State::Dimmed => 2,
            State::Away => 3,
            State::Deep => 4,
            State::Paused => 5,
            State::Degraded => 6,
        }
    }
    pub fn from_u8(v: u8) -> State {
        match v {
            1 => State::Watching,
            2 => State::Dimmed,
            3 => State::Away,
            4 => State::Deep,
            5 => State::Paused,
            6 => State::Degraded,
            _ => State::Active,
        }
    }
}

/// Where the engine publishes the result of a `CheckCamera` run for the tray
/// to display. The engine owns the camera, so only it can run the probe.
pub type CheckSlot = Arc<Mutex<Option<String>>>;

/// How many frames a camera check samples, and how long it waits between them.
/// Two seconds total: long enough for exposure to settle and for the user to be
/// looking at the screen, short enough that blocking the tick loop is harmless.
const CHECK_SAMPLES: usize = 10;
const CHECK_INTERVAL: Duration = Duration::from_millis(200);

pub struct Engine {
    machine: Machine,
    idle: Arc<dyn IdleSource + Sync>,
    camera: Box<dyn Camera>,
    display: Box<dyn DisplayControl>,
    camera_open: bool,
    cadence: Duration,
    /// Kept alongside the machine so `check_camera` can judge against the same
    /// threshold the machine uses. Updated on reload.
    min_face_ratio: f32,
}

impl Engine {
    pub fn new(
        cfg: Config,
        idle: Arc<dyn IdleSource + Sync>,
        camera: Box<dyn Camera>,
        display: Box<dyn DisplayControl>,
    ) -> Self {
        Self {
            machine: Self::build_machine(&cfg),
            idle,
            camera,
            display,
            camera_open: false,
            cadence: Duration::from_secs(1),
            min_face_ratio: cfg.presence.min_face_ratio,
        }
    }

    fn build_machine(cfg: &Config) -> Machine {
        let mut machine = Machine::new(cfg.presence.clone(), cfg.display.dim_level);
        machine.set_awake_hold(cfg.display.hold_awake_while_present);
        machine
    }

    pub fn state(&self) -> State {
        self.machine.state()
    }

    /// Rebuild the machine from a new config, preserving nothing — a reload
    /// is a deliberate reset, and it always leaves the display lit (spec
    /// §2.1). The camera is closed too: the fresh machine starts in `Active`,
    /// which never has the camera open, so leaving it running would be a
    /// stale effect the new machine never asked for.
    pub fn reload(&mut self, cfg: Config) {
        self.display.apply(DisplayLevel::Full);
        self.machine = Self::build_machine(&cfg);
        self.min_face_ratio = cfg.presence.min_face_ratio;
        if self.camera_open {
            self.camera.close();
            self.camera_open = false;
        }
        tracing::info!("config reloaded");
    }

    /// One iteration. Separated from `run` so tests can drive it with a
    /// simulated clock instead of sleeping.
    pub fn tick(&mut self, now: Instant) -> State {
        let idle = self.idle.idle_for();
        let face = if self.camera_open {
            self.camera.probe()
        } else {
            FaceResult::Unknown
        };
        // At `debug` this is the only window into what the camera actually
        // sees, tick by tick -- the difference between "it saw you and stayed
        // put" and "it never saw anything". Gated behind camera_open so it
        // does not spam `Unknown` for every second spent Active with the lens
        // deliberately shut.
        if self.camera_open {
            tracing::debug!(?face, state = ?self.machine.state(), "probe");
        }
        let before = self.machine.state();
        let (state, effects) = self.machine.step(idle, face, now);
        // The single most useful line in the log. Without it a user watching
        // VISOR do nothing has no way to tell whether it never went idle,
        // never saw the camera, or is sitting in a rung it will not leave --
        // the display lines below only fire when something visibly changed.
        if state != before {
            tracing::info!(?before, after = ?state, ?face, idle_secs = idle.as_secs(), "state");
        }
        self.apply(effects, state);
        state
    }

    fn apply(&mut self, effects: Vec<Effect>, state: State) {
        for e in effects {
            match e {
                Effect::OpenCamera => {
                    if !self.camera_open {
                        // Logged because it is the moment the lens actually
                        // opens. A user who wants to audit that claim should
                        // be able to read it off the log rather than take it
                        // on trust.
                        tracing::info!(?state, "camera opened");
                        self.camera.open();
                        self.camera_open = true;
                    }
                }
                Effect::CloseCamera => {
                    if self.camera_open {
                        tracing::info!(?state, "camera closed");
                        self.camera.close();
                        self.camera_open = false;
                    }
                }
                Effect::SetSampleInterval(d) => self.cadence = d,
                Effect::SetDisplay(level) => {
                    tracing::info!(?state, ?level, "display");
                    self.display.apply(level);
                }
                Effect::SetAwakeHold(on) => crate::actions::set_awake_hold(on),
            }
        }
    }

    /// Spec §2.1 — the process must never exit with the screen dark. Goes
    /// through `apply` so the shutdown transition is logged like every other
    /// one and the camera-close guard is not duplicated.
    fn shutdown(&mut self, state: State) {
        self.apply(
            vec![Effect::SetDisplay(DisplayLevel::Full), Effect::CloseCamera],
            state,
        );
    }

    /// Blocking loop. Returns when a `Quit` command arrives or the channel
    /// closes.
    /// Probe the camera a few times and report what it can see.
    ///
    /// Opens the camera if it is closed and closes it again afterwards, so a
    /// check from `Active` -- the common case, where the camera is deliberately
    /// off -- does not leave it running. Blocks the tick loop for about two
    /// seconds; that is acceptable because this only ever runs when the user
    /// explicitly asks for it from the tray.
    pub fn check_camera(&mut self) -> CameraVerdict {
        let was_open = self.camera_open;
        if !was_open {
            self.camera.open();
            self.camera_open = true;
        }

        let mut samples = Vec::with_capacity(CHECK_SAMPLES);
        for i in 0..CHECK_SAMPLES {
            if i > 0 {
                std::thread::sleep(CHECK_INTERVAL);
            }
            samples.push(self.camera.probe());
        }

        if !was_open {
            self.camera.close();
            self.camera_open = false;
        }

        let verdict = camera_verdict(&samples, self.min_face_ratio);
        tracing::info!(?verdict, "camera check: {}", verdict.message());
        verdict
    }

    pub fn run(mut self, rx: Receiver<Command>, status: Arc<AtomicU8>, check: CheckSlot) {
        loop {
            let state = self.tick(Instant::now());
            status.store(state.as_u8(), Ordering::Relaxed);

            match rx.recv_timeout(self.cadence) {
                Ok(Command::Quit) => {
                    self.shutdown(state);
                    return;
                }
                Ok(Command::CheckCamera) => {
                    let verdict = self.check_camera();
                    if let Ok(mut slot) = check.lock() {
                        *slot = Some(verdict.message());
                    }
                }
                Ok(Command::Reload) => {
                    // Config lives on disk, not in the message: re-read it
                    // here rather than plumbing its contents through the
                    // channel. `Resolver::rescan` is NOT called from here —
                    // ruling F8 keeps the `Resolver` on the main thread, so
                    // the pump calls `rescan` itself when it forwards this
                    // same `Command::Reload` (see `ui::tray::run`).
                    let cfg = Config::load_or_default(&Config::default_path());
                    self.reload(cfg);
                    status.store(self.state().as_u8(), Ordering::Relaxed);
                }
                Ok(cmd) => {
                    let (state, effects) = self.machine.command(cmd, Instant::now());
                    self.apply(effects, state);
                    status.store(state.as_u8(), Ordering::Relaxed);
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    // The command channel is gone — whoever owned the Sender
                    // died or dropped it. Exiting is right, but exiting with a
                    // black screen is the one outcome spec §2.1 forbids, so
                    // this path restores the display exactly like Quit does.
                    self.shutdown(state);
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::SpyDisplay;
    use crate::sense::camera::FakeCamera;
    use crate::sense::idle::FakeIdle;
    use std::sync::{Arc, Mutex};

    #[test]
    fn a_camera_check_from_active_leaves_the_camera_closed_again() {
        // The common case: the user asks from Active, where the camera is
        // deliberately off. Checking must not leave it running afterwards --
        // that would quietly break the promise that the lens stays shut while
        // the user is present.
        let cam = FakeCamera::new(vec![
            FaceResult::Face {
                count: 1,
                largest_ratio: 0.42
            };
            16
        ]);
        let opens = cam.open_count();
        let closes = cam.close_count();
        let mut engine = Engine::new(
            Config::default(),
            Arc::new(FakeIdle::new(Duration::ZERO)),
            Box::new(cam),
            Box::new(SpyDisplay::new()),
        );

        let verdict = engine.check_camera();
        assert!(
            verdict.is_ok(),
            "a 0.42 face clears the 0.15 default: {verdict:?}"
        );
        assert_eq!(opens.load(Ordering::Relaxed), 1);
        assert_eq!(closes.load(Ordering::Relaxed), 1, "must close it again");
        assert_eq!(
            engine.state(),
            State::Active,
            "checking is not a state change"
        );
    }

    #[test]
    fn a_camera_check_reports_a_face_that_is_too_small_to_count() {
        // The failure this whole feature exists for: the camera works and sees
        // the user, but below min_face_ratio the machine treats them as away,
        // and nothing else in VISOR would ever tell them why.
        let cam = FakeCamera::new(vec![
            FaceResult::Face {
                count: 1,
                largest_ratio: 0.08
            };
            16
        ]);
        let mut engine = Engine::new(
            Config::default(),
            Arc::new(FakeIdle::new(Duration::ZERO)),
            Box::new(cam),
            Box::new(SpyDisplay::new()),
        );

        let verdict = engine.check_camera();
        assert!(!verdict.is_ok());
        let msg = verdict.message();
        assert!(msg.contains("0.080"), "reports what it saw: {msg}");
        assert!(msg.contains("min_face_ratio"), "names the setting: {msg}");
    }

    #[test]
    fn a_camera_check_while_watching_leaves_the_camera_open() {
        // Checking from a state that legitimately has the camera open must not
        // close it and strip the machine of its input.
        let idle = Arc::new(FakeIdle::new(Duration::from_secs(30)));
        let cam = FakeCamera::new(vec![FaceResult::NoFace; 16]);
        let closes = cam.close_count();
        let mut engine = Engine::new(
            Config::default(),
            idle,
            Box::new(cam),
            Box::new(SpyDisplay::new()),
        );
        engine.tick(Instant::now());
        assert_eq!(engine.state(), State::Watching);

        engine.check_camera();
        assert_eq!(closes.load(Ordering::Relaxed), 0, "must leave it open");
    }

    #[test]
    fn the_state_byte_round_trips_for_every_state() {
        for s in [
            State::Active,
            State::Watching,
            State::Dimmed,
            State::Away,
            State::Deep,
            State::Paused,
            State::Degraded,
        ] {
            assert_eq!(State::from_u8(s.as_u8()), s, "round trip failed for {s:?}");
        }
        // Anything the tray could not have written maps to the safest row:
        // camera closed, display full.
        assert_eq!(State::from_u8(200), State::Active);
    }

    #[test]
    fn entering_a_rung_updates_the_tick_cadence() {
        let idle = Arc::new(FakeIdle::new(Duration::ZERO));
        let mut engine = Engine::new(
            Config::default(),
            idle.clone(),
            Box::new(FakeCamera::new(vec![FaceResult::NoFace; 64])),
            Box::new(SpyDisplay::new()),
        );
        let t0 = Instant::now();

        // `cadence` seeds at 1s and is only ever changed by SetSampleInterval.
        assert_eq!(engine.cadence, Duration::from_secs(1));

        idle.set(Duration::from_secs(30));
        engine.tick(t0 + Duration::from_secs(30)); // -> Watching
        assert_eq!(
            engine.cadence,
            Duration::from_secs(2),
            "Watching must adopt sample_interval"
        );

        engine.tick(t0 + Duration::from_secs(31)); // streak starts
        engine.tick(t0 + Duration::from_secs(52)); // -> Dimmed
        engine.tick(t0 + Duration::from_secs(76)); // -> Away
        assert_eq!(engine.state(), State::Away);
        assert_eq!(
            engine.cadence,
            Duration::from_secs(1),
            "Away must adopt away_sample"
        );
    }

    #[test]
    fn quit_restores_the_display_and_reports_the_last_state() {
        let idle = Arc::new(FakeIdle::new(Duration::from_secs(30)));
        let display = SpyDisplay::new();
        let seen = display.log();
        let engine = Engine::new(
            Config::default(),
            idle,
            Box::new(FakeCamera::new(vec![FaceResult::NoFace; 8])),
            Box::new(display),
        );
        let status = Arc::new(AtomicU8::new(255));

        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(Command::Quit).unwrap();
        engine.run(rx, status.clone(), Arc::new(Mutex::new(None)));

        assert_eq!(
            seen.lock().unwrap().last(),
            Some(&DisplayLevel::Full),
            "spec §2.1: never exit with the screen dark"
        );
        assert_eq!(
            State::from_u8(status.load(Ordering::Relaxed)),
            State::Watching,
            "status reflects the tick that ran before Quit"
        );
    }

    #[test]
    fn a_dropped_sender_also_restores_the_display() {
        // Regression: the Disconnected arm used to `return` without restoring,
        // so a tray that died mid-run would leave the panel black on exit.
        let idle = Arc::new(FakeIdle::new(Duration::from_secs(30)));
        let display = SpyDisplay::new();
        let seen = display.log();
        let engine = Engine::new(
            Config::default(),
            idle,
            Box::new(FakeCamera::new(vec![FaceResult::NoFace; 8])),
            Box::new(display),
        );

        let (tx, rx) = std::sync::mpsc::channel::<Command>();
        drop(tx);
        engine.run(rx, Arc::new(AtomicU8::new(0)), Arc::new(Mutex::new(None)));

        assert_eq!(
            seen.lock().unwrap().last(),
            Some(&DisplayLevel::Full),
            "a dropped Sender must not strand the screen dark"
        );
    }

    #[test]
    fn the_engine_drives_the_full_ladder_and_records_display_changes() {
        let idle = Arc::new(FakeIdle::new(Duration::ZERO));
        let cam = FakeCamera::new(vec![FaceResult::NoFace; 64]);
        let display = SpyDisplay::new();
        let seen = display.log();

        let mut engine = Engine::new(
            Config::default(),
            idle.clone(),
            Box::new(cam),
            Box::new(display),
        );

        // Simulated clock: the engine's tick takes `now` as a parameter so
        // tests can drive it without sleeping.
        let t0 = Instant::now();
        engine.tick(t0); // Active
        idle.set(Duration::from_secs(30));
        engine.tick(t0 + Duration::from_secs(30)); // -> Watching, camera opens
        engine.tick(t0 + Duration::from_secs(31)); // first NoFace: streak starts
        engine.tick(t0 + Duration::from_secs(52)); // -> Dimmed
        engine.tick(t0 + Duration::from_secs(76)); // -> Away
        engine.tick(t0 + Duration::from_secs(940)); // -> Deep

        assert_eq!(engine.state(), State::Deep);
        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                DisplayLevel::Dim(20),
                DisplayLevel::Black,
                DisplayLevel::Off
            ]
        );
    }

    #[test]
    fn the_camera_is_opened_only_while_input_is_idle() {
        let idle = Arc::new(FakeIdle::new(Duration::ZERO));
        let cam = FakeCamera::new(vec![FaceResult::NoFace; 8]);
        let opens = cam.open_count();
        let closes = cam.close_count();

        let mut engine = Engine::new(
            Config::default(),
            idle.clone(),
            Box::new(cam),
            Box::new(SpyDisplay::new()),
        );

        let t0 = Instant::now();
        engine.tick(t0);
        assert_eq!(opens.load(Ordering::Relaxed), 0, "closed while active");

        idle.set(Duration::from_secs(30));
        engine.tick(t0 + Duration::from_secs(30));
        assert_eq!(opens.load(Ordering::Relaxed), 1);

        idle.set(Duration::ZERO);
        engine.tick(t0 + Duration::from_secs(31));
        assert_eq!(closes.load(Ordering::Relaxed), 1, "closed on return");
    }

    #[test]
    fn a_failing_camera_degrades_and_restores_the_display() {
        let idle = Arc::new(FakeIdle::new(Duration::from_secs(30)));
        let cam = FakeCamera::new(vec![FaceResult::Unknown; 8]);
        let display = SpyDisplay::new();
        let seen = display.log();

        let mut engine = Engine::new(Config::default(), idle, Box::new(cam), Box::new(display));

        let t0 = Instant::now();
        for i in 0..6 {
            engine.tick(t0 + Duration::from_secs(30 + i));
        }
        assert_eq!(engine.state(), State::Degraded);
        assert_eq!(*seen.lock().unwrap(), vec![DisplayLevel::Full]);
    }

    #[test]
    fn reload_applies_new_thresholds_without_restarting() {
        let idle = Arc::new(FakeIdle::new(Duration::from_secs(30)));
        let mut engine = Engine::new(
            Config::default(),
            idle,
            Box::new(FakeCamera::new(vec![FaceResult::NoFace; 32])),
            Box::new(SpyDisplay::new()),
        );
        let t0 = Instant::now();
        engine.tick(t0);

        let mut cfg = Config::default();
        cfg.presence.dim_after = Duration::from_secs(5);
        engine.reload(cfg);

        engine.tick(t0 + Duration::from_secs(1)); // Active -> Watching, camera opens
        engine.tick(t0 + Duration::from_secs(2)); // first NoFace: streak STARTS here
        let s = engine.tick(t0 + Duration::from_secs(8)); // streak 6s >= new dim_after 5s
        assert_eq!(s, State::Dimmed, "new dim_after took effect");
    }

    #[test]
    fn reload_always_leaves_the_display_lit() {
        let idle = Arc::new(FakeIdle::new(Duration::from_secs(30)));
        let display = SpyDisplay::new();
        let seen = display.log();
        let mut engine = Engine::new(
            Config::default(),
            idle,
            Box::new(FakeCamera::new(vec![FaceResult::NoFace; 64])),
            Box::new(display),
        );
        let t0 = Instant::now();
        engine.tick(t0);
        engine.tick(t0 + Duration::from_secs(1));
        engine.tick(t0 + Duration::from_secs(22)); // -> Dimmed
        assert_eq!(engine.state(), State::Dimmed);

        engine.reload(Config::default());
        assert_eq!(seen.lock().unwrap().last(), Some(&DisplayLevel::Full));
        assert_eq!(engine.state(), State::Active);
    }
}
