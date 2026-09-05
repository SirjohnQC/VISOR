//! The settings page: the values that until now lived in TOML and nowhere else.
//!
//! Pure, like [`crate::ui::controls`] — no window, no Direct2D, no mouse. The
//! table below is the single source of both what gets drawn and what gets
//! clicked, because two copies of these numbers is how a control ends up drawn
//! in one place and clickable in another.
//!
//! ## Why a second page and not a taller window
//!
//! `docs/tuning-window-design.md` §4 fixes the window at 420×696 so no
//! scrollbar ever has to exist, and bans tabs, dropdowns and toggles. The ban
//! is load-bearing for page one — the instrument face — and this respects it:
//! page two is reached by one [C4 GhostButton], the window never resizes, and
//! the instrument gains no new chrome at all.
//!
//! ## Why every setting is a choice and none is a slider
//!
//! All eight have a small natural set of sensible values. Nobody wants
//! `sample_interval = 2.7s`; offering a continuous rail for it would be false
//! precision, and it would need six more axes. One control type, eight
//! instances, nothing hidden behind a collapsed menu.

use crate::config::Config;
use crate::ui::theme::Theme;
use std::time::Duration;

/// Content column, matching page one so the two pages share an edge.
pub const LEFT: f32 = 22.0;
pub const RIGHT: f32 = 398.0;
/// First usable row, immediately under the title bar's hairline.
pub const TOP: f32 = 52.0;
/// The footer hairline. Nothing may be drawn at or below it: there is no
/// scrollbar, so anything past here is simply unreachable.
pub const FOOTER_HAIRLINE: f32 = 654.0;

pub const SECTION_H: f32 = 16.0;
/// Row internals, as offsets from the row's own `y`.
pub const LABEL_H: f32 = 15.0;
pub const CAPTION_DY: f32 = 16.0;
pub const CAPTION_H: f32 = 15.0;
pub const SEG_DY: f32 = 34.0;
pub const SEG_H: f32 = 24.0;
pub const ROW_H: f32 = SEG_DY + SEG_H;
const SEG_GAP: f32 = 6.0;

/// Which mechanism the resolver is allowed to pick.
///
/// An enum rather than the config's `String` because the page can then only
/// produce a value `Config::validate` accepts — the four spellings live in one
/// place instead of being re-typed next to every comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    Auto,
    Ddc,
    Overlay,
    Broadcast,
}

impl Strategy {
    /// Unknown strings fall back to `Auto`, matching [`Theme::parse`]. A config
    /// carrying one never reaches here anyway — `Config::validate` refuses it
    /// at load and the whole file falls back to defaults, whose strategy *is*
    /// auto — so `Auto` is what is genuinely in effect.
    pub fn parse(s: &str) -> Strategy {
        match s.trim().to_ascii_lowercase().as_str() {
            "ddc" => Strategy::Ddc,
            "overlay" => Strategy::Overlay,
            "broadcast" => Strategy::Broadcast,
            _ => Strategy::Auto,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Strategy::Auto => "auto",
            Strategy::Ddc => "ddc",
            Strategy::Overlay => "overlay",
            Strategy::Broadcast => "broadcast",
        }
    }
}

/// One editable value on the settings page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    SampleInterval,
    AwaySample,
    FaceConfirm,
    WakeConfirm,
    WakeProbation,
    HoldAwake,
    Strategy,
    Theme,
}

/// The eight values, in the units their controls work in.
///
/// `Copy` for the same reason [`crate::ui::window::Editable`] is: the window
/// holds it in a `Cell`, and a status push arriving between a click and its
/// save must not be able to yank a value out from under it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Settings {
    pub sample_interval: f32,
    pub away_sample: f32,
    pub face_confirm: u8,
    pub wake_confirm: u8,
    pub wake_probation: f32,
    pub hold_awake: bool,
    pub strategy: Strategy,
    pub theme: Theme,
}

impl Settings {
    pub fn from_config(c: &Config) -> Settings {
        Settings {
            sample_interval: c.presence.sample_interval.as_secs_f32(),
            away_sample: c.presence.away_sample.as_secs_f32(),
            face_confirm: c.presence.face_confirm,
            wake_confirm: c.presence.wake_confirm,
            wake_probation: c.presence.wake_probation.as_secs_f32(),
            hold_awake: c.display.hold_awake_while_present,
            strategy: Strategy::parse(&c.display.strategy),
            theme: Theme::parse(&c.ui.theme),
        }
    }

