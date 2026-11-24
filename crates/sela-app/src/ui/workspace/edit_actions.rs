//! Grid edit / preview / commit actions and structure-backed EditSession setup.

use std::collections::BTreeMap;

use gpui_kit::component::table::TableSelection;
use gpui_kit::{Context, Focusable as _, Window};

use sela_core::{RelationKind, RowState, TabId, TableStructure, Value};

use super::edit_session::{EditRowId, EditSession, ReadOnlyReason, RowMarker};
use super::{GridSource, Workspace};
use crate::ui::grid::{EditPaintRow, EditPaintState, OverlayPaintRow};

impl Workspace {
    /// Relation kind from the loaded catalog, defaulting to Table.
    pub(super) fn relation_kind_for(&self, schema: &str, table: &str) -> RelationKind {
        for loaded in &self.catalog {
            if loaded.name != schema {
                continue;
            }
            if let Some(info) = loaded.tables.iter().find(|t| t.name == table) {
                return info.kind;
            }
        }
        RelationKind::Table
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
        match &view.source {
            GridSource::Table {
                schema,
                table,
                kind,
            } => {
                let kind = *kind;
                if view.edit.is_none() {
                    view.edit = Some(EditSession::loading(dialect, kind));
                }
                let schema = schema.clone();
                let table = table.clone();
                let _ = view;
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
            let result =
                sela_drivers::drive(async move { session.describe_table(&schema, &table).await })
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
                                view.edit = Some(EditSession::loading(dialect, kind));
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
        let kind = self
            .views
            .get(&tab_id)
            .and_then(|v| match &v.source {
                GridSource::Table { kind, .. } => Some(*kind),
                _ => None,
            })
            .unwrap_or(RelationKind::Table);
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
            view.edit = Some(EditSession::from_structure(&structure, dialect, kind));
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
        if view.edit.as_ref().is_some_and(|e| e.editor.is_some()) {
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
        let server_rows = view.grid.read(cx).delegate().result.total_rows();
        let edit_row = if row < server_rows {
            EditRowId::Server { row_ix: row }
        } else {
            let overlay_ix = row - server_rows;
            let insert_id = view
                .edit
                .as_ref()
                .and_then(|e| e.overlay.get(overlay_ix).map(|o| o.insert_id));
            let Some(insert_id) = insert_id else {
                return;
            };
            EditRowId::Overlay { insert_id }
        };
        let column = view
            .grid
            .read(cx)
            .delegate()
            .result
            .columns()
            .get(col)
            .map(|c| c.name.clone());
        let Some(column) = column else {
            return;
        };
        let (pk, current, deleted) = self.server_row_edit_context(tab_id, row, &column, cx);
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
        row: usize,
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
        let server_rows = delegate.result.total_rows();
        if row >= server_rows {
            let overlay_ix = row - server_rows;
            let insert_id = edit.overlay.get(overlay_ix).map(|o| o.insert_id);
            let value = insert_id.and_then(|id| edit.overlay_value(id, column).cloned());
            return (None, value, false);
        }
        let pk_cols = edit
            .changeset
            .as_ref()
            .map(|cs| cs.pk_columns().to_vec())
            .unwrap_or_default();
        let pk = extract_pk(&delegate.result, row, &pk_cols);
        let deleted = pk
            .as_ref()
            .is_some_and(|p| edit.row_marker_for_server(p) == RowMarker::Deleted);
        let current = if let Some(ref p) = pk {
            if let Some(pending) = edit.pending_server_value(p, column) {
                Some(pending.clone())
            } else {
                cell_value_at(&delegate.result, row, column)
            }
        } else {
            cell_value_at(&delegate.result, row, column)
        };
        (pk, current, deleted)
    }

    pub(super) fn cancel_cell_editor(&mut self, cx: &mut Context<Self>) {
        let Some(tab_id) = self.tabs.active().map(|t| t.id) else {
            return;
        };
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
        let was_null_button = {
            let view = self.views.get(&tab_id).ok_or("No active tab.")?;
            let edit = view.edit.as_ref().ok_or("Not editable.")?;
            let editor = edit.editor.as_ref().ok_or("No open editor.")?;
            editor.is_null && draft.is_empty()
        };
        // Sync draft from the input unless NULL was set via the button and left empty.
        if let Some(edit) = self.view_mut(tab_id).and_then(|v| v.edit.as_mut()) {
            if let Some(ed) = &mut edit.editor {
                if !(ed.is_null && draft.is_empty() && was_null_button) {
                    // Typing into an empty NULL field clears NULL → '' for text.
                    if ed.is_null && !draft.is_empty() {
                        ed.is_null = false;
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
        let columns = {
            let Some(view) = self.views.get(&tab_id) else {
                return;
            };
            let delegate = view.grid.read(cx).delegate();
            let server_rows = delegate.result.total_rows();
            if row >= server_rows {
                if let Some(view) = self.view_mut(tab_id) {
                    view.status = "Select a row first.".into();
                }
                cx.notify();
                return;
            }
            let names: Vec<String> = delegate
                .result
                .columns()
                .iter()
                .map(|c| c.name.clone())
                .collect();
            match delegate.result.row(row) {
                RowState::Ready(cells) => names
                    .into_iter()
                    .zip(cells.iter().cloned())
                    .collect::<Vec<_>>(),
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
                if let Err(e) = edit.duplicate_server_row(&columns, &pk_columns) {
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
            .is_some_and(|e| e.editor.is_some())
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
        let server_rows = self
            .views
            .get(&tab_id)
            .map(|v| v.grid.read(cx).delegate().result.total_rows())
            .unwrap_or(0);
        if row >= server_rows {
            let insert_id = self
                .views
                .get(&tab_id)
                .and_then(|v| v.edit.as_ref())
                .and_then(|e| e.overlay.get(row - server_rows).map(|o| o.insert_id));
            if let Some(id) = insert_id {
                if let Some(view) = self.view_mut(tab_id) {
                    if let Some(edit) = view.edit.as_mut() {
                        if let Err(e) = edit.delete_overlay(id) {
                            view.status = e;
                        }
                    }
                }
            }
        } else {
            let pk_cols = self
                .views
                .get(&tab_id)
                .and_then(|v| v.edit.as_ref())
                .and_then(|e| e.changeset.as_ref())
                .map(|cs| cs.pk_columns().to_vec())
                .unwrap_or_default();
            let pk = self
                .views
                .get(&tab_id)
                .and_then(|v| extract_pk(&v.grid.read(cx).delegate().result, row, &pk_cols));
            if let Some(pk) = pk {
                if let Some(view) = self.view_mut(tab_id) {
                    if let Some(edit) = view.edit.as_mut() {
                        match edit.delete_or_restore_server(&pk) {
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
        self.sync_tab_dirty(tab_id);
        self.sync_grid_edit_paint(tab_id, cx);
        cx.notify();
    }
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

fn cell_value_at(result: &sela_core::ResultWindow, row: usize, column: &str) -> Option<Value> {
    let ix = result.columns().iter().position(|c| c.name == column)?;
    match result.row(row) {
        RowState::Ready(cells) => cells.get(ix).cloned(),
        RowState::Loading => None,
    }
}

fn build_edit_paint(edit: &EditSession, result: &sela_core::ResultWindow) -> EditPaintState {
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
        let marker = edit.row_marker_for_server(&pk);
        if marker == RowMarker::Clean {
            continue;
        }
        let mut dirty_cols = BTreeMap::new();
        if marker == RowMarker::Dirty || marker == RowMarker::Deleted {
            for col in result.columns() {
                if let Some(v) = edit.pending_server_value(&pk, &col.name) {
                    dirty_cols.insert(col.name.clone(), v.display().into_owned());
                }
            }
        }
        let mut original = BTreeMap::new();
        for col in result.columns() {
            if let Some(v) = edit.original_server_value(&pk, &col.name) {
                original.insert(col.name.clone(), v.display().into_owned());
            }
        }
        paint.server.insert(
            row_ix,
            EditPaintRow {
                marker,
                dirty_display: dirty_cols,
                original_display: original,
            },
        );
    }
    for overlay in &edit.overlay {
        let marker = edit.row_marker_for_overlay(overlay.insert_id);
        let mut values = BTreeMap::new();
        if let Some(cs) = &edit.changeset {
            for change in cs.changes() {
                if let sela_core::RowChange::Insert { id, values: vals } = change {
                    if *id == overlay.insert_id {
                        for (k, v) in vals {
                            values.insert(k.clone(), v.display().into_owned());
                        }
                    }
                }
            }
        }
        paint.overlays.push(OverlayPaintRow {
            insert_id: overlay.insert_id,
            marker,
            values,
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
