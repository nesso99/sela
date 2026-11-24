//! Safe-mode statement classification and gating (M2-4 / US-07).
//!
//! One classifier for the SQL editor, grid commit, and history Rerun.
//! Unparseable SQL fails closed (treated as a write). `WHERE 1=1` counts as a
//! WHERE. See `docs/design/ux/09-safe-mode.md`.

use sqlparser::ast::{
    Delete, FromTable, ObjectName, ObjectNamePart, Query, Select, SetExpr, Statement, TableFactor,
    TableWithJoins, Update, UtilityOption,
};
use sqlparser::dialect::{Dialect as SqlDialect, MySqlDialect, PostgreSqlDialect, SQLiteDialect};
use sqlparser::parser::Parser;

use crate::connection::SafeMode;
use crate::types::Dialect;

/// Kind of unrestricted DML that triggers the no-WHERE dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoWhereKind {
    Update,
    Delete,
}

impl NoWhereKind {
    pub fn verb(self) -> &'static str {
        match self {
            Self::Update => "UPDATE",
            Self::Delete => "DELETE",
        }
    }
}

/// Classification of one SQL statement for safe mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatementSafety {
    /// True when the statement may write, take locks that mutate, or could not
    /// be parsed (fail closed).
    pub is_write: bool,
    /// `sqlparser` could not parse the statement.
    pub parse_failed: bool,
    /// `UPDATE` or `DELETE` with no `WHERE` (statement or CTE body).
    pub no_where: Option<NoWhereKind>,
    /// Best-effort target table for the no-WHERE dialog body.
    pub target_table: Option<String>,
}

impl StatementSafety {
    fn read() -> Self {
        Self {
            is_write: false,
            parse_failed: false,
            no_where: None,
            target_table: None,
        }
    }

    fn write() -> Self {
        Self {
            is_write: true,
            parse_failed: false,
            no_where: None,
            target_table: None,
        }
    }

    fn parse_failed() -> Self {
        Self {
            is_write: true,
            parse_failed: true,
            no_where: None,
            target_table: None,
        }
    }
}

/// What the UI should do before sending statements (editor or history Rerun).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditorGate {
    /// Run immediately (all reads, or Off with no warnings).
    Allow,
    /// Read-only connection refused a write. No dialog.
    RefuseReadOnly,
    /// Confirm a write (Confirm writes level).
    ConfirmWrite {
        /// Writing statements in the batch (one line each for run-all).
        write_sqls: Vec<String>,
        /// True when the batch also contains reads that will run only if confirmed.
        has_reads: bool,
    },
    /// `UPDATE` / `DELETE` without `WHERE` (any level except Read-only).
    ConfirmNoWhere {
        kind: NoWhereKind,
        target_table: Option<String>,
        /// The statement that triggered the warning (named in the batch body).
        sql: String,
        /// Other writes in the batch besides the triggering statement.
        other_writes: usize,
    },
    /// Parser failed; Off / Confirm show Run anyway; Read-only hides it.
    ConfirmParseFailed {
        sql: String,
        /// Other writes in the batch besides the triggering statement.
        other_writes: usize,
    },
}

/// What History Rerun should do for one stored statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryRerunGate {
    /// Open the tab and run immediately.
    Run,
    /// Classic "Rerun this statement?" confirm (reads, or Off writes with no extra warnings).
    ConfirmRerun,
    /// Read-only refused a write.
    RefuseReadOnly,
    /// Safe-mode ConfirmWrite / NoWhere / ParseFailed dialog.
    ConfirmSafeMode(EditorGate),
}

/// Grid Commit gate. Commit is always a write that has a key `WHERE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridCommitGate {
    Allow,
    Confirm,
    RefuseReadOnly,
}