    /// Write back only the eight fields this page owns, leaving everything the
    /// rail and the gauge edit exactly as it was.
    pub fn write_into(self, c: &mut Config) {
        c.presence.sample_interval = Duration::from_secs_f32(self.sample_interval);
        c.presence.away_sample = Duration::from_secs_f32(self.away_sample);
        c.presence.face_confirm = self.face_confirm;
        c.presence.wake_confirm = self.wake_confirm;
        c.presence.wake_probation = Duration::from_secs_f32(self.wake_probation);
        c.display.hold_awake_while_present = self.hold_awake;
        c.display.strategy = self.strategy.as_str().to_string();
        c.ui.theme = self.theme.as_str().to_string();
    }
}

/// The values behind each row's option labels. Kept beside the labels they
/// belong to so a fifth option can never be added to one and not the other.
const SAMPLE_SECS: [f32; 4] = [1.0, 2.0, 3.0, 5.0];
const AWAY_SECS: [f32; 4] = [1.0, 2.0, 5.0, 10.0];
const CONFIRMS: [u8; 4] = [1, 2, 3, 4];
const PROBATION_SECS: [f32; 4] = [5.0, 10.0, 20.0, 30.0];

/// Exact match, not nearest: a hand-edited value the page does not offer must
/// light nothing rather than claim its neighbour.
fn index_of_secs(list: &[f32], v: f32) -> Option<usize> {
    list.iter().position(|&c| (c - v).abs() < 0.05)
}

impl Setting {
    /// Which option is in effect, or `None` when the config holds something
    /// this page does not offer.
    pub fn selected(self, s: &Settings) -> Option<usize> {
        match self {
            Setting::SampleInterval => index_of_secs(&SAMPLE_SECS, s.sample_interval),
            Setting::AwaySample => index_of_secs(&AWAY_SECS, s.away_sample),
            Setting::FaceConfirm => CONFIRMS.iter().position(|&c| c == s.face_confirm),
            Setting::WakeConfirm => CONFIRMS.iter().position(|&c| c == s.wake_confirm),
            Setting::WakeProbation => index_of_secs(&PROBATION_SECS, s.wake_probation),
            Setting::HoldAwake => Some(usize::from(s.hold_awake)),
            Setting::Strategy => Some(match s.strategy {
                Strategy::Auto => 0,
                Strategy::Ddc => 1,
                Strategy::Overlay => 2,
                Strategy::Broadcast => 3,
            }),
            Setting::Theme => Some(match s.theme {
                Theme::Dark => 0,
                Theme::Light => 1,
                Theme::Oled => 2,
            }),
        }
    }

    /// What the config actually holds, formatted the way the options are.
    ///
    /// Only ever shown when [`Setting::selected`] returns `None` — a value
    /// somebody typed into `config.toml` by hand. Lighting no segment without
    /// saying what *is* in force would make the row look broken.
    pub fn current(self, s: &Settings) -> String {
        let secs = |v: f32| {
            if (v - v.round()).abs() < 0.05 {
                format!("{}s", v.round() as i64)
            } else {
                format!("{v:.1}s")
            }
        };
        match self {
            Setting::SampleInterval => secs(s.sample_interval),
            Setting::AwaySample => secs(s.away_sample),
            Setting::WakeProbation => secs(s.wake_probation),
            Setting::FaceConfirm => s.face_confirm.to_string(),
            Setting::WakeConfirm => s.wake_confirm.to_string(),
            Setting::HoldAwake => (if s.hold_awake { "On" } else { "Off" }).to_string(),
            Setting::Strategy => s.strategy.as_str().to_string(),
            Setting::Theme => s.theme.as_str().to_string(),
        }
    }

    /// An index the row does not have is ignored rather than clamped: a click
    /// can only ever produce one that exists, so clamping would hide a bug
    /// instead of letting a test catch it.
    pub fn apply(self, s: &mut Settings, i: usize) {
        match self {
            Setting::SampleInterval => {
                if let Some(&v) = SAMPLE_SECS.get(i) {
                    s.sample_interval = v;
                }
            }
            Setting::AwaySample => {
                if let Some(&v) = AWAY_SECS.get(i) {
                    s.away_sample = v;
                }
            }
            Setting::FaceConfirm => {
                if let Some(&v) = CONFIRMS.get(i) {
                    s.face_confirm = v;
                }
            }
            Setting::WakeConfirm => {
                if let Some(&v) = CONFIRMS.get(i) {
                    s.wake_confirm = v;
                }
            }
            Setting::WakeProbation => {
                if let Some(&v) = PROBATION_SECS.get(i) {
                    s.wake_probation = v;
                }
            }
            Setting::HoldAwake => s.hold_awake = i == 1,
            Setting::Strategy => {
                s.strategy = match i {
                    1 => Strategy::Ddc,
                    2 => Strategy::Overlay,
                    3 => Strategy::Broadcast,
                    _ => Strategy::Auto,
                }
            }
            Setting::Theme => {
                s.theme = match i {
                    1 => Theme::Light,
                    2 => Theme::Oled,
                    _ => Theme::Dark,
                }
            }
        }
    }
}

