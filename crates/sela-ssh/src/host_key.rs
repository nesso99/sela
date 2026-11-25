//! known_hosts verification.
//!
//! - Matching key → allow.
//! - Changed key → hard error ([`SshError::HostKeyMismatch`]); no override.
//! - Unknown key → typed challenge with fingerprint; caller must confirm before
//!   the key is written (SSH-18).

use std::path::Path;
use std::sync::{Arc, Mutex};

use russh::keys::check_known_hosts_path;
use russh::keys::known_hosts::learn_known_hosts_path;
use russh::keys::ssh_key::PublicKey;

use crate::error::SshError;

/// Outcome of comparing the server key to known_hosts.
#[derive(Debug, Clone)]
pub enum HostKeyCheck {
    Match,
    Mismatch { line: usize },
    Unknown { fingerprint: String, key: PublicKey },
}

/// SHA256 fingerprint in OpenSSH `SHA256:…` form (no secrets).
pub fn fingerprint_sha256(key: &PublicKey) -> String {
    format!("{}", key.fingerprint(Default::default()))
}

/// Check `known_hosts` at `path` for `host`:`port`.
pub fn check_host_key(
    host: &str,
    port: u16,
    key: &PublicKey,
    known_hosts: &Path,
) -> Result<HostKeyCheck, SshError> {
    // russh compares PublicKey with `==`, which includes the comment. Entries
    // read from known_hosts have an empty comment, so strip ours first.
    let key_for_check = PublicKey::new(key.key_data().clone(), "");
    match check_known_hosts_path(host, port, &key_for_check, known_hosts) {
        Ok(true) => Ok(HostKeyCheck::Match),
        Ok(false) => Ok(HostKeyCheck::Unknown {
            fingerprint: fingerprint_sha256(key),
            key: key.clone(),
        }),
        Err(russh::keys::Error::KeyChanged { line }) => Ok(HostKeyCheck::Mismatch { line }),
        Err(err) => Err(SshError::config(format!(
            "could not read known_hosts: {err}"
        ))),
    }
}

/// Append a trusted host key to known_hosts (after explicit user accept).
pub fn learn_host_key(
    host: &str,
    port: u16,
    key: &PublicKey,
    known_hosts: &Path,
) -> Result<(), SshError> {
    learn_known_hosts_path(host, port, key, known_hosts).map_err(|err| {
        SshError::io(format!(
            "could not write known_hosts {}: {err}",
            known_hosts.display()
        ))
    })
}

/// Shared state for the russh [`russh::client::Handler`] host-key callback.
#[derive(Debug, Default)]
pub(crate) struct HostKeyGate {
    inner: Mutex<HostKeyGateState>,
}

#[derive(Debug, Default)]
struct HostKeyGateState {
    /// When set, accept this exact key once without writing known_hosts.
    accept_once: Option<PublicKey>,
    last: Option<HostKeyCheck>,
}

impl HostKeyGate {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(HostKeyGateState::default()),
        })
    }

    pub fn with_accept_once(key: PublicKey) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(HostKeyGateState {
                accept_once: Some(key),
                last: None,
            }),
        })
    }

    pub fn take_last(&self) -> Option<HostKeyCheck> {
        self.inner.lock().expect("host key gate").last.take()
    }

    pub fn decide(
        &self,
        host: &str,
        port: u16,
        key: &PublicKey,
        known_hosts: &Path,
    ) -> Result<bool, SshError> {
        let mut state = self.inner.lock().expect("host key gate");
        if let Some(accepted) = &state.accept_once {
            if accepted == key {
                state.last = Some(HostKeyCheck::Match);
                return Ok(true);
            }
        }
        let check = check_host_key(host, port, key, known_hosts)?;
        let allow = matches!(check, HostKeyCheck::Match);
        state.last = Some(check);
        Ok(allow)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::tempdir;

    fn public_key_from_ssh_keygen(dir: &Path, name: &str) -> PublicKey {
        let path = dir.join(name);
        let status = Command::new("ssh-keygen")
            .args([
                "-t",
                "ed25519",
                "-N",
                "",
                "-f",
                path.to_str().expect("utf-8 path"),
                "-q",
            ])
            .status()
            .expect("ssh-keygen must be installed for unit tests");
        assert!(status.success());
        let pub_text = std::fs::read_to_string(path.with_extension("pub")).unwrap();
        PublicKey::from_openssh(pub_text.trim()).expect("parse public key")
    }

    #[test]
    // SSH-06 / SSH-07 / SSH-18 unit: match, mismatch, unknown.
    fn host_key_match_mismatch_unknown() {
        let dir = tempdir().unwrap();
        let key = public_key_from_ssh_keygen(dir.path(), "k1");
        let known = dir.path().join("known_hosts");
        learn_host_key("bastion", 22, &key, &known).unwrap();

        assert!(matches!(
            check_host_key("bastion", 22, &key, &known).unwrap(),
            HostKeyCheck::Match
        ));

        let other = public_key_from_ssh_keygen(dir.path(), "k2");
        match check_host_key("bastion", 22, &other, &known).unwrap() {
            HostKeyCheck::Mismatch { line } => assert!(line >= 1),
            other => panic!("expected mismatch, got {other:?}"),
        }

        match check_host_key("other", 22, &key, &known).unwrap() {
            HostKeyCheck::Unknown { fingerprint, .. } => {
                assert!(fingerprint.starts_with("SHA256:"));
            }
            other => panic!("expected unknown, got {other:?}"),
        }
    }

    #[test]
    fn fingerprint_is_stable_and_not_empty() {
        let dir = tempdir().unwrap();
        let key = public_key_from_ssh_keygen(dir.path(), "k");
        let a = fingerprint_sha256(&key);
        let b = fingerprint_sha256(&key);
        assert_eq!(a, b);
        assert!(a.starts_with("SHA256:"));
        assert!(a.len() > 10);
    }
}
