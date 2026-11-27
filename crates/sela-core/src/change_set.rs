//! Pending grid edits for a single table (M2-1).
//!
//! A [`ChangeSet`] records cell updates, row inserts, and row deletes keyed by
//! primary-key identity. Tables without a primary key are not editable: every
//! mutating method requires a non-empty PK definition so generated SQL can use
//! a PK-only `WHERE` clause.
//!
//! Merge rules (applied as edits arrive):
//! - Editing a cell on an **inserted** row updates that insert’s values.
//! - Deleting an **inserted** row removes the insert entirely.
//! - Re-editing a cell back to its original value drops that cell update; an
//!   update with no remaining cells is removed.
//! - Deleting an existing row replaces any pending update for that PK.
//! - Cell edits on a row already marked deleted are rejected.

use std::collections::BTreeMap;

use crate::structure::TableStructure;
use crate::types::Value;

/// Schema-qualified table reference for pending edits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableRef {
    pub schema: String,
    pub table: String,
}

impl TableRef {
    pub fn new(schema: impl Into<String>, table: impl Into<String>) -> Self {
        Self {
            schema: schema.into(),
            table: table.into(),
        }
    }
}

/// One pending cell change on an existing row (original vs new value).
#[derive(Debug, Clone, PartialEq)]
pub struct CellChange {
    pub old_value: Value,
    pub new_value: Value,
}

/// Pending change for one logical row.
#[derive(Debug, Clone, PartialEq)]
pub enum RowChange {
    /// `UPDATE` of an existing row. `pk` is the original primary-key identity
    /// used in `WHERE` (unchanged even if a PK column appears in `cells`).
    Update {
        pk: Vec<Value>,
        cells: BTreeMap<String, CellChange>,
    },
    /// Provisional `INSERT`. `id` is a ChangeSet-local handle (not a DB key).
    Insert {
        id: u64,
        values: BTreeMap<String, Value>,
    },
    /// `DELETE` of an existing row identified by `pk`.
    ///
    /// `expected` holds non-primary-key column originals ANDed into the
    /// optimistic `WHERE` clause. Empty when the caller did not supply a row
    /// snapshot (PK-only delete). When [`ChangeSet::delete_row`] replaces a
    /// pending update, the update’s cell `old_value`s are copied here.
    Delete {
        pk: Vec<Value>,
        expected: BTreeMap<String, Value>,
    },
}

/// Errors from constructing or mutating a [`ChangeSet`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChangeError {
    #[error("table has no primary key and is not editable")]
    NoPrimaryKey,
    #[error("primary key expects {expected} value(s), got {got}")]
    PkArity { expected: usize, got: usize },
    #[error("unknown column `{0}`")]
    UnknownColumn(String),
    #[error("row is marked deleted; clear the delete before editing")]
    RowDeleted,
    #[error("no pending insert with id {0}")]
    InsertNotFound(u64),
    #[error("insert has no column values")]
    EmptyInsert,
    #[error("{0}")]
    Sql(String),
}

impl From<crate::sql::SqlError> for ChangeError {
    fn from(err: crate::sql::SqlError) -> Self {
        Self::Sql(err.to_string())
    }
}

/// Pending edits for one table, ready to compile into a [`crate::ChangePlan`].
#[derive(Debug, Clone, PartialEq)]
pub struct ChangeSet {
    table: TableRef,
    /// Non-empty primary-key column names in key order.
    pk_columns: Vec<String>,
    /// Table columns in display / INSERT order.
    columns: Vec<String>,
    /// Engine `type_name` per column (used for Postgres bind casts).
    column_types: BTreeMap<String, String>,
    changes: Vec<RowChange>,
    next_insert_id: u64,
}

impl ChangeSet {
    /// Create a change set. `pk_columns` must be non-empty.
    pub fn try_new(
        table: TableRef,
        pk_columns: Vec<String>,
        columns: Vec<String>,
    ) -> Result<Self, ChangeError> {
        Self::try_new_with_types(table, pk_columns, columns, BTreeMap::new())
    }

    /// Create a change set with known column type names.
    pub fn try_new_with_types(
        table: TableRef,
        pk_columns: Vec<String>,
        columns: Vec<String>,
        column_types: BTreeMap<String, String>,
    ) -> Result<Self, ChangeError> {
        if pk_columns.is_empty() {
            return Err(ChangeError::NoPrimaryKey);
        }
        for pk in &pk_columns {
            if !columns.iter().any(|c| c == pk) {
                // Allow PK-only column lists when the caller passes a subset;
                // still require the name to appear somewhere useful — if the
                // columns list is empty, synthesise from PK.
                if columns.is_empty() {
                    continue;
                }
                return Err(ChangeError::UnknownColumn(pk.clone()));
            }
        }
        let columns = if columns.is_empty() {
            pk_columns.clone()
        } else {
            columns
        };
        Ok(Self {
            table,
            pk_columns,
            columns,
            column_types,
            changes: Vec::new(),
            next_insert_id: 1,
        })
    }

