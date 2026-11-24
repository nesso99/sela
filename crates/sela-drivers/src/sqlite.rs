use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};

use tokio_util::sync::CancellationToken;

use sqlx::Row;

use sela_core::{
    quote_ident, ApplyChangesResult, ChangePlan, ColumnStructure, DatabaseSession, DbError,
    Dialect, ForeignKeyInfo, IndexInfo, QueryPage, RelationKind, SafeMode, SchemaInfo, StreamBatch,
    StreamOutcome, TableInfo, TableStructure, Value,
};

pub const SAMPLE_SQL: &str = include_str!("../../../scripts/seed-sqlite.sql");

pub struct SqliteSession {
    pool: SqlitePool,
}

impl SqliteSession {
    pub async fn open(path: &Path, create_if_missing: bool) -> Result<Self, DbError> {
        Self::open_with_safe_mode(path, create_if_missing, SafeMode::Off).await
    }

    pub async fn open_with_safe_mode(
        path: &Path,
        create_if_missing: bool,
        safe_mode: SafeMode,
    ) -> Result<Self, DbError> {
        let mut options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(create_if_missing)
            .busy_timeout(Duration::from_secs(5));
        if safe_mode == SafeMode::ReadOnly {
            // URI mode read-only; blocks INSERT/UPDATE/DELETE at the engine.
            options = options.read_only(true);
        }
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .acquire_timeout(Duration::from_secs(10))
            .connect_with(options)
            .await
            .map_err(crate::sqlx_err)?;
        Ok(Self { pool })
    }

    async fn load_columns(
        &self,
        schema_q: &str,
        table: &str,
    ) -> Result<Vec<ColumnStructure>, DbError> {
        // Prefer xinfo so VIRTUAL/STORED generated columns (hidden 2/3) are marked.
        let xinfo_sql = format!(
            "SELECT name, type, \"notnull\", dflt_value, pk, hidden
             FROM {schema_q}.pragma_table_xinfo(?)
             ORDER BY cid"
        );
        match crate::query(&xinfo_sql)
            .bind(table)
            .fetch_all(&self.pool)
            .await
        {
            Ok(rows) => {
                let mut columns = Vec::with_capacity(rows.len());
                for row in rows {
                    let notnull: i64 = row.try_get("notnull").map_err(crate::sqlx_err)?;
                    let default: Option<String> =
                        row.try_get("dflt_value").map_err(crate::sqlx_err)?;
                    let hidden: i64 = row.try_get("hidden").map_err(crate::sqlx_err)?;
                    columns.push(ColumnStructure {
                        name: row.try_get("name").map_err(crate::sqlx_err)?,
                        type_name: row.try_get("type").map_err(crate::sqlx_err)?,
                        nullable: notnull == 0,
                        default,
                        comment: None,
                        generated: hidden == 2 || hidden == 3,
                    });
                }
                return Ok(columns);
            }
            Err(_) => {
                // Older SQLite builds without pragma_table_xinfo.
            }
        }

        let sql = format!(
            "SELECT name, type, \"notnull\", dflt_value, pk
             FROM {schema_q}.pragma_table_info(?)
             ORDER BY cid"
        );
        let rows = crate::query(&sql)
            .bind(table)
            .fetch_all(&self.pool)
            .await
            .map_err(crate::sqlx_err)?;
        let mut columns = Vec::with_capacity(rows.len());
        for row in rows {
            let notnull: i64 = row.try_get("notnull").map_err(crate::sqlx_err)?;
            let default: Option<String> = row.try_get("dflt_value").map_err(crate::sqlx_err)?;
            columns.push(ColumnStructure {
                name: row.try_get("name").map_err(crate::sqlx_err)?,
                type_name: row.try_get("type").map_err(crate::sqlx_err)?,
                nullable: notnull == 0,
                default,
                comment: None,
                generated: false,
            });
        }
        Ok(columns)
    }

