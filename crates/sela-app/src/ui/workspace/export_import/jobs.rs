//! Background export / import jobs on the Workspace (progress, cancel, safe mode).

use std::fs::File;
use std::io::{BufWriter, Cursor, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::WindowExt as _;
use gpui_kit::prelude::*;
use gpui_kit::{px, Context, SharedString, Window};
use tokio_util::sync::CancellationToken;

use sela_core::{
    bind_placeholder, build_table_select, decide_import_gate, discard_export_partial,
    ensure_import_allowed, export_query, export_rows, import_csv_into, qualify, quote_ident,
    ChangePlan, ChangeSet, ColumnMeta, CsvImportOptions, DatabaseSession, Dialect, ExportError,
    ExportFormat, GridCommitGate, ImportError, ImportRow, ImportRowSink, PlanOp, PlannedStatement,
    RowState, SafeMode, StaticTableSchema, TableRef, TableStructure, Value,
    IMPORT_REFUSED_READ_ONLY_STATUS,
};
use sela_drivers::drive;

use super::super::structure::structure_key;
use super::super::{GridSource, TablePane, Workspace};
use super::export_dialog::{
    ExportDialog, ExportDialogContext, ExportDialogEvent, ExportRequest, ExportScope,
};
use super::import_dialog::{
    required_columns, ImportDialog, ImportDialogContext, ImportDialogEvent, ImportRequest,
};
use super::messages::{
    export_capped_note, export_progress_status, export_success_status, format_export_failure,
    format_import_failure, import_confirm_title, import_discard_pending_body,
    import_discard_pending_title, import_progress_status, import_success_status,
    EXPORT_ALREADY_RUNNING, IMPORT_ALREADY_RUNNING, WHOLE_TABLE_DISABLED_REASON,
};

/// Kind of background transfer for status-bar Cancel (EX-06 / IM-04).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferKind {
    Export,
    Import,
}

/// Workspace-level export/import job (one at a time per window).
pub struct TransferJob {
    pub kind: TransferKind,
    pub cancel: CancellationToken,
    pub started: Instant,
    pub done: Arc<AtomicU64>,
    pub total: Option<u64>,
    #[allow(dead_code)] // retained for status / discard paths
    pub path: Option<PathBuf>,
    pub tab_id: sela_core::TabId,
    pub generation: u64,
}

impl Workspace {
    #[allow(dead_code)]
    pub(crate) fn transfer_busy(&self) -> bool {
        self.transfer_job.is_some()
    }

