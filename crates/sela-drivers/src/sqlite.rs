use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};

use sqlx::Row;

use sela_core::{
    quote_ident, DatabaseSession, DbError, Dialect, QueryPage, RelationKind, SchemaInfo, TableInfo,
};

pub const SAMPLE_SQL: &str = include_str!("../../../scripts/seed-sqlite.sql");

pub struct SqliteSession {
    pool: SqlitePool,
}

impl SqliteSession {
    pub async fn open(path: &Path, create_if_missing: bool) -> Result<Self, DbError> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(create_if_missing)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .acquire_timeout(Duration::from_secs(10))
            .connect_with(options)
            .await
            .map_err(crate::sqlx_err)?;
        Ok(Self { pool })
    }
}

#[async_trait]
impl DatabaseSession for SqliteSession {
    fn dialect(&self) -> Dialect {
        Dialect::Sqlite
    }

    async fn list_schemas(&self) -> Result<Vec<SchemaInfo>, DbError> {
        let rows = sqlx::query("SELECT name FROM pragma_database_list ORDER BY seq")
            .fetch_all(&self.pool)
            .await
            .map_err(crate::sqlx_err)?;
        let mut schemas = Vec::new();
        for row in rows {
            let name: String = row.try_get("name").map_err(crate::sqlx_err)?;
            if name.eq_ignore_ascii_case("temp") {
                continue;
            }
            schemas.push(SchemaInfo { name });
        }
        Ok(schemas)
    }

    async fn list_tables(&self, schema: &str) -> Result<Vec<TableInfo>, DbError> {
        let schema = quote_ident(Dialect::Sqlite, schema)
            .map_err(|err| DbError::message(err.to_string()))?;
        let sql = format!(
            "SELECT name, type FROM {schema}.sqlite_schema
             WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite_%'
             ORDER BY name"
        );
        let rows = sqlx::query(&sql)
            .fetch_all(&self.pool)
            .await
            .map_err(crate::sqlx_err)?;
        let mut tables = Vec::with_capacity(rows.len());
        for row in rows {
            let kind: String = row.try_get("type").map_err(crate::sqlx_err)?;
            tables.push(TableInfo {
                name: row.try_get("name").map_err(crate::sqlx_err)?,
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
    use sela_core::{
        preview_table, run_user_sql, split_statements, Cell, StatementOutcome, PAGE_SIZE,
    };

    async fn seeded() -> (tempfile::TempDir, SqliteSession) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("library.sqlite");
        let session = SqliteSession::open(&path, true).await.unwrap();
        for statement in split_statements(SAMPLE_SQL).unwrap() {
            session.execute(&statement).await.unwrap();
        }
        (dir, session)
    }

    #[tokio::test]
    async fn sample_catalog_preview_and_paging() {
        let (_dir, session) = seeded().await;
        let schemas = session.list_schemas().await.unwrap();
        assert!(schemas.iter().any(|schema| schema.name == "main"));
        let tables = session.list_tables("main").await.unwrap();
        assert!(tables
            .iter()
            .any(|table| table.name == "books" && table.kind == RelationKind::Table));
        assert!(tables
            .iter()
            .any(|table| table.name == "books_in_print" && table.kind == RelationKind::View));

        let outcome = preview_table(&session, "main", "books", PAGE_SIZE, 0)
            .await
            .unwrap();
        let StatementOutcome::Rows(rows) = outcome else {
            panic!("expected rows");
        };
        assert!(rows.page.rows.len() > 1);
        assert!(rows.page.rows.len() <= PAGE_SIZE as usize);
        let title = rows
            .page
            .columns
            .iter()
            .position(|column| column.name == "title")
            .unwrap();
        assert!(
            rows.page.columns[title]
                .type_name
                .to_uppercase()
                .contains("TEXT")
                || !rows.page.columns[title].type_name.is_empty()
        );
        assert!(rows.total_rows.unwrap() >= rows.page.rows.len() as u64);

        let notes = rows
            .page
            .columns
            .iter()
            .position(|column| column.name == "notes")
            .unwrap();
        assert!(rows.page.rows.iter().any(|row| row[notes].is_null()));
    }

    #[tokio::test]
    async fn pages_one_hundred_rows_and_reports_errors() {
        let dir = tempfile::tempdir().unwrap();
        let session = SqliteSession::open(&dir.path().join("pages.sqlite"), true)
            .await
            .unwrap();
        session
            .execute(
                "CREATE TABLE readings (
                    id INTEGER PRIMARY KEY,
                    pages INTEGER,
                    rating REAL,
                    note TEXT,
                    cover BLOB
                )",
            )
            .await
            .unwrap();
        for id in 1..=120 {
            session
                .execute(&format!(
                    "INSERT INTO readings (id, pages, rating, note) VALUES ({id}, {id}, 1.5, 'row {id}')"
                ))
                .await
                .unwrap();
        }
        session
            .execute("UPDATE readings SET note = NULL, cover = X'6869' WHERE id = 1")
            .await
            .unwrap();

        let first = preview_table(&session, "main", "readings", PAGE_SIZE, 0)
            .await
            .unwrap();
        let StatementOutcome::Rows(first) = first else {
            panic!("rows");
        };
        assert_eq!(first.page.rows.len(), 100);
        assert_eq!(first.total_rows, Some(120));
        let pages_ix = first
            .page
            .columns
            .iter()
            .position(|column| column.name == "pages")
            .unwrap();
        assert_eq!(first.page.rows[0][pages_ix], Cell::Text("1".into()));

        let second = preview_table(&session, "main", "readings", PAGE_SIZE, 100)
            .await
            .unwrap();
        let StatementOutcome::Rows(second) = second else {
            panic!("rows");
        };
        assert_eq!(second.page.rows.len(), 20);

        let blob = run_user_sql(&session, "SELECT cover FROM readings WHERE id = 1", 10, 0)
            .await
            .unwrap();
        let StatementOutcome::Rows(blob) = blob else {
            panic!("rows");
        };
        assert_eq!(blob.page.rows[0][0], Cell::Text("x'6869'".into()));

        let commented = run_user_sql(
            &session,
            "select count(*) as n from readings -- tail",
            10,
            0,
        )
        .await
        .unwrap();
        let StatementOutcome::Rows(commented) = commented else {
            panic!("rows");
        };
        assert_eq!(commented.page.rows.len(), 1);

        let failed = run_user_sql(&session, "SELECT * FROM missing_table", 10, 0).await;
        assert!(failed.is_err());

        let executed = run_user_sql(
            &session,
            "INSERT INTO readings (id, pages, rating) VALUES (121, 3, 2.0)",
            10,
            0,
        )
        .await
        .unwrap();
        let StatementOutcome::Executed(executed) = executed else {
            panic!("executed");
        };
        assert_eq!(executed.rows_affected, 1);

        let empty = run_user_sql(&session, "SELECT id, note FROM readings WHERE 0", 10, 0)
            .await
            .unwrap();
        let StatementOutcome::Rows(empty) = empty else {
            panic!("rows");
        };
        assert!(empty.page.rows.is_empty());
        assert_eq!(empty.page.columns.len(), 2);
        assert_eq!(empty.page.columns[0].name, "id");
    }

    #[tokio::test]
    async fn refuses_to_create_a_missing_file_unless_asked() {
        let dir = tempfile::tempdir().unwrap();
        let Err(err) = SqliteSession::open(&dir.path().join("nope.sqlite"), false).await else {
            panic!("a missing SQLite file should not be created");
        };
        let message = err.to_string();
        assert!(!message.is_empty());
    }
}
