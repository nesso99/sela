//! Classified SSH failures. Mapped to [`sela_core::DbError::Ssh`] by drivers.

use std::path::PathBuf;

/// SSH tunnel or config failure. Never includes passwords or passphrases.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SshError {
    #[error("{0}")]
    Config(String),
    #[error("{0}")]
    Auth(String),
    #[error("{0}")]
    Connection(String),
    /// Recorded host key does not match the server (no override).
    #[error(
        "SSH host key for {host}:{port} does not match known_hosts. Expected: {expected}. Presented: {presented}."
    )]
    HostKeyMismatch {
        host: String,
        port: u16,
        expected: String,
        presented: String,
    },
    /// First-seen host: caller must confirm before the key is written.
    ///
    /// Prefer [`crate::TunnelOpen::UnknownHostKey`] from [`crate::open_tunnel`];
    /// this variant is used when a pending challenge is rejected or mapped to
    /// [`sela_core::DbError`].
    #[error("SSH host key for {host}:{port} is not in known_hosts (fingerprint {fingerprint}). Confirm before trusting it.")]
    UnknownHostKey {
        host: String,
        port: u16,
        fingerprint: String,
    },
    #[error("{0}")]
    Unsupported(String),
    #[error("{0}")]
    Io(String),
}

impl SshError {
    pub fn config(message: impl Into<String>) -> Self {
        Self::Config(message.into())
    }

    pub fn auth(message: impl Into<String>) -> Self {
        Self::Auth(message.into())
    }

    pub fn connection(message: impl Into<String>) -> Self {
        Self::Connection(message.into())
    }

    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::Unsupported(message.into())
    }

    pub fn io(message: impl Into<String>) -> Self {
        Self::Io(message.into())
    }

    /// Unreachable / timed-out bastion (SSH-08).
    pub fn could_not_reach(host: &str, port: u16) -> Self {
        Self::Connection(format!("SSH: could not reach {host}:{port}."))
    }

    /// Map into [`sela_core::DbError::Ssh`] (US-02: distinct from DB auth/connection).
    pub fn into_db_error(self) -> sela_core::DbError {
        sela_core::DbError::ssh(self.to_string())
    }
}

impl From<std::io::Error> for SshError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err.to_string())
    }
}

/// Expand `~` / `~/…` using an injectable home directory.
pub fn expand_user_path(path: &str, home: Option<&std::path::Path>) -> Result<PathBuf, SshError> {
    let path = path.trim();
    if path.is_empty() {
        return Err(SshError::config("path is empty"));
    }
    if path == "~" {
        let home =
            home.ok_or_else(|| SshError::config("cannot expand ~: home directory is unknown"))?;
        return Ok(home.to_path_buf());
    }
    if let Some(rest) = path.strip_prefix("~/") {
        let home =
            home.ok_or_else(|| SshError::config("cannot expand ~: home directory is unknown"))?;
        return Ok(home.join(rest));
    }
    Ok(PathBuf::from(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    // SSH-02: `~` in a key path expands against the injectable home.
    fn expand_tilde_uses_home() {
        let dir = tempdir().unwrap();
        let path = expand_user_path("~/.ssh/id_ed25519", Some(dir.path())).unwrap();
        assert_eq!(path, dir.path().join(".ssh/id_ed25519"));
    }
}
