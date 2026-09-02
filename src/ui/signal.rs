//! What the tuning window is measuring, and whether the user's line is safe.
//!
//! Pure, like `core`: no hardware, no clock of its own — `now` is passed in,
//! the way `Machine::step` takes it. Everything the gauge draws and every
//! sentence it prints is decided here, so it can all be tested without a
//! webcam or a window.
//!
//! The idea the whole module turns on: the real failure is **not** "my face is
//! too small". It is "my face was briefly too small when I leaned back to
//! think, and VISOR blanked the screen at me." So the number that matters is
//! not the instantaneous ratio, it is the *lowest* ratio over the last few
//! seconds — the envelope minimum. Everything here judges against that.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// How far back the envelope looks. Long enough to catch a lean-back, short
/// enough that the user sees it respond while they move.
pub const ENVELOPE_WINDOW: Duration = Duration::from_secs(10);

/// How long a threshold must hold up before it is called confirmed.
pub const CONFIRM_WINDOW: Duration = Duration::from_secs(10);

/// Headroom over the threshold below which a setting is only "marginal".
/// 15% is about one lean-back in a normal chair.
pub const MARGIN: f32 = 1.15;

/// Smoothing factor for the drawn box and the displayed number.
pub const SMOOTH_ALPHA: f32 = 0.25;

/// What the camera can tell us right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CameraStatus {
    /// Open and producing frames.
    Live,
    /// Deliberately shut (the user is present, or preview is off).
    Closed,
    /// Tried and failed: covered, unplugged, in use, or blocked.
    Unavailable,
}

/// The five states the gauge, the plate and the verdict all key off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalState {
    /// Clears the line with room to spare.
    Good,
    /// Clears the line, but by less than `MARGIN`.
    Marginal,
    /// Measured, and under the line. The failure this window exists for.
    Below,
    /// Camera is working and saw nobody.
    NoFace,
    /// Nothing to measure.
    Unavailable,
}

/// A rolling min/max over the last [`ENVELOPE_WINDOW`] of **raw** readings.
///
/// Deliberately raw rather than smoothed: smoothing exists to stop the drawn
/// number twitching, but the envelope's whole job is to catch the dips that
/// smoothing would hide.
#[derive(Debug, Default)]
pub struct Envelope {
    samples: VecDeque<(Instant, f32)>,
}

impl Envelope {
    pub fn new() -> Self {
        Self {
            samples: VecDeque::new(),
        }
    }

    pub fn push(&mut self, now: Instant, ratio: f32) {
        self.samples.push_back((now, ratio));
        self.expire(now);
    }

    /// Drop anything older than the window. Called on push, and separately by
    /// the window on a tick so a stalled camera does not leave a stale
    /// envelope propping up a "confirmed" verdict.
    pub fn expire(&mut self, now: Instant) {
        while let Some(&(t, _)) = self.samples.front() {
            if now.duration_since(t) > ENVELOPE_WINDOW {
                self.samples.pop_front();
            } else {
                break;
            }
        }
    }