/// One row of the page.
///
/// The caption is not a tooltip: spec §4 forbids hover explanations, so every
/// row carries its consequence permanently, in one line.
pub struct Row {
    pub setting: Setting,
    pub y: f32,
    pub label: &'static str,
    pub caption: &'static str,
    pub options: &'static [&'static str],
}

pub enum Block {
    Section { y: f32, label: &'static str },
    Row(Row),
}

impl Block {
    /// Top and bottom in window coordinates, so the layout test can prove the
    /// blocks neither overlap nor run past the footer.
    pub fn extent(&self) -> (f32, f32) {
        match self {
            Block::Section { y, .. } => (*y, y + SECTION_H),
            Block::Row(r) => (r.y, r.y + ROW_H),
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Block::Section { label, .. } => label,
            Block::Row(r) => r.label,
        }
    }
}

/// The page, top to bottom. The single source of what is drawn *and* what is
/// clickable — every position exists exactly once.
pub const BLOCKS: [Block; 11] = [
    Block::Section {
        y: 52.0,
        label: "W A T C H I N G",
    },
    Block::Row(Row {
        setting: Setting::SampleInterval,
        y: 74.0,
        label: "Check for you every",
        caption: "How often the camera looks while the screen is lit.",
        options: &["1s", "2s", "3s", "5s"],
    }),
    Block::Row(Row {
        setting: Setting::AwaySample,
        y: 138.0,
        label: "Once the screen is black, every",
        caption: "How fast the screen comes back when you return.",
        options: &["1s", "2s", "5s", "10s"],
    }),
    Block::Row(Row {
        setting: Setting::FaceConfirm,
        y: 202.0,
        label: "Sightings that count as present",
        caption: "Consecutive looks before VISOR trusts that you are here.",
        options: &["1", "2", "3", "4"],
    }),
    Block::Section {
        y: 274.0,
        label: "W A K I N G",
    },
    Block::Row(Row {
        setting: Setting::WakeConfirm,
        y: 296.0,
        label: "Sightings that wake the screen",
        caption: "Fewer brings it back sooner; more waits to be sure.",
        options: &["1", "2", "3", "4"],
    }),
    Block::Row(Row {
        setting: Setting::WakeProbation,
        y: 360.0,
        label: "A wake stays provisional for",
        caption: "An unconfirmed wake drops back to where it was after this.",
        options: &["5s", "10s", "20s", "30s"],
    }),
    Block::Row(Row {
        setting: Setting::HoldAwake,
        y: 424.0,
        label: "Keep Windows awake while you are there",
        caption: "Windows will not sleep or lock while the camera sees you.",
        options: &["Off", "On"],
    }),
    Block::Section {
        y: 496.0,
        label: "D I S P L A Y",
    },
    Block::Row(Row {
        setting: Setting::Strategy,
        y: 518.0,
        label: "Dimming mechanism",
        caption: "Auto picks per monitor. Force one only if auto gets it wrong.",
        options: &["Auto", "DDC", "Overlay", "Broadcast"],
    }),
    Block::Row(Row {
        setting: Setting::Theme,
        y: 582.0,
        label: "Theme",
        caption: "OLED is a true black ground \u{2014} it haloes on an LCD.",
        options: &["Dark", "Light", "OLED"],
    }),
];

pub fn rows() -> impl Iterator<Item = &'static Row> {
    BLOCKS.iter().filter_map(|b| match b {
        Block::Row(r) => Some(r),
        _ => None,
    })
}

/// Where one option is drawn. Segments share the content column evenly, so a
/// two-option row gets two wide halves and a four-option row four quarters.
pub fn segment_rect(row: &Row, index: usize) -> (f32, f32, f32, f32) {
    let n = row.options.len().max(1) as f32;
    let w = (RIGHT - LEFT - SEG_GAP * (n - 1.0)) / n;
    let x = LEFT + (w + SEG_GAP) * index as f32;
    let top = row.y + SEG_DY;
    (x, top, x + w, top + SEG_H)
}

