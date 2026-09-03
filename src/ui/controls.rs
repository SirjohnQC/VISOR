//! The arithmetic behind every draggable thing in the tuning window.
//!
//! Pure: no window, no Direct2D, no mouse. The interactive half — capture,
//! hover, keyboard — lives in `ui::window`; everything that decides *what
//! value a pixel means* lives here, so it can be tested without a screen.
//!
//! One primitive serves all six controls the window edits. The gauge threshold
//! runs on an inverted linear axis, `dim_level` on a plain one, and the four
//! sequence markers share a single logarithmic axis. Writing three separate
//! sliders would have meant three separate places for an off-by-one.

/// How a value maps onto the unit interval.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Scale {
    Linear {
        min: f32,
        max: f32,
    },
    /// `t(p) = t0 · (t1/t0)^p`.
    ///
    /// Used for the sequence rail, where the range runs from 5 seconds to an
    /// hour. A linear axis there is useless at both ends: it would give
    /// sub-second precision at 15 minutes and none at all at 20 seconds. A log
    /// axis gives constant *relative* precision instead — one pixel is the
    /// same percentage of the value everywhere on the rail, which is what
    /// anyone actually wants when setting a timeout.
    Log {
        t0: f32,
        t1: f32,
    },
}

impl Scale {
    /// Value → unit position, clamped to 0..=1.
    pub fn to_unit(self, value: f32) -> f32 {
        match self {
            Scale::Linear { min, max } if max > min => {
                ((value - min) / (max - min)).clamp(0.0, 1.0)
            }
            Scale::Log { t0, t1 } if t0 > 0.0 && t1 > t0 => {
                if value <= t0 {
                    0.0
                } else {
                    ((value / t0).ln() / (t1 / t0).ln()).clamp(0.0, 1.0)
                }
            }
            _ => 0.0,
        }
    }

    /// Unit position → value.
    pub fn from_unit(self, p: f32) -> f32 {
        let p = p.clamp(0.0, 1.0);
        match self {
            Scale::Linear { min, max } => min + (max - min) * p,
            Scale::Log { t0, t1 } => t0 * (t1 / t0).powf(p),
        }
    }
}

/// A scale laid onto a run of pixels.
///
/// `lo` is the pixel at unit 0 and `hi` the pixel at unit 1, so a vertical
/// gauge that grows upward is expressed by giving `lo` the *larger* pixel
/// coordinate. No separate "inverted" flag: the direction is already in the
/// numbers, and a flag would be a second thing to get wrong.
#[derive(Debug, Clone, Copy)]
pub struct Axis {
    pub lo: f32,
    pub hi: f32,
    pub scale: Scale,
}

impl Axis {
    pub fn pixel_of(&self, value: f32) -> f32 {
        self.lo + (self.hi - self.lo) * self.scale.to_unit(value)
    }

    pub fn value_at(&self, pixel: f32) -> f32 {
        let span = self.hi - self.lo;
        if span.abs() < f32::EPSILON {
            return self.scale.from_unit(0.0);
        }
        self.scale.from_unit((pixel - self.lo) / span)
    }

    /// Pixels per unit of value at `value`, used to size a magnet in value
    /// space. Approximated over one pixel, which is exact enough for a snap
    /// and avoids differentiating the log scale by hand.
    pub fn value_per_pixel(&self, value: f32) -> f32 {
        let px = self.pixel_of(value);
        (self.value_at(px + 1.0) - self.value_at(px)).abs()
    }
}

/// Pull `value` to the nearest candidate if it is within `magnet_px` pixels.
///
/// The magnet is measured in **pixels, not value**, which matters on the log
/// axis: 7px near "10s" is a couple of seconds and 7px near "30m" is minutes.
/// A magnet defined in value units would be unusable at one end or the other.
pub fn snap(value: f32, candidates: &[f32], axis: &Axis, magnet_px: f32) -> f32 {
    let here = axis.pixel_of(value);
    let mut best: Option<(f32, f32)> = None;
    for &c in candidates {
        let d = (axis.pixel_of(c) - here).abs();
        if d <= magnet_px && best.is_none_or(|(bd, _)| d < bd) {
            best = Some((d, c));
        }
    }
    best.map_or(value, |(_, c)| c)
}

/// The sequence rail's snap set, in seconds.
pub const TIME_SNAPS: [f32; 16] = [
    5.0, 10.0, 15.0, 20.0, 30.0, 45.0, 60.0, 120.0, 180.0, 300.0, 600.0, 900.0, 1200.0, 1800.0,
    2700.0, 3600.0,
];

/// Round a free-dragged rail value to a step a human would have typed.
///
/// The rail is continuous and `Duration::from_secs_f32` is faithful, so a drag
/// that stops between two snaps writes its float straight into the config file:
/// `away_after = "49s 201ms 599us 121ns"` is a real line this produced. Nobody
/// meant 599 microseconds; it is the pixel the mouse happened to be on.
///
/// The two steps match the axis rather than taste. Below two minutes one pixel
/// is well under a second, so whole seconds are the finest thing worth keeping.
/// Above it one pixel is already ~17s, so 15s steps throw away no precision the
/// user could have expressed with the mouse in the first place.
///
/// Every entry in `TIME_SNAPS` is a fixed point of this function, so the magnet
/// and the quantiser can never disagree about where a drag landed.
pub fn quantise_seconds(secs: f32) -> f32 {
    if secs < 120.0 {
        secs.round()
    } else {
        (secs / 15.0).round() * 15.0
    }
}

