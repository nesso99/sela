//! Injectable filesystem locations so tests never touch the real `~/.ssh`.

use std::path::{Path, PathBuf};

use crate::error::SshError;

/// Paths used for SSH config, known_hosts, and `~` expansion.
///
/// Production code uses [`SshPaths::system_default`]. Tests build a temporary
/// `.ssh` tree and pass [`SshPaths::from_home`].
#[derive(Debug, Clone)]
pub struct SshPaths {
    /// Directory used to expand `~` in IdentityFile and similar paths.
    pub home: PathBuf,
    /// OpenSSH client config (`~/.ssh/config`). `None` skips reading config.
    pub config: Option<PathBuf>,
    /// known_hosts file checked (and optionally written) for host keys.
    pub known_hosts: PathBuf,
}

impl SshPaths {
    /// Build paths under `{home}/.ssh`, matching OpenSSH layout.
    pub fn from_home(home: impl AsRef<Path>) -> Self {
        let home = home.as_ref().to_path_buf();
        let ssh_dir = home.join(".ssh");
        Self {
            home,
            config: Some(ssh_dir.join("config")),
            known_hosts: ssh_dir.join("known_hosts"),
        }
    }

    /// Resolve the current user's home (`HOME` / platform home).
    pub fn system_default() -> Result<Self, SshError> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
            .ok_or_else(|| SshError::config("could not determine the home directory"))?;
        Ok(Self::from_home(home))
    }

    /// Override only the known_hosts path (keep config/home).
    pub fn with_known_hosts(mut self, path: impl Into<PathBuf>) -> Self {
        self.known_hosts = path.into();
        self
    }

    /// Disable reading `~/.ssh/config`.
    pub fn without_config(mut self) -> Self {
        self.config = None;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_home_points_at_dot_ssh() {
        let paths = SshPaths::from_home("/tmp/sela-home");
        assert_eq!(paths.home, PathBuf::from("/tmp/sela-home"));
        assert_eq!(
            paths.config.as_deref(),
            Some(Path::new("/tmp/sela-home/.ssh/config"))
        );
        assert_eq!(
            paths.known_hosts,
            PathBuf::from("/tmp/sela-home/.ssh/known_hosts")
        );
    }
}
