use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime, SecondsFormat, Timelike};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Rows fetched for one page of a grid / table preview.
pub const PAGE_SIZE: u32 = 100;

/// Default cap for arbitrary user SQL streams (not table preview).
pub const DEFAULT_SQL_ROW_CAP: u32 = 1_000;

/// Rows per channel batch when streaming a query.
pub const STREAM_BATCH_SIZE: usize = 100;

/// Max Unicode scalars shown in a grid cell (one line; newlines become ⏎).
pub const CELL_DISPLAY_MAX_CHARS: usize = 200;

/// Replacement for newlines in truncated cell display.
pub const CELL_NEWLINE_GLYPH: char = '⏎';

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    Postgres,
    Mysql,
    Sqlite,
}

impl Engine {
    pub fn label(self) -> &'static str {
        match self {
            Self::Postgres => "PostgreSQL",
            Self::Mysql => "MySQL",
            Self::Sqlite => "SQLite",
        }
    }

    pub fn short(self) -> &'static str {
        match self {
            Self::Postgres => "PG",
            Self::Mysql => "MY",
            Self::Sqlite => "SL",
        }
    }

    pub fn default_port(self) -> u16 {
        match self {
            Self::Postgres => 5432,
            Self::Mysql => 3306,
            Self::Sqlite => 0,
        }
    }

    pub fn dialect(self) -> Dialect {
        match self {
            Self::Postgres => Dialect::Postgres,
            Self::Mysql => Dialect::Mysql,
            Self::Sqlite => Dialect::Sqlite,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Postgres,
    Mysql,
    Sqlite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelationKind {
    Table,
    View,
}

impl RelationKind {
    pub fn parse(raw: &str) -> Self {
        if raw.eq_ignore_ascii_case("view") || raw.eq_ignore_ascii_case("VIEW") {
            Self::View
        } else {
            Self::Table
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaInfo {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableInfo {
    pub name: String,
    pub kind: RelationKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnMeta {
    pub name: String,
    pub type_name: String,
}

/// Postgres-style interval (months + days + microseconds).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Interval {
    pub months: i32,
    pub days: i32,
    pub microseconds: i64,
}

/// A typed grid cell. `NULL` stays distinct from the text "NULL".
///
/// `Text` / `Json` use `Arc<str>` so clones are cheap and wide payloads are not
/// duplicated when the grid or window retains them. Json is the **raw** engine
/// string; parse only via [`Value::parse_json`] for detail / pretty / copy-as-JSON.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    UInt(u64),
    Float(f64),
    /// Exact decimal (NUMERIC/DECIMAL); never stored as `f64`.
    Decimal(Decimal),
    Text(Arc<str>),
    Bytes(Vec<u8>),
    Uuid(Uuid),
    /// Raw JSON text from the engine (not a parsed tree).
    Json(Arc<str>),
    Date(NaiveDate),
    Time(NaiveTime),
    /// Naive timestamp (no timezone).
    Timestamp(NaiveDateTime),
    /// Timestamp with timezone offset.
    TimestampTz(DateTime<FixedOffset>),
    Interval(Interval),
    /// Unknown, unsupported, or decode-error marker; keep a readable fallback.
    Other {
        type_name: String,
        text: Arc<str>,
    },
}

impl Value {
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    /// Build [`Value::Text`] from any stringy input.
    pub fn text(value: impl Into<Arc<str>>) -> Self {
        Self::Text(value.into())
    }

    /// Store compact JSON text from an already-parsed value (tests / synthetics).
    pub fn json(value: &serde_json::Value) -> Self {
        Self::Json(Arc::from(compact_json(value)))
    }

    /// Store raw JSON text without parsing.
    pub fn json_raw(value: impl Into<Arc<str>>) -> Self {
        Self::Json(value.into())
    }

    /// Marker used when `try_get_raw` fails (must not be confused with SQL NULL).
    pub fn decode_error(type_name: impl Into<String>) -> Self {
        let type_name = type_name.into();
        Self::Other {
            text: Arc::from(format!("<decode error: {type_name}>")),
            type_name,
        }
    }

    /// True for values built with [`Self::decode_error`].
    pub fn is_decode_error(&self) -> bool {
        match self {
            Self::Other { text, type_name } => {
                text.as_ref() == format!("<decode error: {type_name}>")
            }
            _ => false,
        }
    }

    /// Unsupported type with no text form (`<{type_name}>`), distinct from NULL
    /// and from a hard decode-error marker.
    pub fn is_unsupported_placeholder(&self) -> bool {
        match self {
            Self::Other { text, type_name } => text.as_ref() == format!("<{type_name}>"),
            _ => false,
        }
    }

    pub fn json_str(&self) -> Option<&str> {
        match self {
            Self::Json(text) => Some(text),
            _ => None,
        }
    }

    /// Lazily parse Json (detail view / pretty print / copy as JSON).
    pub fn parse_json(&self) -> Result<serde_json::Value, serde_json::Error> {
        match self {
            Self::Json(text) => serde_json::from_str(text),
            _ => serde_json::from_str("null"),
        }
    }

    /// Full, untruncated display for copy / detail. Grid cells use
    /// [`truncate_one_line`] (via the result window cache) instead.
    pub fn display(&self) -> Cow<'_, str> {
        match self {
            Self::Null => Cow::Borrowed("NULL"),
            Self::Bool(value) => Cow::Owned(value.to_string()),
            Self::Int(value) => Cow::Owned(value.to_string()),
            Self::UInt(value) => Cow::Owned(value.to_string()),
            Self::Float(value) => Cow::Owned(format_float(*value)),
            Self::Decimal(value) => Cow::Owned(value.normalize().to_string()),
            Self::Text(text) => Cow::Borrowed(text),
            Self::Bytes(bytes) => Cow::Owned(format_bytes(bytes)),
            Self::Uuid(value) => Cow::Owned(value.to_string()),
            Self::Json(text) => Cow::Borrowed(text),
            Self::Date(value) => Cow::Owned(value.format("%Y-%m-%d").to_string()),
            Self::Time(value) => Cow::Owned(format_time(value)),
            Self::Timestamp(value) => Cow::Owned(format_timestamp(value)),
            Self::TimestampTz(value) => {
                Cow::Owned(value.to_rfc3339_opts(SecondsFormat::AutoSi, true))
            }
            Self::Interval(value) => Cow::Owned(format_interval(value)),
            Self::Other { text, .. } => Cow::Borrowed(text),
        }
    }

    /// One-line truncated display for a cell renderer (no cache).
    pub fn display_cell(&self) -> String {
        truncate_one_line(&self.display(), CELL_DISPLAY_MAX_CHARS).into_owned()
    }
}

/// Collapse newlines to ⏎ and truncate to `max_chars` Unicode scalars (UTF-8 safe).
///
/// When truncated, an ellipsis `…` is appended (the ellipsis does not count toward
/// `max_chars`). Multi-byte characters are never split.
pub fn truncate_one_line(input: &str, max_chars: usize) -> Cow<'_, str> {
    let mut char_count = 0usize;
    let mut needs_owned = false;
    for c in input.chars() {
        char_count += 1;
        if c == '\n' || c == '\r' || char_count > max_chars {
            needs_owned = true;
            break;
        }
    }
    if !needs_owned {
        return Cow::Borrowed(input);
    }

    let mut out = String::with_capacity(max_chars.min(input.len()) + 4);
    let mut count = 0usize;
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if count >= max_chars {
            out.push('…');
            break;
        }
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push(CELL_NEWLINE_GLYPH);
                count += 1;
            }
            '\n' => {
                out.push(CELL_NEWLINE_GLYPH);
                count += 1;
            }
            other => {
                out.push(other);
                count += 1;
            }
        }
    }
    Cow::Owned(out)
}

