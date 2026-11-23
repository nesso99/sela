//! Domain types and shared logic for Sela.
//!
//! This crate has no GPUI or sqlx dependency. Drivers live in `sela-drivers`;
//! saved-connection persistence lives in `sela-config`.

mod connection;
mod error;
mod result_window;
mod session;
mod sql;
mod structure;
mod tabs;
mod types;

pub use connection::{SavedConnection, TlsMode, TlsSettings};
pub use error::DbError;
pub use result_window::{CellView, ResultWindow, RowState, ROW_WINDOW_MAX_PAGES};
pub use session::{
    default_page_size, default_sql_row_cap, exact_table_rows, preview_table, run_user_sql,
    DatabaseSession, LoadedSchema,
};
pub use sql::{
    classify, first_keyword, is_safe_to_rerun_for_load_more, normalize_sql, qualify, quote_ident,
    select_all, split_statements, strip_comments, wrap_count, wrap_page, SqlError, StatementKind,
};
pub use structure::{ColumnStructure, ForeignKeyInfo, IndexInfo, TableStructure};
pub use tabs::{PersistedTab, PersistedTabs, Tab, TabId, TabKind, TabList};
pub use types::{
    truncate_one_line, ColumnMeta, Dialect, Engine, ExecOutcome, Interval, QueryPage, RelationKind,
    RowCountKind, RowSet, SchemaInfo, StatementOutcome, StreamBatch, StreamOutcome, TableInfo,
    Value, CELL_DISPLAY_MAX_CHARS, CELL_NEWLINE_GLYPH, DEFAULT_SQL_ROW_CAP, PAGE_SIZE,
    STREAM_BATCH_SIZE,
};
