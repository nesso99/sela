use std::time::Duration;

use async_trait::async_trait;
use sqlx::mysql::{MySqlConnectOptions, MySqlPoolOptions, MySqlSslMode};
use sqlx::MySqlPool;

use crate::config::SavedConnection;

use sqlx::Row;

use super::driver::DatabaseSession;
use super::error::DbError;
use super::types::{Dialect, QueryPage, RelationKind, SchemaInfo, TableInfo};

pub struct MysqlSession {
    pool: MySqlPool,
}

impl MysqlSession {
    pub async fn connect(connection: &SavedConnection, password: &str) -> Result<Self, DbError> {
        let options = MySqlConnectOptions::new()
            .host(connection.host.trim())
            .port(connection.port)
            .username(connection.username.trim())
            .password(password)
            .database(connection.database.trim())
            .ssl_mode(if connection.ssl {
                MySqlSslMode::Required
            } else {
                MySqlSslMode::Disabled
            });
        let pool = MySqlPoolOptions::new()
            .max_connections(5)
            .acquire_timeout(Duration::from_secs(10))
            .connect_with(options)
            .await?;
        Ok(Self { pool })
    }

    #[cfg(test)]
    pub async fn connect_url(url: &str) -> Result<Self, DbError> {
        let pool = MySqlPoolOptions::new()
            .max_connections(4)
            .acquire_timeout(Duration::from_secs(10))
            .connect(url)
            .await?;
        Ok(Self { pool })
    }
}

#[async_trait]
impl DatabaseSession for MysqlSession {
    fn dialect(&self) -> Dialect {
        Dialect::Mysql
    }

    async fn list_schemas(&self) -> Result<Vec<SchemaInfo>, DbError> {
        let rows = sqlx::query(
            "SELECT schema_name AS name
             FROM information_schema.schemata
             WHERE schema_name NOT IN ('mysql', 'information_schema', 'performance_schema', 'sys')
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
             WHERE table_schema = ?
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
    use crate::db::{run_user_sql, StatementOutcome};

    #[tokio::test]
    async fn mysql_round_trip_when_configured() {
        let Ok(url) = std::env::var("SELA_MYSQL_URL") else {
            eprintln!("skip mysql: set SELA_MYSQL_URL to run this test");
            return;
        };
        let schema = std::env::var("SELA_MYSQL_SCHEMA").unwrap_or_else(|_| "sample".into());
        let session = MysqlSession::connect_url(&url)
            .await
            .expect("mysql connect");
        session
            .execute("DROP TABLE IF EXISTS sela_probe")
            .await
            .unwrap();
        session
            .execute(
                "CREATE TABLE sela_probe (
                    id INT PRIMARY KEY,
                    name VARCHAR(64) NULL,
                    rating DOUBLE NULL
                )",
            )
            .await
            .unwrap();
        session
            .execute(
                "INSERT INTO sela_probe (id, name, rating) VALUES (1, 'Ada', 1.5), (2, NULL, NULL)",
            )
            .await
            .unwrap();

        let tables = session.list_tables(&schema).await.unwrap();
        assert!(tables.iter().any(|item| item.name == "sela_probe"));
        let outcome = run_user_sql(
            &session,
            "SELECT name, rating FROM sela_probe ORDER BY id",
            10,
            0,
        )
        .await
        .unwrap();
        let StatementOutcome::Rows(rows) = outcome else {
            panic!("expected rows");
        };
        assert_eq!(rows.page.rows.len(), 2);
        assert!(rows.page.rows[1][0].is_null());
        assert_eq!(rows.total_rows, Some(2));
        session.execute("DROP TABLE sela_probe").await.unwrap();
    }
}
