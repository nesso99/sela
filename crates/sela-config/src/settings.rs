//! App settings (`settings.json`). Theme preference and UI font size. No secrets.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::store::{config_dir, write_private, ConfigError};

/// Theme choice persisted in `{config_dir}/settings.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ThemePreference {
    Light,
    #[default]
    Dark,
    System,
}

impl ThemePreference {
    pub fn label(self) -> &'static str {
        match self {
            Self::Light => "Light",
            Self::Dark => "Dark",
            Self::System => "System",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "light" => Some(Self::Light),
            "dark" => Some(Self::Dark),
            "system" => Some(Self::System),
            _ => None,
        }
    }
}

/// Resolve the on-disk preference into a concrete dark/light mode.
///
/// `system_is_dark` comes from the window/OS appearance when the preference is
/// [`ThemePreference::System`]. Missing or unreadable settings mean Dark.
pub fn resolve_theme_mode(preference: ThemePreference, system_is_dark: bool) -> bool {
    match preference {
        ThemePreference::Light => false,
        ThemePreference::Dark => true,
        ThemePreference::System => system_is_dark,
    }
}

/// UI font size step (pixels). Default 13; clamped to 11..=18.
pub const DEFAULT_FONT_SIZE: u8 = 13;
pub const MIN_FONT_SIZE: u8 = 11;
pub const MAX_FONT_SIZE: u8 = 18;

/// Clamp a raw font size into the allowed step range.
pub fn clamp_font_size(raw: i64) -> u8 {
    if raw < i64::from(MIN_FONT_SIZE) {
        MIN_FONT_SIZE
    } else if raw > i64::from(MAX_FONT_SIZE) {
        MAX_FONT_SIZE
    } else {
        raw as u8
    }
}

/// Mono size = max(11, ui − 1).
pub fn mono_font_size(ui: u8) -> u8 {
    let ui = clamp_font_size(i64::from(ui));
    (ui.saturating_sub(1)).max(MIN_FONT_SIZE)
}

/// `text_xs` size: at the default step leave the kit value; otherwise max(11, ui − 2).
///
/// Returns `None` when the kit should keep its own `text_xs` (UI == 13).
pub fn text_xs_font_size(ui: u8) -> Option<u8> {
    let ui = clamp_font_size(i64::from(ui));
    if ui == DEFAULT_FONT_SIZE {
        None
    } else {
        Some(ui.saturating_sub(2).max(MIN_FONT_SIZE))
    }
}

/// UI line height = max(14, round(ui * 16 / 13)).
pub fn ui_line_height(ui: u8) -> u8 {
    let ui = clamp_font_size(i64::from(ui));
    let scaled = ((f64::from(ui) * 16.0) / 13.0).round() as i64;
    scaled.max(14) as u8
}

/// Editor line height = max(18, round(18 * mono / 12)).
pub fn editor_line_height(ui: u8) -> u8 {
    let mono = mono_font_size(ui);
    let scaled = ((18.0 * f64::from(mono)) / 12.0).round() as i64;
    scaled.max(18) as u8
}

/// Grid / sidebar / history / filter row height = max(26, round(26 * mono / 12)).
pub fn content_row_height(ui: u8) -> u8 {
    let mono = mono_font_size(ui);
    let scaled = ((26.0 * f64::from(mono)) / 12.0).round() as i64;
    scaled.max(26) as u8
}

/// In-memory settings loaded from or written to `settings.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppSettings {
    pub theme: ThemePreference,
    pub font_size: u8,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            theme: ThemePreference::Dark,
            font_size: DEFAULT_FONT_SIZE,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SettingsFile {
    #[serde(default)]
    theme: ThemePreference,
    /// Missing or non-integer → 13. Out of range is clamped on load.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    font_size: Option<serde_json::Value>,
}

fn parse_font_size_value(value: Option<&serde_json::Value>) -> u8 {
    match value {
        None => DEFAULT_FONT_SIZE,
        Some(serde_json::Value::Number(n)) => {
            if let Some(i) = n.as_i64() {
                clamp_font_size(i)
            } else if let Some(f) = n.as_f64() {
                clamp_font_size(f.round() as i64)
            } else {
                DEFAULT_FONT_SIZE
            }
        }
        Some(_) => DEFAULT_FONT_SIZE,
    }
}

impl SettingsFile {
    fn into_settings(self) -> AppSettings {
        AppSettings {
            theme: self.theme,
            font_size: parse_font_size_value(self.font_size.as_ref()),
        }
    }

