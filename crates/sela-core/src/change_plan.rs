//! Compile a [`ChangeSet`] into dialect-specific bound SQL (M2-2 / #68).
//!
//! Statement order matches the safe-edit commit protocol: **DELETE**, then
//! **UPDATE**, then **INSERT**. Values are always bind parameters (`$n` on
//! Postgres, `?` on MySQL/SQLite). `UPDATE`/`DELETE` `WHERE` clauses always include
//! the primary-key identity **and** previous non-PK column values (optimistic
//! concurrency): a concurrent edit yields zero rows affected
//! ([`crate::DbError::ChangeConflict`]). NULL old values use `IS NULL` (no bind).
//! [`ChangePlan::preview_sql`] renders a human-readable form with SQL literals
//! for the preview dialog (execution still uses bound parameters only).

use std::collections::BTreeMap;

use crate::change_set::{CellChange, ChangeError, ChangeSet, RowChange, TableRef};
use crate::column_kind::ColumnKind;
use crate::copy_format::{escape_sql_string, value_to_copy_cell, CopyCell, CopyScalarKind};
use crate::sql::{bind_placeholder, qualify, quote_ident};
use crate::types::{Dialect, Value};

/// Kind of DML statement in a [`ChangePlan`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanOp {
    Delete,
    Update,
    Insert,
}

/// Max byte length for blob/bytea optimistic WHERE equality (larger blobs are skipped).
pub const OPTIMISTIC_BLOB_MAX_BYTES: usize = 4096;

/// One bound DML statement ready for transactional execution.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedStatement {
    pub op: PlanOp,
    pub sql: String,
    pub params: Vec<Value>,
    /// Original primary-key identity for UPDATE/DELETE (empty for INSERT).
    /// Used by `apply_changes` when reporting [`crate::DbError::ChangeConflict`].
    pub pk: Vec<Value>,
    /// Non-PK columns whose old values were not ANDed into WHERE (non-comparable
    /// kinds: `Other`, PG `json`, MySQL JSON, XML, geometric, oversized blobs).
    /// Preview lists these so the user knows what was not checked.
    pub unchecked_optimistic: Vec<String>,
}

/// Ordered, dialect-specific SQL plan built from a [`ChangeSet`].
#[derive(Debug, Clone, PartialEq)]
pub struct ChangePlan {
    pub dialect: Dialect,
    pub table: TableRef,
    pub statements: Vec<PlannedStatement>,
}

/// Result of applying a [`ChangePlan`] in one transaction (drivers).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplyChangesResult {
    pub statements_executed: usize,
    pub rows_affected: u64,
}

