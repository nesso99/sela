//! Open an SSH tunnel before dialing Postgres/MySQL (M3-1).

use std::fmt;
use std::net::SocketAddr;

use sela_core::{DbError, Engine, SavedConnection};
use sela_ssh::{
    open_tunnel, SshAuthSecrets, SshPaths, SshTunnel, TunnelOpen, TunnelRequest, UnknownHostKey,
};

/// Auth material for [`crate::connect`] / [`crate::connect_with_auth`].
#[derive(Clone, Default)]
pub struct ConnectAuth {
    pub db_password: String,
    pub ssh: SshAuthSecrets,
    /// When `None`, uses [`SshPaths::system_default`]. Tests inject a temp tree.
    pub ssh_paths: Option<SshPaths>,
}

impl fmt::Debug for ConnectAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectAuth")
            .field(
                "db_password",
                &if self.db_password.is_empty() {
                    ""
                } else {
                    "<redacted>"
                },
            )
            .field("ssh", &self.ssh)
            .field("ssh_paths", &self.ssh_paths)
            .finish()
    }
}

/// Result of connect when SSH may present a first-seen host key (SSH-18).
pub enum ConnectOutcome {
    Ready(Box<dyn sela_core::DatabaseSession>),
    /// Caller must confirm the fingerprint, then call accept_* on the challenge.
    UnknownSshHostKey(Box<UnknownSshHostKeyChallenge>),
}

impl std::fmt::Debug for ConnectOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ready(_) => f.write_str("ConnectOutcome::Ready(..)"),
            Self::UnknownSshHostKey(c) => f
                .debug_struct("ConnectOutcome::UnknownSshHostKey")
                .field("fingerprint", &c.fingerprint)
                .field("host", &c.host)
                .field("port", &c.port)
                .finish(),
        }
    }
}

/// Pending connect after an unknown SSH host key.
pub struct UnknownSshHostKeyChallenge {
    pub fingerprint: String,
    pub host: String,
    pub port: u16,
    pending: UnknownHostKey,
    connection: SavedConnection,
    auth: ConnectAuth,
}

impl UnknownSshHostKeyChallenge {
    pub async fn accept_and_remember(self) -> Result<Box<dyn sela_core::DatabaseSession>, DbError> {
        let tunnel = self
            .pending
            .accept_and_remember()
            .await
            .map_err(SshErrorExt::into_db)?;
        finish_with_tunnel(self.connection, &self.auth.db_password, tunnel).await
    }

    pub async fn accept_once(self) -> Result<Box<dyn sela_core::DatabaseSession>, DbError> {
        let tunnel = self
            .pending
            .accept_once()
            .await
            .map_err(SshErrorExt::into_db)?;
        finish_with_tunnel(self.connection, &self.auth.db_password, tunnel).await
    }

    pub fn reject(self) -> DbError {
        self.pending.reject().into_db_error()
    }
}

trait SshErrorExt {
    fn into_db(self) -> DbError;
}

impl SshErrorExt for sela_ssh::SshError {
    fn into_db(self) -> DbError {
        self.into_db_error()
    }
}

pub(crate) async fn maybe_open_tunnel(
    connection: &SavedConnection,
    auth: &ConnectAuth,
) -> Result<Option<TunnelOpen>, DbError> {
    if !connection.ssh.enabled {
        return Ok(None);
    }
    if connection.engine == Engine::Sqlite {
        return Err(DbError::config("SSH tunnel is not used with SQLite."));
    }
    let paths = match &auth.ssh_paths {
        Some(p) => p.clone(),
        None => SshPaths::system_default().map_err(|err| err.into_db_error())?,
    };
    let request = TunnelRequest {
        ssh: connection.ssh.clone(),
        remote_db_host: connection.host.clone(),
        remote_db_port: connection.port,
        secrets: auth.ssh.clone(),
        paths,
    };
    let open = open_tunnel(request)
        .await
        .map_err(|err| err.into_db_error())?;
    Ok(Some(open))
}

