//! Decode sqlx rows into typed [`sela_core::Value`] cells without panicking.

use std::sync::Arc;

use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use rust_decimal::Decimal;
use sqlx::mysql::MySqlRow;
use sqlx::postgres::PgRow;
use sqlx::sqlite::SqliteRow;
use sqlx::{Column as _, ColumnIndex, Row, TypeInfo, ValueRef};
use uuid::Uuid;

use sela_core::{ColumnMeta, Interval, Value};

pub fn column_meta(column: &impl sqlx::Column) -> ColumnMeta {
    let raw = column.type_info().to_string();
    ColumnMeta {
        name: column.name().to_string(),
        // Never surface SQL NULL / empty as a header type (SQLite undeclared
        // expression columns). Callers refine from the first non-NULL value.
        type_name: normalize_column_type_name(&raw),
    }
}

/// Drop type names that must not appear in headers (`NULL` / empty).
pub(crate) fn normalize_column_type_name(raw: &str) -> String {
    if raw.is_empty() || raw.eq_ignore_ascii_case("NULL") {
        String::new()
    } else if raw.eq_ignore_ascii_case("VOID") {
        // Postgres void (e.g. pg_sleep) — show lowercase per UX.
        "void".into()
    } else {
        raw.to_string()
    }
}

/// SQLite storage-class label for a decoded cell, if it is not SQL NULL.
pub(crate) fn sqlite_storage_class_name(value: &Value) -> Option<&'static str> {
    match value {
        Value::Null => None,
        Value::Int(_) | Value::UInt(_) | Value::Bool(_) => Some("INTEGER"),
        Value::Float(_) | Value::Decimal(_) => Some("REAL"),
        Value::Bytes(_) => Some("BLOB"),
        Value::Text(_)
        | Value::Uuid(_)
        | Value::Json(_)
        | Value::Date(_)
        | Value::Time(_)
        | Value::Timestamp(_)
        | Value::TimestampTz(_)
        | Value::Interval(_)
        | Value::Other { .. } => Some("TEXT"),
    }
}

/// Fill empty / undeclared SQLite column types from the first non-NULL value
/// in each column (#60). Leaves the name empty when every value is NULL.
pub fn refine_sqlite_column_types(columns: &mut [ColumnMeta], rows: &[Vec<Value>]) {
    for (col_ix, column) in columns.iter_mut().enumerate() {
        if !column.type_name.is_empty() && !column.type_name.eq_ignore_ascii_case("NULL") {
            continue;
        }
        let inferred = rows.iter().find_map(|row| {
            row.get(col_ix)
                .and_then(sqlite_storage_class_name)
                .map(str::to_string)
        });
        column.type_name = inferred.unwrap_or_default();
    }
}

pub fn read_postgres_cell(row: &PgRow, index: usize) -> Value {
    let strategy = row
        .columns()
        .get(index)
        .map(|column| crate::pg_decode::strategy_for_type_info(column.type_info()))
        .unwrap_or(crate::pg_decode::PgDecodeStrategy::Fallback);
    crate::pg_decode::read_postgres_cell_with_strategy(row, index, strategy)
}

/// Typed decode for built-in scalars already mapped to [`Value`] variants.
/// Extended Postgres types (arrays, inet, enums, …) are handled by [`crate::pg_decode`].
pub(crate) fn read_postgres_native(row: &PgRow, index: usize, type_name: &str) -> Value {
    match type_name {
        "BOOL" => get_bool(row, index).unwrap_or_else(|| other(type_name, row, index)),
        "INT2" | "INT4" | "INT8" | "OID" => {
            get_int(row, index).unwrap_or_else(|| other(type_name, row, index))
        }
        "FLOAT4" | "FLOAT8" => {
            get_float(row, index).unwrap_or_else(|| other(type_name, row, index))
        }
        "NUMERIC" => get_decimal(row, index).unwrap_or_else(|| other(type_name, row, index)),
        "TEXT" | "VARCHAR" | "NAME" | "XML" | "CITEXT" | "CHAR" | "BPCHAR" | "\"CHAR\"" => {
            get_text(row, index).unwrap_or_else(|| other(type_name, row, index))
        }
        "BYTEA" => get_bytes(row, index).unwrap_or_else(|| other(type_name, row, index)),
        "UUID" => {
            if let Ok(value) = row.try_get::<Uuid, _>(index) {
                Value::Uuid(value)
            } else if let Ok(text) = row.try_get::<String, _>(index) {
                Uuid::parse_str(&text)
                    .map(Value::Uuid)
                    .unwrap_or_else(|_| Value::text(text))
            } else {
                other(type_name, row, index)
            }
        }
        "JSON" | "JSONB" => get_json(row, index).unwrap_or_else(|| other(type_name, row, index)),
        "DATE" => get_date(row, index).unwrap_or_else(|| other(type_name, row, index)),
        "TIME" => get_time(row, index).unwrap_or_else(|| other(type_name, row, index)),
        "TIMESTAMP" => get_timestamp(row, index).unwrap_or_else(|| other(type_name, row, index)),
        "TIMESTAMPTZ" => {
            if let Ok(value) = row.try_get::<DateTime<FixedOffset>, _>(index) {
                Value::TimestampTz(value)
            } else if let Ok(value) = row.try_get::<DateTime<Utc>, _>(index) {
                Value::TimestampTz(utc_to_fixed(value))
            } else {
                other(type_name, row, index)
            }
        }
        "INTERVAL" => match row.try_get::<sqlx::postgres::types::PgInterval, _>(index) {
            Ok(value) => Value::Interval(Interval {
                months: value.months,
                days: value.days,
                microseconds: value.microseconds,
            }),
            Err(_) => other(type_name, row, index),
        },
        _ => other(type_name, row, index),
    }
}