/// Classify one statement for safe mode using the connection dialect.
pub fn classify_statement_safety(sql: &str, dialect: Dialect) -> StatementSafety {
    let trimmed = sql.trim();
    if trimmed.is_empty() {
        return StatementSafety::read();
    }
    let dialect_box: Box<dyn SqlDialect> = match dialect {
        Dialect::Postgres => Box::new(PostgreSqlDialect {}),
        Dialect::Mysql => Box::new(MySqlDialect {}),
        Dialect::Sqlite => Box::new(SQLiteDialect {}),
    };
    let statements = match Parser::parse_sql(dialect_box.as_ref(), trimmed) {
        Ok(stmts) => stmts,
        Err(_) => return StatementSafety::parse_failed(),
    };
    if statements.is_empty() {
        return StatementSafety::parse_failed();
    }
    // A single editor statement should parse to one AST statement. Extra
    // statements (rare) fail closed so we never skip a write.
    if statements.len() > 1 {
        return StatementSafety::parse_failed();
    }
    classify_ast(&statements[0])
}

/// Decide what to do for an editor run (one statement or a run-all batch).
pub fn decide_editor_gate(mode: SafeMode, statements: &[(String, StatementSafety)]) -> EditorGate {
    if statements.is_empty() {
        return EditorGate::Allow;
    }

    let any_write = statements.iter().any(|(_, s)| s.is_write);
    let any_read = statements.iter().any(|(_, s)| !s.is_write);
    let no_where = statements.iter().find_map(|(sql, s)| {
        s.no_where
            .map(|kind| (sql.clone(), kind, s.target_table.clone()))
    });
    let parse_failed = statements
        .iter()
        .find(|(_, s)| s.parse_failed)
        .map(|(sql, _)| sql.clone());

    if !any_write {
        return EditorGate::Allow;
    }

    if mode == SafeMode::ReadOnly {
        return EditorGate::RefuseReadOnly;
    }

    let write_count = statements.iter().filter(|(_, s)| s.is_write).count();
    let other_writes = write_count.saturating_sub(1);

    // Priority: no-WHERE replaces confirm-writes; parse-failed otherwise;
    // then Confirm writes. Off still warns on no-WHERE and parse-failed.
    if let Some((sql, kind, target_table)) = no_where {
        return EditorGate::ConfirmNoWhere {
            kind,
            target_table,
            sql,
            other_writes,
        };
    }
    if let Some(sql) = parse_failed {
        return EditorGate::ConfirmParseFailed { sql, other_writes };
    }
    match mode {
        SafeMode::Off => EditorGate::Allow,
        SafeMode::Confirm => EditorGate::ConfirmWrite {
            write_sqls: statements
                .iter()
                .filter(|(_, s)| s.is_write)
                .map(|(sql, _)| sql.clone())
                .collect(),
            has_reads: any_read,
        },
        SafeMode::ReadOnly => EditorGate::RefuseReadOnly,
    }
}

/// Decide History Rerun for one stored statement.
///
/// Writes follow [`decide_editor_gate`]. When that returns [`EditorGate::Allow`]
/// (Off, no no-WHERE / parse warning), fall through to the classic
/// [`crate::history_rerun_needs_confirm`] path so v0.1 "Rerun this statement?"
/// still asks for writes.
pub fn decide_history_rerun(mode: SafeMode, sql: &str, dialect: Dialect) -> HistoryRerunGate {
    use crate::history_rerun_needs_confirm;

    let safety = classify_statement_safety(sql, dialect);
    if safety.is_write {
        let classified = [(sql.to_string(), safety)];
        match decide_editor_gate(mode, &classified) {
            EditorGate::Allow => {
                if history_rerun_needs_confirm(sql) {
                    HistoryRerunGate::ConfirmRerun
                } else {
                    HistoryRerunGate::Run
                }
            }
            EditorGate::RefuseReadOnly => HistoryRerunGate::RefuseReadOnly,
            gate => HistoryRerunGate::ConfirmSafeMode(gate),
        }
    } else if history_rerun_needs_confirm(sql) {
        HistoryRerunGate::ConfirmRerun
    } else {
        HistoryRerunGate::Run
    }
}

/// Grid Commit is a write with a key WHERE. Never takes the no-WHERE dialog.
pub fn decide_grid_commit_gate(mode: SafeMode) -> GridCommitGate {
    match mode {
        SafeMode::Off => GridCommitGate::Allow,
        SafeMode::Confirm => GridCommitGate::Confirm,
        SafeMode::ReadOnly => GridCommitGate::RefuseReadOnly,
    }
}

