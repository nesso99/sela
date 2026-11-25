//! Golden snapshots for ChangePlan SQL (M2-2).
//!
//! Expected files live under `tests/change_plan_snapshots/`. Regenerate by
//! updating those files when intentional SQL shape changes land.

use std::collections::BTreeMap;
use std::path::PathBuf;

use sela_core::{ChangePlan, ChangeSet, Dialect, TableRef, Value};

fn snapshot_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/change_plan_snapshots")
}

fn assert_snapshot(name: &str, dialect: Dialect, actual: &str) {
    let path = snapshot_dir().join(format!("{name}.{}.sql", dialect_label(dialect)));
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    // Normalize line endings so a CRLF checkout (e.g. Windows) still matches.
    let expected = expected.replace("\r\n", "\n");
    assert_eq!(
        actual.trim_end(),
        expected.trim_end(),
        "snapshot mismatch for {} ({dialect:?})\n--- actual ---\n{actual}\n--- expected ---\n{expected}",
        path.display()
    );
}

fn dialect_label(dialect: Dialect) -> &'static str {
    match dialect {
        Dialect::Postgres => "postgres",
        Dialect::Mysql => "mysql",
        Dialect::Sqlite => "sqlite",
    }
}

fn mixed_changeset() -> ChangeSet {
    let mut cs = ChangeSet::try_new(
        TableRef::new("public", "items"),
        vec!["tenant_id".into(), "id".into()],
        vec![
            "tenant_id".into(),
            "id".into(),
            "name".into(),
            "qty".into(),
            "note".into(),
        ],
    )
    .unwrap();

    // Update with NULL new value.
    cs.set_cell(
        &[Value::Int(1), Value::Int(10)],
        "name",
        Value::text("alpha"),
        Value::Null,
    )
    .unwrap();
    cs.set_cell(
        &[Value::Int(1), Value::Int(10)],
        "qty",
        Value::Int(3),
        Value::Int(7),
    )
    .unwrap();

    // Delete (composite PK; second component NULL → IS NULL).
    cs.delete_row(&[Value::Int(2), Value::Null]).unwrap();

    // Insert with odd-ish values including NULL.
    let mut values = BTreeMap::new();
    values.insert("tenant_id".into(), Value::Int(1));
    values.insert("id".into(), Value::Int(99));
    values.insert("name".into(), Value::text("O'Reilly"));
    values.insert("qty".into(), Value::Int(1));
    values.insert("note".into(), Value::Null);
    cs.insert_row(values).unwrap();

    // Insert that is later deleted must not appear in the plan.
    let mut ghost = BTreeMap::new();
    ghost.insert("tenant_id".into(), Value::Int(1));
    ghost.insert("id".into(), Value::Int(100));
    ghost.insert("name".into(), Value::text("ghost"));
    let gid = cs.insert_row(ghost).unwrap();
    cs.delete_insert(gid).unwrap();

    // Cell revert must drop the whole update for that other row.
    cs.set_cell(
        &[Value::Int(9), Value::Int(9)],
        "name",
        Value::text("tmp"),
        Value::text("changed"),
    )
    .unwrap();
    cs.set_cell(
        &[Value::Int(9), Value::Int(9)],
        "name",
        Value::text("ignored"),
        Value::text("tmp"),
    )
    .unwrap();

    cs
}

fn odd_ident_changeset() -> ChangeSet {
    let mut cs = ChangeSet::try_new(
        TableRef::new("s\"chema", "t`able"),
        vec!["a\"b".into()],
        vec!["a\"b".into(), "c`ol".into()],
    )
    .unwrap();
    cs.set_cell(
        &[Value::Int(1)],
        "c`ol",
        Value::text("old"),
        Value::text("new"),
    )
    .unwrap();
    cs
}

#[test]
fn golden_mixed_plan_per_dialect() {
    let cs = mixed_changeset();
    for dialect in [Dialect::Postgres, Dialect::Mysql, Dialect::Sqlite] {
        let plan = ChangePlan::build(&cs, dialect).unwrap();
        // Bound SQL (placeholders) — one statement per line.
        let mut bound = String::new();
        for stmt in &plan.statements {
            bound.push_str(&stmt.sql);
            bound.push('\n');
        }
        assert_snapshot("mixed_bound", dialect, &bound);

        let preview = plan.preview_sql().unwrap();
        assert_snapshot("mixed_preview", dialect, &preview);
    }
}

#[test]
fn golden_odd_identifiers() {
    let cs = odd_ident_changeset();
    for dialect in [Dialect::Postgres, Dialect::Mysql, Dialect::Sqlite] {
        let plan = ChangePlan::build(&cs, dialect).unwrap();
        let mut bound = String::new();
        for stmt in &plan.statements {
            bound.push_str(&stmt.sql);
            bound.push('\n');
        }
        assert_snapshot("odd_idents_bound", dialect, &bound);
    }
}

#[test]
fn merge_rules_leave_expected_ops() {
    let cs = mixed_changeset();
    // delete + update + insert (ghost insert cancelled; reverted update gone).
    assert_eq!(cs.changes().len(), 3);
    let plan = ChangePlan::build(&cs, Dialect::Postgres).unwrap();
    assert_eq!(plan.statements.len(), 3);
    assert_eq!(plan.statements[0].op, sela_core::PlanOp::Delete);
    assert_eq!(plan.statements[1].op, sela_core::PlanOp::Update);
    assert_eq!(plan.statements[2].op, sela_core::PlanOp::Insert);
}