    async fn load_indexes(&self, schema_q: &str, table: &str) -> Result<Vec<IndexInfo>, DbError> {
        let list_sql = format!(
            "SELECT name, \"unique\", origin
             FROM {schema_q}.pragma_index_list(?)
             ORDER BY seq"
        );
        let rows = crate::query(&list_sql)
            .bind(table)
            .fetch_all(&self.pool)
            .await
            .map_err(crate::sqlx_err)?;

        // Primary-key columns from table_info (autoindexes may omit a usable name).
        let pk_sql =
            format!("SELECT name FROM {schema_q}.pragma_table_info(?) WHERE pk > 0 ORDER BY pk");
        let pk_rows = crate::query(&pk_sql)
            .bind(table)
            .fetch_all(&self.pool)
            .await
            .map_err(crate::sqlx_err)?;
        let pk_columns: Vec<String> = pk_rows
            .into_iter()
            .map(|row| row.try_get("name").map_err(crate::sqlx_err))
            .collect::<Result<_, _>>()?;

        let mut indexes = Vec::new();
        if !pk_columns.is_empty() {
            indexes.push(IndexInfo {
                name: format!("sqlite_autoindex_{table}_pk"),
                columns: pk_columns,
                unique: true,
                primary: true,
                definition: None,
            });
        }

        for row in rows {
            let name: String = row.try_get("name").map_err(crate::sqlx_err)?;
            let unique: i64 = row.try_get("unique").map_err(crate::sqlx_err)?;
            let origin: String = row.try_get("origin").map_err(crate::sqlx_err)?;
            // Skip the autoindex that backs the PRIMARY KEY; we already added it above.
            if origin == "pk" {
                continue;
            }
            let info_sql =
                format!("SELECT name FROM {schema_q}.pragma_index_info(?) ORDER BY seqno");
            let col_rows = crate::query(&info_sql)
                .bind(&name)
                .fetch_all(&self.pool)
                .await
                .map_err(crate::sqlx_err)?;
            let columns = col_rows
                .into_iter()
                .map(|col| col.try_get::<String, _>("name").map_err(crate::sqlx_err))
                .collect::<Result<Vec<_>, _>>()?;
            let def_sql = format!(
                "SELECT sql FROM {schema_q}.sqlite_schema WHERE type = 'index' AND name = ?"
            );
            let definition = match crate::query(&def_sql)
                .bind(&name)
                .fetch_optional(&self.pool)
                .await
                .map_err(crate::sqlx_err)?
            {
                Some(row) => row.try_get("sql").map_err(crate::sqlx_err)?,
                None => None,
            };
            indexes.push(IndexInfo {
                name,
                columns,
                unique: unique != 0,
                primary: false,
                definition,
            });
        }
        Ok(indexes)
    }

    async fn load_foreign_keys(
        &self,
        schema_q: &str,
        table: &str,
    ) -> Result<Vec<ForeignKeyInfo>, DbError> {
        let sql = format!(
            "SELECT id, seq, \"table\" AS ref_table, \"from\" AS from_col, \"to\" AS to_col,
                    on_update, on_delete
             FROM {schema_q}.pragma_foreign_key_list(?)
             ORDER BY id, seq"
        );
        let rows = crate::query(&sql)
            .bind(table)
            .fetch_all(&self.pool)
            .await
            .map_err(crate::sqlx_err)?;

        let mut by_id: Vec<(i64, ForeignKeyInfo)> = Vec::new();
        for row in rows {
            let id: i64 = row.try_get("id").map_err(crate::sqlx_err)?;
            let from_col: String = row.try_get("from_col").map_err(crate::sqlx_err)?;
            let to_col: Option<String> = row.try_get("to_col").map_err(crate::sqlx_err)?;
            if let Some((_, existing)) =
                by_id.iter_mut().find(|(existing_id, _)| *existing_id == id)
            {
                existing.columns.push(from_col);
                if let Some(to_col) = to_col {
                    existing.ref_columns.push(to_col);
                }
            } else {
                by_id.push((
                    id,
                    ForeignKeyInfo {
                        name: format!("fk_{table}_{id}"),
                        columns: vec![from_col],
                        ref_schema: None,
                        ref_table: row.try_get("ref_table").map_err(crate::sqlx_err)?,
                        ref_columns: to_col.into_iter().collect(),
                        on_update: row.try_get("on_update").map_err(crate::sqlx_err)?,
                        on_delete: row.try_get("on_delete").map_err(crate::sqlx_err)?,
                    },
                ));
            }
        }
        Ok(by_id.into_iter().map(|(_, fk)| fk).collect())
    }

