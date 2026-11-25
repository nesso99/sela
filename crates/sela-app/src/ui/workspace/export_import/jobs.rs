//! Background export / import jobs on the Workspace (progress, cancel, safe mode).

use std::fs::File;
use std::io::{BufWriter, Write};
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
    ensure_import_allowed, export_query, export_rows, qualify, quote_ident, ChangePlan, ChangeSet,
    ColumnMapping, ColumnMeta, CsvImportOptions, CsvImporter, DatabaseSession, Dialect,
    ExportError, ExportFormat, GridCommitGate, ImportError, ImportRow, ImportRowSink, PlanOp,
    PlannedStatement, RowState, SafeMode, TableRef, TableStructure, Value,
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
use super::logic::{
    export_paths_to_discard_on_failure, export_temp_path, transfer_completion_applies,
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
    pub tab_id: sela_core::TabId,
    pub generation: u64,
}

impl Workspace {
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

    pub(crate) fn start_export(&mut self, req: ExportRequest, cx: &mut Context<Self>) {
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

        let dest = req.path.clone();
        let tmp = export_temp_path(&dest);

        self.transfer_job = Some(TransferJob {
            kind: TransferKind::Export,
            cancel: cancel.clone(),
            started,
            done: done.clone(),
            total: prepared.total,
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

        let format = req.format.clone();
        let stream = prepared.stream;
        let loaded = prepared.loaded;
        let dest_for_finish = dest.clone();
        let tmp_for_finish = tmp.clone();

        cx.spawn(async move |this, async_cx| {
            // Nested Result so drive can stay typed on DbError while we keep ExportError.
            let driven = drive(async move {
                Ok::<_, sela_core::DbError>(
                    run_export(
                        session.as_ref(),
                        &dest,
                        &tmp,
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
                this.finish_export(
                    tab_id,
                    generation,
                    dest_for_finish,
                    tmp_for_finish,
                    started,
                    result,
                    cx,
                );
            })
            .ok();
        })
        .detach();
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn finish_export(
        &mut self,
        tab_id: sela_core::TabId,
        generation: u64,
        dest: PathBuf,
        tmp: PathBuf,
        started: Instant,
        result: Result<u64, ExportError>,
        cx: &mut Context<Self>,
    ) {
        let still = self.transfer_job.as_ref().is_some_and(|j| {
            transfer_completion_applies(j.generation, j.kind, generation, TransferKind::Export)
        });
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
                    view.status = export_success_status(rows, &dest, ms);
                    view.error = None;
                    view.note = None;
                }
            }
            Err(err) => {
                // Never touch the user destination — only the temp sibling.
                for path in export_paths_to_discard_on_failure(&tmp) {
                    let _ = discard_export_partial(&path);
                }
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

    pub(crate) fn start_import(
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
        let still = self.transfer_job.as_ref().is_some_and(|j| {
            transfer_completion_applies(j.generation, j.kind, generation, TransferKind::Import)
        });
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

/// Stream an export to `tmp`, then rename onto `dest` only on success.
///
/// On cancel/failure deletes `tmp` only — never the user destination.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_export(
    session: &dyn DatabaseSession,
    dest: &PathBuf,
    tmp: &PathBuf,
    format: &ExportFormat,
    stream: Option<(String, Vec<Value>)>,
    loaded: Option<(Vec<String>, Vec<Vec<Value>>)>,
    cancel: CancellationToken,
    done: Arc<AtomicU64>,
) -> Result<u64, ExportError> {
    let file = File::create(tmp).map_err(|e| ExportError::Io(e.to_string()))?;
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
            std::fs::rename(tmp, dest).map_err(|e| ExportError::Io(e.to_string()))?;
            Ok(n)
        }
        Err(err) => {
            drop(writer);
            for path in export_paths_to_discard_on_failure(tmp) {
                let _ = discard_export_partial(&path);
            }
            Err(err)
        }
    }
}

/// Stream CSV via [`CsvImporter`], applying dialog mapping (including when there
/// is no header row). Builds an in-memory [`ChangePlan`] then `apply_changes`
/// (IM-07 streaming sink is backlog for v0.3).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_import(
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

    let table_columns: Vec<ColumnMeta> = structure
        .columns
        .iter()
        .map(|c| ColumnMeta {
            name: c.name.clone(),
            type_name: c.type_name.clone(),
        })
        .collect();

    let file = File::open(path).map_err(|e| ImportError::Io(e.to_string()))?;
    let mut importer = CsvImporter::open_with_columns(file, &table_columns, options.clone())?;
    if let Some(targets) = mapping_override {
        let mapping = ColumnMapping { targets };
        for req in &options.required_columns {
            let mapped = mapping
                .targets
                .iter()
                .flatten()
                .any(|c| c.eq_ignore_ascii_case(req));
            if !mapped {
                return Err(ImportError::Mapping(
                    "Map every required column or cancel.".into(),
                ));
            }
        }
        importer = importer.with_mapping(mapping);
    }

    let plan = match ChangeSet::from_structure(&structure) {
        Ok(mut cs) => {
            drain_importer_into_sink(&mut importer, &mut cs, &cancel, &done)?;
            ChangePlan::build(&cs, dialect).map_err(|e| ImportError::Sink(e.to_string()))?
        }
        Err(_) => {
            let mut rows = Vec::new();
            let mut collector = CollectSink { rows: &mut rows };
            drain_importer_into_sink(&mut importer, &mut collector, &cancel, &done)?;
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

fn drain_importer_into_sink<R, S>(
    importer: &mut CsvImporter<R>,
    sink: &mut S,
    cancel: &CancellationToken,
    done: &Arc<AtomicU64>,
) -> Result<u64, ImportError>
where
    R: std::io::Read,
    S: ImportRowSink,
{
    let mut rows = 0u64;
    while let Some(item) = importer.next_row() {
        if cancel.is_cancelled() {
            return Err(ImportError::Cancelled);
        }
        sink.push_row(item?)?;
        rows += 1;
        done.store(rows, Ordering::Relaxed);
    }
    Ok(rows)
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

#[cfg(test)]
mod transfer_e2e {
    use super::*;
    use std::fs;
    use std::sync::atomic::AtomicU64;
    use std::sync::Arc;

    use sela_core::{
        CsvExportOptions, CsvImportOptions, Engine, ExportFormat, SavedConnection, Value,
    };
    use sela_drivers::{connect, create_sample_database};
    use tokio_util::sync::CancellationToken;

    async fn open_sample() -> (tempfile::TempDir, Box<dyn DatabaseSession>) {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("library.sqlite");
        create_sample_database(&db).await.unwrap();
        let mut conn = SavedConnection::new(Engine::Sqlite);
        conn.file_path = db.to_string_lossy().into();
        let session = connect(&conn, "").await.unwrap();
        (dir, session)
    }

    #[tokio::test]
    async fn export_success_renames_temp_onto_destination() {
        let (dir, session) = open_sample().await;
        let dest = dir.path().join("books.csv");
        let tmp = export_temp_path(&dest);
        let done = Arc::new(AtomicU64::new(0));
        let n = run_export(
            session.as_ref(),
            &dest,
            &tmp,
            &ExportFormat::Csv(CsvExportOptions::default()),
            Some(("SELECT id, title FROM books ORDER BY id".into(), vec![])),
            None,
            CancellationToken::new(),
            done,
        )
        .await
        .unwrap();
        assert!(n >= 1);
        assert!(dest.exists());
        assert!(!tmp.exists());
        let text = fs::read_to_string(&dest).unwrap();
        assert!(text.contains("title"));
        assert!(text.contains("Left Hand"));
    }

    #[tokio::test]
    async fn export_cancel_keeps_preexisting_destination_byte_identical() {
        let (dir, session) = open_sample().await;
        let dest = dir.path().join("keep.csv");
        let original = b"pre-existing destination contents";
        fs::write(&dest, original).unwrap();
        let tmp = export_temp_path(&dest);
        let cancel = CancellationToken::new();
        cancel.cancel();
        let done = Arc::new(AtomicU64::new(0));
        let err = run_export(
            session.as_ref(),
            &dest,
            &tmp,
            &ExportFormat::Csv(CsvExportOptions::default()),
            Some(("SELECT id, title FROM books ORDER BY id".into(), vec![])),
            None,
            cancel,
            done,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ExportError::Cancelled));
        assert_eq!(fs::read(&dest).unwrap(), original);
        assert!(!tmp.exists());
    }

    #[tokio::test]
    async fn export_failure_keeps_preexisting_destination_byte_identical() {
        let (dir, session) = open_sample().await;
        let dest = dir.path().join("keep-fail.csv");
        let original = b"do-not-delete-me";
        fs::write(&dest, original).unwrap();
        let tmp = export_temp_path(&dest);
        // Force a query failure so we never rename.
        let err = run_export(
            session.as_ref(),
            &dest,
            &tmp,
            &ExportFormat::Csv(CsvExportOptions::default()),
            Some(("SELECT * FROM no_such_table".into(), vec![])),
            None,
            CancellationToken::new(),
            Arc::new(AtomicU64::new(0)),
        )
        .await
        .unwrap_err();
        assert!(!matches!(err, ExportError::Cancelled));
        assert_eq!(fs::read(&dest).unwrap(), original);
        assert!(!tmp.exists());
    }

    #[tokio::test]
    async fn import_applies_mapping_without_header() {
        let (dir, session) = open_sample().await;
        // Create a tiny target table.
        session
            .execute("CREATE TABLE items (id INTEGER PRIMARY KEY, title TEXT NOT NULL)")
            .await
            .unwrap();
        let csv_path = dir.path().join("items.csv");
        // title,id order — positional would be wrong.
        fs::write(&csv_path, b"Alpha,10\nBeta,20\n").unwrap();
        let mapping = Some(vec![Some("title".into()), Some("id".into())]);
        let n = run_import(
            session.as_ref(),
            Dialect::Sqlite,
            &csv_path,
            "main",
            "items",
            CsvImportOptions {
                has_header: false,
                required_columns: vec!["id".into(), "title".into()],
                ..CsvImportOptions::default()
            },
            mapping,
            CancellationToken::new(),
            Arc::new(AtomicU64::new(0)),
        )
        .await
        .unwrap();
        assert_eq!(n, 2);
        let page = session
            .query("SELECT id, title FROM items ORDER BY id")
            .await
            .unwrap();
        assert_eq!(page.rows[0][0], Value::Int(10));
        assert_eq!(page.rows[0][1], Value::text("Alpha"));
    }

    #[tokio::test]
    async fn import_cancel_mid_transfer_leaves_table_unchanged() {
        let (dir, session) = open_sample().await;
        session
            .execute("CREATE TABLE items2 (id INTEGER PRIMARY KEY, title TEXT NOT NULL)")
            .await
            .unwrap();
        let csv_path = dir.path().join("items2.csv");
        let mut body = String::from("id,title\n");
        for i in 0..500 {
            body.push_str(&format!("{i},row{i}\n"));
        }
        fs::write(&csv_path, body).unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let err = run_import(
            session.as_ref(),
            Dialect::Sqlite,
            &csv_path,
            "main",
            "items2",
            CsvImportOptions {
                has_header: true,
                required_columns: vec!["id".into(), "title".into()],
                ..CsvImportOptions::default()
            },
            None,
            cancel,
            Arc::new(AtomicU64::new(0)),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ImportError::Cancelled));
        let page = session.query("SELECT count(*) FROM items2").await.unwrap();
        assert_eq!(page.rows[0][0].display(), "0");
    }
}
