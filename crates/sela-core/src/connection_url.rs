//! Parse `postgres://`, `mysql://`, and `sqlite:` URLs into connection fields.
//!
//! Never log the input: it may contain a password. The password is returned
//! separately so callers can put it in the masked field / keychain path.

use percent_encoding::percent_decode_str;
use url::{Host, Url};

use crate::types::Engine;
use crate::TlsMode;

/// Fields filled from a connection URL. Password is optional and never stored
/// on [`crate::SavedConnection`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedConnection {
    pub engine: Engine,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub database: String,
    pub file_path: String,
    /// Set only when the URL has a recognised `sslmode` / `ssl-mode`.
    /// `None` means leave the form's TLS mode and certificate paths alone.
    pub tls_mode: Option<TlsMode>,
    /// Present when the URL had userinfo (including an empty password).
    pub password: Option<String>,
    /// Soft notes for the form result line (ignored params, sslmode=allow, …).
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportUrlError {
    UnsupportedScheme,
    NoHost,
    MultipleHosts,
    NoDatabase,
    NoSqlitePath,
    BadPort,
    BadUrl,
}

impl ImportUrlError {
    pub fn message(&self) -> &'static str {
        match self {
            Self::UnsupportedScheme => "Use a postgres://, mysql://, or sqlite: URL.",
            Self::NoHost => "The URL has no host.",
            Self::MultipleHosts => "The URL has more than one host.",
            Self::NoDatabase => "The URL has no database name.",
            Self::NoSqlitePath => "The URL has no file path.",
            Self::BadPort => "Port must be a number from 1 to 65535.",
            Self::BadUrl => "Could not read the URL.",
        }
    }
}

impl std::fmt::Display for ImportUrlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for ImportUrlError {}

/// Reject truncated or non-hex percent-encoding (`%`, `%A`, `%zz`).
fn has_bad_percent_encoding(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return true;
            }
            if !bytes[i + 1].is_ascii_hexdigit() || !bytes[i + 2].is_ascii_hexdigit() {
                return true;
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    false
}

/// Parse a connection URL. Empty input is a no-op for the caller (returns Err
/// only for real parse failures; empty should be checked before calling).
pub fn parse_connection_url(raw: &str) -> Result<ImportedConnection, ImportUrlError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ImportUrlError::BadUrl);
    }
    if has_bad_percent_encoding(trimmed) {
        return Err(ImportUrlError::BadUrl);
    }

    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("sqlite:") {
        return parse_sqlite(trimmed);
    }
    if !(lower.starts_with("postgres://")
        || lower.starts_with("postgresql://")
        || lower.starts_with("mysql://"))
    {
        return Err(ImportUrlError::UnsupportedScheme);
    }

    let url = Url::parse(trimmed).map_err(|_| ImportUrlError::BadUrl)?;
    let engine = match url.scheme() {
        "postgres" | "postgresql" => Engine::Postgres,
        "mysql" => Engine::Mysql,
        _ => return Err(ImportUrlError::UnsupportedScheme),
    };

    // Multiple hosts: URL with comma in host, or empty host with path-only form.
    // IPv6: Host::Ipv6 yields the address without brackets (`::1`, not `[::1]`).
    let host = match url.host() {
        Some(Host::Domain(d)) => d.to_string(),
        Some(Host::Ipv4(a)) => a.to_string(),
        Some(Host::Ipv6(a)) => a.to_string(),
        None => return Err(ImportUrlError::NoHost),
    };
    if host.is_empty() {
        return Err(ImportUrlError::NoHost);
    }
    if host.contains(',') {
        return Err(ImportUrlError::MultipleHosts);
    }

    let default_port = engine.default_port();
    let port = match url.port() {
        Some(0) => return Err(ImportUrlError::BadPort),
        Some(p) if (1..=65535).contains(&p) => p,
        Some(_) => return Err(ImportUrlError::BadPort),
        None => default_port,
    };

    let username = match percent_decode_str(url.username()).decode_utf8() {
        Ok(u) => u.into_owned(),
        Err(_) => return Err(ImportUrlError::BadUrl),
    };

    let password = if url.password().is_some() || !url.username().is_empty() {
        // userinfo present: include password even when empty (`user:@host`).
        // Url::password is None when there is no `:`; treat that as empty when
        // username is set. When neither username nor password, leave None so
        // an edit does not wipe a loaded secret — but `postgres://@host/db` is
        // unusual. Spec: "A password is applied only when the URL has userinfo."
        // `user:@host` sets password empty. No userinfo leaves password alone.
        let has_userinfo = !url.username().is_empty() || url.password().is_some();
        if has_userinfo {
            Some(match url.password() {
                Some(p) => match percent_decode_str(p).decode_utf8() {
                    Ok(s) => s.into_owned(),
                    Err(_) => return Err(ImportUrlError::BadUrl),
                },
                None => String::new(),
            })
        } else {
            None
        }
    } else {
        None
    };

    // Path is `/dbname` or empty.
    let path = url.path().trim_start_matches('/');
    if path.is_empty() {
        return Err(ImportUrlError::NoDatabase);
    }
    let database = match percent_decode_str(path).decode_utf8() {
        Ok(d) => d.into_owned(),
        Err(_) => return Err(ImportUrlError::BadUrl),
    };
    // Reject path that looks like multiple segments for "database name" — keep
    // the whole path as the database name (Postgres allows that rarely).

    let (tls_mode, notes) = parse_tls_query(engine, url.query());

    Ok(ImportedConnection {
        engine,
        host,
        port,
        username,
        database,
        file_path: String::new(),
        tls_mode,
        password,
        notes,
    })
}

