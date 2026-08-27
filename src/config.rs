use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::sounds::BuiltinSound;

/// The alert sound the app plays on a beep: one of the six synthesized
/// built-ins, or a user-chosen file decoded at boot/selection time (see
/// `src/decode.rs`). On-disk shape (externally tagged, snake_case variant
/// names): `alert_sound = { builtin = "soft_beep" }` or
/// `alert_sound = { custom = { path = "C:/.../ding.wav" } }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertSound {
    Builtin(BuiltinSound),
    Custom { path: String },
}

impl Default for AlertSound {
    fn default() -> Self {
        Self::Builtin(BuiltinSound::SoftBeep)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CalibrationState {
    BaselineOnly,
    CeilingSet,
    CeilingLearned,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct DeviceCalibration {
    pub state: CalibrationState,
    pub quiet_db: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ceiling_db: Option<f32>,
    pub noise_floor_db: f32,
    pub sensitivity: f32,
}

impl Default for DeviceCalibration {
    /// Intentionally-invalid sentinel: a half-parsed entry (missing fields)
    /// must behave as uncalibrated rather than inventing a calibration.
    fn default() -> Self {
        Self {
            state: CalibrationState::BaselineOnly,
            quiet_db: f32::NAN,
            ceiling_db: None,
            noise_floor_db: f32::NAN,
            sensitivity: 0.5,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    pub hold_ms: u64,
    pub cooldown_ms: u64,
    /// Keyed by input device name; each device calibrates separately.
    pub calibration: BTreeMap<String, DeviceCalibration>,
    /// ISO date ("2026-08-27") the calibration-incomplete banner was last
    /// dismissed; `None` before the first dismissal. Compared against
    /// today's date so the banner reappears once per day.
    #[serde(default)]
    pub banner_dismissed_on: Option<String>,
    /// Which sound plays on a beep; see `AlertSound`.
    #[serde(default)]
    pub alert_sound: AlertSound,
    /// 0..=1; see `effective_volume` for the clamped/NaN-safe read.
    #[serde(default)]
    pub alert_volume: f32,
    /// `None` = the system default output device.
    #[serde(default)]
    pub output_device: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            hold_ms: 300,
            cooldown_ms: 3000,
            calibration: BTreeMap::new(),
            banner_dismissed_on: None,
            alert_sound: AlertSound::default(),
            alert_volume: 0.8,
            output_device: None,
        }
    }
}

impl DeviceCalibration {
    /// The single boundary between persisted values and the engine: anything
    /// non-finite or out of range degrades to uncalibrated defaults instead of
    /// poisoning detection (config files get hand-edited).
    pub fn tuning(&self) -> crate::engine::Tuning {
        let defaults = crate::engine::Tuning::default();
        if !self.quiet_db.is_finite() || !self.noise_floor_db.is_finite() {
            return defaults;
        }
        let sensitivity = if self.sensitivity.is_finite() {
            self.sensitivity.clamp(0.0, 1.0)
        } else {
            0.5
        };
        let ceiling_db = self
            .ceiling_db
            .filter(|c| c.is_finite() && crate::engine::calibrate::ceiling_is_sane(self.quiet_db, *c));
        crate::engine::Tuning {
            noise_floor_db: self.noise_floor_db,
            quiet_db: Some(self.quiet_db),
            ceiling_db,
            sensitivity,
        }
    }
}

/// Load-time mirror of `Config` where each calibration entry is still raw
/// TOML; this lets one malformed device entry fail on its own without
/// dragging down `hold_ms`/`cooldown_ms` or other devices' calibration.
#[derive(Debug, Deserialize)]
#[serde(default)]
struct RawConfig {
    hold_ms: u64,
    cooldown_ms: u64,
    calibration: BTreeMap<String, toml::Value>,
    banner_dismissed_on: Option<String>,
    alert_sound: AlertSound,
    alert_volume: f32,
    output_device: Option<String>,
}

impl Default for RawConfig {
    fn default() -> Self {
        let d = Config::default();
        Self {
            hold_ms: d.hold_ms,
            cooldown_ms: d.cooldown_ms,
            calibration: BTreeMap::new(),
            banner_dismissed_on: None,
            alert_sound: d.alert_sound,
            alert_volume: d.alert_volume,
            output_device: d.output_device,
        }
    }
}

impl RawConfig {
    fn into_config(self) -> Config {
        let calibration = self
            .calibration
            .into_iter()
            .map(|(name, value)| {
                let cal = DeviceCalibration::deserialize(value).unwrap_or_default();
                (name, cal)
            })
            .collect();
        Config {
            hold_ms: self.hold_ms,
            cooldown_ms: self.cooldown_ms,
            calibration,
            banner_dismissed_on: self.banner_dismissed_on,
            alert_sound: self.alert_sound,
            alert_volume: self.alert_volume,
            output_device: self.output_device,
        }
    }
}

impl Config {
    pub fn path() -> Option<PathBuf> {
        std::env::var_os("APPDATA")
            .map(|a| Path::new(&a).join("ragequiet").join("config.toml"))
    }

    /// Missing or unreadable/corrupt file yields defaults; the app must always start.
    /// A single malformed `[calibration."device"]` entry must not discard the
    /// rest of the file (global settings, other devices), so calibration
    /// entries are parsed leniently: a bad entry falls back to the
    /// uncalibrated sentinel rather than failing the whole document.
    pub fn load_from(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| toml::from_str::<RawConfig>(&s).ok())
            .map(RawConfig::into_config)
            .unwrap_or_default()
    }

    pub fn save_to(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, toml::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, path)?; // atomic-enough swap; no torn config on crash
        Ok(())
    }

    pub fn load() -> Self {
        Self::path().map(|p| Self::load_from(&p)).unwrap_or_default()
    }

    pub fn save(&self) -> anyhow::Result<()> {
        self.save_to(&Self::path().context("APPDATA not set")?)
    }

    /// The single boundary between the persisted volume and playback:
    /// out-of-range or non-finite (hand-edited config) degrades to the
    /// default 0.8 rather than clamping toward a corrupted extreme, mirroring
    /// `DeviceCalibration::tuning`'s sensitivity handling.
    pub fn effective_volume(&self) -> f32 {
        if self.alert_volume.is_finite() {
            self.alert_volume.clamp(0.0, 1.0)
        } else {
            0.8
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_config_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("ragequiet-test-{tag}-{}", std::process::id())).join("config.toml")
    }

    #[test]
    fn missing_file_loads_defaults() {
        let cfg = Config::load_from(&temp_config_path("missing"));
        assert_eq!(cfg, Config::default());
        assert_eq!(cfg.hold_ms, 300);
        assert_eq!(cfg.cooldown_ms, 3000);
        assert!(cfg.calibration.is_empty());
    }

    #[test]
    fn corrupt_file_loads_defaults() {
        let path = temp_config_path("corrupt");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "not [valid toml ((").unwrap();
        assert_eq!(Config::load_from(&path), Config::default());
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn round_trips_banner_dismissed_on() {
        let path = temp_config_path("banner");
        let cfg = Config {
            banner_dismissed_on: Some("2026-08-27".to_string()),
            ..Config::default()
        };
        cfg.save_to(&path).unwrap();
        assert_eq!(Config::load_from(&path), cfg);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn round_trips_calibration() {
        let path = temp_config_path("roundtrip");
        let mut cfg = Config::default();
        cfg.calibration.insert(
            "Headset Microphone".into(),
            DeviceCalibration {
                state: CalibrationState::CeilingSet,
                quiet_db: -34.2,
                ceiling_db: Some(-18.7),
                noise_floor_db: -58.0,
                sensitivity: 0.5,
            },
        );
        cfg.save_to(&path).unwrap();
        assert_eq!(Config::load_from(&path), cfg);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn serializes_state_as_snake_case() {
        let path = temp_config_path("snake");
        let mut cfg = Config::default();
        cfg.calibration.insert(
            "Mic".into(),
            DeviceCalibration {
                state: CalibrationState::BaselineOnly,
                quiet_db: -34.0,
                ceiling_db: None,
                noise_floor_db: -58.0,
                sensitivity: 0.5,
            },
        );
        cfg.save_to(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"baseline_only\""), "got: {text}");
        assert!(!text.contains("ceiling_db"), "None ceiling must be omitted: {text}");
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn nan_and_inf_calibration_yields_uncalibrated_tuning() {
        let cal = DeviceCalibration {
            state: CalibrationState::CeilingSet,
            quiet_db: f32::NAN,
            ceiling_db: Some(f32::INFINITY),
            noise_floor_db: -58.0,
            sensitivity: 0.5,
        };
        let t = cal.tuning();
        assert!(t.quiet_db.is_none(), "non-finite quiet point must fall back to uncalibrated");
        assert!(t.noise_floor_db.is_finite());
    }

    #[test]
    fn sensitivity_is_clamped_to_unit_range() {
        let mut cal = DeviceCalibration {
            state: CalibrationState::CeilingSet,
            quiet_db: -37.0,
            ceiling_db: Some(-17.0),
            noise_floor_db: -58.0,
            sensitivity: 5.0,
        };
        assert_eq!(cal.tuning().sensitivity, 1.0);
        cal.sensitivity = -2.0;
        assert_eq!(cal.tuning().sensitivity, 0.0);
        cal.sensitivity = f32::NAN;
        assert_eq!(cal.tuning().sensitivity, 0.5);
    }

    #[test]
    fn insane_ceiling_is_dropped_not_fatal() {
        let cal = DeviceCalibration {
            state: CalibrationState::CeilingSet,
            quiet_db: -37.0,
            ceiling_db: Some(-50.0), // "ceiling" quieter than quiet point
            noise_floor_db: -58.0,
            sensitivity: 0.5,
        };
        let t = cal.tuning();
        assert_eq!(t.quiet_db, Some(-37.0), "quiet point survives");
        assert!(t.ceiling_db.is_none(), "inverted ceiling must be discarded");
    }

    #[test]
    fn bare_nan_in_file_does_not_kill_detection() {
        let path = temp_config_path("nanfile");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "hold_ms = 300\ncooldown_ms = 3000\n\n[calibration.\"Mic\"]\nstate = \"baseline_only\"\nquiet_db = nan\nnoise_floor_db = -58.0\nsensitivity = 0.5\n").unwrap();
        let cfg = Config::load_from(&path);
        assert_eq!(cfg.hold_ms, 300);
        let t = cfg.calibration.get("Mic").expect("entry must survive parsing").tuning();
        assert!(t.quiet_db.is_none(), "NaN quiet point must yield uncalibrated tuning");
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn partial_device_entry_does_not_discard_the_config() {
        let path = temp_config_path("partial");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "hold_ms = 450\n\n[calibration.\"Good\"]\nstate = \"ceiling_set\"\nquiet_db = -34.0\nceiling_db = -18.0\nnoise_floor_db = -58.0\nsensitivity = 0.5\n\n[calibration.\"Bad\"]\nstate = \"baseline_only\"\n").unwrap();
        let cfg = Config::load_from(&path);
        assert_eq!(cfg.hold_ms, 450, "global settings must survive a bad device entry");
        assert_eq!(cfg.calibration.get("Good").map(|c| c.quiet_db), Some(-34.0));
        // the Bad entry either parses with harmless defaults or is dropped; it
        // must not produce a calibrated tuning either way
        if let Some(bad) = cfg.calibration.get("Bad") {
            assert!(bad.tuning().quiet_db.is_none());
        }
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }
}

#[cfg(test)]
mod alert_tests {
    use super::*;
    use crate::sounds::BuiltinSound;

    fn temp_config_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("ragequiet-test-alert-{tag}-{}", std::process::id())).join("config.toml")
    }

