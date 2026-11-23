use std::borrow::Cow;
use std::time::Duration;

use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime, SecondsFormat, Timelike};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Rows fetched for one page of a grid.
pub const PAGE_SIZE: u32 = 100;

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
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    UInt(u64),
    Float(f64),
    /// Exact decimal (NUMERIC/DECIMAL); never stored as `f64`.
    Decimal(Decimal),
    Text(String),
    Bytes(Vec<u8>),
    Uuid(Uuid),
    Json(serde_json::Value),
    Date(NaiveDate),
    Time(NaiveTime),
    /// Naive timestamp (no timezone).
    Timestamp(NaiveDateTime),
    /// Timestamp with timezone offset.
    TimestampTz(DateTime<FixedOffset>),
    Interval(Interval),
    /// Unknown or unsupported engine type; keep a readable fallback.
    Other {
        type_name: String,
        text: String,
    },
}

impl Value {
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    /// Stable grid formatting: `NULL`, ISO dates/times, compact JSON, hex blobs.
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
            Self::Json(value) => Cow::Owned(compact_json(value)),
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

#[derive(Debug, Clone)]
pub struct RowSet {
    pub page: QueryPage,
    pub total_rows: Option<u64>,
    pub duration: Duration,
    /// False when the statement could not be wrapped in `LIMIT`/`OFFSET`.
    pub paged: bool,
    pub note: Option<String>,
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
        assert_eq!(
            Value::Json(serde_json::json!({"a": 1})).display(),
            "{\"a\":1}"
        );
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
    }
}