fn classify_ast(stmt: &Statement) -> StatementSafety {
    match stmt {
        Statement::Query(query) => classify_query(query),
        Statement::Insert(insert) => {
            // INSERT source may carry WITH that itself writes; still a write.
            let mut safety = StatementSafety::write();
            if let Some(source) = &insert.source {
                merge_no_where(&mut safety, &classify_query(source));
            }
            safety
        }
        Statement::Update(update) => classify_update(update),
        Statement::Delete(delete) => classify_delete(delete),
        Statement::Explain {
            analyze,
            options,
            statement,
            ..
        } => {
            // EXPLAIN ANALYZE (flag or paren options) executes the plan.
            // sqlparser 0.64 leaves `analyze=false` for `EXPLAIN (ANALYZE …)`
            // and puts ANALYZE in `options` — check both. Fail closed when unclear.
            let _ = statement;
            if *analyze || explain_options_request_analyze(options.as_deref()) {
                StatementSafety::write()
            } else {
                StatementSafety::read()
            }
        }
        Statement::ExplainTable { .. } => StatementSafety::read(),
        // SHOW / DESCRIBE family — reads.
        s if is_show_like(s) => StatementSafety::read(),
        // Everything else (DDL, DML, CALL, transactions, SET, PRAGMA, …) is a write.
        _ => StatementSafety::write(),
    }
}

/// True when EXPLAIN options name ANALYZE / ANALYSE (case-insensitive).
/// An unknown option shape fails closed (treated as ANALYZE).
fn explain_options_request_analyze(options: Option<&[UtilityOption]>) -> bool {
    let Some(options) = options else {
        return false;
    };
    for opt in options {
        let name = opt.name.value.as_str();
        if name.eq_ignore_ascii_case("ANALYZE") || name.eq_ignore_ascii_case("ANALYSE") {
            // Named ANALYZE/ANALYSE: treat as execute. Fail closed on unclear args
            // (including `ANALYZE false` — prefer refusing a silent write).
            return true;
        }
        // Unknown option with an unreadable shape: fail closed if we cannot
        // inspect it further (UtilityOption always has Ident name; continue).
        let _ = &opt.arg;
    }
    false
}

fn is_show_like(stmt: &Statement) -> bool {
    matches!(
        stmt,
        Statement::ShowFunctions { .. }
            | Statement::ShowVariable { .. }
            | Statement::ShowVariables { .. }
            | Statement::ShowCreate { .. }
            | Statement::ShowColumns { .. }
            | Statement::ShowCatalogs { .. }
            | Statement::ShowDatabases { .. }
            | Statement::ShowProcessList { .. }
            | Statement::ShowSchemas { .. }
            | Statement::ShowCharset(_)
            | Statement::ShowObjects(_)
            | Statement::ShowTables { .. }
            | Statement::ShowViews { .. }
            | Statement::ShowCollation { .. }
            | Statement::ShowStatus { .. }
    )
}

fn classify_query(query: &Query) -> StatementSafety {
    let mut safety = StatementSafety::read();
    if let Some(with) = &query.with {
        for cte in &with.cte_tables {
            merge_write(&mut safety, &classify_query(&cte.query));
        }
    }
    merge_write(&mut safety, &classify_set_expr(&query.body));
    // SELECT … FOR UPDATE / FOR SHARE take row locks — treat as writes.
    if !query.locks.is_empty() {
        safety.is_write = true;
    }
    safety
}

fn classify_set_expr(expr: &SetExpr) -> StatementSafety {
    match expr {
        SetExpr::Select(select) => classify_select(select),
        SetExpr::Values(_) | SetExpr::Table(_) => StatementSafety::read(),
        SetExpr::Query(q) => classify_query(q),
        SetExpr::SetOperation { left, right, .. } => {
            let mut safety = classify_set_expr(left);
            merge_write(&mut safety, &classify_set_expr(right));
            safety
        }
        SetExpr::Insert(stmt)
        | SetExpr::Update(stmt)
        | SetExpr::Delete(stmt)
        | SetExpr::Merge(stmt) => classify_ast(stmt),
    }
}

