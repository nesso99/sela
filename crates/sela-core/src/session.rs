use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::error::DbError;
use crate::sql::{self, StatementKind};
use crate::structure::TableStructure;
use crate::types::{
    Dialect, ExecOutcome, QueryPage, RowCountKind, RowSet, SchemaInfo, StatementOutcome,
    StreamBatch, StreamOutcome, TableInfo, DEFAULT_SQL_ROW_CAP, PAGE_SIZE, STREAM_BATCH_SIZE,
};

#[async_trait]
pub trait DatabaseSession: Send + Sync {
    fn dialect(&self) -> Dialect;

    async fn list_schemas(&self) -> Result<Vec<SchemaInfo>, DbError>;

    async fn list_tables(&self, schema: &str) -> Result<Vec<TableInfo>, DbError>;

    async fn query(&self, sql: &str) -> Result<QueryPage, DbError>;

    async fn execute(&self, sql: &str) -> Result<u64, DbError>;

    async fn count_query(&self, sql: &str) -> Result<u64, DbError>;

    /// Cheap row-count estimate for a table (Postgres `reltuples`, MySQL `table_rows`).
    /// SQLite returns `Ok(None)` — use [`exact_table_rows`] only on demand.
    async fn estimate_table_rows(&self, schema: &str, table: &str) -> Result<Option<u64>, DbError>;

    /// Columns, indexes, foreign keys, and DDL for the Structure view.
    async fn describe_table(&self, schema: &str, table: &str) -> Result<TableStructure, DbError>;

    /// Full DDL for a table (Postgres synthesised; MySQL `SHOW CREATE TABLE`; SQLite `sqlite_master`).
    async fn table_ddl(&self, schema: &str, table: &str) -> Result<String, DbError>;

    /// Stream `sql` in batches into `sink`. Honours `cancel` (and server-side cancel when
    /// the driver supports it).
    ///
    /// When `row_cap` is set, the stream pauses after that many rows (emits a
    /// `paused_at_cap` batch) until the cap is raised (Load more) or cancel fires.
    /// Pass `finish_at_cap = true` to stop instead of pausing (test / one-shot helpers).
    async fn stream_query(
        &self,
        sql: &str,
        batch_size: usize,
        row_cap: Option<Arc<AtomicUsize>>,
        finish_at_cap: bool,
        cancel: CancellationToken,
        sink: async_channel::Sender<StreamBatch>,
    ) -> Result<StreamOutcome, DbError>;
}

/// Run one user statement. Queries are streamed up to `row_cap` (default
/// [`DEFAULT_SQL_ROW_CAP`]) without wrapping in `LIMIT`/`OFFSET`, so `ORDER BY` /
/// `FOR UPDATE` semantics are preserved. Non-queries are executed normally.
pub async fn run_user_sql(
    session: &dyn DatabaseSession,
    sql: &str,
    row_cap: u32,
    cancel: CancellationToken,
) -> Result<StatementOutcome, DbError> {
    let started = Instant::now();
    let sql = sql::normalize_sql(sql).map_err(|err| DbError::query(err.to_string()))?;
    match sql::classify(&sql) {
        StatementKind::Execute => {
            if cancel.is_cancelled() {
                return Err(DbError::cancelled("query cancelled"));
            }
            let rows_affected = session.execute(&sql).await?;
            Ok(StatementOutcome::Executed(ExecOutcome {
                rows_affected,
                duration: started.elapsed(),
            }))
        }
        StatementKind::PageableQuery | StatementKind::RawQuery => {
            let cap = row_cap.max(1) as usize;
            let cap_live = Arc::new(AtomicUsize::new(cap));
            let (tx, rx) = async_channel::bounded(4);
            let stream = session.stream_query(
                &sql,
                STREAM_BATCH_SIZE,
                Some(cap_live),
                true, // finish at cap for the synchronous helper
                cancel,
                tx,
            );
            let collect = async {
                let mut columns = Vec::new();
                let mut rows = Vec::new();
                while let Ok(batch) = rx.recv().await {
                    if let Some(cols) = batch.columns {
                        columns = cols;
                    }
                    rows.extend(batch.rows);
                }
                (columns, rows)
            };
            let (outcome, (columns, rows)) = tokio::join!(stream, collect);
            let outcome = outcome?;
            let truncated = outcome.truncated;
            let total = rows.len() as u64;
            let (row_count_kind, note) = if truncated {
                (
                    RowCountKind::AtLeast,
                    Some(format!(
                        "Showing the first {cap} rows. Use Load more to continue."
                    )),
                )
            } else {
                (RowCountKind::Exact, None)
            };
            Ok(StatementOutcome::Rows(RowSet {
                page: QueryPage {
                    columns: if columns.is_empty() {
                        outcome.columns
                    } else {
                        columns
                    },
                    rows,
                    duration: started.elapsed(),
                },
                total_rows: Some(total),
                row_count_kind,
                duration: started.elapsed(),
                paged: false,
                truncated,
                note,
            }))
        }
    }
}

