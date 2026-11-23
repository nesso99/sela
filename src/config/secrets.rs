use std::collections::HashMap;
use std::sync::Mutex;

/// OS keychain service for Sela.
const SERVICE: &str = "app.sela.database";

/// OS keychain service Birch used.
///
/// A password missing from [`SERVICE`] is read from here and, when present,
/// copied to [`SERVICE`]. The Birch entry is left in place.
const LEGACY_SERVICE: &str = "app.birch.database";

/// Keychain addressed by service name.
///
/// [`KeyringStore`] talks to the OS. Tests use an in-memory fake so they never
/// touch the real keychain.
trait KeychainByService {
    fn get(&self, service: &str, account: &str) -> Result<Option<String>, SecretError>;
    fn set(&self, service: &str, account: &str, secret: &str) -> Result<(), SecretError>;
}

/// Read `account` from the Sela service. If it is missing, read Birch's service
/// and copy a hit forward.
fn read_with_legacy<K: KeychainByService>(
    keychain: &K,
    account: &str,
) -> Result<Option<String>, SecretError> {
    if let Some(secret) = keychain.get(SERVICE, account)? {
        return Ok(Some(secret));
    }
    let Some(secret) = keychain.get(LEGACY_SERVICE, account)? else {
        return Ok(None);
    };
    // The password is already in hand. A failed copy must not hide it; the
    // Birch entry stays, and the next read tries the copy again.
    keychain.set(SERVICE, account, &secret).ok();
    Ok(Some(secret))
}

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

impl KeychainByService for KeyringStore {
    fn get(&self, service: &str, account: &str) -> Result<Option<String>, SecretError> {
        let entry = keyring::Entry::new(service, account).map_err(backend)?;
        match entry.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(err) => Err(backend(err)),
        }
    }

    fn set(&self, service: &str, account: &str, secret: &str) -> Result<(), SecretError> {
        let entry = keyring::Entry::new(service, account).map_err(backend)?;
        entry.set_password(secret).map_err(backend)
    }
}

impl SecretStore for KeyringStore {
    fn get(&self, account: &str) -> Result<Option<String>, SecretError> {
        read_with_legacy(self, account)
    }

    fn set(&self, account: &str, secret: &str) -> Result<(), SecretError> {
        KeychainByService::set(self, SERVICE, account, secret)
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

    #[derive(Default)]
    struct FakeKeychain {
        values: Mutex<HashMap<(String, String), String>>,
    }

    impl FakeKeychain {
        fn key(service: &str, account: &str) -> (String, String) {
            (service.to_string(), account.to_string())
        }
    }

    impl KeychainByService for FakeKeychain {
        fn get(&self, service: &str, account: &str) -> Result<Option<String>, SecretError> {
            Ok(self
                .values
                .lock()
                .expect("secret lock")
                .get(&Self::key(service, account))
                .cloned())
        }

        fn set(&self, service: &str, account: &str, secret: &str) -> Result<(), SecretError> {
            self.values
                .lock()
                .expect("secret lock")
                .insert(Self::key(service, account), secret.to_string());
            Ok(())
        }
    }

    #[test]
    fn service_names_are_sela_and_the_birch_legacy() {
        assert_eq!(SERVICE, "app.sela.database");
        assert_eq!(LEGACY_SERVICE, "app.birch.database");
    }

    #[test]
    fn missing_sela_secret_is_copied_from_birch_and_the_old_entry_stays() {
        let keychain = FakeKeychain::default();
        keychain.set(LEGACY_SERVICE, "conn-1", "hunter2").unwrap();
        let found = read_with_legacy(&keychain, "conn-1").unwrap();
        assert_eq!(found.as_deref(), Some("hunter2"));
        assert_eq!(
            keychain.get(SERVICE, "conn-1").unwrap().as_deref(),
            Some("hunter2")
        );
        assert_eq!(
            keychain.get(LEGACY_SERVICE, "conn-1").unwrap().as_deref(),
            Some("hunter2")
        );
    }

    #[test]
    fn sela_secret_wins_and_birch_is_left_unchanged() {
        let keychain = FakeKeychain::default();
        keychain.set(SERVICE, "conn-1", "new").unwrap();
        keychain.set(LEGACY_SERVICE, "conn-1", "old").unwrap();
        assert_eq!(
            read_with_legacy(&keychain, "conn-1").unwrap().as_deref(),
            Some("new")
        );
        assert_eq!(
            keychain.get(LEGACY_SERVICE, "conn-1").unwrap().as_deref(),
            Some("old")
        );
    }

    #[test]
    fn missing_secret_copies_nothing() {
        let keychain = FakeKeychain::default();
        assert_eq!(read_with_legacy(&keychain, "conn-1").unwrap(), None);
        assert_eq!(keychain.get(SERVICE, "conn-1").unwrap(), None);
        assert_eq!(keychain.get(LEGACY_SERVICE, "conn-1").unwrap(), None);
    }

    #[test]
    fn birch_entry_stays_when_the_sela_write_fails() {
        struct FailNewService {
            inner: FakeKeychain,
        }

        impl KeychainByService for FailNewService {
            fn get(&self, service: &str, account: &str) -> Result<Option<String>, SecretError> {
                self.inner.get(service, account)
            }

            fn set(&self, service: &str, account: &str, secret: &str) -> Result<(), SecretError> {
                if service == SERVICE {
                    return Err(SecretError::Backend("read only".into()));
                }
                self.inner.set(service, account, secret)
            }
        }

        let inner = FakeKeychain::default();
        inner.set(LEGACY_SERVICE, "conn-1", "hunter2").unwrap();
        let keychain = FailNewService { inner };
        assert_eq!(
            read_with_legacy(&keychain, "conn-1").unwrap().as_deref(),
            Some("hunter2")
        );
        assert_eq!(
            keychain
                .inner
                .get(LEGACY_SERVICE, "conn-1")
                .unwrap()
                .as_deref(),
            Some("hunter2")
        );
        assert_eq!(keychain.inner.get(SERVICE, "conn-1").unwrap(), None);
    }
}
