use std::collections::HashMap;
use std::sync::Mutex;

use keyring::credential::CredentialPersistence;

const SERVICE: &str = "app.sela.database";

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("{0}")]
    Backend(String),
}

pub trait SecretStore: Send + Sync {
    fn get(&self, account: &str) -> Result<Option<String>, SecretError>;
    fn set(&self, account: &str, secret: &str) -> Result<(), SecretError>;
    fn delete(&self, account: &str) -> Result<(), SecretError>;
}

/// Keychain account for the database password (`connections.json` id).
pub fn db_password_account(connection_id: &str) -> &str {
    connection_id
}

/// Keychain account for the SSH bastion password (SSH-13).
pub fn ssh_password_account(connection_id: &str) -> String {
    format!("{connection_id}:ssh-password")
}

/// Keychain account for the SSH private-key passphrase (SSH-13).
pub fn ssh_passphrase_account(connection_id: &str) -> String {
    format!("{connection_id}:ssh-passphrase")
}

/// Delete every secret tied to a saved connection (DB + SSH).
///
/// Call when the user deletes a connection so SSH passwords / passphrases do
/// not linger after the JSON row is gone (SSH-13).
pub fn delete_connection_secrets(store: &impl SecretStore, connection_id: &str) {
    let _ = store.delete(db_password_account(connection_id));
    let _ = store.delete(&ssh_password_account(connection_id));
    let _ = store.delete(&ssh_passphrase_account(connection_id));
}

/// In-memory store used by tests and as the session fallback.
#[derive(Default)]
pub struct MemorySecretStore {
    values: Mutex<HashMap<String, String>>,
}

impl SecretStore for MemorySecretStore {
    fn get(&self, account: &str) -> Result<Option<String>, SecretError> {
        Ok(self
            .values
            .lock()
            .expect("secret lock")
            .get(account)
            .cloned())
    }

    fn set(&self, account: &str, secret: &str) -> Result<(), SecretError> {
        self.values
            .lock()
            .expect("secret lock")
            .insert(account.to_string(), secret.to_string());
        Ok(())
    }

    fn delete(&self, account: &str) -> Result<(), SecretError> {
        self.values.lock().expect("secret lock").remove(account);
        Ok(())
    }
}

impl SecretStore for Box<dyn SecretStore> {
    fn get(&self, account: &str) -> Result<Option<String>, SecretError> {
        (**self).get(account)
    }

    fn set(&self, account: &str, secret: &str) -> Result<(), SecretError> {
        (**self).set(account, secret)
    }

    fn delete(&self, account: &str) -> Result<(), SecretError> {
        (**self).delete(account)
    }
}

/// True when keyring's default builder is the in-process mock store.
///
/// keyring v3 enables no platform backend unless Cargo features are set. The
/// mock store is the only built-in builder that reports
/// [`CredentialPersistence::EntryOnly`].
pub fn default_credential_store_is_mock() -> bool {
    matches!(
        keyring::default::default_credential_builder().persistence(),
        CredentialPersistence::EntryOnly
    )
}

/// Fail loudly when a release (or debug) build would silently use the mock store.
///
/// Call once at startup. Runtime unavailability of Secret Service / Keychain
/// still returns [`SecretError`] from [`KeyringStore`] so
/// [`CachingSecrets`] can keep the #46 memory-only fallback.
pub fn warn_if_mock_credential_store() {
    let is_mock = default_credential_store_is_mock();
    debug_assert!(
        !is_mock,
        "keyring default credential store is the in-process mock; \
         passwords will not persist across launches. Enable platform features \
         (apple-native, windows-native, sync-secret-service + crypto-rust)."
    );
    if is_mock {
        eprintln!(
            "sela: ERROR: keyring default credential store is the in-process mock; \
             passwords will not persist across launches. Enable platform features \
             (apple-native, windows-native, sync-secret-service + crypto-rust)."
        );
    }
}

/// OS keychain via the `keyring` crate.
///
/// macOS uses Keychain (`apple-native`), Windows uses Credential Manager
/// (`windows-native`), and Linux uses the Secret Service
/// (`sync-secret-service` + `crypto-rust`). A headless Linux session with
/// no secret service returns [`SecretError`].
pub struct KeyringStore;