    /// Build from introspection. Fails when the table has no primary key.
    pub fn from_structure(structure: &TableStructure) -> Result<Self, ChangeError> {
        let pk = structure
            .primary_key_columns()
            .ok_or(ChangeError::NoPrimaryKey)?;
        let columns = structure
            .columns
            .iter()
            .map(|c| c.name.clone())
            .collect::<Vec<_>>();
        let column_types = structure
            .columns
            .iter()
            .map(|c| (c.name.clone(), c.type_name.clone()))
            .collect();
        Self::try_new_with_types(
            TableRef::new(structure.schema.clone(), structure.table.clone()),
            pk,
            columns,
            column_types,
        )
    }

    pub fn table(&self) -> &TableRef {
        &self.table
    }

    pub fn pk_columns(&self) -> &[String] {
        &self.pk_columns
    }

    pub fn columns(&self) -> &[String] {
        &self.columns
    }

    /// Engine type name for `name`, when known.
    pub fn column_type(&self, name: &str) -> Option<&str> {
        self.column_types.get(name).map(String::as_str)
    }

    pub fn changes(&self) -> &[RowChange] {
        &self.changes
    }

    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    pub fn clear(&mut self) {
        self.changes.clear();
    }

    fn require_pk(&self, pk: &[Value]) -> Result<(), ChangeError> {
        if pk.len() != self.pk_columns.len() {
            return Err(ChangeError::PkArity {
                expected: self.pk_columns.len(),
                got: pk.len(),
            });
        }
        Ok(())
    }

