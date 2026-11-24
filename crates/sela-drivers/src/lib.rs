//! Database drivers.
//!
//! [`sela_core::DatabaseSession`] is the seam the UI talks to. PostgreSQL, MySQL,
//! and SQLite each implement it. SQL that Sela generates (identifiers, paging)
//! is built in `sela-core` and covered by unit tests that do not need a server.
//!
//! Public surface: [`connect`], [`drive`], [`load_catalog`], and
//! [`create_sample_database`]. Domain types live in `sela-core`.

mod bind;
mod change_exec;
mod decode;
mod error_map;
mod mysql;
mod pg_decode;
mod postgres;
mod runtime;
mod sqlite;
mod stream;
mod tls;

pub(crate) use bind::bind_params;
pub(crate) use decode::{
    column_meta, read_mysql_cell, read_postgres_cell, read_sqlite_cell, refine_sqlite_column_types,
};

/// Refine undeclared SQLite column types after a `query_page!` fetch.
///
/// No-op when every column already has a non-empty type (Postgres / MySQL).
#[doc(hidden)]
pub fn refine_sqlite_column_types_if_needed(
    columns: &mut [sela_core::ColumnMeta],
    rows: &[Vec<sela_core::Value>],
) {
    if columns
        .iter()
        .any(|c| c.type_name.is_empty() || c.type_name.eq_ignore_ascii_case("NULL"))
    {
        refine_sqlite_column_types(columns, rows);
    }
}
pub(crate) use error_map::sqlx_err;
pub use runtime::drive;
pub use tokio_util::sync::CancellationToken;

/// Build a sqlx query from a dynamically constructed SQL string.
///
/// # Invariant
///
/// [`sqlx::AssertSqlSafe`] marks the string as trusted for sqlx 0.9's
/// [`sqlx::SqlSafeStr`] gate. Callers may pass only:
///
/// 1. SQL the user typed in the editor, or
/// 2. Output of the `sela-core` builders (`quote_ident`, `qualify`,
///    `select_all`, `wrap_page`, `wrap_count`, filter/sort builders), with
///    identifiers passed through [`sela_core::quote_ident`] and values bound
///    as parameters — never `format!`-interpolated into the SQL text.
///
/// Do not wrap arbitrary `format!` strings that embed unquoted identifiers or
/// user values.
#[inline]
pub(crate) fn query<'q, DB: sqlx::Database>(
    sql: &str,
) -> sqlx::query::Query<'q, DB, <DB as sqlx::Database>::Arguments> {
    sqlx::query(sqlx::AssertSqlSafe(sql))
}

/// Convert dynamic SQL into a [`sqlx::SqlStr`] for `Executor::describe`.
///
/// Same invariant as [`query`]: user-typed SQL or `sela-core` builder output
/// with quoted identifiers and bound parameters only.
#[inline]
pub(crate) fn sql_str(sql: &str) -> sqlx::SqlStr {
    use sqlx::SqlSafeStr;
    sqlx::AssertSqlSafe(sql).into_sql_str()
}

use std::time::Duration;

use sela_core::{DatabaseSession, DbError, Engine, LoadedSchema, SavedConnection};

use mysql::MysqlSession;
use postgres::PostgresSession;
use sqlite::SqliteSession;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(12);

/// Open a session for a saved connection. The password is used for network
/// engines and ignored for SQLite.
pub async fn connect(
    connection: &SavedConnection,
    password: &str,
) -> Result<Box<dyn DatabaseSession>, DbError> {
    let connection = connection.clone();
    let password = password.to_string();
    let open = async move {
        match connection.engine {
            Engine::Postgres => Ok(
                Box::new(PostgresSession::connect(&connection, &password).await?)
                    as Box<dyn DatabaseSession>,
            ),
            Engine::Mysql => Ok(
                Box::new(MysqlSession::connect(&connection, &password).await?)
                    as Box<dyn DatabaseSession>,
            ),
            Engine::Sqlite => Ok(Box::new(
                SqliteSession::open(std::path::Path::new(&connection.file_path), false).await?,
            ) as Box<dyn DatabaseSession>),
        }
    };
    match tokio::time::timeout(CONNECT_TIMEOUT, open).await {
        Ok(result) => result,
        Err(_) => Err(DbError::timeout(format!(
            "timed out after {}s while opening the connection",
            CONNECT_TIMEOUT.as_secs()
        ))),
    }
}

/// Create the sample SQLite library used by the "Sample library" button.
pub async fn create_sample_database(path: &std::path::Path) -> Result<(), DbError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| {
            DbError::config(format!("could not create {}: {err}", parent.display()))
        })?;
    }
    let session = SqliteSession::open(path, true).await?;
    for statement in sela_core::split_statements(sqlite::SAMPLE_SQL, sela_core::Dialect::Sqlite)
        .map_err(|err| DbError::query(err.to_string()))?
    {
        session.execute(&statement.text).await?;
    }
    Ok(())
}

/// Load schemas and the tables inside each of them.
pub async fn load_catalog(session: &dyn DatabaseSession) -> Result<Vec<LoadedSchema>, DbError> {
    let schemas = session.list_schemas().await?;
    let mut loaded = Vec::with_capacity(schemas.len());
    for schema in schemas {
        let tables = session.list_tables(&schema.name).await?;
        loaded.push(LoadedSchema {
            name: schema.name,
            tables,
        });
    }
    Ok(loaded)
}
