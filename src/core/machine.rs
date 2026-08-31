use crate::config::PresenceConfig;
use crate::core::types::{DisplayLevel, Effect, FaceResult, State};
use std::time::{Duration, Instant};

pub struct Machine {
    cfg: PresenceConfig,
    #[allow(dead_code)]
    dim_level: u8,
    state: State,
}

impl Machine {
    pub fn new(cfg: PresenceConfig, dim_level: u8) -> Self {
        Self {
            cfg,
            dim_level,
            state: State::Active,
        }
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn step(
        &mut self,
        idle: Duration,
        _face: FaceResult,
        _now: Instant,
    ) -> (State, Vec<Effect>) {
        let mut fx = Vec::new();
        let input_active = idle < self.cfg.idle_grace;

        match (self.state, input_active) {
            (State::Active, false) => {
                self.state = State::Watching;
                fx.push(Effect::OpenCamera);
                fx.push(Effect::SetSampleInterval(self.cfg.sample_interval));
            }
            (State::Watching, true) => {
                self.state = State::Active;
                fx.push(Effect::SetDisplay(DisplayLevel::Full));
                fx.push(Effect::CloseCamera);
            }
            _ => {}
        }
        (self.state, fx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn machine() -> Machine {
        Machine::new(crate::config::PresenceConfig::default(), 20)
    }

    const PRESENT: FaceResult = FaceResult::Face {
        count: 1,
        largest_ratio: 0.4,
    };

    #[test]
    fn starts_active_and_stays_active_while_input_is_recent() {
        let mut m = machine();
        let t0 = Instant::now();
        assert_eq!(m.state(), State::Active);

        let (s, fx) = m.step(Duration::from_secs(5), FaceResult::NoFace, t0);
        assert_eq!(s, State::Active);
        assert!(fx.is_empty(), "no effects while nothing changes");
    }

    #[test]
    fn opens_the_camera_once_input_goes_idle() {
        let mut m = machine();
        let t0 = Instant::now();

        let (s, fx) = m.step(Duration::from_secs(30), FaceResult::Unknown, t0);
        assert_eq!(s, State::Watching);
        assert_eq!(
            fx,
            vec![
                Effect::OpenCamera,
                Effect::SetSampleInterval(Duration::from_secs(2)),
            ]
        );
    }

    #[test]
    fn input_returns_to_active_and_closes_the_camera() {
        let mut m = machine();
        let t0 = Instant::now();
        m.step(Duration::from_secs(30), FaceResult::Unknown, t0);

        let (s, fx) = m.step(Duration::ZERO, PRESENT, t0 + Duration::from_secs(2));
        assert_eq!(s, State::Active);
        assert_eq!(
            fx,
            vec![Effect::SetDisplay(DisplayLevel::Full), Effect::CloseCamera]
        );
    }

    #[test]
    fn a_face_seen_while_watching_produces_no_effects() {
        let mut m = machine();
        let t0 = Instant::now();
        m.step(Duration::from_secs(30), FaceResult::Unknown, t0);

        let (s, fx) = m.step(
            Duration::from_secs(32),
            PRESENT,
            t0 + Duration::from_secs(2),
        );
        assert_eq!(s, State::Watching);
        assert!(fx.is_empty());
    }
}
