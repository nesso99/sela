//! Query execution, cancel, streaming, and result application.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use futures_util::future::{select, Either};
use gpui_kit::Context;
use tokio_util::sync::CancellationToken;

use sela_core::{
    classify, exact_table_rows, normalize_sql, preview_table, run_user_sql, select_all, wrap_page,
    DatabaseSession, DbError, RowCountKind, StatementKind, StatementOutcome, StreamBatch,
    DEFAULT_SQL_ROW_CAP, PAGE_SIZE, STREAM_BATCH_SIZE,
};
use sela_drivers::drive;

use super::{GridSource, Workspace};

impl Workspace {
    pub(super) fn bump(&mut self) -> u64 {
        self.request = self.request.wrapping_add(1);
        self.request
    }

    pub(super) fn cancel_running(&mut self) {
        if let Some(token) = self.cancel.take() {
            token.cancel();
        }
    }

    pub(super) fn stop_query(&mut self, cx: &mut Context<Self>) {
        if !self.busy {
            return;
        }
        self.cancel_running();
        self.status = "Cancelling…".into();
        cx.notify();
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
        self.cancel_running();
        let token = CancellationToken::new();
        self.cancel = Some(token.clone());
        let request = self.bump();
        self.busy = true;
        self.query_started = Some(Instant::now());
        cx.notify();
        cx.spawn(async move |this, async_cx| {
            let result = drive(work).await;
            this.update(async_cx, |this, cx| {
                if this.request != request {
                    return;
                }
                this.busy = false;
                this.cancel = None;
                done(this, result, cx);
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn open_table(&mut self, schema: String, table: String, cx: &mut Context<Self>) {
        self.source = GridSource::Table { schema, table };
        self.page_offset = 0;
        self.sql_row_cap = DEFAULT_SQL_ROW_CAP;
        self.truncated = false;
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
        self.sql_row_cap = DEFAULT_SQL_ROW_CAP;
        self.truncated = false;
        self.load_source(cx);
    }

    pub(super) fn load_more(&mut self, cx: &mut Context<Self>) {
        if !self.truncated {
            return;
        }
        if let Some(cap) = &self.live_row_cap {
            let next = cap
                .load(Ordering::SeqCst)
                .saturating_add(DEFAULT_SQL_ROW_CAP as usize);
            cap.store(next, Ordering::SeqCst);
            self.sql_row_cap = next as u32;
            self.truncated = false;
            self.status = format!("Loading more (cap {next})…");
            self.busy = true;
            cx.notify();
            return;
        }
        // No live stream — re-run with a higher cap.
        self.sql_row_cap = self.sql_row_cap.saturating_add(DEFAULT_SQL_ROW_CAP);
        self.truncated = false;
        self.load_source(cx);
    }

    pub(super) fn request_exact_count(&mut self, cx: &mut Context<Self>) {
        let GridSource::Table { schema, table } = self.source.clone() else {
            return;
        };
        let Some(session) = self.session.clone() else {
            return;
        };
        self.status = "Counting rows…".into();
        self.spawn_work(
            cx,
            async move { exact_table_rows(session.as_ref(), &schema, &table).await },
            |this, result, cx| match result {
                Ok(total) => {
                    this.total_rows = Some(total);
                    this.row_count_kind = RowCountKind::Exact;
                    this.refresh_status_from_state();
                    this.grid.update(cx, |state, cx| {
                        state.delegate_mut().result.set_total_rows(total as usize);
                        state.refresh(cx);
                    });
                    cx.notify();
                }
                Err(err) => {
                    this.error = Some(err.to_string());
                    this.status = "Count failed".into();
                    cx.notify();
                }
            },
        );
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
        let sql_cap = self.sql_row_cap;
        self.error = None;
        self.note = None;
        self.truncated = false;
        self.live_row_cap = None;
        self.status = "Running…".into();
        self.grid.update(cx, |state, cx| {
            state.delegate_mut().loading = true;
            state.delegate_mut().pending_pages.clear();
            state.refresh(cx);
        });

        match &source {
            GridSource::Idle => {
                self.busy = false;
                cx.notify();
            }
            GridSource::Table { .. } => {
                // Table preview: one LIMIT/OFFSET page + estimate (may stream the page).
                let cancel = CancellationToken::new();
                self.cancel_running();
                self.cancel = Some(cancel.clone());
                let request = self.bump();
                self.busy = true;
                self.query_started = Some(Instant::now());
                cx.notify();
                cx.spawn(async move |this, async_cx| {
                    let result = drive(async move {
                        run_source(session, source, offset, sql_cap, cancel).await
                    })
                    .await;
                    this.update(async_cx, |this, cx| {
                        if this.request != request {
                            return;
                        }
                        this.busy = false;
                        this.cancel = None;
                        this.apply_outcome(result, cx);
                    })
                    .ok();
                })
                .detach();
            }
            GridSource::Sql(sql) => {
                self.start_sql_stream(session, sql.clone(), sql_cap, cx);
            }
        }
    }

    fn start_sql_stream(
        &mut self,
        session: Arc<dyn DatabaseSession>,
        sql: String,
        sql_cap: u32,
        cx: &mut Context<Self>,
    ) {
        let Ok(sql) = normalize_sql(&sql) else {
            self.error = Some("run one statement at a time".into());
            self.status = "Query failed".into();
            self.grid.update(cx, |state, cx| {
                state.delegate_mut().loading = false;
                state.refresh(cx);
            });
            cx.notify();
            return;
        };

        match classify(&sql) {
            StatementKind::Execute => {
                let cancel = CancellationToken::new();
                self.cancel_running();
                self.cancel = Some(cancel.clone());
                self.spawn_work(
                    cx,
                    async move {
                        run_user_sql(session.as_ref(), &sql, sql_cap, cancel)
                            .await
                            .map(Some)
                    },
                    |this, result, cx| this.apply_outcome(result, cx),
                );
            }
            StatementKind::PageableQuery | StatementKind::RawQuery => {
                self.cancel_running();
                let cancel = CancellationToken::new();
                self.cancel = Some(cancel.clone());
                let request = self.bump();
                self.busy = true;
                self.query_started = Some(Instant::now());
                let cap = Arc::new(AtomicUsize::new(sql_cap.max(1) as usize));
                self.live_row_cap = Some(cap.clone());
                self.row_count_kind = RowCountKind::AtLeast;
                self.total_rows = Some(0);
                self.paged = false;
                self.grid.update(cx, |state, cx| {
                    state.delegate_mut().result.reset(Vec::new(), 0);
                    state.delegate_mut().loading = true;
                    state.refresh(cx);
                });
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
                                    if this.request != request {
                                        return;
                                    }
                                    this.busy = false;
                                    this.cancel = None;
                                    this.live_row_cap = None;
                                    match outcome {
                                        Ok(summary) => {
                                            this.truncated = summary.truncated;
                                            if summary.truncated {
                                                this.row_count_kind = RowCountKind::AtLeast;
                                                this.note = Some(format!(
                                                    "Showing the first {} rows. Use Load more to continue.",
                                                    summary.rows_delivered
                                                ));
                                            } else {
                                                this.row_count_kind = RowCountKind::Exact;
                                            }
                                            this.total_rows = Some(summary.rows_delivered as u64);
                                            this.page_len = summary.rows_delivered;
                                            this.refresh_status_from_state();
                                            this.grid.update(cx, |state, cx| {
                                                state.delegate_mut().loading = false;
                                                state.delegate_mut().result.set_total_rows(
                                                    summary.rows_delivered,
                                                );
                                                state.refresh(cx);
                                            });
                                        }
                                        Err(err) if matches!(err, DbError::Cancelled { .. }) => {
                                            this.apply_cancelled(err, cx);
                                        }
                                        Err(err) => {
                                            this.status = "Query failed".into();
                                            this.error = Some(err.to_string());
                                            this.grid.update(cx, |state, cx| {
                                                state.delegate_mut().loading = false;
                                                state.refresh(cx);
                                            });
                                        }
                                    }
                                    cx.notify();
                                })
                                .ok();
                                break;
                            }
                            Either::Right((Ok(batch), _)) => {
                                this.update(async_cx, |this, cx| {
                                    if this.request != request {
                                        return;
                                    }
                                    this.apply_stream_batch(batch, cx);
                                })
                                .ok();
                            }
                            Either::Right((Err(_), _)) => {
                                // Channel closed; wait for producer.
                                let outcome = produce.await;
                                this.update(async_cx, |this, cx| {
                                    if this.request != request {
                                        return;
                                    }
                                    this.busy = false;
                                    this.cancel = None;
                                    this.live_row_cap = None;
                                    match outcome {
                                        Ok(summary) => {
                                            this.truncated = summary.truncated;
                                            this.total_rows = Some(summary.rows_delivered as u64);
                                            this.page_len = summary.rows_delivered;
                                            this.refresh_status_from_state();
                                            this.grid.update(cx, |state, cx| {
                                                state.delegate_mut().loading = false;
                                                state.refresh(cx);
                                            });
                                        }
                                        Err(err) if matches!(err, DbError::Cancelled { .. }) => {
                                            this.apply_cancelled(err, cx);
                                        }
                                        Err(err) => {
                                            this.status = "Query failed".into();
                                            this.error = Some(err.to_string());
                                            this.grid.update(cx, |state, cx| {
                                                state.delegate_mut().loading = false;
                                                state.refresh(cx);
                                            });
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

    fn apply_stream_batch(&mut self, batch: StreamBatch, cx: &mut Context<Self>) {
        if batch.paused_at_cap {
            self.truncated = true;
            self.busy = false;
            self.refresh_status_from_state();
            self.grid.update(cx, |state, cx| {
                state.delegate_mut().loading = false;
                state.refresh(cx);
            });
            cx.notify();
            return;
        }

        self.grid.update(cx, |state, cx| {
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
                        // Mid-page insert: take until page boundary.
                        let take = (page_size - offset_in_page).min(rows.len());
                        let chunk: Vec<_> = rows.drain(..take).collect();
                        // Merge into existing page if present by replacing whole page later;
                        // for streaming we always start at page boundaries from batch_size=PAGE_SIZE.
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

        let loaded = self.grid.read(cx).delegate().result.total_rows();
        self.page_len = loaded;
        self.total_rows = Some(loaded as u64);
        self.row_count_kind = RowCountKind::AtLeast;
        self.refresh_status_from_state();
        cx.notify();

        self.fetch_pending_pages(cx);
    }

    pub(super) fn fetch_pending_pages(&mut self, cx: &mut Context<Self>) {
        let GridSource::Table { schema, table } = self.source.clone() else {
            return;
        };
        let pending: Vec<usize> = self.grid.update(cx, |state, _| {
            let d = state.delegate_mut();
            let pages = d.pending_pages.clone();
            d.pending_pages.clear();
            pages
        });
        if pending.is_empty() {
            return;
        }
        let Some(session) = self.session.clone() else {
            return;
        };
        let cancel = self.cancel.clone().unwrap_or_default();
        let request = self.request;
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
                    if this.request != request {
                        return;
                    }
                    if let Ok(rows) = result {
                        this.grid.update(cx, |state, cx| {
                            state.delegate_mut().result.insert_page(page_index, rows);
                            state.refresh(cx);
                        });
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        }
    }

    fn apply_cancelled(&mut self, err: DbError, cx: &mut Context<Self>) {
        let rows = self.grid.read(cx).delegate().result.total_rows();
        self.page_len = rows;
        self.total_rows = Some(rows as u64);
        self.error = None;
        let ms = self
            .query_started
            .map(|t| t.elapsed().as_millis())
            .unwrap_or(0);
        self.status = format!("Cancelled · {rows} rows · {ms} ms · {err}");
        self.note = Some("Query cancelled".into());
        self.grid.update(cx, |state, cx| {
            state.delegate_mut().loading = false;
            state.refresh(cx);
        });
        cx.notify();
    }

    fn refresh_status_from_state(&mut self) {
        let ms = self
            .query_started
            .map(|t| t.elapsed().as_millis())
            .unwrap_or(0);
        let rows = self.total_rows.unwrap_or(self.page_len as u64);
        let count = match self.row_count_kind {
            RowCountKind::Estimated => format!("~{rows} rows"),
            RowCountKind::AtLeast if self.truncated => format!("{rows}+ rows"),
            RowCountKind::Unknown => {
                if self.paged {
                    format!("{} rows (page)", self.page_len)
                } else {
                    format!("{rows} rows")
                }
            }
            _ => format!("{rows} rows"),
        };
        self.status = format!("{count} · {ms} ms");
        if let Some(note) = &self.note {
            self.status = format!("{} · {note}", self.status);
        }
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
                self.row_count_kind = rows.row_count_kind;
                self.paged = rows.paged;
                self.truncated = rows.truncated;
                self.note = rows.note.clone();
                self.refresh_status_from_state();
                if let Some(total) = rows.total_rows {
                    // Prefer estimate for window size when table-paged.
                    let window_rows = if rows.paged {
                        total as usize
                    } else {
                        rows.page.rows.len()
                    };
                    self.grid.update(cx, |state, cx| {
                        let delegate = state.delegate_mut();
                        if rows.paged {
                            delegate
                                .result
                                .reset(rows.page.columns.clone(), window_rows);
                            let page_index = (self.page_offset / PAGE_SIZE as u64) as usize;
                            delegate.result.insert_page(page_index, rows.page.rows);
                        } else {
                            delegate.show_page(rows.page);
                        }
                        state.refresh(cx);
                    });
                } else {
                    self.grid.update(cx, |state, cx| {
                        let delegate = state.delegate_mut();
                        if rows.paged {
                            // Unknown total: show this page only, grow as pages load.
                            let start = self.page_offset as usize;
                            let end = start + rows.page.rows.len();
                            delegate
                                .result
                                .reset(rows.page.columns.clone(), end.max(start + 1));
                            let page_index = start / PAGE_SIZE as usize;
                            delegate.result.insert_page(page_index, rows.page.rows);
                            // Allow scrolling into the next page slot when the page was full.
                            if self.page_len as u32 == PAGE_SIZE {
                                delegate.result.ensure_total_rows(end + PAGE_SIZE as usize);
                            }
                        } else {
                            delegate.show_page(rows.page);
                        }
                        state.refresh(cx);
                    });
                }
                self.fetch_pending_pages(cx);
            }
            Ok(Some(StatementOutcome::Executed(executed))) => {
                self.page_len = 0;
                self.total_rows = None;
                self.paged = false;
                self.truncated = false;
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
            Err(err) if matches!(err, DbError::Cancelled { .. }) => {
                self.apply_cancelled(err, cx);
            }
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
        if !self.paged {
            return false;
        }
        if let Some(total) = self.total_rows {
            if self.row_count_kind == RowCountKind::Estimated
                || self.row_count_kind == RowCountKind::Exact
            {
                return self.page_offset + (self.page_len as u64) < total;
            }
        }
        self.page_len as u32 == PAGE_SIZE
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
