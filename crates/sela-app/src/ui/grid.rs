//! Virtualized result grid backed by gpui-component's `DataTable`.
//!
//! Rows come from [`sela_core::ResultWindow`]: the grid asks by absolute index and
//! paints a placeholder while the page is loading. Display strings are truncated
//! and cached inside the window; copy uses the full [`Value::display`].
//!
//! Header sort: `TableState.sortable` must be on for gpui-component to deliver
//! header clicks (there is no separate click API). [`GridDelegate::perform_sort`]
//! never reorders local rows — it only records a pending server-side ORDER BY.
//!
//! Selection (M1-7): cell / row / column via DataTable; optional Shift range on
//! [`selection_rect`]. Copy formats live in [`sela_core::CopyFormat`].

use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::table::{Column, ColumnSort, TableDelegate, TableSelection, TableState};
use gpui_kit::component::ActiveTheme;
use gpui_kit::prelude::*;
use gpui_kit::{div, px, App, IntoElement, SharedString, Window};

use sela_core::{
    CellPos, CellView, QueryPage, ResultWindow, RowState, SelectionRect, SortDirection, SortSpec,
};

use super::theme::SelaPalette;
use super::{CopyAsCsv, CopyAsInsert, CopyAsJson, CopyAsTsv, CopySelection};

enum CellKind {
    Loading,
    Null,
    Decode(String),
    Normal,
}

pub struct GridDelegate {
    pub result: ResultWindow,
    /// Page indices requested by the last viewport touch (M1-2b will fetch these).
    pub pending_pages: Vec<usize>,
    /// Column header sort requested by the user (table tabs only).
    /// `shift` is true when the click was a Shift-click (multi-sort).
    pub pending_sort: Option<(usize, bool)>,
    /// When true, column headers offer sort clicks (table Data view only).
    pub sortable: bool,
    /// Current sort specs (drives header sort marks ▲1 / ▼2).
    pub sorts: Vec<SortSpec>,
    pub loading: bool,
    pub empty_message: SharedString,
    /// Anchor for Shift-extended rectangular selection.
    pub selection_anchor: Option<CellPos>,
    /// Active rectangular selection (overrides a lone cell when present).
    pub selection_rect: Option<SelectionRect>,
}

impl GridDelegate {
    pub fn empty() -> Self {
        Self {
            result: ResultWindow::empty(),
            pending_pages: Vec::new(),
            pending_sort: None,
            sortable: false,
            sorts: Vec::new(),
            loading: false,
            empty_message: "Open a table, or run a query.".into(),
            selection_anchor: None,
            selection_rect: None,
        }
    }

    pub fn set_loading_message(&mut self) {
        self.empty_message = "Running…".into();
    }

    pub fn show_page(&mut self, page: QueryPage) {
        self.result.replace_with_page(page);
        self.pending_pages.clear();
        self.loading = false;
        self.clear_selection_range();
        if self.result.total_rows() == 0 {
            self.empty_message = "The query returned no rows.".into();
        }
    }

    /// Install a finished page and hide the loading skeleton.
    ///
    /// Table Previous/Next and SQL results both materialize the current page at
    /// row 0 via [`Self::show_page`]. `page_offset` is only the SQL `OFFSET`
    /// used when fetching; it must not become a grid page index, or page 2+
    /// lands past the viewport and the grid shows placeholders (then page-1
    /// rows after a scroll-triggered refetch).
    pub fn install_loaded_page(
        &mut self,
        page: QueryPage,
        _paged: bool,
        _total_rows: Option<usize>,
        _page_offset: u64,
    ) {
        self.show_page(page);
    }

    pub fn show_message(&mut self, message: impl Into<SharedString>) {
        self.result = ResultWindow::empty();
        self.pending_pages.clear();
        self.loading = false;
        self.clear_selection_range();
        self.empty_message = message.into();
    }