impl ChangePlan {
    /// Build a plan for `dialect`. Empty change sets yield an empty plan.
    ///
    /// Optimistic WHERE is always on: UPDATE uses each edited cell’s
    /// `old_value`, DELETE uses [`RowChange::Delete::expected`].
    pub fn build(changeset: &ChangeSet, dialect: Dialect) -> Result<Self, ChangeError> {
        let mut deletes = Vec::new();
        let mut updates = Vec::new();
        let mut inserts = Vec::new();

        for change in changeset.changes() {
            match change {
                RowChange::Delete { pk, expected } => {
                    deletes.push(build_delete(changeset, dialect, pk, expected)?);
                }
                RowChange::Update { pk, cells } => {
                    if cells.is_empty() {
                        continue;
                    }
                    updates.push(build_update(changeset, dialect, pk, cells)?);
                }
                RowChange::Insert { values, .. } => {
                    inserts.push(build_insert(changeset, dialect, values)?);
                }
            }
        }

        let mut statements = Vec::with_capacity(deletes.len() + updates.len() + inserts.len());
        statements.extend(deletes);
        statements.extend(updates);
        statements.extend(inserts);

        Ok(Self {
            dialect,
            table: changeset.table().clone(),
            statements,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.statements.is_empty()
    }

    /// Build the single `INSERT` for `values` against `changeset`'s table
    /// without queueing it in the change set (streaming import sink).
    /// Same SQL as [`Self::build`] produces for a queued insert.
    pub fn insert_statement(
        changeset: &ChangeSet,
        dialect: Dialect,
        values: &BTreeMap<String, Value>,
    ) -> Result<PlannedStatement, ChangeError> {
        changeset.check_columns(values.keys())?;
        build_insert(changeset, dialect, values)
    }

    /// Human-readable preview with SQL literals (display only — never execute).
    ///
    /// Includes `BEGIN;` / `COMMIT;` so M2-3 can show the full transaction.
    /// Columns skipped by optimistic WHERE are listed as SQL comments.
    pub fn preview_sql(&self) -> Result<String, ChangeError> {
        let mut out = String::from("BEGIN;\n");
        for stmt in &self.statements {
            if !stmt.unchecked_optimistic.is_empty() {
                out.push_str("-- unchecked optimistic: ");
                out.push_str(&stmt.unchecked_optimistic.join(", "));
                out.push('\n');
            }
            out.push_str(&render_preview_statement(self.dialect, stmt)?);
            out.push('\n');
        }
        out.push_str("COMMIT;");
        Ok(out)
    }

    /// Preview keeping bind placeholders (and a trailing param-count comment).
    pub fn preview_bound(&self) -> String {
        let mut out = String::from("BEGIN;\n");
        for stmt in &self.statements {
            out.push_str(&stmt.sql);
            out.push(';');
            if !stmt.params.is_empty() {
                out.push_str(&format!(
                    "  -- {} bound param{}",
                    stmt.params.len(),
                    if stmt.params.len() == 1 { "" } else { "s" }
                ));
            }
            out.push('\n');
        }
        out.push_str("COMMIT;");
        out
    }
}

fn build_delete(
    changeset: &ChangeSet,
    dialect: Dialect,
    pk: &[Value],
    expected: &BTreeMap<String, Value>,
) -> Result<PlannedStatement, ChangeError> {
    let mut sql = format!(
        "DELETE FROM {}",
        qualify(dialect, &changeset.table().schema, &changeset.table().table)?
    );
    let mut params = Vec::new();
    append_pk_where(changeset, dialect, &mut sql, &mut params, pk)?;
    let unchecked = append_optimistic_preds(
        changeset,
        dialect,
        &mut sql,
        &mut params,
        &ordered_expected(changeset, expected),
    )?;
    Ok(PlannedStatement {
        op: PlanOp::Delete,
        sql,
        params,
        pk: pk.to_vec(),
        unchecked_optimistic: unchecked,
    })
}

fn build_update(
    changeset: &ChangeSet,
    dialect: Dialect,
    pk: &[Value],
    cells: &BTreeMap<String, CellChange>,
) -> Result<PlannedStatement, ChangeError> {
    let mut sql = format!(
        "UPDATE {} SET ",
        qualify(dialect, &changeset.table().schema, &changeset.table().table)?
    );
    let mut params = Vec::new();
    // Stable SET order: table column order, then any extras alphabetically (BTree).
    let mut first = true;
    for col in changeset.columns() {
        if let Some(cell) = cells.get(col) {
            if !first {
                sql.push_str(", ");
            }
            first = false;
            sql.push_str(&quote_ident(dialect, col)?);
            sql.push_str(" = ");
            sql.push_str(&bind_expr(changeset, dialect, col, params.len()));
            params.push(cell.new_value.clone());
        }
    }
    for (col, cell) in cells {
        if changeset.columns().iter().any(|c| c == col) {
            continue;
        }
        if !first {
            sql.push_str(", ");
        }
        first = false;
        sql.push_str(&quote_ident(dialect, col)?);
        sql.push_str(" = ");
        sql.push_str(&bind_expr(changeset, dialect, col, params.len()));
        params.push(cell.new_value.clone());
    }
    append_pk_where(changeset, dialect, &mut sql, &mut params, pk)?;
    let expected = ordered_cell_old_values(changeset, cells);
    let unchecked = append_optimistic_preds(changeset, dialect, &mut sql, &mut params, &expected)?;
    Ok(PlannedStatement {
        op: PlanOp::Update,
        sql,
        params,
        pk: pk.to_vec(),
        unchecked_optimistic: unchecked,
    })
}

/// Non-PK `(column, old_value)` pairs in table column order, then extras.
fn ordered_cell_old_values(
    changeset: &ChangeSet,
    cells: &BTreeMap<String, CellChange>,
) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    for col in changeset.columns() {
        if changeset.pk_columns().iter().any(|c| c == col) {
            continue;
        }
        if let Some(cell) = cells.get(col) {
            out.push((col.clone(), cell.old_value.clone()));
        }
    }
    for (col, cell) in cells {
        if changeset.columns().iter().any(|c| c == col) {
            continue;
        }
        if changeset.pk_columns().iter().any(|c| c == col) {
            continue;
        }
        out.push((col.clone(), cell.old_value.clone()));
    }
    out
}

fn ordered_expected(
    changeset: &ChangeSet,
    expected: &BTreeMap<String, Value>,
) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    for col in changeset.columns() {
        if changeset.pk_columns().iter().any(|c| c == col) {
            continue;
        }
        if let Some(value) = expected.get(col) {
            out.push((col.clone(), value.clone()));
        }
    }
    for (col, value) in expected {
        if changeset.columns().iter().any(|c| c == col) {
            continue;
        }
        if changeset.pk_columns().iter().any(|c| c == col) {
            continue;
        }
        out.push((col.clone(), value.clone()));
    }
    out
}

