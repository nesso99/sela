//! Execute a [`sela_core::ChangePlan`] inside one sqlx transaction.
//!
//! ## Exact-one-row guard
//!
//! Every `UPDATE` and `DELETE` must report exactly one affected row. Zero rows
//! usually means the row was deleted, its PK changed, or (with optimistic
//! WHERE) a non-PK column was changed concurrently; more than one can happen
//! when a NULL PK component uses `IS NULL` and matches several rows (e.g.
//! SQLite non-INTEGER PKs). Either case returns
//! [`sela_core::DbError::ChangeConflict`] and drops the transaction (rollback).
//! `INSERT` is not checked this way (engine uniqueness errors still abort).
//!
//! ## Cancellation
//!
//! Honours [`tokio_util::sync::CancellationToken`] like streaming queries:
//! cooperative checks before begin and between statements, `tokio::select!`
//! against an in-flight execute, and server-side cancel on Postgres
//! (`pg_cancel_backend`) / MySQL (`KILL QUERY`) from a second pool connection.
//! On cancel the transaction is dropped (rollback) and the connection is
//! closed so a racing cancel cannot poison the next checkout. SQLite has no
//! interrupt API — cancel stops between statements (same limitation as stream).
//!
//! ## MySQL `CLIENT_FOUND_ROWS`
//!
//! sqlx 0.9.1 [`sqlx::mysql::MySqlConnectOptions`] defaults `found_rows: true`,
//! so `rows_affected` is the number of *matched* rows, not only changed rows.
//! A same-value `UPDATE` therefore reports `1` and passes this guard. We also
//! call `.found_rows(true)` explicitly when opening MySQL pools so URL-parsed
//! options keep that behaviour (see `mysql.rs`).

use std::borrow::Cow;
use std::time::Duration;

use sela_core::{
    ApplyChangesResult, ChangePlan, DbError, Dialect, PlanStreamItem, PlannedStatement, TableRef,
};
use sqlx::Connection;
use tokio_util::sync::CancellationToken;

/// Where statements come from: a fully built plan, or a stream (IM-07 import)
/// that commits only after [`PlanStreamItem::End`].
pub(crate) enum StatementSource<'a> {
    Plan {
        plan: &'a ChangePlan,
        next: usize,
    },
    Stream {
        dialect: Dialect,
        table: &'a TableRef,
        rx: async_channel::Receiver<PlanStreamItem>,
        executed: usize,
    },
}

impl<'a> StatementSource<'a> {
    pub(crate) fn plan(plan: &'a ChangePlan) -> Self {
        Self::Plan { plan, next: 0 }
    }

    pub(crate) fn stream(
        dialect: Dialect,
        table: &'a TableRef,
        rx: async_channel::Receiver<PlanStreamItem>,
    ) -> Self {
        Self::Stream {
            dialect,
            table,
            rx,
            executed: 0,
        }
    }

    fn dialect(&self) -> Dialect {
        match self {
            Self::Plan { plan, .. } => plan.dialect,
            Self::Stream { dialect, .. } => *dialect,
        }
    }

    fn table(&self) -> &TableRef {
        match self {
            Self::Plan { plan, .. } => &plan.table,
            Self::Stream { table, .. } => table,
        }
    }

    fn is_empty_plan(&self) -> bool {
        matches!(self, Self::Plan { plan, .. } if plan.statements.is_empty())
    }

    fn executed(&self) -> usize {
        match self {
            Self::Plan { next, .. } => *next,
            Self::Stream { executed, .. } => *executed,
        }
    }

    /// Next statement, `None` at a clean end. Stream closed early is an error.
    async fn next(
        &mut self,
        cancel: &CancellationToken,
    ) -> Result<Option<Cow<'a, PlannedStatement>>, DbError> {
        match self {
            Self::Plan { plan, next } => {
                let stmt = plan.statements.get(*next).map(Cow::Borrowed);
                if stmt.is_some() {
                    *next += 1;
                }
                Ok(stmt)
            }
            Self::Stream { rx, executed, .. } => {
                let item = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {
                        return Err(DbError::cancelled("apply cancelled"));
                    }
                    item = rx.recv() => item,
                };
                match item {
                    Ok(PlanStreamItem::Statement(stmt)) => {
                        *executed += 1;
                        Ok(Some(Cow::Owned(stmt)))
                    }
                    Ok(PlanStreamItem::End) => Ok(None),
                    Err(_) if cancel.is_cancelled() => Err(DbError::cancelled("apply cancelled")),
                    Err(_) => Err(DbError::query("statement stream ended before it finished")),
                }
            }
        }
    }
}