fn format_float(value: f64) -> String {
    if value.is_finite() {
        let text = format!("{value}");
        if text.contains('.') {
            text
        } else {
            format!("{value:.1}")
        }
    } else {
        value.to_string()
    }
}

fn format_bytes(bytes: &[u8]) -> String {
    const MAX: usize = 24;
    let shown = bytes.len().min(MAX);
    let hex: String = bytes[..shown]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if bytes.len() > MAX {
        format!("x'{hex}…' ({} bytes)", bytes.len())
    } else {
        format!("x'{hex}'")
    }
}

fn compact_json(value: &serde_json::Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".into())
}

fn format_time(value: &NaiveTime) -> String {
    if value.nanosecond() == 0 {
        value.format("%H:%M:%S").to_string()
    } else {
        value.format("%H:%M:%S%.f").to_string()
    }
}

fn format_timestamp(value: &NaiveDateTime) -> String {
    if value.time().nanosecond() == 0 {
        value.format("%Y-%m-%dT%H:%M:%S").to_string()
    } else {
        value.format("%Y-%m-%dT%H:%M:%S%.f").to_string()
    }
}

fn format_interval(value: &Interval) -> String {
    let mut parts = Vec::new();
    if value.months != 0 {
        parts.push(format!("{} mon", value.months));
    }
    if value.days != 0 {
        parts.push(format!("{} days", value.days));
    }
    let micros = value.microseconds;
    if micros != 0 || parts.is_empty() {
        let negative = micros < 0;
        let abs = micros.unsigned_abs();
        let hours = abs / 3_600_000_000;
        let minutes = (abs % 3_600_000_000) / 60_000_000;
        let seconds = (abs % 60_000_000) / 1_000_000;
        let frac = abs % 1_000_000;
        let mut time = if frac == 0 {
            format!("{hours}:{minutes:02}:{seconds:02}")
        } else {
            format!("{hours}:{minutes:02}:{seconds:02}.{frac:06}")
        };
        if negative {
            time.insert(0, '-');
        }
        parts.push(time);
    }
    parts.join(" ")
}