fn append_optimistic_preds(
    changeset: &ChangeSet,
    dialect: Dialect,
    sql: &mut String,
    params: &mut Vec<Value>,
    expected: &[(String, Value)],
) -> Result<Vec<String>, ChangeError> {
    let mut unchecked = Vec::new();
    for (col, value) in expected {
        let type_name = changeset.column_type(col);
        if !optimistic_value_comparable(dialect, type_name, value) {
            unchecked.push(col.clone());
            continue;
        }
        sql.push_str(" AND ");
        sql.push_str(&quote_ident(dialect, col)?);
        if value.is_null() {
            sql.push_str(" IS NULL");
        } else {
            sql.push_str(" = ");
            sql.push_str(&bind_expr(changeset, dialect, col, params.len()));
            params.push(value.clone());
        }
    }
    Ok(unchecked)
}

/// Whether an old non-PK value can safely appear in optimistic `WHERE col = $n`.
///
/// Skips [`Value::Other`], Postgres `json` (keeps `jsonb`), MySQL JSON, XML,
/// geometric types, and blobs larger than [`OPTIMISTIC_BLOB_MAX_BYTES`].
pub fn optimistic_value_comparable(
    dialect: Dialect,
    type_name: Option<&str>,
    value: &Value,
) -> bool {
    let lower = type_name.map(|t| t.trim().to_ascii_lowercase());
    let lower = lower.as_deref().unwrap_or("");

    if type_is_xml(lower) || type_is_geometric(lower) {
        return false;
    }
    match dialect {
        Dialect::Postgres if type_is_pg_json_not_jsonb(lower) => return false,
        Dialect::Mysql if lower.contains("json") => return false,
        _ => {}
    }

    match value {
        Value::Other { .. } => false,
        Value::Bytes(bytes) if bytes.len() > OPTIMISTIC_BLOB_MAX_BYTES => false,
        Value::Json(_) => match dialect {
            // jsonb equality is fine; plain json is rejected above via type_name.
            // When the type name is missing, keep Json only for SQLite (text JSON).
            Dialect::Postgres => lower.contains("jsonb"),
            Dialect::Mysql => false,
            Dialect::Sqlite => true,
        },
        _ => true,
    }
}

fn type_is_xml(lower: &str) -> bool {
    lower == "xml" || lower.ends_with(" xml") || lower.contains("xmltype")
}

fn type_is_geometric(lower: &str) -> bool {
    matches!(
        lower,
        "point"
            | "line"
            | "lseg"
            | "box"
            | "path"
            | "polygon"
            | "circle"
            | "geometry"
            | "geography"
    ) || lower.starts_with("geometry(")
        || lower.starts_with("geography(")
}

fn type_is_pg_json_not_jsonb(lower: &str) -> bool {
    (lower == "json" || lower.starts_with("json(") || lower.contains(" json"))
        && !lower.contains("jsonb")
}

fn build_insert(
    changeset: &ChangeSet,
    dialect: Dialect,
    values: &std::collections::BTreeMap<String, Value>,
) -> Result<PlannedStatement, ChangeError> {
    let mut cols = Vec::new();
    let mut params = Vec::new();
    for col in changeset.columns() {
        if let Some(value) = values.get(col) {
            cols.push(col.clone());
            params.push(value.clone());
        }
    }
    for (col, value) in values {
        if changeset.columns().iter().any(|c| c == col) {
            continue;
        }
        cols.push(col.clone());
        params.push(value.clone());
    }
    let table = qualify(dialect, &changeset.table().schema, &changeset.table().table)?;
    if cols.is_empty() {
        // Nothing set: let the engine fill defaults / generated keys.
        // MySQL/MariaDB reject DEFAULT VALUES and need `() VALUES ()`.
        let sql = match dialect {
            Dialect::Mysql => format!("INSERT INTO {table} () VALUES ()"),
            Dialect::Postgres | Dialect::Sqlite => {
                format!("INSERT INTO {table} DEFAULT VALUES")
            }
        };
        return Ok(PlannedStatement {
            op: PlanOp::Insert,
            sql,
            params,
            pk: Vec::new(),
            unchecked_optimistic: Vec::new(),
        });
    }

    let mut sql = format!("INSERT INTO {table} (");
    for (i, col) in cols.iter().enumerate() {
        if i > 0 {
            sql.push_str(", ");
        }
        sql.push_str(&quote_ident(dialect, col)?);
    }
    sql.push_str(") VALUES (");
    for (i, col) in cols.iter().enumerate() {
        if i > 0 {
            sql.push_str(", ");
        }
        // Placeholders are 0-based into this statement’s own params vec.
        sql.push_str(&bind_expr(changeset, dialect, col, i));
    }
    sql.push(')');
    Ok(PlannedStatement {
        op: PlanOp::Insert,
        sql,
        params,
        pk: Vec::new(),
        unchecked_optimistic: Vec::new(),
    })
}

