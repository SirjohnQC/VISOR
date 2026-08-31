use crate::config::PresenceConfig;
use crate::core::types::{DisplayLevel, Effect, FaceResult, State};
use std::time::{Duration, Instant};

pub struct Machine {
    cfg: PresenceConfig,
    dim_level: u8,
    state: State,
    miss_streak_start: Option<Instant>,
    face_run: u8,
}

impl Machine {
    pub fn new(cfg: PresenceConfig, dim_level: u8) -> Self {
        Self {
            cfg,
            dim_level,
            state: State::Active,
            miss_streak_start: None,
            face_run: 0,
        }
    }

    pub fn state(&self) -> State {
        self.state
    }

    /// Spec §4.2 — a face smaller than `min_face_ratio` is not presence.
    fn normalise(&self, face: FaceResult) -> FaceResult {
        match face {
            FaceResult::Face { largest_ratio, .. } if largest_ratio < self.cfg.min_face_ratio => {
                FaceResult::NoFace
            }
            other => other,
        }
    }

    /// Spec §4.3 — maintain the miss streak.
    fn update_streak(&mut self, face: FaceResult, now: Instant) {
        match face {
            FaceResult::Face { .. } => {
                self.face_run = self.face_run.saturating_add(1);
                if self.face_run >= self.cfg.face_confirm {
                    self.miss_streak_start = None;
                }
            }
            FaceResult::Unknown => {
                self.face_run = 0;
                self.miss_streak_start = None;
            }
            FaceResult::NoFace => {
                self.face_run = 0;
                if self.miss_streak_start.is_none() {
                    self.miss_streak_start = Some(now);
                }
            }
        }
    }

    fn streak_for(&self, now: Instant) -> Duration {
        self.miss_streak_start
            .map(|s| now.saturating_duration_since(s))
            .unwrap_or(Duration::ZERO)
    }

    /// The state the ladder says we should be in for a given streak length.
    fn rung_for(&self, streak: Duration) -> State {
        if streak >= self.cfg.deep_after {
            State::Deep
        } else if streak >= self.cfg.away_after {
            State::Away
        } else if streak >= self.cfg.dim_after {
            State::Dimmed
        } else {
            State::Watching
        }
    }

    /// Position on the reduction ladder; larger means further down (darker).
    /// States that are not ladder rungs (Active, Paused, Degraded) have no ordinal.
    fn rung_ordinal(state: State) -> Option<u8> {
        match state {
            State::Watching => Some(0),
            State::Dimmed => Some(1),
            State::Away => Some(2),
            State::Deep => Some(3),
            _ => None,
        }
    }

    fn effects_for_rung(&self, rung: State) -> Vec<Effect> {
        match rung {
            State::Watching => vec![Effect::SetDisplay(DisplayLevel::Full)],
            State::Dimmed => vec![Effect::SetDisplay(DisplayLevel::Dim(self.dim_level))],
            State::Away => vec![
                Effect::SetDisplay(DisplayLevel::Black),
                Effect::SetSampleInterval(self.cfg.away_sample),
            ],
            State::Deep => vec![Effect::SetDisplay(DisplayLevel::Off)],
            _ => Vec::new(),
        }
    }

