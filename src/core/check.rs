//! Verdict logic for the "Check camera" tray action.
//!
//! Pure, like the rest of `core`: it takes the samples an adapter collected and
//! decides what to tell the user. No hardware, no clock, no I/O.
//!
//! This exists because every failure it reports is otherwise silent. A camera
//! that is covered, unplugged, pointed at the ceiling, or simply further from
//! the user than `min_face_ratio` allows all produce exactly the same
//! behaviour: VISOR dims the screen as if nobody were there, and the user has
//! no way to tell why.

use crate::core::types::FaceResult;

/// What a run of probe samples says about whether VISOR can see the user.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CameraVerdict {
    /// Every sample was `Unknown` — the camera could not be read at all.
    /// Covered, unplugged, in use by another application, or permission denied.
    Unavailable,
    /// The camera works, but no face was found in any sample.
    NoFaceSeen,
    /// A face was seen, but smaller than `min_face_ratio`, so the state machine
    /// downgrades it to `NoFace` and will dim on the user anyway.
    TooFar { best: f32, needed: f32 },
    /// A face was seen and it clears the threshold.
    Good { best: f32, needed: f32 },
}

impl CameraVerdict {
    /// True when VISOR would actually register the user as present.
    pub fn is_ok(self) -> bool {
        matches!(self, CameraVerdict::Good { .. })
    }

    /// A single line fit for the tray tooltip and the log.
    ///
    /// The `TooFar` arm suggests a concrete `min_face_ratio` rather than just
    /// reporting the mismatch — that number is the whole reason the user opened
    /// this check, and making them compute it themselves would be unkind.
    pub fn message(self) -> String {
        match self {
            CameraVerdict::Unavailable => {
                "Camera unavailable — covered, unplugged, in use by another app, \
                 or blocked by privacy settings. VISOR will not dim while it \
                 cannot see."
                    .to_string()
            }
            CameraVerdict::NoFaceSeen => {
                "Camera works, but no face was detected. Check that it is \
                 pointed at you and the room is lit."
                    .to_string()
            }
            CameraVerdict::TooFar { best, needed } => {
                let suggested = suggested_ratio(best);
                format!(
                    "Face seen but too small: {best:.3} of frame height, and \
                     min_face_ratio is {needed:.3}. VISOR will treat you as \
                     away. Sit closer, or set min_face_ratio = {suggested:.2}."
                )
            }
            CameraVerdict::Good { best, needed } => {
                format!(
                    "Camera sees you: {best:.3} of frame height, comfortably \
                     over min_face_ratio {needed:.3}."
                )
            }
        }
    }
}

/// A threshold with headroom below what was actually measured, so that normal
/// shifting in the chair does not drop the user under it. Floored so a wildly
/// small reading cannot suggest a value that would match noise.
pub fn suggested_ratio(best: f32) -> f32 {
    (best * 0.7).max(0.03)
}

