//! Bounded row window for large result sets.
//!
//! Keeps at most [`ROW_WINDOW_MAX_PAGES`] pages in memory. The grid asks for rows by
//! absolute index; unloaded rows report [`RowState::Loading`]. Call [`ResultWindow::touch`]
//! when the viewport moves to learn which pages to fetch and to evict distant pages.
//!
//! M1-2b will drive real cancel + batched streaming off this API (dropping per-page COUNT).

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use crate::types::{
    truncate_one_line, ColumnMeta, QueryPage, Value, CELL_DISPLAY_MAX_CHARS, PAGE_SIZE,
};

/// Hard cap on retained pages (configurable constant for M1-2).
pub const ROW_WINDOW_MAX_PAGES: usize = 16;

/// One cell as seen by the grid renderer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CellView {
    /// Page not loaded yet (or between eviction and refill).
    Loading,
    /// Loaded cell with a truncated, cached one-line display string.
    Ready {
        null: bool,
        /// Truncated display for the cell renderer (≤ [`CELL_DISPLAY_MAX_CHARS`] chars).
        display: Arc<str>,
    },
}

/// A full row for copy / detail views (never truncated).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RowState<'a> {
    Loading,
    Ready(&'a [Value]),
}

#[derive(Debug, Clone)]
struct CachedRow {
    cells: Vec<Value>,
    /// Lazily filled truncated display strings, parallel to `cells`.
    displays: Vec<Option<Arc<str>>>,
}

impl CachedRow {
    fn new(cells: Vec<Value>) -> Self {
        let displays = vec![None; cells.len()];
        Self { cells, displays }
    }

    fn cell_display(&mut self, col: usize) -> Option<(bool, Arc<str>)> {
        let cell = self.cells.get(col)?;
        if self.displays[col].is_none() {
            let full = cell.display().into_owned();
            let text = truncate_one_line(&full, CELL_DISPLAY_MAX_CHARS);
            self.displays[col] = Some(Arc::from(text.into_owned()));
        }
        let display = self.displays[col].clone()?;
        Some((cell.is_null(), display))
    }
}

#[derive(Debug, Clone)]
struct Page {
    rows: Vec<CachedRow>,
}

/// In-memory window over a logical result set of `total_rows` rows.
#[derive(Debug, Clone)]
pub struct ResultWindow {
    columns: Vec<ColumnMeta>,
    total_rows: usize,
    page_size: usize,
    max_pages: usize,
    pages: HashMap<usize, Page>,
    /// Last viewport used for eviction distance.
    viewport: Range<usize>,
}

impl Default for ResultWindow {
    fn default() -> Self {
        Self::empty()
    }
}

impl ResultWindow {
    pub fn empty() -> Self {
        Self {
            columns: Vec::new(),
            total_rows: 0,
            page_size: PAGE_SIZE as usize,
            max_pages: ROW_WINDOW_MAX_PAGES,
            pages: HashMap::new(),
            viewport: 0..0,
        }
    }

    pub fn with_capacity(max_pages: usize, page_size: usize) -> Self {
        Self {
            max_pages: max_pages.max(1),
            page_size: page_size.max(1),
            ..Self::empty()
        }
    }

    pub fn columns(&self) -> &[ColumnMeta] {
        &self.columns
    }

    pub fn total_rows(&self) -> usize {
        self.total_rows
    }

    pub fn page_size(&self) -> usize {
        self.page_size
    }

    pub fn max_pages(&self) -> usize {
        self.max_pages
    }

    pub fn loaded_page_count(&self) -> usize {
        self.pages.len()
    }

    pub fn loaded_page_indices(&self) -> Vec<usize> {
        let mut keys: Vec<usize> = self.pages.keys().copied().collect();
        keys.sort_unstable();
        keys
    }

    /// Reset to a new result shape; drops all cached pages.
    pub fn reset(&mut self, columns: Vec<ColumnMeta>, total_rows: usize) {
        self.columns = columns;
        self.total_rows = total_rows;
        self.pages.clear();
        self.viewport = 0..0;
    }

