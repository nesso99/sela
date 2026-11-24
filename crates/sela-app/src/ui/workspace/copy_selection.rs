//! Copy the active grid selection to the clipboard (US-14 / M1-7).

use gpui_kit::{ClipboardItem, Context};

use sela_core::{
    extract_selection_matrix, format_selection, value_to_copy_cell, CellPos, CopyCell, CopyFormat,
    RowState, SelectionRect,
};

use super::{GridSource, Workspace};

impl Workspace {
    /// Select every cell in the active result grid (Ctrl/Cmd+A).
    pub(super) fn select_all_grid(&mut self, cx: &mut Context<Self>) {
        let Some(tab_id) = self.tabs.active().map(|t| t.id) else {
            return;
        };
        let Some(view) = self.views.get(&tab_id) else {
            return;
        };
        view.grid.update(cx, |state, cx| {
            state.delegate_mut().select_all_cells();
            // Do not call set_selected_cell here. It emits TableEvent::SelectCell
            // after this update returns; the subscriber would then run
            // note_cell_focus(..., shift=false) and collapse the full range to
            // the focus cell (RC6 Ctrl+A failure). Keep the current focus cell
            // so arrow keys still navigate from where the user was. No
            // SelectCell follows, so do not arm suppress_next_select_collapse:
            // a stale flag would swallow the user's next plain click.
            state.refresh(cx);
        });
        cx.notify();
    }

    /// Shift+arrow: move the DataTable focus cell and extend `selection_rect`.
    ///
    /// DataTable only binds plain `up`/`down`/`left`/`right`, so Shift+arrow
    /// never matches those keys. We bind `shift-*` to Sela actions and drive
    /// `set_selected_cell` ourselves with a pending Shift flag so the deferred
    /// SelectCell subscriber extends the range instead of resetting it.
    pub(super) fn extend_grid_selection(
        &mut self,
        d_row: isize,
        d_col: isize,
        cx: &mut Context<Self>,
    ) {
        let Some(tab_id) = self.tabs.active().map(|t| t.id) else {
            return;
        };
        let Some(view) = self.views.get(&tab_id) else {
            return;
        };
        view.grid.update(cx, |state, cx| {
            let (row, col) = state.selected_cell().unwrap_or_else(|| {
                state
                    .delegate()
                    .selection_anchor
                    .map(|p| (p.row, p.col))
                    .unwrap_or((0, 0))
            });
            let Some((new_row, new_col)) = state
                .delegate()
                .focus_after_shift_step(row, col, d_row, d_col)
            else {
                return;
            };
            // set_selected_cell emits SelectCell after this update returns. Arm
            // suppress so that deferred handler cannot collapse the range if
            // window.modifiers().shift is already false by then; apply the
            // extend synchronously. The flag is one-shot and is armed only
            // because set_selected_cell below really emits SelectCell.
            state.delegate_mut().note_pointer_shift(true);
            state.delegate_mut().suppress_next_select_collapse();
            state.set_selected_cell(new_row, new_col, cx);
            state.delegate_mut().note_cell_focus(new_row, new_col, true);
            let _ = state.delegate_mut().take_shift_for_select();
            state.refresh(cx);
        });
        cx.notify();
    }

    pub(super) fn copy_selection(&mut self, format: CopyFormat, cx: &mut Context<Self>) {
        let Some(tab_id) = self.tabs.active().map(|t| t.id) else {
            return;
        };
        let dialect = self.active_dialect();
        let table_name = self.views.get(&tab_id).and_then(|view| match &view.source {
            GridSource::Table { schema, table } => Some(format!("{schema}.{table}")),
            _ => None,
        });
        let Some(view) = self.views.get(&tab_id) else {
            return;
        };
        let outcome = {
            let state = view.grid.read(cx);
            let selection = state.selection();
            let delegate = state.delegate();
            let Some(rect) = delegate.copy_rect(selection) else {
                return;
            };
            let columns: Vec<String> = delegate
                .result
                .columns()
                .iter()
                .map(|c| c.name.clone())
                .collect();
            let (min_row, max_row, min_col, max_col) = rect.normalized();
            // Rows are sliced to the selection; remap the rect to local 0-based
            // indices so extract_selection_matrix does not re-apply absolute rows.
            let mut rows = Vec::with_capacity(max_row.saturating_sub(min_row) + 1);
            for row_ix in min_row..=max_row {
                match delegate.result.row(row_ix) {
                    RowState::Ready(cells) => {
                        rows.push(cells.iter().map(value_to_copy_cell).collect());
                    }
                    RowState::Loading => {
                        rows.push(vec![CopyCell::NotLoaded; columns.len().max(1)]);
                    }
                }
            }
            let local = SelectionRect::from_corners(
                CellPos::new(0, min_col),
                CellPos::new(rows.len().saturating_sub(1), max_col),
            );
            let Some((headers, body)) = extract_selection_matrix(&columns, &rows, local) else {
                return;
            };
            format_selection(&headers, &body, format, table_name.as_deref(), dialect)
        };
        match outcome {
            Ok(text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
            Err(err) => {
                if let Some(view) = self.views.get_mut(&tab_id) {
                    view.status = err.status_message();
                }
                cx.notify();
            }
        }
    }
}