    pub(crate) fn check_columns<'a>(
        &self,
        columns: impl IntoIterator<Item = &'a String>,
    ) -> Result<(), ChangeError> {
        for column in columns {
            self.require_column(column)?;
        }
        Ok(())
    }

    fn require_column(&self, column: &str) -> Result<(), ChangeError> {
        if self.columns.iter().any(|c| c == column) {
            Ok(())
        } else {
            Err(ChangeError::UnknownColumn(column.to_string()))
        }
    }

    fn find_existing_mut(&mut self, pk: &[Value]) -> Option<usize> {
        self.changes.iter().position(|c| match c {
            RowChange::Update { pk: existing, .. } | RowChange::Delete { pk: existing, .. } => {
                pk_eq(existing, pk)
            }
            RowChange::Insert { .. } => false,
        })
    }

    fn is_pk_column(&self, column: &str) -> bool {
        self.pk_columns.iter().any(|c| c == column)
    }

    /// Non-PK originals taken from a pending update (for optimistic DELETE).
    fn expected_from_update_cells(
        &self,
        cells: &BTreeMap<String, CellChange>,
    ) -> BTreeMap<String, Value> {
        cells
            .iter()
            .filter(|(col, _)| !self.is_pk_column(col))
            .map(|(col, cell)| (col.clone(), cell.old_value.clone()))
            .collect()
    }

    fn find_insert_mut(&mut self, id: u64) -> Option<usize> {
        self.changes.iter().position(|c| match c {
            RowChange::Insert { id: existing, .. } => *existing == id,
            _ => false,
        })
    }

    /// Set or replace a cell on an existing row (identity = original PK values).
    ///
    /// When `new_value` equals the original `old_value` tracked for that cell
    /// (or the supplied `old_value` on first edit), the cell update is dropped.
    pub fn set_cell(
        &mut self,
        pk: &[Value],
        column: &str,
        old_value: Value,
        new_value: Value,
    ) -> Result<(), ChangeError> {
        self.require_pk(pk)?;
        self.require_column(column)?;

        if let Some(ix) = self.find_existing_mut(pk) {
            match &mut self.changes[ix] {
                RowChange::Delete { .. } => return Err(ChangeError::RowDeleted),
                RowChange::Update { cells, .. } => {
                    let original_old = cells
                        .get(column)
                        .map(|c| c.old_value.clone())
                        .unwrap_or_else(|| old_value.clone());
                    if values_equal(&original_old, &new_value) {
                        cells.remove(column);
                    } else {
                        cells.insert(
                            column.to_string(),
                            CellChange {
                                old_value: original_old,
                                new_value,
                            },
                        );
                    }
                    if cells.is_empty() {
                        self.changes.remove(ix);
                    }
                    return Ok(());
                }
                RowChange::Insert { .. } => unreachable!("find_existing skips inserts"),
            }
        }

        if values_equal(&old_value, &new_value) {
            return Ok(());
        }
        let mut cells = BTreeMap::new();
        cells.insert(
            column.to_string(),
            CellChange {
                old_value,
                new_value,
            },
        );
        self.changes.push(RowChange::Update {
            pk: pk.to_vec(),
            cells,
        });
        Ok(())
    }

    /// Mark an existing row for deletion. Replaces any pending update for that PK.
    ///
    /// When replacing an update, copies non-PK cell `old_value`s into
    /// [`RowChange::Delete::expected`] so optimistic WHERE can still check them.
    pub fn delete_row(&mut self, pk: &[Value]) -> Result<(), ChangeError> {
        self.delete_row_with_expected(pk, BTreeMap::new())
    }

    /// Mark a row for deletion with an explicit non-PK value snapshot for
    /// optional optimistic WHERE. Keys must be known columns; PK columns in
    /// `expected` are ignored. When replacing a pending update, `expected`
    /// wins over (replaces) any values inherited from that update.
    pub fn delete_row_with_expected(
        &mut self,
        pk: &[Value],
        expected: BTreeMap<String, Value>,
    ) -> Result<(), ChangeError> {
        self.require_pk(pk)?;
        for col in expected.keys() {
            self.require_column(col)?;
        }
        let mut expected: BTreeMap<String, Value> = expected
            .into_iter()
            .filter(|(col, _)| !self.is_pk_column(col))
            .collect();

        if let Some(ix) = self.find_existing_mut(pk) {
            if expected.is_empty() {
                if let RowChange::Update { cells, .. } = &self.changes[ix] {
                    expected = self.expected_from_update_cells(cells);
                } else if let RowChange::Delete {
                    expected: prior, ..
                } = &self.changes[ix]
                {
                    expected = prior.clone();
                }
            }
            self.changes[ix] = RowChange::Delete {
                pk: pk.to_vec(),
                expected,
            };
            return Ok(());
        }
        self.changes.push(RowChange::Delete {
            pk: pk.to_vec(),
            expected,
        });
        Ok(())
    }

    /// Queue a new row insert. Returns the provisional insert id.
    pub fn insert_row(&mut self, values: BTreeMap<String, Value>) -> Result<u64, ChangeError> {
        for key in values.keys() {
            self.require_column(key)?;
        }
        let id = self.next_insert_id;
        self.next_insert_id += 1;
        self.changes.push(RowChange::Insert { id, values });
        Ok(id)
    }

    /// Duplicate an existing row as a new insert (same column values).
    pub fn duplicate_row(&mut self, values: BTreeMap<String, Value>) -> Result<u64, ChangeError> {
        self.insert_row(values)
    }

    /// Update a column on a provisional insert.
    pub fn set_insert_cell(
        &mut self,
        insert_id: u64,
        column: &str,
        value: Value,
    ) -> Result<(), ChangeError> {
        self.require_column(column)?;
        let ix = self
            .find_insert_mut(insert_id)
            .ok_or(ChangeError::InsertNotFound(insert_id))?;
        match &mut self.changes[ix] {
            RowChange::Insert { values, .. } => {
                values.insert(column.to_string(), value);
                Ok(())
            }
            _ => Err(ChangeError::InsertNotFound(insert_id)),
        }
    }

    /// Remove a provisional insert (delete-of-insert cancels the insert).
    pub fn delete_insert(&mut self, insert_id: u64) -> Result<(), ChangeError> {
        let ix = self
            .find_insert_mut(insert_id)
            .ok_or(ChangeError::InsertNotFound(insert_id))?;
        self.changes.remove(ix);
        Ok(())
    }

    /// Clear a pending delete for `pk` (undelete).
    ///
    /// Does not restore a prior [`RowChange::Update`] — callers that preserved
    /// cell edits before [`Self::delete_row`] re-apply them with [`Self::set_cell`].
    pub fn undelete_row(&mut self, pk: &[Value]) -> Result<(), ChangeError> {
        self.require_pk(pk)?;
        if let Some(ix) = self.find_existing_mut(pk) {
            if matches!(self.changes[ix], RowChange::Delete { .. }) {
                self.changes.remove(ix);
            }
        }
        Ok(())
    }
}

