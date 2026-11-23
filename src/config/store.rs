use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::model::SavedConnection;

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
        connections_file(&config_dir())
    }

    fn legacy_path() -> PathBuf {
        connections_file(&legacy_config_dir())
    }

    /// Open the Sela connections file, copying a Birch file forward when needed.
    ///
    /// The original Birch file is left in place. A copy error is returned beside
    /// the store so the window can still open.
    pub fn open_default() -> (Self, Option<ConfigError>) {
        let path = Self::default_path();
        let error = migrate_config_file(&path, &Self::legacy_path()).err();
        (Self::with_path(path), error)
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

const CONNECTIONS_FILE: &str = "connections.json";

#[derive(Clone, Copy)]
enum AppName {
    Sela,
    Birch,
}

struct DirEnv<'a> {
    override_dir: Option<&'a Path>,
    home: Option<&'a Path>,
    xdg: Option<&'a Path>,
    appdata: Option<&'a Path>,
}

fn resolve_dir(name: AppName, env: DirEnv<'_>) -> PathBuf {
    if let Some(dir) = env.override_dir {
        return dir.to_path_buf();
    }
    if let Some(home) = env.home {
        #[cfg(target_os = "macos")]
        {
            // `xdg` is only consulted on the non-macOS branch. Mention it so the
            // field stays live when this crate is built for macOS.
            let _xdg = env.xdg;
            let titled = match name {
                AppName::Sela => "Sela",
                AppName::Birch => "Birch",
            };
            return home.join("Library/Application Support").join(titled);
        }
        #[cfg(not(target_os = "macos"))]
        {
            let unix_name = match name {
                AppName::Sela => "sela",
                AppName::Birch => "birch",
            };
            if let Some(xdg) = env.xdg {
                return xdg.join(unix_name);
            }
            return home.join(".config").join(unix_name);
        }
    }
    if let Some(appdata) = env.appdata {
        let titled = match name {
            AppName::Sela => "Sela",
            AppName::Birch => "Birch",
        };
        return appdata.join(titled);
    }
    PathBuf::from(match name {
        AppName::Sela => ".sela",
        AppName::Birch => ".birch",
    })
}

fn dir_env<'a>(
    override_dir: &'a Option<PathBuf>,
    home: &'a Option<PathBuf>,
    xdg: &'a Option<PathBuf>,
    appdata: &'a Option<PathBuf>,
) -> DirEnv<'a> {
    DirEnv {
        override_dir: override_dir.as_deref(),
        home: home.as_deref(),
        xdg: xdg.as_deref(),
        appdata: appdata.as_deref(),
    }
}

pub fn config_dir() -> PathBuf {
    let override_dir = std::env::var_os("SELA_CONFIG_DIR").map(PathBuf::from);
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let xdg = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
    let appdata = std::env::var_os("APPDATA").map(PathBuf::from);
    resolve_dir(AppName::Sela, dir_env(&override_dir, &home, &xdg, &appdata))
}

fn legacy_config_dir() -> PathBuf {
    let override_dir = std::env::var_os("BIRCH_CONFIG_DIR").map(PathBuf::from);
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let xdg = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
    let appdata = std::env::var_os("APPDATA").map(PathBuf::from);
    resolve_dir(
        AppName::Birch,
        dir_env(&override_dir, &home, &xdg, &appdata),
    )
}

fn connections_file(dir: &Path) -> PathBuf {
    dir.join(CONNECTIONS_FILE)
}

/// Copy Birch's connections file to `new_path` when Sela's file is absent.
///
/// The Birch file is not deleted. Returns whether a copy was made.
fn migrate_config_file(new_path: &Path, old_path: &Path) -> Result<bool, ConfigError> {
    if new_path == old_path || new_path.exists() || !old_path.exists() {
        return Ok(false);
    }
    if let Some(parent) = new_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|source| ConfigError::Write {
                path: parent.to_path_buf(),
                source,
            })?;
        }
    }
    let tmp = new_path.with_extension("json.migrate");
    fs::copy(old_path, &tmp).map_err(|source| ConfigError::Write {
        path: tmp.clone(),
        source,
    })?;
    #[cfg(unix)]
    copy_permissions(old_path, &tmp)?;
    if let Err(source) = fs::rename(&tmp, new_path) {
        let _ = fs::remove_file(&tmp);
        return Err(ConfigError::Write {
            path: new_path.to_path_buf(),
            source,
        });
    }
    Ok(true)
}

