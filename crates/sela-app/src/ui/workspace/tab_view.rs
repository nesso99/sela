//! Per-tab UI state: grid, SQL editor, Structure pane, and query bookkeeping.

use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use std::time::Instant;

use gpui_kit::component::input::{
    EditorState, GutterDecorationCollection, InputState, TextDecorationCollection,
};
use gpui_kit::component::table::TableState;
use gpui_kit::prelude::Focusable as _;
use gpui_kit::{px, AppContext as _, Context, Entity, SharedString, Window};

use sela_core::{FilterCondition, ResultWindow, RowCountKind, SortSpec, DEFAULT_SQL_ROW_CAP};

use super::super::grid::GridDelegate;
use super::filter_bar::FilterRowUi;
use super::structure::StructureCache;
use super::{GridSource, TablePane};

pub const DEFAULT_EDITOR_HEIGHT: f32 = 176.;
pub const MIN_EDITOR_HEIGHT: f32 = 72.;
pub const COLLAPSED_EDITOR_HEIGHT: f32 = 28.;
pub const MAX_RESULT_SNAPSHOTS: usize = 8;

#[derive(Clone)]
pub(super) struct SqlResultSnapshot {
    pub statement_index: usize,
    pub result: ResultWindow,
    pub page_offset: u64,
    pub page_len: usize,
    pub total_rows: Option<u64>,
    pub row_count_kind: RowCountKind,
    pub paged: bool,
    pub truncated: bool,
    pub can_load_more: bool,
    pub note: Option<String>,
    pub status: String,
    pub failed: bool,
    pub empty_message: SharedString,
    pub loading: bool,
}

pub(super) struct TabView {
    pub grid: Entity<TableState<GridDelegate>>,
    pub sql: Entity<EditorState>,
    pub sql_gutter: GutterDecorationCollection,
    pub sql_error_marks: TextDecorationCollection,
    pub find_input: Entity<InputState>,
    pub editor_height: f32,
    pub editor_collapsed: bool,
    pub find_open: bool,
    pub find_matches: Vec<std::ops::Range<usize>>,
    pub find_match_ix: usize,
    pub source: GridSource,
    pub table_pane: TablePane,
    pub structure_cache: StructureCache,
    pub structure_error: Option<String>,
    pub page_offset: u64,
    pub page_len: usize,
    pub total_rows: Option<u64>,
    pub row_count_kind: RowCountKind,
    pub paged: bool,
    pub truncated: bool,
    pub can_load_more: bool,
    pub sql_row_cap: u32,
    pub live_row_cap: Option<Arc<AtomicUsize>>,
    pub status: String,
    pub error: Option<String>,
    pub note: Option<String>,
    pub busy: bool,
    pub query_started: Option<Instant>,
    pub sql_run_queue: Vec<String>,
    pub sql_run_base_index: usize,
    pub sql_run_statement_index: usize,
    pub sql_running_statement_index: Option<usize>,
    pub sql_failed_statement: Option<usize>,
    pub result_snapshots: Vec<SqlResultSnapshot>,
    pub active_result_index: usize,
    pub local_sql_error: Option<sela_core::SqlError>,
    pub last_error_sqlstate: Option<String>,
    pub filters: Vec<FilterCondition>,
    pub sorts: Vec<SortSpec>,
    pub filter_bar_open: bool,
    pub filter_rows: Vec<FilterRowUi>,
}

impl TabView {
    pub fn new(window: &mut Window, cx: &mut Context<super::Workspace>, sql_text: &str) -> Self {
        let grid = cx.new(|cx| {
            TableState::new(GridDelegate::empty(), window, cx)
                .sortable(false)
                .col_movable(false)
        });
        let sql = cx.new(|cx| {
            let mut state = EditorState::new(window, cx)
                .default_value(sql_text)
                .placeholder("SELECT * FROM books\n-- Ctrl+Enter or ⌘+Enter runs the statement");
            state.set_gutter_decorations_width(px(2.), cx);
            state
        });
        let (sql_gutter, sql_error_marks) = sql.update(cx, |state, cx| {
            (
                state.create_gutter_decorations_collection(Vec::new(), cx),
                state.create_decorations_collection(Vec::new(), cx),
            )
        });
        let find_input = cx.new(|cx| InputState::new(window, cx).placeholder("Find in SQL"));
        Self {
            grid,
            sql,
            sql_gutter,
            sql_error_marks,
            find_input,
            editor_height: DEFAULT_EDITOR_HEIGHT,
            editor_collapsed: false,
            find_open: false,
            find_matches: Vec::new(),
            find_match_ix: 0,
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
            sql_run_queue: Vec::new(),
            sql_run_base_index: 0,
            sql_run_statement_index: 0,
            sql_running_statement_index: None,
            sql_failed_statement: None,
            result_snapshots: Vec::new(),
            active_result_index: 0,
            local_sql_error: None,
            last_error_sqlstate: None,
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

    pub fn editor_body_height(&self) -> f32 {
        if self.editor_collapsed {
            COLLAPSED_EDITOR_HEIGHT
        } else {
            self.editor_height.max(MIN_EDITOR_HEIGHT)
        }
    }

    pub fn editor_focused(&self, window: &Window, cx: &gpui_kit::App) -> bool {
        self.sql.read(cx).focus_handle(cx).is_focused(window)
    }

    pub fn find_field_focused(&self, window: &Window, cx: &gpui_kit::App) -> bool {
        self.find_input.read(cx).focus_handle(cx).is_focused(window)
    }
}
