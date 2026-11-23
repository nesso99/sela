use std::collections::HashMap;
use std::sync::Mutex;

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

/// OS keychain via the `keyring` crate.
///
/// macOS uses Keychain, Windows uses Credential Manager, and Linux uses the
/// Secret Service (gnome-keyring or KWallet). A headless Linux session with
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
    pub fn get(&self, account: &str) -> Result<String, SecretError> {
        if let Some(value) = self
            .memory
            .lock()
            .expect("secret lock")
            .get(account)
            .cloned()
        {
            return Ok(value);
        }
        match self.inner.get(account)? {
            Some(value) => {
                self.memory
                    .lock()
                    .expect("secret lock")
                    .insert(account.to_string(), value.clone());
                Ok(value)
            }
            None => Ok(String::new()),
        }
    }

    pub fn set(&self, account: &str, secret: &str) -> PasswordPersistence {
        if secret.is_empty() {
            let _ = self.inner.delete(account);
            self.memory.lock().expect("secret lock").remove(account);
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_store_round_trip() {
        let secrets = CachingSecrets::new(MemorySecretStore::default());
        assert_eq!(secrets.get("a").unwrap(), "");
        assert_eq!(secrets.set("a", "s3cret"), PasswordPersistence::Keychain);
        assert_eq!(secrets.get("a").unwrap(), "s3cret");
        assert_eq!(secrets.set("a", ""), PasswordPersistence::Empty);
        assert_eq!(secrets.get("a").unwrap(), "");
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
            }
            other => panic!("expected memory fallback, got {other:?}"),
        }
        assert_eq!(secrets.get("conn").unwrap(), "hunter2");
    }
}
