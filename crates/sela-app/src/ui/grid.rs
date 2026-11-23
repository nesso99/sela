//! Virtualized result grid backed by gpui-component's `DataTable`.

use gpui_kit::component::table::{Column, TableDelegate, TableState};
use gpui_kit::component::ActiveTheme;
use gpui_kit::prelude::*;
use gpui_kit::{div, px, App, IntoElement, SharedString, Window};

use sela_core::{Cell, ColumnMeta, QueryPage};

pub struct GridDelegate {
    pub columns: Vec<ColumnMeta>,
    pub rows: Vec<Vec<Cell>>,
    pub loading: bool,
    pub empty_message: SharedString,
}

impl GridDelegate {
    pub fn empty() -> Self {
        Self {
            columns: Vec::new(),
            rows: Vec::new(),
            loading: false,
            empty_message: "Open a table, or run a query.".into(),
        }
    }

    pub fn show_page(&mut self, page: QueryPage) {
        self.columns = page.columns;
        self.rows = page.rows;
        self.loading = false;
        if self.rows.is_empty() {
            self.empty_message = "The query returned no rows.".into();
        }
    }

    pub fn show_message(&mut self, message: impl Into<SharedString>) {
        self.columns.clear();
        self.rows.clear();
        self.loading = false;
        self.empty_message = message.into();
    }
}

impl TableDelegate for GridDelegate {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        let meta = &self.columns[col_ix];
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
        let cell = self
            .rows
            .get(row_ix)
            .and_then(|row| row.get(col_ix))
            .cloned()
            .unwrap_or(Cell::Null);
        let null = cell.is_null();
        let color = if null {
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
            .when(null, |cell| cell.italic())
            .child(cell.display().to_string())
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
        self.rows
            .get(row_ix)
            .and_then(|row| row.get(col_ix))
            .map(Cell::display)
            .unwrap_or("NULL")
            .to_string()
    }
}
