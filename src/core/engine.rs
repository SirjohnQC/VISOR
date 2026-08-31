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
        let mut machine = Machine::new(cfg.presence.clone(), cfg.display.dim_level);
        machine.set_awake_hold(cfg.display.hold_awake_while_present);
        Self {
            machine,
            idle,
            camera,
            display,
            camera_open: false,
            cadence: Duration::from_secs(1),
        }
    }

    pub fn state(&self) -> State {
        self.machine.state()
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

    /// Blocking loop. Returns when a `Quit` command arrives or the channel
    /// closes.
    pub fn run(mut self, rx: Receiver<Command>, status: Arc<AtomicU8>) {
        loop {
            let state = self.tick(Instant::now());
            status.store(state.as_u8(), Ordering::Relaxed);

            match rx.recv_timeout(self.cadence) {
                Ok(Command::Quit) => {
                    // Always leave the screen on when exiting (spec §2.1).
                    self.display.apply(DisplayLevel::Full);
                    if self.camera_open {
                        self.camera.close();
                    }
                    return;
                }
                Ok(cmd) => {
                    let (state, effects) = self.machine.command(cmd, Instant::now());
                    self.apply(effects, state);
                    status.store(state.as_u8(), Ordering::Relaxed);
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
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
}
