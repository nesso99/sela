//! Minimal OpenSSH client config parser.
//!
//! Honours `Host`, `HostName`, `User`, `Port`, and `IdentityFile` (SSH-05).
//! `Include` under the user’s home is followed one level. `ProxyJump` /
//! `ProxyCommand` on a matching host are unsupported (clear error).

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use crate::error::{expand_user_path, SshError};
use crate::paths::SshPaths;

/// Resolved bastion endpoint after expanding `~` on saved fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSshTarget {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub private_key_path: Option<PathBuf>,
}

/// Values taken from a saved connection (already resolved — no auto config read).
#[derive(Debug, Clone)]
pub struct SshEndpointInput {
    pub host: String,
    /// `0` means unset → 22.
    pub port: u16,
    pub user: String,
    pub private_key_path: String,
}

/// Fields filled by an explicit Import from `~/.ssh/config` (SSH-05).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedSshFields {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub private_key_path: String,
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
        Self::parse_str_with_includes(text, None, 0)
    }

    fn parse_str_with_includes(
        text: &str,
        include_base: Option<&Path>,
        depth: u8,
    ) -> Result<Self, SshError> {
        let mut hosts = Vec::new();
        let mut current: Option<HostBlock> = None;
        // Global Include lines (before any Host) are collected and merged after.
        let mut included = Vec::new();

        for (idx, raw_line) in text.lines().enumerate() {
            let line_no = idx + 1;
            let line = strip_comment(raw_line).trim();
            if line.is_empty() {
                continue;
            }
            let (key, value) = split_key_value(line)
                .ok_or_else(|| SshError::config(format!("invalid ssh config at line {line_no}")))?;
            let key_lower = key.to_ascii_lowercase();
            if key_lower == "include" {
                if depth >= 1 {
                    // One level only (SSH-05).
                    continue;
                }
                if let Some(base) = include_base {
                    for pattern in value.split_whitespace() {
                        let path = expand_include_path(pattern, base)?;
                        if path.is_file() {
                            let nested = read_file_text(&path)?;
                            let nested_file =
                                Self::parse_str_with_includes(&nested, Some(base), depth + 1)?;
                            included.extend(nested_file.hosts);
                        }
                    }
                }
                continue;
            }
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
                _ => {}
            }
        }
        if let Some(block) = current {
            hosts.push(block);
        }
        // Included Host blocks are searched after the primary file (OpenSSH:
        // first obtained value wins; primary file is read first).
        hosts.extend(included);
        Ok(Self { hosts })
    }

    pub fn parse_path(path: &Path) -> Result<Self, SshError> {
        let text = read_file_text(path)?;
        let base = path.parent().unwrap_or_else(|| Path::new("."));
        Self::parse_str_with_includes(&text, Some(base), 0)
    }

    /// Non-wildcard Host alias names for an Import picker (SSH-05).
    pub fn list_aliases(&self) -> Vec<String> {
        let mut out = Vec::new();
        for block in &self.hosts {
            for pattern in &block.patterns {
                if pattern.contains('*') || pattern.contains('?') || pattern.contains('!') {
                    continue;
                }
                if !out.iter().any(|a| a == pattern) {
                    out.push(pattern.clone());
                }
            }
        }
        out
    }

    /// Merge matching Host blocks (first-match wins per OpenSSH for most keywords).
    fn query(&self, alias: &str) -> Result<HostParams, SshError> {
        let mut merged = HostParams::default();
        let mut matched = false;
        for block in &self.hosts {
            if !block
                .patterns
                .iter()
                .any(|p| host_pattern_matches(p, alias))
            {
                continue;
            }
            matched = true;
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
        if !matched {
            return Err(SshError::config(format!(
                "No Host '{alias}' in ~/.ssh/config."
            )));
        }
        if let Some(pj) = &merged.proxy_jump {
            // `ProxyJump none` is still rejected in v0.3 (tracked on #72).
            let _ = pj;
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

fn read_file_text(path: &Path) -> Result<String, SshError> {
    let file = File::open(path).map_err(|_| SshError::config("Could not read ~/.ssh/config."))?;
    let mut text = String::new();
    for line in BufReader::new(file).lines() {
        let line = line.map_err(|_| SshError::config("Could not read ~/.ssh/config."))?;
        text.push_str(&line);
        text.push('\n');
    }
    Ok(text)
}

fn expand_include_path(pattern: &str, base: &Path) -> Result<PathBuf, SshError> {
    let expanded = if pattern.starts_with('~') {
        // Include is restricted to under the user’s home (SSH-05).
        let home = base
            .parent()
            .ok_or_else(|| SshError::config("Could not read ~/.ssh/config."))?;
        expand_user_path(pattern, Some(home))?
    } else if Path::new(pattern).is_absolute() {
        PathBuf::from(pattern)
    } else {
        base.join(pattern)
    };
    Ok(expanded)
}

/// Resolve saved Host/Port/User/key for connect (no auto `ssh_config` read).
pub fn resolve_endpoint(
    input: &SshEndpointInput,
    paths: &SshPaths,
) -> Result<ResolvedSshTarget, SshError> {
    let host = input.host.trim();
    if host.is_empty() {
        return Err(SshError::config("SSH host is required."));
    }
    let user = input.user.trim();
    if user.is_empty() {
        return Err(SshError::config("SSH user is required."));
    }
    let port = if input.port == 0 { 22 } else { input.port };
    let private_key_path = if input.private_key_path.trim().is_empty() {
        None
    } else {
        Some(expand_user_path(
            input.private_key_path.trim(),
            Some(&paths.home),
        )?)
    };
    Ok(ResolvedSshTarget {
        host: host.to_string(),
        port,
        user: user.to_string(),
        private_key_path,
    })
}

/// List `Host` aliases from the injectable config path (SSH-05 picker).
pub fn list_host_aliases(paths: &SshPaths) -> Result<Vec<String>, SshError> {
    let Some(config_path) = &paths.config else {
        return Ok(Vec::new());
    };
    if !config_path.is_file() {
        return Err(SshError::config("Could not read ~/.ssh/config."));
    }
    let file = SshConfigFile::parse_path(config_path)?;
    Ok(file.list_aliases())
}

/// Fill resolved SSH fields from a Host alias (explicit Import; SSH-05).
pub fn import_host_alias(alias: &str, paths: &SshPaths) -> Result<ImportedSshFields, SshError> {
    let alias = alias.trim();
    if alias.is_empty() {
        return Err(SshError::config("No Host '' in ~/.ssh/config."));
    }
    let Some(config_path) = &paths.config else {
        return Err(SshError::config("Could not read ~/.ssh/config."));
    };
    if !config_path.is_file() {
        return Err(SshError::config("Could not read ~/.ssh/config."));
    }
    let file = SshConfigFile::parse_path(config_path)?;
    let params = file.query(alias)?;
    let host = params.hostname.unwrap_or_else(|| alias.to_string());
    let port = params.port.unwrap_or(22);
    let user = params.user.unwrap_or_default();
    let private_key_path = params.identity_file.unwrap_or_default();
    Ok(ImportedSshFields {
        host,
        port,
        user,
        private_key_path,
    })
}

fn strip_comment(line: &str) -> &str {
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
        let imported = import_host_alias("jump", &paths).unwrap();
        assert_eq!(imported.host, "10.0.0.5");
        assert_eq!(imported.port, 2222);
        assert_eq!(imported.user, "tunnel");
        assert_eq!(imported.private_key_path, "~/.ssh/id_ed25519");
        let aliases = list_host_aliases(&paths).unwrap();
        assert!(aliases.iter().any(|a| a == "jump"));
    }

    #[test]
    // SSH-05: Include under home followed one level.
    fn ssh05_include_one_level() {
        let dir = tempdir().unwrap();
        let home = dir.path();
        let ssh_dir = home.join(".ssh");
        std::fs::create_dir_all(&ssh_dir).unwrap();
        let mut included = File::create(ssh_dir.join("extra")).unwrap();
        writeln!(
            included,
            "Host jump\n  HostName 10.0.0.5\n  User tunnel\n  Port 2222\n"
        )
        .unwrap();
        let mut f = File::create(ssh_dir.join("config")).unwrap();
        writeln!(f, "Include ~/.ssh/extra\n").unwrap();
        let paths = SshPaths::from_home(home);
        let imported = import_host_alias("jump", &paths).unwrap();
        assert_eq!(imported.host, "10.0.0.5");
        assert_eq!(imported.user, "tunnel");
    }

    #[test]
    fn resolve_endpoint_expands_tilde_without_config() {
        let dir = tempdir().unwrap();
        let home = dir.path();
        let paths = SshPaths::from_home(home);
        let resolved = resolve_endpoint(
            &SshEndpointInput {
                host: "10.0.0.5".into(),
                port: 2222,
                user: "tunnel".into(),
                private_key_path: "~/.ssh/id_ed25519".into(),
            },
            &paths,
        )
        .unwrap();
        assert_eq!(
            resolved.private_key_path.as_deref(),
            Some(home.join(".ssh/id_ed25519").as_path())
        );
    }

    #[test]
    fn missing_alias_is_clear_error() {
        let dir = tempdir().unwrap();
        let home = dir.path();
        let ssh_dir = home.join(".ssh");
        std::fs::create_dir_all(&ssh_dir).unwrap();
        File::create(ssh_dir.join("config")).unwrap();
        let paths = SshPaths::from_home(home);
        let err = import_host_alias("nope", &paths).unwrap_err();
        assert!(err.to_string().contains("No Host 'nope'"));
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