    /// Clear a stale "Running…" empty state after the engine reports failure.
    ///
    /// When this request delivered no rows, replace the grid with the failed
    /// empty copy. When rows already arrived (stream then error), keep them and
    /// only clear [`Self::loading`].
    pub fn note_query_failed(&mut self) {
        if self.result.total_rows() == 0 {
            self.show_message("Query failed — see the error above.");
        } else {
            self.loading = false;
        }
    }

    /// Clear a stale "Running…" empty state after the user cancels (Stop).
    ///
    /// Same shape as [`Self::note_query_failed`]: with no rows, show the
    /// cancelled empty copy; with streamed rows, keep them and clear loading.
    pub fn note_query_cancelled(&mut self) {
        if self.result.total_rows() == 0 {
            self.show_message("Query cancelled.");
        } else {
            self.loading = false;
        }
    }

    pub fn clear_selection_range(&mut self) {
        self.selection_anchor = None;
        self.selection_rect = None;
    }

    /// Record a cell focus for Shift-range selection.
    pub fn note_cell_focus(&mut self, row: usize, col: usize, shift: bool) {
        let focus = CellPos::new(row, col);
        if shift {
            let (anchor, rect) = SelectionRect::extend_with_focus(self.selection_anchor, focus);
            self.selection_anchor = Some(anchor);
            self.selection_rect = Some(rect);
        } else {
            self.selection_anchor = Some(focus);
            self.selection_rect = Some(SelectionRect::cell(row, col));
        }
    }

    /// Resolve the rectangle to copy from DataTable selection + our range.
    pub fn copy_rect(&self, selection: TableSelection) -> Option<SelectionRect> {
        let rows = self.result.total_rows();
        let cols = self.result.columns().len();
        if rows == 0 || cols == 0 {
            return None;
        }
        if let Some(rect) = self.selection_rect {
            if matches!(selection, TableSelection::Cell(_, _))
                || rect.row_count() * rect.col_count() > 1
            {
                return rect.clamp(rows, cols);
            }
        }
        match selection {
            TableSelection::None => self.selection_rect.and_then(|r| r.clamp(rows, cols)),
            TableSelection::Cell(row, col) => SelectionRect::cell(row, col).clamp(rows, cols),
            TableSelection::Row(row) => {
                SelectionRect::row(row, cols).and_then(|r| r.clamp(rows, cols))
            }
            TableSelection::Column(col) => {
                SelectionRect::column(col, rows).and_then(|r| r.clamp(rows, cols))
            }
        }
    }

    fn sort_mark(&self, name: &str) -> Option<String> {
        self.sorts.iter().enumerate().find_map(|(ix, s)| {
            if s.column == name {
                let arrow = match s.direction {
                    SortDirection::Asc => "▲",
                    SortDirection::Desc => "▼",
                };
                Some(format!("{arrow}{}", ix + 1))
            } else {
                None
            }
        })
    }

    fn column_sort_state(&self, name: &str) -> ColumnSort {
        match self.sorts.iter().find(|s| s.column == name) {
            Some(SortSpec {
                direction: SortDirection::Asc,
                ..
            }) => ColumnSort::Ascending,
            Some(SortSpec {
                direction: SortDirection::Desc,
                ..
            }) => ColumnSort::Descending,
            None => ColumnSort::Default,
        }
    }
}

impl TableDelegate for GridDelegate {
    fn columns_count(&self, _: &App) -> usize {
        self.result.columns().len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.result.total_rows()
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        let meta = &self.result.columns()[col_ix];
        let header = if let Some(mark) = self.sort_mark(&meta.name) {
            format!("{} {mark}    {}", meta.name, meta.type_name)
        } else {
            format!("{}   {}", meta.name, meta.type_name)
        };
        let width = (header.chars().count() as f32 * 7.5).clamp(140.0, 360.0);
        let mut col = Column::new(format!("{col_ix}:{}", meta.name), header)
            .width(px(width))
            .resizable(true)
            .movable(false);
        if self.sortable {
            // Enables the header click target; we ignore DataTable's own cycle
            // and rebuild ORDER BY from `sorts` / `pending_sort`.
            col = col.sort(self.column_sort_state(&meta.name));
        }
        col
    }