/// Prepare a connection for dialing through an SSH local forward.
///
/// Keeps the logical DB [`SavedConnection::host`] for TLS SNI / verify-full and
/// only disables nested SSH. The dial address is the tunnel's loopback listener.
pub(crate) fn prepare_tunneled_connection(
    connection: &SavedConnection,
    local: SocketAddr,
) -> (SavedConnection, SocketAddr) {
    let mut via = connection.clone();
    via.ssh.enabled = false;
    (via, local)
}

pub(crate) async fn finish_with_tunnel(
    connection: SavedConnection,
    db_password: &str,
    tunnel: SshTunnel,
) -> Result<Box<dyn sela_core::DatabaseSession>, DbError> {
    let local = tunnel.local_addr();
    let (via_tunnel, dial) = prepare_tunneled_connection(&connection, local);

    match connection.engine {
        Engine::Postgres => {
            let session = crate::postgres::PostgresSession::connect_with_tunnel(
                &via_tunnel,
                db_password,
                Some(dial),
                tunnel,
            )
            .await?;
            Ok(Box::new(session))
        }
        Engine::Mysql => {
            let session = crate::mysql::MysqlSession::connect_with_tunnel(
                &via_tunnel,
                db_password,
                Some(dial),
                tunnel,
            )
            .await?;
            Ok(Box::new(session))
        }
        Engine::Sqlite => Err(DbError::config("SSH tunnel is not used with SQLite.")),
    }
}

pub(crate) fn challenge_from_unknown(
    unknown: Box<UnknownHostKey>,
    connection: SavedConnection,
    auth: ConnectAuth,
) -> UnknownSshHostKeyChallenge {
    UnknownSshHostKeyChallenge {
        fingerprint: unknown.fingerprint.clone(),
        host: unknown.host.clone(),
        port: unknown.port,
        pending: *unknown,
        connection,
        auth,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sela_core::{SshAuthMethod, SshSettings, TlsMode, TlsSettings};

    #[test]
    fn connect_auth_debug_redacts_db_and_ssh_secrets() {
        let auth = ConnectAuth {
            db_password: "db-secret-value".into(),
            ssh: SshAuthSecrets {
                password: Some("ssh-secret-value".into()),
                passphrase: Some("pp-secret-value".into()),
            },
            ssh_paths: None,
        };
        let debug = format!("{auth:?}");
        assert!(debug.contains("<redacted>"), "{debug}");
        assert!(!debug.contains("db-secret-value"), "{debug}");
        assert!(!debug.contains("ssh-secret-value"), "{debug}");
        assert!(!debug.contains("pp-secret-value"), "{debug}");
    }

    #[test]
    // SSH-21: TLS settings and logical host survive the tunnel rewrite; dial is loopback.
    fn ssh21_tls_settings_forwarded_after_tunnel() {
        let mut connection = SavedConnection::new(Engine::Postgres);
        connection.host = "db.example.com".into();
        connection.port = 5432;
        connection.tls = TlsSettings {
            mode: TlsMode::VerifyFull,
            ca_path: "/tmp/ca.crt".into(),
            ..TlsSettings::default()
        };
        connection.ssh = SshSettings {
            enabled: true,
            host: "bastion".into(),
            port: 22,
            user: "tunnel".into(),
            auth: SshAuthMethod::Password,
            private_key_path: String::new(),
        };
        let local: SocketAddr = "127.0.0.1:45678".parse().unwrap();
        let (via, dial) = prepare_tunneled_connection(&connection, local);
        assert_eq!(via.host, "db.example.com");
        assert_eq!(via.port, 5432);
        assert_eq!(via.tls.mode, TlsMode::VerifyFull);
        assert_eq!(via.tls.ca_path, "/tmp/ca.crt");
        assert!(!via.ssh.enabled);
        assert_eq!(dial.ip().to_string(), "127.0.0.1");
        assert_eq!(dial.port(), 45678);
    }
}