    async fn relation_exists(&self, schema_q: &str, table: &str) -> Result<bool, DbError> {
        let sql = format!(
            "SELECT 1 AS ok FROM {schema_q}.sqlite_schema
             WHERE type IN ('table', 'view') AND name = ?
             LIMIT 1"
        );
        let row = crate::query(&sql)
            .bind(table)
            .fetch_optional(&self.pool)
            .await
            .map_err(crate::sqlx_err)?;
        Ok(row.is_some())
    }

    async fn load_ddl(&self, schema_q: &str, table: &str) -> Result<String, DbError> {
        let table_sql = format!(
            "SELECT sql FROM {schema_q}.sqlite_schema
             WHERE type IN ('table', 'view') AND name = ?
             LIMIT 1"
        );
        let row = crate::query(&table_sql)
            .bind(table)
            .fetch_optional(&self.pool)
            .await
            .map_err(crate::sqlx_err)?
            .ok_or_else(|| DbError::query(format!("table `{table}` not found")))?;
        let mut ddl: String = row
            .try_get::<Option<String>, _>("sql")
            .map_err(crate::sqlx_err)?
            .unwrap_or_default();
        let index_sql = format!(
            "SELECT sql FROM {schema_q}.sqlite_schema
             WHERE type = 'index' AND tbl_name = ? AND sql IS NOT NULL
             ORDER BY name"
        );
        let index_rows = crate::query(&index_sql)
            .bind(table)
            .fetch_all(&self.pool)
            .await
            .map_err(crate::sqlx_err)?;
        for index_row in index_rows {
            let sql: String = index_row.try_get("sql").map_err(crate::sqlx_err)?;
            if !ddl.is_empty() {
                ddl.push_str(";\n");
            }
            ddl.push_str(&sql);
        }
        if !ddl.is_empty() && !ddl.ends_with(';') {
            ddl.push(';');
        }
        Ok(ddl)
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
        let rows = crate::query(&sql)
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

    async fn count_query(&self, sql: &str, params: &[Value]) -> Result<u64, DbError> {
        crate::count_sql!(&self.pool, sql, params)
    }

    async fn apply_changes(&self, plan: &ChangePlan) -> Result<ApplyChangesResult, DbError> {
        crate::change_exec::apply_change_plan!(&self.pool, plan, Dialect::Sqlite)
    }

    async fn estimate_table_rows(
        &self,
        _schema: &str,
        _table: &str,
    ) -> Result<Option<u64>, DbError> {
        // SQLite has no cheap planner estimate comparable to reltuples / table_rows.
        Ok(None)
    }

    async fn describe_table(&self, schema: &str, table: &str) -> Result<TableStructure, DbError> {
        let schema_q =
            quote_ident(Dialect::Sqlite, schema).map_err(|err| DbError::query(err.to_string()))?;
        if !self.relation_exists(&schema_q, table).await? {
            return Err(DbError::query(format!(
                "table `{schema}.{table}` not found"
            )));
        }
        let columns = self.load_columns(&schema_q, table).await?;
        let indexes = self.load_indexes(&schema_q, table).await?;
        let foreign_keys = self.load_foreign_keys(&schema_q, table).await?;
        let ddl = self.load_ddl(&schema_q, table).await?;
        Ok(TableStructure {
            schema: schema.to_string(),
            table: table.to_string(),
            columns,
            indexes,
            foreign_keys,
            ddl,
        })
    }

    async fn table_ddl(&self, schema: &str, table: &str) -> Result<String, DbError> {
        let schema_q =
            quote_ident(Dialect::Sqlite, schema).map_err(|err| DbError::query(err.to_string()))?;
        self.load_ddl(&schema_q, table).await
    }

    async fn stream_query(
        &self,
        sql: &str,
        params: &[Value],
        batch_size: usize,
        row_cap: Option<std::sync::Arc<std::sync::atomic::AtomicUsize>>,
        finish_at_cap: bool,
        cancel: CancellationToken,
        sink: async_channel::Sender<StreamBatch>,
    ) -> Result<StreamOutcome, DbError> {
        crate::stream::stream_sqlite(
            &self.pool,
            sql,
            params,
            batch_size,
            row_cap,
            finish_at_cap,
            cancel,
            sink,
        )
        .await
    }

    async fn close(&self) {
        self.pool.close().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    use sela_core::{
        preview_table, run_user_sql, split_statements, ChangePlan, ChangeSet, Dialect,
        StatementOutcome, TablePreview, TableRef, Value, PAGE_SIZE,
    };

    async fn seeded() -> (tempfile::TempDir, SqliteSession) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("library.sqlite");
        let session = SqliteSession::open(&path, true).await.unwrap();
        for statement in split_statements(SAMPLE_SQL, Dialect::Sqlite).unwrap() {
            session.execute(&statement.text).await.unwrap();
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
            TablePreview::default(),
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
        let exact = sela_core::exact_table_rows(&session, "main", "books", &[])
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
            TablePreview::default(),
        )
        .await
        .unwrap();
        let StatementOutcome::Rows(first) = first else {
            panic!("rows");
        };
        assert_eq!(first.page.rows.len(), 100);
        assert!(first.total_rows.is_none());
        assert_eq!(
            sela_core::exact_table_rows(&session, "main", "readings", &[])
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
            TablePreview::default(),
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
                        &[],
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
        let produce =
            session.stream_query(sql, &[], 10, Some(cap), true, CancellationToken::new(), tx);
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
            session
                .count_query("SELECT COUNT(*) FROM t", &[])
                .await
                .unwrap()
        };
        let after_first = count(&session).await;
        assert_eq!(after_first, 5, "INSERT should have written all five rows");

        // Simulate the load-more fallback: re-run only when the classifier allows it.
        if is_safe_to_rerun_for_load_more(sql) {
            let cap = Arc::new(AtomicUsize::new(10));
            let (tx, rx) = async_channel::bounded(4);
            let produce =
                session.stream_query(sql, &[], 10, Some(cap), true, CancellationToken::new(), tx);
            let drain = async { while rx.recv().await.is_ok() {} };
            let _ = tokio::join!(produce, drain);
        }
        let after_load_more = count(&session).await;
        assert_eq!(
            after_load_more, after_first,
            "load-more must not re-run INSERT … RETURNING (row count changed)"
        );
    }

    #[tokio::test]
    async fn describe_table_reports_columns_indexes_fks_and_ddl() {
        let dir = tempfile::tempdir().unwrap();
        let session = SqliteSession::open(&dir.path().join("structure.sqlite"), true)
            .await
            .unwrap();
        session
            .execute(
                "CREATE TABLE sela_struct_parent (
                    id INTEGER PRIMARY KEY,
                    label TEXT NOT NULL,
                    region TEXT NOT NULL,
                    UNIQUE (label, region)
                )",
            )
            .await
            .unwrap();
        session
            .execute(
                "CREATE TABLE sela_struct_child (
                    id INTEGER PRIMARY KEY,
                    parent_id INTEGER NOT NULL,
                    parent_label TEXT NOT NULL,
                    parent_region TEXT NOT NULL,
                    code TEXT NOT NULL,
                    note TEXT DEFAULT 'n/a',
                    score INTEGER,
                    FOREIGN KEY (parent_id) REFERENCES sela_struct_parent(id),
                    FOREIGN KEY (parent_label, parent_region)
                      REFERENCES sela_struct_parent(label, region)
                )",
            )
            .await
            .unwrap();
        session
            .execute("CREATE UNIQUE INDEX sela_struct_child_code_uidx ON sela_struct_child(code)")
            .await
            .unwrap();
        session
            .execute(
                "CREATE INDEX sela_struct_child_parent_score_idx
                 ON sela_struct_child(parent_id, score)",
            )
            .await
            .unwrap();
        // Mixed-case / quoted identifier round-trip.
        session
            .execute("CREATE TABLE \"Weird Name\" (\"Col Id\" INTEGER PRIMARY KEY)")
            .await
            .unwrap();

