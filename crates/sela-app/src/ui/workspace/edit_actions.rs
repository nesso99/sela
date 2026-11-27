//! Grid edit / preview / commit actions and structure-backed EditSession setup.

use std::collections::BTreeMap;

use gpui_kit::component::table::TableSelection;
use gpui_kit::{Context, Focusable as _, Window};

use sela_core::{RelationKind, RowState, TabId, TableStructure, Value};

use super::edit_session::{
    resolve_overlay_anchors, visual_row_map, visual_row_map_for_overlays, EditRowId, EditSession,
    OverlayRow, PendingIndex, ReadOnlyReason, RowMarker, VisualRowRef,
};
use super::tab_view::CellInspect;
use super::{GridSource, Workspace};
use crate::ui::grid::{EditPaintRow, EditPaintState, OverlayPaintRow};

/// Visual order for the current page, resolving duplicate anchors by source PK.
fn visual_rows_for(
    result: &sela_core::ResultWindow,
    overlays: &[OverlayRow],
    pk_cols: &[String],
) -> Vec<VisualRowRef> {
    let server_count = result.total_rows();
    if overlays.is_empty() {
        return visual_row_map(server_count, &[]);
    }
    let page_pks: Vec<_> = (0..server_count)
        .map(|i| extract_pk(result, i, pk_cols))
        .collect();
    visual_row_map_for_overlays(server_count, overlays, &page_pks)
}

impl Workspace {
    /// Relation kind from the loaded catalog; `None` when not (yet) known.
    pub(super) fn relation_kind_for(&self, schema: &str, table: &str) -> Option<RelationKind> {
        catalog_kind(&self.catalog, schema, table)
    }

    pub(super) fn ensure_edit_session_loading(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        let dialect = self
            .session
            .as_ref()
            .map(|s| s.dialect())
            .unwrap_or(sela_core::Dialect::Sqlite);
        let Some(view) = self.views.get_mut(&tab_id) else {
            return;
        };
        // Restored tabs carry no kind; resolve from the catalog when known.
        let resolved = match &view.source {
            GridSource::Table {
                schema,
                table,
                kind,
            } => kind.or_else(|| catalog_kind(&self.catalog, schema, table)),
            _ => None,
        };
        match &mut view.source {
            GridSource::Table {
                schema,
                table,
                kind,
            } => {
                *kind = resolved;
                if view.edit.is_none() {
                    view.edit = Some(match resolved {
                        Some(kind) => EditSession::loading(dialect, kind),
                        None => EditSession::unresolved_kind(dialect),
                    });
                }
                let schema = schema.clone();
                let table = table.clone();
                self.ensure_structure_for_edit(tab_id, schema, table, cx);
            }
            GridSource::Sql(_) => {
                view.edit = Some(EditSession::sql_result());
            }
            GridSource::Idle => {
                view.edit = None;
            }
        }
    }

