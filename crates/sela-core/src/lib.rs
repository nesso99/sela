//! Domain types and shared logic for Sela.
//!
//! This crate has no GPUI or sqlx dependency. Drivers live in `sela-drivers`;
//! saved-connection persistence lives in `sela-config`.

mod connection;
mod error;
mod session;
mod sql;
mod types;

pub use connection::SavedConnection;
pub use error::DbError;
pub use session::{preview_table, run_user_sql, DatabaseSession, LoadedSchema};
pub use sql::{
    classify, first_keyword, normalize_sql, qualify, quote_ident, select_all, split_statements,
    strip_comments, wrap_count, wrap_page, SqlError, StatementKind,
};
pub use types::{
    ColumnMeta, Dialect, Engine, ExecOutcome, Interval, QueryPage, RelationKind, RowSet,
    SchemaInfo, StatementOutcome, TableInfo, Value, PAGE_SIZE,
};
