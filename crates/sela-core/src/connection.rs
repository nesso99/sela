use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

use crate::types::Engine;

/// SSL/TLS mode for Postgres and MySQL. Ignored for SQLite.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TlsMode {
    /// Plain TCP only (no TLS).
    #[default]
    Disable,
    /// Try TLS first; fall back to plain if the server refuses.
    Prefer,
    /// Require TLS; do not verify the server certificate.
    Require,
    /// Require TLS and verify the server certificate against a CA.
    VerifyCa,
    /// Require TLS, verify the CA, and check the server hostname.
    VerifyFull,
}

impl TlsMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Disable => "disable",
            Self::Prefer => "prefer",
            Self::Require => "require",
            Self::VerifyCa => "verify-ca",
            Self::VerifyFull => "verify-full",
        }
    }

    pub fn all() -> &'static [TlsMode] {
        &[
            Self::Disable,
            Self::Prefer,
            Self::Require,
            Self::VerifyCa,
            Self::VerifyFull,
        ]
    }
}

/// Per-connection TLS settings. Paths are plain filesystem paths (not secrets).
///
/// Passphrase-protected client keys are not supported yet (sqlx/rustls); when
/// that lands, any passphrase will go to the OS keychain — never JSON.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TlsSettings {
    #[serde(default)]
    pub mode: TlsMode,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub ca_path: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub client_cert_path: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub client_key_path: String,
}

impl TlsSettings {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Map a legacy v1 boolean `ssl` into TLS settings when `tls` is absent.
    ///
    /// `ssl: true` → require (same as the old checkbox). `ssl: false` / missing →
    /// disable (the previous default with the checkbox off). This is not a file
    /// migration: the next save writes `schema_version` 2 with an explicit `tls`
    /// object (or omits it when still default).
    pub fn from_legacy_ssl(ssl: bool) -> Self {
        if ssl {
            Self {
                mode: TlsMode::Require,
                ..Self::default()
            }
        } else {
            Self::default()
        }
    }
}

fn default_true() -> bool {
    true
}

/// A connection Sela can reopen. The password is not part of this value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SavedConnection {
    pub id: String,
    pub name: String,
    pub engine: Engine,
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub database: String,
    #[serde(default)]
    pub file_path: String,
    /// TLS options for Postgres/MySQL. Defaults when absent (v1 files load fine).
    #[serde(default, skip_serializing_if = "TlsSettings::is_default")]
    pub tls: TlsSettings,
    /// When false, editor runs on this connection are not written to history.
    #[serde(default = "default_true")]
    pub record_history: bool,
}

impl<'de> Deserialize<'de> for SavedConnection {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            id: String,
            name: String,
            engine: Engine,
            #[serde(default)]
            host: String,
            #[serde(default)]
            port: u16,
            #[serde(default)]
            username: String,
            #[serde(default)]
            database: String,
            #[serde(default)]
            file_path: String,
            /// Present only in schema v2+ files.
            #[serde(default)]
            tls: Option<TlsSettings>,
            /// Legacy v1 boolean. Used only when `tls` is absent.
            #[serde(default)]
            ssl: Option<bool>,
            /// Missing field means record (M1-5). No schema_version bump.
            #[serde(default = "default_true")]
            record_history: bool,
        }

        let raw = Raw::deserialize(deserializer)?;
        let tls = match raw.tls {
            Some(tls) => tls,
            None => TlsSettings::from_legacy_ssl(raw.ssl.unwrap_or(false)),
        };
        Ok(Self {
            id: raw.id,
            name: raw.name,
            engine: raw.engine,
            host: raw.host,
            port: raw.port,
            username: raw.username,
            database: raw.database,
            file_path: raw.file_path,
            tls,
            record_history: raw.record_history,
        })
    }
}

impl SavedConnection {
    pub fn new(engine: Engine) -> Self {
        // Dialog / in-memory default for new network connections is Prefer.
        // Serde default for a missing `tls` object stays Disable via
        // `TlsMode::default` / `TlsSettings::default` — do not change those.
        let tls = match engine {
            Engine::Postgres | Engine::Mysql => TlsSettings {
                mode: TlsMode::Prefer,
                ..TlsSettings::default()
            },
            Engine::Sqlite => TlsSettings::default(),
        };
        Self {
            id: Uuid::new_v4().to_string(),
            name: String::new(),
            port: engine.default_port(),
            engine,
            host: "127.0.0.1".into(),
            username: String::new(),
            database: String::new(),
            file_path: String::new(),
            tls,
            record_history: true,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("Give the connection a name.".into());
        }
        match self.engine {
            Engine::Sqlite => {
                if self.file_path.trim().is_empty() {
                    return Err("Choose a SQLite file path.".into());
                }
            }
            Engine::Postgres | Engine::Mysql => {
                if self.host.trim().is_empty() {
                    return Err("Host is required.".into());
                }
                if self.port == 0 {
                    return Err("Port must be between 1 and 65535.".into());
                }
                if self.username.trim().is_empty() {
                    return Err("User is required.".into());
                }
                if self.database.trim().is_empty() {
                    return Err("Database name is required.".into());
                }
                let has_cert = !self.tls.client_cert_path.trim().is_empty();
                let has_key = !self.tls.client_key_path.trim().is_empty();
                if has_cert != has_key {
                    return Err(
                        "Client certificate and client key paths must both be set, or both empty."
                            .into(),
                    );
                }
                if matches!(self.tls.mode, TlsMode::VerifyCa | TlsMode::VerifyFull)
                    && self.tls.ca_path.trim().is_empty()
                {
                    return Err(
                        "CA certificate path is required for verify-ca and verify-full.".into(),
                    );
                }
            }
        }
        Ok(())
    }

