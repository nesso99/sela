use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use sela_core::SavedConnection;

/// Current on-disk schema for `connections.json`.
///
/// Bumped to 2 for TLS fields. There is **no migration code**: v1 files still
/// load because new fields have serde defaults, and the next save writes v2.
pub const SCHEMA_VERSION: u32 = 2;

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
    #[error("could not open query history at {path}: {source}")]
    History {
        path: PathBuf,
        source: rusqlite::Error,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FileShape {
    #[serde(default = "default_schema_version")]
    schema_version: u32,
    #[serde(default)]
    connections: Vec<SavedConnection>,
}

fn default_schema_version() -> u32 {
    // Missing field means a pre-v2 file; load succeeds via field defaults.
    1
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
            schema_version: SCHEMA_VERSION,
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

pub(crate) fn write_private(path: &Path, body: &[u8]) -> std::io::Result<()> {
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
    use sela_core::{Engine, SavedConnection, TlsMode, TlsSettings};

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
        assert_eq!(loaded[0].tls.mode, TlsMode::Disable);

        store.save(&loaded).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.to_ascii_lowercase().contains("password"));
        assert!(!text.contains("should-not-survive"));
        assert!(text.contains("\"schema_version\": 2"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn safe_mode_round_trip_and_missing_defaults_off() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.json");
        // Legacy file without safe_mode still loads as Off.
        let seeded = r#"{
            "schema_version": 2,
            "connections": [{
                "id": "c1",
                "name": "Local",
                "engine": "postgres",
                "host": "127.0.0.1",
                "port": 5432,
                "username": "sela",
                "database": "sample"
            }]
        }"#;
        fs::write(&path, seeded).unwrap();
        let store = ConfigStore::with_path(&path);
        let mut loaded = store.load().unwrap();
        assert_eq!(loaded[0].safe_mode, sela_core::SafeMode::Off);

        loaded[0].safe_mode = sela_core::SafeMode::Confirm;
        store.save(&loaded).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("\"safe_mode\": \"confirm\"")
                || text.contains("\"safe_mode\":\"confirm\"")
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&text).unwrap()["schema_version"],
            2
        );

        let reloaded = store.load().unwrap();
        assert_eq!(reloaded[0].safe_mode, sela_core::SafeMode::Confirm);

        loaded[0].safe_mode = sela_core::SafeMode::Off;
        store.save(&loaded).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("safe_mode"), "Off omits safe_mode: {text}");
    }

    #[test]
    fn loads_v1_file_without_schema_version_or_tls() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.json");
        // Legacy v1 shape: boolean `ssl`, no schema_version, no `tls` object.
        let seeded = r#"{
            "connections": [{
                "id": "legacy",
                "name": "Legacy PG",
                "engine": "postgres",
                "host": "10.0.0.1",
                "port": 5432,
                "username": "app",
                "database": "orders",
                "ssl": true
            }]
        }"#;
        fs::write(&path, seeded).unwrap();

        let store = ConfigStore::with_path(&path);
        let loaded = store.load().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "Legacy PG");
        // No file migration: legacy ssl:true is honored in memory as require.
        assert_eq!(loaded[0].tls.mode, TlsMode::Require);

        store.save(&loaded).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(saved["schema_version"], 2);
        // Old boolean `ssl` must not be rewritten; mode lands under `tls`.
        assert!(saved["connections"][0].get("ssl").is_none());
        assert_eq!(saved["connections"][0]["tls"]["mode"], "require");
        assert!(!text.to_ascii_lowercase().contains("passphrase"));
        assert!(!text.to_ascii_lowercase().contains("password"));
    }

    #[test]
    fn loads_v1_ssl_false_as_disable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.json");
        let seeded = r#"{
            "connections": [{
                "id": "plain",
                "name": "Plain PG",
                "engine": "postgres",
                "host": "10.0.0.1",
                "port": 5432,
                "username": "app",
                "database": "orders",
                "ssl": false
            }]
        }"#;
        fs::write(&path, seeded).unwrap();
        let loaded = ConfigStore::with_path(&path).load().unwrap();
        assert_eq!(loaded[0].tls.mode, TlsMode::Disable);
    }

    #[test]
    fn v2_round_trip_preserves_tls_paths_not_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.json");
        let mut connection = SavedConnection::new(Engine::Mysql);
        connection.name = "Prod".into();
        connection.host = "db.example.com".into();
        connection.port = 3306;
        connection.username = "app".into();
        connection.database = "orders".into();
        connection.tls = TlsSettings {
            mode: TlsMode::VerifyFull,
            ca_path: "/etc/ssl/certs/ca.pem".into(),
            client_cert_path: "/etc/ssl/certs/client.crt".into(),
            client_key_path: "/etc/ssl/private/client.key".into(),
        };

        let store = ConfigStore::with_path(&path);
        store.save(std::slice::from_ref(&connection)).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"schema_version\": 2"));
        assert!(text.contains("verify-full"));
        assert!(text.contains("/etc/ssl/certs/ca.pem"));
        // Secrets must never appear in the JSON file.
        assert!(!text.to_ascii_lowercase().contains("password"));
        assert!(!text.to_ascii_lowercase().contains("passphrase"));
        assert!(!text.contains("hunter2"));

        let loaded = store.load().unwrap();
        assert_eq!(loaded[0].tls.mode, TlsMode::VerifyFull);
        assert_eq!(loaded[0].tls.ca_path, "/etc/ssl/certs/ca.pem");
        assert_eq!(loaded[0].tls.client_cert_path, "/etc/ssl/certs/client.crt");
        assert_eq!(loaded[0].tls.client_key_path, "/etc/ssl/private/client.key");
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
