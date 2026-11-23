//! Postgres TLS acceptance tests (CI job `db-tls-postgres`).
//!
//! Skipped unless both `SELA_PG_TLS_URL` and `SELA_PG_TLS_CA` are set.
//! Certs are generated at CI/local runtime — never committed.

use sela_core::{DbError, Engine, SavedConnection, TlsMode, TlsSettings};
use sela_drivers::connect;

fn tls_env() -> Option<(SavedConnection, String, String)> {
    let url = std::env::var("SELA_PG_TLS_URL").ok()?;
    let ca = std::env::var("SELA_PG_TLS_CA").ok()?;
    let (host, port, username, password, database) = parse_postgres_url(&url);
    let mut connection = SavedConnection::new(Engine::Postgres);
    connection.name = "tls-ci".into();
    connection.host = host;
    connection.port = port;
    connection.username = username;
    connection.database = database;
    Some((connection, password, ca))
}

fn parse_postgres_url(url: &str) -> (String, u16, String, String, String) {
    let rest = url
        .strip_prefix("postgres://")
        .or_else(|| url.strip_prefix("postgresql://"))
        .unwrap_or_else(|| panic!("SELA_PG_TLS_URL must be a postgres URL, got {url}"));
    let (creds, after_at) = rest
        .split_once('@')
        .unwrap_or_else(|| panic!("URL missing @user:pass@host: {url}"));
    let (username, password) = creds.split_once(':').unwrap_or((creds, ""));
    let after_at = after_at.split('?').next().unwrap_or(after_at);
    let (hostport, database) = after_at
        .split_once('/')
        .unwrap_or_else(|| panic!("URL missing /database: {url}"));
    let (host, port) = match hostport.split_once(':') {
        Some((host, port)) => (
            host.to_string(),
            port.parse()
                .unwrap_or_else(|_| panic!("invalid port in {url}")),
        ),
        None => (hostport.to_string(), 5432),
    };
    (
        host,
        port,
        username.to_string(),
        password.to_string(),
        database.to_string(),
    )
}

#[tokio::test]
async fn postgres_tls_require_succeeds() {
    let Some((mut connection, password, _ca)) = tls_env() else {
        eprintln!("skip postgres TLS: set SELA_PG_TLS_URL and SELA_PG_TLS_CA");
        return;
    };
    connection.tls = TlsSettings {
        mode: TlsMode::Require,
        ..TlsSettings::default()
    };
    let session = connect(&connection, &password).await.unwrap_or_else(|err| {
        panic!("require should connect over TLS: {err:?}");
    });
    let schemas = session.list_schemas().await.expect("list_schemas");
    assert!(!schemas.is_empty());
}

#[tokio::test]
async fn postgres_tls_verify_ca_succeeds() {
    let Some((mut connection, password, ca)) = tls_env() else {
        eprintln!("skip postgres TLS: set SELA_PG_TLS_URL and SELA_PG_TLS_CA");
        return;
    };
    // Cert SAN is localhost only. verify-ca must accept a hostname mismatch
    // when the CA chain is trusted (do not rewrite the host to paper this over).
    connection.host = "127.0.0.1".into();
    connection.tls = TlsSettings {
        mode: TlsMode::VerifyCa,
        ca_path: ca,
        ..TlsSettings::default()
    };
    let session = connect(&connection, &password).await.unwrap_or_else(|err| {
        panic!("verify-ca with test CA should connect despite hostname mismatch: {err:?}");
    });
    let _ = session.list_schemas().await.expect("list_schemas");
}

#[tokio::test]
async fn postgres_tls_verify_full_hostname_mismatch_is_tls() {
    let Some((mut connection, password, ca)) = tls_env() else {
        eprintln!("skip postgres TLS: set SELA_PG_TLS_URL and SELA_PG_TLS_CA");
        return;
    };
    // Cert SAN is localhost only; connecting via 127.0.0.1 must fail verification.
    connection.host = "127.0.0.1".into();
    connection.tls = TlsSettings {
        mode: TlsMode::VerifyFull,
        ca_path: ca,
        ..TlsSettings::default()
    };
    match connect(&connection, &password).await {
        Ok(_) => panic!("verify-full against 127.0.0.1 should fail hostname check"),
        Err(err) => assert!(
            matches!(err, DbError::Tls { .. }),
            "expected DbError::Tls, got {err:?}"
        ),
    }
}

#[tokio::test]
async fn postgres_tls_disable_fails_without_tls_variant() {
    let Some((mut connection, password, _ca)) = tls_env() else {
        eprintln!("skip postgres TLS: set SELA_PG_TLS_URL and SELA_PG_TLS_CA");
        return;
    };
    // CI / run-pg-tls.sh use hostssl-only pg_hba; plain TCP is rejected.
    connection.tls = TlsSettings {
        mode: TlsMode::Disable,
        ..TlsSettings::default()
    };
    match connect(&connection, &password).await {
        Ok(_) => panic!("disable against hostssl-only server should fail"),
        Err(err) => {
            assert!(
                !matches!(err, DbError::Tls { .. }),
                "plain/hostssl rejection must not be classified as Tls, got {err:?}"
            );
            assert!(
                matches!(
                    err,
                    DbError::Connection { .. } | DbError::Auth { .. } | DbError::Config { .. }
                ),
                "expected Connection/Auth/Config, got {err:?}"
            );
        }
    }
}