fn pk_eq(a: &[Value], b: &[Value]) -> bool {
    a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| values_equal(x, y))
}

/// Equality used for merge rules. Treats two `NaN` floats as equal so toggling
/// a float cell through NaN still collapses correctly.
fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Float(x), Value::Float(y)) if x.is_nan() && y.is_nan() => true,
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn books() -> ChangeSet {
        ChangeSet::try_new(
            TableRef::new("public", "books"),
            vec!["id".into()],
            vec!["id".into(), "title".into(), "pages".into()],
        )
        .unwrap()
    }

    #[test]
    fn rejects_empty_primary_key() {
        let err =
            ChangeSet::try_new(TableRef::new("public", "v"), vec![], vec!["a".into()]).unwrap_err();
        assert_eq!(err, ChangeError::NoPrimaryKey);
    }

    #[test]
    fn set_cell_merge_and_revert() {
        let mut cs = books();
        cs.set_cell(
            &[Value::Int(1)],
            "title",
            Value::text("Ada"),
            Value::text("Bob"),
        )
        .unwrap();
        assert_eq!(cs.changes().len(), 1);
        // Second edit keeps the original old value.
        cs.set_cell(
            &[Value::Int(1)],
            "title",
            Value::text("ignored"),
            Value::text("Cara"),
        )
        .unwrap();
        match &cs.changes()[0] {
            RowChange::Update { cells, .. } => {
                let cell = cells.get("title").unwrap();
                assert_eq!(cell.old_value, Value::text("Ada"));
                assert_eq!(cell.new_value, Value::text("Cara"));
            }
            other => panic!("expected Update, got {other:?}"),
        }
        // Revert to original drops the update entirely.
        cs.set_cell(
            &[Value::Int(1)],
            "title",
            Value::text("ignored"),
            Value::text("Ada"),
        )
        .unwrap();
        assert!(cs.is_empty());
    }

    #[test]
    fn delete_replaces_update_and_blocks_edit() {
        let mut cs = books();
        cs.set_cell(
            &[Value::Int(1)],
            "title",
            Value::text("a"),
            Value::text("b"),
        )
        .unwrap();
        cs.delete_row(&[Value::Int(1)]).unwrap();
        assert!(matches!(cs.changes()[0], RowChange::Delete { .. }));
        let err = cs
            .set_cell(
                &[Value::Int(1)],
                "title",
                Value::text("a"),
                Value::text("c"),
            )
            .unwrap_err();
        assert_eq!(err, ChangeError::RowDeleted);
    }

    #[test]
    fn undelete_clears_delete_mark() {
        let mut cs = books();
        cs.delete_row(&[Value::Int(2)]).unwrap();
        assert!(!cs.is_empty());
        cs.undelete_row(&[Value::Int(2)]).unwrap();
        assert!(cs.is_empty());
    }

    #[test]
    fn insert_edit_and_delete_cancels() {
        let mut cs = books();
        let mut values = BTreeMap::new();
        values.insert("id".into(), Value::Int(9));
        values.insert("title".into(), Value::text("new"));
        let id = cs.insert_row(values).unwrap();
        cs.set_insert_cell(id, "title", Value::text("renamed"))
            .unwrap();
        match &cs.changes()[0] {
            RowChange::Insert { values, .. } => {
                assert_eq!(values.get("title"), Some(&Value::text("renamed")));
            }
            other => panic!("expected Insert, got {other:?}"),
        }
        cs.delete_insert(id).unwrap();
        assert!(cs.is_empty());
    }

    #[test]
    fn pk_arity_and_unknown_column() {
        let mut cs = books();
        assert!(matches!(
            cs.set_cell(&[], "title", Value::Null, Value::text("x")),
            Err(ChangeError::PkArity {
                expected: 1,
                got: 0
            })
        ));
        assert!(matches!(
            cs.set_cell(&[Value::Int(1)], "nope", Value::Null, Value::text("x")),
            Err(ChangeError::UnknownColumn(_))
        ));
    }

    #[test]
    fn from_structure_requires_pk() {
        let structure = TableStructure {
            schema: "main".into(),
            table: "t".into(),
            columns: vec![crate::structure::ColumnStructure {
                name: "a".into(),
                type_name: "int".into(),
                nullable: true,
                default: None,
                comment: None,
                generated: false,
            }],
            indexes: vec![],
            foreign_keys: vec![],
            ddl: String::new(),
        };
        assert_eq!(
            ChangeSet::from_structure(&structure).unwrap_err(),
            ChangeError::NoPrimaryKey
        );
    }
}
