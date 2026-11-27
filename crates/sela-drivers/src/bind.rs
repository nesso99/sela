//! Bind [`sela_core::Value`] parameters onto sqlx queries (per dialect).

use sela_core::Value;
use sqlx::query::Query;
use sqlx::{Database, Encode, Type};

/// Native binding of `u64` where the backend has an unsigned 64-bit integer.
///
/// MySQL binds `BIGINT UNSIGNED` natively. Postgres and SQLite have no
/// unsigned 64-bit type, so values above `i64::MAX` fall back to text.
pub(crate) trait BindUInt: Database + Sized {
    fn bind_uint<'q>(
        query: Query<'q, Self, <Self as Database>::Arguments>,
        value: u64,
    ) -> Query<'q, Self, <Self as Database>::Arguments>;
}

impl BindUInt for sqlx::MySql {
    fn bind_uint<'q>(
        query: Query<'q, Self, <Self as Database>::Arguments>,
        value: u64,
    ) -> Query<'q, Self, <Self as Database>::Arguments> {
        query.bind(value)
    }
}

impl BindUInt for sqlx::Postgres {
    fn bind_uint<'q>(
        query: Query<'q, Self, <Self as Database>::Arguments>,
        value: u64,
    ) -> Query<'q, Self, <Self as Database>::Arguments> {
        match i64::try_from(value) {
            Ok(n) => query.bind(n),
            Err(_) => query.bind(value.to_string()),
        }
    }
}

impl BindUInt for sqlx::Sqlite {
    fn bind_uint<'q>(
        query: Query<'q, Self, <Self as Database>::Arguments>,
        value: u64,
    ) -> Query<'q, Self, <Self as Database>::Arguments> {
        match i64::try_from(value) {
            Ok(n) => query.bind(n),
            Err(_) => query.bind(value.to_string()),
        }
    }
}

/// Bind filter/sort parameters for any sqlx database.
///
/// Types that a backend cannot encode natively are bound as text so the
/// engine can cast (or return a clear error) rather than failing at bind time.
pub(crate) fn bind_params<'q, DB>(
    mut query: Query<'q, DB, <DB as Database>::Arguments>,
    params: &'q [Value],
) -> Query<'q, DB, <DB as Database>::Arguments>
where
    DB: Database + BindUInt,
    bool: Encode<'q, DB> + Type<DB>,
    i64: Encode<'q, DB> + Type<DB>,
    f64: Encode<'q, DB> + Type<DB>,
    String: Encode<'q, DB> + Type<DB>,
    &'q str: Encode<'q, DB> + Type<DB>,
    &'q [u8]: Encode<'q, DB> + Type<DB>,
    Option<String>: Encode<'q, DB> + Type<DB>,
{
    for param in params {
        query = match param {
            Value::Null => query.bind(Option::<String>::None),
            Value::Bool(v) => query.bind(*v),
            Value::Int(v) => query.bind(*v),
            Value::UInt(v) => DB::bind_uint(query, *v),
            Value::Float(v) => query.bind(*v),
            Value::Text(v) => query.bind(v.as_ref()),
            Value::Bytes(v) => query.bind(v.as_slice()),
            // Decimal / UUID / temporals: bind as text for maximum dialect
            // coverage (SQLite has no native Decimal/UUID encode).
            Value::Decimal(v) => query.bind(v.normalize().to_string()),
            Value::Uuid(v) => query.bind(v.to_string()),
            Value::Json(v) => query.bind(v.as_ref()),
            Value::Date(v) => query.bind(v.format("%Y-%m-%d").to_string()),
            Value::Time(v) => query.bind(v.format("%H:%M:%S%.f").to_string()),
            Value::Timestamp(v) => query.bind(v.format("%Y-%m-%d %H:%M:%S%.f").to_string()),
            Value::TimestampTz(v) => query.bind(v.to_rfc3339()),
            Value::Interval(_) | Value::Other { .. } => query.bind(param.display().into_owned()),
        };
    }
    query
}
