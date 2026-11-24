//! Query history domain types and pure helpers (no SQLite / GPUI).
//!
//! Persistence lives in `sela-config`. The UI formats times for the panel with
//! [`format_history_when`] and [`format_history_duration`].
//!
//! History stores statement text (including ordinary string literals). Password-
//! setting DDL is skipped rather than redacted; see
//! [`history_sql_contains_password_clause`]. Connection passwords from the
//! keychain are never attached to a history row.

use chrono::{DateTime, Local, NaiveDate, Utc};

use crate::sql::is_safe_to_rerun_for_load_more;
use crate::types::RowCountKind;

/// Default global cap for `history.sqlite` (oldest rows drop first).
pub const HISTORY_ROW_CAP: usize = 10_000;

/// Status-bar note when a finished statement is skipped because it looks like
/// password-setting DDL.
pub const HISTORY_PASSWORD_SKIP_NOTE: &str = "not saved to history (contains a password)";

/// Outcome stored on a history row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryOutcome {
    Success,
    Error,
    Cancelled,
}

impl HistoryOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Error => "error",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "success" => Some(Self::Success),
            "error" => Some(Self::Error),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

/// One finished editor statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub id: String,
    pub connection_id: String,
    pub connection_name: String,
    pub database: String,
    pub sql: String,
    pub started_at: DateTime<Utc>,
    pub duration_ms: u64,
    pub rows_label: String,
    pub outcome: HistoryOutcome,
    pub error_message: String,
}

/// Filters for a history list read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HistoryFilter {
    /// When set, only this connection. `None` means All.
    pub connection_id: Option<String>,
    pub outcome: Option<HistoryOutcome>,
    /// FTS5 query over `sql` only. Empty means no text filter.
    pub search: String,
}

/// Whether an editor statement should become a history row.
pub fn should_record_history_sql(sql: &str) -> bool {
    !sql.trim().is_empty() && !history_sql_contains_password_clause(sql)
}

/// True when `sql` looks like password-setting DDL (case-insensitive).
///
/// Comments and string / dollar-quoted literals are blanked first where
/// practical, so `SELECT password_hash` and a `PASSWORD` mention only inside a
/// comment still return false. Matches CREATE/ALTER USER|ROLE … PASSWORD,
/// adjacent `IDENTIFIED BY`, `IDENTIFIED WITH <plugin> BY` (words may sit
/// between `IDENTIFIED` and `BY`), SET PASSWORD, and ALTER USER … WITH
/// PASSWORD. Does not redact; callers skip recording instead.
pub fn history_sql_contains_password_clause(sql: &str) -> bool {
    let text = blank_comments_and_string_literals(sql);
    let words = history_sql_words(&text);
    if words.is_empty() {
        return false;
    }
    for i in 0..words.len() {
        if keyword_eq(&words[i], "identified") && identified_clause_sets_password(&words, i) {
            return true;
        }
        if keyword_eq(&words[i], "set")
            && words.get(i + 1).is_some_and(|w| keyword_eq(w, "password"))
        {
            return true;
        }
    }
    let mut expect_password = false;
    for i in 0..words.len() {
        if !expect_password {
            let is_create_or_alter =
                keyword_eq(&words[i], "create") || keyword_eq(&words[i], "alter");
            let next_user_or_role = words
                .get(i + 1)
                .is_some_and(|w| keyword_eq(w, "user") || keyword_eq(w, "role"));
            if is_create_or_alter && next_user_or_role {
                expect_password = true;
            }
        } else if keyword_eq(&words[i], "password") {
            return true;
        }
    }
    false
}

/// `IDENTIFIED BY` or `IDENTIFIED WITH <plugin…> BY` (MySQL auth plugins).
///
/// Non-adjacent `IDENTIFIED` … `BY` only counts when `WITH` introduces the
/// plugin clause, so ordinary SQL like `SELECT identified FROM t ORDER BY id`
/// is not treated as password DDL.
fn identified_clause_sets_password(words: &[String], identified_at: usize) -> bool {
    let Some(next) = words.get(identified_at + 1) else {
        return false;
    };
    if keyword_eq(next, "by") {
        return true;
    }
    if !keyword_eq(next, "with") {
        return false;
    }
    for word in &words[identified_at + 2..] {
        if keyword_eq(word, "by") {
            return true;
        }
        if identified_with_clause_terminator(word) {
            return false;
        }
    }
    false
}

