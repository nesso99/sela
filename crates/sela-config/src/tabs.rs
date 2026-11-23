//! Persist the open tab list in `tabs.json` (separate from `connections.json`).
//!
//! Never stores passwords. Missing or corrupt files yield an empty session so
//! a bad file cannot block startup.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use sela_core::PersistedTabs;

use super::store::{config_dir, ConfigError};

#[derive(Debug, Clone)]
pub struct TabStore {
    path: PathBuf,
}

impl TabStore {
    pub fn default_path() -> PathBuf {
        config_dir().join("tabs.json")
    }

    pub fn with_path(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Load tabs. A missing file is empty. Corrupt JSON is also treated as empty
    /// (returned as `Ok(default)`) so the app can start; callers may log.
    pub fn load(&self) -> Result<PersistedTabs, ConfigError> {
        if !self.path.exists() {
            return Ok(PersistedTabs::default());
        }
        let bytes = fs::read(&self.path).map_err(|source| ConfigError::Read {
            path: self.path.clone(),
            source,
        })?;
        match serde_json::from_slice::<PersistedTabs>(&bytes) {
            Ok(tabs) => Ok(tabs),
            Err(_) => Ok(PersistedTabs::default()),
        }
    }

    /// Load and report corruption distinctly for tests / diagnostics.
    pub fn load_strict(&self) -> Result<PersistedTabs, ConfigError> {
        if !self.path.exists() {
            return Ok(PersistedTabs::default());
        }
        let bytes = fs::read(&self.path).map_err(|source| ConfigError::Read {
            path: self.path.clone(),
            source,
        })?;
        serde_json::from_slice(&bytes).map_err(|source| ConfigError::Parse {
            path: self.path.clone(),
            source,
        })
    }

    pub fn save(&self, tabs: &PersistedTabs) -> Result<(), ConfigError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|source| ConfigError::Write {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let body = serde_json::to_vec_pretty(tabs).expect("tab session is always serializable");
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

fn write_private(path: &Path, body: &[u8]) -> std::io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(body)?;
    file.write_all(b"\n")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sela_core::{PersistedTab, TabKind, TabList};

    #[test]
    fn round_trip_preserves_tabs_without_passwords() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tabs.json");
        let mut list = TabList::new();
        list.open_table("conn-1", "public", "books");
        list.open_query("conn-1", "select * from books");
        list.activate_index(1);
        let saved = list.to_persisted();

        let store = TabStore::with_path(&path);
        store.save(&saved).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.to_ascii_lowercase().contains("password"));
        assert!(text.contains("conn-1"));
        assert!(text.contains("select * from books"));

        let loaded = store.load().unwrap();
        assert_eq!(loaded, saved);
        assert_eq!(loaded.active, Some(1));
        assert!(matches!(
            &loaded.tabs[0].kind,
            TabKind::Table { table, .. } if table == "books"
        ));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn missing_file_is_empty_session() {
        let dir = tempfile::tempdir().unwrap();
        let store = TabStore::with_path(dir.path().join("missing.json"));
        let loaded = store.load().unwrap();
        assert!(loaded.tabs.is_empty());
        assert_eq!(loaded.active, None);
    }

    #[test]
    fn corrupt_file_loads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tabs.json");
        fs::write(&path, b"{ not valid json").unwrap();
        let store = TabStore::with_path(&path);
        let loaded = store.load().unwrap();
        assert_eq!(loaded, PersistedTabs::default());
        assert!(store.load_strict().is_err());
    }

    #[test]
    fn empty_query_tab_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let store = TabStore::with_path(dir.path().join("tabs.json"));
        let session = PersistedTabs {
            tabs: vec![PersistedTab {
                connection_id: "c".into(),
                kind: TabKind::Query { sql: String::new() },
            }],
            active: Some(0),
        };
        store.save(&session).unwrap();
        assert_eq!(store.load().unwrap(), session);
    }
}
