//! Identifier quoting, statement splitting, and paging SQL.
//!
//! Birch generates these strings. User SQL is passed through after it has been
//! checked for a single statement.

use super::types::Dialect;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SqlError {
    #[error("the SQL editor is empty")]
    Empty,
    #[error("run one statement at a time")]
    Multiple,
    #[error("identifier is empty")]
    EmptyIdent,
    #[error("identifier contains a null byte")]
    NullByte,
}

pub fn quote_ident(dialect: Dialect, ident: &str) -> Result<String, SqlError> {
    if ident.is_empty() {
        return Err(SqlError::EmptyIdent);
    }
    if ident.contains('\0') {
        return Err(SqlError::NullByte);
    }
    match dialect {
        Dialect::Postgres | Dialect::Sqlite => Ok(format!("\"{}\"", ident.replace('"', "\"\""))),
        Dialect::Mysql => Ok(format!("`{}`", ident.replace('`', "``"))),
    }
}

pub fn qualify(dialect: Dialect, schema: &str, table: &str) -> Result<String, SqlError> {
    Ok(format!(
        "{}.{}",
        quote_ident(dialect, schema)?,
        quote_ident(dialect, table)?
    ))
}

pub fn select_all(dialect: Dialect, schema: &str, table: &str) -> Result<String, SqlError> {
    Ok(format!(
        "SELECT * FROM {}",
        qualify(dialect, schema, table)?
    ))
}

/// Wrap a single query so the engine applies `LIMIT`/`OFFSET`.
///
/// The inner statement is placed on its own lines so a trailing `--` comment
/// cannot swallow the closing parenthesis.
pub fn wrap_page(dialect: Dialect, sql: &str, limit: u32, offset: u64) -> String {
    let _ = dialect;
    format!("SELECT * FROM (\n{sql}\n) AS birch_page LIMIT {limit} OFFSET {offset}")
}

