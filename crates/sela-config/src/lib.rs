//! Saved connections and OS keychain passwords.
//!
//! The JSON file stores hosts, ports, users, database names, and SQLite paths.
//! It never stores a password. Passwords go to the OS keychain when that
//! service is available, and otherwise stay in process memory for the session.

mod secrets;
mod store;

pub use secrets::{
    CachingSecrets, KeyringStore, MemorySecretStore, PasswordPersistence, SecretStore,
};
pub use sela_core::{Engine, SavedConnection};
pub use store::{ConfigError, ConfigStore};
