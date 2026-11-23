use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};

use tokio_util::sync::CancellationToken;

use sqlx::Row;

use sela_core::{
    quote_ident, DatabaseSession, DbError, Dialect, QueryPage, RelationKind, SchemaInfo,
    StreamBatch, StreamOutcome, TableInfo,
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
        let schema =
            quote_ident(Dialect::Sqlite, schema).map_err(|err| DbError::query(err.to_string()))?;
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
        crate::query_page!(&self.pool, sql, crate::read_sqlite_cell)
    }

    async fn execute(&self, sql: &str) -> Result<u64, DbError> {
        crate::exec_sql!(&self.pool, sql)
    }

    async fn count_query(&self, sql: &str) -> Result<u64, DbError> {
        crate::count_sql!(&self.pool, sql)
    }

    async fn estimate_table_rows(
        &self,
        _schema: &str,
        _table: &str,
    ) -> Result<Option<u64>, DbError> {
        // SQLite has no cheap planner estimate comparable to reltuples / table_rows.
        Ok(None)
    }

    async fn stream_query(
        &self,
        sql: &str,
        batch_size: usize,
        row_cap: Option<std::sync::Arc<std::sync::atomic::AtomicUsize>>,
        finish_at_cap: bool,
        cancel: CancellationToken,
        sink: async_channel::Sender<StreamBatch>,
    ) -> Result<StreamOutcome, DbError> {
        crate::stream::stream_sqlite(
            &self.pool,
            sql,
            batch_size,
            row_cap,
            finish_at_cap,
            cancel,
            sink,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sela_core::{
        preview_table, run_user_sql, split_statements, StatementOutcome, Value, PAGE_SIZE,
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

        let outcome = preview_table(
            &session,
            "main",
            "books",
            PAGE_SIZE,
            0,
            CancellationToken::new(),
        )
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
        // SQLite has no planner estimate; exact count is on demand.
        assert!(rows.total_rows.is_none());
        let exact = sela_core::exact_table_rows(&session, "main", "books")
            .await
            .unwrap();
        assert!(exact >= rows.page.rows.len() as u64);

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

        let first = preview_table(
            &session,
            "main",
            "readings",
            PAGE_SIZE,
            0,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let StatementOutcome::Rows(first) = first else {
            panic!("rows");
        };
        assert_eq!(first.page.rows.len(), 100);
        assert!(first.total_rows.is_none());
        assert_eq!(
            sela_core::exact_table_rows(&session, "main", "readings")
                .await
                .unwrap(),
            120
        );
        let pages_ix = first
            .page
            .columns
            .iter()
            .position(|column| column.name == "pages")
            .unwrap();
        assert_eq!(first.page.rows[0][pages_ix], Value::Int(1));

        let second = preview_table(
            &session,
            "main",
            "readings",
            PAGE_SIZE,
            100,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let StatementOutcome::Rows(second) = second else {
            panic!("rows");
        };
        assert_eq!(second.page.rows.len(), 20);

        let blob = run_user_sql(
            &session,
            "SELECT cover FROM readings WHERE id = 1",
            10,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let StatementOutcome::Rows(blob) = blob else {
            panic!("rows");
        };
        assert_eq!(blob.page.rows[0][0], Value::Bytes(b"hi".to_vec()));

        let commented = run_user_sql(
            &session,
            "select count(*) as n from readings -- tail",
            10,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let StatementOutcome::Rows(commented) = commented else {
            panic!("rows");
        };
        assert_eq!(commented.page.rows.len(), 1);

        let failed = run_user_sql(
            &session,
            "SELECT * FROM missing_table",
            10,
            CancellationToken::new(),
        )
        .await;
        assert!(failed.is_err());

        let executed = run_user_sql(
            &session,
            "INSERT INTO readings (id, pages, rating) VALUES (121, 3, 2.0)",
            10,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let StatementOutcome::Executed(executed) = executed else {
            panic!("executed");
        };
        assert_eq!(executed.rows_affected, 1);

        let empty = run_user_sql(
            &session,
            "SELECT id, note FROM readings WHERE 0",
            10,
            CancellationToken::new(),
        )
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

    #[tokio::test]
    async fn typed_value_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let session = SqliteSession::open(&dir.path().join("types.sqlite"), true)
            .await
            .unwrap();
        session
            .execute(
                "CREATE TABLE sela_types (
                    id INTEGER PRIMARY KEY,
                    flag BOOLEAN,
                    whole INTEGER,
                    real_num REAL,
                    label TEXT,
                    payload BLOB,
                    born DATE,
                    punched TIME,
                    seen DATETIME,
                    missing TEXT
                )",
            )
            .await
            .unwrap();
        session
            .execute(
                "INSERT INTO sela_types (
                    id, flag, whole, real_num, label, payload, born, punched, seen, missing
                 ) VALUES (
                    1, 1, -42, 1.5, 'hello', X'6869',
                    '2024-01-02', '03:04:05', '2024-01-02 03:04:05', NULL
                 )",
            )
            .await
            .unwrap();

        let outcome = run_user_sql(
            &session,
            "SELECT flag, whole, real_num, label, payload, born, punched, seen, missing
             FROM sela_types WHERE id = 1",
            10,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let StatementOutcome::Rows(rows) = outcome else {
            panic!("rows");
        };
        let row = &rows.page.rows[0];
        assert_eq!(row[0], Value::Bool(true));
        assert_eq!(row[1], Value::Int(-42));
        assert_eq!(row[2], Value::Float(1.5));
        assert_eq!(row[3], Value::Text("hello".into()));
        assert_eq!(row[4], Value::Bytes(b"hi".to_vec()));
        assert_eq!(
            row[5],
            Value::Date(chrono::NaiveDate::from_ymd_opt(2024, 1, 2).unwrap())
        );
        assert_eq!(
            row[6],
            Value::Time(chrono::NaiveTime::from_hms_opt(3, 4, 5).unwrap())
        );
        assert_eq!(
            row[7],
            Value::Timestamp(
                chrono::NaiveDate::from_ymd_opt(2024, 1, 2)
                    .unwrap()
                    .and_hms_opt(3, 4, 5)
                    .unwrap()
            )
        );
        assert!(row[8].is_null());
        assert_eq!(row[3].display(), "hello");
        assert_eq!(row[8].display(), "NULL");
    }

    #[tokio::test]
    async fn text_affinity_keeps_leading_zeros() {
        let dir = tempfile::tempdir().unwrap();
        let session = SqliteSession::open(&dir.path().join("text_affinity.sqlite"), true)
            .await
            .unwrap();
        session
            .execute(
                "CREATE TABLE codes (
                    id INTEGER PRIMARY KEY,
                    padded TEXT,
                    zip CHAR(5),
                    note VARCHAR(32)
                )",
            )
            .await
            .unwrap();
        session
            .execute("INSERT INTO codes (id, padded, zip, note) VALUES (1, '007', '02134', 'ok')")
            .await
            .unwrap();

        let outcome = run_user_sql(
            &session,
            "SELECT padded, zip, note FROM codes WHERE id = 1",
            10,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let StatementOutcome::Rows(rows) = outcome else {
            panic!("rows");
        };
        let row = &rows.page.rows[0];
        assert_eq!(row[0], Value::Text("007".into()));
        assert_eq!(row[1], Value::Text("02134".into()));
        assert_eq!(row[2], Value::Text("ok".into()));
        assert_eq!(row[0].display(), "007");
        assert_eq!(row[1].display(), "02134");
    }

    #[tokio::test]
    async fn sqlite_sql_cap_and_load_more() {
        let dir = tempfile::tempdir().unwrap();
        let session = SqliteSession::open(&dir.path().join("cap.sqlite"), true)
            .await
            .unwrap();
        session
            .execute("CREATE TABLE nums (id INTEGER PRIMARY KEY)")
            .await
            .unwrap();
        for id in 1..=1500 {
            session
                .execute(&format!("INSERT INTO nums (id) VALUES ({id})"))
                .await
                .unwrap();
        }

        let capped = run_user_sql(
            &session,
            "SELECT id FROM nums ORDER BY id",
            1000,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let StatementOutcome::Rows(capped) = capped else {
            panic!("rows");
        };
        assert_eq!(capped.page.rows.len(), 1000);
        assert!(capped.truncated);
        assert_eq!(capped.row_count_kind, sela_core::RowCountKind::AtLeast);

        // Load more via a higher cap (collect helper finishes at cap each call).
        let more = run_user_sql(
            &session,
            "SELECT id FROM nums ORDER BY id",
            1500,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let StatementOutcome::Rows(more) = more else {
            panic!("rows");
        };
        assert_eq!(more.page.rows.len(), 1500);
        assert!(!more.truncated);

        // Live stream pause + raise-cap path.
        let cap = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(1000));
        let (tx, rx) = async_channel::bounded(8);
        let cancel = CancellationToken::new();
        let produce = {
            let cap = cap.clone();
            let cancel = cancel.clone();
            async move {
                session
                    .stream_query(
                        "SELECT id FROM nums ORDER BY id",
                        100,
                        Some(cap),
                        false,
                        cancel,
                        tx,
                    )
                    .await
            }
        };
        let mut saw_pause = false;
        let mut rows = 0usize;
        let collector = async {
            while let Ok(batch) = rx.recv().await {
                if batch.paused_at_cap {
                    saw_pause = true;
                    cap.store(1500, std::sync::atomic::Ordering::SeqCst);
                    continue;
                }
                rows += batch.rows.len();
            }
            rows
        };
        let (outcome, collected) = tokio::join!(produce, collector);
        let outcome = outcome.unwrap();
        assert!(saw_pause, "expected pause at cap");
        assert_eq!(outcome.rows_delivered, 1500);
        assert_eq!(collected, 1500);
    }

    #[tokio::test]
    async fn sqlite_dml_returning_must_not_rerun_on_load_more() {
        use sela_core::is_safe_to_rerun_for_load_more;
        use std::sync::atomic::AtomicUsize;
        use std::sync::Arc;

        let dir = tempfile::tempdir().unwrap();
        let session = SqliteSession::open(&dir.path().join("returning.sqlite"), true)
            .await
            .unwrap();
        session
            .execute(
                "CREATE TABLE t (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    v TEXT NOT NULL
                )",
            )
            .await
            .unwrap();

        let sql = "INSERT INTO t (v) VALUES ('a'), ('b'), ('c'), ('d'), ('e') RETURNING id, v";
        assert!(
            !is_safe_to_rerun_for_load_more(sql),
            "INSERT … RETURNING must not be eligible for load-more re-run"
        );

        // Stream INSERT … RETURNING with a low cap (same path load-more would raise).
        let cap = Arc::new(AtomicUsize::new(2));
        let (tx, rx) = async_channel::bounded(4);
        let produce = session.stream_query(sql, 10, Some(cap), true, CancellationToken::new(), tx);
        let collect = async {
            let mut n = 0usize;
            while let Ok(batch) = rx.recv().await {
                n += batch.rows.len();
            }
            n
        };
        let (outcome, delivered) = tokio::join!(produce, collect);
        let outcome = outcome.expect("INSERT … RETURNING stream");
        assert!(
            outcome.truncated,
            "expected truncation at cap=2, delivered={delivered}"
        );
        assert_eq!(delivered, 2);

        let count = async |session: &SqliteSession| -> u64 {
            session.count_query("SELECT COUNT(*) FROM t").await.unwrap()
        };
        let after_first = count(&session).await;
        assert_eq!(after_first, 5, "INSERT should have written all five rows");

        // Simulate the load-more fallback: re-run only when the classifier allows it.
        if is_safe_to_rerun_for_load_more(sql) {
            let cap = Arc::new(AtomicUsize::new(10));
            let (tx, rx) = async_channel::bounded(4);
            let produce =
                session.stream_query(sql, 10, Some(cap), true, CancellationToken::new(), tx);
            let drain = async { while rx.recv().await.is_ok() {} };
            let _ = tokio::join!(produce, drain);
        }
        let after_load_more = count(&session).await;
        assert_eq!(
            after_load_more, after_first,
            "load-more must not re-run INSERT … RETURNING (row count changed)"
        );
    }
}