    fn perform_sort(
        &mut self,
        col_ix: usize,
        _sort: ColumnSort,
        window: &mut Window,
        cx: &mut gpui_kit::Context<TableState<Self>>,
    ) {
        if !self.sortable {
            return;
        }
        let shift = window.modifiers().shift;
        self.pending_sort = Some((col_ix, shift));
        cx.notify();
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut gpui_kit::Context<TableState<Self>>,
    ) -> impl IntoElement {
        let palette = SelaPalette::for_mode(cx.theme().mode);
        // Classify before taking the truncated display cache (needs &mut).
        let kind = match self.result.cell_value(row_ix, col_ix) {
            None => CellKind::Loading,
            Some(value) if value.is_null() => CellKind::Null,
            Some(value) if value.is_decode_error() || value.is_unsupported_placeholder() => {
                CellKind::Decode(value.display().into_owned())
            }
            Some(_) => CellKind::Normal,
        };
        let is_decode = matches!(kind, CellKind::Decode(_));
        let (null, loading, text) = match kind {
            CellKind::Loading => (false, true, SharedString::from("…")),
            CellKind::Null => (true, false, SharedString::from("NULL")),
            CellKind::Decode(decode) => (false, false, SharedString::from(format!("!{decode}"))),
            CellKind::Normal => {
                let text = match self.result.cell(row_ix, col_ix) {
                    CellView::Loading => SharedString::from("…"),
                    CellView::Ready { display, .. } => SharedString::from(display),
                };
                (false, false, text)
            }
        };
        let (color, bg) = if is_decode {
            (palette.cell_decode_text, Some(palette.cell_decode_bg))
        } else if null || loading {
            (cx.theme().muted_foreground, None)
        } else {
            (cx.theme().foreground, None)
        };
        // Cell selection installs a Right mouse-down that stop_propagation, so
        // DataTable's row-level TableDelegate::context_menu never opens for
        // cells. Attach the copy menu on the cell content (innermost wins).
        // ContextMenuExt also stop_propagates, so DataTable never sees the
        // right-click — select the cell here when it is outside the current
        // selection so Copy targets the clicked cell (Refs #55).
        let table = cx.entity();
        div()
            .id(format!("grid-cell:{row_ix}:{col_ix}"))
            .h_full()
            .w_full()
            .px_2()
            .flex()
            .items_center()
            .truncate()
            .text_color(color)
            .font_family(cx.theme().mono_font_family.clone())
            .when_some(bg, |cell, bg| cell.bg(bg))
            .when(null || loading, |cell| cell.italic())
            .child(text)
            .context_menu(move |menu, _, menu_cx| {
                table.update(menu_cx, |state, cx| {
                    let selection = state.selection();
                    let range = state.delegate().selection_rect;
                    if !cell_in_selection(row_ix, col_ix, selection, range) {
                        state.set_selected_cell(row_ix, col_ix, cx);
                        // Keep selection_rect in sync immediately; SelectCell
                        // subscribers also call note_cell_focus, which is fine.
                        state.delegate_mut().note_cell_focus(row_ix, col_ix, false);
                    }
                });
                copy_context_menu(menu)
            })
    }

    fn visible_rows_changed(
        &mut self,
        visible_range: std::ops::Range<usize>,
        _: &mut Window,
        cx: &mut gpui_kit::Context<TableState<Self>>,
    ) {
        self.pending_pages = self.result.touch(visible_range);
        if !self.pending_pages.is_empty() {
            cx.notify();
        }
    }

