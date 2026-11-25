//! Domain types and shared logic for Sela.
//!
//! This crate has no GPUI or sqlx dependency. Drivers live in `sela-drivers`;
//! saved-connection persistence lives in `sela-config`.

mod change_plan;
mod change_set;
mod column_kind;
mod connection;
mod connection_color;
mod connection_org;
mod connection_url;
mod copy_format;
mod error;
mod filter;
mod history;
mod result_window;
mod safe_mode;
mod selection;
mod session;
mod sql;
mod structure;
mod tabs;
mod types;

pub use change_plan::{ApplyChangesResult, ChangePlan, PlanOp, PlannedStatement};
pub use change_set::{CellChange, ChangeError, ChangeSet, RowChange, TableRef};
pub use column_kind::{ColumnKind, MAX_SORT_KEYS};
pub use connection::{safe_mode_driven_by_color, SafeMode, SavedConnection, TlsMode, TlsSettings};
pub use connection_color::{default_safety_level_for_color, ConnectionColor, DefaultSafetyLevel};
pub use connection_org::{
    connection_color_is_visible, duplicate_connection, duplicate_status_message,
    existing_group_names, next_duplicate_name, sidebar_connection_layout, SidebarConnectionNode,
};
pub use connection_url::{parse_connection_url, ImportUrlError, ImportedConnection};
pub use copy_format::{
    escape_csv_field, escape_sql_string, extract_selection_matrix, format_selection,
    value_to_copy_cell, CopyCell, CopyError, CopyFormat, CopyScalarKind,
};
pub use error::DbError;
pub use filter::{
    bool_filter_menu_index, bool_filter_menu_value, build_table_count, build_table_page,
    build_table_select, display_bound_sql, escape_like, escape_like_meta, filter_drafts_are_empty,
    format_predicate_preview, insert_filter_draft_after, parse_filter_value, remove_filter_draft,
    should_remove_filter_row_on_backspace, validate_filter_condition, BoundSql, FilterCondition,
    FilterDraft, FilterOp, SortDirection, SortSpec, BOOL_FILTER_VALUES, LIKE_ESCAPE,
};
pub use history::{
    format_history_duration, format_history_when, history_fts_query, history_rerun_needs_confirm,
    history_rows_label_affected, history_rows_label_for_rows, history_sql_contains_password_clause,
    history_sql_preview, should_record_history_sql, HistoryEntry, HistoryFilter, HistoryOutcome,
    HISTORY_PASSWORD_SKIP_NOTE, HISTORY_ROW_CAP,
};
pub use result_window::{CellView, ResultWindow, RowState, ROW_WINDOW_MAX_PAGES};
pub use safe_mode::{
    classify_statement_safety, decide_editor_gate, decide_grid_commit_gate, decide_history_rerun,
    EditorGate, GridCommitGate, HistoryRerunGate, NoWhereKind, StatementSafety,
};
pub use selection::{step_result_index, CellPos, SelectionRect};
pub use session::{
    default_page_size, default_sql_row_cap, exact_table_rows, preview_table, run_user_sql,
    DatabaseSession, LoadedSchema, TablePreview,
};
pub use sql::{
    bind_placeholder, classify, first_keyword, is_safe_to_rerun_for_load_more,
    line_col_to_byte_range, normalize_sql, parse_postgres_error_position, qualify, quote_ident,
    select_all, split_statements, statement_index_at, strip_comments, wrap_count, wrap_page,
    SqlError, SqlStatement, StatementKind,
};
pub use structure::{ColumnStructure, ForeignKeyInfo, IndexInfo, TableStructure};
pub use tabs::{
    count_tabs_requiring_close_confirm, tab_requires_close_confirm, PersistedTab, PersistedTabs,
    Tab, TabId, TabKind, TabList,
};
pub use types::{
    truncate_one_line, ColumnMeta, Dialect, Engine, ExecOutcome, Interval, QueryPage, RelationKind,
    RowCountKind, RowSet, SchemaInfo, StatementOutcome, StreamBatch, StreamOutcome, TableInfo,
    Value, CELL_DISPLAY_MAX_CHARS, CELL_NEWLINE_GLYPH, DEFAULT_SQL_ROW_CAP, PAGE_SIZE,
    STREAM_BATCH_SIZE,
};