    fn from_settings(settings: AppSettings) -> Self {
        Self {
            theme: settings.theme,
            font_size: Some(serde_json::Value::from(clamp_font_size(i64::from(
                settings.font_size,
            )))),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SettingsStore {
    path: PathBuf,
}

impl SettingsStore {
    pub fn default_path() -> PathBuf {
        config_dir().join("settings.json")
    }

    pub fn with_path(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Load settings. A missing or unreadable file means Dark + font 13.
    pub fn load(&self) -> AppSettings {
        self.load_inner().unwrap_or_default()
    }

    /// Theme-only helper kept for call sites that only need the preference.
    pub fn load_theme(&self) -> ThemePreference {
        self.load().theme
    }

    fn load_inner(&self) -> Result<AppSettings, ConfigError> {
        if !self.path.exists() {
            return Ok(AppSettings::default());
        }
        let bytes = fs::read(&self.path).map_err(|source| ConfigError::Read {
            path: self.path.clone(),
            source,
        })?;
        let file: SettingsFile =
            serde_json::from_slice(&bytes).map_err(|source| ConfigError::Parse {
                path: self.path.clone(),
                source,
            })?;
        Ok(file.into_settings())
    }

    pub fn save(&self, settings: AppSettings) -> Result<(), ConfigError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|source| ConfigError::Write {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let body = serde_json::to_vec_pretty(&SettingsFile::from_settings(settings))
            .expect("settings are always serializable");
        let tmp = self.path.with_extension("json.tmp");
        write_private(&tmp, &body).map_err(|source| ConfigError::Write {
            path: tmp.clone(),
            source,
        })?;
        fs::rename(&tmp, &self.path).map_err(|source| ConfigError::Write {
            path: self.path.clone(),
            source,
        })?;
        Ok(())
    }

    /// Convenience: save theme while keeping the current font size from disk.
    pub fn save_theme(&self, theme: ThemePreference) -> Result<(), ConfigError> {
        let mut settings = self.load();
        settings.theme = theme;
        self.save(settings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_is_dark_and_default_font() {
        let dir = tempfile::tempdir().unwrap();
        let store = SettingsStore::with_path(dir.path().join("settings.json"));
        let loaded = store.load();
        assert_eq!(loaded.theme, ThemePreference::Dark);
        assert_eq!(loaded.font_size, DEFAULT_FONT_SIZE);
    }

    #[test]
    fn round_trip_theme_and_font_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let store = SettingsStore::with_path(&path);
        store
            .save(AppSettings {
                theme: ThemePreference::Light,
                font_size: 15,
            })
            .unwrap();
        let loaded = store.load();
        assert_eq!(loaded.theme, ThemePreference::Light);
        assert_eq!(loaded.font_size, 15);
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"theme\": \"light\""));
        assert!(text.contains("\"font_size\": 15"));
        assert!(!text.to_ascii_lowercase().contains("password"));
        assert!(!text.to_ascii_lowercase().contains("secret"));
    }

    #[test]
    fn old_theme_only_file_defaults_font_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, "{ \"theme\": \"system\" }\n").unwrap();
        let store = SettingsStore::with_path(&path);
        let loaded = store.load();
        assert_eq!(loaded.theme, ThemePreference::System);
        assert_eq!(loaded.font_size, DEFAULT_FONT_SIZE);
    }

    #[test]
    fn font_size_clamps_and_rejects_non_integer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, "{ \"theme\": \"dark\", \"font_size\": 3 }\n").unwrap();
        assert_eq!(
            SettingsStore::with_path(&path).load().font_size,
            MIN_FONT_SIZE
        );
        fs::write(&path, "{ \"theme\": \"dark\", \"font_size\": 99 }\n").unwrap();
        assert_eq!(
            SettingsStore::with_path(&path).load().font_size,
            MAX_FONT_SIZE
        );
        fs::write(&path, "{ \"theme\": \"dark\", \"font_size\": \"big\" }\n").unwrap();
        assert_eq!(
            SettingsStore::with_path(&path).load().font_size,
            DEFAULT_FONT_SIZE
        );
    }

    #[test]
    fn unreadable_or_invalid_json_falls_back_to_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, "{not json").unwrap();
        let store = SettingsStore::with_path(&path);
        assert_eq!(store.load(), AppSettings::default());
    }

    #[test]
    fn resolve_theme_mode_matrix() {
        assert!(!resolve_theme_mode(ThemePreference::Light, true));
        assert!(resolve_theme_mode(ThemePreference::Dark, false));
        assert!(resolve_theme_mode(ThemePreference::System, true));
        assert!(!resolve_theme_mode(ThemePreference::System, false));
    }

    #[test]
    fn mono_and_row_formulas() {
        assert_eq!(mono_font_size(11), 11);
        assert_eq!(mono_font_size(13), 12);
        assert_eq!(mono_font_size(18), 17);
        assert_eq!(text_xs_font_size(13), None);
        assert_eq!(text_xs_font_size(11), Some(11));
        assert_eq!(text_xs_font_size(18), Some(16));
        assert_eq!(ui_line_height(13), 16);
        assert_eq!(ui_line_height(11), 14);
        assert_eq!(ui_line_height(18), 22);
        assert_eq!(editor_line_height(13), 18);
        assert_eq!(editor_line_height(18), 26);
        assert_eq!(content_row_height(13), 26);
        assert_eq!(content_row_height(18), 37);
        assert_eq!(clamp_font_size(10), 11);
        assert_eq!(clamp_font_size(19), 18);
    }
}