    pub fn clear(&mut self) {
        self.samples.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    pub fn min(&self) -> Option<f32> {
        self.samples
            .iter()
            .map(|&(_, r)| r)
            .fold(None, |a, r| Some(a.map_or(r, |a: f32| a.min(r))))
    }

    pub fn max(&self) -> Option<f32> {
        self.samples
            .iter()
            .map(|&(_, r)| r)
            .fold(None, |a, r| Some(a.map_or(r, |a: f32| a.max(r))))
    }
}

/// Decide the signal state.
///
/// Judged on the envelope minimum, not the instantaneous reading: a threshold
/// that only holds while the user sits perfectly still is not a threshold that
/// holds.
pub fn classify(camera: CameraStatus, envelope_min: Option<f32>, threshold: f32) -> SignalState {
    match camera {
        CameraStatus::Unavailable | CameraStatus::Closed => SignalState::Unavailable,
        CameraStatus::Live => match envelope_min {
            None => SignalState::NoFace,
            Some(low) if low < threshold => SignalState::Below,
            Some(low) if low < threshold * MARGIN => SignalState::Marginal,
            Some(_) => SignalState::Good,
        },
    }
}

/// The threshold to suggest given the best reading seen.
///
/// Delegates to the tray "Check camera" rule so the window and the tray can
/// never disagree about what to recommend.
pub fn suggested(best: f32) -> f32 {
    crate::core::check::suggested_ratio(best)
}

/// Exponential smoothing for the drawn box and the displayed number.
pub fn smooth(previous: Option<f32>, raw: f32, alpha: f32) -> f32 {
    match previous {
        None => raw,
        Some(p) => p + (raw - p) * alpha,
    }
}

/// Snap to hundredths, but only once the value has genuinely moved.
///
/// Without the dead band the readout sits on a rounding boundary and flickers
/// between 0.19 and 0.20 forever, which destroys the "calm instrument" premise
/// in about one second. The band is a full hundredth wide, so a value must
/// clear the boundary by half a step before the display follows it.
pub fn quantise(shown: Option<f32>, smoothed: f32) -> f32 {
    match shown {
        Some(s) if (smoothed - s).abs() < 0.01 => s,
        _ => (smoothed * 100.0).round() / 100.0,
    }
}

/// Tracks whether the current threshold has held up long enough to trust.
#[derive(Debug, Default)]
pub struct Confirmation {
    started: Option<Instant>,
}

impl Confirmation {
    pub fn new() -> Self {
        Self { started: None }
    }

    /// Begin (or restart) the window. Called when the threshold changes.
    pub fn restart(&mut self, now: Instant) {
        self.started = Some(now);
    }

    pub fn cancel(&mut self) {
        self.started = None;
    }

    /// Fold in this tick. Returns the progress 0.0..=1.0, or `None` when no
    /// confirmation is running.
    ///
    /// A dip below the threshold **restarts the clock**. That is the whole
    /// point: you cannot confirm a bad setting by holding still, because the
    /// thing being tested is precisely whether you can move without VISOR
    /// losing you.
    pub fn tick(&mut self, now: Instant, state: SignalState) -> Option<f32> {
        let started = self.started?;
        if matches!(state, SignalState::Below | SignalState::NoFace) {
            self.started = Some(now);
            return Some(0.0);
        }
        let elapsed = now.duration_since(started);
        Some((elapsed.as_secs_f32() / CONFIRM_WINDOW.as_secs_f32()).clamp(0.0, 1.0))
    }

