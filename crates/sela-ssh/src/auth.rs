//! SSH authentication: password, private key (+ passphrase), ssh-agent.

use std::fmt;
use std::path::Path;
use std::sync::Arc;

use russh::client::Handle;
use russh::keys::agent::client::AgentClient;
use russh::keys::{load_secret_key, PrivateKeyWithHashAlg};
use russh::keys::{ssh_key, HashAlg, PrivateKey, PublicKey};

use crate::error::SshError;
use crate::tunnel::ClientHandler;

/// UX copy for a private key that cannot be parsed (SSH-03 / UX §errors).
const COULD_NOT_READ_KEY: &str = "SSH: could not read the private key.";
/// UX copy for an encrypted key that cannot be unlocked (SSH-03 / UX §errors).
const COULD_NOT_UNLOCK_KEY: &str = "SSH: could not unlock the private key.";

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
    let key = load_private_key(path, passphrase)?;
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

/// Load an OpenSSH private key, mapping russh/ssh_key failures to UX copy.
///
/// Never includes the passphrase in the returned error.
fn load_private_key(path: &Path, passphrase: Option<&str>) -> Result<PrivateKey, SshError> {
    if !path.is_file() {
        return Err(SshError::auth(COULD_NOT_READ_KEY));
    }
    let text = std::fs::read_to_string(path).map_err(|_| SshError::auth(COULD_NOT_READ_KEY))?;
    load_secret_key(path, passphrase)
        .map_err(|err| map_private_key_load_error(err, &text, passphrase))
}

/// Typed mapping of [`russh::keys::Error`] → unlock vs read (SSH-03).
///
/// - [`russh::keys::Error::KeyIsEncrypted`]: encrypted key, no passphrase → unlock.
/// - [`russh::keys::Error::SshKey`](`ssh_key::Error::Crypto`): unlock **only** when
///   the file still parses as an encrypted OpenSSH key **and** a passphrase was
///   supplied (wrong passphrase). Other Crypto sources (bad RSA material, cipher
///   errors, PPK MAC failures) stay as read.
/// - Everything else → read.
fn map_private_key_load_error(
    err: russh::keys::Error,
    key_text: &str,
    passphrase: Option<&str>,
) -> SshError {
    match err {
        russh::keys::Error::KeyIsEncrypted => SshError::auth(COULD_NOT_UNLOCK_KEY),
        russh::keys::Error::SshKey(ssh_key::Error::Crypto) => {
            let encrypted = PrivateKey::from_openssh(key_text)
                .map(|pk| pk.is_encrypted())
                .unwrap_or(false);
            if encrypted && passphrase.is_some() {
                SshError::auth(COULD_NOT_UNLOCK_KEY)
            } else {
                SshError::auth(COULD_NOT_READ_KEY)
            }
        }
        _ => SshError::auth(COULD_NOT_READ_KEY),
    }
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
    use russh::keys::key::safe_rng;
    use russh::keys::Algorithm;
    use std::fs;
    use tempfile::tempdir;

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

    /// Generate an encrypted OpenSSH private key into `dir` (never committed).
    fn write_encrypted_ed25519(dir: &Path, passphrase: &str) -> std::path::PathBuf {
        let mut rng = safe_rng();
        let key = PrivateKey::random(&mut rng, Algorithm::Ed25519).expect("generate key");
        let encrypted = key.encrypt(&mut rng, passphrase).expect("encrypt key");
        let openssh = encrypted
            .to_openssh(ssh_key::LineEnding::LF)
            .expect("serialize openssh");
        let path = dir.join("id_ed25519");
        fs::write(&path, openssh.as_bytes()).expect("write key");
        path
    }

    fn assert_no_passphrase_leak(err: &SshError, passphrase: &str) {
        let display = err.to_string();
        let debug = format!("{err:?}");
        assert!(
            !display.contains(passphrase),
            "Display must not contain passphrase: {display}"
        );
        assert!(
            !debug.contains(passphrase),
            "Debug must not contain passphrase: {debug}"
        );
    }

    #[test]
    // SSH-03: correct passphrase loads the encrypted key.
    fn load_private_key_correct_passphrase_ok() {
        let dir = tempdir().unwrap();
        let passphrase = "correct-horse-battery";
        let path = write_encrypted_ed25519(dir.path(), passphrase);
        load_private_key(&path, Some(passphrase)).expect("correct passphrase must load");
    }

    #[test]
    // SSH-03: wrong passphrase → unlock copy (not read).
    fn load_private_key_wrong_passphrase_unlock() {
        let dir = tempdir().unwrap();
        let passphrase = "correct-horse-battery";
        let wrong = "wrong-passphrase-xyz";
        let path = write_encrypted_ed25519(dir.path(), passphrase);
        let err = load_private_key(&path, Some(wrong)).expect_err("wrong passphrase");
        assert_eq!(err, SshError::auth(COULD_NOT_UNLOCK_KEY));
        assert_no_passphrase_leak(&err, wrong);
        assert_no_passphrase_leak(&err, passphrase);
    }

    #[test]
    // SSH-03: encrypted key with no passphrase → unlock.
    fn load_private_key_encrypted_without_passphrase_unlock() {
        let dir = tempdir().unwrap();
        let passphrase = "correct-horse-battery";
        let path = write_encrypted_ed25519(dir.path(), passphrase);
        let err = load_private_key(&path, None).expect_err("missing passphrase");
        assert_eq!(err, SshError::auth(COULD_NOT_UNLOCK_KEY));
        assert_no_passphrase_leak(&err, passphrase);
    }

    #[test]
    // SSH-03: corrupt / truncated key file → read (not unlock).
    fn load_private_key_corrupt_and_truncated_read() {
        let dir = tempdir().unwrap();

        let corrupt = dir.path().join("corrupt");
        fs::write(
            &corrupt,
            b"-----BEGIN OPENSSH PRIVATE KEY-----\nnot-a-key\n",
        )
        .unwrap();
        let err = load_private_key(&corrupt, Some("any-passphrase")).expect_err("corrupt");
        assert_eq!(err, SshError::auth(COULD_NOT_READ_KEY));
        assert_no_passphrase_leak(&err, "any-passphrase");

        let passphrase = "correct-horse-battery";
        let good = write_encrypted_ed25519(dir.path(), passphrase);
        let text = fs::read_to_string(&good).unwrap();
        let truncated = dir.path().join("truncated");
        // Cut mid-body so PEM header remains but payload is incomplete.
        let cut = text.len() / 2;
        fs::write(&truncated, &text[..cut]).unwrap();
        let err = load_private_key(&truncated, Some(passphrase)).expect_err("truncated");
        assert_eq!(err, SshError::auth(COULD_NOT_READ_KEY));
        assert_no_passphrase_leak(&err, passphrase);
    }

    #[test]
    // Crypto without an encrypted OpenSSH parse must stay "could not read".
    fn map_crypto_without_encrypted_key_is_read() {
        let err = map_private_key_load_error(
            russh::keys::Error::SshKey(ssh_key::Error::Crypto),
            "not-an-openssh-key",
            Some("secret-pass"),
        );
        assert_eq!(err, SshError::auth(COULD_NOT_READ_KEY));
        assert_no_passphrase_leak(&err, "secret-pass");
    }
}
