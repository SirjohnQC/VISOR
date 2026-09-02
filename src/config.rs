use crate::error::{Result, VisorError};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub presence: PresenceConfig,
    pub camera: CameraConfig,
    pub display: DisplayConfig,
    pub ui: UiConfig,
    pub log: LogConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PresenceConfig {
    #[serde(with = "humantime_serde")]
    pub idle_grace: Duration,
    #[serde(with = "humantime_serde")]
    pub sample_interval: Duration,
    #[serde(with = "humantime_serde")]
    pub dim_after: Duration,
    #[serde(with = "humantime_serde")]
    pub away_after: Duration,
    #[serde(with = "humantime_serde")]
    pub deep_after: Duration,
    #[serde(with = "humantime_serde")]
    pub away_sample: Duration,
    pub face_confirm: u8,
    pub wake_confirm: u8,
    #[serde(with = "humantime_serde")]
    pub wake_probation: Duration,
    pub min_face_ratio: f32,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CameraConfig {
    pub device: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DisplayConfig {
    pub targets: Vec<String>,
    pub strategy: String,
    pub dim_level: u8,
    pub hold_awake_while_present: bool,
}

/// Spec §7 addition: how the tuning window looks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UiConfig {
    /// `"dark"` (default), `"light"`, or `"oled"`.
    ///
    /// `oled` is a true-black ground rather than a darker grey: on an OLED
    /// `#000000` switches the subpixels off, where the default dark theme
    /// leaves every one of them lit. It is offered rather than imposed
    /// because pure black with high-contrast text haloes badly on an LCD.
    pub theme: String,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: "dark".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LogConfig {
    pub level: String,
}

impl Default for PresenceConfig {
    fn default() -> Self {
        Self {
            idle_grace: Duration::from_secs(30),
            sample_interval: Duration::from_secs(2),
            dim_after: Duration::from_secs(20),
            away_after: Duration::from_secs(45),
            deep_after: Duration::from_secs(15 * 60),
            away_sample: Duration::from_secs(1),
            face_confirm: 2,
            wake_confirm: 1,
            wake_probation: Duration::from_secs(10),
            min_face_ratio: 0.15,
        }
    }
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            targets: Vec::new(),
            strategy: "auto".to_string(),
            dim_level: 20,
            hold_awake_while_present: false,
        }
    }
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
        }
    }
}

impl Config {
    /// `%APPDATA%\VISOR\config.toml`
    pub fn default_path() -> PathBuf {
        let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(base).join("VISOR").join("config.toml")
    }