    fn loading(&self, _: &App) -> bool {
        self.loading
    }

    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut gpui_kit::Context<TableState<Self>>,
    ) -> impl IntoElement {
        div()
            .p_4()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(self.empty_message.clone())
    }

    fn cell_text(&self, row_ix: usize, col_ix: usize, _: &App) -> String {
        match self.result.row(row_ix) {
            RowState::Ready(cells) => cells
                .get(col_ix)
                .map(|cell| cell.display().into_owned())
                .unwrap_or_else(|| "NULL".into()),
            RowState::Loading => String::new(),
        }
    }

    fn context_menu(
        &mut self,
        _row_ix: usize,
        menu: PopupMenu,
        _window: &mut Window,
        _cx: &mut gpui_kit::Context<TableState<Self>>,
    ) -> PopupMenu {
        // Row-header right-click path (when right_clicked_row is set).
        copy_context_menu(menu)
    }
}

/// Copy formats for the grid context menu (TC-COPY-01 / US-14).
fn copy_context_menu(menu: PopupMenu) -> PopupMenu {
    menu.item(PopupMenuItem::new("Copy").action(Box::new(CopySelection)))
        .separator()
        .item(PopupMenuItem::new("Copy as TSV").action(Box::new(CopyAsTsv)))
        .item(PopupMenuItem::new("Copy as CSV").action(Box::new(CopyAsCsv)))
        .item(PopupMenuItem::new("Copy as JSON").action(Box::new(CopyAsJson)))
        .item(PopupMenuItem::new("Copy as INSERT").action(Box::new(CopyAsInsert)))
}