    #[test]
    fn default_alert_sound_is_soft_beep_at_default_volume() {
        let cfg = Config::default();
        assert_eq!(cfg.alert_sound, AlertSound::Builtin(BuiltinSound::SoftBeep));
        assert_eq!(cfg.alert_volume, 0.8);
        assert_eq!(cfg.output_device, None);
    }

    #[test]
    fn builtin_alert_sound_round_trips() {
        let path = temp_config_path("builtin");
        let cfg = Config { alert_sound: AlertSound::Builtin(BuiltinSound::Chime), ..Config::default() };
        cfg.save_to(&path).unwrap();
        assert_eq!(Config::load_from(&path), cfg);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn custom_alert_sound_round_trips_with_path() {
        let path = temp_config_path("custom");
        let cfg = Config {
            alert_sound: AlertSound::Custom { path: "C:/Users/me/Sounds/ding.wav".to_string() },
            alert_volume: 0.4,
            output_device: Some("Speakers (Realtek)".to_string()),
            ..Config::default()
        };
        cfg.save_to(&path).unwrap();
        assert_eq!(Config::load_from(&path), cfg);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    /// Documents the actual on-disk TOML shape: `toml`'s pretty serializer
    /// renders the externally-tagged enum as its own table (not an inline
    /// table) — `[alert_sound]` / `builtin = "chime"` for the newtype
    /// variant, `[alert_sound.custom]` / `path = "..."` for the struct
    /// variant's named field.
    #[test]
    fn alert_sound_toml_shape_is_externally_tagged_snake_case() {
        let path = temp_config_path("shape-builtin");
        let cfg = Config { alert_sound: AlertSound::Builtin(BuiltinSound::Chime), ..Config::default() };
        cfg.save_to(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[alert_sound]"), "unexpected builtin shape: {text}");
        assert!(text.contains("builtin = \"chime\""), "unexpected builtin shape: {text}");
        std::fs::remove_dir_all(path.parent().unwrap()).ok();

        let path = temp_config_path("shape-custom");
        let cfg = Config {
            alert_sound: AlertSound::Custom { path: "C:/ding.wav".to_string() },
            ..Config::default()
        };
        cfg.save_to(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[alert_sound.custom]"), "unexpected custom shape: {text}");
        assert!(text.contains("path = \"C:/ding.wav\""), "unexpected custom shape: {text}");
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn effective_volume_passes_through_in_range_values() {
        let cfg = Config { alert_volume: 0.3, ..Config::default() };
        assert!((cfg.effective_volume() - 0.3).abs() < 1e-6);
    }

    #[test]
    fn effective_volume_clamps_out_of_range() {
        let mut cfg = Config { alert_volume: 5.0, ..Config::default() };
        assert_eq!(cfg.effective_volume(), 1.0);
        cfg.alert_volume = -2.0;
        assert_eq!(cfg.effective_volume(), 0.0);
    }

    #[test]
    fn effective_volume_falls_back_to_default_on_nan() {
        let cfg = Config { alert_volume: f32::NAN, ..Config::default() };
        assert_eq!(cfg.effective_volume(), 0.8);
    }

    #[test]
    fn missing_alert_fields_in_a_hand_edited_file_default_safely() {
        let path = temp_config_path("partial-alert");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "hold_ms = 300\ncooldown_ms = 3000\n").unwrap();
        let cfg = Config::load_from(&path);
        assert_eq!(cfg.alert_sound, AlertSound::Builtin(BuiltinSound::SoftBeep));
        assert_eq!(cfg.alert_volume, 0.8);
        assert_eq!(cfg.output_device, None);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }
}