impl SecretStore for KeyringStore {
    fn get(&self, account: &str) -> Result<Option<String>, SecretError> {
        let entry = keyring::Entry::new(SERVICE, account).map_err(backend)?;
        match entry.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(err) => Err(backend(err)),
        }
    }

    fn set(&self, account: &str, secret: &str) -> Result<(), SecretError> {
        let entry = keyring::Entry::new(SERVICE, account).map_err(backend)?;
        entry.set_password(secret).map_err(backend)
    }

    fn delete(&self, account: &str) -> Result<(), SecretError> {
        let entry = keyring::Entry::new(SERVICE, account).map_err(backend)?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(backend(err)),
        }
    }
}

fn backend(err: impl std::fmt::Display) -> SecretError {
    SecretError::Backend(err.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasswordPersistence {
    /// Nothing to store. An empty password is not written anywhere.
    Empty,
    Keychain,
    /// The keychain refused the write. The password is held in memory only.
    MemoryOnly(String),
}

/// Status-bar / dialog sentence when the OS keychain cannot store a password.
pub fn memory_only_password_warning(backend_message: &str) -> String {
    format!(
        "The OS keychain is unavailable, so the password is kept for this session only. {backend_message}"
    )
}

/// Dialog helper when a keychain probe fails before save.
pub fn keychain_unavailable_dialog_warning(backend_message: &str) -> String {
    format!(
        "The OS keychain is unavailable, so the password will not be saved after you quit. {backend_message}"
    )
}

/// Tries the OS keychain, and remembers a password for this process if that fails.
pub struct CachingSecrets<S> {
    inner: S,
    memory: Mutex<HashMap<String, String>>,
}

impl<S> CachingSecrets<S> {
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            memory: Mutex::new(HashMap::new()),
        }
    }
}

impl<S: SecretStore> CachingSecrets<S> {
    /// Probe whether the OS keychain backend responds.
    ///
    /// `Ok(())` means the backend answered (including "no entry"). `Err` means
    /// the service is unavailable for this session.
    pub fn probe_keychain(&self) -> Result<(), SecretError> {
        match self.inner.get("__sela_keychain_probe__") {
            Ok(_) => Ok(()),
            Err(err) => Err(err),
        }
    }

    pub fn get(&self, account: &str) -> Result<String, SecretError> {
        Ok(self.get_optional(account)?.unwrap_or_default())
    }

    /// Like [`Self::get`], but `None` when neither memory nor the keychain has
    /// an entry (Duplicate creates a row with no secret).
    pub fn get_optional(&self, account: &str) -> Result<Option<String>, SecretError> {
        if let Some(value) = self
            .memory
            .lock()
            .expect("secret lock")
            .get(account)
            .cloned()
        {
            return Ok(Some(value));
        }
        match self.inner.get(account)? {
            Some(value) => {
                self.memory
                    .lock()
                    .expect("secret lock")
                    .insert(account.to_string(), value.clone());
                Ok(Some(value))
            }
            None => Ok(None),
        }
    }

    pub fn set(&self, account: &str, secret: &str) -> PasswordPersistence {
        if secret.is_empty() {
            let _ = self.inner.delete(account);
            // Remember an intentional empty password for this session so connect
            // does not re-prompt (Duplicate never calls set, so it stays None).
            self.memory
                .lock()
                .expect("secret lock")
                .insert(account.to_string(), String::new());
            return PasswordPersistence::Empty;
        }
        match self.inner.set(account, secret) {
            Ok(()) => {
                self.memory
                    .lock()
                    .expect("secret lock")
                    .insert(account.to_string(), secret.to_string());
                PasswordPersistence::Keychain
            }
            Err(err) => {
                self.memory
                    .lock()
                    .expect("secret lock")
                    .insert(account.to_string(), secret.to_string());
                PasswordPersistence::MemoryOnly(err.to_string())
            }
        }
    }

    pub fn delete(&self, account: &str) {
        let _ = self.inner.delete(account);
        self.memory.lock().expect("secret lock").remove(account);
    }

