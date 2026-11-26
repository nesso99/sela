//! Shared helpers for M3-3 export/import driver integration tests (#127).

use std::io::Cursor;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use sela_core::{
    bind_placeholder, count_csv_data_rows, export_query, qualify, quote_ident, ChangePlan,
    ChangeSet, ColumnMeta, CsvImportOptions, CsvImporter, DatabaseSession, Dialect, Engine,
    ImportError, ImportRow, ImportRowSink, PlanOp, PlannedStatement, TableRef, Value,
};
use sela_core::{parse_connection_url, SavedConnection};
use sela_drivers::{connect, create_sample_database};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// One configured backend for export/import driver tests.
pub struct EngineFixture {
    pub label: &'static str,
    pub dialect: Dialect,
    pub schema: String,
    pub session: Box<dyn DatabaseSession>,
    pub _sqlite_dir: Option<TempDir>,
}

impl EngineFixture {
    pub fn session(&self) -> &dyn DatabaseSession {
        self.session.as_ref()
    }
}

/// Unique table name so parallel CI runs do not collide.
pub fn unique_table_name() -> String {
    let id = Uuid::new_v4().simple().to_string();
    format!("sela_ex_{id}")
}

pub async fn sqlite_fixture() -> EngineFixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("exim.sqlite");
    create_sample_database(&db).await.expect("seed sqlite");
    let mut conn = SavedConnection::new(Engine::Sqlite);
    conn.file_path = db.to_string_lossy().into();
    let session = connect(&conn, "").await.expect("connect sqlite");
    EngineFixture {
        label: "sqlite",
        dialect: Dialect::Sqlite,
        schema: "main".into(),
        session,
        _sqlite_dir: Some(dir),
    }
}

fn connection_from_url(url: &str, engine: Engine) -> (SavedConnection, String) {
    let imported = parse_connection_url(url).expect("parse connection URL");
    assert_eq!(imported.engine, engine);
    let password = imported.password.unwrap_or_default();
    let mut connection = SavedConnection::new(engine);
    connection.host = imported.host;
    connection.port = imported.port;
    connection.username = imported.username;
    connection.database = imported.database;
    connection.file_path = imported.file_path;
    if let Some(mode) = imported.tls_mode {
        connection.tls.mode = mode;
    }
    (connection, password)
}

pub async fn try_postgres_fixture() -> Option<EngineFixture> {
    let url = std::env::var("SELA_POSTGRES_URL").ok()?;
    let schema = std::env::var("SELA_POSTGRES_SCHEMA").unwrap_or_else(|_| "public".into());
    let (connection, password) = connection_from_url(&url, Engine::Postgres);
    let session = connect(&connection, &password).await.unwrap_or_else(|err| {
        panic!("SELA_POSTGRES_URL is set but Postgres is unreachable: {err}")
    });
    Some(EngineFixture {
        label: "postgres",
        dialect: Dialect::Postgres,
        schema,
        session,
        _sqlite_dir: None,
    })
}

pub async fn try_mysql_fixture() -> Option<EngineFixture> {
    let url = std::env::var("SELA_MYSQL_URL").ok()?;
    let schema = std::env::var("SELA_MYSQL_SCHEMA").unwrap_or_else(|_| "sample".into());
    let (connection, password) = connection_from_url(&url, Engine::Mysql);
    let session = connect(&connection, &password)
        .await
        .unwrap_or_else(|err| panic!("SELA_MYSQL_URL is set but MySQL is unreachable: {err}"));
    Some(EngineFixture {
        label: "mysql",
        dialect: Dialect::Mysql,
        schema,
        session,
        _sqlite_dir: None,
    })
}

pub async fn all_engine_fixtures() -> Vec<EngineFixture> {
    let mut out = vec![sqlite_fixture().await];
    if let Some(pg) = try_postgres_fixture().await {
        out.push(pg);
    }
    if let Some(my) = try_mysql_fixture().await {
        out.push(my);
    }
    out
}

pub async fn drop_table(session: &dyn DatabaseSession, schema: &str, table: &str) {
    let qual = qualify(session.dialect(), schema, table).unwrap();
    let sql = format!("DROP TABLE IF EXISTS {qual}");
    let _ = session.execute(&sql).await;
}

pub async fn create_exim_table(session: &dyn DatabaseSession, schema: &str, table: &str) {
    let qual = qualify(session.dialect(), schema, table).unwrap();
    let ddl = match session.dialect() {
        Dialect::Postgres => format!(
            "CREATE TABLE {qual} (
                id integer PRIMARY KEY,
                title text NOT NULL,
                note text
            )"
        ),
        Dialect::Mysql => format!(
            "CREATE TABLE {qual} (
                id int PRIMARY KEY,
                title varchar(512) NOT NULL,
                note text NULL
            ) ENGINE=InnoDB"
        ),
        Dialect::Sqlite => format!(
            "CREATE TABLE {qual} (
                id integer PRIMARY KEY,
                title text NOT NULL,
                note text
            )"
        ),
    };
    session.execute(&ddl).await.expect("create exim table");
}

