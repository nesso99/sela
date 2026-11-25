//! SSH tunnel for Sela (M3-1 / US-02).
//!
//! Opens a local port forward through an SSH bastion so Postgres and MySQL
//! drivers dial `127.0.0.1:<ephemeral>`. SQLite does not use this crate.
//!
//! # Host keys
//!
//! - Matching `known_hosts` → connect.
//! - Changed key → [`SshError::HostKeyMismatch`] (hard error, no override).
//! - Unknown key → [`TunnelOpen::UnknownHostKey`] with a fingerprint; the caller
//!   must confirm before [`UnknownHostKey::accept_and_remember`] writes the key.
//!
//! # Paths
//!
//! Pass [`SshPaths`] so tests inject config / known_hosts / home and never touch
//! the real `~/.ssh`.
//!
//! # Secrets
//!
//! Passwords and key passphrases arrive via [`SshAuthSecrets`] from the OS
//! keychain. They are never logged and never written to config JSON.

mod auth;
mod config;
mod error;
mod host_key;
mod paths;
mod tunnel;

pub use auth::SshAuthSecrets;
pub use config::{
    import_host_alias, list_host_aliases, resolve_endpoint, ImportedSshFields, ResolvedSshTarget,
    SshConfigFile, SshEndpointInput,
};
pub use error::{
    db_error_is_host_key_mismatch, expand_user_path, host_key_changed_body, host_key_changed_title,
    unknown_host_key_body, unknown_host_key_title, SshError, HOST_KEY_MISMATCH_VENDOR,
};
pub use host_key::{check_host_key, fingerprint_sha256, learn_host_key, HostKeyCheck};
pub use paths::SshPaths;
pub use tunnel::{
    open_tunnel, SshTunnel, TunnelOpen, TunnelRequest, UnknownHostKey, SSH_CONNECT_TIMEOUT,
};