/// Whether `(row, col)` is already covered by the DataTable selection or our
/// Shift-range. Right-click keeps the selection when this is true so a range
/// Copy still copies the whole range; otherwise the clicked cell is selected.
pub(crate) fn cell_in_selection(
    row: usize,
    col: usize,
    selection: TableSelection,
    selection_rect: Option<SelectionRect>,
) -> bool {
    if let Some(rect) = selection_rect {
        let (min_r, max_r, min_c, max_c) = rect.normalized();
        if row >= min_r && row <= max_r && col >= min_c && col <= max_c {
            return true;
        }
    }
    match selection {
        TableSelection::None => false,
        TableSelection::Cell(r, c) => r == row && c == col,
        TableSelection::Row(r) => r == row,
        TableSelection::Column(c) => c == col,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use sela_core::{ColumnMeta, QueryPage, Value};

    use super::*;

    fn page(rows: Vec<Vec<Value>>) -> QueryPage {
        QueryPage {
            columns: vec![ColumnMeta {
                name: "title".into(),
                type_name: "TEXT".into(),
            }],
            rows,
            duration: Duration::from_millis(0),
        }
    }

    #[test]
    fn paged_table_page_clears_skeleton_and_shows_cells() {
        let mut grid = GridDelegate::empty();
        grid.loading = true;
        grid.install_loaded_page(
            page(vec![
                vec![Value::Text("Rust".into())],
                vec![Value::Text("SQLite".into())],
                vec![Value::Text("GPUI".into())],
            ]),
            true,
            None,
            0,
        );
        assert!(!grid.loading);
        assert_eq!(grid.result.total_rows(), 3);
        match grid.result.row(0) {
            RowState::Ready(cells) => assert_eq!(cells[0].display().as_ref(), "Rust"),
            RowState::Loading => panic!("first row stayed on the loading placeholder"),
        }
    }

    #[test]
    fn paged_table_page_two_shows_at_row_zero_not_absolute_offset() {
        // Regression #42: page_offset 100 must not insert into ResultWindow page 1
        // while the DataTable viewport stays at row 0 (placeholders, then page-1
        // data after a scroll refetch).
        let mut grid = GridDelegate::empty();
        grid.loading = true;
        grid.install_loaded_page(
            page(vec![
                vec![Value::Text("row-0101".into())],
                vec![Value::Text("row-0102".into())],
            ]),
            true,
            Some(2500),
            100,
        );
        assert!(!grid.loading);
        assert_eq!(grid.result.total_rows(), 2);
        match grid.result.row(0) {
            RowState::Ready(cells) => assert_eq!(cells[0].display().as_ref(), "row-0101"),
            RowState::Loading => panic!("page-2 rows landed past the viewport as placeholders"),
        }
        match grid.result.row(1) {
            RowState::Ready(cells) => assert_eq!(cells[0].display().as_ref(), "row-0102"),
            RowState::Loading => panic!("second page-2 row missing at index 1"),
        }
    }

    #[test]
    fn note_query_failed_replaces_running_empty_message() {
        let mut grid = GridDelegate::empty();
        grid.set_loading_message();
        grid.loading = true;
        grid.note_query_failed();
        assert!(!grid.loading);
        assert_eq!(
            grid.empty_message.as_ref(),
            "Query failed — see the error above."
        );
        assert_eq!(grid.result.total_rows(), 0);
    }

    #[test]
    fn note_query_failed_keeps_rows_already_delivered() {
        let mut grid = GridDelegate::empty();
        grid.show_page(page(vec![vec![Value::Text("kept".into())]]));
        grid.loading = true;
        grid.note_query_failed();
        assert!(!grid.loading);
        match grid.result.row(0) {
            RowState::Ready(cells) => assert_eq!(cells[0].display().as_ref(), "kept"),
            RowState::Loading => panic!("delivered rows were cleared on failure"),
        }
    }

    #[test]
    fn note_query_cancelled_replaces_running_empty_message() {
        let mut grid = GridDelegate::empty();
        grid.set_loading_message();
        grid.loading = true;
        grid.note_query_cancelled();
        assert!(!grid.loading);
        assert_eq!(grid.empty_message.as_ref(), "Query cancelled.");
        assert_eq!(grid.result.total_rows(), 0);
    }

    #[test]
    fn note_query_cancelled_keeps_rows_already_delivered() {
        let mut grid = GridDelegate::empty();
        grid.show_page(page(vec![vec![Value::Text("kept".into())]]));
        grid.loading = true;
        grid.note_query_cancelled();
        assert!(!grid.loading);
        match grid.result.row(0) {
            RowState::Ready(cells) => assert_eq!(cells[0].display().as_ref(), "kept"),
            RowState::Loading => panic!("delivered rows were cleared on cancel"),
        }
    }

    #[test]
    fn cell_in_selection_empty_is_outside() {
        assert!(!cell_in_selection(0, 0, TableSelection::None, None));
    }

    #[test]
    fn cell_in_selection_matches_single_cell() {
        assert!(cell_in_selection(
            1,
            2,
            TableSelection::Cell(1, 2),
            Some(SelectionRect::cell(1, 2))
        ));
        assert!(!cell_in_selection(
            0,
            0,
            TableSelection::Cell(1, 2),
            Some(SelectionRect::cell(1, 2))
        ));
    }

    #[test]
    fn cell_in_selection_keeps_range_member() {
        let range = SelectionRect::from_corners(CellPos::new(0, 0), CellPos::new(2, 2));
        assert!(cell_in_selection(
            1,
            1,
            TableSelection::Cell(0, 0),
            Some(range)
        ));
        assert!(!cell_in_selection(
            3,
            3,
            TableSelection::Cell(0, 0),
            Some(range)
        ));
    }

    #[test]
    fn cell_in_selection_row_and_column() {
        assert!(cell_in_selection(4, 0, TableSelection::Row(4), None));
        assert!(!cell_in_selection(3, 0, TableSelection::Row(4), None));
        assert!(cell_in_selection(0, 2, TableSelection::Column(2), None));
        assert!(!cell_in_selection(0, 1, TableSelection::Column(2), None));
    }

    #[test]
    fn copy_rect_none_when_nothing_selected() {
        let grid = GridDelegate::empty();
        // Empty result → None; with rows but no selection also None.
        assert!(grid.copy_rect(TableSelection::None).is_none());
        let mut grid = GridDelegate::empty();
        grid.show_page(page(vec![vec![Value::Text("a".into())]]));
        assert!(grid.copy_rect(TableSelection::None).is_none());
    }
}
