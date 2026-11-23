use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Rows fetched for one page of a grid.
pub const PAGE_SIZE: u32 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    Postgres,
    Mysql,
    Sqlite,
}

impl Engine {
    pub fn label(self) -> &'static str {
        match self {
            Self::Postgres => "PostgreSQL",
            Self::Mysql => "MySQL",
            Self::Sqlite => "SQLite",
        }
    }

    pub fn short(self) -> &'static str {
        match self {
            Self::Postgres => "PG",
            Self::Mysql => "MY",
            Self::Sqlite => "SL",
        }
    }

    pub fn default_port(self) -> u16 {
        match self {
            Self::Postgres => 5432,
            Self::Mysql => 3306,
            Self::Sqlite => 0,
        }
    }

    pub fn dialect(self) -> Dialect {
        match self {
            Self::Postgres => Dialect::Postgres,
            Self::Mysql => Dialect::Mysql,
            Self::Sqlite => Dialect::Sqlite,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Postgres,
    Mysql,
    Sqlite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelationKind {
    Table,
    View,
}

impl RelationKind {
    pub fn parse(raw: &str) -> Self {
        if raw.eq_ignore_ascii_case("view") || raw.eq_ignore_ascii_case("VIEW") {
            Self::View
        } else {
            Self::Table
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaInfo {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableInfo {
    pub name: String,
    pub kind: RelationKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnMeta {
    pub name: String,
    pub type_name: String,
}

/// A grid cell. `NULL` stays distinct from the text "NULL".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cell {
    Null,
    Text(String),
}

impl Cell {
    pub fn display(&self) -> &str {
        match self {
            Self::Null => "NULL",
            Self::Text(text) => text,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }
}

#[derive(Debug, Clone)]
pub struct QueryPage {
    pub columns: Vec<ColumnMeta>,
    pub rows: Vec<Vec<Cell>>,
    pub duration: Duration,
}

#[derive(Debug, Clone)]
pub struct RowSet {
    pub page: QueryPage,
    pub total_rows: Option<u64>,
    pub duration: Duration,
    /// False when the statement could not be wrapped in `LIMIT`/`OFFSET`.
    pub paged: bool,
    pub note: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ExecOutcome {
    pub rows_affected: u64,
    pub duration: Duration,
}

#[derive(Debug, Clone)]
pub enum StatementOutcome {
    Rows(RowSet),
    Executed(ExecOutcome),
}
