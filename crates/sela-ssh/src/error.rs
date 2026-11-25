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
    ///
    /// Host-key mismatch uses vendor code `host-key-mismatch` and the UX dialog
    /// body (SSH-07) so the UI can show a hard-error alert without a Replace button.
    pub fn into_db_error(self) -> sela_core::DbError {
        match self {
            Self::HostKeyMismatch {
                host,
                expected,
                presented,
                ..
            } => sela_core::DbError::Ssh {
                message: host_key_changed_body(&host, &expected, &presented),
                sqlstate: None,
                vendor_code: Some(HOST_KEY_MISMATCH_VENDOR.into()),
            },
            other => sela_core::DbError::ssh(other.to_string()),
        }
    }
}

/// Vendor code on [`sela_core::DbError::Ssh`] for a changed host key (SSH-07).
pub const HOST_KEY_MISMATCH_VENDOR: &str = "host-key-mismatch";

/// Dialog title for a changed SSH host key (SSH-07).
pub fn host_key_changed_title() -> &'static str {
    "SSH host key changed"
}

/// Dialog body for a changed SSH host key (SSH-07). No override / Replace.
pub fn host_key_changed_body(host: &str, expected: &str, presented: &str) -> String {
    format!(
        "The host key for {host} does not match known_hosts. This can mean the server was rebuilt — or that someone is intercepting the connection.\n\nExpected:\n{expected}\n\nPresented:\n{presented}\n\nSela will not connect. Remove or update the entry for this host in ~/.ssh/known_hosts (or %USERPROFILE%\\.ssh\\known_hosts on Windows), then try again."
    )
}

/// Dialog title for a first-seen SSH host key (SSH-18).
pub fn unknown_host_key_title() -> &'static str {
    "Unknown SSH host key"
}

/// Dialog body for a first-seen SSH host key (SSH-18).
pub fn unknown_host_key_body(host: &str, fingerprint: &str) -> String {
    format!(
        "The host key for {host} is not in known_hosts.\n\nSHA256 fingerprint:\n{fingerprint}\n\nAccept and save this key to known_hosts?"
    )
}

/// True when a [`sela_core::DbError`] is the hard host-key-changed case (SSH-07).
pub fn db_error_is_host_key_mismatch(err: &sela_core::DbError) -> bool {
    matches!(
        err,
        sela_core::DbError::Ssh {
            vendor_code: Some(code),
            ..
        } if code == HOST_KEY_MISMATCH_VENDOR
    )
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

    #[test]
    // SSH-07: mismatch maps to DbError::Ssh with UX body and vendor code.
    fn host_key_mismatch_into_db_error_uses_ux_copy() {
        let err = SshError::HostKeyMismatch {
            host: "bastion.example.com".into(),
            port: 22,
            expected: "SHA256:old".into(),
            presented: "SHA256:new".into(),
        };
        let db = err.into_db_error();
        assert!(db_error_is_host_key_mismatch(&db));
        let msg = db.to_string();
        assert!(msg.contains("Expected:"));
        assert!(msg.contains("Presented:"));
        assert!(msg.contains("Sela will not connect."));
        assert!(msg.contains("SHA256:old"));
        assert!(msg.contains("SHA256:new"));
        assert_eq!(host_key_changed_title(), "SSH host key changed");
        assert_eq!(unknown_host_key_title(), "Unknown SSH host key");
        let body = unknown_host_key_body("bastion.example.com", "SHA256:abc");
        assert!(body.contains("SHA256:abc"));
        assert!(body.contains("Accept and save"));
    }
}
