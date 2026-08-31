use crate::actions::DisplayControl;
use crate::config::Config;
use crate::core::machine::Machine;
use crate::core::types::{Command, DisplayLevel, Effect, FaceResult, State};
use crate::sense::camera::Camera;
use crate::sense::idle::IdleSource;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
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

pub struct Engine {
    machine: Machine,
    idle: Arc<dyn IdleSource + Sync>,
    camera: Box<dyn Camera>,
    display: Box<dyn DisplayControl>,
    camera_open: bool,
    cadence: Duration,
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
        let (state, effects) = self.machine.step(idle, face, now);
        self.apply(effects, state);
        state
    }

    fn apply(&mut self, effects: Vec<Effect>, state: State) {
        for e in effects {
            match e {
                Effect::OpenCamera => {
                    if !self.camera_open {
                        self.camera.open();
                        self.camera_open = true;
                    }
                }
                Effect::CloseCamera => {
                    if self.camera_open {
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
    pub fn run(mut self, rx: Receiver<Command>, status: Arc<AtomicU8>) {
        loop {
            let state = self.tick(Instant::now());
            status.store(state.as_u8(), Ordering::Relaxed);

            match rx.recv_timeout(self.cadence) {
                Ok(Command::Quit) => {
                    self.shutdown(state);
                    return;
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
    use std::sync::Arc;

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
        engine.run(rx, status.clone());

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
        engine.run(rx, Arc::new(AtomicU8::new(0)));

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
