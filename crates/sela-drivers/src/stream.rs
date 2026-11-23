//! Batched `sqlx` streaming with cooperative + server-side cancel.
//!
//! Columns come from the **first row** (or from `describe` only when the result is
//! empty). Calling `describe` on the same connection *before* `fetch` makes
//! Postgres buffer the whole result before the first streamed row — which breaks
//! “first batch before the query finishes”.
//!
//! ## SQLite cancel limitation
//!
//! `sqlx`'s SQLite driver exposes no interrupt / progress-handler API. On cancel we
//! stop consuming the `fetch` stream and drop it so the client stops reading rows,
//! but a long-running SQLite statement may keep running inside the engine until it
//! produces the next row or finishes. We intentionally do **not** switch to
//! `rusqlite` in this milestone.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use sqlx::{Executor, Row as _};
use tokio_util::sync::CancellationToken;

use sela_core::{ColumnMeta, DbError, StreamBatch, StreamOutcome, Value};

use crate::decode::{column_meta, read_mysql_cell, read_sqlite_cell};
use crate::pg_decode::{
    read_postgres_cell_with_strategy, strategies_from_columns, PgDecodeStrategy,
};

pub(crate) async fn stream_postgres(
    pool: &sqlx::PgPool,
    sql: &str,
    batch_size: usize,
    row_cap: Option<Arc<AtomicUsize>>,
    finish_at_cap: bool,
    cancel: CancellationToken,
    sink: async_channel::Sender<StreamBatch>,
) -> Result<StreamOutcome, DbError> {
    let started = Instant::now();
    if cancel.is_cancelled() {
        return Err(DbError::cancelled("query cancelled"));
    }

    let mut conn = pool.acquire().await.map_err(crate::sqlx_err)?;
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *conn)
        .await
        .map_err(crate::sqlx_err)?;

    let cancel_watch = cancel.clone();
    let pool_cancel = pool.clone();
    let cancel_task = tokio::spawn(async move {
        cancel_watch.cancelled().await;
        let _ = sqlx::query("SELECT pg_cancel_backend($1)")
            .bind(pid)
            .execute(&pool_cancel)
            .await;
    });

    let sink_meta = sink.clone();
    let mut columns = Vec::new();
    // Cache one decode strategy per column for the lifetime of this result.
    let mut strategies: Option<Vec<PgDecodeStrategy>> = None;
    let result = {
        let mut stream = sqlx::query(sql).fetch(&mut *conn);
        drain_stream::<sqlx::Postgres, _, _>(
            &mut stream,
            &mut columns,
            batch_size,
            row_cap,
            finish_at_cap,
            cancel.clone(),
            sink,
            started,
            |row, index| {
                let plan = strategies.get_or_insert_with(|| strategies_from_columns(row.columns()));
                let strategy = plan
                    .get(index)
                    .copied()
                    .unwrap_or(PgDecodeStrategy::Fallback);
                read_postgres_cell_with_strategy(row, index, strategy)
            },
        )
        .await
    };
    let cancelled = cancel.is_cancelled() || matches!(&result, Err(DbError::Cancelled { .. }));
    let result = if cancelled {
        result
    } else {
        match result {
            Ok(mut outcome) if outcome.rows_delivered == 0 && outcome.columns.is_empty() => {
                let described = conn.describe(sql).await.map_err(crate::sqlx_err)?;
                outcome.columns = described.columns().iter().map(column_meta).collect();
                let _ = sink_meta
                    .send(StreamBatch {
                        columns: Some(outcome.columns.clone()),
                        rows: Vec::new(),
                        start_index: 0,
                        paused_at_cap: false,
                    })
                    .await;
                Ok(outcome)
            }
            other => other,
        }
    };

    // If cancel fired, an in-flight pg_cancel_backend may still be racing. Closing
    // this connection prevents that cancel from hitting the next pooled checkout.
    if cancelled || cancel.is_cancelled() {
        let _ = tokio::time::timeout(Duration::from_secs(1), cancel_task).await;
        let _ = conn.close().await;
    } else {
        cancel_task.abort();
        // Cancel may have raced in after the check above; never return a poisoned conn.
        if cancel.is_cancelled() {
            let _ = conn.close().await;
        } else {
            drop(conn);
        }
    }
    result
}