macro_rules! run_statements_in_tx {
    ($tx:expr, $source:expr, $cancel:expr) => {{
        let source: &mut $crate::change_exec::StatementSource<'_> = $source;
        let cancel: &::tokio_util::sync::CancellationToken = $cancel;
        let mut rows_affected = 0u64;
        loop {
            if cancel.is_cancelled() {
                return Err(::sela_core::DbError::cancelled("apply cancelled"));
            }
            let Some(stmt) = source.next(cancel).await? else {
                break;
            };
            let statement_index = source.executed() - 1;
            let exec =
                $crate::bind_params($crate::query(&stmt.sql), &stmt.params).execute(&mut *$tx);
            let result = ::tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    return Err(::sela_core::DbError::cancelled("apply cancelled"));
                }
                result = exec => result.map_err($crate::sqlx_err)?,
            };
            let n = result.rows_affected();
            match stmt.op {
                ::sela_core::PlanOp::Update | ::sela_core::PlanOp::Delete => {
                    if n != 1 {
                        // Dropping `tx` without commit rolls the whole plan back.
                        return Err(::sela_core::DbError::change_conflict(
                            source.table(),
                            statement_index,
                            n,
                            &stmt.pk,
                        ));
                    }
                }
                ::sela_core::PlanOp::Insert => {}
            }
            rows_affected = rows_affected.saturating_add(n);
        }
        if cancel.is_cancelled() {
            return Err(::sela_core::DbError::cancelled("apply cancelled"));
        }
        $tx.commit().await.map_err($crate::sqlx_err)?;
        Ok(::sela_core::ApplyChangesResult {
            statements_executed: source.executed(),
            rows_affected,
        })
    }};
}

fn dialect_mismatch(source: &StatementSource<'_>, expected: Dialect) -> Result<(), DbError> {
    if source.dialect() != expected {
        return Err(DbError::query(format!(
            "change plan dialect {:?} does not match session {:?}",
            source.dialect(),
            expected
        )));
    }
    Ok(())
}

fn empty_ok() -> ApplyChangesResult {
    ApplyChangesResult {
        statements_executed: 0,
        rows_affected: 0,
    }
}

pub(crate) async fn apply_postgres(
    pool: &sqlx::PgPool,
    mut source: StatementSource<'_>,
    cancel: CancellationToken,
) -> Result<ApplyChangesResult, DbError> {
    dialect_mismatch(&source, Dialect::Postgres)?;
    if cancel.is_cancelled() {
        return Err(DbError::cancelled("apply cancelled"));
    }
    if source.is_empty_plan() {
        return Ok(empty_ok());
    }

    let mut conn = pool.acquire().await.map_err(crate::sqlx_err)?;
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *conn)
        .await
        .map_err(crate::sqlx_err)?;

    // Cancel helper shares the session pool (max_connections = 5). When every
    // connection is checked out, this acquire can wait/fail and server-side
    // cancel is skipped; cooperative checks + dropping the tx still roll back.
    let cancel_watch = cancel.clone();
    let pool_cancel = pool.clone();
    let cancel_task = tokio::spawn(async move {
        cancel_watch.cancelled().await;
        let _ = sqlx::query("SELECT pg_cancel_backend($1)")
            .bind(pid)
            .execute(&pool_cancel)
            .await;
    });

    let result = async {
        let mut tx = conn.begin().await.map_err(crate::sqlx_err)?;
        run_statements_in_tx!(tx, &mut source, &cancel)
    }
    .await;

    let cancelled = cancel.is_cancelled() || matches!(&result, Err(DbError::Cancelled { .. }));
    if cancelled {
        let _ = tokio::time::timeout(Duration::from_secs(1), cancel_task).await;
        let _ = conn.close().await;
    } else {
        cancel_task.abort();
    }
    result
}

pub(crate) async fn apply_mysql(
    pool: &sqlx::MySqlPool,
    mut source: StatementSource<'_>,
    cancel: CancellationToken,
) -> Result<ApplyChangesResult, DbError> {
    dialect_mismatch(&source, Dialect::Mysql)?;
    if cancel.is_cancelled() {
        return Err(DbError::cancelled("apply cancelled"));
    }
    if source.is_empty_plan() {
        return Ok(empty_ok());
    }

    let mut conn = pool.acquire().await.map_err(crate::sqlx_err)?;
    let connection_id: u64 = sqlx::query_scalar("SELECT CONNECTION_ID()")
        .fetch_one(&mut *conn)
        .await
        .map_err(crate::sqlx_err)?;

    // Cancel helper shares the session pool (max_connections = 5). When every
    // connection is checked out, this acquire can wait/fail and server-side
    // cancel is skipped; cooperative checks + dropping the tx still roll back.
    let cancel_watch = cancel.clone();
    let pool_cancel = pool.clone();
    let cancel_task = tokio::spawn(async move {
        cancel_watch.cancelled().await;
        let kill = format!("KILL QUERY {connection_id}");
        let _ = crate::query(&kill).execute(&pool_cancel).await;
    });

    let result = async {
        let mut tx = conn.begin().await.map_err(crate::sqlx_err)?;
        run_statements_in_tx!(tx, &mut source, &cancel)
    }
    .await;

    let cancelled = cancel.is_cancelled() || matches!(&result, Err(DbError::Cancelled { .. }));
    if cancelled {
        let _ = tokio::time::timeout(Duration::from_secs(1), cancel_task).await;
        let _ = conn.close().await;
    } else {
        cancel_task.abort();
    }
    result
}

pub(crate) async fn apply_sqlite(
    pool: &sqlx::SqlitePool,
    mut source: StatementSource<'_>,
    cancel: CancellationToken,
) -> Result<ApplyChangesResult, DbError> {
    dialect_mismatch(&source, Dialect::Sqlite)?;
    if cancel.is_cancelled() {
        return Err(DbError::cancelled("apply cancelled"));
    }
    if source.is_empty_plan() {
        return Ok(empty_ok());
    }

    // No server-side interrupt (same as stream). Still check the token between
    // statements and select against in-flight executes.
    let mut tx = pool.begin().await.map_err(crate::sqlx_err)?;
    run_statements_in_tx!(tx, &mut source, &cancel)
}
