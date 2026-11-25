//! known_hosts verification.
//!
//! - Matching key → allow.
//! - Changed key → hard error ([`SshError::HostKeyMismatch`]); no override.
//! - Unknown key → typed challenge with fingerprint; caller must confirm before
//!   the key is written (SSH-18).

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use russh::keys::check_known_hosts_path;
use russh::keys::ssh_key::PublicKey;

use crate::error::SshError;

/// Outcome of comparing the server key to known_hosts.
#[derive(Debug, Clone)]
pub enum HostKeyCheck {
    Match,
    Mismatch { expected: String, presented: String },
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
        Err(russh::keys::Error::KeyChanged { line }) => {
            let expected = expected_fingerprint_at_line(known_hosts, line)
                .unwrap_or_else(|| "(unknown)".into());
            Ok(HostKeyCheck::Mismatch {
                expected,
                presented: fingerprint_sha256(key),
            })
        }
        Err(err) => Err(SshError::config(format!(
            "could not read known_hosts: {err}"
        ))),
    }
}

fn expected_fingerprint_at_line(known_hosts: &Path, line: usize) -> Option<String> {
    let text = fs::read_to_string(known_hosts).ok()?;
    let raw = text.lines().nth(line.saturating_sub(1))?;
    // known_hosts: [host]:port type key [comment]
    let parts: Vec<&str> = raw.split_whitespace().collect();
    if parts.len() < 3 {
        return None;
    }
    let encoded = format!("{} {}", parts[1], parts[2]);
    let key = PublicKey::from_openssh(&encoded).ok()?;
    Some(fingerprint_sha256(&key))
}

/// Append a trusted host key to known_hosts (after explicit user accept).
///
/// Creates `~/.ssh` at mode `0700` and the file at `0600` on Unix when missing
/// (SSH-18). Uses OpenSSH `[host]:port` form when `port != 22`.
pub fn learn_host_key(
    host: &str,
    port: u16,
    key: &PublicKey,
    known_hosts: &Path,
) -> Result<(), SshError> {
    if let Some(parent) = known_hosts.parent() {
        if !parent.exists() {
            fs::create_dir_all(parent).map_err(|err| {
                SshError::io(format!("could not create {}: {err}", parent.display()))
            })?;
            #[cfg(unix)]
            {
                let mut perms = fs::metadata(parent)
                    .map_err(|err| {
                        SshError::io(format!("could not stat {}: {err}", parent.display()))
                    })?
                    .permissions();
                perms.set_mode(0o700);
                fs::set_permissions(parent, perms).map_err(|err| {
                    SshError::io(format!("could not chmod 0700 {}: {err}", parent.display()))
                })?;
            }
        }
    }

    let host_field = if port == 22 {
        host.to_string()
    } else {
        format!("[{host}]:{port}")
    };
    // Strip comment so the written line matches OpenSSH / russh expectations.
    let key_line = PublicKey::new(key.key_data().clone(), "");
    let line = format!(
        "{} {}\n",
        host_field,
        key_line
            .to_openssh()
            .map_err(|err| SshError::io(format!("could not encode host key: {err}")))?
    );

    let mut opts = OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        opts.mode(0o600);
    }
    let mut file = opts.open(known_hosts).map_err(|err| {
        SshError::io(format!(
            "could not write known_hosts {}: {err}",
            known_hosts.display()
        ))
    })?;
    file.write_all(line.as_bytes()).map_err(|err| {
        SshError::io(format!(
            "could not write known_hosts {}: {err}",
            known_hosts.display()
        ))
    })?;

    #[cfg(unix)]
    {
        let mut perms = fs::metadata(known_hosts)
            .map_err(|err| {
                SshError::io(format!("could not stat {}: {err}", known_hosts.display()))
            })?
            .permissions();
        perms.set_mode(0o600);
        fs::set_permissions(known_hosts, perms).map_err(|err| {
            SshError::io(format!(
                "could not chmod 0600 {}: {err}",
                known_hosts.display()
            ))
        })?;
    }

    Ok(())
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
            HostKeyCheck::Mismatch {
                expected,
                presented,
            } => {
                assert!(expected.starts_with("SHA256:"));
                assert!(presented.starts_with("SHA256:"));
                assert_ne!(expected, presented);
            }
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
    // SSH-18: non-22 port uses [host]:port; unix modes 0700/0600.
    fn learn_host_key_bracket_port_and_modes() {
        let dir = tempdir().unwrap();
        let ssh_dir = dir.path().join(".ssh");
        let known = ssh_dir.join("known_hosts");
        let key = public_key_from_ssh_keygen(dir.path(), "k");
        learn_host_key("127.0.0.1", 2222, &key, &known).unwrap();
        let text = fs::read_to_string(&known).unwrap();
        assert!(
            text.contains("[127.0.0.1]:2222"),
            "expected bracket form, got {text}"
        );
        #[cfg(unix)]
        {
            let dir_mode = fs::metadata(&ssh_dir).unwrap().permissions().mode() & 0o777;
            let file_mode = fs::metadata(&known).unwrap().permissions().mode() & 0o777;
            assert_eq!(dir_mode, 0o700);
            assert_eq!(file_mode, 0o600);
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
