//! Identifier quoting, statement splitting, and paging SQL.
//!
//! Sela generates these strings. User SQL is passed through after it has been
//! checked for a single statement (or split for multi-statement runs).

use std::ops::Range;

use crate::types::Dialect;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SqlError {
    #[error("the SQL editor is empty")]
    Empty,
    #[error("run one statement at a time")]
    Multiple,
    /// A quote was still open at end of input. `offset` is the byte index of
    /// the opening quote in the original buffer.
    #[error("Unclosed quote")]
    UnclosedQuote { offset: usize },
    #[error("identifier is empty")]
    EmptyIdent,
    #[error("identifier contains a null byte")]
    NullByte,
}

/// One statement from [`split_statements`], with its span in the original buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlStatement {
    /// Trimmed statement text without a trailing semicolon.
    pub text: String,
    /// Byte range in the original buffer covering this statement’s region:
    /// from after the previous semicolon (or buffer start) through the last
    /// non-semicolon character of this statement (before a trailing `;`).
    pub range: Range<usize>,
}

impl SqlStatement {
    pub fn as_str(&self) -> &str {
        &self.text
    }
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
    Ok(statement.text.clone())
}

/// Index of the statement that [`secondary-enter`](crate) would run at `offset`.
///
/// The caret belongs to a statement when it sits inside that statement’s
/// [`SqlStatement::range`], or in the whitespace and comments between the
/// previous statement and this one. A caret in the leading comments belongs to
/// the first statement. Returns `None` when `statements` is empty.
pub fn statement_index_at(statements: &[SqlStatement], offset: usize) -> Option<usize> {
    if statements.is_empty() {
        return None;
    }
    for (ix, stmt) in statements.iter().enumerate() {
        let region_end = statements
            .get(ix + 1)
            .map(|next| next.range.start)
            .unwrap_or(usize::MAX);
        if offset < region_end {
            // Before this statement’s region ends (or into the gap before the next).
            if offset >= stmt.range.start || ix == 0 {
                return Some(ix);
            }
        }
    }
    Some(statements.len() - 1)
}

/// Split SQL into statements, ignoring semicolons inside quotes and comments.
///
/// Each item carries the trimmed statement text and its byte range in `sql`.
/// An unclosed quote yields [`SqlError::UnclosedQuote`] with the opening offset.
pub fn split_statements(sql: &str) -> Result<Vec<SqlStatement>, SqlError> {
    let bytes = sql.as_bytes();
    let mut statements = Vec::new();
    let mut region_start = 0usize;
    let mut i = 0usize;
    let mut quote: Option<(u8, usize)> = None;

    while i < bytes.len() {
        if let Some((q, open_at)) = quote {
            let ch = bytes[i];
            if ch == q {
                if q != b'\'' && q != b'"' && q != b'`' {
                    quote = None;
                    i += 1;
                    continue;
                } else if i + 1 < bytes.len() && bytes[i + 1] == q {
                    i += 2;
                    continue;
                } else {
                    quote = None;
                    i += 1;
                    continue;
                }
            } else if q == b'\'' && ch == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            let _ = open_at;
            i += 1;
            continue;
        }

        let ch = bytes[i];
        if ch == b'-' && i + 1 < bytes.len() && bytes[i + 1] == b'-' {
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if ch == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
            continue;
        }
        if ch == b'\'' || ch == b'"' || ch == b'`' {
            quote = Some((ch, i));
            i += 1;
            continue;
        }
        if ch == b';' {
            push_statement(sql, &mut statements, region_start, i)?;
            i += 1;
            region_start = i;
            continue;
        }
        i += 1;
    }

    if let Some((_, open_at)) = quote {
        return Err(SqlError::UnclosedQuote { offset: open_at });
    }
    push_statement(sql, &mut statements, region_start, sql.len())?;
    if statements.is_empty() {
        return Err(SqlError::Empty);
    }
    Ok(statements)
}