    /// Replace the window with a single materialized page (current prev/next UX).
    pub fn replace_with_page(&mut self, page: QueryPage) {
        let total = page.rows.len();
        self.reset(page.columns, total);
        self.insert_page(0, page.rows);
    }

    /// Insert (or replace) a page of rows. `rows` may be shorter than `page_size` on the last page.
    pub fn insert_page(&mut self, page_index: usize, rows: Vec<Vec<Value>>) {
        let cached = rows.into_iter().map(CachedRow::new).collect();
        self.pages.insert(page_index, Page { rows: cached });
        self.evict_far_from_viewport();
    }

    pub fn page_index_for_row(&self, row: usize) -> usize {
        row / self.page_size
    }

    /// Row by absolute index: [`RowState::Loading`] if the page is not resident.
    pub fn row(&self, row: usize) -> RowState<'_> {
        if row >= self.total_rows {
            return RowState::Loading;
        }
        let page_index = self.page_index_for_row(row);
        let offset = row % self.page_size;
        match self.pages.get(&page_index).and_then(|p| p.rows.get(offset)) {
            Some(cached) => RowState::Ready(&cached.cells),
            None => RowState::Loading,
        }
    }

    /// Full untruncated value for copy / detail. `None` if unloaded or OOB.
    pub fn cell_value(&self, row: usize, col: usize) -> Option<&Value> {
        match self.row(row) {
            RowState::Ready(cells) => cells.get(col),
            RowState::Loading => None,
        }
    }

    /// Grid cell view: loading placeholder or cached truncated display.
    pub fn cell(&mut self, row: usize, col: usize) -> CellView {
        if row >= self.total_rows {
            return CellView::Loading;
        }
        let page_index = self.page_index_for_row(row);
        let offset = row % self.page_size;
        let Some(page) = self.pages.get_mut(&page_index) else {
            return CellView::Loading;
        };
        let Some(cached) = page.rows.get_mut(offset) else {
            return CellView::Loading;
        };
        match cached.cell_display(col) {
            Some((null, display)) => CellView::Ready { null, display },
            None => CellView::Loading,
        }
    }

    /// Update the viewport, return page indices that still need loading, and evict far pages.
    pub fn touch(&mut self, visible: Range<usize>) -> Vec<usize> {
        let start = visible.start.min(self.total_rows);
        let end = visible.end.min(self.total_rows);
        self.viewport = start..end;
        let needed = self.pages_covering(start..end);
        self.evict_far_from_viewport();
        needed
            .into_iter()
            .filter(|page| !self.pages.contains_key(page))
            .collect()
    }

    /// Page indices that cover `rows` (inclusive of partial pages).
    pub fn pages_covering(&self, rows: Range<usize>) -> Vec<usize> {
        if rows.start >= rows.end || self.total_rows == 0 {
            return Vec::new();
        }
        let start = rows.start.min(self.total_rows.saturating_sub(1));
        let end = rows.end.min(self.total_rows);
        let first = start / self.page_size;
        let last = (end - 1) / self.page_size;
        (first..=last).collect()
    }

    fn evict_far_from_viewport(&mut self) {
        while self.pages.len() > self.max_pages {
            let Some(victim) = self.farthest_page() else {
                break;
            };
            self.pages.remove(&victim);
        }
    }

    fn farthest_page(&self) -> Option<usize> {
        if self.pages.is_empty() {
            return None;
        }
        let center = if self.viewport.start < self.viewport.end {
            (self.viewport.start + self.viewport.end.saturating_sub(1)) / 2
        } else {
            0
        };
        let center_page = center / self.page_size;
        self.pages
            .keys()
            .copied()
            .max_by_key(|page| page.abs_diff(center_page))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Value;

    fn cols() -> Vec<ColumnMeta> {
        vec![ColumnMeta {
            name: "c0".into(),
            type_name: "text".into(),
        }]
    }

    fn page_rows(page: usize, page_size: usize, width: usize) -> Vec<Vec<Value>> {
        (0..page_size)
            .map(|i| {
                let row = page * page_size + i;
                (0..width)
                    .map(|c| Value::Text(format!("r{row}-c{c}").into()))
                    .collect()
            })
            .collect()
    }

    #[test]
    fn window_loads_on_demand_and_reports_loading() {
        let mut window = ResultWindow::with_capacity(4, 10);
        window.reset(cols(), 100);
        assert!(matches!(window.row(0), RowState::Loading));
        assert_eq!(window.cell(0, 0), CellView::Loading);

        let needed = window.touch(0..5);
        assert_eq!(needed, vec![0]);
        window.insert_page(0, page_rows(0, 10, 1));

        assert!(matches!(window.row(0), RowState::Ready(_)));
        assert!(matches!(window.row(9), RowState::Ready(_)));
        assert!(matches!(window.row(10), RowState::Loading));

        match window.cell(3, 0) {
            CellView::Ready { null, display } => {
                assert!(!null);
                assert_eq!(&*display, "r3-c0");
            }
            other => panic!("expected ready, got {other:?}"),
        }
        // Second access hits the display cache (same Arc).
        let first = match window.cell(3, 0) {
            CellView::Ready { display, .. } => display,
            _ => panic!("ready"),
        };
        let second = match window.cell(3, 0) {
            CellView::Ready { display, .. } => display,
            _ => panic!("ready"),
        };
        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn eviction_keeps_pages_near_viewport() {
        let mut window = ResultWindow::with_capacity(3, 10);
        window.reset(cols(), 200);
        for page in 0..5 {
            window.insert_page(page, page_rows(page, 10, 1));
        }
        // insert_page evicts down to max_pages using viewport 0..0 → keeps lowest indices near 0.
        assert!(window.loaded_page_count() <= 3);

        window.touch(150..160);
        for page in [15, 16] {
            window.insert_page(page, page_rows(page, 10, 1));
        }
        // After focusing near row 150, the farthest early pages are gone; near pages retained.
        let loaded = window.loaded_page_indices();
        assert!(loaded.len() <= 3, "loaded={loaded:?}");
        assert!(
            loaded.contains(&15) && loaded.contains(&16),
            "loaded={loaded:?}"
        );
        assert!(!loaded.contains(&0), "page 0 should be evicted: {loaded:?}");
        assert!(matches!(window.row(150), RowState::Ready(_)));
        assert!(matches!(window.row(0), RowState::Loading));
    }

    #[test]
    fn replace_with_page_sets_total_to_page_len() {
        let mut window = ResultWindow::empty();
        window.replace_with_page(QueryPage {
            columns: cols(),
            rows: page_rows(0, 7, 1),
            duration: std::time::Duration::ZERO,
        });
        assert_eq!(window.total_rows(), 7);
        assert_eq!(window.loaded_page_count(), 1);
        assert!(matches!(window.row(6), RowState::Ready(_)));
        assert!(matches!(window.row(7), RowState::Loading));
    }

    #[test]
    fn cell_value_returns_full_untruncated_text() {
        let mut window = ResultWindow::with_capacity(2, 10);
        window.reset(cols(), 10);
        let long = format!("head\n{}", "a".repeat(500));
        window.insert_page(0, vec![vec![Value::Text(long.clone().into())]]);
        let full = window.cell_value(0, 0).unwrap();
        assert_eq!(full.display().as_ref(), long);
        match window.cell(0, 0) {
            CellView::Ready { display, .. } => {
                assert!(display.chars().count() <= CELL_DISPLAY_MAX_CHARS + 1); // + ellipsis
                assert!(!display.contains('\n'));
                assert!(display.starts_with("head⏎"));
                assert!(display.ends_with('…'));
            }
            other => panic!("{other:?}"),
        }
    }
}