fn parse_sqlite(raw: &str) -> Result<ImportedConnection, ImportUrlError> {
    // Accept sqlite:, sqlite://, sqlite:///
    let after_scheme = raw
        .strip_prefix("sqlite:")
        .or_else(|| {
            let lower = raw.to_ascii_lowercase();
            if lower.starts_with("sqlite:") {
                Some(&raw["sqlite:".len()..])
            } else {
                None
            }
        })
        .ok_or(ImportUrlError::UnsupportedScheme)?;

    let path_part = if let Some(rest) = after_scheme.strip_prefix("//") {
        // sqlite:///absolute or sqlite://relative
        if let Some(abs) = rest.strip_prefix('/') {
            // sqlite:///home/... → /home/...
            format!("/{abs}")
        } else if rest.is_empty() {
            String::new()
        } else {
            // sqlite://relative.sqlite → relative.sqlite
            rest.to_string()
        }
    } else {
        // sqlite:relative.sqlite
        after_scheme.to_string()
    };

    // Drop query/fragment if present on the path part.
    let path_only = path_part
        .split_once(['?', '#'])
        .map(|(p, _)| p)
        .unwrap_or(&path_part);

    if path_only.is_empty() || path_only == "/" {
        return Err(ImportUrlError::NoSqlitePath);
    }

    let file_path = match percent_decode_str(path_only).decode_utf8() {
        Ok(p) => p.into_owned(),
        Err(_) => return Err(ImportUrlError::BadUrl),
    };

    Ok(ImportedConnection {
        engine: Engine::Sqlite,
        host: String::new(),
        port: 0,
        username: String::new(),
        database: String::new(),
        file_path,
        tls_mode: None,
        password: None,
        notes: Vec::new(),
    })
}