#[derive(Debug, Clone)]
pub struct QueryPage {
    pub columns: Vec<ColumnMeta>,
    pub rows: Vec<Vec<Value>>,
    pub duration: Duration,
}

/// How [`RowSet::total_rows`] / the grid row count should be interpreted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RowCountKind {
    /// Exact `COUNT(*)` (or a fully consumed stream with no cap).
    Exact,
    /// Engine estimate (e.g. `pg_class.reltuples`, MySQL `table_rows`).
    Estimated,
    /// At least this many rows were seen; more may exist past a cap (`1000+`).
    AtLeast,
    /// No estimate available (typical for SQLite table preview).
    #[default]
    Unknown,
}

#[derive(Debug, Clone)]
pub struct RowSet {
    pub page: QueryPage,
    pub total_rows: Option<u64>,
    pub row_count_kind: RowCountKind,
    pub duration: Duration,
    /// True when table preview uses server `LIMIT`/`OFFSET` paging.
    pub paged: bool,
    /// True when a stream stopped at a row cap and more rows may exist.
    pub truncated: bool,
    pub note: Option<String>,
}

/// One batch from a streaming query (columns on the first batch only).
#[derive(Debug, Clone)]
pub struct StreamBatch {
    pub columns: Option<Vec<ColumnMeta>>,
    pub rows: Vec<Vec<Value>>,
    pub start_index: usize,
    /// Stream reached the current row cap and is waiting for the cap to rise (Load more).
    pub paused_at_cap: bool,
}

/// Summary returned when a stream completes, hits a cap, or is cancelled.
#[derive(Debug, Clone)]
pub struct StreamOutcome {
    pub columns: Vec<ColumnMeta>,
    pub rows_delivered: usize,
    pub truncated: bool,
    pub cancelled: bool,
    pub duration: Duration,
}

#[derive(Debug, Clone)]
pub struct ExecOutcome {
    pub rows_affected: u64,
    pub duration: Duration,
}

#[derive(Debug, Clone)]
pub enum StatementOutcome {
    Rows(RowSet),
    Executed(ExecOutcome),
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use rust_decimal::Decimal;
    use std::str::FromStr;

    #[test]
    fn display_null_bool_numbers_and_text() {
        assert_eq!(Value::Null.display(), "NULL");
        assert_eq!(Value::Bool(true).display(), "true");
        assert_eq!(Value::Int(-7).display(), "-7");
        assert_eq!(Value::UInt(9).display(), "9");
        assert_eq!(Value::Float(1.5).display(), "1.5");
        assert_eq!(Value::Float(2.0).display(), "2.0");
        assert_eq!(
            Value::Decimal(Decimal::from_str("123.4500").unwrap()).display(),
            "123.45"
        );
        assert_eq!(Value::Text("hi".into()).display(), "hi");
    }

