//! SSH authentication: password, private key (+ passphrase), ssh-agent.

use std::path::Path;
use std::sync::Arc;

use russh::client::Handle;
use russh::keys::agent::client::AgentClient;
use russh::keys::{load_secret_key, PrivateKeyWithHashAlg};
use russh::keys::{HashAlg, PublicKey};

use crate::error::SshError;
use crate::tunnel::ClientHandler;

/// Secrets supplied by the caller (from the OS keychain). Never logged.
#[derive(Debug, Clone, Default)]
pub struct SshAuthSecrets {
    /// Bastion password when [`sela_core::SshAuthMethod::Password`].
    pub password: Option<String>,
    /// Private-key passphrase when the key file is encrypted.
    pub passphrase: Option<String>,
}

/// Which auth method to attempt.
#[derive(Debug, Clone)]
pub enum AuthMethod {
    Password,
    PrivateKey { path: std::path::PathBuf },
    Agent,
}

pub(crate) async fn authenticate(
    session: &mut Handle<ClientHandler>,
    user: &str,
    method: &AuthMethod,
    secrets: &SshAuthSecrets,
) -> Result<(), SshError> {
    match method {
        AuthMethod::Password => {
            let password = secrets.password.as_deref().unwrap_or("");
            let result = session
                .authenticate_password(user, password)
                .await
                .map_err(map_auth_transport)?;
            if !result.success() {
                return Err(SshError::auth("SSH password authentication failed"));
            }
            Ok(())
        }
        AuthMethod::PrivateKey { path } => {
            authenticate_private_key(session, user, path, secrets.passphrase.as_deref()).await
        }
        AuthMethod::Agent => authenticate_agent(session, user).await,
    }
}

async fn authenticate_private_key(
    session: &mut Handle<ClientHandler>,
    user: &str,
    path: &Path,
    passphrase: Option<&str>,
) -> Result<(), SshError> {
    if !path.is_file() {
        return Err(SshError::auth(format!(
            "SSH identity file not found: {}",
            path.display()
        )));
    }
    let key = load_secret_key(path, passphrase).map_err(|err| {
        // Wrong passphrase and unreadable key both surface as auth/config — never
        // echo the passphrase.
        let msg = err.to_string();
        if msg.to_ascii_lowercase().contains("password")
            || msg.to_ascii_lowercase().contains("passphrase")
            || msg.to_ascii_lowercase().contains("decrypt")
        {
            SshError::auth("SSH private key passphrase is wrong or the key could not be decrypted")
        } else {
            SshError::auth(format!("could not load SSH identity file: {err}"))
        }
    })?;
    let hash = session
        .best_supported_rsa_hash()
        .await
        .map_err(map_auth_transport)?
        .flatten();
    let result = session
        .authenticate_publickey(user, PrivateKeyWithHashAlg::new(Arc::new(key), hash))
        .await
        .map_err(map_auth_transport)?;
    if !result.success() {
        return Err(SshError::auth("SSH public-key authentication failed"));
    }
    Ok(())
}

async fn authenticate_agent(
    session: &mut Handle<ClientHandler>,
    user: &str,
) -> Result<(), SshError> {
    #[cfg(unix)]
    {
        let mut agent = AgentClient::connect_env().await.map_err(|err| {
            SshError::auth(format!(
                "could not connect to ssh-agent (is SSH_AUTH_SOCK set?): {err}"
            ))
        })?;
        let identities: Vec<PublicKey> = agent.request_identities().await.map_err(|err| {
            SshError::auth(format!("ssh-agent refused to list identities: {err}"))
        })?;
        if identities.is_empty() {
            return Err(SshError::auth("ssh-agent has no identities"));
        }
        let hash: Option<HashAlg> = session
            .best_supported_rsa_hash()
            .await
            .map_err(map_auth_transport)?
            .flatten();
        for identity in identities {
            let result = session
                .authenticate_publickey_with(user, identity, hash, &mut agent)
                .await
                .map_err(|err| SshError::auth(format!("ssh-agent authentication failed: {err}")))?;
            if result.success() {
                return Ok(());
            }
        }
        Err(SshError::auth(
            "ssh-agent authentication failed for all identities",
        ))
    }
    #[cfg(windows)]
    {
        let mut agent = AgentClient::connect_pageant().await;
        let identities: Vec<PublicKey> = agent
            .request_identities()
            .await
            .map_err(|err| SshError::auth(format!("Pageant refused to list identities: {err}")))?;
        if identities.is_empty() {
            return Err(SshError::auth("Pageant has no identities"));
        }
        let hash: Option<HashAlg> = session
            .best_supported_rsa_hash()
            .await
            .map_err(map_auth_transport)?
            .flatten();
        for identity in identities {
            let result = session
                .authenticate_publickey_with(user, identity, hash, &mut agent)
                .await
                .map_err(|err| SshError::auth(format!("Pageant authentication failed: {err}")))?;
            if result.success() {
                return Ok(());
            }
        }
        Err(SshError::auth(
            "Pageant authentication failed for all identities",
        ))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (session, user);
        Err(SshError::unsupported(
            "ssh-agent authentication is not supported on this platform",
        ))
    }
}

fn map_auth_transport(err: russh::Error) -> SshError {
    SshError::connection(format!("SSH transport error during authentication: {err}"))
}