/// Round-trip fixture rows (NULL, quotes, unicode).
pub async fn seed_exim_sample(session: &dyn DatabaseSession, schema: &str, table: &str) {
    let qual = qualify(session.dialect(), schema, table).unwrap();
    let sql = match session.dialect() {
        Dialect::Sqlite => format!(
            "INSERT INTO {qual} (id, title, note) VALUES \
             (1, 'a,b', NULL), \
             (2, 'say \"hi\"' || char(10) || 'there', ''), \
             (3, 'café', 'NULL')"
        ),
        Dialect::Mysql => format!(
            "INSERT INTO {qual} (id, title, note) VALUES \
             (1, 'a,b', NULL), \
             (2, CONCAT('say \"hi\"', CHAR(10), 'there'), ''), \
             (3, 'café', 'NULL')"
        ),
        Dialect::Postgres => format!(
            "INSERT INTO {qual} (id, title, note) VALUES \
             (1, 'a,b', NULL), \
             (2, E'say \"hi\"\\nthere', ''), \
             (3, 'café', 'NULL')"
        ),
    };
    session.execute(&sql).await.expect("seed sample");
}

pub async fn seed_exim_many(session: &dyn DatabaseSession, schema: &str, table: &str, count: u32) {
    let qual = qualify(session.dialect(), schema, table).unwrap();
    for start in (1..=count).step_by(50) {
        let end = (start + 49).min(count);
        let mut values = String::new();
        for id in start..=end {
            if !values.is_empty() {
                values.push_str(", ");
            }
            values.push_str(&format!("({id}, 'row{id}', NULL)"));
        }
        let sql = format!("INSERT INTO {qual} (id, title, note) VALUES {values}");
        session.execute(&sql).await.expect("seed batch");
    }
}

pub async fn row_count(session: &dyn DatabaseSession, schema: &str, table: &str) -> u64 {
    let qual = qualify(session.dialect(), schema, table).unwrap();
    session
        .count_query(&format!("SELECT COUNT(*) FROM {qual}"), &[])
        .await
        .expect("count")
}

struct CollectSink<'a> {
    rows: &'a mut Vec<ImportRow>,
}

impl ImportRowSink for CollectSink<'_> {
    fn push_row(&mut self, row: ImportRow) -> Result<(), ImportError> {
        self.rows.push(row);
        Ok(())
    }
}

fn build_insert_only_plan(
    dialect: Dialect,
    schema: &str,
    table: &str,
    rows: &[ImportRow],
) -> Result<ChangePlan, ImportError> {
    let table_ref = TableRef::new(schema, table);
    let mut statements = Vec::with_capacity(rows.len());
    for row in rows {
        if row.values.is_empty() {
            continue;
        }
        let cols: Vec<_> = row.values.keys().cloned().collect();
        let params: Vec<_> = cols.iter().map(|c| row.values[c].clone()).collect();
        let mut sql = format!(
            "INSERT INTO {} (",
            qualify(dialect, schema, table).map_err(|e| ImportError::Sink(e.to_string()))?
        );
        for (i, col) in cols.iter().enumerate() {
            if i > 0 {
                sql.push_str(", ");
            }
            sql.push_str(&quote_ident(dialect, col).map_err(|e| ImportError::Sink(e.to_string()))?);
        }
        sql.push_str(") VALUES (");
        for i in 0..cols.len() {
            if i > 0 {
                sql.push_str(", ");
            }
            sql.push_str(&bind_placeholder(dialect, i));
        }
        sql.push(')');
        statements.push(PlannedStatement {
            op: PlanOp::Insert,
            sql,
            params,
            pk: Vec::new(),
            unchecked_optimistic: Vec::new(),
        });
    }
    Ok(ChangePlan {
        dialect,
        table: table_ref,
        statements,
    })
}

fn drain_importer_into_sink<R, S>(
    importer: &mut CsvImporter<R>,
    sink: &mut S,
    cancel: &CancellationToken,
    done: &Arc<AtomicU64>,
    progress: &mut dyn FnMut(u64),
) -> Result<u64, ImportError>
where
    R: std::io::Read,
    S: ImportRowSink,
{
    let mut rows = 0u64;
    while let Some(item) = importer.next_row() {
        if cancel.is_cancelled() {
            return Err(ImportError::Cancelled);
        }
        let row = item?;
        sink.push_row(row)?;
        rows += 1;
        done.store(rows, Ordering::Relaxed);
        progress(rows);
    }
    Ok(rows)
}

/// CSV bytes with `count` unique insert rows (header + ids `0..count-1`).
pub fn large_import_csv(count: usize) -> Vec<u8> {
    let mut body = String::from("id,title,note\n");
    for i in 0..count {
        body.push_str(&format!("{i},r{i},\n"));
    }
    body.into_bytes()
}