fn append_pk_where(
    changeset: &ChangeSet,
    dialect: Dialect,
    sql: &mut String,
    params: &mut Vec<Value>,
    pk: &[Value],
) -> Result<(), ChangeError> {
    if pk.len() != changeset.pk_columns().len() {
        return Err(ChangeError::PkArity {
            expected: changeset.pk_columns().len(),
            got: pk.len(),
        });
    }
    sql.push_str(" WHERE ");
    for (i, (col, value)) in changeset.pk_columns().iter().zip(pk.iter()).enumerate() {
        if i > 0 {
            sql.push_str(" AND ");
        }
        sql.push_str(&quote_ident(dialect, col)?);
        // NULL PK components use `IS NULL` (no bind). Non-null use `= $n` / `= ?`.
        if value.is_null() {
            sql.push_str(" IS NULL");
        } else {
            sql.push_str(" = ");
            sql.push_str(&bind_expr(changeset, dialect, col, params.len()));
            params.push(value.clone());
        }
    }
    Ok(())
}

/// Bind placeholder, with a Postgres cast when the column type needs one.
fn bind_expr(changeset: &ChangeSet, dialect: Dialect, col: &str, param_ix: usize) -> String {
    let ph = bind_placeholder(dialect, param_ix);
    if dialect != Dialect::Postgres {
        return ph;
    }
    let Some(type_name) = changeset.column_type(col) else {
        return ph;
    };
    match postgres_bind_cast(type_name) {
        Some(cast) => format!("{ph}::{cast}"),
        None => ph,
    }
}

fn postgres_bind_cast(type_name: &str) -> Option<&'static str> {
    let lower = type_name.to_ascii_lowercase();
    match ColumnKind::from_type_name(type_name) {
        ColumnKind::Uuid => Some("uuid"),
        ColumnKind::Date => Some("date"),
        ColumnKind::Time => Some("time"),
        ColumnKind::Timestamp => {
            if lower.contains("with time zone") || lower.contains("timestamptz") {
                Some("timestamptz")
            } else {
                Some("timestamp")
            }
        }
        ColumnKind::Decimal => Some("numeric"),
        ColumnKind::Json => {
            if lower.contains("jsonb") {
                Some("jsonb")
            } else {
                Some("json")
            }
        }
        ColumnKind::Interval => Some("interval"),
        _ => None,
    }
}

fn render_preview_statement(
    dialect: Dialect,
    stmt: &PlannedStatement,
) -> Result<String, ChangeError> {
    let mut out = String::new();
    let mut param_ix = 0usize;
    let bytes = stmt.sql.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        match dialect {
            Dialect::Postgres if bytes[i] == b'$' => {
                let start = i + 1;
                let mut end = start;
                while end < bytes.len() && bytes[end].is_ascii_digit() {
                    end += 1;
                }
                if start < end {
                    let n: usize = stmt.sql[start..end].parse().unwrap_or(0);
                    let value = stmt
                        .params
                        .get(n.saturating_sub(1))
                        .ok_or_else(|| ChangeError::Sql("preview param out of range".into()))?;
                    out.push_str(&value_to_sql_literal(value, dialect));
                    i = end;
                    continue;
                }
                out.push('$');
                i += 1;
            }
            Dialect::Mysql | Dialect::Sqlite if bytes[i] == b'?' => {
                let value = stmt
                    .params
                    .get(param_ix)
                    .ok_or_else(|| ChangeError::Sql("preview param out of range".into()))?;
                param_ix += 1;
                out.push_str(&value_to_sql_literal(value, dialect));
                i += 1;
            }
            _ => {
                out.push(bytes[i] as char);
                i += 1;
            }
        }
    }
    out.push(';');
    Ok(out)
}