/// Page through `SELECT *` for one table using `LIMIT`/`OFFSET` and an estimate
/// instead of `COUNT(*)`. Pass [`PAGE_SIZE`] at the call site for `limit`.
pub async fn preview_table(
    session: &dyn DatabaseSession,
    schema: &str,
    table: &str,
    limit: u32,
    offset: u64,
    cancel: CancellationToken,
) -> Result<StatementOutcome, DbError> {
    let started = Instant::now();
    if cancel.is_cancelled() {
        return Err(DbError::cancelled("query cancelled"));
    }
    let sql = sql::select_all(session.dialect(), schema, table)
        .map_err(|err| DbError::query(err.to_string()))?;
    let page_sql = sql::wrap_page(session.dialect(), &sql, limit, offset);
    let (tx, rx) = async_channel::bounded(4);
    let stream = session.stream_query(
        &page_sql,
        STREAM_BATCH_SIZE,
        Some(Arc::new(AtomicUsize::new(limit as usize))),
        true,
        cancel,
        tx,
    );
    let collect = async {
        let mut columns = Vec::new();
        let mut rows = Vec::new();
        while let Ok(batch) = rx.recv().await {
            if let Some(cols) = batch.columns {
                columns = cols;
            }
            rows.extend(batch.rows);
        }
        (columns, rows)
    };
    let (outcome, (columns, rows)) = tokio::join!(stream, collect);
    let outcome = outcome?;
    let estimate = session
        .estimate_table_rows(schema, table)
        .await
        .ok()
        .flatten();
    let (row_count_kind, total_rows) = match estimate {
        Some(n) => (RowCountKind::Estimated, Some(n)),
        None => (RowCountKind::Unknown, None),
    };
    Ok(StatementOutcome::Rows(RowSet {
        page: QueryPage {
            columns: if columns.is_empty() {
                outcome.columns
            } else {
                columns
            },
            rows,
            duration: started.elapsed(),
        },
        total_rows,
        row_count_kind,
        duration: started.elapsed(),
        paged: true,
        truncated: false,
        note: None,
    }))
}

/// Exact `COUNT(*)` for a table (on-demand; not used on every page turn).
pub async fn exact_table_rows(
    session: &dyn DatabaseSession,
    schema: &str,
    table: &str,
) -> Result<u64, DbError> {
    let sql = sql::select_all(session.dialect(), schema, table)
        .map_err(|err| DbError::query(err.to_string()))?;
    let count_sql = sql::wrap_count(session.dialect(), &sql);
    session.count_query(&count_sql).await
}

/// Default row cap for the SQL editor.
pub fn default_sql_row_cap() -> u32 {
    DEFAULT_SQL_ROW_CAP
}

/// Default page size for table preview.
pub fn default_page_size() -> u32 {
    PAGE_SIZE
}

#[derive(Debug, Clone)]
pub struct LoadedSchema {
    pub name: String,
    pub tables: Vec<TableInfo>,
}
