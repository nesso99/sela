//! Turn a sqlx cell into a display string without losing NULL.

use sqlx::{Column as _, ColumnIndex, Row, ValueRef};

use sela_core::{Cell, ColumnMeta};

pub fn read_cell<R>(row: &R, index: usize) -> Cell
where
    R: Row,
    for<'a> String: sqlx::Decode<'a, R::Database> + sqlx::Type<R::Database>,
    for<'a> i64: sqlx::Decode<'a, R::Database> + sqlx::Type<R::Database>,
    for<'a> i32: sqlx::Decode<'a, R::Database> + sqlx::Type<R::Database>,
    for<'a> f64: sqlx::Decode<'a, R::Database> + sqlx::Type<R::Database>,
    for<'a> bool: sqlx::Decode<'a, R::Database> + sqlx::Type<R::Database>,
    for<'a> Vec<u8>: sqlx::Decode<'a, R::Database> + sqlx::Type<R::Database>,
    for<'a> sqlx::types::chrono::NaiveDate: sqlx::Decode<'a, R::Database> + sqlx::Type<R::Database>,
    for<'a> sqlx::types::chrono::NaiveDateTime:
        sqlx::Decode<'a, R::Database> + sqlx::Type<R::Database>,
    for<'a> sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>:
        sqlx::Decode<'a, R::Database> + sqlx::Type<R::Database>,
    usize: ColumnIndex<R>,
{
    let Ok(raw) = row.try_get_raw(index) else {
        return Cell::Text(String::new());
    };
    if ValueRef::is_null(&raw) {
        return Cell::Null;
    }
    if let Ok(value) = row.try_get::<i64, _>(index) {
        return Cell::Text(value.to_string());
    }
    if let Ok(value) = row.try_get::<i32, _>(index) {
        return Cell::Text(value.to_string());
    }
    if let Ok(value) = row.try_get::<f64, _>(index) {
        return Cell::Text(format_float(value));
    }
    if let Ok(value) = row.try_get::<bool, _>(index) {
        return Cell::Text(value.to_string());
    }
    if let Ok(value) = row.try_get::<sqlx::types::chrono::NaiveDate, _>(index) {
        return Cell::Text(value.format("%Y-%m-%d").to_string());
    }
    if let Ok(value) = row.try_get::<sqlx::types::chrono::NaiveDateTime, _>(index) {
        return Cell::Text(value.format("%Y-%m-%d %H:%M:%S").to_string());
    }
    if let Ok(value) =
        row.try_get::<sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>, _>(index)
    {
        return Cell::Text(value.format("%Y-%m-%d %H:%M:%S").to_string());
    }
    if let Ok(value) = row.try_get::<String, _>(index) {
        return Cell::Text(value);
    }
    if let Ok(value) = row.try_get::<Vec<u8>, _>(index) {
        return Cell::Text(format_blob(&value));
    }
    let type_name = row
        .columns()
        .get(index)
        .map(|column| column.type_info().to_string())
        .unwrap_or_else(|| "value".into());
    Cell::Text(format!("<{type_name}>"))
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

fn format_blob(bytes: &[u8]) -> String {
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

pub fn column_meta(column: &impl sqlx::Column) -> ColumnMeta {
    ColumnMeta {
        name: column.name().to_string(),
        type_name: column.type_info().to_string(),
    }
}

/// Run inside a concrete driver method so sqlx can see the real database type.
#[macro_export]
macro_rules! query_page {
    ($pool:expr, $sql:expr) => {{
        use sqlx::Row as _;
        let pool = $pool;
        let sql: &str = $sql;
        let described = sqlx::Executor::describe(pool, sql)
            .await
            .map_err($crate::sqlx_err)?;
        let mut columns: Vec<$crate::ColumnMeta> = described
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
                cells.push($crate::read_cell(row, index));
            }
            rows.push(cells);
        }
        Ok($crate::QueryPage {
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