    /// Remove the database password and any SSH secrets for this connection (SSH-13).
    pub fn delete_connection(&self, connection_id: &str) {
        self.delete(connection_id);
        self.delete(&ssh_password_account(connection_id));
        self.delete(&ssh_passphrase_account(connection_id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_store_round_trip() {
        let secrets = CachingSecrets::new(MemorySecretStore::default());
        assert_eq!(secrets.get("a").unwrap(), "");
        assert_eq!(secrets.get_optional("a").unwrap(), None);
        assert_eq!(secrets.set("a", "s3cret"), PasswordPersistence::Keychain);
        assert_eq!(secrets.get("a").unwrap(), "s3cret");
        assert_eq!(
            secrets.get_optional("a").unwrap().as_deref(),
            Some("s3cret")
        );
        assert_eq!(secrets.set("a", ""), PasswordPersistence::Empty);
        assert_eq!(secrets.get("a").unwrap(), "");
        // Intentional empty is remembered in-session so connect does not re-prompt.
        assert_eq!(secrets.get_optional("a").unwrap().as_deref(), Some(""));
        // An account that was never set stays missing (Duplicate → prompt).
        assert_eq!(secrets.get_optional("never-set").unwrap(), None);
    }

    #[test]
    // SSH-13: account helpers and delete_connection clear DB + SSH secrets.
    fn ssh13_delete_connection_clears_ssh_secrets() {
        let secrets = CachingSecrets::new(MemorySecretStore::default());
        let id = "conn-1";
        assert_eq!(secrets.set(id, "db"), PasswordPersistence::Keychain);
        assert_eq!(
            secrets.set(&ssh_password_account(id), "ssh-pw"),
            PasswordPersistence::Keychain
        );
        assert_eq!(
            secrets.set(&ssh_passphrase_account(id), "ssh-pp"),
            PasswordPersistence::Keychain
        );
        secrets.delete_connection(id);
        assert_eq!(secrets.get_optional(id).unwrap(), None);
        assert_eq!(
            secrets.get_optional(&ssh_password_account(id)).unwrap(),
            None
        );
        assert_eq!(
            secrets.get_optional(&ssh_passphrase_account(id)).unwrap(),
            None
        );
        assert_eq!(ssh_password_account(id), "conn-1:ssh-password");
        assert_eq!(ssh_passphrase_account(id), "conn-1:ssh-passphrase");
    }

    #[test]
    fn keychain_failure_keeps_the_password_in_memory_only() {
        struct Broken;
        impl SecretStore for Broken {
            fn get(&self, _: &str) -> Result<Option<String>, SecretError> {
                Err(SecretError::Backend("no session bus".into()))
            }
            fn set(&self, _: &str, _: &str) -> Result<(), SecretError> {
                Err(SecretError::Backend("no session bus".into()))
            }
            fn delete(&self, _: &str) -> Result<(), SecretError> {
                Err(SecretError::Backend("no session bus".into()))
            }
        }

        let secrets = CachingSecrets::new(Broken);
        match secrets.set("conn", "hunter2") {
            PasswordPersistence::MemoryOnly(message) => {
                assert!(message.contains("session bus"));
                let warning = memory_only_password_warning(&message);
                assert!(warning.contains("kept for this session only"));
                assert!(warning.contains("session bus"));
                let dialog = keychain_unavailable_dialog_warning(&message);
                assert!(dialog.contains("will not be saved after you quit"));
            }
            other => panic!("expected memory fallback, got {other:?}"),
        }
        assert_eq!(secrets.get("conn").unwrap(), "hunter2");
        // Reconnect / switch in the same process still reads the memory copy.
        assert_eq!(secrets.get("conn").unwrap(), "hunter2");
        assert!(secrets.probe_keychain().is_err());
    }

    #[test]
    fn probe_keychain_succeeds_when_backend_answers() {
        let secrets = CachingSecrets::new(MemorySecretStore::default());
        assert!(secrets.probe_keychain().is_ok());
    }

    #[test]
    fn default_credential_builder_is_not_mock_on_supported_targets() {
        // Platforms where Sela ships with a native keyring feature enabled.
        // The mock store is EntryOnly; native Secret Service / Keychain /
        // Credential Manager report UntilDelete (or UntilReboot for keyutils).
        #[cfg(any(
            target_os = "linux",
            target_os = "freebsd",
            target_os = "openbsd",
            target_os = "macos",
            target_os = "windows",
        ))]
        {
            assert!(
                !default_credential_store_is_mock(),
                "default keyring builder is the mock store; check Cargo.toml features \
                 (apple-native, windows-native, sync-secret-service, crypto-rust)"
            );
            assert!(!matches!(
                keyring::default::default_credential_builder().persistence(),
                CredentialPersistence::EntryOnly
            ));
        }
    }
}