/// 149 unique ids then a duplicate `id=1` as the 150th data row (parse-clean).
pub fn duplicate_pk_import_csv() -> Vec<u8> {
    let mut body = String::from("id,title,note\n");
    for i in 1..150 {
        body.push_str(&format!("{i},r{i},\n"));
    }
    body.push_str("1,dup,\n");
    body.into_bytes()
}

#[allow(clippy::too_many_arguments)]
async fn build_import_plan_from_csv(
    session: &dyn DatabaseSession,
    dialect: Dialect,
    schema: &str,
    table: &str,
    csv: &[u8],
    options: CsvImportOptions,
    drain_cancel: &CancellationToken,
    done: &Arc<AtomicU64>,
    on_progress: &mut dyn FnMut(u64),
) -> Result<ChangePlan, ImportError> {
    let structure = session
        .describe_table(schema, table)
        .await
        .map_err(|e| ImportError::Schema(e.to_string()))?;
    let table_columns: Vec<ColumnMeta> = structure
        .columns
        .iter()
        .map(|c| ColumnMeta {
            name: c.name.clone(),
            type_name: c.type_name.clone(),
        })
        .collect();

    let reader = Cursor::new(csv);
    let mut importer = CsvImporter::open_with_columns(reader, &table_columns, options)?;

    match ChangeSet::from_structure(&structure) {
        Ok(mut cs) => {
            drain_importer_into_sink(&mut importer, &mut cs, drain_cancel, done, on_progress)?;
            ChangePlan::build(&cs, dialect).map_err(|e| ImportError::Sink(e.to_string()))
        }
        Err(_) => {
            let mut rows = Vec::new();
            let mut collector = CollectSink { rows: &mut rows };
            drain_importer_into_sink(
                &mut importer,
                &mut collector,
                drain_cancel,
                done,
                on_progress,
            )?;
            build_insert_only_plan(dialect, schema, table, &rows)
        }
    }
}

/// Driver import: stream CSV, build one [`ChangePlan`], `apply_changes` in one transaction.
#[allow(clippy::too_many_arguments)]
pub async fn driver_import_csv(
    session: &dyn DatabaseSession,
    dialect: Dialect,
    schema: &str,
    table: &str,
    csv: &[u8],
    options: CsvImportOptions,
    cancel: CancellationToken,
    mut on_progress: impl FnMut(u64),
) -> Result<u64, ImportError> {
    let done = Arc::new(AtomicU64::new(0));
    let plan = build_import_plan_from_csv(
        session,
        dialect,
        schema,
        table,
        csv,
        options,
        &cancel,
        &done,
        &mut on_progress,
    )
    .await?;

    if cancel.is_cancelled() {
        return Err(ImportError::Cancelled);
    }

    session
        .apply_changes(&plan, cancel)
        .await
        .map_err(map_apply_error)?;
    Ok(done.load(Ordering::Relaxed))
}

fn map_apply_error(e: sela_core::DbError) -> ImportError {
    if matches!(e, sela_core::DbError::Cancelled { .. }) {
        ImportError::Cancelled
    } else {
        ImportError::Sink(e.to_string())
    }
}

/// IM-04: parse the full CSV, then cancel while `apply_changes` is in flight.
pub async fn driver_import_cancel_during_apply(
    session: &dyn DatabaseSession,
    dialect: Dialect,
    schema: &str,
    table: &str,
    csv: &[u8],
    options: CsvImportOptions,
) -> Result<(), ImportError> {
    let done = Arc::new(AtomicU64::new(0));
    let parse_cancel = CancellationToken::new();
    let mut noop = |_| {};
    let plan = build_import_plan_from_csv(
        session,
        dialect,
        schema,
        table,
        csv,
        options,
        &parse_cancel,
        &done,
        &mut noop,
    )
    .await?;
    assert!(
        plan.statements.len() > 100,
        "need enough INSERTs for mid-apply cancel"
    );

    let cancel = CancellationToken::new();
    let trip = cancel.clone();
    tokio::spawn(async move {
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
        trip.cancel();
    });

    session
        .apply_changes(&plan, cancel)
        .await
        .map_err(map_apply_error)?;
    Ok(())
}

/// Export a session query into an in-memory UTF-8 buffer.
#[allow(clippy::too_many_arguments)]
pub async fn export_to_vec(
    session: &dyn DatabaseSession,
    sql: &str,
    params: &[Value],
    format: &sela_core::ExportFormat,
    cancel: CancellationToken,
) -> Result<(u64, Vec<u8>), sela_core::ExportError> {
    let mut out = Vec::new();
    let (n, _outcome) =
        export_query(session, sql, params, &mut out, format, cancel, |_| {}).await?;
    Ok((n, out))
}

pub fn csv_data_row_count(csv: &[u8], options: &CsvImportOptions) -> u64 {
    count_csv_data_rows(Cursor::new(csv), options).expect("prescan")
}