pub fn read_mysql_cell(row: &MySqlRow, index: usize) -> Value {
    let type_name = type_name(row, index);
    if let Some(early) = early_value(probe_null(row, index), &type_name) {
        return early;
    }
    match type_name.as_str() {
        "BOOLEAN" => get_bool(row, index).unwrap_or_else(|| other(&type_name, row, index)),
        "TINYINT" | "SMALLINT" | "MEDIUMINT" | "INT" | "BIGINT" | "YEAR" => {
            get_int(row, index).unwrap_or_else(|| other(&type_name, row, index))
        }
        "TINYINT UNSIGNED" | "SMALLINT UNSIGNED" | "MEDIUMINT UNSIGNED" | "INT UNSIGNED" => {
            if let Ok(value) = row.try_get::<u64, _>(index) {
                Value::UInt(value)
            } else {
                get_int(row, index).unwrap_or_else(|| other(&type_name, row, index))
            }
        }
        "BIGINT UNSIGNED" => match row.try_get::<u64, _>(index) {
            Ok(value) => Value::UInt(value),
            Err(_) => other(&type_name, row, index),
        },
        "FLOAT" | "DOUBLE" => {
            get_float(row, index).unwrap_or_else(|| other(&type_name, row, index))
        }
        "DECIMAL" => get_decimal(row, index).unwrap_or_else(|| other(&type_name, row, index)),
        "CHAR" | "VARCHAR" | "TINYTEXT" | "TEXT" | "MEDIUMTEXT" | "LONGTEXT" | "ENUM" | "SET" => {
            get_text(row, index).unwrap_or_else(|| other(&type_name, row, index))
        }
        "BINARY" | "VARBINARY" | "TINYBLOB" | "BLOB" | "MEDIUMBLOB" | "LONGBLOB" => {
            get_bytes(row, index).unwrap_or_else(|| other(&type_name, row, index))
        }
        "BIT" => {
            if let Ok(value) = row.try_get::<bool, _>(index) {
                Value::Bool(value)
            } else if let Ok(bytes) = row.try_get::<Vec<u8>, _>(index) {
                if bytes.len() == 1 {
                    Value::Bool(bytes[0] != 0)
                } else {
                    Value::Bytes(bytes)
                }
            } else if let Ok(value) = row.try_get::<u64, _>(index) {
                Value::UInt(value)
            } else {
                other(&type_name, row, index)
            }
        }
        "JSON" => get_json(row, index).unwrap_or_else(|| other(&type_name, row, index)),
        "DATE" => get_date(row, index).unwrap_or_else(|| other(&type_name, row, index)),
        "TIME" => get_time(row, index).unwrap_or_else(|| other(&type_name, row, index)),
        "DATETIME" => get_timestamp(row, index).unwrap_or_else(|| other(&type_name, row, index)),
        "TIMESTAMP" => {
            if let Ok(value) = row.try_get::<DateTime<Utc>, _>(index) {
                Value::TimestampTz(utc_to_fixed(value))
            } else if let Ok(value) = row.try_get::<NaiveDateTime, _>(index) {
                Value::Timestamp(value)
            } else {
                other(&type_name, row, index)
            }
        }
        _ => other(&type_name, row, index),
    }
}

