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

use std::time::Duration;

use sela_core::{ApplyChangesResult, ChangePlan, DbError, Dialect};
use sqlx::Connection;
use tokio_util::sync::CancellationToken;

macro_rules! run_statements_in_tx {
    ($tx:expr, $plan:expr, $cancel:expr) => {{
        let plan: &::sela_core::ChangePlan = $plan;
        let cancel: &::tokio_util::sync::CancellationToken = $cancel;
        let mut rows_affected = 0u64;
        for (statement_index, stmt) in plan.statements.iter().enumerate() {
            if cancel.is_cancelled() {
                return Err(::sela_core::DbError::cancelled("apply cancelled"));
            }
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
                            &plan.table,
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
            statements_executed: plan.statements.len(),
            rows_affected,
        })
    }};
}

fn dialect_mismatch(plan: &ChangePlan, expected: Dialect) -> Result<(), DbError> {
    if plan.dialect != expected {
        return Err(DbError::query(format!(
            "change plan dialect {:?} does not match session {:?}",
            plan.dialect, expected
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
    plan: &ChangePlan,
    cancel: CancellationToken,
) -> Result<ApplyChangesResult, DbError> {
    dialect_mismatch(plan, Dialect::Postgres)?;
    if cancel.is_cancelled() {
        return Err(DbError::cancelled("apply cancelled"));
    }
    if plan.statements.is_empty() {
        return Ok(empty_ok());
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

    let result = async {
        let mut tx = conn.begin().await.map_err(crate::sqlx_err)?;
        run_statements_in_tx!(tx, plan, &cancel)
    }
    .await;

    let cancelled = cancel.is_cancelled() || matches!(&result, Err(DbError::Cancelled { .. }));
    if cancelled || cancel.is_cancelled() {
        let _ = tokio::time::timeout(Duration::from_secs(1), cancel_task).await;
        let _ = conn.close().await;
    } else {
        cancel_task.abort();
        if cancel.is_cancelled() {
            let _ = conn.close().await;
        }
    }
    result
}

pub(crate) async fn apply_mysql(
    pool: &sqlx::MySqlPool,
    plan: &ChangePlan,
    cancel: CancellationToken,
) -> Result<ApplyChangesResult, DbError> {
    dialect_mismatch(plan, Dialect::Mysql)?;
    if cancel.is_cancelled() {
        return Err(DbError::cancelled("apply cancelled"));
    }
    if plan.statements.is_empty() {
        return Ok(empty_ok());
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
        let _ = crate::query(&kill).execute(&pool_cancel).await;
    });

    let result = async {
        let mut tx = conn.begin().await.map_err(crate::sqlx_err)?;
        run_statements_in_tx!(tx, plan, &cancel)
    }
    .await;

    let cancelled = cancel.is_cancelled() || matches!(&result, Err(DbError::Cancelled { .. }));
    if cancelled || cancel.is_cancelled() {
        let _ = tokio::time::timeout(Duration::from_secs(1), cancel_task).await;
        let _ = conn.close().await;
    } else {
        cancel_task.abort();
        if cancel.is_cancelled() {
            let _ = conn.close().await;
        }
    }
    result
}

pub(crate) async fn apply_sqlite(
    pool: &sqlx::SqlitePool,
    plan: &ChangePlan,
    cancel: CancellationToken,
) -> Result<ApplyChangesResult, DbError> {
    dialect_mismatch(plan, Dialect::Sqlite)?;
    if cancel.is_cancelled() {
        return Err(DbError::cancelled("apply cancelled"));
    }
    if plan.statements.is_empty() {
        return Ok(empty_ok());
    }

    // No server-side interrupt (same as stream). Still check the token between
    // statements and select against in-flight executes.
    let mut tx = pool.begin().await.map_err(crate::sqlx_err)?;
    run_statements_in_tx!(tx, plan, &cancel)
}