pub(crate) async fn stream_mysql(
    pool: &sqlx::MySqlPool,
    sql: &str,
    batch_size: usize,
    row_cap: Option<Arc<AtomicUsize>>,
    finish_at_cap: bool,
    cancel: CancellationToken,
    sink: async_channel::Sender<StreamBatch>,
) -> Result<StreamOutcome, DbError> {
    let started = Instant::now();
    if cancel.is_cancelled() {
        return Err(DbError::cancelled("query cancelled"));
    }

    let mut conn = pool.acquire().await.map_err(crate::sqlx_err)?;
    let connection_id: u64 = sqlx::query_scalar("SELECT CONNECTION_ID()")
        .fetch_one(&mut *conn)
        .await
        .map_err(crate::sqlx_err)?;

    let cancel_watch = cancel.clone();
    let pool_cancel = pool.clone();
    let cancel_task = tokio::spawn(async move {
        cancel_watch.cancelled().await;
        let kill = format!("KILL QUERY {connection_id}");
        let _ = sqlx::query(&kill).execute(&pool_cancel).await;
    });

    let sink_meta = sink.clone();
    let mut columns = Vec::new();
    let result = {
        let mut stream = sqlx::query(sql).fetch(&mut *conn);
        drain_stream::<sqlx::MySql, _, _>(
            &mut stream,
            &mut columns,
            batch_size,
            row_cap,
            finish_at_cap,
            cancel.clone(),
            sink,
            started,
            read_mysql_cell,
        )
        .await
    };
    let cancelled = cancel.is_cancelled() || matches!(&result, Err(DbError::Cancelled { .. }));
    let result = if cancelled {
        result
    } else {
        match result {
            Ok(mut outcome) if outcome.rows_delivered == 0 && outcome.columns.is_empty() => {
                let described = conn.describe(sql).await.map_err(crate::sqlx_err)?;
                outcome.columns = described.columns().iter().map(column_meta).collect();
                let _ = sink_meta
                    .send(StreamBatch {
                        columns: Some(outcome.columns.clone()),
                        rows: Vec::new(),
                        start_index: 0,
                        paused_at_cap: false,
                    })
                    .await;
                Ok(outcome)
            }
            other => other,
        }
    };

    if cancelled || cancel.is_cancelled() {
        let _ = tokio::time::timeout(Duration::from_secs(1), cancel_task).await;
        let _ = conn.close().await;
    } else {
        cancel_task.abort();
        if cancel.is_cancelled() {
            let _ = conn.close().await;
        } else {
            drop(conn);
        }
    }
    result
}

pub(crate) async fn stream_sqlite(
    pool: &sqlx::SqlitePool,
    sql: &str,
    batch_size: usize,
    row_cap: Option<Arc<AtomicUsize>>,
    finish_at_cap: bool,
    cancel: CancellationToken,
    sink: async_channel::Sender<StreamBatch>,
) -> Result<StreamOutcome, DbError> {
    let started = Instant::now();
    if cancel.is_cancelled() {
        return Err(DbError::cancelled("query cancelled"));
    }

    let mut conn = pool.acquire().await.map_err(crate::sqlx_err)?;
    let sink_meta = sink.clone();
    let mut columns = Vec::new();
    let result = {
        let mut stream = sqlx::query(sql).fetch(&mut *conn);
        drain_stream::<sqlx::Sqlite, _, _>(
            &mut stream,
            &mut columns,
            batch_size,
            row_cap,
            finish_at_cap,
            cancel,
            sink,
            started,
            read_sqlite_cell,
        )
        .await
    };
    match result {
        Ok(mut outcome) if outcome.rows_delivered == 0 && outcome.columns.is_empty() => {
            let described = conn.describe(sql).await.map_err(crate::sqlx_err)?;
            outcome.columns = described.columns().iter().map(column_meta).collect();
            let _ = sink_meta
                .send(StreamBatch {
                    columns: Some(outcome.columns.clone()),
                    rows: Vec::new(),
                    start_index: 0,
                    paused_at_cap: false,
                })
                .await;
            Ok(outcome)
        }
        other => other,
    }
}

