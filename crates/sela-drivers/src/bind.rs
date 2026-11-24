//! Bind [`sela_core::Value`] parameters onto sqlx queries (per dialect).

use sela_core::Value;
use sqlx::query::Query;
use sqlx::{Database, Encode, Type};

/// Bind filter/sort parameters for any sqlx database.
///
/// Types that a backend cannot encode natively are bound as text so the
/// engine can cast (or return a clear error) rather than failing at bind time.
pub(crate) fn bind_params<'q, DB>(
    mut query: Query<'q, DB, <DB as Database>::Arguments>,
    params: &'q [Value],
) -> Query<'q, DB, <DB as Database>::Arguments>
where
    DB: Database,
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
            Value::UInt(v) => {
                if let Ok(n) = i64::try_from(*v) {
                    query.bind(n)
                } else {
                    query.bind(v.to_string())
                }
            }
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
