use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

use crate::connection_color::{
    default_safety_level_for_color, ConnectionColor, DefaultSafetyLevel,
};
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

fn is_false(value: &bool) -> bool {
    !*value
}

fn default_ssh_port() -> u16 {
    22
}

fn is_default_ssh_port(port: &u16) -> bool {
    *port == 0 || *port == 22
}

/// Expand a leading `~/` using `$HOME` / `USERPROFILE`. Other paths are unchanged.
fn expand_tilde_path(path: &str) -> std::path::PathBuf {
    if path == "~" {
        if let Ok(home) = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")) {
            return std::path::PathBuf::from(home);
        }
    }
    if let Some(rest) = path.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")) {
            return std::path::PathBuf::from(home).join(rest);
        }
    }
    std::path::PathBuf::from(path)
}

/// How the SSH bastion authenticates the client.
///
/// Passwords and key passphrases are never stored on this type — they go to the
/// OS keychain (same path as the database password).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SshAuthMethod {
    /// SSH password (secret store account `{id}:ssh-password`).
    Password,
    /// Private key file; optional passphrase in the secret store (`{id}:ssh-passphrase`).
    /// Default for new SSH rows (UX 10-ssh-tunnel).
    #[default]
    PrivateKey,
    /// Keys from `ssh-agent` (`SSH_AUTH_SOCK` / Pageant).
    Agent,
}

/// Per-connection SSH tunnel settings. Missing JSON field loads as disabled.
///
/// No `schema_version` bump: serde defaults keep v0.2 files loadable (ADR 0006).
/// SSH passwords and key passphrases are never written here.
///
/// Saved JSON stores **resolved** Host/Port/User/key path after an explicit
/// Import from `~/.ssh/config` — connect does not re-read the config file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshSettings {
    /// When false, the driver connects directly (no tunnel).
    #[serde(default, skip_serializing_if = "is_false")]
    pub enabled: bool,
    /// Bastion host (resolved `HostName`, or the alias when none was set).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub host: String,
    /// Bastion port. `0` or `22` omit on save; connect treats omitted as 22.
    #[serde(
        default = "default_ssh_port",
        skip_serializing_if = "is_default_ssh_port"
    )]
    pub port: u16,
    /// Bastion user.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub user: String,
    #[serde(default)]
    pub auth: SshAuthMethod,
    /// Private key path (for [`SshAuthMethod::PrivateKey`]). `~` is expanded at connect.
    /// Accepts legacy JSON key `identity_file`.
    #[serde(
        default,
        alias = "identity_file",
        skip_serializing_if = "String::is_empty"
    )]
    pub private_key_path: String,
}

impl Default for SshSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            host: String::new(),
            port: 22,
            user: String::new(),
            auth: SshAuthMethod::PrivateKey,
            private_key_path: String::new(),
        }
    }
}

impl SshSettings {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// Per-connection write guard (M2-4). Missing JSON field loads as [`SafeMode::Off`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SafeMode {
    /// Writes run immediately. `UPDATE` / `DELETE` without `WHERE` still confirm.
    #[default]
    Off,
    /// Writes confirm before they run.
    Confirm,
    /// Writes are refused. Reads run.
    ReadOnly,
}