pub fn read_sqlite_cell(row: &SqliteRow, index: usize) -> Value {
    let type_name = type_name(row, index);
    if let Some(early) = early_value(probe_null(row, index), &type_name) {
        return early;
    }
    // sqlx reports undeclared / expression columns as type name "NULL" (or
    // empty). That is not SQL NULL — probe the value's storage class instead.
    if type_name.is_empty() || type_name == "NULL" {
        return decode_sqlite_storage_class(row, index, &type_name);
    }
    match type_name.as_str() {
        "INTEGER" | "INT4" => get_int(row, index).unwrap_or_else(|| other(&type_name, row, index)),
        "BOOLEAN" => get_bool(row, index)
            .or_else(|| get_int(row, index))
            .unwrap_or_else(|| other(&type_name, row, index)),
        "REAL" | "FLOAT" | "DOUBLE" => {
            get_float(row, index).unwrap_or_else(|| other(&type_name, row, index))
        }
        // TEXT affinity (TEXT/CHAR/CLOB/VARCHAR…): always Text so leading zeros stay exact.
        "TEXT" => get_text(row, index).unwrap_or_else(|| other(&type_name, row, index)),
        // NUMERIC affinity may store integer, real, or text; probe richer types first.
        "NUMERIC" => {
            if let Some(value) = get_int(row, index) {
                value
            } else if let Some(value) = get_float(row, index) {
                value
            } else {
                get_text(row, index).unwrap_or_else(|| other(&type_name, row, index))
            }
        }
        "BLOB" => get_bytes(row, index).unwrap_or_else(|| other(&type_name, row, index)),
        "DATE" => get_date(row, index)
            .or_else(|| get_text(row, index))
            .unwrap_or_else(|| other(&type_name, row, index)),
        "TIME" => get_time(row, index)
            .or_else(|| get_text(row, index))
            .unwrap_or_else(|| other(&type_name, row, index)),
        "DATETIME" => get_timestamp(row, index)
            .or_else(|| {
                row.try_get::<DateTime<Utc>, _>(index)
                    .ok()
                    .map(|value| Value::TimestampTz(utc_to_fixed(value)))
            })
            .or_else(|| get_text(row, index))
            .unwrap_or_else(|| other(&type_name, row, index)),
        // Unknown declared types: probe storage classes dynamically.
        _ => decode_sqlite_storage_class(row, index, &type_name),
    }
}

