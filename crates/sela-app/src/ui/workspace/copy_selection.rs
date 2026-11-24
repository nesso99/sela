//! Copy the active grid selection to the clipboard (US-14 / M1-7).

use gpui_kit::{ClipboardItem, Context};

use sela_core::{
    extract_selection_matrix, format_selection, value_to_copy_cell, CellPos, CopyCell, CopyFormat,
    RowState, SelectionRect,
};

use super::{GridSource, Workspace};

impl Workspace {
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