#[cfg(unix)]
fn copy_permissions(from: &Path, to: &Path) -> Result<(), ConfigError> {
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::metadata(from)
        .map_err(|source| ConfigError::Read {
            path: from.to_path_buf(),
            source,
        })?
        .permissions()
        .mode();
    let mut perms = fs::metadata(to)
        .map_err(|source| ConfigError::Read {
            path: to.to_path_buf(),
            source,
        })?
        .permissions();
    perms.set_mode(mode & 0o777);
    fs::set_permissions(to, perms).map_err(|source| ConfigError::Write {
        path: to.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::SavedConnection;
    use crate::db::Engine;

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
                "username": "birch",
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
        pg.username = "birch".into();
        pg.database = "sample".into();
        assert!(pg.validate().is_ok());
        pg.port = 0;
        assert!(pg.validate().is_err());
    }

    fn sample_env<'a>(
        override_dir: Option<&'a Path>,
        home: Option<&'a Path>,
        xdg: Option<&'a Path>,
        appdata: Option<&'a Path>,
    ) -> DirEnv<'a> {
        DirEnv {
            override_dir,
            home,
            xdg,
            appdata,
        }
    }

    #[test]
    fn directories_follow_the_platform_and_keep_the_birch_names_for_migration() {
        let custom = PathBuf::from("/tmp/custom");
        let home = PathBuf::from("/home/ada");
        let xdg = PathBuf::from("/xdg");
        let appdata = PathBuf::from("/AppData/Roaming");

        assert_eq!(
            resolve_dir(
                AppName::Sela,
                sample_env(Some(&custom), Some(&home), Some(&xdg), Some(&appdata)),
            ),
            custom
        );
        assert_eq!(
            resolve_dir(AppName::Sela, sample_env(None, None, None, None)),
            PathBuf::from(".sela")
        );
        assert_eq!(
            resolve_dir(AppName::Birch, sample_env(None, None, None, None)),
            PathBuf::from(".birch")
        );
        assert_eq!(
            resolve_dir(AppName::Sela, sample_env(None, None, Some(&xdg), None)),
            PathBuf::from(".sela")
        );

        let sela_appdata = resolve_dir(AppName::Sela, sample_env(None, None, None, Some(&appdata)));
        let birch_appdata =
            resolve_dir(AppName::Birch, sample_env(None, None, None, Some(&appdata)));
        assert_eq!(sela_appdata, appdata.join("Sela"));
        assert_eq!(birch_appdata, appdata.join("Birch"));

        let sela_home = resolve_dir(
            AppName::Sela,
            sample_env(None, Some(&home), Some(&xdg), Some(&appdata)),
        );
        let birch_home = resolve_dir(
            AppName::Birch,
            sample_env(None, Some(&home), Some(&xdg), Some(&appdata)),
        );
        #[cfg(target_os = "macos")]
        {
            assert_eq!(sela_home, home.join("Library/Application Support/Sela"));
            assert_eq!(birch_home, home.join("Library/Application Support/Birch"));
        }
        #[cfg(not(target_os = "macos"))]
        {
            assert_eq!(sela_home, xdg.join("sela"));
            assert_eq!(birch_home, xdg.join("birch"));
        }

        let sela_default = resolve_dir(AppName::Sela, sample_env(None, Some(&home), None, None));
        let birch_default = resolve_dir(AppName::Birch, sample_env(None, Some(&home), None, None));
        #[cfg(target_os = "macos")]
        {
            assert_eq!(sela_default, home.join("Library/Application Support/Sela"));
            assert_eq!(
                birch_default,
                home.join("Library/Application Support/Birch")
            );
        }
        #[cfg(not(target_os = "macos"))]
        {
            assert_eq!(sela_default, home.join(".config/sela"));
            assert_eq!(birch_default, home.join(".config/birch"));
        }
        assert_eq!(
            connections_file(&sela_default).file_name().unwrap(),
            "connections.json"
        );
        assert_eq!(
            connections_file(&birch_default).file_name().unwrap(),
            "connections.json"
        );
    }

    #[test]
    fn copies_birch_config_without_deleting_it() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("birch").join("connections.json");
        let new = dir.path().join("sela").join("connections.json");
        fs::create_dir_all(old.parent().unwrap()).unwrap();
        let body = b"{\"connections\":[]}\n";
        fs::write(&old, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&old).unwrap().permissions();
            perms.set_mode(0o600);
            fs::set_permissions(&old, perms).unwrap();
        }

        assert!(migrate_config_file(&new, &old).unwrap());
        assert_eq!(fs::read(&old).unwrap(), body);
        assert_eq!(fs::read(&new).unwrap(), body);
        let migrated_dir: Vec<_> = fs::read_dir(new.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(
            migrated_dir,
            vec![std::ffi::OsString::from("connections.json")]
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&new).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert!(!migrate_config_file(&new, &old).unwrap());
    }

    #[test]
    fn does_not_overwrite_an_existing_sela_config() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("old.json");
        let new = dir.path().join("new.json");
        fs::write(&old, b"old").unwrap();
        fs::write(&new, b"new").unwrap();
        assert!(!migrate_config_file(&new, &old).unwrap());
        assert_eq!(fs::read(&old).unwrap(), b"old");
        assert_eq!(fs::read(&new).unwrap(), b"new");
    }

    #[test]
    fn missing_birch_config_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("missing.json");
        let new = dir.path().join("sela").join("connections.json");
        assert!(!migrate_config_file(&new, &old).unwrap());
        assert!(!new.exists());
    }

    #[test]
    fn same_path_is_not_copied_onto_itself() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.json");
        fs::write(&path, b"same\n").unwrap();
        assert!(!migrate_config_file(&path, &path).unwrap());
        assert_eq!(fs::read(&path).unwrap(), b"same\n");
    }
}