fn push_statement(
    sql: &str,
    statements: &mut Vec<SqlStatement>,
    region_start: usize,
    region_end: usize,
) -> Result<(), SqlError> {
    let slice = &sql[region_start..region_end];
    let meaningful = strip_comments(slice);
    if meaningful.trim().is_empty() {
        return Ok(());
    }
    let text = trim_statement(slice);
    // Map trimmed text back into the original buffer for the tight range.
    let trimmed_start = slice.find(&text).unwrap_or(0);
    let start = region_start + trimmed_start;
    let end = start + text.len();
    statements.push(SqlStatement {
        text,
        range: start..end,
    });
    Ok(())
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

/// Parse a Postgres-style `LINE n` position from an engine error message.
///
/// Returns `(line_1_based, optional_column_1_based)` when the message contains
/// a `LINE n:` marker. Column is taken from a following caret (`^`) line when
/// present, measured from the start of the SQL fragment after `LINE n: `.
/// Returns `None` when no position can be parsed — callers must not guess a
/// gutter line.
pub fn parse_postgres_error_position(message: &str) -> Option<(usize, Option<usize>)> {
    let mut line: Option<usize> = None;
    let mut sql_column_origin: Option<usize> = None;
    for raw in message.lines() {
        let trimmed = raw.trim_start();
        if let Some(rest) = trimmed.strip_prefix("LINE ") {
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(n) = digits.parse::<usize>() {
                if n > 0 {
                    line = Some(n);
                    // Postgres aligns the caret under the fragment that follows
                    // "LINE n: " on this same line (including the space after `:`).
                    let prefix = format!("LINE {n}:");
                    sql_column_origin = trimmed.find(&prefix).map(|at| {
                        // Past "LINE n:" and the usual single space.
                        at + prefix.len() + 1
                    });
                    continue;
                }
            }
        }
        if let Some(origin) = sql_column_origin.take() {
            // Caret line keeps leading spaces so it lines up under LINE n: ….
            if let Some(caret_at) = raw.find('^') {
                let only_caret = raw
                    .chars()
                    .all(|c| c == ' ' || c == '\t' || c == '^' || c == '\r');
                if only_caret && caret_at >= origin {
                    let col = caret_at - origin + 1;
                    return Some((line?, Some(col)));
                }
            }
        }
    }
    line.map(|n| (n, None))
}

/// Convert a 1-based line (and optional 1-based column) into a byte range in `sql`.
///
/// When column is `None`, the range covers the whole line (excluding the newline).
/// Returns `None` when the line is out of range.
pub fn line_col_to_byte_range(
    sql: &str,
    line_1based: usize,
    column_1based: Option<usize>,
) -> Option<Range<usize>> {
    if line_1based == 0 {
        return None;
    }
    let mut start = 0usize;
    let mut current = 1usize;
    for (idx, ch) in sql.char_indices() {
        if current == line_1based {
            start = idx;
            break;
        }
        if ch == '\n' {
            current += 1;
            start = idx + 1;
        }
    }
    if current != line_1based {
        return None;
    }
    let line_end = sql[start..]
        .find('\n')
        .map(|rel| start + rel)
        .unwrap_or(sql.len());
    if let Some(col) = column_1based {
        if col == 0 {
            return Some(start..line_end);
        }
        let mut byte = start;
        let mut col_ix = 1usize;
        for (idx, _) in sql[start..line_end].char_indices() {
            if col_ix == col {
                byte = start + idx;
                break;
            }
            col_ix += 1;
            byte = start + idx + sql[start + idx..].chars().next()?.len_utf8();
        }
        // Underline at least one character when possible.
        let end = if byte < line_end {
            let ch_len = sql[byte..]
                .chars()
                .next()
                .map(|c| c.len_utf8())
                .unwrap_or(1);
            (byte + ch_len).min(line_end)
        } else {
            line_end
        };
        Some(byte..end.max(byte))
    } else {
        Some(start..line_end)
    }
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
    fn unclosed_quote_is_distinct_error() {
        let err = split_statements("select 'oops").unwrap_err();
        match err {
            SqlError::UnclosedQuote { offset } => {
                assert_eq!(offset, "select ".len());
            }
            other => panic!("expected UnclosedQuote, got {other:?}"),
        }
        assert_eq!(
            normalize_sql("select \"x").unwrap_err(),
            SqlError::UnclosedQuote { offset: 7 }
        );
    }

    #[test]
    fn split_returns_byte_ranges() {
        let sql = "select 1;  select 2";
        let parts = split_statements(sql).unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].text, "select 1");
        assert_eq!(&sql[parts[0].range.clone()], "select 1");
        assert_eq!(parts[1].text, "select 2");
        assert_eq!(&sql[parts[1].range.clone()], "select 2");
    }

    #[test]
    fn statement_at_caret_uses_gaps_and_leading_comments() {
        let sql = "-- lead\nselect 1;\n  -- mid\nselect 2";
        let parts = split_statements(sql).unwrap();
        assert_eq!(parts.len(), 2);
        // Caret in leading comment → first statement.
        assert_eq!(statement_index_at(&parts, 0), Some(0));
        assert_eq!(statement_index_at(&parts, parts[0].range.start), Some(0));
        // Caret in the gap / mid comment before second → second.
        let mid = sql.find("-- mid").unwrap();
        assert_eq!(statement_index_at(&parts, mid), Some(1));
        assert_eq!(statement_index_at(&parts, parts[1].range.end), Some(1));
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
        assert!(parts[1].text.contains("Ada; Lovelace"));
    }

    #[test]
    fn parse_postgres_line_and_caret() {
        let msg = "ERROR:  syntax error at or near \"FORM\"\nLINE 1: SELECT * FORM books\n                 ^\n";
        // Column is 1-based within the SQL fragment after "LINE 1: " (F of FORM).
        assert_eq!(parse_postgres_error_position(msg), Some((1, Some(10))));

        let line_only = "ERROR: something\nLINE 3: SELECT 1\nDETAIL: more";
        assert_eq!(parse_postgres_error_position(line_only), Some((3, None)));

        assert_eq!(parse_postgres_error_position("no position here"), None);
    }

    #[test]
    fn line_col_maps_to_bytes() {
        let sql = "select 1\nselect 2";
        assert_eq!(line_col_to_byte_range(sql, 1, None), Some(0..8));
        assert_eq!(line_col_to_byte_range(sql, 2, Some(1)), Some(9..10));
        assert_eq!(line_col_to_byte_range(sql, 9, None), None);
    }
}
