use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use sela_core::SavedConnection;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("could not read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not write {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("connections file {path} is not valid JSON: {source}")]
    Parse {
        path: PathBuf,
        source: serde_json::Error,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct FileShape {
    #[serde(default)]
    connections: Vec<SavedConnection>,
}

#[derive(Debug, Clone)]
pub struct ConfigStore {
    path: PathBuf,
}

impl ConfigStore {
    pub fn default_path() -> PathBuf {
        config_dir().join("connections.json")
    }

    pub fn with_path(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<Vec<SavedConnection>, ConfigError> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let bytes = fs::read(&self.path).map_err(|source| ConfigError::Read {
            path: self.path.clone(),
            source,
        })?;
        let file: FileShape =
            serde_json::from_slice(&bytes).map_err(|source| ConfigError::Parse {
                path: self.path.clone(),
                source,
            })?;
        Ok(file.connections)
    }

    pub fn save(&self, connections: &[SavedConnection]) -> Result<(), ConfigError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|source| ConfigError::Write {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let body = serde_json::to_vec_pretty(&FileShape {
            connections: connections.to_vec(),
        })
        .expect("connection config is always serializable");
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

pub fn config_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("SELA_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        #[cfg(target_os = "macos")]
        {
            return home.join("Library/Application Support/Sela");
        }
        #[cfg(not(target_os = "macos"))]
        {
            if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
                return PathBuf::from(xdg).join("sela");
            }
            return home.join(".config").join("sela");
        }
    }
    if let Some(appdata) = std::env::var_os("APPDATA") {
        return PathBuf::from(appdata).join("Sela");
    }
    PathBuf::from(".sela")
}

#[cfg(test)]
mod tests {
    use super::*;
    use sela_core::{Engine, SavedConnection};

    #[test]
    fn round_trip_omits_any_password_field() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.json");
        let seeded = r#"{
            "connections": [{
                "id": "c1",
                "name": "Local",
                "engine": "postgres",
                "host": "127.0.0.1",
                "port": 5432,
                "username": "sela",
                "database": "sample",
                "password": "should-not-survive",
                "ssl": false
            }]
        }"#;
        fs::write(&path, seeded).unwrap();

        let store = ConfigStore::with_path(&path);
        let loaded = store.load().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "Local");
        assert_eq!(loaded[0].engine, Engine::Postgres);
        assert_eq!(loaded[0].database, "sample");

        store.save(&loaded).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.to_ascii_lowercase().contains("password"));
        assert!(!text.contains("should-not-survive"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn missing_file_is_an_empty_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let store = ConfigStore::with_path(dir.path().join("missing.json"));
        assert!(store.load().unwrap().is_empty());
    }

    #[test]
    fn sqlite_connection_keeps_its_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.json");
        let mut connection = SavedConnection::new(Engine::Sqlite);
        connection.name = "Library".into();
        connection.file_path = "/tmp/library.sqlite".into();
        let store = ConfigStore::with_path(&path);
        store.save(std::slice::from_ref(&connection)).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded[0].file_path, "/tmp/library.sqlite");
        assert_eq!(loaded[0].engine, Engine::Sqlite);
    }

    #[test]
    fn validation_requires_the_fields_each_engine_uses() {
        let mut sqlite = SavedConnection::new(Engine::Sqlite);
        sqlite.name = "Notes".into();
        assert!(sqlite.validate().is_err());
        sqlite.file_path = "notes.sqlite".into();
        assert!(sqlite.validate().is_ok());

        let mut pg = SavedConnection::new(Engine::Postgres);
        pg.name = "Warehouse".into();
        pg.username = "sela".into();
        pg.database = "sample".into();
        assert!(pg.validate().is_ok());
        pg.port = 0;
        assert!(pg.validate().is_err());
    }
}
