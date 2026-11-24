//! Execute a [`sela_core::ChangePlan`] inside one sqlx transaction.

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
        for stmt in &plan.statements {
            let result = $crate::bind_params($crate::query(&stmt.sql), &stmt.params)
                .execute(&mut *tx)
                .await
                .map_err($crate::sqlx_err)?;
            rows_affected = rows_affected.saturating_add(result.rows_affected());
        }
        tx.commit().await.map_err($crate::sqlx_err)?;
        Ok(::sela_core::ApplyChangesResult {
            statements_executed: plan.statements.len(),
            rows_affected,
        })
    }};
}

pub(crate) use apply_change_plan;
