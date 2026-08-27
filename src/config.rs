use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CalibrationState {
    BaselineOnly,
    CeilingSet,
    CeilingLearned,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeviceCalibration {
    pub state: CalibrationState,
    pub quiet_db: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ceiling_db: Option<f32>,
    pub noise_floor_db: f32,
    pub sensitivity: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    pub hold_ms: u64,
    pub cooldown_ms: u64,
    /// Keyed by input device name; each device calibrates separately.
    pub calibration: BTreeMap<String, DeviceCalibration>,
}

impl Default for Config {
    fn default() -> Self {
        Self { hold_ms: 300, cooldown_ms: 3000, calibration: BTreeMap::new() }
    }
}

impl Config {
    pub fn path() -> Option<PathBuf> {
        std::env::var_os("APPDATA")
            .map(|a| Path::new(&a).join("ragequiet").join("config.toml"))
    }

    /// Missing or unreadable/corrupt file yields defaults; the app must always start.
    pub fn load_from(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
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
}