/// The step `quantise_seconds` rounds to at `secs`.
fn step_at(secs: f32) -> f32 {
    if secs < 120.0 { 1.0 } else { 15.0 }
}

/// Quantise the three ladder values together, keeping them strictly ordered.
///
/// Quantising them independently can collide. The drag clamp guarantees only
/// `LADDER_GAP` (5s) between neighbours, and above two minutes the step is 15s,
/// so `away = 925` and `deep = 930` both land on 930. `Config::validate` then
/// refuses the write and the drag silently fails to save -- while the window
/// goes on showing the value the user picked, so nothing on screen says the
/// setting did not take.
///
/// Collisions are resolved by pushing the later marker up one step. That is a
/// push, which dragging deliberately does not do; the difference is that here
/// the alternative is not a surprising second change but no change at all.
pub fn quantise_ladder(dim: f32, away: f32, deep: f32) -> (f32, f32, f32) {
    let dim = quantise_seconds(dim);
    let mut away = quantise_seconds(away);
    if away <= dim {
        away = dim + step_at(dim);
    }
    let mut deep = quantise_seconds(deep);
    if deep <= away {
        deep = away + step_at(away);
    }
    (dim, away, deep)
}

/// Keep a value at least `gap` away from a neighbour, on the given side.
///
/// The three ladder thresholds must stay ordered. They **hard-clamp** rather
/// than pushing each other along: dragging `dim` up against `black` stops it
/// dead instead of shoving `black` further out. Cascading felt clever in the
/// sketch and is surprising in the hand — you go to adjust one number and
/// silently change two.
pub fn clamp_below(value: f32, ceiling: f32, gap: f32) -> f32 {
    value.min(ceiling - gap).max(0.0)
}

