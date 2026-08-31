use crate::config::PresenceConfig;
use crate::core::types::{Command, DisplayLevel, Effect, FaceResult, State};
use std::time::{Duration, Instant};

/// Spec §4.7 — Degraded retries the camera every 5 minutes.
const DEGRADED_RETRY: Duration = Duration::from_secs(5 * 60);

/// Spec §4.6 — a camera-triggered restore holds probation until confirmed
/// or expired. `fallback` is the rung the machine returns to on expiry.
#[derive(Debug, Clone, Copy)]
struct Probation {
    since: Instant,
    fallback: State,
}

pub struct Machine {
    cfg: PresenceConfig,
    dim_level: u8,
    state: State,
    miss_streak_start: Option<Instant>,
    face_run: u8,
    probation: Option<Probation>,
    unknown_run: u8,
    degraded_since: Option<Instant>,
    /// Spec §7 `hold_awake_while_present`. Off by default.
    awake_hold: bool,
    /// Last value of `SetAwakeHold` actually emitted, so the effect is only
    /// re-emitted on change (ruling F5(b)).
    awake_hold_asserted: bool,
}

impl Machine {
    pub fn new(cfg: PresenceConfig, dim_level: u8) -> Self {
        Self {
            cfg,
            dim_level,
            state: State::Active,
            miss_streak_start: None,
            face_run: 0,
            probation: None,
            unknown_run: 0,
            degraded_since: None,
            awake_hold: false,
            awake_hold_asserted: false,
        }
    }

    pub fn state(&self) -> State {
        self.state
    }

    /// Enable spec §7 hold_awake_while_present.
    pub fn set_awake_hold(&mut self, enabled: bool) {
        self.awake_hold = enabled;
    }

    /// Spec §7 — `SetAwakeHold` is emitted only when the presence-derived
    /// value actually changes, never on every tick (ruling F5(b)); otherwise
    /// it would break the `assert!(fx.is_empty())` tests from Tasks 2-4
    /// whenever the hold is enabled.
    fn awake_hold_effect(&mut self, new_state: State) -> Option<Effect> {
        if !self.awake_hold {
            return None;
        }
        let present = matches!(new_state, State::Active | State::Watching);
        if present == self.awake_hold_asserted {
            return None;
        }
        self.awake_hold_asserted = present;
        Some(Effect::SetAwakeHold(present))
    }

    fn recover_from_fault(&mut self) -> Vec<Effect> {
        self.state = State::Active;
        self.miss_streak_start = None;
        self.face_run = 0;
        self.unknown_run = 0;
        self.probation = None;
        self.degraded_since = None;
        Vec::new()
    }

