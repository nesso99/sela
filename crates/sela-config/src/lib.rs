//! Saved connections and OS keychain passwords.
//!
//! The JSON file stores hosts, ports, users, database names, SQLite paths, TLS
//! paths, and SSH tunnel settings (no secrets). Passwords and SSH passphrases
//! go to the OS keychain when that service is available, and otherwise stay in
//! process memory for the session.

mod history;
mod secrets;
mod settings;
mod store;
mod tabs;

pub use history::{checkpoint_history_connection, HistoryOpen, HistoryStore};
pub use secrets::{
    db_password_account, default_credential_store_is_mock, delete_connection_secrets,
    keychain_unavailable_dialog_warning, memory_only_password_warning, ssh_passphrase_account,
    ssh_password_account, warn_if_mock_credential_store, CachingSecrets, KeyringStore,
    MemorySecretStore, PasswordPersistence, SecretStore,
};
pub use sela_core::{
    Engine, PersistedTab, PersistedTabs, SavedConnection, SshAuthMethod, SshSettings, TabKind,
    TlsMode, TlsSettings,
};
pub use settings::{
    clamp_font_size, content_row_height, editor_line_height, mono_font_size, resolve_theme_mode,
    text_xs_font_size, ui_line_height, AppSettings, SettingsStore, ThemePreference,
    DEFAULT_FONT_SIZE, MAX_FONT_SIZE, MIN_FONT_SIZE,
};
pub use store::{config_dir, ConfigError, ConfigStore, SCHEMA_VERSION};
pub use tabs::TabStore;