fn classify_select(select: &Select) -> StatementSafety {
    let mut safety = StatementSafety::read();
    // SELECT … INTO creates / fills a table (or MySQL OUTFILE when parseable).
    if select.into.is_some() {
        safety.is_write = true;
    }
    // Nested queries in FROM (and JOIN) may carry INTO / locks.
    for table in &select.from {
        merge_write(&mut safety, &classify_table_with_joins(table));
    }
    safety
}

fn classify_table_with_joins(table: &TableWithJoins) -> StatementSafety {
    let mut safety = classify_table_factor(&table.relation);
    for join in &table.joins {
        merge_write(&mut safety, &classify_table_factor(&join.relation));
    }
    safety
}

fn classify_table_factor(factor: &TableFactor) -> StatementSafety {
    match factor {
        TableFactor::Derived { subquery, .. } => classify_query(subquery),
        TableFactor::NestedJoin {
            table_with_joins, ..
        } => classify_table_with_joins(table_with_joins),
        TableFactor::Pivot { table, .. } | TableFactor::Unpivot { table, .. } => {
            classify_table_factor(table)
        }
        _ => StatementSafety::read(),
    }
}

fn classify_update(update: &Update) -> StatementSafety {
    let table = table_with_joins_name(&update.table);
    if update.selection.is_none() {
        StatementSafety {
            is_write: true,
            parse_failed: false,
            no_where: Some(NoWhereKind::Update),
            target_table: table,
        }
    } else {
        let mut safety = StatementSafety::write();
        safety.target_table = table;
        safety
    }
}

fn classify_delete(delete: &Delete) -> StatementSafety {
    let table = from_table_name(&delete.from);
    if delete.selection.is_none() {
        StatementSafety {
            is_write: true,
            parse_failed: false,
            no_where: Some(NoWhereKind::Delete),
            target_table: table,
        }
    } else {
        let mut safety = StatementSafety::write();
        safety.target_table = table;
        safety
    }
}

fn merge_write(into: &mut StatementSafety, other: &StatementSafety) {
    if other.is_write {
        into.is_write = true;
    }
    if other.parse_failed {
        into.parse_failed = true;
        into.is_write = true;
    }
    merge_no_where(into, other);
}

fn merge_no_where(into: &mut StatementSafety, other: &StatementSafety) {
    if into.no_where.is_none() {
        if let Some(kind) = other.no_where {
            into.no_where = Some(kind);
            if into.target_table.is_none() {
                into.target_table = other.target_table.clone();
            }
        }
    }
}

fn from_table_name(from: &FromTable) -> Option<String> {
    let tables = match from {
        FromTable::WithFromKeyword(t) | FromTable::WithoutKeyword(t) => t,
    };
    tables.first().and_then(table_with_joins_name)
}

fn table_with_joins_name(table: &TableWithJoins) -> Option<String> {
    match &table.relation {
        TableFactor::Table { name, .. } => Some(object_name_display(name)),
        _ => None,
    }
}

