//! Query execution, paging, and result application.

use std::sync::Arc;

use gpui_kit::Context;

use sela_core::{
    preview_table, run_user_sql, DatabaseSession, DbError, StatementOutcome, PAGE_SIZE,
};
use sela_drivers::drive;

use super::{GridSource, Workspace};

impl Workspace {
    pub(super) fn bump(&mut self) -> u64 {
        self.request = self.request.wrapping_add(1);
        self.request
    }

    pub(super) fn spawn_work<T, Done>(
        &mut self,
        cx: &mut Context<Self>,
        work: impl std::future::Future<Output = Result<T, DbError>> + Send + 'static,
        done: Done,
    ) where
        T: Send + 'static,
        Done: FnOnce(&mut Self, Result<T, DbError>, &mut Context<Self>) + 'static,
    {
        let request = self.bump();
        self.busy = true;
        cx.notify();
        cx.spawn(async move |this, async_cx| {
            let result = drive(work).await;
            this.update(async_cx, |this, cx| {
                if this.request != request {
                    return;
                }
                this.busy = false;
                done(this, result, cx);
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn open_table(&mut self, schema: String, table: String, cx: &mut Context<Self>) {
        self.source = GridSource::Table { schema, table };
        self.page_offset = 0;
        self.load_source(cx);
    }

    pub(super) fn run_editor(&mut self, cx: &mut Context<Self>) {
        let sql = self.sql.read(cx).value().to_string();
        if sql.trim().is_empty() {
            self.status = "Write a SQL statement, then run it.".into();
            self.error = None;
            cx.notify();
            return;
        }
        self.source = GridSource::Sql(sql);
        self.page_offset = 0;
        self.load_source(cx);
    }

    pub(super) fn load_source(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            self.error = Some("Connect to a database before running SQL.".into());
            self.status = "Not connected".into();
            cx.notify();
            return;
        };
        let source = self.source.clone();
        let offset = self.page_offset;
        self.error = None;
        self.note = None;
        self.status = "Running…".into();
        self.grid.update(cx, |state, cx| {
            state.delegate_mut().loading = true;
            state.refresh(cx);
        });
        self.spawn_work(
            cx,
            async move { run_source(session, source, offset).await },
            |this, result, cx| this.apply_outcome(result, cx),
        );
    }

    pub(super) fn apply_outcome(
        &mut self,
        result: Result<Option<StatementOutcome>, DbError>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(Some(StatementOutcome::Rows(rows))) => {
                self.page_len = rows.page.rows.len();
                self.total_rows = rows.total_rows;
                self.paged = rows.paged;
                self.note = rows.note.clone();
                let start = if self.page_len == 0 {
                    0
                } else {
                    self.page_offset + 1
                };
                let end = self.page_offset + self.page_len as u64;
                let ms = rows.duration.as_millis();
                self.status = match rows.total_rows {
                    Some(total) => format!("{total} rows · {ms} ms · {start}–{end}"),
                    None => format!("{} rows · {ms} ms", self.page_len),
                };
                if let Some(note) = &rows.note {
                    self.status = format!("{} · {note}", self.status);
                }
                self.grid.update(cx, |state, cx| {
                    state.delegate_mut().show_page(rows.page);
                    state.refresh(cx);
                });
            }
            Ok(Some(StatementOutcome::Executed(executed))) => {
                self.page_len = 0;
                self.total_rows = None;
                self.paged = false;
                self.note = None;
                let message = format!(
                    "{} rows affected · {} ms",
                    executed.rows_affected,
                    executed.duration.as_millis()
                );
                self.status = message.clone();
                self.grid.update(cx, |state, cx| {
                    state.delegate_mut().show_message(message);
                    state.refresh(cx);
                });
            }
            Ok(None) => {}
            Err(err) => {
                self.status = "Query failed".into();
                self.error = Some(err.to_string());
                self.grid.update(cx, |state, cx| {
                    state.delegate_mut().loading = false;
                    state.refresh(cx);
                });
            }
        }
        cx.notify();
    }

    pub(super) fn step_page(&mut self, delta: i64, cx: &mut Context<Self>) {
        if delta < 0 {
            if self.page_offset == 0 {
                return;
            }
            self.page_offset = self.page_offset.saturating_sub(PAGE_SIZE as u64);
        } else if self.can_go_next() {
            self.page_offset += PAGE_SIZE as u64;
        } else {
            return;
        }
        self.load_source(cx);
    }

    pub(super) fn can_go_next(&self) -> bool {
        if self.page_len == 0 {
            return false;
        }
        if let Some(total) = self.total_rows {
            self.page_offset + (self.page_len as u64) < total
        } else {
            self.paged && self.page_len as u32 == PAGE_SIZE
        }
    }
}

pub(super) async fn run_source(
    session: Arc<dyn DatabaseSession>,
    source: GridSource,
    offset: u64,
) -> Result<Option<StatementOutcome>, DbError> {
    match source {
        GridSource::Idle => Ok(None),
        GridSource::Table { schema, table } => {
            preview_table(session.as_ref(), &schema, &table, PAGE_SIZE, offset)
                .await
                .map(Some)
        }
        GridSource::Sql(sql) => run_user_sql(session.as_ref(), &sql, PAGE_SIZE, offset)
            .await
            .map(Some),
    }
}
