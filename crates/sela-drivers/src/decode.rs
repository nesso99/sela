//! Decode sqlx rows into typed [`sela_core::Value`] cells without panicking.

use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use rust_decimal::Decimal;
use sqlx::mysql::MySqlRow;
use sqlx::postgres::PgRow;
use sqlx::sqlite::SqliteRow;
use sqlx::{Column as _, ColumnIndex, Row, TypeInfo, ValueRef};
use uuid::Uuid;

use sela_core::{ColumnMeta, Interval, Value};

pub fn column_meta(column: &impl sqlx::Column) -> ColumnMeta {
    ColumnMeta {
        name: column.name().to_string(),
        type_name: column.type_info().to_string(),
    }
}

pub fn read_postgres_cell(row: &PgRow, index: usize) -> Value {
    if is_null(row, index) {
        return Value::Null;
    }
    let type_name = type_name(row, index);
    match type_name.as_str() {
        "BOOL" => get_bool(row, index).unwrap_or_else(|| other(&type_name, row, index)),
        "INT2" | "INT4" | "INT8" | "OID" => {
            get_int(row, index).unwrap_or_else(|| other(&type_name, row, index))
        }
        "FLOAT4" | "FLOAT8" => {
            get_float(row, index).unwrap_or_else(|| other(&type_name, row, index))
        }
        "NUMERIC" | "MONEY" => {
            get_decimal(row, index).unwrap_or_else(|| other(&type_name, row, index))
        }
        "TEXT" | "VARCHAR" | "NAME" | "XML" | "CITEXT" | "CHAR" | "BPCHAR" | "\"CHAR\"" => {
            get_text(row, index).unwrap_or_else(|| other(&type_name, row, index))
        }
        "BYTEA" => get_bytes(row, index).unwrap_or_else(|| other(&type_name, row, index)),
        "UUID" => {
            if let Ok(value) = row.try_get::<Uuid, _>(index) {
                Value::Uuid(value)
            } else if let Ok(text) = row.try_get::<String, _>(index) {
                Uuid::parse_str(&text)
                    .map(Value::Uuid)
                    .unwrap_or(Value::Text(text))
            } else {
                other(&type_name, row, index)
            }
        }
        "JSON" | "JSONB" => get_json(row, index).unwrap_or_else(|| other(&type_name, row, index)),
        "DATE" => get_date(row, index).unwrap_or_else(|| other(&type_name, row, index)),
        "TIME" => get_time(row, index).unwrap_or_else(|| other(&type_name, row, index)),
        "TIMESTAMP" => get_timestamp(row, index).unwrap_or_else(|| other(&type_name, row, index)),
        "TIMESTAMPTZ" => {
            if let Ok(value) = row.try_get::<DateTime<FixedOffset>, _>(index) {
                Value::TimestampTz(value)
            } else if let Ok(value) = row.try_get::<DateTime<Utc>, _>(index) {
                Value::TimestampTz(utc_to_fixed(value))
            } else {
                other(&type_name, row, index)
            }
        }
        "INTERVAL" => match row.try_get::<sqlx::postgres::types::PgInterval, _>(index) {
            Ok(value) => Value::Interval(Interval {
                months: value.months,
                days: value.days,
                microseconds: value.microseconds,
            }),
            Err(_) => other(&type_name, row, index),
        },
        _ => other(&type_name, row, index),
    }
}

pub fn read_mysql_cell(row: &MySqlRow, index: usize) -> Value {
    if is_null(row, index) {
        return Value::Null;
    }
    let type_name = type_name(row, index);
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
    if is_null(row, index) {
        return Value::Null;
    }
    let type_name = type_name(row, index);
    match type_name.as_str() {
        "NULL" => Value::Null,
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
        // Undeclared / unknown declared types: probe storage classes dynamically.
        _ => {
            if let Some(value) = get_int(row, index) {
                value
            } else if let Some(value) = get_float(row, index) {
                value
            } else if let Some(value) = get_text(row, index) {
                value
            } else if let Some(value) = get_bytes(row, index) {
                value
            } else {
                other(&type_name, row, index)
            }
        }
    }
}

fn is_null<R>(row: &R, index: usize) -> bool
where
    R: Row,
    usize: ColumnIndex<R>,
{
    match row.try_get_raw(index) {
        Ok(raw) => ValueRef::is_null(&raw),
        Err(_) => true,
    }
}

fn type_name<R>(row: &R, index: usize) -> String
where
    R: Row,
{
    row.columns()
        .get(index)
        .map(|column| column.type_info().name().to_ascii_uppercase())
        .unwrap_or_else(|| "VALUE".into())
}

fn other<R>(type_name: &str, row: &R, index: usize) -> Value
where
    R: Row,
    usize: ColumnIndex<R>,
    for<'a> String: sqlx::Decode<'a, R::Database> + sqlx::Type<R::Database>,
    for<'a> Vec<u8>: sqlx::Decode<'a, R::Database> + sqlx::Type<R::Database>,
{
    if let Ok(text) = row.try_get::<String, _>(index) {
        return Value::Other {
            type_name: type_name.to_string(),
            text,
        };
    }
    if let Ok(bytes) = row.try_get::<Vec<u8>, _>(index) {
        return Value::Bytes(bytes);
    }
    Value::Other {
        type_name: type_name.to_string(),
        text: format!("<{type_name}>"),
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
        return Some(Value::Text(text));
    }
    None
}

fn get_text<R>(row: &R, index: usize) -> Option<Value>
where
    R: Row,
    usize: ColumnIndex<R>,
    for<'a> String: sqlx::Decode<'a, R::Database> + sqlx::Type<R::Database>,
{
    row.try_get::<String, _>(index).ok().map(Value::Text)
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
    if let Ok(value) = row.try_get::<serde_json::Value, _>(index) {
        return Some(Value::Json(value));
    }
    if let Ok(text) = row.try_get::<String, _>(index) {
        if let Ok(value) = serde_json::from_str(&text) {
            return Some(Value::Json(value));
        }
        return Some(Value::Text(text));
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
        let described = sqlx::Executor::describe(pool, sql)
            .await
            .map_err($crate::sqlx_err)?;
        let mut columns: Vec<::sela_core::ColumnMeta> = described
            .columns()
            .iter()
            .map($crate::column_meta)
            .collect();
        let fetched = sqlx::query(sql)
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
        let result = sqlx::query($sql)
            .execute($pool)
            .await
            .map_err($crate::sqlx_err)?;
        Ok(result.rows_affected())
    }};
}

#[macro_export]
macro_rules! count_sql {
    ($pool:expr, $sql:expr) => {{
        use sqlx::Row as _;
        let row = sqlx::query($sql)
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
