//! Query execution, cancel, streaming, and result application (per tab).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use futures_util::future::{select, Either};
use gpui_kit::Context;
use tokio_util::sync::CancellationToken;

use sela_core::{
    classify, exact_table_rows, is_safe_to_rerun_for_load_more, normalize_sql, preview_table,
    run_user_sql, select_all, wrap_page, DatabaseSession, DbError, RowCountKind, StatementKind,
    StatementOutcome, StreamBatch, TabId, DEFAULT_SQL_ROW_CAP, PAGE_SIZE, STREAM_BATCH_SIZE,
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
        if let Some(tab) = self.tabs.get_mut(id) {
            tab.cancel_running();
        }
        if let Some(view) = self.view_mut(id) {
            view.status = "Cancelling…".into();
        }
        cx.notify();
    }

    pub(super) fn run_editor(&mut self, cx: &mut Context<Self>) {
        let Some(tab_id) = self.tabs.active().map(|tab| tab.id) else {
            return;
        };
        let sql = {
            let Some(view) = self.views.get(&tab_id) else {
                return;
            };
            view.sql.read(cx).value().to_string()
        };
        if sql.trim().is_empty() {
            if let Some(view) = self.view_mut(tab_id) {
                view.status = "Write a SQL statement, then run it.".into();
                view.error = None;
            }
            cx.notify();
            return;
        }
        if let Some(view) = self.view_mut(tab_id) {
            view.source = GridSource::Sql(sql);
            view.table_pane = super::TablePane::Data;
            view.structure_error = None;
            view.page_offset = 0;
            view.sql_row_cap = DEFAULT_SQL_ROW_CAP;
            view.truncated = false;
        }
        self.persist_tabs(cx);
        self.load_source_for(tab_id, cx);
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
        let Some(session) = self.session.clone() else {
            return;
        };
        if let Some(view) = self.view_mut(tab_id) {
            view.status = "Counting rows…".into();
        }
        self.spawn_work(
            Some(tab_id),
            cx,
            |_| async move { exact_table_rows(session.as_ref(), &schema, &table).await },
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
                    if let Some(view) = this.view_mut(tab_id) {
                        view.error = Some(err.to_string());
                        view.status = "Count failed".into();
                    }
                    cx.notify();
                }
            },
        );
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
        view.error = None;
        view.note = None;
        view.truncated = false;
        view.can_load_more = false;
        view.live_row_cap = None;
        view.status = "Running…".into();
        view.grid.update(cx, |state, cx| {
            state.delegate_mut().loading = true;
            state.delegate_mut().pending_pages.clear();
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
                        run_source(session, source, offset, sql_cap, cancel).await
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
        let Ok(sql) = normalize_sql(&sql) else {
            if let Some(view) = self.view_mut(tab_id) {
                view.error = Some("run one statement at a time".into());
                view.status = "Query failed".into();
                view.grid.update(cx, |state, cx| {
                    state.delegate_mut().loading = false;
                    state.refresh(cx);
                });
            }
            cx.notify();
            return;
        };

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
                                    &sql,
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
                                        }
                                        Err(err) if matches!(err, DbError::Cancelled { .. }) => {
                                            this.apply_cancelled(tab_id, err, cx);
                                        }
                                        Err(err) => {
                                            if let Some(view) = this.view_mut(tab_id) {
                                                view.status = "Query failed".into();
                                                view.error = Some(err.to_string());
                                                view.grid.update(cx, |state, cx| {
                                                    state.delegate_mut().loading = false;
                                                    state.refresh(cx);
                                                });
                                            }
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
                                        }
                                        Err(err) if matches!(err, DbError::Cancelled { .. }) => {
                                            this.apply_cancelled(tab_id, err, cx);
                                        }
                                        Err(err) => {
                                            if let Some(view) = this.view_mut(tab_id) {
                                                view.status = "Query failed".into();
                                                view.error = Some(err.to_string());
                                                view.grid.update(cx, |state, cx| {
                                                    state.delegate_mut().loading = false;
                                                    state.refresh(cx);
                                                });
                                            }
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
            let cancel = cancel.clone();
            let offset = (page_index as u64) * (PAGE_SIZE as u64);
            cx.spawn(async move |this, async_cx| {
                let result = drive(async move {
                    let sql = select_all(session.dialect(), &schema, &table)
                        .map_err(|err| DbError::query(err.to_string()))?;
                    let page_sql = wrap_page(session.dialect(), &sql, PAGE_SIZE, offset);
                    let (tx, rx) = async_channel::bounded(2);
                    let stream = session.stream_query(
                        &page_sql,
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
        let rows = view.total_rows.unwrap_or(view.page_len as u64);
        let count = match view.row_count_kind {
            RowCountKind::Estimated => format!("~{rows} rows"),
            RowCountKind::AtLeast if view.truncated => format!("{rows}+ rows"),
            RowCountKind::Unknown => {
                if view.paged {
                    format!("{} rows (page)", view.page_len)
                } else {
                    format!("{rows} rows")
                }
            }
            _ => format!("{rows} rows"),
        };
        view.status = format!("{count} · {ms} ms");
        if let Some(note) = &view.note {
            view.status = format!("{} · {note}", view.status);
        }
    }

    pub(super) fn apply_outcome(
        &mut self,
        tab_id: TabId,
        result: Result<Option<StatementOutcome>, DbError>,
        cx: &mut Context<Self>,
    ) {
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
                    view.status = "Query failed".into();
                    view.error = Some(err.to_string());
                    view.grid.update(cx, |state, cx| {
                        state.delegate_mut().loading = false;
                        state.refresh(cx);
                    });
                }
            }
        }
        cx.notify();
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

pub(super) async fn run_source(
    session: Arc<dyn DatabaseSession>,
    source: GridSource,
    offset: u64,
    sql_cap: u32,
    cancel: CancellationToken,
) -> Result<Option<StatementOutcome>, DbError> {
    match source {
        GridSource::Idle => Ok(None),
        GridSource::Table { schema, table } => {
            preview_table(session.as_ref(), &schema, &table, PAGE_SIZE, offset, cancel)
                .await
                .map(Some)
        }
        GridSource::Sql(sql) => run_user_sql(session.as_ref(), &sql, sql_cap, cancel)
            .await
            .map(Some),
    }
}
