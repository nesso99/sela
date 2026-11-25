//! Injectable table-column lookups for export/import (M3-3).
//!
//! Headless and unit tests inject an in-memory source so they never touch a
//! live database. The app can wrap a schema cache or `describe_table` later.

use std::collections::HashMap;

use thiserror::Error;

use crate::types::ColumnMeta;

/// Failure looking up table columns for import mapping / export helpers.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SchemaSourceError {
    #[error("table `{schema}.{table}` not found")]
    NotFound { schema: String, table: String },
    #[error("{0}")]
    Message(String),
}

/// Column metadata for one table, without requiring a live DB session.
pub trait TableSchemaSource {
    fn columns(&self, schema: &str, table: &str) -> Result<Vec<ColumnMeta>, SchemaSourceError>;
}

/// In-memory schema map for tests and fixtures.
#[derive(Debug, Clone, Default)]
pub struct StaticTableSchema {
    tables: HashMap<(String, String), Vec<ColumnMeta>>,
}

impl StaticTableSchema {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(
        &mut self,
        schema: impl Into<String>,
        table: impl Into<String>,
        columns: Vec<ColumnMeta>,
    ) {
        self.tables.insert((schema.into(), table.into()), columns);
    }

    pub fn with_table(
        mut self,
        schema: impl Into<String>,
        table: impl Into<String>,
        columns: Vec<ColumnMeta>,
    ) -> Self {
        self.insert(schema, table, columns);
        self
    }
}

impl TableSchemaSource for StaticTableSchema {
    fn columns(&self, schema: &str, table: &str) -> Result<Vec<ColumnMeta>, SchemaSourceError> {
        self.tables
            .get(&(schema.to_string(), table.to_string()))
            .cloned()
            .ok_or_else(|| SchemaSourceError::NotFound {
                schema: schema.to_string(),
                table: table.to_string(),
            })
    }
}

impl TableSchemaSource for HashMap<(String, String), Vec<ColumnMeta>> {
    fn columns(&self, schema: &str, table: &str) -> Result<Vec<ColumnMeta>, SchemaSourceError> {
        self.get(&(schema.to_string(), table.to_string()))
            .cloned()
            .ok_or_else(|| SchemaSourceError::NotFound {
                schema: schema.to_string(),
                table: table.to_string(),
            })
    }
}