    pub fn command(&mut self, cmd: Command, _now: Instant) -> (State, Vec<Effect>) {
        match cmd {
            Command::Pause if self.state != State::Paused => {
                self.state = State::Paused;
                self.miss_streak_start = None;
                self.face_run = 0;
                self.probation = None;
                // `step` returns immediately while Paused, so this is the only
                // chance to release the hold. Without it, pausing VISOR would
                // keep the system awake for the whole duration of the pause.
                let mut fx = vec![Effect::SetDisplay(DisplayLevel::Full), Effect::CloseCamera];
                if let Some(e) = self.awake_hold_effect(self.state) {
                    fx.push(e);
                }
                (self.state, fx)
            }
            Command::Resume if self.state == State::Paused => {
                self.state = State::Active;
                let mut fx = Vec::new();
                if let Some(e) = self.awake_hold_effect(self.state) {
                    fx.push(e);
                }
                (self.state, fx)
            }
            _ => (self.state, Vec::new()),
        }
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
        if self.state == State::Paused {
            return (self.state, Vec::new());
        }

        if self.state == State::Degraded {
            // Spec §4.7 — the camera is retried every 5 minutes; a
            // non-Unknown result on a due retry returns to Active.
            //
            // Ruling: this check sits ahead of the `input_active` and
            // `state == State::Active` blocks below, so returning input does
            // NOT itself lift Degraded. That is intentional: input does not
            // repair a broken camera, and Degraded has already restored the
            // display to full and closed the camera, so there is nothing for
            // input to fix.
            let due = self
                .degraded_since
                .map(|s| now.saturating_duration_since(s) >= DEGRADED_RETRY)
                .unwrap_or(true);
            if due && !matches!(face, FaceResult::Unknown) {
                let fx = self.recover_from_fault();
                return (self.state, fx);
            }
            return (self.state, Vec::new());
        }

        let face = self.normalise(face);
        let input_active = idle < self.cfg.idle_grace;
        let mut fx = Vec::new();

        if input_active {
            if self.state != State::Active {
                self.state = State::Active;
                self.miss_streak_start = None;
                self.face_run = 0;
                self.probation = None;
                fx.push(Effect::SetDisplay(DisplayLevel::Full));
                fx.push(Effect::CloseCamera);
            }
            if let Some(e) = self.awake_hold_effect(self.state) {
                fx.push(e);
            }
            return (self.state, fx);
        }

        if self.state == State::Active {
            self.state = State::Watching;
            self.miss_streak_start = None;
            self.face_run = 0;
            self.probation = None;
            fx.push(Effect::OpenCamera);
            fx.push(Effect::SetSampleInterval(self.cfg.sample_interval));
            if let Some(e) = self.awake_hold_effect(self.state) {
                fx.push(e);
            }
            return (self.state, fx);
        }

        // Spec §4.7 — three consecutive Unknowns mean we can no longer tell.
        // Leaving the screen dark would be the harmful outcome.
        // Ruling F4: this must sit below the Active early-return above. A
        // closed camera (the common case, in Active) reports Unknown by
        // design, and counting those ticks would degrade every run by its
        // third tick. Placing it here means unknown_run only accumulates
        // while the camera is genuinely open (Watching/Dimmed/Away/Deep).
        if matches!(face, FaceResult::Unknown) {
            self.unknown_run = self.unknown_run.saturating_add(1);
            if self.unknown_run >= 3 {
                self.state = State::Degraded;
                // Spec §4.7 — "the camera is retried every 5 minutes". The retry
                // clock measures time since VISOR gave up, so it starts here, at
                // the tick that enters Degraded and closes the camera.
                self.degraded_since = Some(now);
                self.probation = None;
                self.miss_streak_start = None;
                let mut fx = vec![Effect::SetDisplay(DisplayLevel::Full), Effect::CloseCamera];
                if let Some(e) = self.awake_hold_effect(self.state) {
                    fx.push(e);
                }
                return (self.state, fx);
            }
        } else {
            self.unknown_run = 0;
        }

        self.update_streak(face, now);

        // Spec §4.6 — a camera-triggered restore is on probation until a
        // second hit confirms it.
        if let Some(p) = self.probation {
            let confirmed = self.face_run >= self.cfg.face_confirm;
            if confirmed {
                self.probation = None;
            } else if now.saturating_duration_since(p.since) >= self.cfg.wake_probation {
                self.probation = None;
                self.state = p.fallback;
                let mut fx = self.effects_for_rung(p.fallback);
                if let Some(e) = self.awake_hold_effect(self.state) {
                    fx.push(e);
                }
                return (self.state, fx);
            } else {
                if let Some(e) = self.awake_hold_effect(self.state) {
                    fx.push(e);
                }
                return (self.state, fx);
            }
        }

        let reduced = matches!(self.state, State::Dimmed | State::Away | State::Deep);
        let waking =
            matches!(face, FaceResult::Face { .. }) && self.face_run >= self.cfg.wake_confirm;

        // Spec §4.5 — restore to Full in a single step from any rung.
        if reduced && waking {
            self.probation = Some(Probation {
                since: now,
                fallback: self.state,
            });
            self.state = State::Watching;
            fx.push(Effect::SetDisplay(DisplayLevel::Full));
            fx.push(Effect::SetSampleInterval(self.cfg.sample_interval));
            if let Some(e) = self.awake_hold_effect(self.state) {
                fx.push(e);
            }
            return (self.state, fx);
        }

        // Ladder fall-through — downward-only, per ruling F3 (Task 3). Do NOT
        // replace this with `if rung != self.state`.
        let rung = self.rung_for(self.streak_for(now));
        let deeper = match (Self::rung_ordinal(self.state), Self::rung_ordinal(rung)) {
            (Some(current), Some(next)) => next > current,
            _ => false,
        };
        if deeper {
            self.state = rung;
            fx.extend(self.effects_for_rung(rung));
        }
        if let Some(e) = self.awake_hold_effect(self.state) {
            fx.push(e);
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

    /// Drive the machine down to a given rung and return the base instant.
    fn into_rung(m: &mut Machine, rung: State) -> Instant {
        let t0 = into_watching(m);
        let secs: u64 = match rung {
            State::Dimmed => 21,
            State::Away => 46,
            State::Deep => 901,
            other => panic!("not a rung: {other:?}"),
        };
        m.step(
            Duration::from_secs(31),
            FaceResult::NoFace,
            t0 + Duration::from_secs(1),
        );
        m.step(
            Duration::from_secs(31 + secs),
            FaceResult::NoFace,
            t0 + Duration::from_secs(secs),
        );
        assert_eq!(m.state(), rung);
        t0
    }

    #[test]
    fn one_face_hit_restores_to_full_from_every_rung() {
        for rung in [State::Dimmed, State::Away, State::Deep] {
            let mut m = machine();
            let t0 = into_rung(&mut m, rung);
            let (s, fx) = m.step(
                Duration::from_secs(999),
                PRESENT,
                t0 + Duration::from_secs(902),
            );
            assert_eq!(s, State::Watching, "restored from {rung:?}");
            assert_eq!(
                fx,
                vec![
                    Effect::SetDisplay(DisplayLevel::Full),
                    Effect::SetSampleInterval(Duration::from_secs(2)),
                ],
                "restored from {rung:?} in one step"
            );
        }
    }

    #[test]
    fn probation_expires_back_to_the_rung_it_came_from() {
        let mut m = machine();
        let t0 = into_rung(&mut m, State::Away);

        // camera-triggered restore
        let (s, _) = m.step(
            Duration::from_secs(999),
            PRESENT,
            t0 + Duration::from_secs(47),
        );
        assert_eq!(s, State::Watching);

        // 10s later with nothing confirming it — back to Away
        let (s, fx) = m.step(
            Duration::from_secs(999),
            FaceResult::NoFace,
            t0 + Duration::from_secs(58),
        );
        assert_eq!(s, State::Away);
        assert_eq!(
            fx,
            vec![
                Effect::SetDisplay(DisplayLevel::Black),
                Effect::SetSampleInterval(Duration::from_secs(1)),
            ]
        );
    }

    #[test]
    fn a_second_hit_confirms_probation() {
        let mut m = machine();
        let t0 = into_rung(&mut m, State::Away);

        m.step(
            Duration::from_secs(999),
            PRESENT,
            t0 + Duration::from_secs(47),
        );
        let (s, fx) = m.step(
            Duration::from_secs(999),
            PRESENT,
            t0 + Duration::from_secs(49),
        );
        assert_eq!(s, State::Watching);
        assert!(fx.is_empty());

        // well past wake_probation — still awake, because it was confirmed
        let (s, _) = m.step(
            Duration::from_secs(999),
            PRESENT,
            t0 + Duration::from_secs(70),
        );
        assert_eq!(s, State::Watching);
    }

    #[test]
    fn probation_preserves_the_miss_streak() {
        let mut m = machine();
        let t0 = into_rung(&mut m, State::Away);

        // spurious wake at +47s, expires at +58s
        m.step(
            Duration::from_secs(999),
            PRESENT,
            t0 + Duration::from_secs(47),
        );
        let (s, _) = m.step(
            Duration::from_secs(999),
            FaceResult::NoFace,
            t0 + Duration::from_secs(58),
        );
        assert_eq!(s, State::Away);

        // The original streak began at t0+1. deep_after is 15m, so Deep is due
        // at t0+901 — NOT 15m after the spurious wake.
        let (s, _) = m.step(
            Duration::from_secs(999),
            FaceResult::NoFace,
            t0 + Duration::from_secs(902),
        );
        assert_eq!(s, State::Deep, "streak resumed rather than restarting");
    }

    /// Carried over from the Task 3 review: no existing test distinguishes
    /// "2 consecutive hits" from "2 hits ever". Deleting `self.face_run = 0;`
    /// from update_streak's NoFace arm currently survives the whole suite.
    #[test]
    fn non_consecutive_face_hits_do_not_clear_the_streak() {
        let mut m = machine();
        let t0 = into_watching(&mut m);

        // Face, NoFace, Face — two hits in total, but never two in a row.
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
            FaceResult::NoFace,
            t0 + Duration::from_secs(5),
        );
        m.step(
            Duration::from_secs(37),
            PRESENT,
            t0 + Duration::from_secs(7),
        );
        let (s, _) = m.step(
            Duration::from_secs(51),
            FaceResult::NoFace,
            t0 + Duration::from_secs(21),
        );
        assert_eq!(
            s,
            State::Dimmed,
            "face_confirm is consecutive, not cumulative"
        );
    }

    #[test]
    fn input_triggered_restore_never_enters_probation() {
        let mut m = machine();
        let t0 = into_rung(&mut m, State::Away);

        let (s, fx) = m.step(
            Duration::ZERO,
            FaceResult::NoFace,
            t0 + Duration::from_secs(47),
        );
        assert_eq!(s, State::Active);
        assert_eq!(
            fx,
            vec![Effect::SetDisplay(DisplayLevel::Full), Effect::CloseCamera]
        );

        // long past wake_probation, still Active — no probation to expire
        let (s, _) = m.step(
            Duration::ZERO,
            FaceResult::NoFace,
            t0 + Duration::from_secs(120),
        );
        assert_eq!(s, State::Active);
    }

    use crate::core::types::Command;

    #[test]
    fn three_consecutive_unknowns_degrade_and_restore_the_display() {
        let mut m = machine();
        let t0 = into_rung(&mut m, State::Deep);

        m.step(
            Duration::from_secs(999),
            FaceResult::Unknown,
            t0 + Duration::from_secs(902),
        );
        m.step(
            Duration::from_secs(999),
            FaceResult::Unknown,
            t0 + Duration::from_secs(903),
        );
        let (s, fx) = m.step(
            Duration::from_secs(999),
            FaceResult::Unknown,
            t0 + Duration::from_secs(904),
        );

        assert_eq!(s, State::Degraded);
        assert_eq!(
            fx,
            vec![Effect::SetDisplay(DisplayLevel::Full), Effect::CloseCamera]
        );
    }

    #[test]
    fn pausing_releases_the_awake_hold() {
        let mut m = machine();
        let t0 = Instant::now();
        m.set_awake_hold(true);

        // Entering Watching asserts the hold.
        let (_, fx) = m.step(Duration::from_secs(30), FaceResult::Unknown, t0);
        assert!(fx.contains(&Effect::SetAwakeHold(true)));

        // Pause must release it. `step` returns early while Paused, so if the
        // command does not emit this, nothing ever will and the system stays
        // awake for the whole pause.
        let (s, fx) = m.command(Command::Pause, t0 + Duration::from_secs(5));
        assert_eq!(s, State::Paused);
        assert!(
            fx.contains(&Effect::SetAwakeHold(false)),
            "pause must release the awake hold, got {fx:?}"
        );

        let (s, fx) = m.command(Command::Resume, t0 + Duration::from_secs(6));
        assert_eq!(s, State::Active);
        assert!(
            fx.contains(&Effect::SetAwakeHold(true)),
            "resume re-asserts it, got {fx:?}"
        );
    }

    #[test]
    fn a_successful_probe_run_recovers_from_degraded() {
        let mut m = machine();
        let t0 = into_rung(&mut m, State::Away);
        for i in 0..3 {
            m.step(
                Duration::from_secs(999),
                FaceResult::Unknown,
                t0 + Duration::from_secs(902 + i),
            );
        }
        assert_eq!(m.state(), State::Degraded);

        // retry_after elapses and the camera works again
        let (s, fx) = m.step(
            Duration::from_secs(999),
            PRESENT,
            t0 + Duration::from_secs(904 + 5 * 60),
        );
        assert_eq!(s, State::Active);
        assert!(
            fx.is_empty(),
            "Active is already Full with the camera closed"
        );
    }

    #[test]
    fn degraded_ignores_faces_until_the_retry_window_elapses() {
        let mut m = machine();
        let t0 = into_rung(&mut m, State::Away);
        for i in 0..3 {
            m.step(
                Duration::from_secs(999),
                FaceResult::Unknown,
                t0 + Duration::from_secs(902 + i),
            );
        }
        let (s, _) = m.step(
            Duration::from_secs(999),
            PRESENT,
            t0 + Duration::from_secs(910),
        );
        assert_eq!(s, State::Degraded);
    }

    #[test]
    fn pause_restores_the_display_and_resume_returns_to_active() {
        let mut m = machine();
        let t0 = into_rung(&mut m, State::Away);

        let (s, fx) = m.command(Command::Pause, t0 + Duration::from_secs(50));
        assert_eq!(s, State::Paused);
        assert_eq!(
            fx,
            vec![Effect::SetDisplay(DisplayLevel::Full), Effect::CloseCamera]
        );

        // steps do nothing while paused
        let (s, fx) = m.step(
            Duration::from_secs(999),
            FaceResult::NoFace,
            t0 + Duration::from_secs(999),
        );
        assert_eq!(s, State::Paused);
        assert!(fx.is_empty());

        let (s, fx) = m.command(Command::Resume, t0 + Duration::from_secs(1000));
        assert_eq!(s, State::Active);
        assert!(fx.is_empty());
    }

    #[test]
    fn a_closed_camera_reporting_unknown_never_degrades() {
        let mut m = machine();
        let t0 = Instant::now();
        // Active, camera closed, engine feeds Unknown every tick for a long while.
        for i in 0..10 {
            let (s, fx) = m.step(
                Duration::from_secs(1),
                FaceResult::Unknown,
                t0 + Duration::from_secs(i),
            );
            assert_eq!(s, State::Active, "tick {i} must stay Active");
            assert!(fx.is_empty(), "tick {i} must produce no effects");
        }
    }

    #[test]
    fn awake_hold_is_emitted_only_when_enabled() {
        let mut m = machine();
        let t0 = Instant::now();
        m.set_awake_hold(true);

        // Active -> Watching. Camera opens; hold is asserted.
        let (s, fx) = m.step(Duration::from_secs(30), FaceResult::Unknown, t0);
        assert_eq!(s, State::Watching);
        assert!(
            fx.contains(&Effect::SetAwakeHold(true)),
            "hold asserted on entering Watching"
        );

        // First NoFace after opening: this is the tick that STARTS the miss streak.
        let (s, fx) = m.step(
            Duration::from_secs(999),
            FaceResult::NoFace,
            t0 + Duration::from_secs(2),
        );
        assert_eq!(
            s,
            State::Watching,
            "streak has just started, nothing steps down yet"
        );
        assert!(
            !fx.iter().any(|e| matches!(e, Effect::SetAwakeHold(_))),
            "hold value is unchanged, so nothing is re-emitted"
        );

        // dim_after (20s) has now elapsed since the streak started at t0+2.
        let (s, fx) = m.step(
            Duration::from_secs(999),
            FaceResult::NoFace,
            t0 + Duration::from_secs(23),
        );
        assert_eq!(s, State::Dimmed);
        assert!(
            fx.contains(&Effect::SetAwakeHold(false)),
            "hold released on stepping down"
        );
    }
}