fn identified_with_clause_terminator(word: &str) -> bool {
    keyword_eq(word, "as")
        || keyword_eq(word, "require")
        || keyword_eq(word, "account")
        || keyword_eq(word, "identified")
        || keyword_eq(word, "create")
        || keyword_eq(word, "alter")
        || keyword_eq(word, "set")
        || keyword_eq(word, "select")
        || keyword_eq(word, "insert")
        || keyword_eq(word, "update")
        || keyword_eq(word, "delete")
        || keyword_eq(word, "grant")
        || keyword_eq(word, "revoke")
        || keyword_eq(word, "drop")
}

fn keyword_eq(word: &str, expected_lower: &str) -> bool {
    word.eq_ignore_ascii_case(expected_lower)
}

fn history_sql_words(sql: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    for ch in sql.chars() {
        // Digits stay in the token so names like `caching_sha2_password` are one
        // word (splitting on `2` used to yield `_password` and miss the clause).
        if ch.is_ascii_alphanumeric() || ch == '_' {
            current.push(ch);
        } else if !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

/// Drop `--` / `/* */` comments and blank string / dollar-quote interiors so
/// password DDL keywords are matched only outside literals.
fn blank_comments_and_string_literals(sql: &str) -> String {
    let chars: Vec<char> = sql.chars().collect();
    let mut out = String::with_capacity(sql.len());
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
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
            let quote = ch;
            out.push(quote);
            i += 1;
            while i < chars.len() {
                let c = chars[i];
                if c == quote {
                    if i + 1 < chars.len() && chars[i + 1] == quote {
                        out.push(' ');
                        out.push(' ');
                        i += 2;
                        continue;
                    }
                    out.push(quote);
                    i += 1;
                    break;
                }
                // MySQL-style backslash escape: skip the escaped char.
                if c == '\\' && i + 1 < chars.len() {
                    out.push(' ');
                    out.push(' ');
                    i += 2;
                    continue;
                }
                out.push(' ');
                i += 1;
            }
            continue;
        }
        if ch == '$' {
            if let Some(delim_len) = dollar_quote_opener_len(&chars, i) {
                let delim: String = chars[i..i + delim_len].iter().collect();
                out.push_str(&delim);
                i += delim_len;
                while i + delim_len <= chars.len() {
                    let slice: String = chars[i..i + delim_len].iter().collect();
                    if slice == delim {
                        out.push_str(&delim);
                        i += delim_len;
                        break;
                    }
                    out.push(' ');
                    i += 1;
                }
                continue;
            }
        }
        out.push(ch);
        i += 1;
    }
    out
}

/// Length of a Postgres dollar-quote opener at `i`, or `None` for `$1` placeholders.
fn dollar_quote_opener_len(chars: &[char], i: usize) -> Option<usize> {
    if chars.get(i) != Some(&'$') {
        return None;
    }
    let mut j = i + 1;
    if j < chars.len() && chars[j].is_ascii_digit() {
        return None;
    }
    while j < chars.len() {
        let c = chars[j];
        if c == '$' {
            return Some(j - i + 1);
        }
        if !(c.is_ascii_alphanumeric() || c == '_') {
            return None;
        }
        j += 1;
    }
    None
}

/// Rerun confirms unless the load-more classifier says the statement is safe.
pub fn history_rerun_needs_confirm(sql: &str) -> bool {
    !is_safe_to_rerun_for_load_more(sql)
}

/// Build the `rows_label` string the status line would show for a row result.
pub fn history_rows_label_for_rows(
    kind: RowCountKind,
    total_rows: Option<u64>,
    page_len: usize,
    truncated: bool,
) -> String {
    let rows = total_rows.unwrap_or(page_len as u64);
    match kind {
        RowCountKind::Estimated => format!("~{rows} rows"),
        RowCountKind::AtLeast if truncated => format!("{rows}+ rows"),
        RowCountKind::Unknown => format!("{page_len} rows"),
        _ => format!("{rows} rows"),
    }
}

/// `rows_label` for an EXECUTE / DML result.
pub fn history_rows_label_affected(rows_affected: u64) -> String {
    format!("{rows_affected} rows affected")
}