/// Which option, if any, is under this point. Walks the same table the painter
/// walks, which is the whole reason the table exists.
pub fn hit(x: f32, y: f32) -> Option<(Setting, usize)> {
    for row in rows() {
        for i in 0..row.options.len() {
            let (l, t, r, b) = segment_rect(row, i);
            if x >= l && x <= r && y >= t && y <= b {
                return Some((row.setting, i));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    /// The page has no scrollbar by construction, so a row that does not fit
    /// is a row that is simply invisible and unreachable.
    #[test]
    fn every_row_fits_between_the_status_band_and_the_footer() {
        let mut lowest = TOP;
        for b in BLOCKS {
            let (top, bottom) = b.extent();
            assert!(
                top >= lowest,
                "{:?} at {top} overlaps what is above it (which ends at {lowest})",
                b.label()
            );
            lowest = bottom;
        }
        assert!(
            lowest <= FOOTER_HAIRLINE,
            "the page runs {} px past the footer hairline",
            lowest - FOOTER_HAIRLINE
        );
    }

    #[test]
    fn a_segment_is_clickable_exactly_where_it_is_drawn() {
        for row in rows() {
            for i in 0..row.options.len() {
                let (l, t, r, b) = segment_rect(row, i);
                let centre = ((l + r) / 2.0, (t + b) / 2.0);
                assert_eq!(
                    hit(centre.0, centre.1),
                    Some((row.setting, i)),
                    "{} option {i} is not clickable at its own centre",
                    row.label
                );
            }
            // The gap between two segments belongs to neither.
            if row.options.len() > 1 {
                let (_, _, r0, _) = segment_rect(row, 0);
                let (l1, t, _, b) = segment_rect(row, 1);
                let mid = ((r0 + l1) / 2.0, (t + b) / 2.0);
                assert_eq!(hit(mid.0, mid.1), None, "{} gap is live", row.label);
            }
        }
    }

    /// The whole point of the page: a click has to survive the trip out to
    /// `config.toml` and back and still light the same segment. An option
    /// whose value the config cannot hold would light nothing after a save,
    /// so the setting would look like it refused the click.
    #[test]
    fn every_offered_option_round_trips_through_the_config() {
        for row in rows() {
            for i in 0..row.options.len() {
                let mut cfg = Config::default();
                let mut s = Settings::from_config(&cfg);
                row.setting.apply(&mut s, i);
                s.write_into(&mut cfg);
                let back = Settings::from_config(&cfg);
                assert_eq!(
                    row.setting.selected(&back),
                    Some(i),
                    "{} option {:?} did not survive the round trip",
                    row.label,
                    row.options[i]
                );
            }
        }
    }

    /// `Config::save` validates before writing, so an option that produced an
    /// invalid config would make the click a silent no-op: the segment would
    /// light, the file would never change, and the setting would revert on the
    /// next start with nothing to explain why.
    #[test]
    fn every_offered_option_writes_a_config_the_save_path_accepts() {
        for row in rows() {
            for i in 0..row.options.len() {
                let mut cfg = Config::default();
                let mut s = Settings::from_config(&cfg);
                row.setting.apply(&mut s, i);
                s.write_into(&mut cfg);
                assert!(
                    cfg.validate().is_ok(),
                    "{} option {:?} writes a config save would refuse",
                    row.label,
                    row.options[i]
                );
            }
        }
    }

    /// A hand-edited TOML value the page does not offer must light nothing
    /// rather than light the nearest neighbour — showing "2s" selected while
    /// the file says 4s would be a lie, and the page must not quietly rewrite
    /// a value the user chose deliberately.
    #[test]
    fn a_value_the_page_does_not_offer_lights_no_segment() {
        let mut cfg = Config::default();
        cfg.presence.sample_interval = std::time::Duration::from_secs(4);
        let s = Settings::from_config(&cfg);
        assert_eq!(Setting::SampleInterval.selected(&s), None);
        // ...and the row has to say what IS in force, or it just looks broken.
        assert_eq!(Setting::SampleInterval.current(&s), "4s");
    }

    #[test]
    fn defaults_light_exactly_one_segment_in_every_row() {
        let s = Settings::from_config(&Config::default());
        for row in rows() {
            assert!(
                row.setting.selected(&s).is_some(),
                "{} offers nothing matching the shipped default",
                row.label
            );
        }
    }
}
