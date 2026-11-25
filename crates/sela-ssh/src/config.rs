//! Minimal OpenSSH client config parser.
//!
//! Honours `Host`, `HostName`, `User`, `Port`, and `IdentityFile` (SSH-05).
//! `ProxyJump` / `ProxyCommand` on a matching host are unsupported (clear error).

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use crate::error::{expand_user_path, SshError};
use crate::paths::SshPaths;

/// Resolved bastion endpoint after applying `~/.ssh/config`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSshTarget {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub identity_file: Option<PathBuf>,
}

/// Values taken from a saved connection before config merge.
#[derive(Debug, Clone)]
pub struct SshEndpointInput {
    /// Host or `Host` alias from the connection form.
    pub host: String,
    /// `0` means unset (use config or 22).
    pub port: u16,
    pub user: String,
    pub identity_file: String,
    pub use_ssh_config: bool,
}

#[derive(Debug, Default, Clone)]
struct HostParams {
    hostname: Option<String>,
    user: Option<String>,
    port: Option<u16>,
    identity_file: Option<String>,
    proxy_jump: Option<String>,
    proxy_command: Option<String>,
}

#[derive(Debug)]
struct HostBlock {
    patterns: Vec<String>,
    params: HostParams,
}

/// Parsed `ssh_config` (subset).
#[derive(Debug, Default)]
pub struct SshConfigFile {
    hosts: Vec<HostBlock>,
}

impl SshConfigFile {
    pub fn parse_str(text: &str) -> Result<Self, SshError> {
        let mut hosts = Vec::new();
        let mut current: Option<HostBlock> = None;

        for (idx, raw_line) in text.lines().enumerate() {
            let line_no = idx + 1;
            let line = strip_comment(raw_line).trim();
            if line.is_empty() {
                continue;
            }
            let (key, value) = split_key_value(line)
                .ok_or_else(|| SshError::config(format!("invalid ssh config at line {line_no}")))?;
            let key_lower = key.to_ascii_lowercase();
            if key_lower == "host" {
                if let Some(block) = current.take() {
                    hosts.push(block);
                }
                let patterns = value
                    .split_whitespace()
                    .map(|s| s.to_string())
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>();
                if patterns.is_empty() {
                    return Err(SshError::config(format!(
                        "Host with no patterns at line {line_no}"
                    )));
                }
                current = Some(HostBlock {
                    patterns,
                    params: HostParams::default(),
                });
                continue;
            }
            let block = current.get_or_insert_with(|| HostBlock {
                patterns: vec!["*".into()],
                params: HostParams::default(),
            });
            match key_lower.as_str() {
                "hostname" => block.params.hostname = Some(value.to_string()),
                "user" => block.params.user = Some(value.to_string()),
                "port" => {
                    let port: u16 = value
                        .parse()
                        .map_err(|_| SshError::config(format!("invalid Port at line {line_no}")))?;
                    block.params.port = Some(port);
                }
                "identityfile" => block.params.identity_file = Some(value.to_string()),
                "proxyjump" => block.params.proxy_jump = Some(value.to_string()),
                "proxycommand" => block.params.proxy_command = Some(value.to_string()),
                _ => {
                    // Ignore other keywords (KeepAlive, etc.).
                }
            }
        }
        if let Some(block) = current {
            hosts.push(block);
        }
        Ok(Self { hosts })
    }

    pub fn parse_path(path: &Path) -> Result<Self, SshError> {
        let file = File::open(path).map_err(|err| {
            SshError::config(format!(
                "could not read ssh config {}: {err}",
                path.display()
            ))
        })?;
        let mut text = String::new();
        for line in BufReader::new(file).lines() {
            let line = line.map_err(|err| {
                SshError::config(format!(
                    "could not read ssh config {}: {err}",
                    path.display()
                ))
            })?;
            text.push_str(&line);
            text.push('\n');
        }
        Self::parse_str(&text)
    }