/// Decide what a run of samples means.
///
/// `Unknown` samples are ignored rather than counted as absence — that is the
/// same fail-safe stance as spec §4.7 — but a run that is *entirely* `Unknown`
/// means the camera never produced a usable frame, which is itself the answer.
pub fn camera_verdict(samples: &[FaceResult], needed: f32) -> CameraVerdict {
    let mut usable = false;
    let mut best: Option<f32> = None;

    for s in samples {
        match *s {
            FaceResult::Unknown => {}
            FaceResult::NoFace => usable = true,
            FaceResult::Face { largest_ratio, .. } => {
                usable = true;
                best = Some(best.map_or(largest_ratio, |b: f32| b.max(largest_ratio)));
            }
        }
    }

    if !usable {
        return CameraVerdict::Unavailable;
    }
    match best {
        None => CameraVerdict::NoFaceSeen,
        Some(b) if b < needed => CameraVerdict::TooFar { best: b, needed },
        Some(b) => CameraVerdict::Good { best: b, needed },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NEEDED: f32 = 0.15;

    fn face(r: f32) -> FaceResult {
        FaceResult::Face {
            count: 1,
            largest_ratio: r,
        }
    }

    #[test]
    fn an_all_unknown_run_means_the_camera_is_unavailable() {
        let samples = [FaceResult::Unknown; 5];
        assert_eq!(camera_verdict(&samples, NEEDED), CameraVerdict::Unavailable);
        assert!(!camera_verdict(&samples, NEEDED).is_ok());
    }

    #[test]
    fn an_empty_run_is_unavailable_rather_than_absence() {
        // Nothing was sampled at all, which is a failure to check — not a
        // finding that the user is missing.
        assert_eq!(camera_verdict(&[], NEEDED), CameraVerdict::Unavailable);
    }

    #[test]
    fn a_working_camera_with_nobody_there_reports_no_face() {
        let samples = [FaceResult::NoFace, FaceResult::NoFace];
        assert_eq!(camera_verdict(&samples, NEEDED), CameraVerdict::NoFaceSeen);
    }

    #[test]
    fn a_single_unknown_does_not_hide_a_working_camera() {
        // Spec §4.7's stance: Unknown is not evidence of anything, so one bad
        // frame among good ones must not change the verdict.
        let samples = [FaceResult::Unknown, FaceResult::NoFace];
        assert_eq!(camera_verdict(&samples, NEEDED), CameraVerdict::NoFaceSeen);
    }

    #[test]
    fn a_face_below_the_threshold_is_reported_as_too_far() {
        let samples = [FaceResult::NoFace, face(0.09)];
        assert_eq!(
            camera_verdict(&samples, NEEDED),
            CameraVerdict::TooFar {
                best: 0.09,
                needed: NEEDED
            }
        );
    }

    #[test]
    fn the_best_sample_wins_not_the_last() {
        // The user may glance away mid-check; the best frame is the honest
        // answer to "can it see me when I am sitting normally".
        let samples = [face(0.30), FaceResult::NoFace, face(0.05)];
        assert_eq!(
            camera_verdict(&samples, NEEDED),
            CameraVerdict::Good {
                best: 0.30,
                needed: NEEDED
            }
        );
    }

    #[test]
    fn a_ratio_exactly_at_the_threshold_passes() {
        // The machine downgrades a face when ratio < min_face_ratio, so equal
        // must pass here too or the check would disagree with the behaviour.
        let samples = [face(NEEDED)];
        assert!(camera_verdict(&samples, NEEDED).is_ok());
    }

    #[test]
    fn the_too_far_message_suggests_a_usable_threshold() {
        let v = CameraVerdict::TooFar {
            best: 0.09,
            needed: NEEDED,
        };
        let m = v.message();
        // 0.09 * 0.7 = 0.063 -> rendered as 0.06, which is below what was
        // actually measured, so the user gets headroom rather than a value
        // they would sit exactly on.
        assert!(m.contains("0.06"), "should suggest a threshold: {m}");
        assert!(m.contains("0.090"), "should report what it saw: {m}");
    }

    #[test]
    fn the_suggested_threshold_is_always_below_what_was_measured() {
        for best in [0.05f32, 0.09, 0.16, 0.4, 0.9] {
            let s = suggested_ratio(best);
            assert!(s < best, "suggestion {s} must leave headroom under {best}");
            assert!(s >= 0.03, "suggestion {s} must not match noise");
        }
    }

    #[test]
    fn every_verdict_produces_a_non_empty_message() {
        for v in [
            CameraVerdict::Unavailable,
            CameraVerdict::NoFaceSeen,
            CameraVerdict::TooFar {
                best: 0.05,
                needed: NEEDED,
            },
            CameraVerdict::Good {
                best: 0.4,
                needed: NEEDED,
            },
        ] {
            assert!(!v.message().is_empty());
        }
    }
}