#[allow(clippy::too_many_arguments)]
async fn drain_stream<DB, S, F>(
    stream: &mut S,
    columns: &mut Vec<ColumnMeta>,
    batch_size: usize,
    row_cap: Option<Arc<AtomicUsize>>,
    finish_at_cap: bool,
    cancel: CancellationToken,
    sink: async_channel::Sender<StreamBatch>,
    started: Instant,
    mut read_cell: F,
) -> Result<StreamOutcome, DbError>
where
    DB: sqlx::Database,
    S: StreamExt<Item = Result<DB::Row, sqlx::Error>> + Unpin,
    F: FnMut(&DB::Row, usize) -> Value,
    DB::Row: sqlx::Row,
{
    use sqlx::Row as _;

    let batch_size = batch_size.max(1);
    let mut batch_rows: Vec<Vec<Value>> = Vec::with_capacity(batch_size);
    let mut start_index = 0usize;
    let mut rows_delivered = 0usize;
    let mut sent_columns = false;
    let mut truncated = false;

    loop {
        if cancel.is_cancelled() {
            let _ = flush_batch(
                &sink,
                columns,
                &mut batch_rows,
                &mut start_index,
                &mut sent_columns,
                false,
            )
            .await;
            return Err(DbError::cancelled(format!(
                "query cancelled after {rows_delivered} rows"
            )));
        }

        if let Some(cap) = row_cap.as_ref() {
            let limit = cap.load(Ordering::SeqCst);
            if rows_delivered >= limit {
                // Peek whether more rows exist.
                let peek = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {
                        return Err(DbError::cancelled(format!(
                            "query cancelled after {rows_delivered} rows"
                        )));
                    }
                    next = stream.next() => next,
                };
                match peek {
                    Some(Ok(row)) => {
                        truncated = true;
                        // Put the peeked row back into the next batch by decoding now
                        // and holding it until the cap rises (or finish).
                        if columns.is_empty() {
                            *columns = row.columns().iter().map(column_meta).collect();
                        }
                        let width = columns.len();
                        let mut cells = Vec::with_capacity(width);
                        for index in 0..width {
                            cells.push(read_cell(&row, index));
                        }
                        if finish_at_cap {
                            // Do not deliver the peeked row; result is truncated at cap.
                            break;
                        }
                        let _ = flush_batch(
                            &sink,
                            columns,
                            &mut batch_rows,
                            &mut start_index,
                            &mut sent_columns,
                            false,
                        )
                        .await;
                        let _ = sink
                            .send(StreamBatch {
                                columns: None,
                                rows: Vec::new(),
                                start_index,
                                paused_at_cap: true,
                            })
                            .await;
                        // Wait until Load more raises the cap.
                        let held = cells;
                        loop {
                            if cancel.is_cancelled() {
                                return Err(DbError::cancelled(format!(
                                    "query cancelled after {rows_delivered} rows"
                                )));
                            }
                            if cap.load(Ordering::SeqCst) > rows_delivered {
                                batch_rows.push(held);
                                rows_delivered += 1;
                                break;
                            }
                            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                        }
                        continue;
                    }
                    Some(Err(err)) => {
                        let mapped = crate::sqlx_err(err);
                        if matches!(mapped, DbError::Cancelled { .. }) || cancel.is_cancelled() {
                            return Err(DbError::cancelled(format!(
                                "query cancelled after {rows_delivered} rows"
                            )));
                        }
                        return Err(mapped);
                    }
                    None => {
                        truncated = false;
                        break;
                    }
                }
            }
        }

        let next = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                let _ = flush_batch(
                    &sink,
                    columns,
                    &mut batch_rows,
                    &mut start_index,
                    &mut sent_columns,
                    false,
                )
                .await;
                return Err(DbError::cancelled(format!(
                    "query cancelled after {rows_delivered} rows"
                )));
            }
            row = stream.next() => row,
        };

        let Some(row_result) = next else {
            break;
        };
        let row = match row_result {
            Ok(row) => row,
            Err(err) => {
                let mapped = crate::sqlx_err(err);
                if matches!(mapped, DbError::Cancelled { .. }) || cancel.is_cancelled() {
                    let _ = flush_batch(
                        &sink,
                        columns,
                        &mut batch_rows,
                        &mut start_index,
                        &mut sent_columns,
                        false,
                    )
                    .await;
                    return Err(DbError::cancelled(format!(
                        "query cancelled after {rows_delivered} rows"
                    )));
                }
                return Err(mapped);
            }
        };

        if columns.is_empty() {
            *columns = row.columns().iter().map(column_meta).collect();
        }
        let width = columns.len();
        let mut cells = Vec::with_capacity(width);
        for index in 0..width {
            cells.push(read_cell(&row, index));
        }
        batch_rows.push(cells);
        rows_delivered += 1;

        let at_cap = row_cap
            .as_ref()
            .is_some_and(|cap| rows_delivered >= cap.load(Ordering::SeqCst));
        if (batch_rows.len() >= batch_size || at_cap)
            && !flush_batch(
                &sink,
                columns,
                &mut batch_rows,
                &mut start_index,
                &mut sent_columns,
                false,
            )
            .await
        {
            return Err(DbError::cancelled(format!(
                "query cancelled after {rows_delivered} rows"
            )));
        }
    }

    let _ = flush_batch(
        &sink,
        columns,
        &mut batch_rows,
        &mut start_index,
        &mut sent_columns,
        false,
    )
    .await;

    // Zero-row results leave `columns` empty; the caller describes after drop(stream).
    if !sent_columns && !columns.is_empty() {
        let _ = sink
            .send(StreamBatch {
                columns: Some(columns.clone()),
                rows: Vec::new(),
                start_index: 0,
                paused_at_cap: false,
            })
            .await;
    }

    if cancel.is_cancelled() {
        return Err(DbError::cancelled(format!(
            "query cancelled after {rows_delivered} rows"
        )));
    }

    Ok(StreamOutcome {
        columns: columns.clone(),
        rows_delivered,
        truncated,
        cancelled: false,
        duration: started.elapsed(),
    })
}

async fn flush_batch(
    sink: &async_channel::Sender<StreamBatch>,
    columns: &[ColumnMeta],
    batch_rows: &mut Vec<Vec<Value>>,
    start_index: &mut usize,
    sent_columns: &mut bool,
    paused_at_cap: bool,
) -> bool {
    if batch_rows.is_empty() && *sent_columns && !paused_at_cap {
        return true;
    }
    let rows = std::mem::take(batch_rows);
    let columns = if !*sent_columns {
        *sent_columns = true;
        Some(columns.to_vec())
    } else {
        None
    };
    let batch = StreamBatch {
        columns,
        rows,
        start_index: *start_index,
        paused_at_cap,
    };
    let count = batch.rows.len();
    if sink.send(batch).await.is_err() {
        return false;
    }
    *start_index += count;
    true
}
