//! Per-tab UI state: grid, SQL editor, Structure pane, and query bookkeeping.

use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use std::time::Instant;

use gpui_kit::component::input::{
    EditorState, GutterDecorationCollection, InputState, TextDecorationCollection,
};
use gpui_kit::component::table::TableState;
use gpui_kit::{px, AppContext as _, Entity, Focusable as _, SharedString, Window};

use sela_core::{FilterCondition, ResultWindow, RowCountKind, SortSpec, DEFAULT_SQL_ROW_CAP};

use super::super::grid::GridDelegate;
use super::edit_session::EditSession;
use super::filter_bar::FilterRowUi;
use super::structure::StructureCache;
use super::{GridSource, TablePane};

pub const DEFAULT_EDITOR_HEIGHT: f32 = 176.;
pub const MIN_EDITOR_HEIGHT: f32 = 72.;
pub const COLLAPSED_EDITOR_HEIGHT: f32 = 28.;
pub const MAX_RESULT_SNAPSHOTS: usize = 8;

#[derive(Clone)]
pub(crate) struct SqlResultSnapshot {
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

pub(crate) struct TabView {
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
    /// Duration of the last completed query/count, used by the status `ms`
    /// when the tab is idle (Structure → Data must not use wall time since
    /// `query_started`, #95).
    pub last_query_ms: u128,
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
    /// When true, the in-flight SQL statement came from the editor run path and
    /// should be written to history when it finishes (not load-more / catalog).
    pub history_should_record: bool,
    /// Set when a finished statement was skipped because it looks like
    /// password-setting DDL; flushed into the status strip after the outcome.
    pub history_password_note_pending: bool,
    /// M2-1 / M2-3 grid editing session (table Data only).
    pub edit: Option<EditSession>,
    /// Shared input for the open in-cell editor.
    pub cell_editor: Entity<InputState>,
    /// Multi-line editor for long values (multi-line text, JSON; #72).
    pub popover_editor: Entity<EditorState>,
    /// Popover offset from the results-area origin, set when the editor opens.
    pub popover_anchor: (f32, f32),
    /// Set after a successful commit while the page reload is in flight.
    pub post_commit_reload: bool,
    /// Open cell inspect popover (Space; 01-main-window.md).
    pub cell_inspect: Option<CellInspect>,
}

/// Payload for the Space inspect popover.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CellInspect {
    pub column: String,
    pub type_name: String,
    pub body: String,
    pub decode_error: bool,
    /// Offset from the results-area origin to the focused cell (07-grid-editing.md).
    pub anchor_left: f32,
    pub anchor_top: f32,
}

impl TabView {
    pub fn new(window: &mut Window, cx: &mut gpui_kit::App, sql_text: &str) -> Self {
        let grid = cx.new(|cx| {
            TableState::new(GridDelegate::empty(), window, cx)
                .sortable(false)
                .col_movable(false)
                .cell_selectable(true)
                .row_selectable(true)
                .col_selectable(true)
                .row_header(true)
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
        let cell_editor = cx.new(|cx| InputState::new(window, cx).placeholder("NULL"));
        let popover_editor = cx.new(|cx| EditorState::new(window, cx).placeholder("NULL"));
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
            last_query_ms: 0,
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
            history_should_record: false,
            history_password_note_pending: false,
            edit: None,
            cell_editor,
            popover_editor,
            popover_anchor: (8.0, 8.0),
            post_commit_reload: false,
            cell_inspect: None,
        }
    }

    pub fn source_label(&self) -> String {
        match &self.source {
            GridSource::Idle => "No table open".into(),
            GridSource::Table { schema, table, .. } => format!("{schema}.{table}"),
            GridSource::Sql(_) => "SQL result".into(),
        }
    }

    pub fn cell_editor_focused(&self, window: &Window, cx: &gpui_kit::App) -> bool {
        self.cell_editor
            .read(cx)
            .focus_handle(cx)
            .is_focused(window)
            || self
                .popover_editor
                .read(cx)
                .focus_handle(cx)
                .is_focused(window)
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