    pub fn endpoint_label(&self) -> String {
        match self.engine {
            Engine::Sqlite => self.file_path.clone(),
            Engine::Postgres | Engine::Mysql => {
                format!(
                    "{}@{}:{}/{}",
                    self.username, self.host, self.port, self.database
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tls_mode_serde_kebab() {
        let mode = TlsMode::VerifyFull;
        let json = serde_json::to_string(&mode).unwrap();
        assert_eq!(json, "\"verify-full\"");
        let back: TlsMode = serde_json::from_str(&json).unwrap();
        assert_eq!(back, TlsMode::VerifyFull);
    }

    #[test]
    fn missing_tls_and_ssl_defaults_to_disable() {
        let raw = r#"{
            "id": "c1",
            "name": "Local",
            "engine": "postgres",
            "host": "127.0.0.1",
            "port": 5432,
            "username": "sela",
            "database": "sample"
        }"#;
        let conn: SavedConnection = serde_json::from_str(raw).unwrap();
        assert_eq!(conn.tls.mode, TlsMode::Disable);
        assert!(conn.tls.ca_path.is_empty());
        assert!(conn.record_history);
    }

    #[test]
    fn record_history_false_round_trips() {
        let raw = r#"{
            "id": "c1",
            "name": "Quiet",
            "engine": "sqlite",
            "file_path": "/tmp/a.sqlite",
            "record_history": false
        }"#;
        let conn: SavedConnection = serde_json::from_str(raw).unwrap();
        assert!(!conn.record_history);
        let text = serde_json::to_string(&conn).unwrap();
        assert!(text.contains("\"record_history\":false"));
    }

    #[test]
    fn new_network_connection_defaults_tls_prefer() {
        assert_eq!(
            SavedConnection::new(Engine::Postgres).tls.mode,
            TlsMode::Prefer
        );
        assert_eq!(
            SavedConnection::new(Engine::Mysql).tls.mode,
            TlsMode::Prefer
        );
        assert_eq!(
            SavedConnection::new(Engine::Sqlite).tls.mode,
            TlsMode::Disable
        );
        // Serde default must stay Disable even though new() uses Prefer.
        assert_eq!(TlsMode::default(), TlsMode::Disable);
        assert_eq!(TlsSettings::default().mode, TlsMode::Disable);
    }

    #[test]
    fn legacy_ssl_true_maps_to_require() {
        let raw = r#"{
            "id": "c1",
            "name": "Secure",
            "engine": "postgres",
            "host": "127.0.0.1",
            "port": 5432,
            "username": "sela",
            "database": "sample",
            "ssl": true
        }"#;
        let conn: SavedConnection = serde_json::from_str(raw).unwrap();
        assert_eq!(conn.tls.mode, TlsMode::Require);
        assert!(conn.tls.ca_path.is_empty());
    }

    #[test]
    fn legacy_ssl_false_maps_to_disable() {
        let raw = r#"{
            "id": "c1",
            "name": "Plain",
            "engine": "mysql",
            "host": "127.0.0.1",
            "port": 3306,
            "username": "sela",
            "database": "sample",
            "ssl": false
        }"#;
        let conn: SavedConnection = serde_json::from_str(raw).unwrap();
        assert_eq!(conn.tls.mode, TlsMode::Disable);
    }

    #[test]
    fn explicit_tls_wins_over_legacy_ssl() {
        let raw = r#"{
            "id": "c1",
            "name": "Mixed",
            "engine": "postgres",
            "host": "127.0.0.1",
            "port": 5432,
            "username": "sela",
            "database": "sample",
            "ssl": true,
            "tls": { "mode": "prefer" }
        }"#;
        let conn: SavedConnection = serde_json::from_str(raw).unwrap();
        assert_eq!(conn.tls.mode, TlsMode::Prefer);
    }

    #[test]
    fn serialize_never_writes_legacy_ssl() {
        let mut conn = SavedConnection::new(Engine::Postgres);
        conn.name = "Secure".into();
        conn.username = "sela".into();
        conn.database = "sample".into();
        conn.tls.mode = TlsMode::Require;
        let text = serde_json::to_string(&conn).unwrap();
        assert!(!text.contains("\"ssl\""));
        assert!(text.contains("\"require\"") || text.contains("tls"));
    }

    #[test]
    fn verify_modes_require_ca_path() {
        let mut pg = SavedConnection::new(Engine::Postgres);
        pg.name = "Warehouse".into();
        pg.username = "sela".into();
        pg.database = "sample".into();
        pg.tls.mode = TlsMode::VerifyCa;
        assert!(pg.validate().is_err());
        pg.tls.ca_path = "/tmp/ca.pem".into();
        assert!(pg.validate().is_ok());
    }

    #[test]
    fn client_cert_and_key_must_pair() {
        let mut pg = SavedConnection::new(Engine::Postgres);
        pg.name = "Warehouse".into();
        pg.username = "sela".into();
        pg.database = "sample".into();
        pg.tls.client_cert_path = "/tmp/client.crt".into();
        assert!(pg.validate().is_err());
        pg.tls.client_key_path = "/tmp/client.key".into();
        assert!(pg.validate().is_ok());
    }
}