/// Returns `Some(mode)` only for a recognised sslmode / ssl-mode value.
fn parse_tls_query(engine: Engine, query: Option<&str>) -> (Option<TlsMode>, Vec<String>) {
    let mut tls_mode = None;
    let mut notes = Vec::new();
    let Some(query) = query else {
        return (None, notes);
    };

    let mut ignored = Vec::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = match pair.split_once('=') {
            Some((k, v)) => (k, v),
            None => (pair, ""),
        };
        let key_l = key.to_ascii_lowercase();
        let is_ssl = match engine {
            Engine::Postgres => key_l == "sslmode",
            Engine::Mysql => key_l == "ssl-mode" || key_l == "sslmode",
            Engine::Sqlite => false,
        };
        if !is_ssl {
            ignored.push(key.to_string());
            continue;
        }
        let value_l = percent_decode_str(value)
            .decode_utf8()
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_else(|_| value.to_ascii_lowercase());
        match value_l.as_str() {
            "disable" => tls_mode = Some(TlsMode::Disable),
            "prefer" => tls_mode = Some(TlsMode::Prefer),
            "require" => tls_mode = Some(TlsMode::Require),
            "verify-ca" => tls_mode = Some(TlsMode::VerifyCa),
            "verify-full" => tls_mode = Some(TlsMode::VerifyFull),
            "allow" => {
                tls_mode = Some(TlsMode::Prefer);
                notes.push(format!("{key}={value} was saved as prefer."));
            }
            other => {
                // Leave tls_mode unset so the form keeps its current mode.
                notes.push(format!("Unknown sslmode '{other}'."));
            }
        }
    }
    if !ignored.is_empty() {
        notes.push(format!("Ignored URL parameters: {}.", ignored.join(", ")));
    }
    (tls_mode, notes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn postgres_full_url() {
        let imported = parse_connection_url(
            "postgres://alice%40x:s%3Bcret@db.example:6543/app%2Fdb?sslmode=require&connect_timeout=5",
        )
        .unwrap();
        assert_eq!(imported.engine, Engine::Postgres);
        assert_eq!(imported.host, "db.example");
        assert_eq!(imported.port, 6543);
        assert_eq!(imported.username, "alice@x");
        assert_eq!(imported.password.as_deref(), Some("s;cret"));
        assert_eq!(imported.database, "app/db");
        assert_eq!(imported.tls_mode, Some(TlsMode::Require));
        assert!(imported
            .notes
            .iter()
            .any(|n| n.contains("Ignored URL parameters") && n.contains("connect_timeout")));
    }

    #[test]
    fn postgresql_scheme_and_default_port() {
        let imported = parse_connection_url("postgresql://u@127.0.0.1/sample").unwrap();
        assert_eq!(imported.engine, Engine::Postgres);
        assert_eq!(imported.port, 5432);
        assert_eq!(imported.password.as_deref(), Some(""));
        assert_eq!(imported.tls_mode, None);
    }

    #[test]
    fn mysql_url_and_ssl_mode() {
        let imported =
            parse_connection_url("mysql://root:p@127.0.0.1:3307/shop?ssl-mode=verify-full")
                .unwrap();
        assert_eq!(imported.engine, Engine::Mysql);
        assert_eq!(imported.port, 3307);
        assert_eq!(imported.tls_mode, Some(TlsMode::VerifyFull));
        assert_eq!(imported.password.as_deref(), Some("p"));
    }

    #[test]
    fn sslmode_allow_becomes_prefer() {
        let imported = parse_connection_url("postgres://u@h/db?sslmode=allow").unwrap();
        assert_eq!(imported.tls_mode, Some(TlsMode::Prefer));
        assert!(imported.notes.iter().any(|n| n.contains("saved as prefer")));
    }

    #[test]
    fn no_sslmode_leaves_tls_mode_none() {
        let imported = parse_connection_url("postgres://u@h/db").unwrap();
        assert_eq!(imported.tls_mode, None);
        assert!(imported.notes.is_empty());
    }

    #[test]
    fn unknown_sslmode_leaves_tls_mode_none_with_note() {
        let imported = parse_connection_url("postgres://u@h/db?sslmode=bogus").unwrap();
        assert_eq!(imported.tls_mode, None);
        assert!(imported
            .notes
            .iter()
            .any(|n| n.contains("Unknown sslmode 'bogus'")));
    }

    #[test]
    fn ipv6_host_strips_brackets() {
        let imported = parse_connection_url("postgres://u@[::1]:5432/db").unwrap();
        assert_eq!(imported.host, "::1");
        assert_eq!(imported.port, 5432);
    }

    #[test]
    fn missing_parts_and_invalid() {
        assert_eq!(
            parse_connection_url("jdbc:postgres://h/db").unwrap_err(),
            ImportUrlError::UnsupportedScheme
        );
        assert_eq!(
            parse_connection_url("postgres:///db").unwrap_err(),
            ImportUrlError::NoHost
        );
        assert_eq!(
            parse_connection_url("postgres://a,b/db").unwrap_err(),
            ImportUrlError::MultipleHosts
        );
        assert_eq!(
            parse_connection_url("postgres://h").unwrap_err(),
            ImportUrlError::NoDatabase
        );
        assert_eq!(
            parse_connection_url("postgres://u@h:0/db").unwrap_err(),
            ImportUrlError::BadPort
        );
        // Port above 65535 is rejected by the URL parser (BadUrl) or our check.
        assert!(parse_connection_url("postgres://u@h:99999/db").is_err());
    }

    #[test]
    fn no_userinfo_leaves_password_none() {
        let imported = parse_connection_url("postgres://127.0.0.1/sample").unwrap();
        assert!(imported.password.is_none());
        assert!(imported.username.is_empty());
    }

    #[test]
    fn sqlite_paths() {
        assert_eq!(
            parse_connection_url("sqlite:///home/me/db.sqlite")
                .unwrap()
                .file_path,
            "/home/me/db.sqlite"
        );
        assert_eq!(
            parse_connection_url("sqlite://relative.sqlite")
                .unwrap()
                .file_path,
            "relative.sqlite"
        );
        assert_eq!(
            parse_connection_url("sqlite:relative.sqlite")
                .unwrap()
                .file_path,
            "relative.sqlite"
        );
        assert_eq!(
            parse_connection_url("sqlite:").unwrap_err(),
            ImportUrlError::NoSqlitePath
        );
        assert_eq!(
            parse_connection_url("sqlite:///home/me/my%20db.sqlite")
                .unwrap()
                .file_path,
            "/home/me/my db.sqlite"
        );
    }

    #[test]
    fn bad_percent_encoding() {
        assert_eq!(
            parse_connection_url("postgres://u:%@h/db").unwrap_err(),
            ImportUrlError::BadUrl
        );
        assert_eq!(
            parse_connection_url("postgres://u:%zz@h/db").unwrap_err(),
            ImportUrlError::BadUrl
        );
        assert_eq!(
            parse_connection_url("sqlite:///home/%GGdb").unwrap_err(),
            ImportUrlError::BadUrl
        );
    }
}