        let structure = session
            .describe_table("main", "sela_struct_child")
            .await
            .unwrap();
        assert_eq!(structure.table, "sela_struct_child");

        let note = structure
            .columns
            .iter()
            .find(|column| column.name == "note")
            .expect("note column");
        assert!(note.nullable);
        assert_eq!(note.default.as_deref(), Some("'n/a'"));

        assert!(structure.indexes.iter().any(|index| index.primary));
        assert!(structure.indexes.iter().any(|index| {
            index.name == "sela_struct_child_code_uidx" && index.unique && !index.primary
        }));
        assert!(structure.indexes.iter().any(|index| {
            index.name == "sela_struct_child_parent_score_idx"
                && index.columns == vec!["parent_id".to_string(), "score".to_string()]
        }));

        assert_eq!(structure.foreign_keys.len(), 2);
        assert!(structure.foreign_keys.iter().any(|fk| {
            fk.columns == vec!["parent_id".to_string()]
                && fk.ref_table == "sela_struct_parent"
                && fk.ref_columns == vec!["id".to_string()]
        }));
        assert!(structure.foreign_keys.iter().any(|fk| {
            fk.columns == vec!["parent_label".to_string(), "parent_region".to_string()]
                && fk.ref_columns == vec!["label".to_string(), "region".to_string()]
        }));

