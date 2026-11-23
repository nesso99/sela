//! Virtualized result grid backed by gpui-component's `DataTable`.
//!
//! Rows come from [`sela_core::ResultWindow`]: the grid asks by absolute index and
//! paints a placeholder while the page is loading. Display strings are truncated
//! and cached inside the window; copy uses the full [`Value::display`].

use gpui_kit::component::table::{Column, TableDelegate, TableState};
use gpui_kit::component::ActiveTheme;
use gpui_kit::prelude::*;
use gpui_kit::{div, px, App, IntoElement, SharedString, Window};

use sela_core::{CellView, QueryPage, ResultWindow, RowState};

pub struct GridDelegate {
    pub result: ResultWindow,
    /// Page indices requested by the last viewport touch (M1-2b will fetch these).
    pub pending_pages: Vec<usize>,
    pub loading: bool,
    pub empty_message: SharedString,
}

impl GridDelegate {
    pub fn empty() -> Self {
        Self {
            result: ResultWindow::empty(),
            pending_pages: Vec::new(),
            loading: false,
            empty_message: "Open a table, or run a query.".into(),
        }
    }

    pub fn show_page(&mut self, page: QueryPage) {
        self.result.replace_with_page(page);
        self.pending_pages.clear();
        self.loading = false;
        if self.result.total_rows() == 0 {
            self.empty_message = "The query returned no rows.".into();
        }
    }

    pub fn show_message(&mut self, message: impl Into<SharedString>) {
        self.result = ResultWindow::empty();
        self.pending_pages.clear();
        self.loading = false;
        self.empty_message = message.into();
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
        let header = format!("{}   {}", meta.name, meta.type_name);
        let width = (header.chars().count() as f32 * 7.5).clamp(140.0, 340.0);
        Column::new(format!("{col_ix}:{}", meta.name), header)
            .width(px(width))
            .resizable(true)
            .movable(false)
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut gpui_kit::Context<TableState<Self>>,
    ) -> impl IntoElement {
        let view = self.result.cell(row_ix, col_ix);
        let (null, loading, text) = match view {
            CellView::Loading => (false, true, SharedString::from("…")),
            // Prefer Arc→SharedString when available; fall back to borrowed str.
            CellView::Ready { null, display } => (null, false, SharedString::from(display)),
        };
        let color = if null || loading {
            cx.theme().muted_foreground
        } else {
            cx.theme().foreground
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
        // Copy / export: full untruncated value.
        match self.result.row(row_ix) {
            RowState::Ready(cells) => cells
                .get(col_ix)
                .map(|cell| cell.display().into_owned())
                .unwrap_or_else(|| "NULL".into()),
            RowState::Loading => String::new(),
        }
    }
}