/// Local-time "When" column: today `HH:MM`, yesterday `Yesterday`, else `YYYY-MM-DD`.
pub fn format_history_when(started_at: DateTime<Utc>, now_local: DateTime<Local>) -> String {
    let local = started_at.with_timezone(&Local);
    let today = now_local.date_naive();
    let day = local.date_naive();
    if day == today {
        return local.format("%H:%M").to_string();
    }
    if day == today.pred_opt().unwrap_or(NaiveDate::MIN) {
        return "Yesterday".into();
    }
    local.format("%Y-%m-%d").to_string()
}

/// Duration column: under 10s as `{ms} ms`, otherwise `{s} s` with one decimal.
pub fn format_history_duration(duration_ms: u64) -> String {
    if duration_ms < 10_000 {
        format!("{duration_ms} ms")
    } else {
        let secs = duration_ms as f64 / 1000.0;
        format!("{secs:.1} s")
    }
}

/// Collapse runs of whitespace for a one-line SQL preview (display only).
pub fn history_sql_preview(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Build an FTS5 MATCH query that keeps each token as a prefix match.
///
/// Returns `None` when there is nothing useful to search (empty / punctuation).
pub fn history_fts_query(search: &str) -> Option<String> {
    let tokens: Vec<String> = search
        .split_whitespace()
        .filter_map(|token| {
            let cleaned: String = token
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
                .collect();
            if cleaned.is_empty() {
                None
            } else {
                // Quote to keep FTS operators in user input from changing the query.
                Some(format!("\"{cleaned}\"*"))
            }
        })
        .collect();
    if tokens.is_empty() {
        None
    } else {
        Some(tokens.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn whitespace_only_sql_is_not_recorded() {
        assert!(!should_record_history_sql(""));
        assert!(!should_record_history_sql("   \n\t  "));
        assert!(should_record_history_sql("select 1"));
    }

    #[test]
    fn password_setting_ddl_is_not_recorded() {
        let positives = [
            "CREATE ROLE x WITH LOGIN PASSWORD 'secret'",
            "ALTER USER x WITH PASSWORD 'secret'",
            "ALTER ROLE x PASSWORD 'secret'",
            "CREATE USER 'u'@'%' IDENTIFIED BY 'secret'",
            "ALTER USER 'u'@'%' IDENTIFIED BY 'secret'",
            "SET PASSWORD FOR 'u'@'%' = 'secret'",
            "create role App WITH PASSWORD 'x'",
            "SET password = PASSWORD('x')",
            // MySQL IDENTIFIED WITH <plugin> BY (issue #32 / QA proposed test #1).
            "CREATE USER 'u'@'%' IDENTIFIED WITH mysql_native_password BY 'secret'",
            "ALTER USER 'u'@'%' IDENTIFIED WITH mysql_native_password BY 'secret'",
            "CREATE USER 'u'@'%' IDENTIFIED WITH caching_sha2_password BY 'secret'",
            "CREATE USER 'u'@'%' IDENTIFIED WITH auth_plugin BY 'secret'",
            "CREATE USER u IDENTIFIED WITH auth_plugin BY 'secret'",
        ];
        for sql in positives {
            assert!(
                history_sql_contains_password_clause(sql),
                "expected password clause: {sql}"
            );
            assert!(
                !should_record_history_sql(sql),
                "must skip recording: {sql}"
            );
        }
    }

    #[test]
    fn password_word_in_select_or_comment_is_still_recorded() {
        let negatives = [
            "SELECT password_hash FROM users",
            "select Password from accounts",
            "SELECT password FROM users",
            "-- CREATE ROLE x WITH PASSWORD 'secret'\nSELECT 1",
            "/* ALTER USER x WITH PASSWORD 'x' */ SELECT 1",
            "SELECT 'IDENTIFIED BY secret' AS demo",
            "ALTER TABLE users ADD COLUMN password TEXT",
            "CREATE TABLE t (password text)",
            // QA proposed test #1 negatives: plugin DDL only inside comment/string.
            "-- CREATE USER 'u'@'%' IDENTIFIED WITH mysql_native_password BY 'secret'\nSELECT 1",
            "SELECT 'CREATE USER u IDENTIFIED WITH caching_sha2_password BY ''secret''' AS demo",
            // IDENTIFIED without BY must not match; ORDER BY must not either.
            "CREATE USER u IDENTIFIED WITH mysql_native_password AS 'hash'",
            "SELECT identified FROM t ORDER BY id",
        ];
        for sql in negatives {
            assert!(
                !history_sql_contains_password_clause(sql),
                "false positive for: {sql}"
            );
            assert!(should_record_history_sql(sql), "must still record: {sql}");
        }
    }

    #[test]
    fn password_clause_ignores_literals_but_not_real_ddl_beside_them() {
        // Keyword only inside a comment / string must not trigger.
        assert!(!history_sql_contains_password_clause(
            "SELECT 1 -- IDENTIFIED WITH mysql_native_password BY 'secret'"
        ));
        assert!(!history_sql_contains_password_clause(
            "SELECT 'IDENTIFIED WITH caching_sha2_password BY secret'"
        ));
        // A real clause after a string/comment that mentions the keywords still matches.
        assert!(history_sql_contains_password_clause(
            "SELECT 'nope'; CREATE USER 'u'@'%' IDENTIFIED WITH mysql_native_password BY 'secret'"
        ));
        assert!(history_sql_contains_password_clause(
            "/* note */ ALTER USER 'u'@'%' IDENTIFIED WITH auth_plugin BY 'secret'"
        ));
    }

    #[test]
    fn rerun_confirm_follows_load_more_classifier() {
        assert!(!history_rerun_needs_confirm("SELECT 1"));
        assert!(!history_rerun_needs_confirm("SHOW search_path"));
        assert!(history_rerun_needs_confirm("DELETE FROM t"));
        assert!(history_rerun_needs_confirm("UPDATE t SET a = 1"));
        assert!(history_rerun_needs_confirm("EXPLAIN ANALYZE SELECT 1"));
        // Known classifier limit: side-effecting functions still look safe.
        assert!(!history_rerun_needs_confirm("SELECT my_func()"));
    }

    #[test]
    fn rows_label_matches_status_shapes() {
        assert_eq!(
            history_rows_label_for_rows(RowCountKind::Exact, Some(3), 3, false),
            "3 rows"
        );
        assert_eq!(
            history_rows_label_for_rows(RowCountKind::Estimated, Some(1000), 100, false),
            "~1000 rows"
        );
        assert_eq!(
            history_rows_label_for_rows(RowCountKind::AtLeast, Some(1000), 1000, true),
            "1000+ rows"
        );
        assert_eq!(history_rows_label_affected(2), "2 rows affected");
        assert_eq!(history_rows_label_affected(0), "0 rows affected");
    }

    #[test]
    fn when_and_duration_formatting() {
        let now = Local.with_ymd_and_hms(2026, 3, 15, 18, 0, 0).unwrap();
        let today = Local
            .with_ymd_and_hms(2026, 3, 15, 10, 5, 0)
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(format_history_when(today, now), "10:05");
        let yesterday = Local
            .with_ymd_and_hms(2026, 3, 14, 12, 0, 0)
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(format_history_when(yesterday, now), "Yesterday");
        let older = Local
            .with_ymd_and_hms(2026, 3, 1, 12, 0, 0)
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(format_history_when(older, now), "2026-03-01");

        assert_eq!(format_history_duration(42), "42 ms");
        assert_eq!(format_history_duration(9_999), "9999 ms");
        assert_eq!(format_history_duration(10_000), "10.0 s");
        assert_eq!(format_history_duration(12_300), "12.3 s");
    }

    #[test]
    fn fts_query_tokenizes_and_skips_noise() {
        assert_eq!(history_fts_query("").as_deref(), None);
        assert_eq!(history_fts_query("   ").as_deref(), None);
        assert_eq!(
            history_fts_query("books author").as_deref(),
            Some("\"books\"* \"author\"*")
        );
        // Punctuation alone does not become a MATCH clause.
        assert_eq!(history_fts_query("* + ( )").as_deref(), None);
    }

    #[test]
    fn sql_preview_collapses_whitespace() {
        assert_eq!(
            history_sql_preview("select  *\nfrom  books"),
            "select * from books"
        );
    }
}