fn value_to_sql_literal(value: &Value, dialect: Dialect) -> String {
    match value_to_copy_cell(value) {
        CopyCell::Null => "NULL".into(),
        CopyCell::Value(text, CopyScalarKind::Number) => text,
        CopyCell::Value(text, CopyScalarKind::Bool) => text,
        CopyCell::Value(text, CopyScalarKind::Text) => escape_sql_string(&text, dialect),
        CopyCell::Decode(text) => escape_sql_string(&text, dialect),
        CopyCell::NotLoaded | CopyCell::Unset => "NULL".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::change_set::{ChangeSet, TableRef};
    use std::collections::BTreeMap;

    fn odd_table() -> ChangeSet {
        ChangeSet::try_new(
            TableRef::new("s\"chema", "t`ab"),
            vec!["a\"b".into(), "c`d".into()],
            vec!["a\"b".into(), "c`d".into(), "name".into(), "note".into()],
        )
        .unwrap()
    }

    #[test]
    fn empty_insert_uses_default_values() {
        let mut cs = ChangeSet::try_new(
            TableRef::new("public", "books"),
            vec!["id".into()],
            vec!["id".into(), "title".into()],
        )
        .unwrap();
        cs.insert_row(BTreeMap::new()).unwrap();
        let pg = ChangePlan::build(&cs, Dialect::Postgres).unwrap();
        assert_eq!(
            pg.statements[0].sql,
            "INSERT INTO \"public\".\"books\" DEFAULT VALUES"
        );
        assert!(pg.statements[0].params.is_empty());
        let my = ChangePlan::build(&cs, Dialect::Mysql).unwrap();
        assert!(
            my.statements[0].sql.ends_with("() VALUES ()"),
            "{}",
            my.statements[0].sql
        );
    }

    #[test]
    fn ordered_delete_update_insert_postgres() {
        let mut cs = ChangeSet::try_new(
            TableRef::new("public", "books"),
            vec!["id".into()],
            vec!["id".into(), "title".into()],
        )
        .unwrap();
        cs.set_cell(
            &[Value::Int(1)],
            "title",
            Value::text("old"),
            Value::text("new"),
        )
        .unwrap();
        cs.delete_row(&[Value::Int(2)]).unwrap();
        let mut values = BTreeMap::new();
        values.insert("id".into(), Value::Int(3));
        values.insert("title".into(), Value::text("ins"));
        cs.insert_row(values).unwrap();

        let plan = ChangePlan::build(&cs, Dialect::Postgres).unwrap();
        assert_eq!(plan.statements.len(), 3);
        assert_eq!(plan.statements[0].op, PlanOp::Delete);
        assert_eq!(plan.statements[1].op, PlanOp::Update);
        assert_eq!(plan.statements[2].op, PlanOp::Insert);
        assert_eq!(
            plan.statements[0].sql,
            "DELETE FROM \"public\".\"books\" WHERE \"id\" = $1"
        );
        assert_eq!(
            plan.statements[1].sql,
            "UPDATE \"public\".\"books\" SET \"title\" = $1 WHERE \"id\" = $2 AND \"title\" = $3"
        );
        assert_eq!(
            plan.statements[2].sql,
            "INSERT INTO \"public\".\"books\" (\"id\", \"title\") VALUES ($1, $2)"
        );
    }

    #[test]
    fn mysql_and_sqlite_use_question_placeholders() {
        let mut cs = ChangeSet::try_new(
            TableRef::new("sample", "books"),
            vec!["id".into()],
            vec!["id".into(), "title".into()],
        )
        .unwrap();
        cs.set_cell(&[Value::Int(1)], "title", Value::Null, Value::text("x"))
            .unwrap();
        let my = ChangePlan::build(&cs, Dialect::Mysql).unwrap();
        assert_eq!(
            my.statements[0].sql,
            "UPDATE `sample`.`books` SET `title` = ? WHERE `id` = ? AND `title` IS NULL"
        );
        let sl = ChangePlan::build(&cs, Dialect::Sqlite).unwrap();
        assert_eq!(
            sl.statements[0].sql,
            "UPDATE \"sample\".\"books\" SET \"title\" = ? WHERE \"id\" = ? AND \"title\" IS NULL"
        );
    }

    #[test]
    fn composite_pk_and_odd_identifiers() {
        let mut cs = odd_table();
        cs.set_cell(
            &[Value::Int(1), Value::text("k")],
            "name",
            Value::text("a"),
            Value::Null,
        )
        .unwrap();
        cs.delete_row(&[Value::Int(2), Value::Null]).unwrap();

        let pg = ChangePlan::build(&cs, Dialect::Postgres).unwrap();
        assert_eq!(
            pg.statements[0].sql,
            "DELETE FROM \"s\"\"chema\".\"t`ab\" WHERE \"a\"\"b\" = $1 AND \"c`d\" IS NULL"
        );
        assert_eq!(
            pg.statements[1].sql,
            "UPDATE \"s\"\"chema\".\"t`ab\" SET \"name\" = $1 WHERE \"a\"\"b\" = $2 AND \"c`d\" = $3 AND \"name\" = $4"
        );
        assert_eq!(pg.statements[1].params[0], Value::Null);
        assert_eq!(pg.statements[1].params[3], Value::text("a"));

        let my = ChangePlan::build(&cs, Dialect::Mysql).unwrap();
        assert_eq!(
            my.statements[0].sql,
            "DELETE FROM `s\"chema`.`t``ab` WHERE `a\"b` = ? AND `c``d` IS NULL"
        );
    }

    #[test]
    fn preview_sql_interpolates_literals_for_display() {
        let mut cs = ChangeSet::try_new(
            TableRef::new("public", "books"),
            vec!["id".into()],
            vec!["id".into(), "title".into()],
        )
        .unwrap();
        cs.set_cell(
            &[Value::Int(1)],
            "title",
            Value::text("o"),
            Value::text("O'Reilly"),
        )
        .unwrap();
        let plan = ChangePlan::build(&cs, Dialect::Postgres).unwrap();
        let preview = plan.preview_sql().unwrap();
        assert!(preview.starts_with("BEGIN;\n"));
        assert!(preview.contains(
            "UPDATE \"public\".\"books\" SET \"title\" = 'O''Reilly' WHERE \"id\" = 1 AND \"title\" = 'o';"
        ));
        assert!(preview.ends_with("COMMIT;"));
    }

    #[test]
    fn postgres_typed_binds_emit_casts() {
        use chrono::{FixedOffset, NaiveDate, TimeZone};
        use rust_decimal::Decimal;
        use std::str::FromStr;
        use uuid::Uuid;

        let mut types = BTreeMap::new();
        types.insert("id".into(), "uuid".into());
        types.insert("d".into(), "date".into());
        types.insert("amt".into(), "numeric".into());
        types.insert("doc".into(), "jsonb".into());
        types.insert("ts".into(), "timestamp with time zone".into());
        let mut cs = ChangeSet::try_new_with_types(
            TableRef::new("public", "typed"),
            vec!["id".into()],
            vec![
                "id".into(),
                "d".into(),
                "amt".into(),
                "doc".into(),
                "ts".into(),
            ],
            types,
        )
        .unwrap();

        let id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let id_del = Uuid::parse_str("6ba7b810-9dad-11d1-80b4-00c04fd430c8").unwrap();
        let tz = FixedOffset::east_opt(0)
            .unwrap()
            .with_ymd_and_hms(2024, 1, 2, 3, 4, 5)
            .unwrap();
        let tz2 = FixedOffset::east_opt(0)
            .unwrap()
            .with_ymd_and_hms(2024, 6, 15, 12, 0, 0)
            .unwrap();
        cs.set_cell(
            &[Value::Uuid(id)],
            "d",
            Value::Date(NaiveDate::from_ymd_opt(2024, 1, 1).unwrap()),
            Value::Date(NaiveDate::from_ymd_opt(2024, 6, 15).unwrap()),
        )
        .unwrap();
        cs.set_cell(
            &[Value::Uuid(id)],
            "amt",
            Value::Decimal(Decimal::from_str("1.00").unwrap()),
            Value::Decimal(Decimal::from_str("2.50").unwrap()),
        )
        .unwrap();
        cs.set_cell(
            &[Value::Uuid(id)],
            "doc",
            Value::json_raw("{}"),
            Value::json_raw(r#"{"a":1}"#),
        )
        .unwrap();
        cs.set_cell(
            &[Value::Uuid(id)],
            "ts",
            Value::TimestampTz(tz),
            Value::TimestampTz(tz2),
        )
        .unwrap();
        cs.delete_row(&[Value::Uuid(id_del)]).unwrap();

        let mut insert = BTreeMap::new();
        insert.insert("id".into(), Value::Uuid(Uuid::nil()));
        insert.insert(
            "d".into(),
            Value::Date(NaiveDate::from_ymd_opt(2025, 1, 1).unwrap()),
        );
        insert.insert(
            "amt".into(),
            Value::Decimal(Decimal::from_str("0").unwrap()),
        );
        insert.insert("doc".into(), Value::json_raw("[]"));
        insert.insert("ts".into(), Value::TimestampTz(tz2));
        cs.insert_row(insert).unwrap();

        let plan = ChangePlan::build(&cs, Dialect::Postgres).unwrap();
        let delete_sql = &plan.statements[0].sql;
        let update_sql = &plan.statements[1].sql;
        let insert_sql = &plan.statements[2].sql;

        assert!(
            delete_sql.contains("$1::uuid"),
            "delete WHERE: {delete_sql}"
        );
        assert!(update_sql.contains("$1::date"), "update SET: {update_sql}");
        assert!(
            update_sql.contains("$2::numeric"),
            "update SET: {update_sql}"
        );
        assert!(update_sql.contains("$3::jsonb"), "update SET: {update_sql}");
        assert!(
            update_sql.contains("$4::timestamptz"),
            "update SET: {update_sql}"
        );
        assert!(
            update_sql.contains("$5::uuid"),
            "update WHERE pk: {update_sql}"
        );
        // Optimistic old-value binds reuse the same casts ($6..$9).
        assert!(
            update_sql.contains("$6::date")
                && update_sql.contains("$7::numeric")
                && update_sql.contains("$8::jsonb")
                && update_sql.contains("$9::timestamptz"),
            "update optimistic WHERE: {update_sql}"
        );
        assert!(
            insert_sql.contains("$1::uuid"),
            "insert VALUES: {insert_sql}"
        );
        assert!(
            insert_sql.contains("$2::date"),
            "insert VALUES: {insert_sql}"
        );
        assert!(
            insert_sql.contains("$3::numeric"),
            "insert VALUES: {insert_sql}"
        );
        assert!(
            insert_sql.contains("$4::jsonb"),
            "insert VALUES: {insert_sql}"
        );
        assert!(
            insert_sql.contains("$5::timestamptz"),
            "insert VALUES: {insert_sql}"
        );
    }

    /// OW-01: UPDATE/DELETE AND previous non-PK values; NULL → IS NULL.
    #[test]
    fn ow01_optimistic_where_ands_old_non_pk_values_and_nulls() {
        let mut cs = ChangeSet::try_new(
            TableRef::new("public", "books"),
            vec!["id".into()],
            vec!["id".into(), "title".into(), "note".into()],
        )
        .unwrap();
        cs.set_cell(
            &[Value::Int(1)],
            "title",
            Value::text("old"),
            Value::text("new"),
        )
        .unwrap();
        cs.set_cell(&[Value::Int(1)], "note", Value::Null, Value::text("n"))
            .unwrap();
        let mut expected = BTreeMap::new();
        expected.insert("title".into(), Value::text("gone"));
        expected.insert("note".into(), Value::Null);
        cs.delete_row_with_expected(&[Value::Int(2)], expected)
            .unwrap();

        let pg = ChangePlan::build(&cs, Dialect::Postgres).unwrap();
        assert_eq!(
            pg.statements[0].sql,
            "DELETE FROM \"public\".\"books\" WHERE \"id\" = $1 AND \"title\" = $2 AND \"note\" IS NULL"
        );
        assert_eq!(
            pg.statements[0].params,
            vec![Value::Int(2), Value::text("gone")]
        );
        assert_eq!(
            pg.statements[1].sql,
            "UPDATE \"public\".\"books\" SET \"title\" = $1, \"note\" = $2 WHERE \"id\" = $3 AND \"title\" = $4 AND \"note\" IS NULL"
        );
        assert_eq!(
            pg.statements[1].params,
            vec![
                Value::text("new"),
                Value::text("n"),
                Value::Int(1),
                Value::text("old"),
            ]
        );

        let my = ChangePlan::build(&cs, Dialect::Mysql).unwrap();
        assert_eq!(
            my.statements[1].sql,
            "UPDATE `public`.`books` SET `title` = ?, `note` = ? WHERE `id` = ? AND `title` = ? AND `note` IS NULL"
        );
        let sl = ChangePlan::build(&cs, Dialect::Sqlite).unwrap();
        assert_eq!(
            sl.statements[1].sql,
            "UPDATE \"public\".\"books\" SET \"title\" = ?, \"note\" = ? WHERE \"id\" = ? AND \"title\" = ? AND \"note\" IS NULL"
        );
    }

    /// Skip non-comparable optimistic preds; preview lists unchecked columns.
    #[test]
    fn optimistic_skips_non_comparable_kinds_and_lists_unchecked() {
        let mut types = BTreeMap::new();
        types.insert("id".into(), "integer".into());
        types.insert("title".into(), "text".into());
        types.insert("score".into(), "real".into());
        types.insert("meta".into(), "json".into());
        types.insert("doc".into(), "jsonb".into());
        types.insert("blob".into(), "bytea".into());
        types.insert("shape".into(), "point".into());
        let mut cs = ChangeSet::try_new_with_types(
            TableRef::new("public", "items"),
            vec!["id".into()],
            vec![
                "id".into(),
                "title".into(),
                "score".into(),
                "meta".into(),
                "doc".into(),
                "blob".into(),
                "shape".into(),
            ],
            types,
        )
        .unwrap();
        let big = vec![0u8; OPTIMISTIC_BLOB_MAX_BYTES + 1];
        cs.set_cell(
            &[Value::Int(1)],
            "title",
            Value::text("a"),
            Value::text("b"),
        )
        .unwrap();
        cs.set_cell(
            &[Value::Int(1)],
            "score",
            Value::Float(1.5),
            Value::Float(2.5),
        )
        .unwrap();
        cs.set_cell(
            &[Value::Int(1)],
            "meta",
            Value::json_raw(r#"{"k":1}"#),
            Value::json_raw(r#"{"k":2}"#),
        )
        .unwrap();
        cs.set_cell(
            &[Value::Int(1)],
            "doc",
            Value::json_raw(r#"{"j":1}"#),
            Value::json_raw(r#"{"j":2}"#),
        )
        .unwrap();
        cs.set_cell(
            &[Value::Int(1)],
            "blob",
            Value::Bytes(big.clone()),
            Value::Bytes(vec![1, 2, 3]),
        )
        .unwrap();
        cs.set_cell(
            &[Value::Int(1)],
            "shape",
            Value::Other {
                type_name: "point".into(),
                text: "(0,0)".into(),
            },
            Value::Other {
                type_name: "point".into(),
                text: "(1,1)".into(),
            },
        )
        .unwrap();

        let plan = ChangePlan::build(&cs, Dialect::Postgres).unwrap();
        let update = &plan.statements[0];
        let where_sql = update.sql.split(" WHERE ").nth(1).unwrap_or("");
        assert!(
            where_sql.contains("AND \"title\" =") && where_sql.contains("AND \"score\" ="),
            "{}",
            update.sql
        );
        assert!(
            where_sql.contains("AND \"doc\" =") && where_sql.contains("::jsonb"),
            "jsonb kept: {}",
            update.sql
        );
        assert!(
            !where_sql.contains("\"meta\""),
            "plain json skipped in WHERE: {}",
            update.sql
        );
        assert!(
            !where_sql.contains("\"blob\"") && !where_sql.contains("\"shape\""),
            "blob/geo skipped in WHERE: {}",
            update.sql
        );
        assert_eq!(
            update.unchecked_optimistic,
            vec!["meta".to_string(), "blob".to_string(), "shape".to_string()]
        );
        let preview = plan.preview_sql().unwrap();
        assert!(
            preview.contains("-- unchecked optimistic: meta, blob, shape"),
            "{preview}"
        );

        // MySQL: JSON skipped entirely; float kept.
        let mut types = BTreeMap::new();
        types.insert("id".into(), "int".into());
        types.insert("score".into(), "double".into());
        types.insert("meta".into(), "json".into());
        let mut cs = ChangeSet::try_new_with_types(
            TableRef::new("sample", "items"),
            vec!["id".into()],
            vec!["id".into(), "score".into(), "meta".into()],
            types,
        )
        .unwrap();
        cs.set_cell(
            &[Value::Int(1)],
            "score",
            Value::Float(1.0),
            Value::Float(2.0),
        )
        .unwrap();
        cs.set_cell(
            &[Value::Int(1)],
            "meta",
            Value::json_raw("{}"),
            Value::json_raw("[]"),
        )
        .unwrap();
        let my = ChangePlan::build(&cs, Dialect::Mysql).unwrap();
        let my_where = my.statements[0].sql.split(" WHERE ").nth(1).unwrap_or("");
        assert!(
            my_where.contains("AND `score` =") && !my_where.contains("`meta`"),
            "{}",
            my.statements[0].sql
        );
        assert_eq!(
            my.statements[0].unchecked_optimistic,
            vec!["meta".to_string()]
        );
    }

    #[test]
    fn delete_row_inherits_update_old_values_for_optimistic() {
        let mut cs = ChangeSet::try_new(
            TableRef::new("public", "books"),
            vec!["id".into()],
            vec!["id".into(), "title".into()],
        )
        .unwrap();
        cs.set_cell(
            &[Value::Int(1)],
            "title",
            Value::text("was"),
            Value::text("edited"),
        )
        .unwrap();
        cs.delete_row(&[Value::Int(1)]).unwrap();
        let plan = ChangePlan::build(&cs, Dialect::Postgres).unwrap();
        assert_eq!(
            plan.statements[0].sql,
            "DELETE FROM \"public\".\"books\" WHERE \"id\" = $1 AND \"title\" = $2"
        );
        assert_eq!(
            plan.statements[0].params,
            vec![Value::Int(1), Value::text("was")]
        );
    }
}
