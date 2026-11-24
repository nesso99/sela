//! Query history domain types and pure helpers (no SQLite / GPUI).
//!
//! Persistence lives in `sela-config`. The UI formats times for the panel with
//! [`format_history_when`] and [`format_history_duration`].

use chrono::{DateTime, Local, NaiveDate, Utc};

use crate::sql::is_safe_to_rerun_for_load_more;
use crate::types::RowCountKind;

/// Default global cap for `history.sqlite` (oldest rows drop first).
pub const HISTORY_ROW_CAP: usize = 10_000;

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
    !sql.trim().is_empty()
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