    /// Open the Export dialog for the active result / table (UX 11).
    pub(crate) fn open_export_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.transfer_job.is_some() {
            self.set_active_status(EXPORT_ALREADY_RUNNING, cx);
            return;
        }
        let Some(view) = self.active_view() else {
            return;
        };
        let dialect = self.active_dialect();
        let (table_data, schema, table, sql_result_tab) = match &view.source {
            GridSource::Table { schema, table, .. } if view.table_pane == TablePane::Data => {
                (true, Some(schema.clone()), Some(table.clone()), false)
            }
            GridSource::Sql(_) => (false, None, None, true),
            GridSource::Table { schema, table, .. } => {
                (false, Some(schema.clone()), Some(table.clone()), false)
            }
            GridSource::Idle => return,
        };
        let ctx = ExportDialogContext {
            table_data,
            schema,
            table,
            dialect,
            sql_result_tab,
        };
        let form = cx.new(|cx| ExportDialog::new(ctx, window, cx));
        cx.subscribe_in(&form, window, {
            move |workspace, _form, event, window, cx| match event {
                ExportDialogEvent::Cancel => {
                    window.close_dialog(cx);
                    workspace.export_dialog = None;
                }
                ExportDialogEvent::Export(req) => {
                    let req = req.clone();
                    window.close_dialog(cx);
                    workspace.export_dialog = None;
                    workspace.start_export(req, cx);
                }
            }
        })
        .detach();
        let form_for_dialog = form.clone();
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title(SharedString::from("Export"))
                .width(px(560.))
                .on_ok(|_, _, _| false)
                .child(form_for_dialog.clone())
        });
        self.export_dialog = Some(form);
        cx.notify();
    }

    /// Open Import CSV for the active table Data tab.
    pub(crate) fn open_import_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.transfer_job.is_some() {
            self.set_active_status(IMPORT_ALREADY_RUNNING, cx);
            return;
        }
        let mode = self.active_safe_mode();
        if mode == SafeMode::ReadOnly {
            if let Some(view) = self.active_view_mut() {
                view.status = IMPORT_REFUSED_READ_ONLY_STATUS.into();
                view.error = Some(IMPORT_REFUSED_READ_ONLY_STATUS.into());
            }
            cx.notify();
            return;
        }
        let Some(view) = self.active_view() else {
            return;
        };
        let GridSource::Table { schema, table, .. } = &view.source else {
            self.set_active_status("Open a table Data tab to import CSV.", cx);
            return;
        };
        if view.table_pane != TablePane::Data {
            self.set_active_status("Switch to the Data pane to import CSV.", cx);
            return;
        }
        let schema = schema.clone();
        let table = table.clone();
        let key = structure_key(&schema, &table);
        let structure = view.structure_cache.get(&key).cloned();
        if let Some(structure) = structure {
            self.show_import_dialog(structure, window, cx);
            return;
        }
        self.fetch_structure_then_import(schema, table, window, cx);
    }

    fn set_active_status(&mut self, status: &str, cx: &mut Context<Self>) {
        if let Some(view) = self.active_view_mut() {
            view.status = status.into();
        } else {
            self.workspace_status = Some(status.into());
        }
        cx.notify();
    }

    fn fetch_structure_then_import(
        &mut self,
        schema: String,
        table: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.session.clone() else {
            return;
        };
        let Some(tab_id) = self.tabs.active().map(|t| t.id) else {
            return;
        };
        let window_handle = window.window_handle();
        let entity = cx.entity();
        cx.spawn(async move |this, async_cx| {
            let result = drive(async move { session.describe_table(&schema, &table).await }).await;
            match result {
                Ok(structure) => {
                    this.update(async_cx, |this, cx| {
                        let key = structure_key(&structure.schema, &structure.table);
                        if let Some(view) = this.view_mut(tab_id) {
                            view.structure_cache.insert(key, structure.clone());
                        }
                        cx.notify();
                        structure
                    })
                    .ok()
                    .and_then(|structure| {
                        window_handle
                            .update(async_cx, |_, window, app| {
                                entity.update(app, |this, cx| {
                                    this.show_import_dialog(structure, window, cx);
                                });
                            })
                            .ok()
                    });
                }
                Err(err) => {
                    this.update(async_cx, |this, cx| {
                        if let Some(view) = this.view_mut(tab_id) {
                            view.error = Some(format!("Import failed: {err}"));
                            view.status = "Import failed.".into();
                        }
                        cx.notify();
                    })
                    .ok();
                }
            }
        })
        .detach();
    }

    fn show_import_dialog(
        &mut self,
        structure: TableStructure,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mode = self.active_safe_mode();
        let ctx = ImportDialogContext {
            schema: structure.schema.clone(),
            table: structure.table.clone(),
            structure,
            read_only: mode == SafeMode::ReadOnly,
        };
        let form = cx.new(|cx| ImportDialog::new(ctx, window, cx));
        cx.subscribe_in(&form, window, {
            move |workspace, form, event, window, cx| match event {
                ImportDialogEvent::Cancel => {
                    window.close_dialog(cx);
                    workspace.import_dialog = None;
                }
                ImportDialogEvent::Ready(req) => {
                    let mapping = form.read(cx).take_mapping_targets();
                    let req = req.clone();
                    window.close_dialog(cx);
                    workspace.import_dialog = None;
                    workspace.begin_import_gated(req, mapping, window, cx);
                }
            }
        })
        .detach();
        let form_for_dialog = form.clone();
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title(SharedString::from("Import CSV"))
                .width(px(560.))
                .on_ok(|_, _, _| false)
                .child(form_for_dialog.clone())
        });
        self.import_dialog = Some(form);
        cx.notify();
    }

    fn begin_import_gated(
        &mut self,
        req: ImportRequest,
        mapping: Option<Vec<Option<String>>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pending = self
            .active_view()
            .and_then(|v| v.edit.as_ref())
            .map(|e| e.pending_counts().total())
            .unwrap_or(0);
        if pending > 0 {
            self.show_import_discard_pending(req, mapping, pending, window, cx);
            return;
        }
        self.gate_import_safe_mode(req, mapping, window, cx);
    }

    fn show_import_discard_pending(
        &mut self,
        req: ImportRequest,
        mapping: Option<Vec<Option<String>>>,
        pending: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.import_confirm_open {
            return;
        }
        self.import_confirm_open = true;
        let title = import_discard_pending_title().to_string();
        let body = import_discard_pending_body(pending, &req.schema, &req.table);
        let this = cx.entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let this_cancel = this.clone();
            let this_ok = this.clone();
            let req = req.clone();
            let mapping = mapping.clone();
            alert
                .title(title.clone())
                .description(body.clone())
                .on_ok(move |_, _, cx| {
                    this_ok.update(cx, |ws, _| ws.import_confirm_open = false);
                    true
                })
                .on_cancel(move |_, _, cx| {
                    this_cancel.update(cx, |ws, _| ws.import_confirm_open = false);
                    true
                })
                .footer(
                    DialogFooter::new()
                        .child(
                            Button::new("import-discard-cancel")
                                .label("Cancel")
                                .primary()
                                .on_click({
                                    let this = this.clone();
                                    move |_, window, cx| {
                                        this.update(cx, |ws, _| ws.import_confirm_open = false);
                                        window.close_dialog(cx);
                                    }
                                }),
                        )
                        .child(
                            Button::new("import-discard-continue")
                                .label("Discard and continue")
                                .on_click({
                                    let this = this.clone();
                                    let req = req.clone();
                                    let mapping = mapping.clone();
                                    move |_, window, cx| {
                                        window.close_dialog(cx);
                                        this.update(cx, |ws, cx| {
                                            ws.import_confirm_open = false;
                                            if let Some(view) = ws.active_view_mut() {
                                                if let Some(edit) = view.edit.as_mut() {
                                                    edit.discard();
                                                }
                                            }
                                            ws.gate_import_safe_mode(
                                                req.clone(),
                                                mapping.clone(),
                                                window,
                                                cx,
                                            );
                                        });
                                    }
                                }),
                        ),
                )
        });
    }

    fn gate_import_safe_mode(
        &mut self,
        req: ImportRequest,
        mapping: Option<Vec<Option<String>>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mode = self.active_safe_mode();
        match decide_import_gate(mode) {
            GridCommitGate::RefuseReadOnly => {
                if let Some(view) = self.active_view_mut() {
                    view.status = IMPORT_REFUSED_READ_ONLY_STATUS.into();
                    view.error = Some(IMPORT_REFUSED_READ_ONLY_STATUS.into());
                }
                cx.notify();
            }
            GridCommitGate::Allow => {
                let _ = ensure_import_allowed(mode, true);
                self.start_import(req, mapping, cx);
            }
            GridCommitGate::Confirm => {
                self.show_import_confirm(req, mapping, window, cx);
            }
        }
    }

    fn show_import_confirm(
        &mut self,
        req: ImportRequest,
        mapping: Option<Vec<Option<String>>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.import_confirm_open {
            return;
        }
        self.import_confirm_open = true;
        let title = import_confirm_title(req.total_rows, &req.schema, &req.table);
        let file_name = req
            .path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("CSV file");
        let body = format!("File: {file_name}");
        let this = cx.entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let this_cancel = this.clone();
            let this_ok = this.clone();
            let req = req.clone();
            let mapping = mapping.clone();
            alert
                .title(title.clone())
                .description(body.clone())
                .on_ok(move |_, _, cx| {
                    this_ok.update(cx, |ws, _| ws.import_confirm_open = false);
                    true
                })
                .on_cancel(move |_, _, cx| {
                    this_cancel.update(cx, |ws, _| ws.import_confirm_open = false);
                    true
                })
                .footer(
                    DialogFooter::new()
                        .child(
                            Button::new("import-confirm-cancel")
                                .label("Cancel")
                                .primary()
                                .on_click({
                                    let this = this.clone();
                                    move |_, window, cx| {
                                        this.update(cx, |ws, _| ws.import_confirm_open = false);
                                        window.close_dialog(cx);
                                    }
                                }),
                        )
                        .child(
                            Button::new("import-confirm-import")
                                .label("Import")
                                .on_click({
                                    let this = this.clone();
                                    let req = req.clone();
                                    let mapping = mapping.clone();
                                    move |_, window, cx| {
                                        window.close_dialog(cx);
                                        this.update(cx, |ws, cx| {
                                            ws.import_confirm_open = false;
                                            ws.start_import(req.clone(), mapping.clone(), cx);
                                        });
                                    }
                                }),
                        ),
                )
        });
    }

    fn start_export(&mut self, req: ExportRequest, cx: &mut Context<Self>) {
        if self.transfer_job.is_some() {
            self.set_active_status(EXPORT_ALREADY_RUNNING, cx);
            return;
        }
        let Some(tab_id) = self.tabs.active().map(|t| t.id) else {
            return;
        };
        let Some(session) = self.session.clone() else {
            return;
        };
        let dialect = session.dialect();
        let Some(view) = self.views.get(&tab_id) else {
            return;
        };

        let prepared = match prepare_export_source(view, &req, dialect, cx) {
            Ok(v) => v,
            Err(msg) => {
                self.set_active_status(&msg, cx);
                return;
            }
        };

        let cancel = CancellationToken::new();
        let done = Arc::new(AtomicU64::new(0));
        let started = Instant::now();
        self.transfer_generation = self.transfer_generation.wrapping_add(1);
        let generation = self.transfer_generation;

        if let Some(note) = prepared.note {
            if let Some(view) = self.view_mut(tab_id) {
                view.note = Some(note);
            }
        }

        self.transfer_job = Some(TransferJob {
            kind: TransferKind::Export,
            cancel: cancel.clone(),
            started,
            done: done.clone(),
            total: prepared.total,
            path: Some(req.path.clone()),
            tab_id,
            generation,
        });
        if let Some(tab) = self.tabs.get_mut(tab_id) {
            tab.set_cancel(cancel.clone());
        }
        if let Some(view) = self.view_mut(tab_id) {
            view.busy = true;
            view.query_started = Some(started);
            view.status = export_progress_status(0, prepared.total, Duration::ZERO);
            view.error = None;
        }
        self.spawn_transfer_status_tick(cx);
        cx.notify();

        let path = req.path.clone();
        let format = req.format.clone();
        let path_for_discard = path.clone();
        let stream = prepared.stream;
        let loaded = prepared.loaded;

        cx.spawn(async move |this, async_cx| {
            // Nested Result so drive can stay typed on DbError while we keep ExportError.
            let driven = drive(async move {
                Ok::<_, sela_core::DbError>(
                    run_export(
                        session.as_ref(),
                        &path,
                        &format,
                        stream,
                        loaded,
                        cancel,
                        done,
                    )
                    .await,
                )
            })
            .await;
            let result = match driven {
                Ok(inner) => inner,
                Err(err) => Err(ExportError::Query(err.to_string())),
            };

            this.update(async_cx, |this, cx| {
                this.finish_export(tab_id, generation, path_for_discard, started, result, cx);
            })
            .ok();
        })
        .detach();
    }

    fn finish_export(
        &mut self,
        tab_id: sela_core::TabId,
        generation: u64,
        path: PathBuf,
        started: Instant,
        result: Result<u64, ExportError>,
        cx: &mut Context<Self>,
    ) {
        let still = self
            .transfer_job
            .as_ref()
            .is_some_and(|j| j.generation == generation && j.kind == TransferKind::Export);
        if !still {
            return;
        }
        self.transfer_job = None;
        if let Some(tab) = self.tabs.get_mut(tab_id) {
            tab.clear_cancel();
        }
        if let Some(view) = self.view_mut(tab_id) {
            view.busy = false;
        }
        match result {
            Ok(rows) => {
                let ms = started.elapsed().as_millis();
                if let Some(view) = self.view_mut(tab_id) {
                    view.status = export_success_status(rows, &path, ms);
                    view.error = None;
                    view.note = None;
                }
            }
            Err(err) => {
                let _ = discard_export_partial(&path);
                let (banner, status) = format_export_failure(&err);
                if let Some(view) = self.view_mut(tab_id) {
                    view.status = status;
                    if matches!(err, ExportError::Cancelled) {
                        view.error = None;
                    } else {
                        view.error = Some(banner);
                    }
                }
            }
        }
        cx.notify();
    }

    fn start_import(
        &mut self,
        req: ImportRequest,
        mapping: Option<Vec<Option<String>>>,
        cx: &mut Context<Self>,
    ) {
        if self.transfer_job.is_some() {
            self.set_active_status(IMPORT_ALREADY_RUNNING, cx);
            return;
        }
        let Some(tab_id) = self.tabs.active().map(|t| t.id) else {
            return;
        };
        let Some(session) = self.session.clone() else {
            return;
        };
        let dialect = session.dialect();
        let cancel = CancellationToken::new();
        let done = Arc::new(AtomicU64::new(0));
        let started = Instant::now();
        self.transfer_generation = self.transfer_generation.wrapping_add(1);
        let generation = self.transfer_generation;
        let total = req.total_rows;

        self.transfer_job = Some(TransferJob {
            kind: TransferKind::Import,
            cancel: cancel.clone(),
            started,
            done: done.clone(),
            total: Some(total),
            path: Some(req.path.clone()),
            tab_id,
            generation,
        });
        if let Some(tab) = self.tabs.get_mut(tab_id) {
            tab.set_cancel(cancel.clone());
        }
        if let Some(view) = self.view_mut(tab_id) {
            view.busy = true;
            view.query_started = Some(started);
            view.status = import_progress_status(0, total, Duration::ZERO);
            view.error = None;
        }
        self.spawn_transfer_status_tick(cx);
        cx.notify();

        let schema = req.schema.clone();
        let table = req.table.clone();
        let options = req.options.clone();
        let path = req.path.clone();

        cx.spawn(async move |this, async_cx| {
            let driven = drive(async move {
                Ok::<_, sela_core::DbError>(
                    run_import(
                        session.as_ref(),
                        dialect,
                        &path,
                        &schema,
                        &table,
                        options,
                        mapping,
                        cancel,
                        done,
                    )
                    .await,
                )
            })
            .await;
            let result = match driven {
                Ok(inner) => inner,
                Err(err) => Err(ImportError::Sink(err.to_string())),
            };

            this.update(async_cx, |this, cx| {
                this.finish_import(tab_id, generation, started, result, cx);
            })
            .ok();
        })
        .detach();
    }

    fn finish_import(
        &mut self,
        tab_id: sela_core::TabId,
        generation: u64,
        started: Instant,
        result: Result<u64, ImportError>,
        cx: &mut Context<Self>,
    ) {
        let still = self
            .transfer_job
            .as_ref()
            .is_some_and(|j| j.generation == generation && j.kind == TransferKind::Import);
        if !still {
            return;
        }
        self.transfer_job = None;
        if let Some(tab) = self.tabs.get_mut(tab_id) {
            tab.clear_cancel();
        }
        if let Some(view) = self.view_mut(tab_id) {
            view.busy = false;
        }
        match result {
            Ok(rows) => {
                let ms = started.elapsed().as_millis();
                if let Some(view) = self.view_mut(tab_id) {
                    view.status = import_success_status(rows, ms);
                    view.error = None;
                }
                self.reload_table_page_after_commit(tab_id, cx);
            }
            Err(err) => {
                let (banner, status) = format_import_failure(&err);
                if let Some(view) = self.view_mut(tab_id) {
                    view.status = status;
                    if matches!(err, ImportError::Cancelled) {
                        view.error = None;
                    } else {
                        view.error = Some(banner);
                    }
                }
            }
        }
        cx.notify();
    }

    fn spawn_transfer_status_tick(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, async_cx| loop {
            async_cx
                .background_executor()
                .timer(Duration::from_millis(250))
                .await;
            let cont = this
                .update(async_cx, |this, cx| {
                    let Some(job) = this.transfer_job.as_ref() else {
                        return false;
                    };
                    let done = job.done.load(Ordering::Relaxed);
                    let elapsed = job.started.elapsed();
                    let status = match job.kind {
                        TransferKind::Export => export_progress_status(done, job.total, elapsed),
                        TransferKind::Import => {
                            let total = job.total.unwrap_or(0);
                            import_progress_status(done, total, elapsed)
                        }
                    };
                    let tab_id = job.tab_id;
                    if let Some(view) = this.view_mut(tab_id) {
                        if view.busy {
                            view.status = status;
                        }
                    }
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

    /// Cancel the active export/import (Stop / secondary-. / status Cancel).
    pub(crate) fn cancel_transfer_if_active(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(job) = self.transfer_job.as_ref() else {
            return false;
        };
        job.cancel.cancel();
        if let Some(view) = self.view_mut(job.tab_id) {
            view.status = "Cancelling…".into();
        }
        cx.notify();
        true
    }
}

struct PreparedExport {
    stream: Option<(String, Vec<Value>)>,
    loaded: Option<(Vec<String>, Vec<Vec<Value>>)>,
    note: Option<String>,
    total: Option<u64>,
}

fn prepare_export_source(
    view: &super::super::tab_view::TabView,
    req: &ExportRequest,
    dialect: Dialect,
    cx: &Context<Workspace>,
) -> Result<PreparedExport, String> {
    match (&view.source, req.scope) {
        (
            GridSource::Table { schema, table, .. },
            ExportScope::CurrentResult | ExportScope::WholeTable,
        ) if view.table_pane == TablePane::Data || matches!(req.scope, ExportScope::WholeTable) => {
            if matches!(req.scope, ExportScope::WholeTable)
                && view.table_pane != TablePane::Data
                && !matches!(view.source, GridSource::Table { .. })
            {
                return Err(WHOLE_TABLE_DISABLED_REASON.into());
            }
            // Whole table on a non-Data pane with a table source: still stream.
            let bound = build_table_select(dialect, schema, table, &view.filters, &view.sorts)
                .map_err(|e| e.to_string())?;
            Ok(PreparedExport {
                stream: Some((bound.sql, bound.params)),
                loaded: None,
                note: None,
                total: view.total_rows,
            })
        }
        (_, ExportScope::WholeTable) => Err(WHOLE_TABLE_DISABLED_REASON.into()),
        (_, ExportScope::CurrentResult) => {
            let state = view.grid.read(cx);
            let result = &state.delegate().result;
            let columns: Vec<String> = result.columns().iter().map(|c| c.name.clone()).collect();
            let mut rows = Vec::new();
            for i in 0..result.total_rows() {
                match result.row(i) {
                    RowState::Ready(cells) => rows.push(cells.to_vec()),
                    RowState::Loading => break,
                }
            }
            let n = rows.len() as u64;
            let note = if view.truncated || view.can_load_more {
                Some(export_capped_note(n.max(u64::from(view.sql_row_cap))))
            } else {
                None
            };
            Ok(PreparedExport {
                stream: None,
                loaded: Some((columns, rows)),
                note,
                total: Some(n),
            })
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_export(
    session: &dyn DatabaseSession,
    path: &PathBuf,
    format: &ExportFormat,
    stream: Option<(String, Vec<Value>)>,
    loaded: Option<(Vec<String>, Vec<Vec<Value>>)>,
    cancel: CancellationToken,
    done: Arc<AtomicU64>,
) -> Result<u64, ExportError> {
    let tmp = path.with_extension(format!(
        "{}.sela-tmp",
        path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("export")
    ));
    let file = File::create(&tmp).map_err(|e| ExportError::Io(e.to_string()))?;
    let mut writer = BufWriter::new(file);
    let progress = |n: u64| {
        done.store(n, Ordering::Relaxed);
    };
    let result = if let Some((sql, params)) = stream {
        export_query(
            session,
            &sql,
            &params,
            &mut writer,
            format,
            cancel,
            progress,
        )
        .await
        .map(|(n, _)| n)
    } else if let Some((columns, rows)) = loaded {
        export_rows(&mut writer, &columns, rows, format, &cancel, progress)
    } else {
        Err(ExportError::Io("nothing to export".into()))
    };
    match result {
        Ok(n) => {
            writer.flush().map_err(|e| ExportError::Io(e.to_string()))?;
            drop(writer);
            std::fs::rename(&tmp, path).map_err(|e| ExportError::Io(e.to_string()))?;
            Ok(n)
        }
        Err(err) => {
            drop(writer);
            let _ = discard_export_partial(&tmp);
            let _ = discard_export_partial(path);
            Err(err)
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_import(
    session: &dyn DatabaseSession,
    dialect: Dialect,
    path: &PathBuf,
    schema: &str,
    table: &str,
    mut options: CsvImportOptions,
    mapping_override: Option<Vec<Option<String>>>,
    cancel: CancellationToken,
    done: Arc<AtomicU64>,
) -> Result<u64, ImportError> {
    let structure = session
        .describe_table(schema, table)
        .await
        .map_err(|e| ImportError::Schema(e.to_string()))?;
    options.required_columns = required_columns(&structure);

    let bytes = std::fs::read(path).map_err(|e| ImportError::Io(e.to_string()))?;
    sela_core::require_utf8(&bytes)?;

    let table_columns: Vec<ColumnMeta> = structure
        .columns
        .iter()
        .map(|c| ColumnMeta {
            name: c.name.clone(),
            type_name: c.type_name.clone(),
        })
        .collect();

    // When the user remapped columns, rewrite headers so import_csv_into picks
    // them up (Skip columns become unique ignored names).
    let source_bytes = if let Some(targets) = mapping_override.as_ref() {
        rewrite_csv_headers(&bytes, options.delimiter, options.has_header, targets)?
    } else {
        bytes
    };

    let mut schema_source = StaticTableSchema::new();
    schema_source.insert(schema, table, table_columns);

    let plan = match ChangeSet::from_structure(&structure) {
        Ok(mut cs) => {
            import_csv_into(
                Cursor::new(source_bytes.as_slice()),
                &schema_source,
                schema,
                table,
                options,
                &mut cs,
                &cancel,
                |n| done.store(n, Ordering::Relaxed),
            )?;
            ChangePlan::build(&cs, dialect).map_err(|e| ImportError::Sink(e.to_string()))?
        }
        Err(_) => {
            let mut rows = Vec::new();
            let mut collector = CollectSink { rows: &mut rows };
            import_csv_into(
                Cursor::new(source_bytes.as_slice()),
                &schema_source,
                schema,
                table,
                options,
                &mut collector,
                &cancel,
                |n| done.store(n, Ordering::Relaxed),
            )?;
            build_insert_only_plan(dialect, schema, table, &rows)?
        }
    };

    if cancel.is_cancelled() {
        return Err(ImportError::Cancelled);
    }

    let row_count = done.load(Ordering::Relaxed);
    session.apply_changes(&plan, cancel).await.map_err(|e| {
        if matches!(e, sela_core::DbError::Cancelled { .. }) {
            ImportError::Cancelled
        } else {
            ImportError::Sink(e.to_string())
        }
    })?;
    Ok(row_count.max(plan.statements.len() as u64))
}

struct CollectSink<'a> {
    rows: &'a mut Vec<ImportRow>,
}

impl ImportRowSink for CollectSink<'_> {
    fn push_row(&mut self, row: ImportRow) -> Result<(), ImportError> {
        self.rows.push(row);
        Ok(())
    }
}

/// Rewrite the header line so auto-map follows the dialog mapping (IM-03).
fn rewrite_csv_headers(
    bytes: &[u8],
    delimiter: u8,
    has_header: bool,
    targets: &[Option<String>],
) -> Result<Vec<u8>, ImportError> {
    if !has_header {
        return Ok(bytes.to_vec());
    }
    let text = sela_core::require_utf8(bytes)?;
    let (first_line, rest) = match text.find('\n') {
        Some(i) => (&text[..=i], &text[i + 1..]),
        None => (text, ""),
    };
    let delim = delimiter as char;
    let mut new_headers = Vec::with_capacity(targets.len());
    for (i, target) in targets.iter().enumerate() {
        match target {
            Some(name) => new_headers.push(escape_header(name, delim)),
            None => new_headers.push(format!("__sela_skip_{i}")),
        }
    }
    let mut out = new_headers.join(&delim.to_string());
    out.push('\n');
    out.push_str(rest.trim_start_matches('\r'));
    let _ = first_line;
    Ok(out.into_bytes())
}

fn escape_header(name: &str, delim: char) -> String {
    if name.contains(delim) || name.contains('"') || name.contains('\n') {
        format!("\"{}\"", name.replace('"', "\"\""))
    } else {
        name.to_string()
    }
}

fn build_insert_only_plan(
    dialect: Dialect,
    schema: &str,
    table: &str,
    rows: &[ImportRow],
) -> Result<ChangePlan, ImportError> {
    let table_ref = TableRef::new(schema, table);
    let mut statements = Vec::with_capacity(rows.len());
    for row in rows {
        if row.values.is_empty() {
            continue;
        }
        let cols: Vec<_> = row.values.keys().cloned().collect();
        let params: Vec<_> = cols.iter().map(|c| row.values[c].clone()).collect();
        let mut sql = format!(
            "INSERT INTO {} (",
            qualify(dialect, schema, table).map_err(|e| ImportError::Sink(e.to_string()))?
        );
        for (i, col) in cols.iter().enumerate() {
            if i > 0 {
                sql.push_str(", ");
            }
            sql.push_str(&quote_ident(dialect, col).map_err(|e| ImportError::Sink(e.to_string()))?);
        }
        sql.push_str(") VALUES (");
        for i in 0..cols.len() {
            if i > 0 {
                sql.push_str(", ");
            }
            sql.push_str(&bind_placeholder(dialect, i));
        }
        sql.push(')');
        statements.push(PlannedStatement {
            op: PlanOp::Insert,
            sql,
            params,
            pk: Vec::new(),
            unchecked_optimistic: Vec::new(),
        });
    }
    Ok(ChangePlan {
        dialect,
        table: table_ref,
        statements,
    })
}
