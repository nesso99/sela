use std::time::Instant;

use async_trait::async_trait;

use crate::error::DbError;
use crate::sql::{self, StatementKind};
use crate::types::{
    Dialect, ExecOutcome, QueryPage, RowSet, SchemaInfo, StatementOutcome, TableInfo,
};

#[async_trait]
pub trait DatabaseSession: Send + Sync {
    fn dialect(&self) -> Dialect;

    async fn list_schemas(&self) -> Result<Vec<SchemaInfo>, DbError>;

    async fn list_tables(&self, schema: &str) -> Result<Vec<TableInfo>, DbError>;

    async fn query(&self, sql: &str) -> Result<QueryPage, DbError>;

    async fn execute(&self, sql: &str) -> Result<u64, DbError>;

    async fn count_query(&self, sql: &str) -> Result<u64, DbError>;
}

/// Run one user statement. Queries are paged; other statements are executed.
pub async fn run_user_sql(
    session: &dyn DatabaseSession,
    sql: &str,
    limit: u32,
    offset: u64,
) -> Result<StatementOutcome, DbError> {
    let started = Instant::now();
    let sql = sql::normalize_sql(sql).map_err(|err| DbError::query(err.to_string()))?;
    match sql::classify(&sql) {
        StatementKind::Execute => {
            let rows_affected = session.execute(&sql).await?;
            Ok(StatementOutcome::Executed(ExecOutcome {
                rows_affected,
                duration: started.elapsed(),
            }))
        }
        StatementKind::PageableQuery => {
            let page_sql = sql::wrap_page(session.dialect(), &sql, limit, offset);
            let count_sql = sql::wrap_count(session.dialect(), &sql);
            let mut page = session.query(&page_sql).await?;
            let total_rows = session.count_query(&count_sql).await.ok();
            page.duration = started.elapsed();
            Ok(StatementOutcome::Rows(RowSet {
                page,
                total_rows,
                duration: started.elapsed(),
                paged: true,
                note: None,
            }))
        }
        StatementKind::RawQuery => {
            let mut page = session.query(&sql).await?;
            let mut note = None;
            if page.rows.len() > limit as usize {
                page.rows.truncate(limit as usize);
                note = Some(format!(
                    "Showing the first {limit} rows. This statement cannot be paged on the server."
                ));
            }
            page.duration = started.elapsed();
            Ok(StatementOutcome::Rows(RowSet {
                page,
                total_rows: None,
                duration: started.elapsed(),
                paged: false,
                note,
            }))
        }
    }
}

/// Page through `SELECT *` for one table. `limit` defaults to [`crate::PAGE_SIZE`] at the call site.
pub async fn preview_table(
    session: &dyn DatabaseSession,
    schema: &str,
    table: &str,
    limit: u32,
    offset: u64,
) -> Result<StatementOutcome, DbError> {
    let sql = sql::select_all(session.dialect(), schema, table)
        .map_err(|err| DbError::query(err.to_string()))?;
    run_user_sql(session, &sql, limit, offset).await
}

#[derive(Debug, Clone)]
pub struct LoadedSchema {
    pub name: String,
    pub tables: Vec<TableInfo>,
}