    /// Merge matching Host blocks (first-match wins per OpenSSH for most keywords).
    fn query(&self, alias: &str) -> Result<HostParams, SshError> {
        let mut merged = HostParams::default();
        for block in &self.hosts {
            if !block
                .patterns
                .iter()
                .any(|p| host_pattern_matches(p, alias))
            {
                continue;
            }
            // First match wins for each field (OpenSSH: first obtained value used).
            if merged.hostname.is_none() {
                merged.hostname = block.params.hostname.clone();
            }
            if merged.user.is_none() {
                merged.user = block.params.user.clone();
            }
            if merged.port.is_none() {
                merged.port = block.params.port;
            }
            if merged.identity_file.is_none() {
                merged.identity_file = block.params.identity_file.clone();
            }
            if merged.proxy_jump.is_none() {
                merged.proxy_jump = block.params.proxy_jump.clone();
            }
            if merged.proxy_command.is_none() {
                merged.proxy_command = block.params.proxy_command.clone();
            }
        }
        if merged.proxy_jump.is_some() {
            return Err(SshError::unsupported(
                "ProxyJump in ~/.ssh/config is not supported. Use a single bastion host.",
            ));
        }
        if merged.proxy_command.is_some() {
            return Err(SshError::unsupported(
                "ProxyCommand in ~/.ssh/config is not supported. Use a single bastion host.",
            ));
        }
        Ok(merged)
    }
}

/// Resolve bastion host/port/user/identity using optional ssh config (SSH-05).
pub fn resolve_endpoint(
    input: &SshEndpointInput,
    paths: &SshPaths,
) -> Result<ResolvedSshTarget, SshError> {
    let alias = input.host.trim();
    if alias.is_empty() {
        return Err(SshError::config("SSH host is empty"));
    }

    let mut hostname = alias.to_string();
    let mut user = input.user.trim().to_string();
    let mut port = if input.port == 0 { 22 } else { input.port };
    let mut identity = input.identity_file.trim().to_string();

    if input.use_ssh_config {
        if let Some(config_path) = &paths.config {
            if config_path.is_file() {
                let file = SshConfigFile::parse_path(config_path)?;
                let params = file.query(alias)?;
                if let Some(h) = params.hostname {
                    hostname = h;
                }
                if user.is_empty() {
                    if let Some(u) = params.user {
                        user = u;
                    }
                }
                // Connection port 22 (default) may still be overridden by config
                // when the form left the OpenSSH default; treat 0 as unset only.
                // Explicit non-22 always wins. When form says 22 and config has
                // another Port, prefer config (OpenSSH: command-line Port wins;
                // we treat "22" as the unset default so aliases work).
                if input.port == 0 || input.port == 22 {
                    if let Some(p) = params.port {
                        port = p;
                    }
                }
                if identity.is_empty() {
                    if let Some(id) = params.identity_file {
                        identity = id;
                    }
                }
            }
        }
    }

    if user.is_empty() {
        return Err(SshError::config(
            "SSH user is empty (set User on the connection or in ~/.ssh/config)",
        ));
    }

    let identity_file = if identity.is_empty() {
        None
    } else {
        Some(expand_user_path(&identity, Some(&paths.home))?)
    };

    Ok(ResolvedSshTarget {
        host: hostname,
        port,
        user,
        identity_file,
    })
}

fn strip_comment(line: &str) -> &str {
    // OpenSSH: `#` starts a comment unless inside quotes (we ignore quoted #).
    match line.find('#') {
        Some(i) => &line[..i],
        None => line,
    }
}

fn split_key_value(line: &str) -> Option<(&str, &str)> {
    let line = line.trim();
    if let Some((k, v)) = line.split_once('=') {
        let k = k.trim();
        let v = v.trim().trim_matches('"');
        if k.is_empty() || v.is_empty() {
            return None;
        }
        return Some((k, v));
    }
    let mut parts = line.split_whitespace();
    let k = parts.next()?;
    let rest: Vec<&str> = parts.collect();
    if rest.is_empty() {
        return None;
    }
    // Rejoin so values with spaces (ProxyCommand) stay intact.
    let start = line.find(k)? + k.len();
    let v = line[start..].trim();
    if v.is_empty() {
        return None;
    }
    let _ = rest;
    Some((k, v))
}

