//! Execute a [`sela_core::ChangePlan`] inside one sqlx transaction.
//!
//! ## Exact-one-row guard
//!
//! Every `UPDATE` and `DELETE` must report exactly one affected row. Zero rows
//! usually means the row was deleted or its PK changed concurrently; more than
//! one can happen when a NULL PK component uses `IS NULL` and matches several
//! rows (e.g. SQLite non-INTEGER PKs). Either case returns
//! [`sela_core::DbError::ChangeConflict`] and drops the transaction (rollback).
//! `INSERT` is not checked this way (engine uniqueness errors still abort).
//!
//! ## MySQL `CLIENT_FOUND_ROWS`
//!
//! sqlx 0.9.1 [`sqlx::mysql::MySqlConnectOptions`] defaults `found_rows: true`,
//! so `rows_affected` is the number of *matched* rows, not only changed rows.
//! A same-value `UPDATE` therefore reports `1` and passes this guard. We also
//! call `.found_rows(true)` explicitly when opening MySQL pools so URL-parsed
//! options keep that behaviour (see `mysql.rs`).

macro_rules! apply_change_plan {
    ($pool:expr, $plan:expr, $expected:expr) => {{
        let plan: &::sela_core::ChangePlan = $plan;
        let expected: ::sela_core::Dialect = $expected;
        if plan.dialect != expected {
            return Err(::sela_core::DbError::query(format!(
                "change plan dialect {:?} does not match session {:?}",
                plan.dialect, expected
            )));
        }
        if plan.statements.is_empty() {
            return Ok(::sela_core::ApplyChangesResult {
                statements_executed: 0,
                rows_affected: 0,
            });
        }
        let mut tx = $pool.begin().await.map_err($crate::sqlx_err)?;
        let mut rows_affected = 0u64;
        for (statement_index, stmt) in plan.statements.iter().enumerate() {
            let result = $crate::bind_params($crate::query(&stmt.sql), &stmt.params)
                .execute(&mut *tx)
                .await
                .map_err($crate::sqlx_err)?;
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
        tx.commit().await.map_err($crate::sqlx_err)?;
        Ok(::sela_core::ApplyChangesResult {
            statements_executed: plan.statements.len(),
            rows_affected,
        })
    }};
}

pub(crate) use apply_change_plan;
