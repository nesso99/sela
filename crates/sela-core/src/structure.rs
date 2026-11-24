//! Read-only table structure: columns, indexes, foreign keys, and DDL.

/// One column from [`TableStructure`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnStructure {
    pub name: String,
    pub type_name: String,
    pub nullable: bool,
    pub default: Option<String>,
    pub comment: Option<String>,
    /// True when the engine reports the column as identity, generated, or
    /// auto-increment (drivers set this from catalog metadata).
    pub generated: bool,
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

impl TableStructure {
    /// Primary-key column names in key order, when the engine reported a PK index.
    ///
    /// Returns `None` when there is no primary key (table is not grid-editable).
    pub fn primary_key_columns(&self) -> Option<Vec<String>> {
        self.indexes
            .iter()
            .find(|idx| idx.primary && !idx.columns.is_empty())
            .map(|idx| idx.columns.clone())
    }
}
