//! SSH authentication: password, private key (+ passphrase), ssh-agent.

use std::fmt;
use std::path::Path;
use std::sync::Arc;

use russh::client::Handle;
use russh::keys::agent::client::AgentClient;
use russh::keys::{load_secret_key, PrivateKeyWithHashAlg};
use russh::keys::{HashAlg, PublicKey};

use crate::error::SshError;
use crate::tunnel::ClientHandler;

/// Secrets supplied by the caller (from the OS keychain). Never logged.
#[derive(Clone, Default)]
pub struct SshAuthSecrets {
    /// Bastion password when [`sela_core::SshAuthMethod::Password`].
    pub password: Option<String>,
    /// Private-key passphrase when the key file is encrypted.
    pub passphrase: Option<String>,
}

impl fmt::Debug for SshAuthSecrets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SshAuthSecrets")
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field(
                "passphrase",
                &self.passphrase.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
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
                return Err(SshError::auth("SSH: authentication failed."));
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
        return Err(SshError::auth("SSH: could not read the private key."));
    }
    let key = load_secret_key(path, passphrase).map_err(|err| {
        // Wrong passphrase and unreadable key both surface as auth — never
        // echo the passphrase.
        // russh/ssh-key surfaces a wrong passphrase as "cryptographic error"
        // or "encrypted", not the words password/passphrase/decrypt.
        let msg = err.to_string().to_ascii_lowercase();
        if msg.contains("password")
            || msg.contains("passphrase")
            || msg.contains("decrypt")
            || msg.contains("crypto")
            || msg.contains("encrypted")
        {
            SshError::auth("SSH: could not unlock the private key.")
        } else {
            SshError::auth("SSH: could not read the private key.")
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
        return Err(SshError::auth("SSH: authentication failed."));
    }
    Ok(())
}

async fn authenticate_agent(
    session: &mut Handle<ClientHandler>,
    user: &str,
) -> Result<(), SshError> {
    #[cfg(unix)]
    {
        let mut agent = AgentClient::connect_env()
            .await
            .map_err(|_| SshError::auth("SSH: SSH agent is not available."))?;
        let identities: Vec<PublicKey> = agent
            .request_identities()
            .await
            .map_err(|_| SshError::auth("SSH: SSH agent is not available."))?;
        if identities.is_empty() {
            return Err(SshError::auth("SSH: SSH agent has no identities."));
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
                .map_err(|_| SshError::auth("SSH: authentication failed."))?;
            if result.success() {
                return Ok(());
            }
        }
        Err(SshError::auth("SSH: authentication failed."))
    }
    #[cfg(windows)]
    {
        let mut agent = AgentClient::connect_pageant().await;
        let identities: Vec<PublicKey> = agent
            .request_identities()
            .await
            .map_err(|_| SshError::auth("SSH: SSH agent is not available."))?;
        if identities.is_empty() {
            return Err(SshError::auth("SSH: SSH agent has no identities."));
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
                .map_err(|_| SshError::auth("SSH: authentication failed."))?;
            if result.success() {
                return Ok(());
            }
        }
        Err(SshError::auth("SSH: authentication failed."))
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
    SshError::connection(format!(
        "SSH: could not reach bastion during authentication ({err})."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_auth_secrets_debug_redacts_values() {
        let secrets = SshAuthSecrets {
            password: Some("super-secret-pw".into()),
            passphrase: Some("key-passphrase-xyz".into()),
        };
        let debug = format!("{secrets:?}");
        assert!(debug.contains("<redacted>"), "{debug}");
        assert!(!debug.contains("super-secret-pw"), "{debug}");
        assert!(!debug.contains("key-passphrase-xyz"), "{debug}");
    }
}
