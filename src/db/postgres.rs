use std::time::Duration;

use async_trait::async_trait;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};
use sqlx::PgPool;

use crate::config::SavedConnection;

use sqlx::Row;

use super::driver::DatabaseSession;
use super::error::DbError;
use super::types::{Dialect, QueryPage, RelationKind, SchemaInfo, TableInfo};

pub struct PostgresSession {
    pool: PgPool,
}

impl PostgresSession {
    pub async fn connect(connection: &SavedConnection, password: &str) -> Result<Self, DbError> {
        let options = PgConnectOptions::new()
            .host(connection.host.trim())
            .port(connection.port)
            .username(connection.username.trim())
            .password(password)
            .database(connection.database.trim())
            .ssl_mode(if connection.ssl {
                PgSslMode::Require
            } else {
                PgSslMode::Disable
            });
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .acquire_timeout(Duration::from_secs(10))
            .connect_with(options)
            .await?;
        Ok(Self { pool })
    }

    #[cfg(test)]
    pub async fn connect_url(url: &str) -> Result<Self, DbError> {
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .acquire_timeout(Duration::from_secs(10))
            .connect(url)
            .await?;
        Ok(Self { pool })
    }
}

#[async_trait]
impl DatabaseSession for PostgresSession {
    fn dialect(&self) -> Dialect {
        Dialect::Postgres
    }

    async fn list_schemas(&self) -> Result<Vec<SchemaInfo>, DbError> {
        let rows = sqlx::query(
            "SELECT schema_name AS name
             FROM information_schema.schemata
             WHERE schema_name NOT IN ('pg_catalog', 'information_schema', 'pg_toast')
               AND schema_name NOT LIKE 'pg_temp_%'
               AND schema_name NOT LIKE 'pg_toast_temp_%'
             ORDER BY schema_name",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut schemas = Vec::with_capacity(rows.len());
        for row in rows {
            schemas.push(SchemaInfo {
                name: row.try_get("name")?,
            });
        }
        Ok(schemas)
    }

    async fn list_tables(&self, schema: &str) -> Result<Vec<TableInfo>, DbError> {
        let rows = sqlx::query(
            "SELECT table_name AS name, table_type AS kind
             FROM information_schema.tables
             WHERE table_schema = $1
             ORDER BY table_name",
        )
        .bind(schema)
        .fetch_all(&self.pool)
        .await?;
        let mut tables = Vec::with_capacity(rows.len());
        for row in rows {
            let kind: String = row.try_get("kind")?;
            tables.push(TableInfo {
                name: row.try_get("name")?,
                kind: RelationKind::parse(&kind),
            });
        }
        Ok(tables)
    }

    async fn query(&self, sql: &str) -> Result<QueryPage, DbError> {
        crate::query_page!(&self.pool, sql)
    }

    async fn execute(&self, sql: &str) -> Result<u64, DbError> {
        crate::exec_sql!(&self.pool, sql)
    }

    async fn count_query(&self, sql: &str) -> Result<u64, DbError> {
        crate::count_sql!(&self.pool, sql)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{preview_table, run_user_sql, StatementOutcome, PAGE_SIZE};

    #[tokio::test]
    async fn postgres_round_trip_when_configured() {
        let Ok(url) = std::env::var("SELA_POSTGRES_URL") else {
            eprintln!("skip postgres: set SELA_POSTGRES_URL to run this test");
            return;
        };
        let schema = std::env::var("SELA_POSTGRES_SCHEMA").unwrap_or_else(|_| "public".into());
        let session = PostgresSession::connect_url(&url)
            .await
            .expect("postgres connect");
        session
            .execute("DROP TABLE IF EXISTS sela_probe")
            .await
            .unwrap();
        session
            .execute(
                "CREATE TABLE sela_probe (
                    id integer PRIMARY KEY,
                    name text,
                    in_print boolean,
                    published date
                )",
            )
            .await
            .unwrap();
        session
            .execute(
                "INSERT INTO sela_probe (id, name, in_print, published)
                 VALUES (1, 'Ada', true, DATE '1815-12-10'), (2, NULL, false, NULL)",
            )
            .await
            .unwrap();

        let schemas = session.list_schemas().await.unwrap();
        assert!(schemas.iter().any(|item| item.name == schema));
        let tables = session.list_tables(&schema).await.unwrap();
        assert!(tables.iter().any(|item| item.name == "sela_probe"));

        let outcome = preview_table(&session, &schema, "sela_probe", PAGE_SIZE, 0)
            .await
            .unwrap();
        let StatementOutcome::Rows(rows) = outcome else {
            panic!("expected rows");
        };
        assert_eq!(rows.page.rows.len(), 2);
        assert!(rows.page.columns.iter().any(|column| column.name == "name"));
        assert_eq!(rows.total_rows, Some(2));

        let queried = run_user_sql(&session, "SELECT name FROM sela_probe WHERE id = 2", 10, 0)
            .await
            .unwrap();
        let StatementOutcome::Rows(rows) = queried else {
            panic!("expected rows");
        };
        assert!(rows.page.rows[0][0].is_null());

        session.execute("DROP TABLE sela_probe").await.unwrap();
    }
}
