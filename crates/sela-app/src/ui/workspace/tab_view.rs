//! Per-tab UI state: grid, SQL editor, Structure pane, and query bookkeeping.
//!
//! Logical tab identity / cancel / dirty live in [`sela_core::TabList`].
//! This struct holds the GPUI entities and the ResultWindow-facing fields.
//! Data/Structure mode and the structure cache are per tab so a load for one
//! tab never paints into another.

use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use std::time::Instant;

use gpui_kit::component::input::TextareaState;
use gpui_kit::component::table::TableState;
use gpui_kit::{AppContext as _, Context, Entity, Window};

use sela_core::{FilterCondition, RowCountKind, SortSpec, DEFAULT_SQL_ROW_CAP};

use super::super::grid::GridDelegate;
use super::filter_bar::FilterRowUi;
use super::structure::StructureCache;
use super::{GridSource, TablePane};

pub(super) struct TabView {
    pub grid: Entity<TableState<GridDelegate>>,
    pub sql: Entity<TextareaState>,
    pub source: GridSource,
    /// Data vs Structure for table tabs (ignored for query-only sources).
    pub table_pane: TablePane,
    pub structure_cache: StructureCache,
    pub structure_error: Option<String>,
    pub page_offset: u64,
    pub page_len: usize,
    pub total_rows: Option<u64>,
    pub row_count_kind: RowCountKind,
    pub paged: bool,
    pub truncated: bool,
    /// When true, the Load more button may raise a live stream cap or re-run a
    /// read-only statement. Cleared for DML / locking SQL that must not re-run.
    pub can_load_more: bool,
    pub sql_row_cap: u32,
    pub live_row_cap: Option<Arc<AtomicUsize>>,
    pub status: String,
    pub error: Option<String>,
    pub note: Option<String>,
    pub busy: bool,
    pub query_started: Option<Instant>,
    /// Active filters for table previews (AND-combined).
    pub filters: Vec<FilterCondition>,
    /// Active ORDER BY for table previews.
    pub sorts: Vec<SortSpec>,
    /// Whether the filter bar is visible.
    pub filter_bar_open: bool,
    /// Editable filter rows (UI state; synced into `filters` on Apply).
    pub filter_rows: Vec<FilterRowUi>,
}

impl TabView {
    pub fn new(window: &mut Window, cx: &mut Context<super::Workspace>, sql_text: &str) -> Self {
        let grid = cx.new(|cx| {
            TableState::new(GridDelegate::empty(), window, cx)
                .sortable(true)
                .col_movable(false)
        });
        let sql = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(6)
                .default_value(sql_text)
                .placeholder("SELECT * FROM books\n-- Ctrl+Enter or ⌘+Enter runs the statement")
        });
        Self {
            grid,
            sql,
            source: GridSource::Idle,
            table_pane: TablePane::Data,
            structure_cache: StructureCache::new(),
            structure_error: None,
            page_offset: 0,
            page_len: 0,
            total_rows: None,
            row_count_kind: RowCountKind::Unknown,
            paged: false,
            truncated: false,
            can_load_more: false,
            sql_row_cap: DEFAULT_SQL_ROW_CAP,
            live_row_cap: None,
            status: String::new(),
            error: None,
            note: None,
            busy: false,
            query_started: None,
            filters: Vec::new(),
            sorts: Vec::new(),
            filter_bar_open: false,
            filter_rows: Vec::new(),
        }
    }

    pub fn source_label(&self) -> String {
        match &self.source {
            GridSource::Idle => "No table open".into(),
            GridSource::Table { schema, table } => format!("{schema}.{table}"),
            GridSource::Sql(_) => "SQL result".into(),
        }
    }

    pub fn clear_structure(&mut self) {
        self.structure_cache.clear();
        self.structure_error = None;
    }
}
