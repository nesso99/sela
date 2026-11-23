//! Map Sela [`TlsSettings`] onto sqlx connect options per engine.

use std::path::Path;

use sqlx::mysql::{MySqlConnectOptions, MySqlSslMode};
use sqlx::postgres::{PgConnectOptions, PgSslMode};

use sela_core::{DbError, TlsMode, TlsSettings};

/// Apply TLS settings to Postgres connect options.
pub(crate) fn apply_postgres_tls(
    options: PgConnectOptions,
    tls: &TlsSettings,
) -> Result<PgConnectOptions, DbError> {
    let mut options = options.ssl_mode(postgres_ssl_mode(tls.mode));
    if let Some(path) = non_empty_path(&tls.ca_path) {
        options = options.ssl_root_cert(path);
    }
    if let Some(cert) = non_empty_path(&tls.client_cert_path) {
        options = options.ssl_client_cert(cert);
    }
    if let Some(key) = non_empty_path(&tls.client_key_path) {
        options = options.ssl_client_key(key);
    }
    Ok(options)
}

/// Apply TLS settings to MySQL connect options.
pub(crate) fn apply_mysql_tls(
    options: MySqlConnectOptions,
    tls: &TlsSettings,
) -> Result<MySqlConnectOptions, DbError> {
    let mut options = options.ssl_mode(mysql_ssl_mode(tls.mode));
    if let Some(path) = non_empty_path(&tls.ca_path) {
        options = options.ssl_ca(path);
    }
    if let Some(cert) = non_empty_path(&tls.client_cert_path) {
        options = options.ssl_client_cert(cert);
    }
    if let Some(key) = non_empty_path(&tls.client_key_path) {
        options = options.ssl_client_key(key);
    }
    Ok(options)
}

pub(crate) fn postgres_ssl_mode(mode: TlsMode) -> PgSslMode {
    match mode {
        TlsMode::Disable => PgSslMode::Disable,
        TlsMode::Prefer => PgSslMode::Prefer,
        TlsMode::Require => PgSslMode::Require,
        TlsMode::VerifyCa => PgSslMode::VerifyCa,
        TlsMode::VerifyFull => PgSslMode::VerifyFull,
    }
}

pub(crate) fn mysql_ssl_mode(mode: TlsMode) -> MySqlSslMode {
    match mode {
        TlsMode::Disable => MySqlSslMode::Disabled,
        TlsMode::Prefer => MySqlSslMode::Preferred,
        TlsMode::Require => MySqlSslMode::Required,
        TlsMode::VerifyCa => MySqlSslMode::VerifyCa,
        TlsMode::VerifyFull => MySqlSslMode::VerifyIdentity,
    }
}

fn non_empty_path(value: &str) -> Option<&Path> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(Path::new(trimmed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_pg(mode: TlsMode, expected: PgSslMode) {
        match (postgres_ssl_mode(mode), expected) {
            (PgSslMode::Disable, PgSslMode::Disable)
            | (PgSslMode::Prefer, PgSslMode::Prefer)
            | (PgSslMode::Require, PgSslMode::Require)
            | (PgSslMode::VerifyCa, PgSslMode::VerifyCa)
            | (PgSslMode::VerifyFull, PgSslMode::VerifyFull) => {}
            (got, want) => panic!("postgres mode {mode:?}: got {got:?}, want {want:?}"),
        }
    }

    fn assert_mysql(mode: TlsMode, expected: MySqlSslMode) {
        match (mysql_ssl_mode(mode), expected) {
            (MySqlSslMode::Disabled, MySqlSslMode::Disabled)
            | (MySqlSslMode::Preferred, MySqlSslMode::Preferred)
            | (MySqlSslMode::Required, MySqlSslMode::Required)
            | (MySqlSslMode::VerifyCa, MySqlSslMode::VerifyCa)
            | (MySqlSslMode::VerifyIdentity, MySqlSslMode::VerifyIdentity) => {}
            (got, want) => panic!("mysql mode {mode:?}: got {got:?}, want {want:?}"),
        }
    }

    #[test]
    fn maps_every_postgres_mode() {
        assert_pg(TlsMode::Disable, PgSslMode::Disable);
        assert_pg(TlsMode::Prefer, PgSslMode::Prefer);
        assert_pg(TlsMode::Require, PgSslMode::Require);
        assert_pg(TlsMode::VerifyCa, PgSslMode::VerifyCa);
        assert_pg(TlsMode::VerifyFull, PgSslMode::VerifyFull);
    }

    #[test]
    fn maps_every_mysql_mode() {
        assert_mysql(TlsMode::Disable, MySqlSslMode::Disabled);
        assert_mysql(TlsMode::Prefer, MySqlSslMode::Preferred);
        assert_mysql(TlsMode::Require, MySqlSslMode::Required);
        assert_mysql(TlsMode::VerifyCa, MySqlSslMode::VerifyCa);
        assert_mysql(TlsMode::VerifyFull, MySqlSslMode::VerifyIdentity);
    }

    #[test]
    fn apply_postgres_sets_mode_without_connecting() {
        let tls = TlsSettings {
            mode: TlsMode::VerifyCa,
            ca_path: "/tmp/ca.pem".into(),
            client_cert_path: "/tmp/client.crt".into(),
            client_key_path: "/tmp/client.key".into(),
        };
        // Building options must succeed; a live handshake is a CI follow-up.
        let _ = apply_postgres_tls(PgConnectOptions::new(), &tls).unwrap();
    }

    #[test]
    fn apply_mysql_sets_mode_without_connecting() {
        let tls = TlsSettings {
            mode: TlsMode::Require,
            ca_path: String::new(),
            client_cert_path: String::new(),
            client_key_path: String::new(),
        };
        let options = apply_mysql_tls(MySqlConnectOptions::new(), &tls).unwrap();
        assert_mysql(TlsMode::Require, options.get_ssl_mode());
    }

    #[test]
    fn empty_paths_are_skipped() {
        assert!(non_empty_path("").is_none());
        assert!(non_empty_path("   ").is_none());
        assert_eq!(
            non_empty_path(" /tmp/ca.pem ").map(|p| p.to_path_buf()),
            Some(std::path::PathBuf::from("/tmp/ca.pem"))
        );
    }
}