        let ddl = session
            .table_ddl("main", "sela_struct_child")
            .await
            .unwrap();
        assert!(ddl.to_uppercase().contains("CREATE TABLE"));
        assert!(ddl.contains("sela_struct_child"));
        assert!(ddl.contains("sela_struct_child_code_uidx"));
        assert!(structure.ddl.contains("CREATE TABLE"));

        let quoted = session.describe_table("main", "Weird Name").await.unwrap();
        assert_eq!(quoted.columns[0].name, "Col Id");
        assert!(quoted.indexes.iter().any(|index| index.primary));

        let missing = session.describe_table("main", "no_such_table").await;
        assert!(missing.is_err(), "missing table must surface an error");
    }

    #[tokio::test]
    async fn filter_and_sort_bound_params_with_special_chars() {
        use sela_core::{FilterCondition, FilterOp, SortSpec, StatementOutcome, TablePreview};

        let dir = tempfile::tempdir().unwrap();
        let session = SqliteSession::open(&dir.path().join("filter.sqlite"), true)
            .await
            .unwrap();
        session
            .execute(
                "CREATE TABLE sela_filter (
                    id INTEGER PRIMARY KEY,
                    name TEXT NOT NULL,
                    note TEXT
                )",
            )
            .await
            .unwrap();
        // Values with ', %, _, \, !, and unicode — must match as literals, not wildcards.
        session
            .execute(
                "INSERT INTO sela_filter (id, name, note) VALUES
                 (1, 'Ada', 'plain'),
                 (2, '100%', 'pct'),
                 (3, 'a_b', 'under'),
                 (4, 'back\\slash', 'esc'),
                 (5, 'O''Brien', 'quote'),
                 (6, 'ユニコード', 'unicode'),
                 (7, 'drop; --', 'inject'),
                 (8, 'wow!', 'bang')",
            )
            .await
            .unwrap();

        let contains = preview_table(
            &session,
            "main",
            "sela_filter",
            PAGE_SIZE,
            0,
            CancellationToken::new(),
            TablePreview {
                filters: &[FilterCondition::new("name", FilterOp::Contains, "%")],
                sorts: &[SortSpec::asc("id")],
            },
        )
        .await
        .unwrap();
        let StatementOutcome::Rows(rows) = contains else {
            panic!("rows");
        };
        assert_eq!(rows.page.rows.len(), 1);
        assert_eq!(rows.page.rows[0][1], Value::text("100%"));
        assert_eq!(rows.row_count_kind, sela_core::RowCountKind::Unknown);

        let underscore = preview_table(
            &session,
            "main",
            "sela_filter",
            PAGE_SIZE,
            0,
            CancellationToken::new(),
            TablePreview {
                filters: &[FilterCondition::new("name", FilterOp::Contains, "_")],
                sorts: &[],
            },
        )
        .await
        .unwrap();
        let StatementOutcome::Rows(rows) = underscore else {
            panic!("rows");
        };
        assert_eq!(rows.page.rows.len(), 1);
        assert_eq!(rows.page.rows[0][1], Value::text("a_b"));

        let bang = preview_table(
            &session,
            "main",
            "sela_filter",
            PAGE_SIZE,
            0,
            CancellationToken::new(),
            TablePreview {
                filters: &[FilterCondition::new("name", FilterOp::Contains, "!")],
                sorts: &[],
            },
        )
        .await
        .unwrap();
        let StatementOutcome::Rows(rows) = bang else {
            panic!("rows");
        };
        assert_eq!(rows.page.rows.len(), 1);
        assert_eq!(rows.page.rows[0][1], Value::text("wow!"));

        let backslash = preview_table(
            &session,
            "main",
            "sela_filter",
            PAGE_SIZE,
            0,
            CancellationToken::new(),
            TablePreview {
                filters: &[FilterCondition::new("name", FilterOp::Contains, r"\")],
                sorts: &[],
            },
        )
        .await
        .unwrap();
        let StatementOutcome::Rows(rows) = backslash else {
            panic!("rows");
        };
        assert_eq!(rows.page.rows.len(), 1);
        assert_eq!(rows.page.rows[0][1], Value::text(r"back\slash"));

        let starts = preview_table(
            &session,
            "main",
            "sela_filter",
            PAGE_SIZE,
            0,
            CancellationToken::new(),
            TablePreview {
                filters: &[FilterCondition::new("name", FilterOp::StartsWith, "ユニ")],
                sorts: &[SortSpec::desc("id")],
            },
        )
        .await
        .unwrap();
        let StatementOutcome::Rows(rows) = starts else {
            panic!("rows");
        };
        assert_eq!(rows.page.rows.len(), 1);
        assert_eq!(rows.page.rows[0][1], Value::text("ユニコード"));

        // Injection attempt is a literal equality, not executed SQL.
        let inject = preview_table(
            &session,
            "main",
            "sela_filter",
            PAGE_SIZE,
            0,
            CancellationToken::new(),
            TablePreview {
                filters: &[FilterCondition::new(
                    "name",
                    FilterOp::Eq,
                    "1; DROP TABLE sela_filter;--",
                )],
                sorts: &[],
            },
        )
        .await
        .unwrap();
        let StatementOutcome::Rows(rows) = inject else {
            panic!("rows");
        };
        assert!(rows.page.rows.is_empty());
        let still = sela_core::exact_table_rows(&session, "main", "sela_filter", &[])
            .await
            .unwrap();
        assert_eq!(still, 8);

        let sorted =
            preview_table(
                &session,
                "main",
                "sela_filter",
                PAGE_SIZE,
                0,
                CancellationToken::new(),
                TablePreview {
                    filters: &[
                        FilterCondition::new("id", FilterOp::In, "1, 3, 5").with_type("integer")
                    ],
                    sorts: &[SortSpec::desc("id")],
                },
            )
            .await
            .unwrap();
        let StatementOutcome::Rows(rows) = sorted else {
            panic!("rows");
        };
        assert_eq!(rows.page.rows.len(), 3);
        assert_eq!(rows.page.rows[0][0], Value::Int(5));
        assert_eq!(rows.page.rows[1][0], Value::Int(3));
        assert_eq!(rows.page.rows[2][0], Value::Int(1));

        let exact = sela_core::exact_table_rows(
            &session,
            "main",
            "sela_filter",
            &[FilterCondition::new("note", FilterOp::IsNotNull, "")],
        )
        .await
        .unwrap();
        assert_eq!(exact, 8);
    }

    /// Menu `LIKE` with `ESCAPE '!'`: `!%` / `!_` / `!!` match literal `%` / `_` / `!`.
    #[tokio::test]
    async fn like_escape_matches_literals() {
        use sela_core::{FilterCondition, FilterOp, SortSpec, StatementOutcome, TablePreview};

        let dir = tempfile::tempdir().unwrap();
        let session = SqliteSession::open(&dir.path().join("like_escape.sqlite"), true)
            .await
            .unwrap();
        session
            .execute(
                "CREATE TABLE sela_like_escape (
                    id INTEGER PRIMARY KEY,
                    name TEXT NOT NULL
                )",
            )
            .await
            .unwrap();
        session
            .execute(
                "INSERT INTO sela_like_escape (id, name) VALUES
                 (1, '100%'),
                 (2, '100x'),
                 (3, 'a_b'),
                 (4, 'axb'),
                 (5, 'wow!')",
            )
            .await
            .unwrap();

        let pct = preview_table(
            &session,
            "main",
            "sela_like_escape",
            PAGE_SIZE,
            0,
            CancellationToken::new(),
            TablePreview {
                filters: &[FilterCondition::new("name", FilterOp::Like, "100!%")],
                sorts: &[SortSpec::asc("id")],
            },
        )
        .await
        .unwrap();
        let StatementOutcome::Rows(rows) = pct else {
            panic!("rows");
        };
        assert_eq!(rows.page.rows.len(), 1);
        assert_eq!(rows.page.rows[0][1], Value::text("100%"));

        let under = preview_table(
            &session,
            "main",
            "sela_like_escape",
            PAGE_SIZE,
            0,
            CancellationToken::new(),
            TablePreview {
                filters: &[FilterCondition::new("name", FilterOp::Like, "a!_b")],
                sorts: &[],
            },
        )
        .await
        .unwrap();
        let StatementOutcome::Rows(rows) = under else {
            panic!("rows");
        };
        assert_eq!(rows.page.rows.len(), 1);
        assert_eq!(rows.page.rows[0][1], Value::text("a_b"));

        let bang = preview_table(
            &session,
            "main",
            "sela_like_escape",
            PAGE_SIZE,
            0,
            CancellationToken::new(),
            TablePreview {
                filters: &[FilterCondition::new("name", FilterOp::Like, "wow!!")],
                sorts: &[],
            },
        )
        .await
        .unwrap();
        let StatementOutcome::Rows(rows) = bang else {
            panic!("rows");
        };
        assert_eq!(rows.page.rows.len(), 1);
        assert_eq!(rows.page.rows[0][1], Value::text("wow!"));
    }

    #[tokio::test]
    async fn apply_change_plan_commits_and_rolls_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("changes.sqlite");
        let session = SqliteSession::open(&path, true).await.unwrap();
        session
            .execute(
                "CREATE TABLE sela_changes (
                    id INTEGER PRIMARY KEY,
                    name TEXT NOT NULL,
                    note TEXT
                )",
            )
            .await
            .unwrap();
        session
            .execute("INSERT INTO sela_changes (id, name, note) VALUES (1, 'a', NULL)")
            .await
            .unwrap();
        session
            .execute("INSERT INTO sela_changes (id, name, note) VALUES (2, 'b', 'keep')")
            .await
            .unwrap();

        let mut cs = ChangeSet::try_new(
            TableRef::new("main", "sela_changes"),
            vec!["id".into()],
            vec!["id".into(), "name".into(), "note".into()],
        )
        .unwrap();
        cs.set_cell(
            &[Value::Int(1)],
            "name",
            Value::text("a"),
            Value::text("alpha"),
        )
        .unwrap();
        cs.delete_row(&[Value::Int(2)]).unwrap();
        let mut values = BTreeMap::new();
        values.insert("id".into(), Value::Int(3));
        values.insert("name".into(), Value::text("c"));
        values.insert("note".into(), Value::Null);
        cs.insert_row(values).unwrap();

        let plan = ChangePlan::build(&cs, Dialect::Sqlite).unwrap();
        let result = session.apply_changes(&plan).await.unwrap();
        assert_eq!(result.statements_executed, 3);

        let page = session
            .query("SELECT id, name, note FROM sela_changes ORDER BY id")
            .await
            .unwrap();
        assert_eq!(page.rows.len(), 2);
        assert_eq!(page.rows[0][0], Value::Int(1));
        assert_eq!(page.rows[0][1], Value::text("alpha"));
        assert_eq!(page.rows[1][0], Value::Int(3));

        // Second statement fails (duplicate PK) → whole transaction rolls back.
        let mut bad = ChangeSet::try_new(
            TableRef::new("main", "sela_changes"),
            vec!["id".into()],
            vec!["id".into(), "name".into(), "note".into()],
        )
        .unwrap();
        bad.set_cell(
            &[Value::Int(1)],
            "name",
            Value::text("alpha"),
            Value::text("should_rollback"),
        )
        .unwrap();
        let mut dup = BTreeMap::new();
        dup.insert("id".into(), Value::Int(3));
        dup.insert("name".into(), Value::text("dup"));
        bad.insert_row(dup).unwrap();
        let bad_plan = ChangePlan::build(&bad, Dialect::Sqlite).unwrap();
        assert!(session.apply_changes(&bad_plan).await.is_err());

        let page = session
            .query("SELECT name FROM sela_changes WHERE id = 1")
            .await
            .unwrap();
        assert_eq!(page.rows[0][0], Value::text("alpha"));
    }

    #[tokio::test]
    async fn apply_change_plan_missing_update_and_delete_roll_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("changes_guard.sqlite");
        let session = SqliteSession::open(&path, true).await.unwrap();
        session
            .execute(
                "CREATE TABLE sela_guard (
                    id INTEGER PRIMARY KEY,
                    name TEXT NOT NULL
                )",
            )
            .await
            .unwrap();
        session
            .execute("INSERT INTO sela_guard (id, name) VALUES (1, 'a'), (2, 'b')")
            .await
            .unwrap();

        // UPDATE existing then UPDATE missing → ChangeConflict; first must not commit.
        let mut cs = ChangeSet::try_new(
            TableRef::new("main", "sela_guard"),
            vec!["id".into()],
            vec!["id".into(), "name".into()],
        )
        .unwrap();
        cs.set_cell(
            &[Value::Int(1)],
            "name",
            Value::text("a"),
            Value::text("changed"),
        )
        .unwrap();
        cs.set_cell(
            &[Value::Int(999)],
            "name",
            Value::text("ghost"),
            Value::text("nope"),
        )
        .unwrap();
        let plan = ChangePlan::build(&cs, Dialect::Sqlite).unwrap();
        let err = session.apply_changes(&plan).await.unwrap_err();
        match err {
            DbError::ChangeConflict {
                statement_index,
                rows_affected,
                ref pk,
                ..
            } => {
                assert_eq!(statement_index, 1);
                assert_eq!(rows_affected, 0);
                assert_eq!(pk, &["999".to_string()]);
            }
            other => panic!("expected ChangeConflict, got {other:?}"),
        }
        let page = session
            .query("SELECT name FROM sela_guard WHERE id = 1")
            .await
            .unwrap();
        assert_eq!(page.rows[0][0], Value::text("a"));

        // DELETE existing then DELETE missing → earlier delete must not commit.
        let mut cs = ChangeSet::try_new(
            TableRef::new("main", "sela_guard"),
            vec!["id".into()],
            vec!["id".into(), "name".into()],
        )
        .unwrap();
        cs.delete_row(&[Value::Int(2)]).unwrap();
        cs.delete_row(&[Value::Int(999)]).unwrap();
        let plan = ChangePlan::build(&cs, Dialect::Sqlite).unwrap();
        let err = session.apply_changes(&plan).await.unwrap_err();
        assert!(
            matches!(
                err,
                DbError::ChangeConflict {
                    statement_index: 1,
                    rows_affected: 0,
                    ..
                }
            ),
            "{err:?}"
        );
        let page = session
            .query("SELECT id FROM sela_guard WHERE id = 2")
            .await
            .unwrap();
        assert_eq!(page.rows.len(), 1, "earlier DELETE must roll back");
        let page = session
            .query("SELECT COUNT(*) AS c FROM sela_guard")
            .await
            .unwrap();
        assert_eq!(page.rows[0][0], Value::Int(2));
    }

    #[tokio::test]
    async fn sqlite_read_only_safe_mode_rejects_insert() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ro.sqlite");
        {
            let rw = SqliteSession::open(&path, true).await.unwrap();
            rw.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT NOT NULL)")
                .await
                .unwrap();
            rw.execute("INSERT INTO t (id, name) VALUES (1, 'a')")
                .await
                .unwrap();
            rw.close().await;
        }
        let session = SqliteSession::open_with_safe_mode(&path, false, SafeMode::ReadOnly)
            .await
            .unwrap();
        let err = session
            .execute("INSERT INTO t (id, name) VALUES (2, 'b')")
            .await
            .expect_err("INSERT must fail under SQLite read_only");
        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("readonly") || msg.contains("read-only") || msg.contains("read only"),
            "unexpected error: {err}"
        );
        let page = session.query("SELECT id FROM t").await.unwrap();
        assert_eq!(page.rows.len(), 1);
    }
}