fn object_name_display(name: &ObjectName) -> String {
    name.0
        .iter()
        .map(|part| match part {
            ObjectNamePart::Identifier(ident) => ident.value.clone(),
            other => other.to_string(),
        })
        .collect::<Vec<_>>()
        .join(".")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn safety(sql: &str, dialect: Dialect) -> StatementSafety {
        classify_statement_safety(sql, dialect)
    }

    fn assert_read(sql: &str, dialect: Dialect) {
        let s = safety(sql, dialect);
        assert!(
            !s.is_write && !s.parse_failed && s.no_where.is_none(),
            "expected read for {sql:?} on {dialect:?}: {s:?}"
        );
    }

    fn assert_write(sql: &str, dialect: Dialect) {
        let s = safety(sql, dialect);
        assert!(
            s.is_write && !s.parse_failed,
            "expected write for {sql:?} on {dialect:?}: {s:?}"
        );
    }

    fn assert_no_where(sql: &str, dialect: Dialect, kind: NoWhereKind) {
        let s = safety(sql, dialect);
        assert_eq!(
            s.no_where,
            Some(kind),
            "expected no-WHERE {kind:?} for {sql:?} on {dialect:?}: {s:?}"
        );
        assert!(s.is_write);
    }

    fn assert_parse_failed(sql: &str, dialect: Dialect) {
        let s = safety(sql, dialect);
        assert!(
            s.parse_failed && s.is_write,
            "expected parse failure for {sql:?} on {dialect:?}: {s:?}"
        );
    }

    #[test]
    fn reads_select_values_show_explain() {
        for dialect in [Dialect::Postgres, Dialect::Mysql, Dialect::Sqlite] {
            assert_read("SELECT 1", dialect);
            assert_read("VALUES (1), (2)", dialect);
            assert_read("EXPLAIN SELECT 1", dialect);
        }
        assert_read("SHOW search_path", Dialect::Postgres);
        assert_read("SHOW TABLES", Dialect::Mysql);
        // Function call in SELECT stays a read (same caution as load-more limit).
        assert_read("SELECT my_func()", Dialect::Postgres);
    }

    #[test]
    fn explain_analyze_is_write() {
        for dialect in [Dialect::Postgres, Dialect::Mysql, Dialect::Sqlite] {
            assert_write("EXPLAIN ANALYZE SELECT 1", dialect);
        }
        // sqlparser 0.64: paren form leaves analyze=false and puts ANALYZE in options.
        assert_write("EXPLAIN (ANALYZE) DELETE FROM t", Dialect::Postgres);
        assert_write(
            "EXPLAIN (ANALYZE true, BUFFERS) SELECT 1",
            Dialect::Postgres,
        );
        assert_write("EXPLAIN (ANALYZE false) SELECT 1", Dialect::Postgres);
    }

    #[test]
    fn select_into_and_locks_are_writes() {
        assert_write("SELECT a INTO b FROM t", Dialect::Postgres);
        assert_write("SELECT * FROM t FOR UPDATE", Dialect::Postgres);
        assert_write("SELECT * FROM t FOR SHARE", Dialect::Postgres);
        assert_write(
            "(SELECT 1) UNION ALL (SELECT 2) FOR UPDATE",
            Dialect::Postgres,
        );
        // Nested / set-expression / CTE bodies still see INTO.
        assert_write(
            "SELECT * FROM (SELECT a INTO b FROM t) AS x",
            Dialect::Postgres,
        );
        assert_write("(SELECT a INTO b FROM t) UNION SELECT 1", Dialect::Postgres);
        assert_write(
            "WITH x AS (SELECT a INTO b FROM t) SELECT * FROM x",
            Dialect::Postgres,
        );
    }

    #[test]
    fn classifier_writes_misc_statements() {
        for dialect in [Dialect::Postgres, Dialect::Mysql, Dialect::Sqlite] {
            assert_write("TRUNCATE TABLE t", dialect);
            assert_write("SET a = 1", dialect);
            assert_write("PREPARE p AS SELECT 1", dialect);
            assert_write("EXECUTE p", dialect);
        }
        assert_write("COPY t TO STDOUT", Dialect::Postgres);
        assert_write("CALL foo()", Dialect::Postgres);
        assert_write("GRANT SELECT ON t TO u", Dialect::Postgres);
        assert_write("VACUUM", Dialect::Postgres);
        assert_write("LOCK TABLE t IN ACCESS EXCLUSIVE MODE", Dialect::Postgres);
        assert_write(
            "MERGE INTO t USING s ON true WHEN MATCHED THEN DELETE",
            Dialect::Postgres,
        );
        assert_write("ATTACH DATABASE 'x' AS y", Dialect::Sqlite);
        assert_write("PRAGMA x = 1", Dialect::Sqlite);
    }

    #[test]
    fn mysql_version_comment_delete_is_no_where() {
        assert_no_where(
            "/*!50000 DELETE FROM t */",
            Dialect::Mysql,
            NoWhereKind::Delete,
        );
    }

    #[test]
    fn fail_closed_smuggle_and_unparsed_writes() {
        // Multi-statement smuggle via MySQL version comment.
        assert_parse_failed("SELECT 1 /*!; DELETE FROM t */", Dialect::Mysql);
        // Unparsed / unsupported forms fail closed.
        assert_parse_failed("DO $$ BEGIN NULL; END $$", Dialect::Postgres);
        assert_parse_failed("LOAD DATA INFILE 'x' INTO TABLE t", Dialect::Mysql);
        assert_parse_failed("SELECT 1 INTO OUTFILE '/tmp/x'", Dialect::Mysql);
    }

    #[test]
    fn batch_no_where_reports_other_writes() {
        let stmts = [
            ("SELECT 1".into(), StatementSafety::read()),
            (
                "DELETE FROM t".into(),
                StatementSafety {
                    is_write: true,
                    parse_failed: false,
                    no_where: Some(NoWhereKind::Delete),
                    target_table: Some("t".into()),
                },
            ),
            (
                "INSERT INTO t (a) VALUES (1)".into(),
                StatementSafety::write(),
            ),
            (
                "UPDATE t SET a = 2 WHERE id = 1".into(),
                StatementSafety::write(),
            ),
        ];
        match decide_editor_gate(SafeMode::Off, &stmts) {
            EditorGate::ConfirmNoWhere { other_writes, .. } => {
                assert_eq!(other_writes, 2);
            }
            other => panic!("expected ConfirmNoWhere, got {other:?}"),
        }
        let parse_batch = [
            ("???".into(), StatementSafety::parse_failed()),
            (
                "INSERT INTO t (a) VALUES (1)".into(),
                StatementSafety::write(),
            ),
        ];
        match decide_editor_gate(SafeMode::Confirm, &parse_batch) {
            EditorGate::ConfirmParseFailed { other_writes, .. } => {
                assert_eq!(other_writes, 1);
            }
            other => panic!("expected ConfirmParseFailed, got {other:?}"),
        }
    }

    #[test]
    fn history_rerun_off_write_asks_classic_confirm() {
        // Off write without no-WHERE / parse warning → classic Rerun confirm.
        assert_eq!(
            decide_history_rerun(
                SafeMode::Off,
                "UPDATE t SET a = 1 WHERE id = 1",
                Dialect::Postgres
            ),
            HistoryRerunGate::ConfirmRerun
        );
        // Safe read may still skip classic confirm when load-more says safe.
        assert_eq!(
            decide_history_rerun(SafeMode::Off, "SELECT 1", Dialect::Postgres),
            HistoryRerunGate::Run
        );
        assert_eq!(
            decide_history_rerun(
                SafeMode::ReadOnly,
                "UPDATE t SET a = 1 WHERE id = 1",
                Dialect::Postgres
            ),
            HistoryRerunGate::RefuseReadOnly
        );
        assert!(matches!(
            decide_history_rerun(
                SafeMode::Confirm,
                "UPDATE t SET a = 1 WHERE id = 1",
                Dialect::Postgres
            ),
            HistoryRerunGate::ConfirmSafeMode(EditorGate::ConfirmWrite { .. })
        ));
    }

    #[test]
    fn writes_dml_ddl_transactions() {
        for dialect in [Dialect::Postgres, Dialect::Mysql, Dialect::Sqlite] {
            assert_write("INSERT INTO t (a) VALUES (1)", dialect);
            assert_write("UPDATE t SET a = 1 WHERE id = 1", dialect);
            assert_write("DELETE FROM t WHERE id = 1", dialect);
            assert_write("CREATE TABLE t (id INT)", dialect);
            assert_write("DROP TABLE t", dialect);
            assert_write("ALTER TABLE t ADD COLUMN x INT", dialect);
            assert_write("BEGIN", dialect);
            assert_write("COMMIT", dialect);
            assert_write("ROLLBACK", dialect);
        }
    }

    #[test]
    fn no_where_update_and_delete() {
        for dialect in [Dialect::Postgres, Dialect::Mysql, Dialect::Sqlite] {
            assert_no_where("UPDATE t SET a = 1", dialect, NoWhereKind::Update);
            assert_no_where("DELETE FROM t", dialect, NoWhereKind::Delete);
            // WHERE 1=1 counts as a WHERE — do not warn.
            let s = safety("UPDATE t SET a = 1 WHERE 1 = 1", dialect);
            assert!(s.is_write);
            assert!(s.no_where.is_none(), "{s:?}");
            let s = safety("DELETE FROM t WHERE 1 = 1", dialect);
            assert!(s.is_write);
            assert!(s.no_where.is_none(), "{s:?}");
        }
    }

    #[test]
    fn comments_and_string_literals_with_keywords() {
        for dialect in [Dialect::Postgres, Dialect::Mysql, Dialect::Sqlite] {
            assert_read("SELECT 'UPDATE t SET a = 1' AS s", dialect);
            assert_read("SELECT 'DELETE FROM t' AS s", dialect);
            assert_read("/* UPDATE t SET a = 1 */ SELECT 1", dialect);
            assert_read("-- DELETE FROM t\nSELECT 1", dialect);
            assert_write("/* note */ UPDATE t SET a = 1 WHERE id = 1", dialect);
        }
    }

    #[test]
    fn cte_with_write_is_write_and_no_where_inside_cte() {
        // Postgres data-modifying CTE.
        let sql = "WITH u AS (UPDATE t SET a = 1 RETURNING *) SELECT * FROM u";
        let s = safety(sql, Dialect::Postgres);
        assert!(s.is_write, "{s:?}");
        assert_eq!(s.no_where, Some(NoWhereKind::Update), "{s:?}");

        let sql = "WITH u AS (UPDATE t SET a = 1 WHERE id = 1 RETURNING *) SELECT * FROM u";
        let s = safety(sql, Dialect::Postgres);
        assert!(s.is_write, "{s:?}");
        assert!(s.no_where.is_none(), "{s:?}");

        let sql = "WITH d AS (DELETE FROM t RETURNING *) SELECT * FROM d";
        let s = safety(sql, Dialect::Postgres);
        assert!(s.is_write, "{s:?}");
        assert_eq!(s.no_where, Some(NoWhereKind::Delete), "{s:?}");

        // Read-only CTE stays a read.
        assert_read(
            "WITH t AS (SELECT 1 AS a) SELECT * FROM t",
            Dialect::Postgres,
        );
        assert_read("WITH t AS (SELECT 1 AS a) SELECT * FROM t", Dialect::Mysql);
        assert_read("WITH t AS (SELECT 1 AS a) SELECT * FROM t", Dialect::Sqlite);
    }

    #[test]
    fn unparseable_fails_closed() {
        for dialect in [Dialect::Postgres, Dialect::Mysql, Dialect::Sqlite] {
            assert_parse_failed("NOT VALID SQL !!!", dialect);
            assert_parse_failed("SELECT FROM", dialect);
            // Two statements in one buffer without going through split_statements
            // fail closed so a write cannot hide behind a read.
            assert_parse_failed("SELECT 1; DELETE FROM t", dialect);
        }
    }

    #[test]
    fn run_all_batch_gating_multiple_statements() {
        let stmts = [
            ("SELECT 1".into(), StatementSafety::read()),
            (
                "INSERT INTO t (a) VALUES (1)".into(),
                StatementSafety::write(),
            ),
            ("SELECT 2".into(), StatementSafety::read()),
        ];
        match decide_editor_gate(SafeMode::Confirm, &stmts) {
            EditorGate::ConfirmWrite {
                write_sqls,
                has_reads,
            } => {
                assert!(has_reads);
                assert_eq!(write_sqls, vec!["INSERT INTO t (a) VALUES (1)".to_string()]);
            }
            other => panic!("expected ConfirmWrite, got {other:?}"),
        }
        assert_eq!(decide_editor_gate(SafeMode::Off, &stmts), EditorGate::Allow);
    }

    #[test]
    fn gate_off_allows_writes_but_warns_no_where_and_parse() {
        let write = (
            "UPDATE t SET a = 1 WHERE id = 1".into(),
            StatementSafety::write(),
        );
        assert_eq!(
            decide_editor_gate(SafeMode::Off, std::slice::from_ref(&write)),
            EditorGate::Allow
        );

        let no_where = (
            "DELETE FROM t".into(),
            StatementSafety {
                is_write: true,
                parse_failed: false,
                no_where: Some(NoWhereKind::Delete),
                target_table: Some("t".into()),
            },
        );
        match decide_editor_gate(SafeMode::Off, &[no_where]) {
            EditorGate::ConfirmNoWhere {
                kind: NoWhereKind::Delete,
                ..
            } => {}
            other => panic!("expected ConfirmNoWhere, got {other:?}"),
        }

        let bad = ("???".into(), StatementSafety::parse_failed());
        match decide_editor_gate(SafeMode::Off, &[bad]) {
            EditorGate::ConfirmParseFailed { .. } => {}
            other => panic!("expected ConfirmParseFailed, got {other:?}"),
        }
    }

    #[test]
    fn gate_confirm_asks_on_write_batch_with_reads() {
        let stmts = [
            ("SELECT 1".into(), StatementSafety::read()),
            (
                "UPDATE t SET a = 1 WHERE id = 1".into(),
                StatementSafety::write(),
            ),
        ];
        match decide_editor_gate(SafeMode::Confirm, &stmts) {
            EditorGate::ConfirmWrite {
                write_sqls,
                has_reads,
            } => {
                assert!(has_reads);
                assert_eq!(write_sqls.len(), 1);
            }
            other => panic!("expected ConfirmWrite, got {other:?}"),
        }
    }

    #[test]
    fn gate_confirm_no_where_replaces_confirm_write() {
        let stmts = [(
            "UPDATE t SET a = 1".into(),
            StatementSafety {
                is_write: true,
                parse_failed: false,
                no_where: Some(NoWhereKind::Update),
                target_table: Some("t".into()),
            },
        )];
        match decide_editor_gate(SafeMode::Confirm, &stmts) {
            EditorGate::ConfirmNoWhere {
                kind: NoWhereKind::Update,
                ..
            } => {}
            other => panic!("expected ConfirmNoWhere, got {other:?}"),
        }
    }

    #[test]
    fn gate_read_only_refuses_writes_allows_reads() {
        let read = ("SELECT 1".into(), StatementSafety::read());
        assert_eq!(
            decide_editor_gate(SafeMode::ReadOnly, &[read]),
            EditorGate::Allow
        );
        let write = (
            "UPDATE t SET a = 1 WHERE id = 1".into(),
            StatementSafety::write(),
        );
        assert_eq!(
            decide_editor_gate(SafeMode::ReadOnly, &[write]),
            EditorGate::RefuseReadOnly
        );
        let no_where = (
            "DELETE FROM t".into(),
            StatementSafety {
                is_write: true,
                parse_failed: false,
                no_where: Some(NoWhereKind::Delete),
                target_table: Some("t".into()),
            },
        );
        assert_eq!(
            decide_editor_gate(SafeMode::ReadOnly, &[no_where]),
            EditorGate::RefuseReadOnly
        );
        let bad = ("???".into(), StatementSafety::parse_failed());
        assert_eq!(
            decide_editor_gate(SafeMode::ReadOnly, &[bad]),
            EditorGate::RefuseReadOnly
        );
    }

    #[test]
    fn grid_commit_gate() {
        assert_eq!(
            decide_grid_commit_gate(SafeMode::Off),
            GridCommitGate::Allow
        );
        assert_eq!(
            decide_grid_commit_gate(SafeMode::Confirm),
            GridCommitGate::Confirm
        );
        assert_eq!(
            decide_grid_commit_gate(SafeMode::ReadOnly),
            GridCommitGate::RefuseReadOnly
        );
    }

    #[test]
    fn target_table_extracted_for_no_where() {
        let s = safety("UPDATE public.books SET title = 'x'", Dialect::Postgres);
        assert_eq!(s.no_where, Some(NoWhereKind::Update));
        assert_eq!(s.target_table.as_deref(), Some("public.books"));

        let s = safety("DELETE FROM `orders`", Dialect::Mysql);
        assert_eq!(s.no_where, Some(NoWhereKind::Delete));
        assert_eq!(s.target_table.as_deref(), Some("orders"));
    }
}
