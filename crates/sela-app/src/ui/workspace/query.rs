//! Query execution, cancel, streaming, and result application (per tab).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use futures_util::future::{select, Either};
use gpui_kit::Context;
use tokio_util::sync::CancellationToken;

use sela_core::{
    build_table_page, classify, exact_table_rows, history_rows_label_affected,
    history_rows_label_for_rows, is_safe_to_rerun_for_load_more, preview_table, run_user_sql,
    DatabaseSession, DbError, FilterCondition, HistoryOutcome, RowCountKind, SortSpec, SqlError,
    StatementKind, StatementOutcome, StreamBatch, TabId, TablePreview, DEFAULT_SQL_ROW_CAP,
    PAGE_SIZE, STREAM_BATCH_SIZE,
};
use sela_drivers::drive;

use super::{GridSource, Workspace};

impl Workspace {
    pub(super) fn spawn_work<T, Fut, Done>(
        &mut self,
        tab_id: Option<TabId>,
        cx: &mut Context<Self>,
        work: impl FnOnce(CancellationToken) -> Fut + Send + 'static,
        done: Done,
    ) where
        T: Send + 'static,
        Fut: std::future::Future<Output = Result<T, DbError>> + Send + 'static,
        Done: FnOnce(&mut Self, Result<T, DbError>, &mut Context<Self>) + 'static,
    {
        let token = CancellationToken::new();
        let request = if let Some(id) = tab_id {
            if let Some(tab) = self.tabs.get_mut(id) {
                tab.cancel_running();
                tab.set_cancel(token.clone());
                tab.bump_request()
            } else {
                0
            }
        } else {
            0
        };
        if let Some(id) = tab_id {
            if let Some(view) = self.view_mut(id) {
                view.busy = true;
                view.query_started = Some(Instant::now());
            }
        }
        let fut = work(token);
        cx.notify();
        cx.spawn(async move |this, async_cx| {
            let result = drive(fut).await;
            this.update(async_cx, |this, cx| {
                if let Some(id) = tab_id {
                    let Some(tab) = this.tabs.get(id) else {
                        return;
                    };
                    if tab.request != request {
                        return;
                    }
                    if let Some(tab) = this.tabs.get_mut(id) {
                        tab.clear_cancel();
                    }
                    if let Some(view) = this.view_mut(id) {
                        view.busy = false;
                    }
                }
                done(this, result, cx);
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn stop_query(&mut self, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.active_mut() else {
            return;
        };
        let id = tab.id;
        let Some(view) = self.views.get(&id) else {
            return;
        };
        if !view.busy {
            return;
        }
        self.clear_run_queue(id);
        if let Some(tab) = self.tabs.get_mut(id) {
            tab.cancel_running();
        }
        if let Some(view) = self.view_mut(id) {
            view.status = "Cancelling…".into();
            view.sql_running_statement_index = None;
        }
        cx.notify();
    }

    pub(super) fn run_all_editor(&mut self, cx: &mut Context<Self>) {
        let Some(tab_id) = self.tabs.active().map(|tab| tab.id) else {
            return;
        };
        match self.statements_for_run_all(cx) {
            Err(SqlError::Empty) => self.prompt_empty_sql(tab_id, cx),
            Err(SqlError::UnclosedQuote { offset }) => self.set_local_sql_error(tab_id, offset, cx),
            Err(_) => self.prompt_empty_sql(tab_id, cx),
            Ok(statements) if statements.is_empty() => self.prompt_empty_sql(tab_id, cx),
            Ok(statements) => self.begin_sql_run(tab_id, statements, cx),
        }
    }

    pub(super) fn run_editor(&mut self, cx: &mut Context<Self>) {
        let Some(tab_id) = self.tabs.active().map(|tab| tab.id) else {
            return;
        };
        match self.statements_for_run(cx) {
            Err(SqlError::Empty) => self.prompt_empty_sql(tab_id, cx),
            Err(SqlError::UnclosedQuote { offset }) => self.set_local_sql_error(tab_id, offset, cx),
            Err(_) => self.prompt_empty_sql(tab_id, cx),
            Ok(statements) if statements.is_empty() => self.prompt_empty_sql(tab_id, cx),
            Ok(statements) => self.begin_sql_run(tab_id, statements, cx),
        }
    }

    fn prompt_empty_sql(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        if let Some(view) = self.view_mut(tab_id) {
            view.status = "Write a SQL statement, then run it.".into();
            view.error = None;
            view.local_sql_error = None;
        }
        cx.notify();
    }

    fn set_local_sql_error(&mut self, tab_id: TabId, offset: usize, cx: &mut Context<Self>) {
        if let Some(view) = self.view_mut(tab_id) {
            view.error = Some("Unclosed quote".into());
            view.local_sql_error = Some(SqlError::UnclosedQuote { offset });
            view.status = "Unclosed quote".into();
        }
        cx.notify();
    }

    pub(super) fn begin_sql_run(
        &mut self,
        tab_id: TabId,
        statements: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        if let Some(view) = self.view_mut(tab_id) {
            view.result_snapshots.clear();
            view.sql_run_queue = statements;
            view.sql_run_base_index = 0;
            view.sql_failed_statement = None;
            view.local_sql_error = None;
            view.error = None;
            view.last_error_sqlstate = None;
            view.active_result_index = 0;
            view.table_pane = super::TablePane::Data;
            view.structure_error = None;
            view.page_offset = 0;
            view.sql_row_cap = DEFAULT_SQL_ROW_CAP;
            view.truncated = false;
            view.history_should_record = false;
        }
        self.clear_sql_error_decorations(tab_id, cx);
        self.persist_tabs(cx);
        self.run_next_sql_statement(tab_id, cx);
    }

    fn run_next_sql_statement(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        let has_next = {
            let Some(view) = self.view_mut(tab_id) else {
                return;
            };
            let Some(sql) = view.sql_run_queue.first().cloned() else {
                view.sql_running_statement_index = None;
                view.busy = false;
                view.history_should_record = false;
                return;
            };
            view.sql_run_statement_index = view.sql_run_base_index + 1;
            view.sql_running_statement_index = Some(view.sql_run_base_index);
            view.source = GridSource::Sql(sql);
            view.history_should_record = true;
            true
        };
        if has_next {
            self.load_source_for(tab_id, cx);
            self.spawn_running_status_tick(tab_id, cx);
        }
    }

    fn spawn_running_status_tick(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        let request = self.tabs.get(tab_id).map(|t| t.request).unwrap_or(0);
        cx.spawn(async move |this, async_cx| loop {
            async_cx
                .background_executor()
                .timer(std::time::Duration::from_millis(250))
                .await;
            let cont = this
                .update(async_cx, |ws, cx| {
                    let Some(tab) = ws.tabs.get(tab_id) else {
                        return false;
                    };
                    if tab.request != request {
                        return false;
                    }
                    let Some(view) = ws.views.get_mut(&tab_id) else {
                        return false;
                    };
                    if !view.busy {
                        return false;
                    }
                    let secs = view
                        .query_started
                        .map(|t| t.elapsed().as_secs_f32())
                        .unwrap_or(0.0);
                    view.status = format!("Running · {secs:.1} s");
                    cx.notify();
                    true
                })
                .unwrap_or(false);
            if !cont {
                break;
            }
        })
        .detach();
    }

    fn finish_sql_statement(
        &mut self,
        tab_id: TabId,
        failed: bool,
        sqlstate: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if let Some(view) = self.view_mut(tab_id) {
            if failed {
                view.sql_failed_statement = Some(view.sql_run_statement_index);
                view.last_error_sqlstate = sqlstate.clone();
                view.status = Self::format_query_failed_status(
                    Some(view.sql_run_statement_index),
                    sqlstate.as_deref(),
                );
            }
            view.sql_running_statement_index = None;
            if !view.sql_run_queue.is_empty() {
                view.sql_run_queue.remove(0);
            }
            view.sql_run_base_index += 1;
        }
        self.push_result_snapshot(tab_id, cx);
        if failed {
            if let Some(view) = self.view_mut(tab_id) {
                if let Some(last) = view.result_snapshots.last_mut() {
                    last.failed = true;
                }
            }
            cx.notify();
            return;
        }
        let has_more = self
            .views
            .get(&tab_id)
            .is_some_and(|view| !view.sql_run_queue.is_empty());
        if has_more {
            self.run_next_sql_statement(tab_id, cx);
        }
        cx.notify();
    }

    fn maybe_finish_sql_batch(
        &mut self,
        tab_id: TabId,
        result: &Result<(), DbError>,
        cx: &mut Context<Self>,
    ) {
        let running_batch = self.views.get(&tab_id).is_some_and(|view| {
            matches!(view.source, GridSource::Sql(_))
                && (view.sql_run_base_index > 0
                    || !view.sql_run_queue.is_empty()
                    || view.sql_running_statement_index.is_some())
        });
        if !running_batch {
            return;
        }
        match result {
            Err(err) => {
                self.finish_sql_statement(tab_id, true, err.sqlstate().map(str::to_string), cx);
            }
            Ok(()) => self.finish_sql_statement(tab_id, false, None, cx),
        }
    }

    pub(super) fn advance_sql_batch_after_stream(
        &mut self,
        tab_id: TabId,
        failed: bool,
        err: Option<&DbError>,
        cx: &mut Context<Self>,
    ) {
        if failed {
            let sqlstate = err.and_then(|e| e.sqlstate()).map(str::to_string);
            self.finish_sql_statement(tab_id, true, sqlstate, cx);
        } else {
            self.finish_sql_statement(tab_id, false, None, cx);
        }
    }

    pub(super) fn load_more(&mut self, cx: &mut Context<Self>) {
        let Some(tab_id) = self.tabs.active().map(|tab| tab.id) else {
            return;
        };
        let Some(view) = self.views.get(&tab_id) else {
            return;
        };
        if !view.truncated || !view.can_load_more {
            return;
        }
        if let Some(cap) = view.live_row_cap.clone() {
            let next = cap
                .load(Ordering::SeqCst)
                .saturating_add(DEFAULT_SQL_ROW_CAP as usize);
            cap.store(next, Ordering::SeqCst);
            if let Some(view) = self.view_mut(tab_id) {
                view.sql_row_cap = next as u32;
                view.truncated = false;
                view.status = format!("Loading more (cap {next})…");
                view.busy = true;
            }
            cx.notify();
            return;
        }
        let sql = match &view.source {
            GridSource::Sql(sql) => sql.clone(),
            _ => {
                self.refuse_unsafe_load_more(tab_id, cx);
                return;
            }
        };
        if !is_safe_to_rerun_for_load_more(&sql) {
            self.refuse_unsafe_load_more(tab_id, cx);
            return;
        }
        if let Some(view) = self.view_mut(tab_id) {
            view.sql_row_cap = view.sql_row_cap.saturating_add(DEFAULT_SQL_ROW_CAP);
            view.truncated = false;
        }
        self.load_source_for(tab_id, cx);
    }

    fn refuse_unsafe_load_more(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        if let Some(view) = self.view_mut(tab_id) {
            let n = view.sql_row_cap;
            view.can_load_more = false;
            view.status = format!("Result capped at {n} rows. Re-run manually to see more.");
            view.note = Some(
                "Load more is disabled: re-running this statement could repeat side effects."
                    .into(),
            );
        }
        cx.notify();
    }

    fn sql_allows_load_more_rerun(source: &GridSource) -> bool {
        match source {
            GridSource::Sql(sql) => is_safe_to_rerun_for_load_more(sql),
            GridSource::Table { .. } | GridSource::Idle => false,
        }
    }

    pub(super) fn request_exact_count(&mut self, cx: &mut Context<Self>) {
        let Some(tab_id) = self.tabs.active().map(|tab| tab.id) else {
            return;
        };
        let Some(view) = self.views.get(&tab_id) else {
            return;
        };
        let GridSource::Table { schema, table } = view.source.clone() else {
            return;
        };
        let filters = view.filters.clone();
        let Some(session) = self.session.clone() else {
            return;
        };
        if let Some(view) = self.view_mut(tab_id) {
            view.status = "Counting…".into();
        }
        self.spawn_work(
            Some(tab_id),
            cx,
            |cancel| async move {
                let work = exact_table_rows(session.as_ref(), &schema, &table, &filters);
                futures_util::pin_mut!(work);
                let aborted = cancel.cancelled();
                futures_util::pin_mut!(aborted);
                match select(work, aborted).await {
                    Either::Left((result, _)) => {
                        if cancel.is_cancelled() {
                            Err(DbError::cancelled("count cancelled"))
                        } else {
                            result
                        }
                    }
                    Either::Right((_, _)) => Err(DbError::cancelled("count cancelled")),
                }
            },
            move |this, result, cx| match result {
                Ok(total) => {
                    if let Some(view) = this.view_mut(tab_id) {
                        view.total_rows = Some(total);
                        view.row_count_kind = RowCountKind::Exact;
                        view.grid.update(cx, |state, cx| {
                            state.delegate_mut().result.set_total_rows(total as usize);
                            state.refresh(cx);
                        });
                    }
                    this.refresh_status_from_state(tab_id);
                    cx.notify();
                }
                Err(err) => {
                    // Count failure/cancel is not a data-query failure — keep the
                    // grid and previous kind; do not set view.error (no banner).
                    if let Some(view) = this.view_mut(tab_id) {
                        view.error = None;
                        view.status = if matches!(err, DbError::Cancelled { .. }) {
                            "Count cancelled".into()
                        } else {
                            format!("Count failed · {err}")
                        };
                    }
                    cx.notify();
                }
            },
        );
        // Tick Counting… · elapsed ~4×/sec while this request is busy.
        let request = self.tabs.get(tab_id).map(|t| t.request).unwrap_or(0);
        cx.spawn(async move |this, async_cx| loop {
            async_cx
                .background_executor()
                .timer(std::time::Duration::from_millis(250))
                .await;
            let cont = this
                .update(async_cx, |ws, cx| {
                    let Some(tab) = ws.tabs.get(tab_id) else {
                        return false;
                    };
                    if tab.request != request {
                        return false;
                    }
                    let Some(view) = ws.views.get_mut(&tab_id) else {
                        return false;
                    };
                    if !view.busy {
                        return false;
                    }
                    let secs = view
                        .query_started
                        .map(|t| t.elapsed().as_secs_f32())
                        .unwrap_or(0.0);
                    view.status = format!("Counting… · {secs:.1} s");
                    cx.notify();
                    true
                })
                .unwrap_or(false);
            if !cont {
                break;
            }
        })
        .detach();
    }

    pub(super) fn load_source_for(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            if let Some(view) = self.view_mut(tab_id) {
                view.error = Some("Connect to a database before running SQL.".into());
                view.status = "Not connected".into();
            }
            cx.notify();
            return;
        };
        let Some(view) = self.views.get_mut(&tab_id) else {
            return;
        };
        let source = view.source.clone();
        let offset = view.page_offset;
        let sql_cap = view.sql_row_cap;
        let filters = view.filters.clone();
        let sorts = view.sorts.clone();
        let is_table = matches!(source, GridSource::Table { .. });
        view.error = None;
        view.note = None;
        view.truncated = false;
        view.can_load_more = false;
        view.grid.update(cx, |state, _| {
            // gpui-component only delivers header clicks when TableState.sortable
            // is true; that path calls perform_sort and does not reorder rows.
            state.sortable = is_table;
            let d = state.delegate_mut();
            d.sortable = is_table;
            d.sorts = sorts.clone();
        });
        view.live_row_cap = None;
        view.status = "Running…".into();
        view.grid.update(cx, |state, cx| {
            let delegate = state.delegate_mut();
            delegate.loading = true;
            delegate.pending_pages.clear();
            if matches!(&source, GridSource::Sql(_)) {
                delegate.result.reset(Vec::new(), 0);
                delegate.set_loading_message();
            }
            state.refresh(cx);
        });

        match &source {
            GridSource::Idle => {
                view.busy = false;
                cx.notify();
            }
            GridSource::Table { .. } => {
                let cancel = CancellationToken::new();
                let request = if let Some(tab) = self.tabs.get_mut(tab_id) {
                    tab.cancel_running();
                    tab.set_cancel(cancel.clone());
                    tab.bump_request()
                } else {
                    0
                };
                if let Some(view) = self.view_mut(tab_id) {
                    view.busy = true;
                    view.query_started = Some(Instant::now());
                }
                cx.notify();
                cx.spawn(async move |this, async_cx| {
                    let result = drive(async move {
                        run_source(session, source, offset, sql_cap, cancel, &filters, &sorts).await
                    })
                    .await;
                    this.update(async_cx, |this, cx| {
                        let Some(tab) = this.tabs.get(tab_id) else {
                            return;
                        };
                        if tab.request != request {
                            return;
                        }
                        if let Some(tab) = this.tabs.get_mut(tab_id) {
                            tab.clear_cancel();
                        }
                        if let Some(view) = this.view_mut(tab_id) {
                            view.busy = false;
                        }
                        this.apply_outcome(tab_id, result, cx);
                    })
                    .ok();
                })
                .detach();
            }
            GridSource::Sql(sql) => {
                self.start_sql_stream(tab_id, session, sql.clone(), sql_cap, cx);
            }
        }
    }

    fn start_sql_stream(
        &mut self,
        tab_id: TabId,
        session: Arc<dyn DatabaseSession>,
        sql: String,
        sql_cap: u32,
        cx: &mut Context<Self>,
    ) {
        match classify(&sql) {
            StatementKind::Execute => {
                self.spawn_work(
                    Some(tab_id),
                    cx,
                    move |cancel| async move {
                        run_user_sql(session.as_ref(), &sql, sql_cap, cancel)
                            .await
                            .map(Some)
                    },
                    move |this, result, cx| this.apply_outcome(tab_id, result, cx),
                );
            }
            StatementKind::PageableQuery | StatementKind::RawQuery => {
                let cancel = CancellationToken::new();
                let request = if let Some(tab) = self.tabs.get_mut(tab_id) {
                    tab.cancel_running();
                    tab.set_cancel(cancel.clone());
                    tab.bump_request()
                } else {
                    0
                };
                let cap = Arc::new(AtomicUsize::new(sql_cap.max(1) as usize));
                if let Some(view) = self.view_mut(tab_id) {
                    view.busy = true;
                    view.query_started = Some(Instant::now());
                    view.live_row_cap = Some(cap.clone());
                    view.row_count_kind = RowCountKind::AtLeast;
                    view.total_rows = Some(0);
                    view.paged = false;
                    view.grid.update(cx, |state, cx| {
                        state.delegate_mut().result.reset(Vec::new(), 0);
                        state.delegate_mut().loading = true;
                        state.refresh(cx);
                    });
                }
                cx.notify();

                let (tx, rx) = async_channel::bounded::<StreamBatch>(8);
                cx.spawn(async move |this, async_cx| {
                    let produce = drive({
                        let session = session.clone();
                        let sql = sql.clone();
                        let cancel = cancel.clone();
                        let cap = cap.clone();
                        async move {
                            session
                                .stream_query(
                                    &sql, &[],
                                    STREAM_BATCH_SIZE,
                                    Some(cap),
                                    false,
                                    cancel,
                                    tx,
                                )
                                .await
                        }
                    });
                    let mut produce = Box::pin(produce);

                    loop {
                        let recv = rx.recv();
                        futures_util::pin_mut!(recv);
                        match select(produce.as_mut(), recv).await {
                            Either::Left((outcome, _)) => {
                                this.update(async_cx, |this, cx| {
                                    let Some(tab) = this.tabs.get(tab_id) else {
                                        return;
                                    };
                                    if tab.request != request {
                                        return;
                                    }
                                    if let Some(tab) = this.tabs.get_mut(tab_id) {
                                        tab.clear_cancel();
                                    }
                                    if let Some(view) = this.view_mut(tab_id) {
                                        view.busy = false;
                                        view.live_row_cap = None;
                                    }
                                    match outcome {
                                        Ok(summary) => {
                                            if let Some(view) = this.view_mut(tab_id) {
                                                view.truncated = summary.truncated;
                                                view.can_load_more = summary.truncated
                                                    && Self::sql_allows_load_more_rerun(
                                                        &view.source,
                                                    );
                                                if summary.truncated {
                                                    view.row_count_kind = RowCountKind::AtLeast;
                                                    if view.can_load_more {
                                                        view.note = Some(format!(
                                                            "Showing the first {} rows. Use Load more to continue.",
                                                            summary.rows_delivered
                                                        ));
                                                    } else {
                                                        view.note = Some(format!(
                                                            "Showing the first {} rows. Re-run manually to see more.",
                                                            summary.rows_delivered
                                                        ));
                                                    }
                                                } else {
                                                    view.row_count_kind = RowCountKind::Exact;
                                                }
                                                view.total_rows =
                                                    Some(summary.rows_delivered as u64);
                                                view.page_len = summary.rows_delivered;
                                                view.grid.update(cx, |state, cx| {
                                                    state.delegate_mut().loading = false;
                                                    state.delegate_mut().result.set_total_rows(
                                                        summary.rows_delivered,
                                                    );
                                                    state.refresh(cx);
                                                });
                                            }
                                            this.refresh_status_from_state(tab_id);
                                            this.advance_sql_batch_after_stream(
                                                tab_id, false, None, cx,
                                            );
                                        }
                                        Err(err) if matches!(err, DbError::Cancelled { .. }) => {
                                            this.apply_cancelled(tab_id, err, cx);
                                        }
                                        Err(err) => {
                                            if let Some(view) = this.view_mut(tab_id) {
                                                let stmt = view.sql_run_statement_index;
                                                view.status = Self::format_query_failed_status(
                                                    Some(stmt),
                                                    err.sqlstate(),
                                                );
                                                view.error = Some(err.to_string());
                                                view.grid.update(cx, |state, cx| {
                                                    state.delegate_mut().loading = false;
                                                    state.refresh(cx);
                                                });
                                            }
                                            this.advance_sql_batch_after_stream(
                                                tab_id, true, Some(&err), cx,
                                            );
                                        }
                                    }
                                    cx.notify();
                                })
                                .ok();
                                break;
                            }
                            Either::Right((Ok(batch), _)) => {
                                this.update(async_cx, |this, cx| {
                                    let Some(tab) = this.tabs.get(tab_id) else {
                                        return;
                                    };
                                    if tab.request != request {
                                        return;
                                    }
                                    this.apply_stream_batch(tab_id, batch, cx);
                                })
                                .ok();
                            }
                            Either::Right((Err(_), _)) => {
                                let outcome = produce.await;
                                this.update(async_cx, |this, cx| {
                                    let Some(tab) = this.tabs.get(tab_id) else {
                                        return;
                                    };
                                    if tab.request != request {
                                        return;
                                    }
                                    if let Some(tab) = this.tabs.get_mut(tab_id) {
                                        tab.clear_cancel();
                                    }
                                    if let Some(view) = this.view_mut(tab_id) {
                                        view.busy = false;
                                        view.live_row_cap = None;
                                    }
                                    match outcome {
                                        Ok(summary) => {
                                            if let Some(view) = this.view_mut(tab_id) {
                                                view.truncated = summary.truncated;
                                                view.can_load_more = summary.truncated
                                                    && Self::sql_allows_load_more_rerun(
                                                        &view.source,
                                                    );
                                                view.total_rows =
                                                    Some(summary.rows_delivered as u64);
                                                view.page_len = summary.rows_delivered;
                                                view.grid.update(cx, |state, cx| {
                                                    state.delegate_mut().loading = false;
                                                    state.refresh(cx);
                                                });
                                            }
                                            this.refresh_status_from_state(tab_id);
                                            this.advance_sql_batch_after_stream(
                                                tab_id, false, None, cx,
                                            );
                                        }
                                        Err(err) if matches!(err, DbError::Cancelled { .. }) => {
                                            this.apply_cancelled(tab_id, err, cx);
                                        }
                                        Err(err) => {
                                            if let Some(view) = this.view_mut(tab_id) {
                                                let stmt = view.sql_run_statement_index;
                                                view.status = Self::format_query_failed_status(
                                                    Some(stmt),
                                                    err.sqlstate(),
                                                );
                                                view.error = Some(err.to_string());
                                                view.grid.update(cx, |state, cx| {
                                                    state.delegate_mut().loading = false;
                                                    state.refresh(cx);
                                                });
                                            }
                                            this.advance_sql_batch_after_stream(
                                                tab_id, true, Some(&err), cx,
                                            );
                                        }
                                    }
                                    cx.notify();
                                })
                                .ok();
                                break;
                            }
                        }
                    }
                })
                .detach();
            }
        }
    }

    fn apply_stream_batch(&mut self, tab_id: TabId, batch: StreamBatch, cx: &mut Context<Self>) {
        let Some(view) = self.view_mut(tab_id) else {
            return;
        };
        if batch.paused_at_cap {
            view.truncated = true;
            view.can_load_more = view.live_row_cap.is_some();
            view.busy = false;
            view.grid.update(cx, |state, cx| {
                state.delegate_mut().loading = false;
                state.refresh(cx);
            });
            self.refresh_status_from_state(tab_id);
            cx.notify();
            return;
        }

        view.grid.update(cx, |state, cx| {
            let delegate = state.delegate_mut();
            if let Some(columns) = batch.columns {
                if delegate.result.columns().is_empty() {
                    delegate.result.set_columns(columns);
                }
            }
            if !batch.rows.is_empty() {
                let page_size = delegate.result.page_size().max(1);
                let mut rows = batch.rows;
                let mut absolute = batch.start_index;
                while !rows.is_empty() {
                    let page_index = absolute / page_size;
                    let offset_in_page = absolute % page_size;
                    if offset_in_page != 0 {
                        let take = (page_size - offset_in_page).min(rows.len());
                        let chunk: Vec<_> = rows.drain(..take).collect();
                        delegate.result.insert_page(page_index, chunk);
                        absolute += take;
                        continue;
                    }
                    let take = rows.len().min(page_size);
                    let chunk: Vec<_> = rows.drain(..take).collect();
                    let end = absolute + chunk.len();
                    delegate.result.insert_page(page_index, chunk);
                    delegate.result.ensure_total_rows(end);
                    absolute = end;
                }
                delegate.loading = false;
            }
            state.refresh(cx);
        });

        let loaded = self
            .views
            .get(&tab_id)
            .map(|view| view.grid.read(cx).delegate().result.total_rows())
            .unwrap_or(0);
        if let Some(view) = self.view_mut(tab_id) {
            view.page_len = loaded;
            view.total_rows = Some(loaded as u64);
            view.row_count_kind = RowCountKind::AtLeast;
        }
        self.refresh_status_from_state(tab_id);
        cx.notify();
        self.fetch_pending_pages_for(tab_id, cx);
    }

    pub(super) fn fetch_pending_pages_for(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        let Some(view) = self.views.get(&tab_id) else {
            return;
        };
        let GridSource::Table { schema, table } = view.source.clone() else {
            return;
        };
        let filters = view.filters.clone();
        let sorts = view.sorts.clone();
        let pending: Vec<usize> = self.views.get_mut(&tab_id).map_or(Vec::new(), |view| {
            view.grid.update(cx, |state, _| {
                let d = state.delegate_mut();
                let pages = d.pending_pages.clone();
                d.pending_pages.clear();
                pages
            })
        });
        if pending.is_empty() {
            return;
        }
        let Some(session) = self.session.clone() else {
            return;
        };
        let cancel = self
            .tabs
            .get(tab_id)
            .and_then(|tab| tab.cancel_token().cloned())
            .unwrap_or_default();
        let request = self.tabs.get(tab_id).map(|tab| tab.request).unwrap_or(0);
        for page_index in pending {
            let session = session.clone();
            let schema = schema.clone();
            let table = table.clone();
            let filters = filters.clone();
            let sorts = sorts.clone();
            let cancel = cancel.clone();
            let offset = (page_index as u64) * (PAGE_SIZE as u64);
            cx.spawn(async move |this, async_cx| {
                let result = drive(async move {
                    let bound = build_table_page(
                        session.dialect(),
                        &schema,
                        &table,
                        &filters,
                        &sorts,
                        PAGE_SIZE,
                        offset,
                    )
                    .map_err(|err| DbError::query(err.to_string()))?;
                    let (tx, rx) = async_channel::bounded(2);
                    let stream = session.stream_query(
                        &bound.sql,
                        &bound.params,
                        STREAM_BATCH_SIZE,
                        Some(Arc::new(AtomicUsize::new(PAGE_SIZE as usize))),
                        true,
                        cancel,
                        tx,
                    );
                    let collect = async {
                        let mut rows = Vec::new();
                        while let Ok(batch) = rx.recv().await {
                            rows.extend(batch.rows);
                        }
                        rows
                    };
                    let (outcome, rows) = futures_util::future::join(stream, collect).await;
                    outcome?;
                    Ok(rows)
                })
                .await;
                this.update(async_cx, |this, cx| {
                    let Some(tab) = this.tabs.get(tab_id) else {
                        return;
                    };
                    if tab.request != request {
                        return;
                    }
                    if let Ok(rows) = result {
                        if let Some(view) = this.view_mut(tab_id) {
                            view.grid.update(cx, |state, cx| {
                                state.delegate_mut().result.insert_page(page_index, rows);
                                state.refresh(cx);
                            });
                        }
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        }
    }

    fn apply_cancelled(&mut self, tab_id: TabId, err: DbError, cx: &mut Context<Self>) {
        self.clear_run_queue(tab_id);
        let (sql, should_record, rows_label) = {
            let Some(view) = self.view_mut(tab_id) else {
                return;
            };
            let rows = view.grid.read(cx).delegate().result.total_rows();
            view.page_len = rows;
            view.total_rows = Some(rows as u64);
            view.error = None;
            let ms = view
                .query_started
                .map(|t| t.elapsed().as_millis())
                .unwrap_or(0);
            view.status = format!("Cancelled · {rows} rows · {ms} ms · {err}");
            view.note = Some("Query cancelled".into());
            view.grid.update(cx, |state, cx| {
                state.delegate_mut().loading = false;
                state.refresh(cx);
            });
            let sql = match &view.source {
                GridSource::Sql(sql) => Some(sql.clone()),
                _ => None,
            };
            let should_record = view.history_should_record;
            view.history_should_record = false;
            let rows_label = format!("{rows} rows");
            (sql, should_record, rows_label)
        };
        if should_record {
            if let Some(sql) = sql {
                self.record_finished_sql(
                    tab_id,
                    &sql,
                    HistoryOutcome::Cancelled,
                    rows_label,
                    String::new(),
                    cx,
                );
            }
        }
        self.flush_history_password_note(tab_id);
        cx.notify();
    }

    fn refresh_status_from_state(&mut self, tab_id: TabId) {
        let Some(view) = self.view_mut(tab_id) else {
            return;
        };
        let ms = view
            .query_started
            .map(|t| t.elapsed().as_millis())
            .unwrap_or(0);
        view.status = format_status_line(
            view.row_count_kind,
            view.total_rows,
            view.page_len,
            view.truncated,
            ms,
            view.note.as_deref(),
        );
    }

    pub(super) fn apply_outcome(
        &mut self,
        tab_id: TabId,
        result: Result<Option<StatementOutcome>, DbError>,
        cx: &mut Context<Self>,
    ) {
        let batch_outcome = match &result {
            Err(DbError::Cancelled { .. }) => None,
            Err(err) => Some(Err(err.clone())),
            Ok(_) => Some(Ok(())),
        };
        self.maybe_record_history_from_outcome(tab_id, &result, cx);
        match result {
            Ok(Some(StatementOutcome::Rows(rows))) => {
                let page_offset = self
                    .views
                    .get(&tab_id)
                    .map(|view| view.page_offset)
                    .unwrap_or(0);
                let allows_load_more = self
                    .views
                    .get(&tab_id)
                    .map(|view| Self::sql_allows_load_more_rerun(&view.source))
                    .unwrap_or(false);
                if let Some(view) = self.view_mut(tab_id) {
                    view.page_len = rows.page.rows.len();
                    view.total_rows = rows.total_rows;
                    view.row_count_kind = rows.row_count_kind;
                    view.paged = rows.paged;
                    view.truncated = rows.truncated;
                    view.can_load_more = rows.truncated && allows_load_more;
                    view.note = if rows.truncated && !view.can_load_more {
                        Some(format!(
                            "Showing the first {} rows. Re-run manually to see more.",
                            rows.page.rows.len()
                        ))
                    } else {
                        rows.note.clone()
                    };
                }
                self.refresh_status_from_state(tab_id);
                if let Some(view) = self.view_mut(tab_id) {
                    if let Some(total) = rows.total_rows {
                        let window_rows = if rows.paged {
                            total as usize
                        } else {
                            rows.page.rows.len()
                        };
                        view.grid.update(cx, |state, cx| {
                            let delegate = state.delegate_mut();
                            if rows.paged {
                                delegate
                                    .result
                                    .reset(rows.page.columns.clone(), window_rows);
                                let page_index = (page_offset / PAGE_SIZE as u64) as usize;
                                delegate.result.insert_page(page_index, rows.page.rows);
                            } else {
                                delegate.show_page(rows.page);
                            }
                            state.refresh(cx);
                        });
                    } else {
                        let page_len = view.page_len;
                        view.grid.update(cx, |state, cx| {
                            let delegate = state.delegate_mut();
                            if rows.paged {
                                let start = page_offset as usize;
                                let end = start + rows.page.rows.len();
                                delegate
                                    .result
                                    .reset(rows.page.columns.clone(), end.max(start + 1));
                                let page_index = start / PAGE_SIZE as usize;
                                delegate.result.insert_page(page_index, rows.page.rows);
                                if page_len as u32 == PAGE_SIZE {
                                    delegate.result.ensure_total_rows(end + PAGE_SIZE as usize);
                                }
                            } else {
                                delegate.show_page(rows.page);
                            }
                            state.refresh(cx);
                        });
                    }
                }
                self.fetch_pending_pages_for(tab_id, cx);
            }
            Ok(Some(StatementOutcome::Executed(executed))) => {
                if let Some(view) = self.view_mut(tab_id) {
                    view.page_len = 0;
                    view.total_rows = None;
                    view.paged = false;
                    view.truncated = false;
                    view.can_load_more = false;
                    view.note = None;
                    let message = format!(
                        "{} rows affected · {} ms",
                        executed.rows_affected,
                        executed.duration.as_millis()
                    );
                    view.status = message.clone();
                    view.grid.update(cx, |state, cx| {
                        state.delegate_mut().show_message(message);
                        state.refresh(cx);
                    });
                }
            }
            Ok(None) => {}
            Err(err) if matches!(err, DbError::Cancelled { .. }) => {
                self.apply_cancelled(tab_id, err, cx);
            }
            Err(err) => {
                if let Some(view) = self.view_mut(tab_id) {
                    let stmt = view.sql_run_statement_index;
                    view.status = Self::format_query_failed_status(Some(stmt), err.sqlstate());
                    view.error = Some(err.to_string());
                    view.last_error_sqlstate = err.sqlstate().map(str::to_string);
                    view.grid.update(cx, |state, cx| {
                        state.delegate_mut().loading = false;
                        state.refresh(cx);
                    });
                }
            }
        }
        if let Some(outcome) = batch_outcome {
            self.maybe_finish_sql_batch(tab_id, &outcome, cx);
        }
        self.flush_history_password_note(tab_id);
        cx.notify();
    }

    fn maybe_record_history_from_outcome(
        &mut self,
        tab_id: TabId,
        result: &Result<Option<StatementOutcome>, DbError>,
        cx: &mut Context<Self>,
    ) {
        let Some(view) = self.views.get(&tab_id) else {
            return;
        };
        if !view.history_should_record {
            return;
        }
        let GridSource::Sql(sql) = view.source.clone() else {
            return;
        };
        let (outcome, rows_label, error_message) = match result {
            Ok(Some(StatementOutcome::Rows(rows))) => (
                HistoryOutcome::Success,
                history_rows_label_for_rows(
                    rows.row_count_kind,
                    rows.total_rows,
                    rows.page.rows.len(),
                    rows.truncated,
                ),
                String::new(),
            ),
            Ok(Some(StatementOutcome::Executed(executed))) => (
                HistoryOutcome::Success,
                history_rows_label_affected(executed.rows_affected),
                String::new(),
            ),
            Ok(None) => return,
            Err(DbError::Cancelled { .. }) => {
                // apply_cancelled records cancelled rows.
                return;
            }
            Err(err) => (HistoryOutcome::Error, String::new(), err.to_string()),
        };
        if let Some(view) = self.view_mut(tab_id) {
            view.history_should_record = false;
        }
        self.record_finished_sql(tab_id, &sql, outcome, rows_label, error_message, cx);
    }

    pub(super) fn step_page(&mut self, delta: i64, cx: &mut Context<Self>) {
        let Some(tab_id) = self.tabs.active().map(|tab| tab.id) else {
            return;
        };
        let Some(view) = self.view_mut(tab_id) else {
            return;
        };
        if delta < 0 {
            if view.page_offset == 0 {
                return;
            }
            view.page_offset = view.page_offset.saturating_sub(PAGE_SIZE as u64);
        } else if self.can_go_next() {
            if let Some(view) = self.view_mut(tab_id) {
                view.page_offset += PAGE_SIZE as u64;
            }
        } else {
            return;
        }
        self.load_source_for(tab_id, cx);
    }

    pub(super) fn can_go_next(&self) -> bool {
        let Some(view) = self.active_view() else {
            return false;
        };
        if !view.paged {
            return false;
        }
        if let Some(total) = view.total_rows {
            if view.row_count_kind == RowCountKind::Estimated
                || view.row_count_kind == RowCountKind::Exact
            {
                return view.page_offset + (view.page_len as u64) < total;
            }
        }
        view.page_len as u32 == PAGE_SIZE
    }
}

/// Status-bar count sentence (extracted for unit tests / UX acceptance).
pub(super) fn format_status_line(
    kind: RowCountKind,
    total_rows: Option<u64>,
    page_len: usize,
    truncated: bool,
    ms: u128,
    note: Option<&str>,
) -> String {
    let rows = total_rows.unwrap_or(page_len as u64);
    let count = match kind {
        RowCountKind::Estimated => format!("~{rows} rows"),
        RowCountKind::AtLeast if truncated => format!("{rows}+ rows"),
        RowCountKind::Unknown => {
            format!("unknown rows · {page_len} on this page")
        }
        _ => format!("{rows} rows"),
    };
    let mut status = format!("{count} · {ms} ms");
    if let Some(note) = note {
        // Avoid duplicating the unknown-rows sentence when a note is set.
        if !matches!(kind, RowCountKind::Unknown) {
            status = format!("{status} · {note}");
        }
    }
    status
}

pub(super) async fn run_source(
    session: Arc<dyn DatabaseSession>,
    source: GridSource,
    offset: u64,
    sql_cap: u32,
    cancel: CancellationToken,
    filters: &[FilterCondition],
    sorts: &[SortSpec],
) -> Result<Option<StatementOutcome>, DbError> {
    match source {
        GridSource::Idle => Ok(None),
        GridSource::Table { schema, table } => preview_table(
            session.as_ref(),
            &schema,
            &table,
            PAGE_SIZE,
            offset,
            cancel,
            TablePreview { filters, sorts },
        )
        .await
        .map(Some),
        GridSource::Sql(sql) => run_user_sql(session.as_ref(), &sql, sql_cap, cancel)
            .await
            .map(Some),
    }
}

#[cfg(test)]
mod tests {
    use super::format_status_line;
    use sela_core::RowCountKind;

    #[test]
    fn unknown_filtered_status_matches_ux() {
        let s = format_status_line(RowCountKind::Unknown, None, 100, false, 18, None);
        assert_eq!(s, "unknown rows · 100 on this page · 18 ms");
    }

    #[test]
    fn unknown_does_not_append_note() {
        let s = format_status_line(
            RowCountKind::Unknown,
            None,
            50,
            false,
            3,
            Some("Filtered results…"),
        );
        assert_eq!(s, "unknown rows · 50 on this page · 3 ms");
        assert!(!s.contains("Filtered"));
    }
}