    fn ensure_structure_for_edit(
        &mut self,
        tab_id: TabId,
        schema: String,
        table: String,
        cx: &mut Context<Self>,
    ) {
        let key = format!("{schema}\0{table}");
        if let Some(view) = self.views.get(&tab_id) {
            if let Some(structure) = view.structure_cache.get(&key).cloned() {
                self.apply_structure_to_edit(tab_id, structure, cx);
                return;
            }
        }
        // Soft load: do not cancel an in-flight table page query.
        let Some(session) = self.session.clone() else {
            return;
        };
        let dialect = session.dialect();
        let expected_schema = schema.clone();
        let expected_table = table.clone();
        let kind = self.relation_kind_for(&schema, &table);
        cx.spawn(async move |this, async_cx| {
            let result = sela_drivers::drive(async move {
                sela_drivers::retry_transient(STRUCTURE_ATTEMPTS, STRUCTURE_RETRY_DELAY, || {
                    session.describe_table(&schema, &table)
                })
                .await
            })
            .await;
            this.update(async_cx, |this, cx| {
                let still = this.views.get(&tab_id).is_some_and(|view| {
                    matches!(
                        &view.source,
                        GridSource::Table { schema, table, .. }
                            if schema == &expected_schema && table == &expected_table
                    )
                });
                if !still {
                    return;
                }
                match result {
                    Ok(structure) => {
                        let key = format!("{}\0{}", structure.schema, structure.table);
                        if let Some(view) = this.view_mut(tab_id) {
                            view.structure_cache.insert(key, structure.clone());
                        }
                        this.apply_structure_to_edit(tab_id, structure, cx);
                    }
                    Err(err) => {
                        if let Some(view) = this.view_mut(tab_id) {
                            let keep_dirty = view.edit.as_ref().is_some_and(|e| e.is_dirty());
                            if keep_dirty {
                                // Spec: do not wipe pending edits when structure refresh fails.
                                view.status = format!("Structure for edit failed: {err}");
                            } else {
                                view.edit = Some(match kind {
                                    Some(kind) => EditSession::loading(dialect, kind),
                                    None => EditSession::unresolved_kind(dialect),
                                });
                                view.status = format!("Structure for edit failed: {err}");
                            }
                        }
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    fn apply_structure_to_edit(
        &mut self,
        tab_id: TabId,
        structure: TableStructure,
        cx: &mut Context<Self>,
    ) {
        let dialect = self
            .session
            .as_ref()
            .map(|s| s.dialect())
            .unwrap_or(sela_core::Dialect::Sqlite);
        let kind = self.views.get(&tab_id).and_then(|v| match &v.source {
            GridSource::Table {
                kind,
                schema,
                table,
            } => kind.or_else(|| catalog_kind(&self.catalog, schema, table)),
            _ => None,
        });
        // Keep pending edits if the same table already has a dirty session.
        let keep = self
            .views
            .get(&tab_id)
            .and_then(|v| v.edit.as_ref())
            .is_some_and(|e| e.is_dirty());
        if keep {
            self.sync_grid_edit_paint(tab_id, cx);
            return;
        }
        if let Some(view) = self.view_mut(tab_id) {
            view.edit = Some(match kind {
                Some(kind) => EditSession::from_structure(&structure, dialect, kind),
                // Never assume Table: an unknown relation stays read-only.
                None => EditSession::unresolved_kind(dialect),
            });
        }
        self.sync_tab_dirty(tab_id);
        self.sync_grid_edit_paint(tab_id, cx);
        cx.notify();
    }

    /// Push overlay / marker state into the grid delegate for painting.
    pub(super) fn sync_grid_edit_paint(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        let Some(view) = self.views.get(&tab_id) else {
            return;
        };
        let mut paint = view
            .edit
            .as_ref()
            .map(|edit| build_edit_paint(edit, &view.grid.read(cx).delegate().result))
            .unwrap_or_default();
        if view.edit.as_ref().is_some_and(|e| !e.row_chords_allowed()) {
            paint.editor_input = Some(view.cell_editor.clone());
        }
        let grid = view.grid.clone();
        grid.update(cx, |state, cx| {
            state.delegate_mut().edit_paint = paint;
            state.refresh(cx);
        });
    }

    pub(super) fn begin_cell_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_active_edit_safe_mode();
        let Some(tab_id) = self.tabs.active().map(|t| t.id) else {
            return;
        };
        if let Some(view) = self.view_mut(tab_id) {
            view.cell_inspect = None;
        }
        let Some(view) = self.views.get(&tab_id) else {
            return;
        };
        if !matches!(view.table_pane, super::TablePane::Data) {
            return;
        }
        // SQL result: status only.
        if matches!(view.source, GridSource::Sql(_)) {
            if let Some(view) = self.view_mut(tab_id) {
                view.status = ReadOnlyReason::SqlResult.status_line().into();
            }
            cx.notify();
            return;
        }
        let selection = view.grid.read(cx).selection();
        let (row, col) = match selection {
            TableSelection::Cell(r, c) => (r, c),
            _ => {
                if let Some(view) = self.view_mut(tab_id) {
                    view.status = "Select a cell first.".into();
                }
                cx.notify();
                return;
            }
        };
        let pk_cols = view
            .edit
            .as_ref()
            .and_then(|e| e.changeset.as_ref())
            .map(|cs| cs.pk_columns().to_vec())
            .unwrap_or_default();
        let (resolved, column) = {
            let state = view.grid.read(cx);
            let overlays = view
                .edit
                .as_ref()
                .map(|e| e.overlay.as_slice())
                .unwrap_or(&[]);
            let visual = visual_rows_for(&state.delegate().result, overlays, &pk_cols);
            let resolved = visual.get(row).copied();
            let column = state
                .delegate()
                .result
                .columns()
                .get(col)
                .map(|c| c.name.clone());
            (resolved, column)
        };
        let Some(resolved) = resolved else {
            return;
        };
        let edit_row = match resolved {
            VisualRowRef::Server { row_ix } => EditRowId::Server { row_ix },
            VisualRowRef::Overlay { overlay_ix } => {
                let insert_id = view
                    .edit
                    .as_ref()
                    .and_then(|e| e.overlay.get(overlay_ix).map(|o| o.insert_id));
                let Some(insert_id) = insert_id else {
                    return;
                };
                EditRowId::Overlay { insert_id }
            }
        };
        let Some(column) = column else {
            return;
        };
        let (pk, current, deleted) = self.server_row_edit_context(tab_id, resolved, &column, cx);
        let old_value = match &edit_row {
            EditRowId::Server { .. } => pk.as_ref().and_then(|p| {
                self.views
                    .get(&tab_id)
                    .and_then(|v| v.edit.as_ref())
                    .and_then(|e| e.original_server_value(p, &column).cloned())
                    .or_else(|| current.clone())
            }),
            EditRowId::Overlay { .. } => current.clone(),
        };
        let status_msg;
        {
            let Some(view) = self.view_mut(tab_id) else {
                return;
            };
            let Some(edit) = view.edit.as_mut() else {
                view.status = "This table is read-only.".into();
                cx.notify();
                return;
            };
            match edit.begin_edit(
                edit_row,
                column.clone(),
                col,
                current.as_ref(),
                deleted,
                pk,
                old_value,
            ) {
                Ok(()) => {
                    let draft = edit
                        .editor
                        .as_ref()
                        .map(|e| {
                            if e.is_null {
                                String::new()
                            } else {
                                e.draft.clone()
                            }
                        })
                        .unwrap_or_default();
                    let placeholder = if edit.editor.as_ref().is_some_and(|e| e.is_null) {
                        "NULL"
                    } else if edit
                        .changeset
                        .as_ref()
                        .is_some_and(|cs| cs.pk_columns().iter().any(|p| p == &column))
                        && current.is_none()
                        && !edit.is_generated(&column)
                    {
                        "Required"
                    } else {
                        "NULL"
                    };
                    view.cell_editor.update(cx, |input, cx| {
                        input.set_placeholder(placeholder, window, cx);
                        input.set_value(&draft, window, cx);
                    });
                    view.cell_editor.read(cx).focus_handle(cx).focus(window, cx);
                    status_msg = None;
                }
                Err(msg) => {
                    status_msg = Some(msg.to_string());
                }
            }
            if let Some(msg) = status_msg {
                view.status = msg;
            }
        }
        self.sync_grid_edit_paint(tab_id, cx);
        cx.notify();
    }

    fn server_row_edit_context(
        &self,
        tab_id: TabId,
        resolved: VisualRowRef,
        column: &str,
        cx: &Context<Self>,
    ) -> (Option<Vec<Value>>, Option<Value>, bool) {
        let Some(view) = self.views.get(&tab_id) else {
            return (None, None, false);
        };
        let Some(edit) = view.edit.as_ref() else {
            return (None, None, false);
        };
        let delegate = view.grid.read(cx).delegate();
        match resolved {
            VisualRowRef::Overlay { overlay_ix } => {
                let insert_id = edit.overlay.get(overlay_ix).map(|o| o.insert_id);
                let value = insert_id.and_then(|id| edit.overlay_value(id, column).cloned());
                (None, value, false)
            }
            VisualRowRef::Server { row_ix } => {
                let pk_cols = edit
                    .changeset
                    .as_ref()
                    .map(|cs| cs.pk_columns().to_vec())
                    .unwrap_or_default();
                let pk = extract_pk(&delegate.result, row_ix, &pk_cols);
                let deleted = pk
                    .as_ref()
                    .is_some_and(|p| edit.row_marker_for_server(p) == RowMarker::Deleted);
                let current = if let Some(ref p) = pk {
                    if let Some(pending) = edit.pending_server_value(p, column) {
                        Some(pending.clone())
                    } else {
                        cell_value_at(&delegate.result, row_ix, column)
                    }
                } else {
                    cell_value_at(&delegate.result, row_ix, column)
                };
                (pk, current, deleted)
            }
        }
    }

    pub(super) fn cancel_cell_editor(&mut self, cx: &mut Context<Self>) {
        let Some(tab_id) = self.tabs.active().map(|t| t.id) else {
            return;
        };
        // Escape closes inspect first, then the cell editor / preview sheet.
        if let Some(view) = self.view_mut(tab_id) {
            if view.cell_inspect.take().is_some() {
                cx.notify();
                return;
            }
        }
        if let Some(edit) = self.view_mut(tab_id).and_then(|v| v.edit.as_mut()) {
            if edit.preview_open && edit.editor.is_none() {
                edit.preview_open = false;
                cx.notify();
                return;
            }
            edit.cancel_editor();
        }
        self.sync_grid_edit_paint(tab_id, cx);
        cx.notify();
    }

    /// Space: open the inspect popover for the focused cell (01 / 07).
    pub(super) fn inspect_focused_cell(&mut self, cx: &mut Context<Self>) {
        let Some(tab_id) = self.tabs.active().map(|t| t.id) else {
            return;
        };
        let Some(view) = self.views.get(&tab_id) else {
            return;
        };
        if !matches!(view.table_pane, super::TablePane::Data) {
            return;
        }
        // Do not inspect while the cell editor is open (Space must type a space).
        if view.edit.as_ref().is_some_and(|e| !e.row_chords_allowed()) {
            return;
        }
        let selection = view.grid.read(cx).selection();
        let (row, col) = match selection {
            TableSelection::Cell(r, c) => (r, c),
            _ => return,
        };
        let delegate = view.grid.read(cx).delegate();
        let overlays = view
            .edit
            .as_ref()
            .map(|e| e.overlay.as_slice())
            .unwrap_or(&[]);
        let pk_cols = view
            .edit
            .as_ref()
            .and_then(|e| e.changeset.as_ref())
            .map(|cs| cs.pk_columns().to_vec())
            .unwrap_or_default();
        let visual = visual_rows_for(&delegate.result, overlays, &pk_cols);
        let Some(resolved) = visual.get(row).copied() else {
            return;
        };
        let col_meta = delegate.result.columns().get(col);
        let Some(col_meta) = col_meta else {
            return;
        };
        let column = col_meta.name.clone();
        let type_name = col_meta.type_name.clone();
        let (value, dirty_display) = match resolved {
            VisualRowRef::Server { row_ix } => {
                let dirty = view.edit.as_ref().and_then(|edit| {
                    let pk_cols = edit
                        .changeset
                        .as_ref()
                        .map(|cs| cs.pk_columns().to_vec())
                        .unwrap_or_default();
                    let pk = extract_pk(&delegate.result, row_ix, &pk_cols)?;
                    edit.pending_server_value(&pk, &column)
                        .map(|v| v.display().into_owned())
                });
                (delegate.result.cell_value(row_ix, col).cloned(), dirty)
            }
            VisualRowRef::Overlay { overlay_ix } => {
                let insert_id = view
                    .edit
                    .as_ref()
                    .and_then(|e| e.overlay.get(overlay_ix).map(|o| o.insert_id));
                let value = insert_id.and_then(|id| {
                    view.edit
                        .as_ref()
                        .and_then(|e| e.overlay_value(id, &column).cloned())
                });
                (value, None)
            }
        };
        let row_h = f32::from(sela_config::content_row_height(self.font_size));
        let (anchor_left, anchor_top) = {
            let table = view.grid.read(cx);
            let mut widths = Vec::with_capacity(col);
            for c in 0..col {
                let w = table.col_width(c).map(|p| p.as_f32()).unwrap_or(140.0);
                widths.push(w);
            }
            let scroll_x = table.horizontal_scroll_handle.offset().x.as_f32();
            let scroll_y = table
                .vertical_scroll_handle
                .0
                .borrow()
                .base_handle
                .offset()
                .y
                .as_f32();
            // DataTable row header is `w_3()` (0.75rem ≈ 12px); header band is one row.
            cell_inspect_anchor(
                row,
                col,
                &widths,
                CellInspectLayout {
                    row_height: row_h,
                    header_height: row_h,
                    row_header_width: 12.0,
                    scroll_x,
                    scroll_y,
                },
            )
        };
        let inspect = match dirty_display {
            Some(display) => CellInspect {
                column,
                type_name,
                body: display,
                decode_error: false,
                anchor_left,
                anchor_top,
            },
            None => match value.as_ref() {
                Some(v) => {
                    let mut inspect = cell_inspect_for_value(&column, &type_name, v);
                    inspect.anchor_left = anchor_left;
                    inspect.anchor_top = anchor_top;
                    inspect
                }
                None => CellInspect {
                    column,
                    type_name,
                    body: inspect_body_for_absent_value().into(),
                    decode_error: false,
                    anchor_left,
                    anchor_top,
                },
            },
        };
        if let Some(view) = self.view_mut(tab_id) {
            view.cell_inspect = Some(inspect);
        }
        cx.notify();
    }

    pub(super) fn accept_cell_editor(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        self.sync_active_edit_safe_mode();
        let Some(tab_id) = self.tabs.active().map(|t| t.id) else {
            return Err("No active tab.".into());
        };
        let draft = self
            .views
            .get(&tab_id)
            .map(|v| v.cell_editor.read(cx).value().to_string())
            .unwrap_or_default();
        // Sync draft from the input. The ghost NULL button sets `forced_null` and
        // clears the input before accept; leave that path alone.
        if let Some(edit) = self.view_mut(tab_id).and_then(|v| v.edit.as_mut()) {
            if let Some(ed) = &mut edit.editor {
                if ed.forced_null && draft.is_empty() {
                    // Keep explicit NULL from the ghost button.
                } else {
                    if ed.is_null && !draft.is_empty() {
                        ed.is_null = false;
                        ed.forced_null = false;
                    }
                    ed.draft = draft;
                }
            }
        }
        let result = {
            let view = self.view_mut(tab_id).ok_or("No active tab.")?;
            let edit = view.edit.as_mut().ok_or("Not editable.")?;
            edit.accept_editor()
        };
        self.sync_tab_dirty(tab_id);
        self.sync_grid_edit_paint(tab_id, cx);
        cx.notify();
        result
    }

    pub(super) fn set_editor_null_and_accept(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab_id) = self.tabs.active().map(|t| t.id) else {
            return;
        };
        if let Some(edit) = self.view_mut(tab_id).and_then(|v| v.edit.as_mut()) {
            edit.set_editor_null();
        }
        if let Some(view) = self.views.get(&tab_id) {
            view.cell_editor.update(cx, |input, cx| {
                input.set_value("", window, cx);
            });
        }
        let _ = self.accept_cell_editor(cx);
    }

    /// True while the active tab's in-cell editor is open.
    pub(super) fn active_cell_editor_open(&self) -> bool {
        self.active_view()
            .and_then(|v| v.edit.as_ref())
            .is_some_and(|e| !e.row_chords_allowed())
    }

    pub(super) fn insert_edit_row(&mut self, cx: &mut Context<Self>) {
        self.sync_active_edit_safe_mode();
        let Some(tab_id) = self.tabs.active().map(|t| t.id) else {
            return;
        };
        if let Some(view) = self.view_mut(tab_id) {
            if let Some(edit) = view.edit.as_mut() {
                if let Err(e) = edit.insert_blank_row() {
                    view.status = e;
                }
            } else {
                view.status = "This table is read-only.".into();
            }
        }
        self.sync_tab_dirty(tab_id);
        self.sync_grid_edit_paint(tab_id, cx);
        cx.notify();
    }

    pub(super) fn duplicate_edit_row(&mut self, cx: &mut Context<Self>) {
        self.sync_active_edit_safe_mode();
        let Some(tab_id) = self.tabs.active().map(|t| t.id) else {
            return;
        };
        let selection = self.views.get(&tab_id).map(|v| v.grid.read(cx).selection());
        let row = match selection {
            Some(TableSelection::Cell(r, _)) | Some(TableSelection::Row(r)) => r,
            _ => {
                if let Some(view) = self.view_mut(tab_id) {
                    view.status = "Select a row first.".into();
                }
                cx.notify();
                return;
            }
        };
        let (columns, source_pk) = {
            let Some(view) = self.views.get(&tab_id) else {
                return;
            };
            let delegate = view.grid.read(cx).delegate();
            let overlays = view
                .edit
                .as_ref()
                .map(|e| e.overlay.as_slice())
                .unwrap_or(&[]);
            let pk_columns = view
                .edit
                .as_ref()
                .and_then(|e| e.changeset.as_ref())
                .map(|cs| cs.pk_columns().to_vec())
                .unwrap_or_default();
            let visual = visual_rows_for(&delegate.result, overlays, &pk_columns);
            let Some(VisualRowRef::Server { row_ix }) = visual.get(row).copied() else {
                if let Some(view) = self.view_mut(tab_id) {
                    view.status = "Select a row first.".into();
                }
                cx.notify();
                return;
            };
            let names: Vec<String> = delegate
                .result
                .columns()
                .iter()
                .map(|c| c.name.clone())
                .collect();
            let Some(source_pk) = extract_pk(&delegate.result, row_ix, &pk_columns) else {
                if let Some(view) = self.view_mut(tab_id) {
                    view.status = "Select a row first.".into();
                }
                cx.notify();
                return;
            };
            match delegate.result.row(row_ix) {
                RowState::Ready(cells) => (
                    names
                        .into_iter()
                        .zip(cells.iter().cloned())
                        .collect::<Vec<_>>(),
                    source_pk,
                ),
                RowState::Loading => {
                    if let Some(view) = self.view_mut(tab_id) {
                        view.status = "Select a row first.".into();
                    }
                    cx.notify();
                    return;
                }
            }
        };
        let pk_columns = self
            .views
            .get(&tab_id)
            .and_then(|v| v.edit.as_ref())
            .and_then(|e| e.changeset.as_ref())
            .map(|cs| cs.pk_columns().to_vec())
            .unwrap_or_default();
        if let Some(view) = self.view_mut(tab_id) {
            if let Some(edit) = view.edit.as_mut() {
                if let Err(e) = edit.duplicate_server_row(&columns, &pk_columns, source_pk) {
                    view.status = e;
                }
            } else {
                view.status = "This table is read-only.".into();
            }
        }
        self.sync_tab_dirty(tab_id);
        self.sync_grid_edit_paint(tab_id, cx);
        cx.notify();
    }

    pub(super) fn delete_edit_row(&mut self, cx: &mut Context<Self>) {
        self.sync_active_edit_safe_mode();
        let Some(tab_id) = self.tabs.active().map(|t| t.id) else {
            return;
        };
        // If cell editor open, leave secondary-backspace to the field.
        if self
            .views
            .get(&tab_id)
            .and_then(|v| v.edit.as_ref())
            .is_some_and(|e| !e.row_chords_allowed())
        {
            return;
        }
        let selection = self.views.get(&tab_id).map(|v| v.grid.read(cx).selection());
        let row = match selection {
            Some(TableSelection::Cell(r, _)) | Some(TableSelection::Row(r)) => r,
            _ => {
                if let Some(view) = self.view_mut(tab_id) {
                    view.status = "Select a row first.".into();
                }
                cx.notify();
                return;
            }
        };
        let resolved = {
            let Some(view) = self.views.get(&tab_id) else {
                return;
            };
            let delegate = view.grid.read(cx).delegate();
            let overlays = view
                .edit
                .as_ref()
                .map(|e| e.overlay.as_slice())
                .unwrap_or(&[]);
            let pk_cols = view
                .edit
                .as_ref()
                .and_then(|e| e.changeset.as_ref())
                .map(|cs| cs.pk_columns().to_vec())
                .unwrap_or_default();
            visual_rows_for(&delegate.result, overlays, &pk_cols)
                .get(row)
                .copied()
        };
        match resolved {
            Some(VisualRowRef::Overlay { overlay_ix }) => {
                let insert_id = self
                    .views
                    .get(&tab_id)
                    .and_then(|v| v.edit.as_ref())
                    .and_then(|e| e.overlay.get(overlay_ix).map(|o| o.insert_id));
                if let Some(id) = insert_id {
                    if let Some(view) = self.view_mut(tab_id) {
                        if let Some(edit) = view.edit.as_mut() {
                            if let Err(e) = edit.delete_overlay(id) {
                                view.status = e;
                            }
                        }
                    }
                }
            }
            Some(VisualRowRef::Server { row_ix }) => {
                let pk_cols = self
                    .views
                    .get(&tab_id)
                    .and_then(|v| v.edit.as_ref())
                    .and_then(|e| e.changeset.as_ref())
                    .map(|cs| cs.pk_columns().to_vec())
                    .unwrap_or_default();
                let (pk, expected) = self
                    .views
                    .get(&tab_id)
                    .map_or((None, BTreeMap::new()), |v| {
                        let result = &v.grid.read(cx).delegate().result;
                        let pk = extract_pk(result, row_ix, &pk_cols);
                        let expected = if let Some(ref p) = pk {
                            extract_optimistic_expected(
                                result,
                                row_ix,
                                &pk_cols,
                                v.edit.as_ref(),
                                p,
                            )
                        } else {
                            BTreeMap::new()
                        };
                        (pk, expected)
                    });
                if let Some(pk) = pk {
                    if let Some(view) = self.view_mut(tab_id) {
                        if let Some(edit) = view.edit.as_mut() {
                            match edit.delete_or_restore_server_with_expected(&pk, expected) {
                                Ok(_) => {}
                                Err(e) => view.status = e,
                            }
                        } else {
                            view.status = "This table is read-only.".into();
                        }
                    }
                } else if let Some(view) = self.view_mut(tab_id) {
                    view.status = "Select a row first.".into();
                }
            }
            None => {
                if let Some(view) = self.view_mut(tab_id) {
                    view.status = "Select a row first.".into();
                }
            }
        }
        self.sync_tab_dirty(tab_id);
        self.sync_grid_edit_paint(tab_id, cx);
        cx.notify();
    }
}

/// Build the Space inspect popover payload from a full [`Value`] (not truncated).
/// Inspect body when the cell has no value (unset insert column).
pub(crate) fn inspect_body_for_absent_value() -> &'static str {
    "Unset"
}

pub(crate) fn cell_inspect_for_value(column: &str, type_name: &str, value: &Value) -> CellInspect {
    CellInspect {
        column: column.to_string(),
        type_name: type_name.to_string(),
        body: value.display().into_owned(),
        decode_error: value.is_decode_error(),
        anchor_left: 8.0,
        anchor_top: 8.0,
    }
}

/// Grid metrics used to place the Space inspect popover.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CellInspectLayout {
    pub row_height: f32,
    pub header_height: f32,
    pub row_header_width: f32,
    /// Table scroll offsets (negative when scrolled).
    pub scroll_x: f32,
    pub scroll_y: f32,
}

/// Local origin for the Space inspect popover relative to the results area.
pub(crate) fn cell_inspect_anchor(
    row: usize,
    col: usize,
    col_widths: &[f32],
    layout: CellInspectLayout,
) -> (f32, f32) {
    let mut left = layout.row_header_width + layout.scroll_x;
    for w in col_widths.iter().take(col) {
        left += *w;
    }
    let top = layout.header_height + row as f32 * layout.row_height + layout.scroll_y;
    (left.max(8.0), top.max(8.0))
}

fn extract_pk(
    result: &sela_core::ResultWindow,
    row: usize,
    pk_cols: &[String],
) -> Option<Vec<Value>> {
    if pk_cols.is_empty() {
        return None;
    }
    let names: Vec<String> = result.columns().iter().map(|c| c.name.clone()).collect();
    let RowState::Ready(cells) = result.row(row) else {
        return None;
    };
    let mut pk = Vec::with_capacity(pk_cols.len());
    for name in pk_cols {
        let ix = names.iter().position(|n| n == name)?;
        pk.push(cells.get(ix)?.clone());
    }
    Some(pk)
}

/// Non-PK originals for optimistic DELETE WHERE: pending `old_value` when the
/// cell was edited, otherwise the loaded server cell.
///
/// Passes every loaded non-PK through; [`ChangePlan`] skips non-comparable
/// kinds and lists them under `-- unchecked optimistic:` in Preview.
fn extract_optimistic_expected(
    result: &sela_core::ResultWindow,
    row: usize,
    pk_cols: &[String],
    edit: Option<&EditSession>,
    pk: &[Value],
) -> BTreeMap<String, Value> {
    let mut expected = BTreeMap::new();
    let RowState::Ready(cells) = result.row(row) else {
        return expected;
    };
    for (ix, col) in result.columns().iter().enumerate() {
        if pk_cols.iter().any(|p| p == &col.name) {
            continue;
        }
        let original = edit
            .and_then(|e| e.original_server_value(pk, &col.name).cloned())
            .or_else(|| cells.get(ix).cloned());
        if let Some(value) = original {
            expected.insert(col.name.clone(), value);
        }
    }
    expected
}

const STRUCTURE_ATTEMPTS: usize = 3;
/// Zero in tests so retries never leak timers onto the shared db runtime.
const STRUCTURE_RETRY_DELAY: std::time::Duration = if cfg!(test) {
    std::time::Duration::ZERO
} else {
    std::time::Duration::from_millis(250)
};

fn catalog_kind(
    catalog: &[super::LoadedSchema],
    schema: &str,
    table: &str,
) -> Option<RelationKind> {
    catalog
        .iter()
        .filter(|loaded| loaded.name == schema)
        .find_map(|loaded| loaded.tables.iter().find(|t| t.name == table))
        .map(|info| info.kind)
}

fn cell_value_at(result: &sela_core::ResultWindow, row: usize, column: &str) -> Option<Value> {
    let ix = result.columns().iter().position(|c| c.name == column)?;
    match result.row(row) {
        RowState::Ready(cells) => cells.get(ix).cloned(),
        RowState::Loading => None,
    }
}

fn build_edit_paint(edit: &EditSession, result: &sela_core::ResultWindow) -> EditPaintState {
    build_edit_paint_with(edit, result, &edit.pending_index())
}

/// Paint state using a prebuilt [`PendingIndex`] (one hash lookup per row).
pub(super) fn build_edit_paint_with(
    edit: &EditSession,
    result: &sela_core::ResultWindow,
    index: &PendingIndex<'_>,
) -> EditPaintState {
    let mut paint = EditPaintState::default();
    let pk_cols = edit
        .changeset
        .as_ref()
        .map(|cs| cs.pk_columns().to_vec())
        .unwrap_or_default();
    for row_ix in 0..result.total_rows() {
        let Some(pk) = extract_pk(result, row_ix, &pk_cols) else {
            continue;
        };
        let marker = index.marker(&pk);
        if marker == RowMarker::Clean {
            continue;
        }
        let mut dirty_cols = BTreeMap::new();
        let mut dirty_null = std::collections::BTreeSet::new();
        if marker == RowMarker::Dirty || marker == RowMarker::Deleted {
            for col in result.columns() {
                if let Some(v) = index.pending_value(&pk, &col.name) {
                    dirty_cols.insert(col.name.clone(), v.display().into_owned());
                    if v.is_null() {
                        dirty_null.insert(col.name.clone());
                    }
                }
            }
        }
        let mut original = BTreeMap::new();
        for col in result.columns() {
            if let Some(v) = index.original_value(&pk, &col.name) {
                original.insert(col.name.clone(), v.display().into_owned());
            }
        }
        paint.server.insert(
            row_ix,
            EditPaintRow {
                marker,
                dirty_display: dirty_cols,
                dirty_null,
                original_display: original,
            },
        );
    }
    let page_pks: Vec<_> = (0..result.total_rows())
        .map(|i| extract_pk(result, i, &pk_cols))
        .collect();
    let overlay_anchors = resolve_overlay_anchors(&edit.overlay, &page_pks);
    for (overlay, after_server_row) in edit.overlay.iter().zip(overlay_anchors) {
        let marker = edit.row_marker_for_overlay(overlay.insert_id);
        let raw = index
            .insert_values(overlay.insert_id)
            .cloned()
            .unwrap_or_default();
        paint.overlays.push(OverlayPaintRow {
            insert_id: overlay.insert_id,
            marker,
            raw,
            after_server_row,
            required_unset: edit
                .changeset
                .as_ref()
                .map(|cs| {
                    cs.pk_columns()
                        .iter()
                        .filter(|c| {
                            !edit.is_generated(c)
                                && edit.overlay_value(overlay.insert_id, c).is_none()
                        })
                        .cloned()
                        .collect()
                })
                .unwrap_or_default(),
            generated: edit.generated_columns.iter().cloned().collect(),
        });
    }
    if let Some(ed) = &edit.editor {
        paint.editing = Some((ed.row.clone(), ed.col_ix));
        paint.editor_error = ed.validation_error.clone();
    }
    paint
}

#[cfg(test)]
mod tests {
    use super::{cell_inspect_anchor, cell_inspect_for_value, CellInspectLayout};
    use sela_core::Value;

    #[test]
    fn inspect_unset_body_is_unset_not_ellipsis() {
        assert_eq!(super::inspect_body_for_absent_value(), "Unset");
        assert_ne!(super::inspect_body_for_absent_value(), "…");
    }

    #[test]
    fn inspect_uses_full_display_not_truncated() {
        let long = "x".repeat(250);
        let value = Value::text(long.as_str());
        let inspect = cell_inspect_for_value("note", "text", &value);
        assert_eq!(inspect.column, "note");
        assert_eq!(inspect.type_name, "text");
        assert_eq!(inspect.body, long);
        assert!(!inspect.decode_error);
    }

    #[test]
    fn inspect_marks_decode_error() {
        let value = Value::decode_error("jsonb");
        let inspect = cell_inspect_for_value("payload", "jsonb", &value);
        assert!(inspect.decode_error);
        assert!(inspect.body.contains("decode error"));
    }

    #[test]
    fn inspect_anchor_follows_cell_not_grid_origin() {
        let widths = [140.0_f32, 200.0, 160.0];
        let base = CellInspectLayout {
            row_height: 26.0,
            header_height: 26.0,
            row_header_width: 12.0,
            scroll_x: 0.0,
            scroll_y: 0.0,
        };
        // Row 2, col 1: past row header + first column, below header + two rows.
        let (left, top) = cell_inspect_anchor(2, 1, &widths, base);
        assert!((left - (12.0 + 140.0)).abs() < f32::EPSILON);
        assert!((top - (26.0 + 2.0 * 26.0)).abs() < f32::EPSILON);
        // Scrolled down/right: offsets are negative.
        let (left2, top2) = cell_inspect_anchor(
            2,
            1,
            &widths,
            CellInspectLayout {
                scroll_x: -40.0,
                scroll_y: -20.0,
                ..base
            },
        );
        assert!((left2 - (12.0 + 140.0 - 40.0)).abs() < f32::EPSILON);
        assert!((top2 - (26.0 + 52.0 - 20.0)).abs() < f32::EPSILON);
        // Clamp keeps a minimum inset when the cell is scrolled off the top-left.
        let (left3, top3) = cell_inspect_anchor(
            0,
            0,
            &widths,
            CellInspectLayout {
                scroll_x: -100.0,
                scroll_y: -100.0,
                ..base
            },
        );
        assert!((left3 - 8.0).abs() < f32::EPSILON);
        assert!((top3 - 8.0).abs() < f32::EPSILON);
    }
}
