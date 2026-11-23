//! Read-only table structure: columns, indexes, foreign keys, and DDL.

/// One column from [`TableStructure`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnStructure {
    pub name: String,
    pub type_name: String,
    pub nullable: bool,
    pub default: Option<String>,
    pub comment: Option<String>,
}

/// An index on a table (including the primary key when the engine reports it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexInfo {
    pub name: String,
    pub columns: Vec<String>,
    pub unique: bool,
    pub primary: bool,
    /// Engine-provided definition (`CREATE INDEX …` / `pg_get_indexdef`), when available.
    pub definition: Option<String>,
}

/// A foreign-key relationship.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignKeyInfo {
    pub name: String,
    pub columns: Vec<String>,
    pub ref_schema: Option<String>,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
    pub on_update: Option<String>,
    pub on_delete: Option<String>,
}

/// Full read-only description of a table for the Structure view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableStructure {
    pub schema: String,
    pub table: String,
    pub columns: Vec<ColumnStructure>,
    pub indexes: Vec<IndexInfo>,
    pub foreign_keys: Vec<ForeignKeyInfo>,
    /// Full DDL for the table (and related indexes when the engine provides them).
    pub ddl: String,
}
