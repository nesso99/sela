//! App settings (`settings.json`). No secrets — theme preference only for now.

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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SettingsFile {
    #[serde(default)]
    theme: ThemePreference,
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

    /// Load settings. A missing or unreadable file means Dark (current binary default).
    pub fn load(&self) -> ThemePreference {
        match self.load_inner() {
            Ok(theme) => theme,
            Err(_) => ThemePreference::Dark,
        }
    }

    fn load_inner(&self) -> Result<ThemePreference, ConfigError> {
        if !self.path.exists() {
            return Ok(ThemePreference::Dark);
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
        Ok(file.theme)
    }

    pub fn save(&self, theme: ThemePreference) -> Result<(), ConfigError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|source| ConfigError::Write {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let body = serde_json::to_vec_pretty(&SettingsFile { theme })
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_is_dark() {
        let dir = tempfile::tempdir().unwrap();
        let store = SettingsStore::with_path(dir.path().join("settings.json"));
        assert_eq!(store.load(), ThemePreference::Dark);
    }

    #[test]
    fn round_trip_theme_preference() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let store = SettingsStore::with_path(&path);
        store.save(ThemePreference::Light).unwrap();
        assert_eq!(store.load(), ThemePreference::Light);
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"theme\": \"light\""));
        assert!(!text.to_ascii_lowercase().contains("password"));
        assert!(!text.to_ascii_lowercase().contains("secret"));

        store.save(ThemePreference::System).unwrap();
        assert_eq!(store.load(), ThemePreference::System);
    }

    #[test]
    fn unreadable_or_invalid_json_falls_back_to_dark() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, "{not json").unwrap();
        let store = SettingsStore::with_path(&path);
        assert_eq!(store.load(), ThemePreference::Dark);
    }

    #[test]
    fn resolve_theme_mode_matrix() {
        assert!(!resolve_theme_mode(ThemePreference::Light, true));
        assert!(resolve_theme_mode(ThemePreference::Dark, false));
        assert!(resolve_theme_mode(ThemePreference::System, true));
        assert!(!resolve_theme_mode(ThemePreference::System, false));
    }
}