    #[test]
    fn display_bytes_uuid_json_temporals() {
        assert_eq!(Value::Bytes(b"hi".to_vec()).display(), "x'6869'");
        let id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        assert_eq!(
            Value::Uuid(id).display(),
            "550e8400-e29b-41d4-a716-446655440000"
        );
        let json = Value::json(&serde_json::json!({"a": 1}));
        assert_eq!(json.display(), "{\"a\":1}");
        assert_eq!(json.parse_json().unwrap(), serde_json::json!({"a": 1}));
        // Raw storage stays unparsed until asked.
        let raw = Value::json_raw("{\n  \"a\": 1\n}");
        assert_eq!(raw.display().as_ref(), "{\n  \"a\": 1\n}");
        assert_eq!(raw.parse_json().unwrap(), serde_json::json!({"a": 1}));
        assert_eq!(
            Value::Date(NaiveDate::from_ymd_opt(2024, 1, 2).unwrap()).display(),
            "2024-01-02"
        );
        assert_eq!(
            Value::Time(NaiveTime::from_hms_opt(3, 4, 5).unwrap()).display(),
            "03:04:05"
        );
        assert_eq!(
            Value::Timestamp(
                NaiveDate::from_ymd_opt(2024, 1, 2)
                    .unwrap()
                    .and_hms_opt(3, 4, 5)
                    .unwrap()
            )
            .display(),
            "2024-01-02T03:04:05"
        );
        let tz = FixedOffset::east_opt(0)
            .unwrap()
            .with_ymd_and_hms(2024, 1, 2, 3, 4, 5)
            .unwrap();
        assert_eq!(Value::TimestampTz(tz).display(), "2024-01-02T03:04:05Z");
        assert_eq!(
            Value::Interval(Interval {
                months: 1,
                days: 2,
                microseconds: 3_600_000_000,
            })
            .display(),
            "1 mon 2 days 1:00:00"
        );
        assert_eq!(
            Value::Other {
                type_name: "point".into(),
                text: "(1,2)".into(),
            }
            .display(),
            "(1,2)"
        );
        assert!(!Value::decode_error("TEXT").is_null());
        assert!(Value::decode_error("TEXT").is_decode_error());
        assert!(!Value::Null.is_decode_error());
        assert!(Value::Other {
            type_name: "point".into(),
            text: "<point>".into(),
        }
        .is_unsupported_placeholder());
        assert!(!Value::Other {
            type_name: "point".into(),
            text: "(1,2)".into(),
        }
        .is_unsupported_placeholder());
    }

    #[test]
    fn truncate_one_line_newlines_and_utf8_boundaries() {
        assert_eq!(truncate_one_line("hi", 200), "hi");
        assert_eq!(truncate_one_line("a\nb\r\nc", 200), "a⏎b⏎c");
        // 3-byte UTF-8 (日本語) must not be split mid-character.
        let japanese = "あいうえおかきくけこ"; // 10 chars
        let truncated = truncate_one_line(japanese, 3);
        assert_eq!(truncated, "あいう…");
        assert!(truncated.is_char_boundary(truncated.len()));
        // Exactly max_chars, no ellipsis.
        assert_eq!(truncate_one_line("abcd", 4), "abcd");
        // Over max: ellipsis after max_chars scalars.
        assert_eq!(truncate_one_line("abcde", 4), "abcd…");
        // Combining: emoji (may be multiple scalars) still respects char boundaries.
        let emoji = "😀😀😀😀";
        let cut = truncate_one_line(emoji, 2);
        assert_eq!(cut.chars().count(), 3); // two emoji + ellipsis
        assert!(cut.ends_with('…'));
        // display_cell uses the helper; newline inside the window becomes ⏎.
        let wide = Value::text(format!("hello\n{}", "x".repeat(250)));
        let cell = wide.display_cell();
        assert!(cell.chars().count() <= CELL_DISPLAY_MAX_CHARS + 1);
        assert!(cell.starts_with("hello⏎"));
        assert!(cell.ends_with('…'));
        assert_eq!(wide.display().as_ref().len(), 5 + 1 + 250);
    }

    #[test]
    fn json_stays_raw_until_parsed() {
        let value = Value::json_raw(r#"{"keep":"raw","n":2}"#);
        assert_eq!(value.json_str(), Some(r#"{"keep":"raw","n":2}"#));
        assert_eq!(
            value.parse_json().unwrap(),
            serde_json::json!({"keep": "raw", "n": 2})
        );
    }
}