fn host_pattern_matches(pattern: &str, host: &str) -> bool {
    if pattern == host {
        return true;
    }
    // Simple glob: `*` and `?` only (enough for common Host aliases).
    let pattern = pattern.as_bytes();
    let host = host.as_bytes();
    match_glob(pattern, host)
}

fn match_glob(pattern: &[u8], text: &[u8]) -> bool {
    let mut p = 0;
    let mut t = 0;
    let mut star_p = None;
    let mut star_t = 0;
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == b'?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            star_p = Some(p);
            star_t = t;
            p += 1;
        } else if let Some(sp) = star_p {
            p = sp + 1;
            star_t += 1;
            t = star_t;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }
    p == pattern.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    // SSH-05: Host alias resolves HostName/User/Port/IdentityFile.
    fn ssh05_host_alias_resolves_fields() {
        let dir = tempdir().unwrap();
        let home = dir.path();
        let ssh_dir = home.join(".ssh");
        std::fs::create_dir_all(&ssh_dir).unwrap();
        let mut f = File::create(ssh_dir.join("config")).unwrap();
        writeln!(
            f,
            "Host jump\n  HostName 10.0.0.5\n  User tunnel\n  Port 2222\n  IdentityFile ~/.ssh/id_ed25519\n"
        )
        .unwrap();
        let paths = SshPaths::from_home(home);
        let resolved = resolve_endpoint(
            &SshEndpointInput {
                host: "jump".into(),
                port: 22,
                user: String::new(),
                identity_file: String::new(),
                use_ssh_config: true,
            },
            &paths,
        )
        .unwrap();
        assert_eq!(resolved.host, "10.0.0.5");
        assert_eq!(resolved.port, 2222);
        assert_eq!(resolved.user, "tunnel");
        assert_eq!(
            resolved.identity_file.as_deref(),
            Some(home.join(".ssh/id_ed25519").as_path())
        );
    }

    #[test]
    fn connection_fields_override_config_user_and_nondefault_port() {
        let dir = tempdir().unwrap();
        let home = dir.path();
        let ssh_dir = home.join(".ssh");
        std::fs::create_dir_all(&ssh_dir).unwrap();
        let mut f = File::create(ssh_dir.join("config")).unwrap();
        writeln!(
            f,
            "Host jump\n  HostName 10.0.0.5\n  User tunnel\n  Port 2222\n"
        )
        .unwrap();
        let paths = SshPaths::from_home(home);
        let resolved = resolve_endpoint(
            &SshEndpointInput {
                host: "jump".into(),
                port: 2200,
                user: "explicit".into(),
                identity_file: String::new(),
                use_ssh_config: true,
            },
            &paths,
        )
        .unwrap();
        assert_eq!(resolved.host, "10.0.0.5");
        assert_eq!(resolved.port, 2200);
        assert_eq!(resolved.user, "explicit");
    }

    #[test]
    fn proxyjump_is_unsupported() {
        let text = "Host jump\n  HostName 10.0.0.5\n  ProxyJump other\n";
        let file = SshConfigFile::parse_str(text).unwrap();
        let err = file.query("jump").unwrap_err();
        assert!(matches!(err, SshError::Unsupported(_)));
        assert!(err.to_string().contains("ProxyJump"));
    }

    #[test]
    fn proxycommand_is_unsupported() {
        let text = "Host jump\n  ProxyCommand ssh -W %h:%p other\n";
        let file = SshConfigFile::parse_str(text).unwrap();
        let err = file.query("jump").unwrap_err();
        assert!(matches!(err, SshError::Unsupported(_)));
    }

    #[test]
    fn glob_patterns() {
        assert!(host_pattern_matches("db-*", "db-prod"));
        assert!(host_pattern_matches("*", "anything"));
        assert!(!host_pattern_matches("db-*", "api-prod"));
    }
}