pub fn wrap_count(dialect: Dialect, sql: &str) -> String {
    let _ = dialect;
    format!("SELECT COUNT(*) AS birch_count FROM (\n{sql}\n) AS birch_page")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatementKind {
    /// Returns a row set and can be paged by wrapping it.
    PageableQuery,
    /// Returns rows, but wrapping it (SHOW, EXPLAIN, PRAGMA) is not reliable.
    RawQuery,
    Execute,
}

pub fn classify(sql: &str) -> StatementKind {
    match first_keyword(sql).as_deref() {
        Some("select" | "with" | "values") => StatementKind::PageableQuery,
        Some("show" | "explain" | "describe" | "desc" | "pragma" | "table") => {
            StatementKind::RawQuery
        }
        _ => StatementKind::Execute,
    }
}

pub fn first_keyword(sql: &str) -> Option<String> {
    let stripped = strip_comments(sql);
    let token = stripped.split_whitespace().next()?;
    let keyword: String = token
        .chars()
        .take_while(|ch| ch.is_ascii_alphabetic())
        .collect();
    if keyword.is_empty() {
        None
    } else {
        Some(keyword.to_ascii_lowercase())
    }
}

/// Validate that `sql` holds one statement and return it without the trailing semicolon.
pub fn normalize_sql(sql: &str) -> Result<String, SqlError> {
    let statements = split_statements(sql)?;
    let statement = statements.first().ok_or(SqlError::Empty)?;
    if statements.len() != 1 {
        return Err(SqlError::Multiple);
    }
    Ok(statement.clone())
}

/// Split SQL into statements, ignoring semicolons inside quotes and comments.
pub fn split_statements(sql: &str) -> Result<Vec<String>, SqlError> {
    let mut statements = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = sql.chars().collect();
    let mut i = 0;
    let mut quote: Option<char> = None;
    while i < chars.len() {
        let ch = chars[i];
        if let Some(q) = quote {
            current.push(ch);
            if ch == q {
                if q != '\'' && q != '"' && q != '`' {
                    quote = None;
                } else if i + 1 < chars.len() && chars[i + 1] == q {
                    current.push(chars[i + 1]);
                    i += 2;
                    continue;
                } else {
                    quote = None;
                }
            } else if q == '\'' && ch == '\\' && i + 1 < chars.len() {
                current.push(chars[i + 1]);
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }

        if ch == '-' && i + 1 < chars.len() && chars[i + 1] == '-' {
            current.push(ch);
            current.push(chars[i + 1]);
            i += 2;
            while i < chars.len() && chars[i] != '\n' {
                current.push(chars[i]);
                i += 1;
            }
            continue;
        }
        if ch == '/' && i + 1 < chars.len() && chars[i + 1] == '*' {
            current.push(ch);
            current.push(chars[i + 1]);
            i += 2;
            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                current.push(chars[i]);
                i += 1;
            }
            if i + 1 < chars.len() {
                current.push(chars[i]);
                current.push(chars[i + 1]);
                i += 2;
            }
            continue;
        }
        if ch == '\'' || ch == '"' || ch == '`' {
            quote = Some(ch);
            current.push(ch);
            i += 1;
            continue;
        }
        if ch == ';' {
            push_statement(&mut statements, &mut current);
            i += 1;
            continue;
        }
        current.push(ch);
        i += 1;
    }
    if quote.is_some() {
        return Err(SqlError::Multiple);
    }
    push_statement(&mut statements, &mut current);
    if statements.is_empty() {
        return Err(SqlError::Empty);
    }
    Ok(statements)
}

fn push_statement(statements: &mut Vec<String>, current: &mut String) {
    let meaningful = strip_comments(current);
    if !meaningful.trim().is_empty() {
        statements.push(trim_statement(current));
    }
    current.clear();
}

fn trim_statement(sql: &str) -> String {
    sql.trim().trim_end_matches(';').trim().to_string()
}

pub fn strip_comments(sql: &str) -> String {
    let mut out = String::new();
    let chars: Vec<char> = sql.chars().collect();
    let mut i = 0;
    let mut quote: Option<char> = None;
    while i < chars.len() {
        let ch = chars[i];
        if let Some(q) = quote {
            out.push(ch);
            if ch == q {
                if i + 1 < chars.len() && chars[i + 1] == q {
                    out.push(chars[i + 1]);
                    i += 2;
                    continue;
                }
                quote = None;
            }
            i += 1;
            continue;
        }
        if ch == '-' && i + 1 < chars.len() && chars[i + 1] == '-' {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if ch == '/' && i + 1 < chars.len() && chars[i + 1] == '*' {
            i += 2;
            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            i = (i + 2).min(chars.len());
            out.push(' ');
            continue;
        }
        if ch == '\'' || ch == '"' || ch == '`' {
            quote = Some(ch);
        }
        out.push(ch);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_identifiers_per_dialect() {
        assert_eq!(
            quote_ident(Dialect::Postgres, "public").unwrap(),
            "\"public\""
        );
        assert_eq!(
            quote_ident(Dialect::Postgres, "a\"b").unwrap(),
            "\"a\"\"b\""
        );
        assert_eq!(quote_ident(Dialect::Mysql, "a`b").unwrap(), "`a``b`");
        assert_eq!(
            qualify(Dialect::Sqlite, "main", "books").unwrap(),
            "\"main\".\"books\""
        );
        assert!(quote_ident(Dialect::Postgres, "").is_err());
        assert!(quote_ident(Dialect::Mysql, "a\0b").is_err());
    }

    #[test]
    fn normalize_rejects_empty_and_multiple_statements() {
        assert_eq!(normalize_sql("   ").unwrap_err(), SqlError::Empty);
        assert_eq!(
            normalize_sql("select 1; select 2").unwrap_err(),
            SqlError::Multiple
        );
        assert_eq!(normalize_sql("select 'a;b'").unwrap(), "select 'a;b'");
        assert_eq!(normalize_sql("select 1;").unwrap(), "select 1");
        assert_eq!(
            normalize_sql("-- note\nselect 1").unwrap(),
            "-- note\nselect 1"
        );
    }

    #[test]
    fn classifies_and_wraps_without_eating_line_comments() {
        assert_eq!(classify("select 1"), StatementKind::PageableQuery);
        assert_eq!(
            classify("WITH t AS (select 1) select * from t"),
            StatementKind::PageableQuery
        );
        assert_eq!(
            classify("/* c */ UPDATE t SET a = 1"),
            StatementKind::Execute
        );
        assert_eq!(
            classify("PRAGMA table_info(books)"),
            StatementKind::RawQuery
        );
        let wrapped = wrap_page(Dialect::Postgres, "select 1 -- tail", 100, 20);
        assert!(wrapped.contains("LIMIT 100 OFFSET 20"));
        assert!(wrapped.contains("\n) AS birch_page"));
        let count = wrap_count(Dialect::Mysql, "select 1");
        assert!(count.starts_with("SELECT COUNT(*)"));
    }

    #[test]
    fn split_keeps_seed_scripts_as_separate_statements() {
        let sql = r#"
            -- header
            CREATE TABLE authors (id INTEGER PRIMARY KEY, name TEXT);
            INSERT INTO authors (id, name) VALUES (1, 'Ada; Lovelace');
            /* done */
        "#;
        let parts = split_statements(sql).unwrap();
        assert_eq!(parts.len(), 2);
        assert!(parts[1].contains("Ada; Lovelace"));
    }
}
