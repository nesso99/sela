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

use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
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

    pub fn show_message(&mut self, message: impl Into<SharedString>) {
        self.result = ResultWindow::empty();
        self.pending_pages.clear();
        self.loading = false;
        self.clear_selection_range();
        self.empty_message = message.into();
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
        div()
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
        menu.item(PopupMenuItem::new("Copy as TSV").action(Box::new(CopyAsTsv)))
            .item(PopupMenuItem::new("Copy as CSV").action(Box::new(CopyAsCsv)))
            .item(PopupMenuItem::new("Copy as JSON").action(Box::new(CopyAsJson)))
            .item(PopupMenuItem::new("Copy as INSERT").action(Box::new(CopyAsInsert)))
            .separator()
            .item(PopupMenuItem::new("Copy").action(Box::new(CopySelection)))
    }
}
