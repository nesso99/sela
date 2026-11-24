//! Saved connections and OS keychain passwords.
//!
//! The JSON file stores hosts, ports, users, database names, and SQLite paths.
//! It never stores a password. Passwords go to the OS keychain when that
//! service is available, and otherwise stay in process memory for the session.

mod history;
mod secrets;
mod settings;
mod store;
mod tabs;

pub use history::{checkpoint_history_connection, HistoryOpen, HistoryStore};
pub use secrets::{
    default_credential_store_is_mock, keychain_unavailable_dialog_warning,
    memory_only_password_warning, warn_if_mock_credential_store, CachingSecrets, KeyringStore,
    MemorySecretStore, PasswordPersistence, SecretStore,
};
pub use sela_core::{
    Engine, PersistedTab, PersistedTabs, SavedConnection, TabKind, TlsMode, TlsSettings,
};
pub use settings::{resolve_theme_mode, SettingsStore, ThemePreference};
pub use store::{config_dir, ConfigError, ConfigStore, SCHEMA_VERSION};
pub use tabs::TabStore;