    pub fn is_confirmed(&self, now: Instant) -> bool {
        self.started
            .is_some_and(|s| now.duration_since(s) >= CONFIRM_WINDOW)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn the_envelope_reports_the_lowest_and_highest_of_the_window() {
        let t = t0();
        let mut e = Envelope::new();
        for (i, r) in [0.30f32, 0.19, 0.26, 0.22].iter().enumerate() {
            e.push(t + Duration::from_secs(i as u64), *r);
        }
        assert_eq!(e.min(), Some(0.19));
        assert_eq!(e.max(), Some(0.30));
    }

    #[test]
    fn readings_older_than_the_window_stop_counting() {
        // Otherwise a dip from a minute ago would hold the verdict down
        // forever, and the user could never get to "confirmed" no matter how
        // well they were sitting now.
        let t = t0();
        let mut e = Envelope::new();
        e.push(t, 0.05);
        e.push(t + ENVELOPE_WINDOW + Duration::from_secs(1), 0.30);
        assert_eq!(e.min(), Some(0.30), "the old dip must have expired");
    }

    #[test]
    fn a_stalled_camera_does_not_leave_a_stale_envelope() {
        // No new samples arrive, but time passes: expiring on a tick rather
        // than only on push is what stops a dead camera propping up a
        // "confirmed" reading from ten seconds ago.
        let t = t0();
        let mut e = Envelope::new();
        e.push(t, 0.30);
        assert!(!e.is_empty());
        e.expire(t + ENVELOPE_WINDOW + Duration::from_secs(1));
        assert!(e.is_empty());
        assert_eq!(e.min(), None);
    }

    #[test]
    fn the_verdict_is_judged_on_the_dip_not_the_moment() {
        // Sitting at 0.30 right now means nothing if you were at 0.11 two
        // seconds ago -- that dip is exactly when VISOR would have blanked.
        assert_eq!(
            classify(CameraStatus::Live, Some(0.11), 0.15),
            SignalState::Below
        );
        assert_eq!(
            classify(CameraStatus::Live, Some(0.30), 0.15),
            SignalState::Good
        );
    }

    #[test]
    fn clearing_the_line_by_a_hair_is_marginal_not_good() {
        // 0.16 over a 0.15 line is 6% of headroom: one lean back and VISOR
        // thinks you left. Calling that "good" would be the window lying.
        assert_eq!(
            classify(CameraStatus::Live, Some(0.16), 0.15),
            SignalState::Marginal
        );
        // 15% clear is the boundary, and it counts as good.
        assert_eq!(
            classify(CameraStatus::Live, Some(0.15 * MARGIN), 0.15),
            SignalState::Good
        );
    }

    #[test]
    fn a_live_camera_seeing_nobody_is_not_the_same_as_no_camera() {
        assert_eq!(
            classify(CameraStatus::Live, None, 0.15),
            SignalState::NoFace
        );
        assert_eq!(
            classify(CameraStatus::Unavailable, None, 0.15),
            SignalState::Unavailable
        );
        assert_eq!(
            classify(CameraStatus::Closed, None, 0.15),
            SignalState::Unavailable
        );
    }

    #[test]
    fn the_readout_does_not_flicker_on_a_rounding_boundary() {
        // Sitting at 0.195 must not oscillate 0.19 / 0.20 forever.
        assert_eq!(quantise(Some(0.19), 0.195), 0.19);
        assert_eq!(quantise(Some(0.19), 0.198), 0.19);
        // But a genuine move is followed.
        assert_eq!(quantise(Some(0.19), 0.201), 0.20);
        assert_eq!(quantise(Some(0.19), 0.179), 0.18);
        // With nothing shown yet it simply snaps.
        assert_eq!(quantise(None, 0.194), 0.19);
    }

    #[test]
    fn smoothing_moves_toward_the_reading_without_jumping_to_it() {
        assert_eq!(
            smooth(None, 0.30, SMOOTH_ALPHA),
            0.30,
            "first reading lands"
        );
        let s = smooth(Some(0.20), 0.40, 0.25);
        assert!(s > 0.20 && s < 0.40, "moved toward but not to: {s}");
        assert!((s - 0.25).abs() < 1e-6);
    }

    #[test]
    fn a_dip_below_the_line_restarts_the_confirmation() {
        // The rule that makes confirmation mean something: you cannot sit
        // perfectly still for ten seconds and be told a bad threshold is fine.
        let t = t0();
        let mut c = Confirmation::new();
        c.restart(t);
        assert_eq!(
            c.tick(t + Duration::from_secs(8), SignalState::Good),
            Some(0.8)
        );

        let dip = t + Duration::from_secs(9);
        assert_eq!(c.tick(dip, SignalState::Below), Some(0.0), "clock resets");
        assert!(!c.is_confirmed(dip + Duration::from_secs(9)), "not yet");
        assert!(c.is_confirmed(dip + CONFIRM_WINDOW));
    }

    #[test]
    fn losing_the_face_also_restarts_the_confirmation() {
        // A face that vanishes mid-check is the same failure as one that goes
        // too small -- VISOR would have started its miss streak either way.
        let t = t0();
        let mut c = Confirmation::new();
        c.restart(t);
        assert_eq!(
            c.tick(t + Duration::from_secs(5), SignalState::NoFace),
            Some(0.0)
        );
        assert!(!c.is_confirmed(t + Duration::from_secs(9)));
    }

    #[test]
    fn nothing_is_confirmed_until_a_check_is_started() {
        let t = t0();
        let mut c = Confirmation::new();
        assert_eq!(c.tick(t, SignalState::Good), None);
        assert!(!c.is_confirmed(t + Duration::from_secs(60)));
    }

    #[test]
    fn the_window_suggests_what_the_tray_would() {
        // One rule, one helper. If these ever diverged, "Check camera" and the
        // tuning window would recommend different numbers for the same face.
        for best in [0.05f32, 0.11, 0.31] {
            assert_eq!(suggested(best), crate::core::check::suggested_ratio(best));
        }
    }
}
