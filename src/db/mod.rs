//! Database drivers.
//!
//! [`driver::DatabaseSession`] is the seam the UI talks to. PostgreSQL, MySQL,
//! and SQLite each implement it. SQL that Birch generates (identifiers, paging)
//! is built in [`sql`] and covered by unit tests that do not need a server.

mod decode;
mod driver;
mod error;
mod mysql;
mod postgres;
mod runtime;
mod sql;
mod sqlite;
mod types;

pub(crate) use decode::{column_meta, read_cell};
pub use driver::{preview_table, run_user_sql, DatabaseSession};
pub use error::DbError;
pub use runtime::drive;
pub use sql::{normalize_sql, qualify, quote_ident, wrap_page};
pub use types::{
    Cell, ColumnMeta, Engine, ExecOutcome, QueryPage, RelationKind, RowSet, SchemaInfo,
    StatementOutcome, TableInfo, PAGE_SIZE,
};

use std::time::Duration;

use crate::config::SavedConnection;

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
        Err(_) => Err(DbError::message(format!(
            "timed out after {}s while opening the connection",
            CONNECT_TIMEOUT.as_secs()
        ))),
    }
}

/// Create the sample SQLite library used by the "Sample library" button.
pub async fn create_sample_database(path: &std::path::Path) -> Result<(), DbError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| {
            DbError::message(format!("could not create {}: {err}", parent.display()))
        })?;
    }
    let session = SqliteSession::open(path, true).await?;
    for statement in sql::split_statements(sqlite::SAMPLE_SQL)
        .map_err(|err| DbError::message(err.to_string()))?
    {
        session.execute(&statement).await?;
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

#[derive(Debug, Clone)]
pub struct LoadedSchema {
    pub name: String,
    pub tables: Vec<TableInfo>,
}
