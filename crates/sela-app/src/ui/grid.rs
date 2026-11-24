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
use gpui_kit::{div, px, App, IntoElement, MouseButton, SharedString, Window};

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
    /// Shift held on the last left mouse-down inside a cell. Prefer this over
    /// `window.modifiers()` when handling [`TableEvent::SelectCell`], because
    /// the click subscriber can run after modifiers have already changed.
    /// Also set by Shift+arrow handlers before they call `set_selected_cell`.
    pointer_shift: Option<bool>,
    /// When true, the next [`TableEvent::SelectCell`] must not call
    /// [`Self::note_cell_focus`] with `shift = false` (which would collapse an
    /// intentional range such as Select-All). Consumed by
    /// [`Self::take_suppress_select_collapse`].
    suppress_select_collapse: bool,
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
            pointer_shift: None,
            suppress_select_collapse: false,
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
        self.pointer_shift = None;
        self.suppress_select_collapse = false;
    }

    /// Remember whether Shift was held on the pointer press that is about to
    /// select a cell (see [`Self::take_shift_for_select`]).
    pub fn note_pointer_shift(&mut self, shift: bool) {
        self.pointer_shift = Some(shift);
    }

    /// Consume a pointer-captured Shift flag, if any.
    pub fn take_shift_for_select(&mut self) -> Option<bool> {
        self.pointer_shift.take()
    }

    /// Ignore the next non-Shift [`TableEvent::SelectCell`] collapse.
    ///
    /// `TableState::set_selected_cell` emits `SelectCell` after the current
    /// update returns; without this flag, that deferred handler would call
    /// [`Self::note_cell_focus`] with `shift = false` and wipe a range that was
    /// just set (Shift+arrow, right-click keep-range).
    pub fn suppress_next_select_collapse(&mut self) {
        self.suppress_select_collapse = true;
    }

    /// Consume the range-keep suppress flag.
    pub fn take_suppress_select_collapse(&mut self) -> bool {
        let flag = self.suppress_select_collapse;
        self.suppress_select_collapse = false;
        flag
    }

    /// Apply a [`TableEvent::SelectCell`] the same way the workspace subscriber
    /// does: honor suppress, then pointer-captured Shift, then `window_shift`.
    ///
    /// Unit tests use this to drive the real post-`set_selected_cell` order
    /// without a live GPUI window.
    pub fn apply_select_cell_event(&mut self, row: usize, col: usize, window_shift: bool) {
        if self.take_suppress_select_collapse() {
            return;
        }
        let shift = self.take_shift_for_select().unwrap_or(window_shift);
        self.note_cell_focus(row, col, shift);
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

    /// Select every loaded cell (Ctrl/Cmd+A).
    pub fn select_all_cells(&mut self) {
        let rows = self.result.total_rows();
        let cols = self.result.columns().len();
        if rows == 0 || cols == 0 {
            self.clear_selection_range();
            return;
        }
        self.selection_anchor = Some(CellPos::new(0, 0));
        self.selection_rect = Some(SelectionRect::from_corners(
            CellPos::new(0, 0),
            CellPos::new(rows - 1, cols - 1),
        ));
        self.pointer_shift = None;
    }

    /// Move the focus cell by `(d_row, d_col)` and extend `selection_rect`.
    ///
    /// Used by Shift+arrow. Returns the new focus when the move (or a no-op at
    /// the edge that still refreshes the range) should call
    /// `TableState::set_selected_cell`. The caller must set
    /// [`Self::note_pointer_shift`]`(true)` before `set_selected_cell` so the
    /// deferred `SelectCell` subscriber extends instead of collapsing.
    pub fn focus_after_shift_step(
        &self,
        row: usize,
        col: usize,
        d_row: isize,
        d_col: isize,
    ) -> Option<(usize, usize)> {
        let rows = self.result.total_rows();
        let cols = self.result.columns().len();
        if rows == 0 || cols == 0 {
            return None;
        }
        let new_row = (row as isize + d_row).clamp(0, rows as isize - 1) as usize;
        let new_col = (col as isize + d_col).clamp(0, cols as isize - 1) as usize;
        Some((new_row, new_col))
    }

    /// Whether `(row, col)` lies inside the active Shift-range (if any).
    pub fn cell_in_range(&self, row: usize, col: usize) -> bool {
        self.selection_rect.is_some_and(|rect| {
            let (min_r, max_r, min_c, max_c) = rect.normalized();
            row >= min_r && row <= max_r && col >= min_c && col <= max_c
        })
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
        // Omit an empty type (SQLite undeclared / all-NULL expressions) so the
        // header is just the column name — never show "NULL" as a type.
        let header = match (self.sort_mark(&meta.name), meta.type_name.is_empty()) {
            (Some(mark), true) => format!("{} {mark}", meta.name),
            (Some(mark), false) => format!("{} {mark}    {}", meta.name, meta.type_name),
            (None, true) => meta.name.clone(),
            (None, false) => format!("{}   {}", meta.name, meta.type_name),
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
        let in_range = self.cell_in_range(row_ix, col_ix);
        let (color, decode_bg) = if is_decode {
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
        // selection so Copy targets the clicked cell (Refs #55). When the click
        // is already inside `selection_rect`, leave the range alone (#59).
        let table = cx.entity();
        let table_shift = table.clone();
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
            .when_some(decode_bg, |cell, bg| cell.bg(bg))
            // Paint the Shift-range wash under cell text (DataTable only
            // highlights the focused cell).
            .when(in_range && !is_decode, |cell| {
                cell.bg(cx.theme().tokens.table_active)
            })
            .when(null || loading, |cell| cell.italic())
            .child(text)
            // Capture Shift on press before SelectCell runs; window.modifiers()
            // alone is not reliable by the time the table event subscriber fires.
            .on_mouse_down(MouseButton::Left, move |event, _, cx| {
                let shift = event.modifiers.shift;
                table_shift.update(cx, |state, _| {
                    state.delegate_mut().note_pointer_shift(shift);
                });
            })
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
                    // Inside an existing range: do not call set_selected_cell or
                    // note_cell_focus — that would collapse the range (#59 / #55).
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

    fn multi_page() -> QueryPage {
        QueryPage {
            columns: vec![
                ColumnMeta {
                    name: "id".into(),
                    type_name: "INTEGER".into(),
                },
                ColumnMeta {
                    name: "title".into(),
                    type_name: "TEXT".into(),
                },
            ],
            rows: vec![
                vec![Value::Int(1), Value::Text("a".into())],
                vec![Value::Int(2), Value::Text("b".into())],
                vec![Value::Int(3), Value::Text("c".into())],
            ],
            duration: Duration::from_millis(0),
        }
    }

    #[test]
    fn shift_click_extends_selection_rect_from_anchor() {
        let mut grid = GridDelegate::empty();
        grid.show_page(multi_page());
        grid.note_cell_focus(0, 0, false);
        grid.note_cell_focus(2, 1, true);
        let rect = grid.selection_rect.expect("range");
        assert_eq!(rect.normalized(), (0, 2, 0, 1));
        assert!(grid.cell_in_range(1, 0));
        assert!(!grid.cell_in_range(0, 2));
    }

    #[test]
    fn select_all_cells_covers_full_grid() {
        let mut grid = GridDelegate::empty();
        grid.show_page(multi_page());
        grid.select_all_cells();
        let rect = grid.selection_rect.expect("all");
        assert_eq!(rect.normalized(), (0, 2, 0, 1));
        let copy = grid
            .copy_rect(TableSelection::Cell(0, 0))
            .expect("copy all");
        assert_eq!(copy.normalized(), (0, 2, 0, 1));
    }

    #[test]
    fn right_click_inside_range_keeps_selection_rect() {
        let mut grid = GridDelegate::empty();
        grid.show_page(multi_page());
        grid.note_cell_focus(0, 0, false);
        grid.note_cell_focus(2, 1, true);
        let before = grid.selection_rect;
        // Simulate the context-menu path: only select when outside.
        assert!(cell_in_selection(
            1,
            0,
            TableSelection::Cell(2, 1),
            grid.selection_rect
        ));
        // Inside → do not call note_cell_focus(false).
        assert_eq!(grid.selection_rect, before);
        // Outside → collapse to the clicked cell.
        assert!(!cell_in_selection(
            0,
            0,
            TableSelection::Cell(2, 1),
            // Use a range that does not include (0,0) after moving focus only —
            // with the full range (0,0) is inside; pick a cell past the grid.
            Some(SelectionRect::from_corners(
                CellPos::new(1, 0),
                CellPos::new(2, 1)
            ))
        ));
        grid.note_cell_focus(0, 0, false);
        assert_eq!(grid.selection_rect, Some(SelectionRect::cell(0, 0)));
    }

    #[test]
    fn pointer_shift_preferred_over_window_modifiers() {
        let mut grid = GridDelegate::empty();
        grid.note_pointer_shift(true);
        assert_eq!(grid.take_shift_for_select(), Some(true));
        assert_eq!(grid.take_shift_for_select(), None);
    }

    /// RC6 Ctrl+A failure: `set_selected_cell(0, 0)` emits `SelectCell` after
    /// `select_all_cells` returns; without suppress, that collapses the range.
    #[test]
    fn select_all_then_deferred_select_cell_without_suppress_collapses() {
        let mut grid = GridDelegate::empty();
        grid.show_page(multi_page());
        grid.note_cell_focus(1, 0, false);
        grid.select_all_cells();
        assert_eq!(grid.selection_rect.expect("all").normalized(), (0, 2, 0, 1));
        // Deferred SelectCell(0, 0) with shift=false — the RC6 bug path.
        grid.apply_select_cell_event(0, 0, false);
        assert_eq!(
            grid.selection_rect.expect("collapsed"),
            SelectionRect::cell(0, 0)
        );
    }

    #[test]
    fn select_all_then_plain_click_collapses_to_clicked_cell() {
        let mut grid = GridDelegate::empty();
        grid.show_page(multi_page());
        grid.note_cell_focus(1, 0, false);
        // Ctrl/Cmd+A path: select_all_cells only, no suppress flag armed.
        grid.select_all_cells();
        assert_eq!(grid.selection_rect.expect("all").normalized(), (0, 2, 0, 1));
        // The user's next plain click must collapse to the clicked cell.
        grid.apply_select_cell_event(2, 1, false);
        assert_eq!(
            grid.selection_rect.expect("single"),
            SelectionRect::cell(2, 1)
        );
    }

    #[test]
    fn suppress_is_one_shot_after_deferred_select_cell() {
        let mut grid = GridDelegate::empty();
        grid.show_page(multi_page());
        grid.note_cell_focus(0, 0, false);
        grid.note_cell_focus(2, 1, true);
        // Armed right before a set_selected_cell that emits SelectCell.
        grid.suppress_next_select_collapse();
        grid.apply_select_cell_event(2, 1, false);
        assert_eq!(grid.selection_rect.expect("kept").normalized(), (0, 2, 0, 1));
        // A later plain SelectCell still collapses.
        grid.apply_select_cell_event(1, 0, false);
        assert_eq!(
            grid.selection_rect.expect("single"),
            SelectionRect::cell(1, 0)
        );
    }

    /// RC6 Shift+Down failure: DataTable never sees `shift-down`, so we drive
    /// the move ourselves with a pending Shift flag (or suppress + sync extend).
    #[test]
    fn shift_arrow_keyboard_path_extends_via_pending_shift() {
        let mut grid = GridDelegate::empty();
        grid.show_page(multi_page());
        grid.note_cell_focus(1, 1, false);
        // First Shift+Down: pending shift + SelectCell to row 2.
        let (r, c) = grid.focus_after_shift_step(1, 1, 1, 0).expect("step");
        assert_eq!((r, c), (2, 1));
        grid.note_pointer_shift(true);
        // window_shift=false proves the pending flag is what extends the range.
        grid.apply_select_cell_event(r, c, false);
        assert_eq!(
            grid.selection_rect.expect("range").normalized(),
            (1, 2, 1, 1)
        );
        // Second Shift+Down: grow to rows 1..=2 still (already at last row).
        let (r2, c2) = grid.focus_after_shift_step(2, 1, 1, 0).expect("edge");
        assert_eq!((r2, c2), (2, 1));
        grid.note_pointer_shift(true);
        grid.apply_select_cell_event(r2, c2, false);
        assert_eq!(
            grid.selection_rect.expect("edge").normalized(),
            (1, 2, 1, 1)
        );
    }

    #[test]
    fn shift_arrow_twice_from_row0_grows_two_rows() {
        let mut grid = GridDelegate::empty();
        grid.show_page(multi_page());
        grid.note_cell_focus(0, 0, false);
        for step in 1..=2 {
            let (r, c) = grid
                .focus_after_shift_step(step - 1, 0, 1, 0)
                .expect("step");
            assert_eq!((r, c), (step, 0));
            grid.note_pointer_shift(true);
            grid.suppress_next_select_collapse();
            // Production path: sync extend, then deferred SelectCell is suppressed.
            grid.note_cell_focus(r, c, true);
            let _ = grid.take_shift_for_select();
            grid.apply_select_cell_event(r, c, false);
        }
        assert_eq!(
            grid.selection_rect.expect("grown").normalized(),
            (0, 2, 0, 0)
        );
    }

    #[test]
    fn shift_click_still_extends_after_keyboard_helpers() {
        let mut grid = GridDelegate::empty();
        grid.show_page(multi_page());
        grid.note_cell_focus(0, 0, false);
        grid.note_pointer_shift(true);
        grid.apply_select_cell_event(2, 1, false);
        assert_eq!(
            grid.selection_rect.expect("click range").normalized(),
            (0, 2, 0, 1)
        );
    }
}