    pub fn step(&mut self, idle: Duration, face: FaceResult, now: Instant) -> (State, Vec<Effect>) {
        let face = self.normalise(face);
        let input_active = idle < self.cfg.idle_grace;
        let mut fx = Vec::new();

        if input_active {
            if self.state != State::Active {
                self.state = State::Active;
                self.miss_streak_start = None;
                self.face_run = 0;
                fx.push(Effect::SetDisplay(DisplayLevel::Full));
                fx.push(Effect::CloseCamera);
            }
            return (self.state, fx);
        }

        if self.state == State::Active {
            self.state = State::Watching;
            self.miss_streak_start = None;
            self.face_run = 0;
            fx.push(Effect::OpenCamera);
            fx.push(Effect::SetSampleInterval(self.cfg.sample_interval));
            return (self.state, fx);
        }

        self.update_streak(face, now);
        let rung = self.rung_for(self.streak_for(now));
        // Ruling F3 / spec §4.4, §4.7 — the ladder is downward-only. A cleared
        // streak (which a single Unknown causes) must never re-light the panel;
        // only the explicit restore path or returning input moves us back up.
        let deeper = match (Self::rung_ordinal(self.state), Self::rung_ordinal(rung)) {
            (Some(current), Some(next)) => next > current,
            _ => false,
        };
        if deeper {
            self.state = rung;
            fx.extend(self.effects_for_rung(rung));
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

    /// Drive the machine from Active into Watching, returning the base instant.
    fn into_watching(m: &mut Machine) -> Instant {
        let t0 = Instant::now();
        m.step(Duration::from_secs(30), FaceResult::Unknown, t0);
        assert_eq!(m.state(), State::Watching);
        t0
    }

    #[test]
    fn walks_the_full_ladder_on_one_unbroken_streak() {
        let mut m = machine();
        let t0 = into_watching(&mut m);

        // streak begins
        let (s, fx) = m.step(
            Duration::from_secs(31),
            FaceResult::NoFace,
            t0 + Duration::from_secs(1),
        );
        assert_eq!(s, State::Watching, "not dimmed until dim_after elapses");
        assert!(fx.is_empty());

        // +20s of misses -> Dimmed
        let (s, fx) = m.step(
            Duration::from_secs(51),
            FaceResult::NoFace,
            t0 + Duration::from_secs(21),
        );
        assert_eq!(s, State::Dimmed);
        assert_eq!(fx, vec![Effect::SetDisplay(DisplayLevel::Dim(20))]);

        // +45s of misses -> Away (black overlay, panel still powered)
        let (s, fx) = m.step(
            Duration::from_secs(76),
            FaceResult::NoFace,
            t0 + Duration::from_secs(46),
        );
        assert_eq!(s, State::Away);
        assert_eq!(
            fx,
            vec![
                Effect::SetDisplay(DisplayLevel::Black),
                Effect::SetSampleInterval(Duration::from_secs(1)),
            ]
        );

        // +15m of misses -> Deep (true power off)
        let (s, fx) = m.step(
            Duration::from_secs(960),
            FaceResult::NoFace,
            t0 + Duration::from_secs(901),
        );
        assert_eq!(s, State::Deep);
        assert_eq!(fx, vec![Effect::SetDisplay(DisplayLevel::Off)]);
    }

    #[test]
    fn one_isolated_face_hit_does_not_reset_the_streak() {
        let mut m = machine();
        let t0 = into_watching(&mut m);

        m.step(
            Duration::from_secs(31),
            FaceResult::NoFace,
            t0 + Duration::from_secs(1),
        );
        // a single hit amid misses — face_confirm is 2, so the streak survives
        m.step(
            Duration::from_secs(41),
            PRESENT,
            t0 + Duration::from_secs(11),
        );
        let (s, _) = m.step(
            Duration::from_secs(51),
            FaceResult::NoFace,
            t0 + Duration::from_secs(21),
        );
        assert_eq!(s, State::Dimmed, "streak measured from the original miss");
    }

    #[test]
    fn two_consecutive_face_hits_clear_the_streak() {
        let mut m = machine();
        let t0 = into_watching(&mut m);

        m.step(
            Duration::from_secs(31),
            FaceResult::NoFace,
            t0 + Duration::from_secs(1),
        );
        m.step(
            Duration::from_secs(33),
            PRESENT,
            t0 + Duration::from_secs(3),
        );
        m.step(
            Duration::from_secs(35),
            PRESENT,
            t0 + Duration::from_secs(5),
        );
        let (s, _) = m.step(
            Duration::from_secs(51),
            FaceResult::NoFace,
            t0 + Duration::from_secs(21),
        );
        assert_eq!(
            s,
            State::Watching,
            "streak restarted at t0+21, dim not yet due"
        );
    }

    #[test]
    fn a_single_unknown_clears_the_streak() {
        let mut m = machine();
        let t0 = into_watching(&mut m);

        m.step(
            Duration::from_secs(31),
            FaceResult::NoFace,
            t0 + Duration::from_secs(1),
        );
        m.step(
            Duration::from_secs(33),
            FaceResult::Unknown,
            t0 + Duration::from_secs(3),
        );
        let (s, _) = m.step(
            Duration::from_secs(51),
            FaceResult::NoFace,
            t0 + Duration::from_secs(21),
        );
        assert_eq!(s, State::Watching, "Unknown reset the streak immediately");
    }

    #[test]
    fn a_distant_face_is_downgraded_to_no_face() {
        let mut m = machine();
        let t0 = into_watching(&mut m);
        let distant = FaceResult::Face {
            count: 1,
            largest_ratio: 0.05,
        };

        m.step(
            Duration::from_secs(31),
            distant,
            t0 + Duration::from_secs(1),
        );
        let (s, _) = m.step(
            Duration::from_secs(51),
            distant,
            t0 + Duration::from_secs(21),
        );
        assert_eq!(s, State::Dimmed, "someone across the room is not presence");
    }

    #[test]
    fn an_unknown_never_walks_the_ladder_back_up() {
        let mut m = machine();
        let t0 = into_watching(&mut m);

        // Fall all the way to Away.
        m.step(
            Duration::from_secs(31),
            FaceResult::NoFace,
            t0 + Duration::from_secs(1),
        );
        let (s, _) = m.step(
            Duration::from_secs(76),
            FaceResult::NoFace,
            t0 + Duration::from_secs(46),
        );
        assert_eq!(s, State::Away);

        // A camera hiccup clears the streak (spec §4.3) but must NOT wake the panel.
        let (s, fx) = m.step(
            Duration::from_secs(80),
            FaceResult::Unknown,
            t0 + Duration::from_secs(50),
        );
        assert_eq!(
            s,
            State::Away,
            "spec §4.7: Unknown does not trigger a wake from Away"
        );
        assert!(
            fx.is_empty(),
            "no display effect may be emitted for an Unknown"
        );
    }
}
