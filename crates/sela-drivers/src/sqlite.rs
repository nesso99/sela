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
            "SELECT name, \"unique\", origin, partial
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
                partial: false,
                expression: false,
                definition: None,
            });
        }

        for row in rows {
            let name: String = row.try_get("name").map_err(crate::sqlx_err)?;
            let unique: i64 = row.try_get("unique").map_err(crate::sqlx_err)?;
            let origin: String = row.try_get("origin").map_err(crate::sqlx_err)?;
            let partial: i64 = row.try_get("partial").map_err(crate::sqlx_err)?;
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
            // Expression members report a NULL name; they are not plain columns.
            let mut columns = Vec::new();
            let mut expression = false;
            for col in col_rows {
                match col
                    .try_get::<Option<String>, _>("name")
                    .map_err(crate::sqlx_err)?
                {
                    Some(c) => columns.push(c),
                    None => expression = true,
                }
            }
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
                partial: partial != 0,
                expression,
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

    async fn apply_changes(
        &self,
        plan: &ChangePlan,
        cancel: CancellationToken,
    ) -> Result<ApplyChangesResult, DbError> {
        crate::change_exec::apply_sqlite(
            &self.pool,
            crate::change_exec::StatementSource::plan(plan),
            cancel,
        )
        .await
    }

    async fn apply_statement_stream(
        &self,
        dialect: Dialect,
        table: &sela_core::TableRef,
        statements: async_channel::Receiver<sela_core::PlanStreamItem>,
        cancel: CancellationToken,
    ) -> Result<ApplyChangesResult, DbError> {
        crate::change_exec::apply_sqlite(
            &self.pool,
            crate::change_exec::StatementSource::stream(dialect, table, statements),
            cancel,
        )
        .await
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
        // Stopping the stream at the cap can abort the SQLite statement before
        // commit (0 rows). Re-seed with a non-streaming INSERT so the load-more
        // check below still exercises a stable row count.
        session.execute("DELETE FROM t").await.unwrap();
        session
            .execute("INSERT INTO t (v) VALUES ('a'), ('b'), ('c'), ('d'), ('e')")
            .await
            .unwrap();
        let after_first = count(&session).await;
        assert_eq!(after_first, 5);

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
        let result = session
            .apply_changes(&plan, CancellationToken::new())
            .await
            .unwrap();
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
        assert!(session
            .apply_changes(&bad_plan, CancellationToken::new())
            .await
            .is_err());

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
        let err = session
            .apply_changes(&plan, CancellationToken::new())
            .await
            .unwrap_err();
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
        let err = session
            .apply_changes(&plan, CancellationToken::new())
            .await
            .unwrap_err();
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

    /// Partial (any whitespace around WHERE) and expression unique indexes must
    /// never become the row identity, since their WHERE could match many rows.
    #[tokio::test]
    async fn partial_and_expression_unique_indexes_are_not_row_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("uniq_bad.sqlite");
        let session = SqliteSession::open(&path, true).await.unwrap();
        for sql in [
            "CREATE TABLE p1 (a INTEGER NOT NULL, b INTEGER)",
            "CREATE UNIQUE INDEX p1_u ON p1 (a)\nWHERE b > 0",
            "CREATE TABLE p2 (a INTEGER NOT NULL, b INTEGER)",
            "CREATE UNIQUE INDEX p2_u ON p2 (a) WHERE(b > 0)",
            "CREATE TABLE e1 (a INTEGER NOT NULL, b TEXT NOT NULL)",
            "CREATE UNIQUE INDEX e1_u ON e1 (a, lower(b))",
        ] {
            session.execute(sql).await.unwrap();
        }
        for t in ["p1", "p2", "e1"] {
            let s = session.describe_table("main", t).await.unwrap();
            assert_eq!(s.row_key_columns(), None, "{t}");
        }
    }

    /// Row identity without a PK: unique NOT NULL key drives commit + optimistic WHERE;
    /// a table with neither key is rejected.
    #[tokio::test]
    async fn unique_not_null_key_commits_and_no_key_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("uniq_key.sqlite");
        let session = SqliteSession::open(&path, true).await.unwrap();
        session
            .execute("CREATE TABLE sela_uk (code TEXT NOT NULL UNIQUE, name TEXT)")
            .await
            .unwrap();
        session
            .execute("INSERT INTO sela_uk (code, name) VALUES ('a', 'x'), ('b', 'y')")
            .await
            .unwrap();
        session
            .execute("CREATE TABLE sela_nokey (code TEXT, name TEXT)")
            .await
            .unwrap();

        let structure = session.describe_table("main", "sela_uk").await.unwrap();
        let mut cs = ChangeSet::from_structure(&structure).unwrap();
        assert_eq!(cs.pk_columns(), ["code".to_string()]);
        cs.set_cell(
            &[Value::text("a")],
            "name",
            Value::text("x"),
            Value::text("changed"),
        )
        .unwrap();
        let plan = ChangePlan::build(&cs, Dialect::Sqlite).unwrap();
        session
            .apply_changes(&plan, CancellationToken::new())
            .await
            .unwrap();
        let page = session
            .query("SELECT name FROM sela_uk WHERE code = 'a'")
            .await
            .unwrap();
        assert_eq!(page.rows[0][0], Value::text("changed"));

        // Optimistic WHERE: stale old value conflicts and does not apply.
        let mut cs = ChangeSet::from_structure(&structure).unwrap();
        cs.set_cell(
            &[Value::text("a")],
            "name",
            Value::text("stale"),
            Value::text("again"),
        )
        .unwrap();
        let plan = ChangePlan::build(&cs, Dialect::Sqlite).unwrap();
        let err = session
            .apply_changes(&plan, CancellationToken::new())
            .await
            .unwrap_err();
        assert!(matches!(err, DbError::ChangeConflict { .. }), "{err:?}");

        let nokey = session.describe_table("main", "sela_nokey").await.unwrap();
        assert!(ChangeSet::from_structure(&nokey).is_err());
    }

    /// CAN-02 (SQLite cooperative): pre-cancelled token → nothing applied.
    /// Blocked-writer interrupt is not available (M1 SQLite cancel limitation).
    #[tokio::test]
    async fn can02_sqlite_apply_changes_cancel_before_begin_applies_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("changes_cancel.sqlite");
        let session = SqliteSession::open(&path, true).await.unwrap();
        session
            .execute(
                "CREATE TABLE sela_cancel (
                    id INTEGER PRIMARY KEY,
                    name TEXT NOT NULL
                )",
            )
            .await
            .unwrap();
        session
            .execute("INSERT INTO sela_cancel (id, name) VALUES (1, 'a')")
            .await
            .unwrap();

        let mut cs = ChangeSet::try_new(
            TableRef::new("main", "sela_cancel"),
            vec!["id".into()],
            vec!["id".into(), "name".into()],
        )
        .unwrap();
        cs.set_cell(
            &[Value::Int(1)],
            "name",
            Value::text("a"),
            Value::text("cancelled"),
        )
        .unwrap();
        let plan = ChangePlan::build(&cs, Dialect::Sqlite).unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let err = session
            .apply_changes(&plan, cancel)
            .await
            .expect_err("pre-cancelled apply must fail");
        assert!(
            matches!(err, DbError::Cancelled { .. }),
            "expected Cancelled, got {err:?}"
        );
        let page = session
            .query("SELECT name FROM sela_cancel WHERE id = 1")
            .await
            .unwrap();
        assert_eq!(page.rows[0][0], Value::text("a"));
    }

    /// OW-02: concurrent non-PK edit → ChangeConflict + full rollback.
    #[tokio::test]
    async fn ow02_sqlite_optimistic_conflict_rolls_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("changes_opt.sqlite");
        let session = SqliteSession::open(&path, true).await.unwrap();
        session
            .execute(
                "CREATE TABLE sela_opt (
                    id INTEGER PRIMARY KEY,
                    name TEXT NOT NULL,
                    note TEXT
                )",
            )
            .await
            .unwrap();
        session
            .execute("INSERT INTO sela_opt (id, name, note) VALUES (1, 'a', NULL), (2, 'b', 'x')")
            .await
            .unwrap();

        // Concurrent edit of the non-PK column the plan expects.
        session
            .execute("UPDATE sela_opt SET name = 'concurrent' WHERE id = 1")
            .await
            .unwrap();

        let mut cs = ChangeSet::try_new(
            TableRef::new("main", "sela_opt"),
            vec!["id".into()],
            vec!["id".into(), "name".into(), "note".into()],
        )
        .unwrap();
        cs.set_cell(
            &[Value::Int(1)],
            "name",
            Value::text("a"),
            Value::text("mine"),
        )
        .unwrap();
        // Also delete another row so we can prove rollback of earlier work.
        let mut expected = BTreeMap::new();
        expected.insert("name".into(), Value::text("b"));
        expected.insert("note".into(), Value::text("x"));
        cs.delete_row_with_expected(&[Value::Int(2)], expected)
            .unwrap();

        let plan = ChangePlan::build(&cs, Dialect::Sqlite).unwrap();
        // DELETE runs first; UPDATE with stale name expectation conflicts.
        let err = session
            .apply_changes(&plan, CancellationToken::new())
            .await
            .unwrap_err();
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
            .query("SELECT id, name FROM sela_opt ORDER BY id")
            .await
            .unwrap();
        assert_eq!(
            page.rows.len(),
            2,
            "DELETE must roll back with the conflict"
        );
        assert_eq!(page.rows[0][1], Value::text("concurrent"));
        assert_eq!(page.rows[1][1], Value::text("b"));
    }

    /// OW-03: commit succeeds with optimistic WHERE, including NULL old non-PK.
    #[tokio::test]
    async fn ow03_sqlite_optimistic_commit_succeeds_with_null_old() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("changes_opt_ok.sqlite");
        let session = SqliteSession::open(&path, true).await.unwrap();
        session
            .execute(
                "CREATE TABLE sela_opt_ok (
                    id INTEGER PRIMARY KEY,
                    name TEXT NOT NULL,
                    note TEXT
                )",
            )
            .await
            .unwrap();
        session
            .execute("INSERT INTO sela_opt_ok (id, name, note) VALUES (1, 'a', NULL)")
            .await
            .unwrap();

        let mut cs = ChangeSet::try_new(
            TableRef::new("main", "sela_opt_ok"),
            vec!["id".into()],
            vec!["id".into(), "name".into(), "note".into()],
        )
        .unwrap();
        cs.set_cell(
            &[Value::Int(1)],
            "name",
            Value::text("a"),
            Value::text("ok"),
        )
        .unwrap();
        cs.set_cell(&[Value::Int(1)], "note", Value::Null, Value::text("filled"))
            .unwrap();
        let plan = ChangePlan::build(&cs, Dialect::Sqlite).unwrap();
        assert!(
            plan.statements[0].sql.contains("AND \"note\" IS NULL"),
            "{}",
            plan.statements[0].sql
        );
        let result = session
            .apply_changes(&plan, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result.statements_executed, 1);
        let page = session
            .query("SELECT name, note FROM sela_opt_ok WHERE id = 1")
            .await
            .unwrap();
        assert_eq!(page.rows[0][0], Value::text("ok"));
        assert_eq!(page.rows[0][1], Value::text("filled"));
    }

    /// Optimistic WHERE with float / json / blob: UPDATE + DELETE (OW typed cols).
    #[tokio::test]
    async fn ow_sqlite_optimistic_float_json_blob_update_and_delete() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("changes_typed.sqlite");
        let session = SqliteSession::open(&path, true).await.unwrap();
        session
            .execute(
                "CREATE TABLE sela_typed (
                    id INTEGER PRIMARY KEY,
                    name TEXT NOT NULL,
                    score REAL NOT NULL,
                    meta TEXT NOT NULL,
                    payload BLOB NOT NULL
                )",
            )
            .await
            .unwrap();
        session
            .execute(
                "INSERT INTO sela_typed (id, name, score, meta, payload)
                 VALUES (1, 'a', 1.5, '{\"k\":1}', X'0102'),
                        (2, 'b', 2.5, '{\"k\":2}', X'0304')",
            )
            .await
            .unwrap();

        let mut types = BTreeMap::new();
        types.insert("id".into(), "INTEGER".into());
        types.insert("name".into(), "TEXT".into());
        types.insert("score".into(), "REAL".into());
        types.insert("meta".into(), "TEXT".into());
        types.insert("payload".into(), "BLOB".into());
        let cols = vec![
            "id".into(),
            "name".into(),
            "score".into(),
            "meta".into(),
            "payload".into(),
        ];
        let mut cs = ChangeSet::try_new_with_types(
            TableRef::new("main", "sela_typed"),
            vec!["id".into()],
            cols.clone(),
            types.clone(),
        )
        .unwrap();
        cs.set_cell(
            &[Value::Int(1)],
            "name",
            Value::text("a"),
            Value::text("a2"),
        )
        .unwrap();
        cs.set_cell(
            &[Value::Int(1)],
            "score",
            Value::Float(1.5),
            Value::Float(1.75),
        )
        .unwrap();
        cs.set_cell(
            &[Value::Int(1)],
            "meta",
            Value::json_raw(r#"{"k":1}"#),
            Value::json_raw(r#"{"k":9}"#),
        )
        .unwrap();
        cs.set_cell(
            &[Value::Int(1)],
            "payload",
            Value::Bytes(vec![1, 2]),
            Value::Bytes(vec![9, 9]),
        )
        .unwrap();
        let plan = ChangePlan::build(&cs, Dialect::Sqlite).unwrap();
        assert!(
            plan.statements[0].sql.contains("AND \"score\" =")
                && plan.statements[0].sql.contains("AND \"meta\" =")
                && plan.statements[0].sql.contains("AND \"payload\" ="),
            "{}",
            plan.statements[0].sql
        );
        session
            .apply_changes(&plan, CancellationToken::new())
            .await
            .unwrap();

        // Concurrent float edit → conflict on UPDATE.
        session
            .execute("UPDATE sela_typed SET score = 9.0 WHERE id = 1")
            .await
            .unwrap();
        let mut cs = ChangeSet::try_new_with_types(
            TableRef::new("main", "sela_typed"),
            vec!["id".into()],
            cols.clone(),
            types.clone(),
        )
        .unwrap();
        cs.set_cell(
            &[Value::Int(1)],
            "name",
            Value::text("a2"),
            Value::text("stale"),
        )
        .unwrap();
        cs.set_cell(
            &[Value::Int(1)],
            "score",
            Value::Float(1.75),
            Value::Float(3.0),
        )
        .unwrap();
        let plan = ChangePlan::build(&cs, Dialect::Sqlite).unwrap();
        let err = session
            .apply_changes(&plan, CancellationToken::new())
            .await
            .unwrap_err();
        assert!(matches!(err, DbError::ChangeConflict { .. }), "{err:?}");

        // DELETE: oversized blob skipped (listed unchecked); float still checked.
        let big = vec![0u8; sela_core::OPTIMISTIC_BLOB_MAX_BYTES + 8];
        session
            .execute("UPDATE sela_typed SET score = 2.5, name = 'b' WHERE id = 2")
            .await
            .unwrap();
        // Store a large blob on row 2 for the skip path.
        let mut cs = ChangeSet::try_new_with_types(
            TableRef::new("main", "sela_typed"),
            vec!["id".into()],
            cols,
            types,
        )
        .unwrap();
        let mut expected = BTreeMap::new();
        expected.insert("name".into(), Value::text("b"));
        expected.insert("score".into(), Value::Float(2.5));
        expected.insert("meta".into(), Value::json_raw(r#"{"k":2}"#));
        expected.insert("payload".into(), Value::Bytes(big));
        cs.delete_row_with_expected(&[Value::Int(2)], expected)
            .unwrap();
        let plan = ChangePlan::build(&cs, Dialect::Sqlite).unwrap();
        assert_eq!(
            plan.statements[0].unchecked_optimistic,
            vec!["payload".to_string()]
        );
        assert!(
            plan.statements[0].sql.contains("AND \"score\" =")
                && !plan.statements[0].sql.contains("\"payload\""),
            "{}",
            plan.statements[0].sql
        );
        session
            .apply_changes(&plan, CancellationToken::new())
            .await
            .unwrap();
        let page = session
            .query("SELECT COUNT(*) FROM sela_typed WHERE id = 2")
            .await
            .unwrap();
        assert_eq!(page.rows[0][0], Value::Int(0));
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
