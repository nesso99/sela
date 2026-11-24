//! Compile a [`ChangeSet`] into dialect-specific bound SQL (M2-2).
//!
//! Statement order matches the safe-edit commit protocol: **DELETE**, then
//! **UPDATE**, then **INSERT**. Values are always bind parameters (`$n` on
//! Postgres, `?` on MySQL/SQLite). `UPDATE`/`DELETE` `WHERE` clauses use only
//! primary-key columns. [`ChangePlan::preview_sql`] renders a human-readable
//! form with SQL literals for the M2-3 preview dialog (execution still uses
//! bound parameters only).

use crate::change_set::{ChangeError, ChangeSet, RowChange, TableRef};
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

/// One bound DML statement ready for transactional execution.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedStatement {
    pub op: PlanOp,
    pub sql: String,
    pub params: Vec<Value>,
    /// Original primary-key identity for UPDATE/DELETE (empty for INSERT).
    /// Used by `apply_changes` when reporting [`crate::DbError::ChangeConflict`].
    pub pk: Vec<Value>,
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
    pub fn build(changeset: &ChangeSet, dialect: Dialect) -> Result<Self, ChangeError> {
        let mut deletes = Vec::new();
        let mut updates = Vec::new();
        let mut inserts = Vec::new();

        for change in changeset.changes() {
            match change {
                RowChange::Delete { pk } => {
                    deletes.push(build_delete(changeset, dialect, pk)?);
                }
                RowChange::Update { pk, cells } => {
                    if cells.is_empty() {
                        continue;
                    }
                    updates.push(build_update(changeset, dialect, pk, cells)?);
                }
                RowChange::Insert { values, .. } => {
                    if values.is_empty() {
                        return Err(ChangeError::EmptyInsert);
                    }
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

    /// Human-readable preview with SQL literals (display only — never execute).
    ///
    /// Includes `BEGIN;` / `COMMIT;` so M2-3 can show the full transaction.
    pub fn preview_sql(&self) -> Result<String, ChangeError> {
        let mut out = String::from("BEGIN;\n");
        for stmt in &self.statements {
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
) -> Result<PlannedStatement, ChangeError> {
    let mut sql = format!(
        "DELETE FROM {}",
        qualify(dialect, &changeset.table().schema, &changeset.table().table)?
    );
    let mut params = Vec::new();
    append_pk_where(changeset, dialect, &mut sql, &mut params, pk)?;
    Ok(PlannedStatement {
        op: PlanOp::Delete,
        sql,
        params,
        pk: pk.to_vec(),
    })
}

fn build_update(
    changeset: &ChangeSet,
    dialect: Dialect,
    pk: &[Value],
    cells: &std::collections::BTreeMap<String, crate::change_set::CellChange>,
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
    Ok(PlannedStatement {
        op: PlanOp::Update,
        sql,
        params,
        pk: pk.to_vec(),
    })
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
    if cols.is_empty() {
        return Err(ChangeError::EmptyInsert);
    }

    let mut sql = format!(
        "INSERT INTO {} (",
        qualify(dialect, &changeset.table().schema, &changeset.table().table)?
    );
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
        CopyCell::NotLoaded => "NULL".into(),
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
            "UPDATE \"public\".\"books\" SET \"title\" = $1 WHERE \"id\" = $2"
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
            "UPDATE `sample`.`books` SET `title` = ? WHERE `id` = ?"
        );
        let sl = ChangePlan::build(&cs, Dialect::Sqlite).unwrap();
        assert_eq!(
            sl.statements[0].sql,
            "UPDATE \"sample\".\"books\" SET \"title\" = ? WHERE \"id\" = ?"
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
            "UPDATE \"s\"\"chema\".\"t`ab\" SET \"name\" = $1 WHERE \"a\"\"b\" = $2 AND \"c`d\" = $3"
        );
        assert_eq!(pg.statements[1].params[0], Value::Null);

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
        assert!(preview
            .contains("UPDATE \"public\".\"books\" SET \"title\" = 'O''Reilly' WHERE \"id\" = 1;"));
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
            "update WHERE: {update_sql}"
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
}