impl SafeMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Confirm => "Confirm",
            Self::ReadOnly => "Read-only",
        }
    }

    /// Toolbar segment: `Safe: Off`, `Safe: Confirm`, or `Safe: Read-only`.
    pub fn toolbar_segment(self) -> String {
        format!("Safe: {}", self.label())
    }

    pub fn form_label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Confirm => "Confirm writes",
            Self::ReadOnly => "Read-only",
        }
    }

    pub fn helper(self) -> &'static str {
        match self {
            Self::Off => "Writes run without a confirm. UPDATE and DELETE without WHERE still ask.",
            Self::Confirm => {
                "Writes ask before they run. UPDATE and DELETE without WHERE still ask."
            }
            // docs/design/ux/05-connection-dialog.md Safe mode helper table.
            Self::ReadOnly => "Writes are refused. Reads run as usual.",
        }
    }

    pub fn all() -> &'static [SafeMode] {
        &[Self::Off, Self::Confirm, Self::ReadOnly]
    }

    pub fn is_default(&self) -> bool {
        *self == Self::Off
    }
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
    /// SSH tunnel for Postgres/MySQL. Missing field loads as disabled (v0.2 files).
    #[serde(default, skip_serializing_if = "SshSettings::is_default")]
    pub ssh: SshSettings,
    /// When false, editor runs on this connection are not written to history.
    #[serde(default = "default_true")]
    pub record_history: bool,
    /// Named swatch (`none` omitted on save). Missing field loads as None.
    #[serde(default, skip_serializing_if = "ConnectionColor::is_none")]
    pub color: ConnectionColor,
    /// Optional short tag after the name in the sidebar. Max 16 after trim.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    /// Optional one-level sidebar group. Max 40; `/` rejected.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub group: String,
    /// Write guard for the editor and grid commit. Missing = Off.
    #[serde(default, skip_serializing_if = "SafeMode::is_default")]
    pub safe_mode: SafeMode,
    /// When true, connect asks for a password if none is in the keychain/memory
    /// (Duplicate). Missing field loads as false so trust/peer and empty-password
    /// rows keep connecting after restart.
    #[serde(default, skip_serializing_if = "is_false")]
    pub needs_password: bool,
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
            /// Missing field means disabled (M3-1). No schema_version bump.
            #[serde(default)]
            ssh: SshSettings,
            /// Missing field means record (M1-5). No schema_version bump.
            #[serde(default = "default_true")]
            record_history: bool,
            /// Missing field means none (M2-5). No schema_version bump.
            #[serde(default)]
            color: ConnectionColor,
            #[serde(default)]
            label: String,
            #[serde(default)]
            group: String,
            /// Missing field means Off (M2-4). No schema_version bump.
            #[serde(default)]
            safe_mode: SafeMode,
            /// Missing field means false (pre-Duplicate rows / trust auth).
            #[serde(default)]
            needs_password: bool,
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
            ssh: raw.ssh,
            record_history: raw.record_history,
            color: raw.color,
            label: raw.label,
            group: raw.group,
            safe_mode: raw.safe_mode,
            needs_password: raw.needs_password,
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
            ssh: SshSettings::default(),
            record_history: true,
            color: ConnectionColor::None,
            label: String::new(),
            group: String::new(),
            safe_mode: SafeMode::Off,
            needs_password: false,
        }
    }

    /// Connect should open the password dialog only for rows marked after
    /// Duplicate when no secret is present. Absent secret + false keeps the
    /// empty-password / trust-auth path.
    pub fn should_prompt_for_password(&self, secret: Option<&str>) -> bool {
        self.needs_password && secret.is_none()
    }

    /// Cleared on Save and on a successful connect.
    pub fn clear_needs_password(&mut self) {
        self.needs_password = false;
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("Give the connection a name.".into());
        }
        let label = self.label.trim();
        if label.chars().count() > 16 {
            return Err("Label must be 16 characters or fewer.".into());
        }
        let group = self.group.trim();
        if group.chars().count() > 40 {
            return Err("Group must be 40 characters or fewer.".into());
        }
        if group.contains('/') {
            return Err("Groups are one level. Remove the slash.".into());
        }
        match self.engine {
            Engine::Sqlite => {
                if self.file_path.trim().is_empty() {
                    return Err("Choose a SQLite file path.".into());
                }
                // SSH does not apply to SQLite (file on disk). Reject enabled SSH
                // so a bad save cannot leave a half-configured row.
                if self.ssh.enabled {
                    return Err("SSH tunnel is not used with SQLite.".into());
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
                if self.ssh.enabled {
                    if self.ssh.host.trim().is_empty() {
                        return Err("SSH host is required.".into());
                    }
                    if self.ssh.port == 0 {
                        return Err("SSH port must be a number from 1 to 65535.".into());
                    }
                    if self.ssh.user.trim().is_empty() {
                        return Err("SSH user is required.".into());
                    }
                    if self.ssh.auth == SshAuthMethod::PrivateKey {
                        let path = self.ssh.private_key_path.trim();
                        if path.is_empty() {
                            return Err("Private key path is required.".into());
                        }
                        let expanded = expand_tilde_path(path);
                        if !expanded.is_file() {
                            return Err("Private key was not found.".into());
                        }
                    }
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

/// Safe mode driven by the color swatch on a **new** connection dialog.
///
/// When `user_touched_safe_mode` is false, Red → Confirm writes and any other
/// color → Off. When the user has already picked Safe mode, returns `current`
/// unchanged (color never overwrites an explicit pick). Editing an existing
/// connection does not call this (see docs/design/ux/09-safe-mode.md).
pub fn safe_mode_driven_by_color(
    color: ConnectionColor,
    user_touched_safe_mode: bool,
    current: SafeMode,
) -> SafeMode {
    if user_touched_safe_mode {
        return current;
    }
    match default_safety_level_for_color(color) {
        DefaultSafetyLevel::ConfirmWrites => SafeMode::Confirm,
        DefaultSafetyLevel::Off => SafeMode::Off,
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
    fn missing_safe_mode_defaults_to_off() {
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
        assert_eq!(conn.safe_mode, SafeMode::Off);
        let text = serde_json::to_string(&conn).unwrap();
        assert!(
            !text.contains("safe_mode"),
            "Off must omit safe_mode on save: {text}"
        );
    }

    #[test]
    fn safe_mode_confirm_and_read_only_round_trip() {
        for (mode, wire) in [
            (SafeMode::Confirm, "confirm"),
            (SafeMode::ReadOnly, "read-only"),
        ] {
            let raw = format!(
                r#"{{
                    "id": "c1",
                    "name": "Prod",
                    "engine": "mysql",
                    "host": "127.0.0.1",
                    "port": 3306,
                    "username": "sela",
                    "database": "sample",
                    "safe_mode": "{wire}"
                }}"#
            );
            let conn: SavedConnection = serde_json::from_str(&raw).unwrap();
            assert_eq!(conn.safe_mode, mode);
            let text = serde_json::to_string(&conn).unwrap();
            assert!(text.contains(&format!("\"safe_mode\":\"{wire}\"")));
            let back: SavedConnection = serde_json::from_str(&text).unwrap();
            assert_eq!(back.safe_mode, mode);
        }
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

    #[test]
    fn missing_color_label_group_default_and_round_trip() {
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
        assert_eq!(conn.color, ConnectionColor::None);
        assert!(conn.label.is_empty());
        assert!(conn.group.is_empty());
        let text = serde_json::to_string(&conn).unwrap();
        assert!(!text.contains("\"color\""));
        assert!(!text.contains("\"label\""));
        assert!(!text.contains("\"group\""));

        let mut tagged = conn;
        tagged.color = ConnectionColor::Red;
        tagged.label = "prod".into();
        tagged.group = "Work".into();
        let text = serde_json::to_string(&tagged).unwrap();
        assert!(text.contains("\"color\":\"red\""));
        assert!(text.contains("\"label\":\"prod\""));
        assert!(text.contains("\"group\":\"Work\""));
        let back: SavedConnection = serde_json::from_str(&text).unwrap();
        assert_eq!(back.color, ConnectionColor::Red);
        assert_eq!(back.label, "prod");
        assert_eq!(back.group, "Work");
    }

    #[test]
    fn unknown_color_name_loads_as_none() {
        let raw = r#"{
            "id": "c1",
            "name": "Local",
            "engine": "sqlite",
            "file_path": "/tmp/a.sqlite",
            "color": "hotpink"
        }"#;
        let conn: SavedConnection = serde_json::from_str(raw).unwrap();
        assert_eq!(conn.color, ConnectionColor::None);
    }

    #[test]
    fn label_and_group_validation() {
        let mut c = SavedConnection::new(Engine::Sqlite);
        c.name = "Lib".into();
        c.file_path = "/tmp/a.sqlite".into();
        c.label = "abcdefghijklmnopq".into(); // 17
        assert_eq!(
            c.validate().unwrap_err(),
            "Label must be 16 characters or fewer."
        );
        c.label = "prod".into();
        c.group = "a".repeat(41);
        assert_eq!(
            c.validate().unwrap_err(),
            "Group must be 40 characters or fewer."
        );
        c.group = "Work/Nested".into();
        assert_eq!(
            c.validate().unwrap_err(),
            "Groups are one level. Remove the slash."
        );
        c.group = "Work".into();
        assert!(c.validate().is_ok());
    }

    #[test]
    fn safe_mode_helper_matches_connection_dialog_spec() {
        // docs/design/ux/05-connection-dialog.md Safe mode helper table.
        assert_eq!(
            SafeMode::Off.helper(),
            "Writes run without a confirm. UPDATE and DELETE without WHERE still ask."
        );
        assert_eq!(
            SafeMode::Confirm.helper(),
            "Writes ask before they run. UPDATE and DELETE without WHERE still ask."
        );
        assert_eq!(
            SafeMode::ReadOnly.helper(),
            "Writes are refused. Reads run as usual."
        );
    }

    #[test]
    fn new_connection_red_defaults_to_confirm_until_explicit_pick() {
        // New dialog: Red → Confirm writes while Safe mode is untouched.
        assert_eq!(
            safe_mode_driven_by_color(ConnectionColor::Red, false, SafeMode::Off),
            SafeMode::Confirm
        );
        // Leaving Red restores Off while still untouched.
        assert_eq!(
            safe_mode_driven_by_color(ConnectionColor::Blue, false, SafeMode::Confirm),
            SafeMode::Off
        );
        // Explicit Safe mode pick is respected; color no longer drives the level.
        assert_eq!(
            safe_mode_driven_by_color(ConnectionColor::Red, true, SafeMode::ReadOnly),
            SafeMode::ReadOnly
        );
        assert_eq!(
            safe_mode_driven_by_color(ConnectionColor::None, true, SafeMode::Confirm),
            SafeMode::Confirm
        );
    }

    #[test]
    fn empty_password_after_restart_does_not_prompt() {
        // Intentional empty password (trust/peer): keychain has no item after
        // restart (`get_optional` → None). Without needs_password, connect with "".
        let conn = SavedConnection::new(Engine::Postgres);
        assert!(!conn.needs_password);
        assert!(!conn.should_prompt_for_password(None));
        assert!(!conn.should_prompt_for_password(Some("")));
    }

    #[test]
    fn save_clears_needs_password() {
        let mut conn = SavedConnection::new(Engine::Mysql);
        conn.needs_password = true;
        conn.clear_needs_password();
        assert!(!conn.needs_password);
        assert!(!conn.should_prompt_for_password(None));
    }

    #[test]
    fn needs_password_missing_loads_false_and_omits_on_save() {
        let raw = r#"{
            "id": "c1",
            "name": "Trust",
            "engine": "postgres",
            "host": "127.0.0.1",
            "port": 5432,
            "username": "sela",
            "database": "sample"
        }"#;
        let conn: SavedConnection = serde_json::from_str(raw).unwrap();
        assert!(!conn.needs_password);
        let text = serde_json::to_string(&conn).unwrap();
        assert!(
            !text.contains("needs_password"),
            "false must omit needs_password on save: {text}"
        );

        let mut flagged = conn;
        flagged.needs_password = true;
        let flagged_text = serde_json::to_string(&flagged).unwrap();
        assert!(flagged_text.contains("\"needs_password\":true"));
        let back: SavedConnection = serde_json::from_str(&flagged_text).unwrap();
        assert!(back.needs_password);
    }

    #[test]
    fn missing_ssh_defaults_disabled_and_omits_on_save() {
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
        assert!(!conn.ssh.enabled);
        assert_eq!(conn.ssh.auth, SshAuthMethod::PrivateKey);
        let text = serde_json::to_string(&conn).unwrap();
        assert!(
            !text.contains("\"ssh\""),
            "default SSH must omit on save: {text}"
        );
        assert!(
            !text.to_ascii_lowercase().contains("password"),
            "SSH must never put a password field in JSON: {text}"
        );
    }

    #[test]
    fn ssh_settings_round_trip_without_secrets() {
        let mut conn = SavedConnection::new(Engine::Postgres);
        conn.name = "Bastion".into();
        conn.username = "sela".into();
        conn.database = "sample".into();
        conn.ssh = SshSettings {
            enabled: true,
            host: "jump".into(),
            port: 2222,
            user: "tunnel".into(),
            auth: SshAuthMethod::PrivateKey,
            private_key_path: "~/.ssh/id_ed25519".into(),
        };
        let text = serde_json::to_string(&conn).unwrap();
        assert!(text.contains("\"enabled\":true"));
        assert!(text.contains("\"auth\":\"private-key\""));
        assert!(text.contains("\"private_key_path\""));
        assert!(!text.contains("identity_file"));
        assert!(!text.to_ascii_lowercase().contains("passphrase"));
        assert!(!text.to_ascii_lowercase().contains("\"password\""));
        let back: SavedConnection = serde_json::from_str(&text).unwrap();
        assert_eq!(back.ssh, conn.ssh);
    }

    #[test]
    // SSH-20 unit: duplicate keeps SSH host settings; secrets are not on the type.
    fn ssh20_duplicate_copies_ssh_host_settings_not_secrets() {
        use crate::duplicate_connection;
        let mut source = SavedConnection::new(Engine::Postgres);
        source.name = "Prod".into();
        source.username = "sela".into();
        source.database = "sample".into();
        source.ssh = SshSettings {
            enabled: true,
            host: "bastion.example".into(),
            port: 2222,
            user: "tunnel".into(),
            auth: SshAuthMethod::Password,
            private_key_path: String::new(),
        };
        let copy = duplicate_connection(&source, ["Prod"]);
        assert_ne!(copy.id, source.id);
        assert_eq!(copy.ssh, source.ssh);
        let text = serde_json::to_string(&copy).unwrap();
        assert!(!text.to_ascii_lowercase().contains("passphrase"));
        assert!(!text.contains("ssh-password"));
    }

    #[test]
    fn legacy_identity_file_json_loads_as_private_key_path() {
        let raw = r#"{
            "id": "c1",
            "name": "Local",
            "engine": "postgres",
            "host": "127.0.0.1",
            "port": 5432,
            "username": "sela",
            "database": "sample",
            "ssh": {
                "enabled": true,
                "host": "jump",
                "user": "tunnel",
                "auth": "private-key",
                "identity_file": "~/.ssh/id_ed25519"
            }
        }"#;
        let conn: SavedConnection = serde_json::from_str(raw).unwrap();
        assert_eq!(conn.ssh.private_key_path, "~/.ssh/id_ed25519");
    }

    #[test]
    fn sqlite_rejects_enabled_ssh() {
        let mut c = SavedConnection::new(Engine::Sqlite);
        c.name = "Lib".into();
        c.file_path = "/tmp/a.sqlite".into();
        c.ssh.enabled = true;
        c.ssh.host = "bastion".into();
        assert_eq!(
            c.validate().unwrap_err(),
            "SSH tunnel is not used with SQLite."
        );
    }

    #[test]
    fn ssh_enabled_requires_host_user_and_key_when_private_key() {
        let mut c = SavedConnection::new(Engine::Postgres);
        c.name = "Remote".into();
        c.username = "sela".into();
        c.database = "sample".into();
        c.ssh.enabled = true;
        assert_eq!(c.validate().unwrap_err(), "SSH host is required.");
        c.ssh.host = "bastion".into();
        assert_eq!(c.validate().unwrap_err(), "SSH user is required.");
        c.ssh.user = "tunnel".into();
        c.ssh.auth = SshAuthMethod::PrivateKey;
        assert_eq!(c.validate().unwrap_err(), "Private key path is required.");
        c.ssh.auth = SshAuthMethod::Password;
        assert!(c.validate().is_ok());
    }
}