/// Decode a SQLite cell from its runtime storage class (INTEGER / REAL / TEXT /
/// BLOB). Used when the declared type is empty, `"NULL"`, or otherwise unknown.
fn decode_sqlite_storage_class(row: &SqliteRow, index: usize, type_name: &str) -> Value {
    if let Some(value) = get_int(row, index) {
        value
    } else if let Some(value) = get_float(row, index) {
        value
    } else if let Some(value) = get_text(row, index) {
        value
    } else if let Some(value) = get_bytes(row, index) {
        value
    } else {
        other(type_name, row, index)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NullProbe {
    Null,
    Present,
    /// `try_get_raw` failed — must not be treated as SQL NULL.
    Error,
}

pub(crate) fn probe_null<R>(row: &R, index: usize) -> NullProbe
where
    R: Row,
    usize: ColumnIndex<R>,
{
    match row.try_get_raw(index) {
        Ok(raw) => {
            if ValueRef::is_null(&raw) {
                NullProbe::Null
            } else {
                NullProbe::Present
            }
        }
        Err(_) => NullProbe::Error,
    }
}

/// Map a null probe to an early-return value. `None` means decode the typed cell.
pub(crate) fn early_value(probe: NullProbe, type_name: &str) -> Option<Value> {
    match probe {
        NullProbe::Null => Some(Value::Null),
        NullProbe::Error => Some(Value::decode_error(type_name)),
        NullProbe::Present => None,
    }
}

pub(crate) fn type_name<R>(row: &R, index: usize) -> String
where
    R: Row,
{
    row.columns()
        .get(index)
        .map(|column| column.type_info().name().to_ascii_uppercase())
        .unwrap_or_else(|| "VALUE".into())
}

pub(crate) fn other<R>(type_name: &str, row: &R, index: usize) -> Value
where
    R: Row,
    usize: ColumnIndex<R>,
    for<'a> String: sqlx::Decode<'a, R::Database> + sqlx::Type<R::Database>,
    for<'a> Vec<u8>: sqlx::Decode<'a, R::Database> + sqlx::Type<R::Database>,
{
    if let Ok(text) = row.try_get::<String, _>(index) {
        return Value::Other {
            type_name: type_name.to_string(),
            text: Arc::from(text),
        };
    }
    if let Ok(bytes) = row.try_get::<Vec<u8>, _>(index) {
        return Value::Bytes(bytes);
    }
    Value::Other {
        type_name: type_name.to_string(),
        text: Arc::from(format!("<{type_name}>")),
    }
}

fn get_bool<R>(row: &R, index: usize) -> Option<Value>
where
    R: Row,
    usize: ColumnIndex<R>,
    for<'a> bool: sqlx::Decode<'a, R::Database> + sqlx::Type<R::Database>,
{
    row.try_get::<bool, _>(index).ok().map(Value::Bool)
}

fn get_int<R>(row: &R, index: usize) -> Option<Value>
where
    R: Row,
    usize: ColumnIndex<R>,
    for<'a> i64: sqlx::Decode<'a, R::Database> + sqlx::Type<R::Database>,
    for<'a> i32: sqlx::Decode<'a, R::Database> + sqlx::Type<R::Database>,
    for<'a> i16: sqlx::Decode<'a, R::Database> + sqlx::Type<R::Database>,
    for<'a> i8: sqlx::Decode<'a, R::Database> + sqlx::Type<R::Database>,
{
    if let Ok(value) = row.try_get::<i64, _>(index) {
        return Some(Value::Int(value));
    }
    if let Ok(value) = row.try_get::<i32, _>(index) {
        return Some(Value::Int(i64::from(value)));
    }
    if let Ok(value) = row.try_get::<i16, _>(index) {
        return Some(Value::Int(i64::from(value)));
    }
    if let Ok(value) = row.try_get::<i8, _>(index) {
        return Some(Value::Int(i64::from(value)));
    }
    None
}

fn get_float<R>(row: &R, index: usize) -> Option<Value>
where
    R: Row,
    usize: ColumnIndex<R>,
    for<'a> f64: sqlx::Decode<'a, R::Database> + sqlx::Type<R::Database>,
    for<'a> f32: sqlx::Decode<'a, R::Database> + sqlx::Type<R::Database>,
{
    if let Ok(value) = row.try_get::<f64, _>(index) {
        return Some(Value::Float(value));
    }
    if let Ok(value) = row.try_get::<f32, _>(index) {
        return Some(Value::Float(f64::from(value)));
    }
    None
}

fn get_decimal<R>(row: &R, index: usize) -> Option<Value>
where
    R: Row,
    usize: ColumnIndex<R>,
    for<'a> Decimal: sqlx::Decode<'a, R::Database> + sqlx::Type<R::Database>,
    for<'a> String: sqlx::Decode<'a, R::Database> + sqlx::Type<R::Database>,
{
    if let Ok(value) = row.try_get::<Decimal, _>(index) {
        return Some(Value::Decimal(value));
    }
    if let Ok(text) = row.try_get::<String, _>(index) {
        if let Ok(value) = text.parse::<Decimal>() {
            return Some(Value::Decimal(value));
        }
        return Some(Value::text(text));
    }
    None
}

fn get_text<R>(row: &R, index: usize) -> Option<Value>
where
    R: Row,
    usize: ColumnIndex<R>,
    for<'a> String: sqlx::Decode<'a, R::Database> + sqlx::Type<R::Database>,
{
    row.try_get::<String, _>(index).ok().map(Value::text)
}

fn get_bytes<R>(row: &R, index: usize) -> Option<Value>
where
    R: Row,
    usize: ColumnIndex<R>,
    for<'a> Vec<u8>: sqlx::Decode<'a, R::Database> + sqlx::Type<R::Database>,
{
    row.try_get::<Vec<u8>, _>(index).ok().map(Value::Bytes)
}

fn get_json<R>(row: &R, index: usize) -> Option<Value>
where
    R: Row,
    usize: ColumnIndex<R>,
    for<'a> serde_json::Value: sqlx::Decode<'a, R::Database> + sqlx::Type<R::Database>,
    for<'a> String: sqlx::Decode<'a, R::Database> + sqlx::Type<R::Database>,
{
    // Prefer the raw engine string so we do not build a serde tree for every cell.
    if let Ok(text) = row.try_get::<String, _>(index) {
        return Some(Value::json_raw(text));
    }
    if let Ok(value) = row.try_get::<serde_json::Value, _>(index) {
        return Some(Value::json(&value));
    }
    None
}

fn get_date<R>(row: &R, index: usize) -> Option<Value>
where
    R: Row,
    usize: ColumnIndex<R>,
    for<'a> NaiveDate: sqlx::Decode<'a, R::Database> + sqlx::Type<R::Database>,
{
    row.try_get::<NaiveDate, _>(index).ok().map(Value::Date)
}

fn get_time<R>(row: &R, index: usize) -> Option<Value>
where
    R: Row,
    usize: ColumnIndex<R>,
    for<'a> NaiveTime: sqlx::Decode<'a, R::Database> + sqlx::Type<R::Database>,
{
    row.try_get::<NaiveTime, _>(index).ok().map(Value::Time)
}

fn get_timestamp<R>(row: &R, index: usize) -> Option<Value>
where
    R: Row,
    usize: ColumnIndex<R>,
    for<'a> NaiveDateTime: sqlx::Decode<'a, R::Database> + sqlx::Type<R::Database>,
{
    row.try_get::<NaiveDateTime, _>(index)
        .ok()
        .map(Value::Timestamp)
}

fn utc_to_fixed(value: DateTime<Utc>) -> DateTime<FixedOffset> {
    let offset = FixedOffset::east_opt(0).expect("UTC offset");
    value.with_timezone(&offset)
}

/// Run inside a concrete driver method so sqlx can see the real database type.
#[macro_export]
macro_rules! query_page {
    ($pool:expr, $sql:expr, $read_cell:path) => {{
        use sqlx::Row as _;
        let pool = $pool;
        let sql: &str = $sql;
        let described = sqlx::Executor::describe(pool, $crate::sql_str(sql))
            .await
            .map_err($crate::sqlx_err)?;
        let mut columns: Vec<::sela_core::ColumnMeta> = described
            .columns()
            .iter()
            .map($crate::column_meta)
            .collect();
        let fetched = $crate::query(sql)
            .fetch_all(pool)
            .await
            .map_err($crate::sqlx_err)?;
        if columns.is_empty() {
            if let Some(row) = fetched.first() {
                columns = row.columns().iter().map($crate::column_meta).collect();
            }
        }
        let width = columns.len();
        let mut rows = Vec::with_capacity(fetched.len());
        for row in &fetched {
            let mut cells = Vec::with_capacity(width);
            for index in 0..width {
                cells.push($read_cell(row, index));
            }
            rows.push(cells);
        }
        // SQLite expression columns arrive as empty / "NULL" type names; fill
        // from the first non-NULL storage class when this path decoded SQLite.
        $crate::refine_sqlite_column_types_if_needed(&mut columns, &rows);
        Ok(::sela_core::QueryPage {
            columns,
            rows,
            duration: std::time::Duration::ZERO,
        })
    }};
}

#[macro_export]
macro_rules! exec_sql {
    ($pool:expr, $sql:expr) => {{
        let result = $crate::query($sql)
            .execute($pool)
            .await
            .map_err($crate::sqlx_err)?;
        Ok(result.rows_affected())
    }};
}

#[macro_export]
macro_rules! count_sql {
    ($pool:expr, $sql:expr) => {{
        $crate::count_sql!($pool, $sql, &[])
    }};
    ($pool:expr, $sql:expr, $params:expr) => {{
        use sqlx::Row as _;
        let row = $crate::bind_params($crate::query($sql), $params)
            .fetch_one($pool)
            .await
            .map_err($crate::sqlx_err)?;
        let count = if let Ok(value) = row.try_get::<i64, _>(0) {
            value
        } else {
            i64::from(row.try_get::<i32, _>(0).map_err($crate::sqlx_err)?)
        };
        Ok(u64::try_from(count).unwrap_or(0))
    }};
}

#[cfg(test)]
mod tests {
    use super::*;
    use sela_core::QueryPage;

    #[test]
    fn raw_access_error_is_other_not_null() {
        let value = early_value(NullProbe::Error, "TEXT").expect("error marker");
        assert!(
            !value.is_null(),
            "try_get_raw Err must not be disguised as NULL"
        );
        match value {
            Value::Other { type_name, text } => {
                assert_eq!(type_name, "TEXT");
                assert!(text.contains("decode error"));
            }
            other => panic!("expected Other, got {other:?}"),
        }
        assert!(early_value(NullProbe::Null, "TEXT").unwrap().is_null());
        assert!(early_value(NullProbe::Present, "TEXT").is_none());
    }

    /// Expression columns with no declared type must use the value's storage
    /// class (INTEGER / REAL / TEXT / BLOB / NULL), not treat type name
    /// `"NULL"` as SQL NULL (#54).
    #[tokio::test]
    async fn sqlite_undeclared_types_decode_from_storage_class() {
        use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

        // One connection: plain `:memory:` is not shared across pool slots.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .expect("in-memory sqlite");

        let row = crate::query("SELECT 42, 1.5, 'a' || 'b', X'6869', NULL, CAST(7 AS INTEGER)")
            .fetch_one(&pool)
            .await
            .expect("select expressions");

        // Declared type for bare expressions is empty / "NULL" in sqlx.
        for index in 0..5 {
            let declared = type_name(&row, index);
            assert!(
                declared.is_empty() || declared == "NULL",
                "expected undeclared type at {index}, got {declared}"
            );
        }

        assert_eq!(read_sqlite_cell(&row, 0), Value::Int(42));
        assert_eq!(read_sqlite_cell(&row, 1), Value::Float(1.5));
        assert_eq!(read_sqlite_cell(&row, 2), Value::text("ab"));
        assert_eq!(read_sqlite_cell(&row, 3), Value::Bytes(b"hi".to_vec()));
        assert!(read_sqlite_cell(&row, 4).is_null());
        // CAST still has a declared INTEGER affinity — keep typed path.
        assert_eq!(read_sqlite_cell(&row, 5), Value::Int(7));
    }

    #[test]
    fn normalize_column_type_never_emits_null() {
        assert_eq!(normalize_column_type_name("NULL"), "");
        assert_eq!(normalize_column_type_name("null"), "");
        assert_eq!(normalize_column_type_name(""), "");
        assert_eq!(normalize_column_type_name("INTEGER"), "INTEGER");
        assert_eq!(normalize_column_type_name("VOID"), "void");
        assert_eq!(normalize_column_type_name("void"), "void");
    }

    #[test]
    fn refine_sqlite_types_from_first_non_null() {
        let mut columns = vec![
            ColumnMeta {
                name: "a".into(),
                type_name: String::new(),
            },
            ColumnMeta {
                name: "b".into(),
                type_name: "NULL".into(),
            },
            ColumnMeta {
                name: "c".into(),
                type_name: String::new(),
            },
            ColumnMeta {
                name: "d".into(),
                type_name: "TEXT".into(),
            },
        ];
        let rows = vec![
            vec![Value::Null, Value::Null, Value::Float(1.5), Value::Int(1)],
            vec![
                Value::Int(42),
                Value::text("x"),
                Value::Float(2.0),
                Value::Int(2),
            ],
        ];
        refine_sqlite_column_types(&mut columns, &rows);
        assert_eq!(columns[0].type_name, "INTEGER");
        assert_eq!(columns[1].type_name, "TEXT");
        assert_eq!(columns[2].type_name, "REAL");
        // Declared TEXT is left alone.
        assert_eq!(columns[3].type_name, "TEXT");
    }

    #[test]
    fn refine_sqlite_types_all_null_stays_empty() {
        let mut columns = vec![ColumnMeta {
            name: "n".into(),
            type_name: String::new(),
        }];
        let rows = vec![vec![Value::Null], vec![Value::Null]];
        refine_sqlite_column_types(&mut columns, &rows);
        assert_eq!(columns[0].type_name, "");
    }

    /// Expression columns must show INTEGER/REAL/TEXT/BLOB in the header, never
    /// NULL (#60).
    #[tokio::test]
    async fn sqlite_expression_column_meta_uses_storage_class() -> Result<(), sela_core::DbError> {
        use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .expect("in-memory sqlite");

        let page: QueryPage = crate::query_page!(
            &pool,
            "SELECT 42 AS n, 1.5 AS f, 'a' || 'b' AS s, X'6869' AS b, NULL AS z",
            read_sqlite_cell
        )?;

        assert_eq!(page.columns[0].type_name, "INTEGER");
        assert_eq!(page.columns[1].type_name, "REAL");
        assert_eq!(page.columns[2].type_name, "TEXT");
        assert_eq!(page.columns[3].type_name, "BLOB");
        assert_eq!(page.columns[4].type_name, "");
        assert!(!page
            .columns
            .iter()
            .any(|c| c.type_name.eq_ignore_ascii_case("NULL")));
        Ok(())
    }
}
