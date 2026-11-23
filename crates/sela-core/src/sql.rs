//! Identifier quoting, statement splitting, and paging SQL.
//!
//! Sela generates these strings. User SQL is passed through after it has been
//! checked for a single statement.

use crate::types::Dialect;

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
    format!("SELECT * FROM (\n{sql}\n) AS sela_page LIMIT {limit} OFFSET {offset}")
}

pub fn wrap_count(dialect: Dialect, sql: &str) -> String {
    let _ = dialect;
    format!("SELECT COUNT(*) AS sela_count FROM (\n{sql}\n) AS sela_page")
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

/// Whether "Load more" may re-execute `sql` with a higher row cap.
///
/// Re-running is only safe for read-only statements. Anything that might write,
/// take locks, advance sequences, or whose shape we cannot prove is read-only
/// returns `false` (conservative). Live-stream load-more (raising a shared cap on
/// an already-open cursor) does not use this gate.
pub fn is_safe_to_rerun_for_load_more(sql: &str) -> bool {
    let stripped = strip_comments(sql);
    let words = sql_keywords(&stripped);
    let Some(first) = words.first().copied() else {
        return false;
    };
    if keyword_eq(first, "show") {
        return true;
    }
    if keyword_eq(first, "explain") {
        // EXPLAIN ANALYZE actually runs the plan (including DML). Refuse it.
        return !words.get(1).is_some_and(|w| keyword_eq(w, "analyze"));
    }
    if keyword_eq(first, "select") || keyword_eq(first, "values") {
        return !select_or_values_has_side_effects(&words);
    }
    if keyword_eq(first, "with") {
        return !with_has_dml_or_side_effects(&words);
    }
    // DESCRIBE / PRAGMA / TABLE / CALL / DML / unknown — do not re-run.
    false
}

fn sql_keywords(sql: &str) -> Vec<&str> {
    let mut words = Vec::new();
    let mut start: Option<usize> = None;
    for (i, ch) in sql.char_indices() {
        if ch.is_ascii_alphabetic() || ch == '_' {
            if start.is_none() {
                start = Some(i);
            }
        } else if let Some(s) = start.take() {
            words.push(&sql[s..i]);
        }
    }
    if let Some(s) = start {
        words.push(&sql[s..]);
    }
    words
}

fn keyword_eq(word: &str, expected_lower: &str) -> bool {
    word.eq_ignore_ascii_case(expected_lower)
}

fn select_or_values_has_side_effects(words: &[&str]) -> bool {
    has_locking_clause(words)
        || has_mutating_keyword(words)
        || has_sequence_call(words)
        || has_select_into(words)
}

fn with_has_dml_or_side_effects(words: &[&str]) -> bool {
    // Any DML / DDL / CALL inside a CTE makes re-run unsafe.
    has_mutating_keyword(words)
        || has_locking_clause(words)
        || has_sequence_call(words)
        || has_select_into(words)
}

fn has_mutating_keyword(words: &[&str]) -> bool {
    const MUTATING: &[&str] = &[
        "insert", "update", "delete", "merge", "call", "do", "copy", "truncate", "alter", "create",
        "drop", "grant", "revoke", "refresh", "execute", "perform", "replace", "upsert", "vacuum",
        "reindex", "cluster", "listen", "notify", "security", "load",
    ];
    words
        .iter()
        .any(|w| MUTATING.iter().any(|m| keyword_eq(w, m)))
}

fn has_sequence_call(words: &[&str]) -> bool {
    words
        .iter()
        .any(|w| keyword_eq(w, "nextval") || keyword_eq(w, "setval") || keyword_eq(w, "currval"))
}

fn has_select_into(words: &[&str]) -> bool {
    // SELECT … INTO … (Postgres) materializes a table; reject when `into` appears
    // after an initial `select` and before `from` / end — also reject any `into`
    // in a select-list statement as conservative.
    let Some(select_at) = words.iter().position(|w| keyword_eq(w, "select")) else {
        return false;
    };
    words[select_at + 1..].iter().any(|w| keyword_eq(w, "into"))
}

fn has_locking_clause(words: &[&str]) -> bool {
    for i in 0..words.len() {
        if !keyword_eq(words[i], "for") {
            continue;
        }
        let rest = &words[i + 1..];
        if rest
            .first()
            .is_some_and(|w| keyword_eq(w, "update") || keyword_eq(w, "share"))
        {
            return true;
        }
        if rest.len() >= 3
            && keyword_eq(rest[0], "no")
            && keyword_eq(rest[1], "key")
            && keyword_eq(rest[2], "update")
        {
            return true;
        }
        if rest.len() >= 2 && keyword_eq(rest[0], "key") && keyword_eq(rest[1], "share") {
            return true;
        }
    }
    false
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
        assert!(wrapped.contains("\n) AS sela_page"));
        let count = wrap_count(Dialect::Mysql, "select 1");
        assert!(count.starts_with("SELECT COUNT(*)"));
    }

    #[test]
    fn load_more_rerun_allows_only_safe_reads() {
        assert!(is_safe_to_rerun_for_load_more(
            "SELECT id FROM nums ORDER BY id"
        ));
        assert!(is_safe_to_rerun_for_load_more(
            "WITH t AS (SELECT 1 AS id) SELECT * FROM t"
        ));
        assert!(is_safe_to_rerun_for_load_more("VALUES (1), (2), (3)"));
        assert!(is_safe_to_rerun_for_load_more("SHOW search_path"));
        assert!(is_safe_to_rerun_for_load_more("EXPLAIN SELECT 1"));
        assert!(is_safe_to_rerun_for_load_more(
            "EXPLAIN (FORMAT JSON) SELECT 1"
        ));

        assert!(!is_safe_to_rerun_for_load_more(
            "INSERT INTO t (id) VALUES (1) RETURNING id"
        ));
        assert!(!is_safe_to_rerun_for_load_more(
            "WITH x AS (INSERT INTO t VALUES (1) RETURNING id) SELECT * FROM x"
        ));
        assert!(!is_safe_to_rerun_for_load_more(
            "UPDATE t SET a = 1 RETURNING *"
        ));
        assert!(!is_safe_to_rerun_for_load_more(
            "DELETE FROM t RETURNING id"
        ));
        assert!(!is_safe_to_rerun_for_load_more(
            "SELECT * FROM t FOR UPDATE"
        ));
        assert!(!is_safe_to_rerun_for_load_more("SELECT * FROM t FOR SHARE"));
        assert!(!is_safe_to_rerun_for_load_more(
            "SELECT * FROM t FOR NO KEY UPDATE"
        ));
        assert!(!is_safe_to_rerun_for_load_more(
            "SELECT nextval('sela_seq')"
        ));
        assert!(!is_safe_to_rerun_for_load_more("EXPLAIN ANALYZE SELECT 1"));
        assert!(!is_safe_to_rerun_for_load_more(
            "EXPLAIN ANALYZE INSERT INTO t VALUES (1)"
        ));
        assert!(!is_safe_to_rerun_for_load_more("CALL some_proc()"));
        assert!(!is_safe_to_rerun_for_load_more("PRAGMA table_info(books)"));
        assert!(!is_safe_to_rerun_for_load_more("SELECT a INTO b FROM t"));
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
