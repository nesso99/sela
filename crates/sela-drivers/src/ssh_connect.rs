//! Open an SSH tunnel before dialing Postgres/MySQL (M3-1).

use sela_core::{DbError, Engine, SavedConnection};
use sela_ssh::{
    open_tunnel, SshAuthSecrets, SshPaths, SshTunnel, TunnelOpen, TunnelRequest, UnknownHostKey,
};

/// Auth material for [`crate::connect`] / [`crate::connect_with_auth`].
#[derive(Debug, Clone, Default)]
pub struct ConnectAuth {
    pub db_password: String,
    pub ssh: SshAuthSecrets,
    /// When `None`, uses [`SshPaths::system_default`]. Tests inject a temp tree.
    pub ssh_paths: Option<SshPaths>,
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

pub(crate) async fn finish_with_tunnel(
    connection: SavedConnection,
    db_password: &str,
    tunnel: SshTunnel,
) -> Result<Box<dyn sela_core::DatabaseSession>, DbError> {
    let local = tunnel.local_addr();
    let mut via_tunnel = connection.clone();
    // SSH-09: driver always dials the ephemeral loopback listener.
    via_tunnel.host = local.ip().to_string();
    via_tunnel.port = local.port();
    // Do not recurse into SSH on the rewritten endpoint.
    via_tunnel.ssh.enabled = false;

    match connection.engine {
        Engine::Postgres => {
            let session = crate::postgres::PostgresSession::connect_with_tunnel(
                &via_tunnel,
                db_password,
                tunnel,
            )
            .await?;
            Ok(Box::new(session))
        }
        Engine::Mysql => {
            let session =
                crate::mysql::MysqlSession::connect_with_tunnel(&via_tunnel, db_password, tunnel)
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