pub fn clamp_above(value: f32, floor: f32, gap: f32) -> f32 {
    value.max(floor + gap)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RAIL: Scale = Scale::Log {
        t0: 5.0,
        t1: 3600.0,
    };

    fn rail_axis() -> Axis {
        Axis {
            lo: 29.0,
            hi: 391.0,
            scale: RAIL,
        }
    }

    #[test]
    fn a_linear_scale_round_trips() {
        let s = Scale::Linear {
            min: 1.0,
            max: 99.0,
        };
        for v in [1.0f32, 20.0, 50.0, 99.0] {
            assert!((s.from_unit(s.to_unit(v)) - v).abs() < 0.01, "{v}");
        }
    }

    #[test]
    fn the_log_scale_spans_five_seconds_to_an_hour() {
        assert!((RAIL.from_unit(0.0) - 5.0).abs() < 0.001);
        assert!((RAIL.from_unit(1.0) - 3600.0).abs() < 0.1);
        // Geometric midpoint, not arithmetic: sqrt(5 * 3600) = 134.2s.
        let mid = RAIL.from_unit(0.5);
        assert!((mid - 134.16).abs() < 0.5, "midpoint was {mid}");
    }

    #[test]
    fn the_log_axis_gives_constant_relative_precision() {
        // The reason for a log axis at all: one pixel should be the same
        // PERCENTAGE of the value everywhere, so 20s is adjustable in seconds
        // and 15m is not adjustable in seconds.
        let a = rail_axis();
        let pct = |v: f32| a.value_per_pixel(v) / v;
        let near = pct(10.0);
        let far = pct(900.0);
        assert!(
            (near - far).abs() / near < 0.05,
            "relative precision drifted: {near} vs {far}"
        );
        // And it is the ~1.8% the design predicted.
        assert!((0.01..0.03).contains(&near), "got {near}");
    }

    #[test]
    fn the_defaults_land_where_the_design_says() {
        let a = rail_axis();
        // idle_grace 30s, then dim at +20s, black at +45s, off at +15m,
        // all measured from the last keypress.
        for (secs, expect) in [(30.0, 128.0), (50.0, 156.0), (75.0, 178.0), (930.0, 316.0)] {
            let px = a.pixel_of(secs);
            assert!(
                (px - expect).abs() < 2.0,
                "{secs}s should sit near x{expect}, got {px}"
            );
        }
    }

    #[test]
    fn a_vertical_axis_needs_no_inverted_flag() {
        // The gauge grows upward: unit 0 is the BOTTOM pixel. Expressing that
        // by ordering lo/hi means there is no second place to get it wrong.
        let g = Axis {
            lo: 348.0,
            hi: 108.0,
            scale: Scale::Linear {
                min: 0.0,
                max: 0.60,
            },
        };
        assert!((g.pixel_of(0.0) - 348.0).abs() < 0.01, "zero at the bottom");
        assert!((g.pixel_of(0.60) - 108.0).abs() < 0.01, "max at the top");
        assert!((g.value_at(228.0) - 0.30).abs() < 0.01, "halfway");
        // Dragging DOWN must lower the value.
        assert!(g.value_at(300.0) < g.value_at(200.0));
    }

    #[test]
    fn a_value_outside_the_axis_is_clamped_not_extrapolated() {
        let a = rail_axis();
        assert!((a.pixel_of(1.0) - 29.0).abs() < 0.01, "below t0 pins to lo");
        assert!(
            (a.pixel_of(99999.0) - 391.0).abs() < 0.01,
            "above t1 pins to hi"
        );
        // And a pixel off the end yields the end value, never a negative time.
        assert!(a.value_at(-100.0) >= 5.0);
        assert!(a.value_at(9999.0) <= 3600.1);
    }

    #[test]
    fn the_magnet_is_measured_in_pixels_so_it_works_at_both_ends() {
        let a = rail_axis();
        // Near the fast end, 21s is a few pixels from the 20s stop.
        assert_eq!(snap(21.0, &TIME_SNAPS, &a, 7.0), 20.0);
        // At the slow end, 16 minutes is likewise a few pixels from 15.
        assert_eq!(snap(960.0, &TIME_SNAPS, &a, 7.0), 900.0);
        // A magnet defined in seconds could not do both: 60s of slack would
        // swallow every stop below a minute.
        assert!(
            (snap(960.0, &TIME_SNAPS, &a, 7.0) - 960.0).abs() > 30.0,
            "the slow-end snap must actually move the value"
        );
    }

    #[test]
    fn a_value_far_from_every_stop_is_left_alone() {
        // Free positioning has to remain possible, or the snaps become the
        // only settings the rail can express.
        let a = rail_axis();
        assert_eq!(snap(37.0, &TIME_SNAPS, &a, 2.0), 37.0);
    }

    #[test]
    fn the_ladder_thresholds_clamp_rather_than_push() {
        // Dragging dim up against black stops it dead. Cascading would mean
        // going to adjust one number and silently changing two.
        assert_eq!(clamp_below(50.0, 45.0, 5.0), 40.0);
        assert_eq!(clamp_below(20.0, 45.0, 5.0), 20.0, "clear of it, untouched");
        assert_eq!(clamp_above(10.0, 45.0, 5.0), 50.0);
        assert_eq!(clamp_above(90.0, 45.0, 5.0), 90.0);
        // And nothing can be dragged negative.
        assert_eq!(clamp_below(1.0, 3.0, 5.0), 0.0);
    }

    #[test]
    fn a_dragged_value_quantises_to_a_step_a_human_would_type() {
        // The rail hands back a float and `Duration::from_secs_f32` writes it
        // to TOML verbatim, so without this one drag leaves
        // `away_after = "49s 201ms 599us 121ns"` in a file the user reads.
        assert_eq!(quantise_seconds(49.2016), 49.0);
        assert_eq!(quantise_seconds(20.4), 20.0);
        // Above two minutes, 15s steps: nobody sets a 15-minute timeout to the
        // second, and one pixel of the log rail is ~17s out there anyway.
        assert_eq!(quantise_seconds(937.0), 930.0);
        assert_eq!(quantise_seconds(930.0), 930.0);
        // Every result must be a whole number of seconds.
        for v in [5.3f32, 49.2016, 119.7, 121.4, 933.9, 3599.6] {
            assert_eq!(quantise_seconds(v).fract(), 0.0, "{v} left a fraction");
        }
        // And it must never nudge a snapped value off its stop, or the magnet
        // and the quantiser would fight over every drag that lands on one.
        for s in TIME_SNAPS {
            assert_eq!(quantise_seconds(s), s, "snap {s} moved");
        }
    }

    #[test]
    fn quantising_the_ladder_cannot_collide_two_markers() {
        // The drag clamp only guarantees LADDER_GAP (5s) between neighbours,
        // but above two minutes the quantiser steps by 15s -- so 925 and 930
        // both land on 930, `Config::validate` refuses the write, and the drag
        // silently fails to save while the window goes on showing what the
        // user chose. A save that quietly does nothing is the worst outcome
        // available here, so the quantiser has to keep the order it inherited.
        let (dim, away, deep) = quantise_ladder(20.0, 925.0, 930.0);
        assert!(dim < away && away < deep, "got {dim} {away} {deep}");
        // The same collision exists below two minutes at 1s steps.
        let (dim, away, deep) = quantise_ladder(20.2, 20.4, 26.0);
        assert!(dim < away && away < deep, "got {dim} {away} {deep}");
        // A ladder already legal and on-step passes through untouched.
        assert_eq!(quantise_ladder(20.0, 45.0, 900.0), (20.0, 45.0, 900.0));
    }

    #[test]
    fn a_degenerate_axis_does_not_divide_by_zero() {
        let a = Axis {
            lo: 100.0,
            hi: 100.0,
            scale: Scale::Linear { min: 0.0, max: 1.0 },
        };
        let v = a.value_at(100.0);
        assert!(v.is_finite(), "got {v}");
    }
}
