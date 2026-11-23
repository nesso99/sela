//! Saved connections and OS keychain passwords.
//!
//! The JSON file stores hosts, ports, users, database names, and SQLite paths.
//! It never stores a password. Passwords go to the OS keychain when that
//! service is available, and otherwise stay in process memory for the session.
//!
//! On startup, a Birch `connections.json` is copied to the Sela path when the
//! Sela file is not there yet. The Birch file is not deleted. Keychain reads
//! fall back to Birch's service name and copy the secret forward.

mod model;
mod secrets;
mod store;

pub use model::{Engine, SavedConnection};
pub use secrets::{
    CachingSecrets, KeyringStore, MemorySecretStore, PasswordPersistence, SecretStore,
};
pub use store::{ConfigError, ConfigStore};