    /// Spec §7: a config that fails to parse or validate falls back to
    /// defaults and logs loudly rather than refusing to start.
    pub fn load_or_default(path: &Path) -> Config {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e,
                    "no config file; using defaults");
                return Config::default();
            }
        };
        let cfg: Config = match toml::from_str(&text) {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(path = %path.display(), error = %e,
                    "config failed to parse; falling back to defaults");
                return Config::default();
            }
        };
        if let Err(e) = cfg.validate() {
            tracing::error!(path = %path.display(), error = %e,
                "config failed validation; falling back to defaults");
            return Config::default();
        }
        cfg
    }

    /// Write defaults to `path` on first run. Best effort — a failure here
    /// must not stop VISOR from running.
    pub fn write_defaults_if_missing(path: &Path) {
        if path.exists() {
            return;
        }
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match toml::to_string_pretty(&Config::default()) {
            Ok(text) => {
                if let Err(e) = std::fs::write(path, text) {
                    tracing::warn!(path = %path.display(), error = %e,
                        "could not write default config");
                }
            }
            Err(e) => tracing::warn!(error = %e, "could not serialise default config"),
        }
    }

    /// Write the config back to disk.
    ///
    /// Validated first: the tuning window clamps as you drag, but a value that
    /// somehow arrived out of order must not be written, because
    /// `load_or_default` would then silently fall back to defaults on the next
    /// start and the user would find every setting reverted with no
    /// explanation.
    ///
    /// **This rewrites the whole file**, so hand-written comments and any
    /// ordering the user chose are lost. Preserving them needs a
    /// format-preserving TOML parser, which is a new dependency, and the
    /// allowlist is closed deliberately — it is what makes "no network code"
    /// checkable from `Cargo.lock`. The file is machine-written on first run
    /// anyway, so the cost is small, but it is a real cost and not an
    /// oversight.
    ///
    /// Writes through a temporary file and renames: a half-written config is
    /// a config that fails to parse, and this program is supposed to survive
    /// being killed at any moment.
    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let text = toml::to_string_pretty(self)
            .map_err(|e| VisorError::Config(format!("could not serialise config: {e}")))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| VisorError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text).map_err(|source| VisorError::Io {
            path: tmp.clone(),
            source,
        })?;
        std::fs::rename(&tmp, path).map_err(|source| VisorError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        tracing::info!(path = %path.display(), "config saved");
        Ok(())
    }

    /// Spec §7: dim_after < away_after < deep_after, all durations positive,
    /// min_face_ratio in (0,1), dim_level in 1..=99.
    pub fn validate(&self) -> Result<()> {
        let p = &self.presence;
        let bad = |m: &str| Err(VisorError::Config(m.to_string()));

        if !(p.dim_after < p.away_after && p.away_after < p.deep_after) {
            return bad("dim_after < away_after < deep_after must hold");
        }
        for (name, d) in [
            ("idle_grace", p.idle_grace),
            ("sample_interval", p.sample_interval),
            ("dim_after", p.dim_after),
            ("away_sample", p.away_sample),
            ("wake_probation", p.wake_probation),
        ] {
            if d.is_zero() {
                return bad(&format!("{name} must be greater than zero"));
            }
        }
        if !(p.min_face_ratio > 0.0 && p.min_face_ratio < 1.0) {
            return bad("min_face_ratio must be between 0 and 1 exclusive");
        }
        if p.face_confirm == 0 || p.wake_confirm == 0 {
            return bad("face_confirm and wake_confirm must be at least 1");
        }
        if !(1..=99).contains(&self.display.dim_level) {
            return bad("dim_level must be between 1 and 99");
        }
        if !matches!(
            self.display.strategy.as_str(),
            "auto" | "ddc" | "overlay" | "broadcast"
        ) {
            return bad("strategy must be auto, ddc, overlay or broadcast");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn defaults_match_the_spec() {
        let c = Config::default();
        assert_eq!(c.presence.idle_grace, Duration::from_secs(30));
        assert_eq!(c.presence.dim_after, Duration::from_secs(20));
        assert_eq!(c.presence.away_after, Duration::from_secs(45));
        assert_eq!(c.presence.deep_after, Duration::from_secs(15 * 60));
        assert_eq!(c.presence.away_sample, Duration::from_secs(1));
        assert_eq!(c.presence.face_confirm, 2);
        assert_eq!(c.presence.wake_confirm, 1);
        assert_eq!(c.display.dim_level, 20);
        assert!(!c.display.hold_awake_while_present);
    }

    #[test]
    fn parses_humantime_durations() {
        let toml = r#"
            [presence]
            idle_grace = "45s"
            deep_after = "10m"
        "#;
        let c: Config = toml::from_str(toml).unwrap();
        assert_eq!(c.presence.idle_grace, Duration::from_secs(45));
        assert_eq!(c.presence.deep_after, Duration::from_secs(600));
        // unspecified fields fall back to defaults
        assert_eq!(c.presence.away_after, Duration::from_secs(45));
    }

    #[test]
    fn a_saved_config_reloads_to_exactly_what_was_saved() {
        // The tuning window writes through this on every drag release. If a
        // round trip lost or altered anything, a setting would appear to take
        // and then quietly revert on the next start.
        let dir = std::env::temp_dir().join("visor-save-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");

        let mut cfg = Config::default();
        cfg.presence.min_face_ratio = 0.085;
        cfg.presence.dim_after = Duration::from_secs(37);
        cfg.presence.away_after = Duration::from_secs(95);
        cfg.display.dim_level = 35;
        cfg.ui.theme = "oled".to_string();

        cfg.save(&path).unwrap();
        assert_eq!(Config::load_or_default(&path), cfg);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn an_invalid_config_is_refused_rather_than_written() {
        // Writing it would be worse than refusing: load_or_default would fall
        // back to defaults next start, and the user would find EVERY setting
        // reverted with nothing to explain why.
        let dir = std::env::temp_dir().join("visor-save-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bad.toml");

        let mut cfg = Config::default();
        cfg.presence.dim_after = Duration::from_secs(600); // past away_after
        assert!(cfg.save(&path).is_err());
        assert!(!path.exists(), "nothing may be left on disk");
    }

    #[test]
    fn validate_rejects_non_increasing_thresholds() {
        let mut c = Config::default();
        c.presence.away_after = Duration::from_secs(10); // below dim_after
        assert!(c.validate().is_err());
    }

    #[test]
    fn validate_rejects_out_of_range_ratio_and_dim_level() {
        let mut c = Config::default();
        c.presence.min_face_ratio = 1.5;
        assert!(c.validate().is_err());

        let mut c = Config::default();
        c.display.dim_level = 0;
        assert!(c.validate().is_err());
    }

    #[test]
    fn invalid_config_falls_back_to_defaults_rather_than_failing() {
        let dir = std::env::temp_dir().join("visor_cfg_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bad.toml");
        std::fs::write(&path, "this is not valid toml {{{").unwrap();

        let c = Config::load_or_default(&path);
        assert_eq!(c, Config::default());
        std::fs::remove_file(&path).ok();
    }
}
