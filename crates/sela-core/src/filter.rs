//! Table filter / sort SQL builder (M1-3).
//!
//! Builds `SELECT * FROM … WHERE … ORDER BY …` with dialect-quoted identifiers
//! and **bound parameters only** — filter values are never string-interpolated
//! into SQL. LIKE wildcards in user values are escaped.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::sql::{qualify, quote_ident, SqlError};
use crate::types::{Dialect, Value};

/// Comparison / pattern operator for one filter condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FilterOp {
    #[default]
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Contains,
    NotContains,
    StartsWith,
    EndsWith,
    IsNull,
    IsNotNull,
    In,
}

impl FilterOp {
    pub fn label(self) -> &'static str {
        match self {
            Self::Eq => "=",
            Self::Ne => "≠",
            Self::Lt => "<",
            Self::Le => "≤",
            Self::Gt => ">",
            Self::Ge => "≥",
            Self::Contains => "contains",
            Self::NotContains => "not contains",
            Self::StartsWith => "starts with",
            Self::EndsWith => "ends with",
            Self::IsNull => "is null",
            Self::IsNotNull => "is not null",
            Self::In => "in",
        }
    }

    pub fn needs_value(self) -> bool {
        !matches!(self, Self::IsNull | Self::IsNotNull)
    }

    pub const ALL: &'static [FilterOp] = &[
        FilterOp::Eq,
        FilterOp::Ne,
        FilterOp::Lt,
        FilterOp::Le,
        FilterOp::Gt,
        FilterOp::Ge,
        FilterOp::Contains,
        FilterOp::NotContains,
        FilterOp::StartsWith,
        FilterOp::EndsWith,
        FilterOp::IsNull,
        FilterOp::IsNotNull,
        FilterOp::In,
    ];

    pub fn next(self) -> Self {
        let all = Self::ALL;
        let ix = all.iter().position(|op| *op == self).unwrap_or(0);
        all[(ix + 1) % all.len()]
    }
}

/// One AND-combined filter condition (column + operator + typed value text).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilterCondition {
    pub column: String,
    pub op: FilterOp,
    /// User-entered value. Ignored for [`FilterOp::IsNull`] / [`FilterOp::IsNotNull`].
    /// For [`FilterOp::In`], a comma-separated list.
    #[serde(default)]
    pub value: String,
    /// Optional SQL type name from the result header (guides value parsing).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column_type: Option<String>,
}

impl FilterCondition {
    pub fn new(column: impl Into<String>, op: FilterOp, value: impl Into<String>) -> Self {
        Self {
            column: column.into(),
            op,
            value: value.into(),
            column_type: None,
        }
    }

    pub fn with_type(mut self, type_name: impl Into<String>) -> Self {
        self.column_type = Some(type_name.into());
        self
    }
}

/// Ascending or descending sort on one column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortDirection {
    Asc,
    Desc,
}

impl SortDirection {
    pub fn sql(self) -> &'static str {
        match self {
            Self::Asc => "ASC",
            Self::Desc => "DESC",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SortSpec {
    pub column: String,
    pub direction: SortDirection,
}

impl SortSpec {
    pub fn asc(column: impl Into<String>) -> Self {
        Self {
            column: column.into(),
            direction: SortDirection::Asc,
        }
    }

    pub fn desc(column: impl Into<String>) -> Self {
        Self {
            column: column.into(),
            direction: SortDirection::Desc,
        }
    }
}

/// SQL text plus ordered bind parameters (never interpolated).
#[derive(Debug, Clone, PartialEq)]
pub struct BoundSql {
    pub sql: String,
    pub params: Vec<Value>,
}

/// Escape `\`, `%`, and `_` for `LIKE … ESCAPE '\'` patterns.
pub fn escape_like(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' | '%' | '_' => {
                out.push('\\');
                out.push(ch);
            }
            other => out.push(other),
        }
    }
    out
}

/// Parse a filter value using an optional column type hint.
///
/// Tries int / float / bool / date / uuid when the type name suggests it;
/// otherwise (or on parse failure) returns [`Value::Text`] so the engine can
/// cast or error.
pub fn parse_filter_value(raw: &str, type_name: Option<&str>) -> Value {
    let hint = type_name.unwrap_or("").to_ascii_lowercase();
    if hint_is_bool(&hint) {
        if let Some(b) = parse_bool(raw) {
            return Value::Bool(b);
        }
    }
    if hint_is_int(&hint) {
        if let Ok(n) = raw.trim().parse::<i64>() {
            return Value::Int(n);
        }
    }
    if hint_is_float(&hint) {
        if let Ok(n) = raw.trim().parse::<f64>() {
            return Value::Float(n);
        }
    }
    if hint_is_uuid(&hint) {
        if let Ok(u) = Uuid::parse_str(raw.trim()) {
            return Value::Uuid(u);
        }
    }
    if hint_is_date(&hint) {
        if let Ok(d) = NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d") {
            return Value::Date(d);
        }
    }
    // No hint (or hint failed): try common literals, then text.
    if let Some(b) = parse_bool(raw) {
        return Value::Bool(b);
    }
    if let Ok(n) = raw.trim().parse::<i64>() {
        // Prefer text when the column is clearly textual so `id = '1'` style
        // still works when the user typed digits into a text column — but
        // without a hint, binding as int is fine for numeric comparisons.
        if hint.is_empty() || hint_is_int(&hint) || hint_is_float(&hint) {
            return Value::Int(n);
        }
    }
    Value::text(raw)
}

fn hint_is_bool(hint: &str) -> bool {
    hint.contains("bool") || hint == "bit" || hint == "tinyint(1)"
}

fn hint_is_int(hint: &str) -> bool {
    hint.contains("int")
        || hint.contains("serial")
        || hint == "integer"
        || hint.starts_with("number")
}

fn hint_is_float(hint: &str) -> bool {
    hint.contains("float")
        || hint.contains("double")
        || hint.contains("real")
        || hint.contains("numeric")
        || hint.contains("decimal")
        || hint.contains("money")
}

fn hint_is_uuid(hint: &str) -> bool {
    hint.contains("uuid")
}

fn hint_is_date(hint: &str) -> bool {
    hint == "date" || hint.starts_with("date(")
}

fn parse_bool(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" | "t" | "yes" | "y" | "1" => Some(true),
        "false" | "f" | "no" | "n" | "0" => Some(false),
        _ => None,
    }
}

fn placeholder(dialect: Dialect, index: usize) -> String {
    match dialect {
        Dialect::Postgres => format!("${}", index + 1),
        Dialect::Mysql | Dialect::Sqlite => "?".into(),
    }
}

fn like_op(dialect: Dialect, negated: bool) -> &'static str {
    match (dialect, negated) {
        (Dialect::Postgres, false) => "ILIKE",
        (Dialect::Postgres, true) => "NOT ILIKE",
        (_, false) => "LIKE",
        (_, true) => "NOT LIKE",
    }
}

/// `SELECT * FROM schema.table` plus optional WHERE (AND) and ORDER BY.
pub fn build_table_select(
    dialect: Dialect,
    schema: &str,
    table: &str,
    filters: &[FilterCondition],
    sorts: &[SortSpec],
) -> Result<BoundSql, SqlError> {
    let mut sql = format!("SELECT * FROM {}", qualify(dialect, schema, table)?);
    let mut params = Vec::new();
    append_where(dialect, &mut sql, &mut params, filters)?;
    append_order_by(dialect, &mut sql, sorts)?;
    Ok(BoundSql { sql, params })
}

/// Exact `COUNT(*)` for a table with the same WHERE as the preview.
pub fn build_table_count(
    dialect: Dialect,
    schema: &str,
    table: &str,
    filters: &[FilterCondition],
) -> Result<BoundSql, SqlError> {
    let mut sql = format!(
        "SELECT COUNT(*) AS sela_count FROM {}",
        qualify(dialect, schema, table)?
    );
    let mut params = Vec::new();
    append_where(dialect, &mut sql, &mut params, filters)?;
    Ok(BoundSql { sql, params })
}

/// Page a filtered/sorted table preview with `LIMIT`/`OFFSET` (no subquery wrap).
pub fn build_table_page(
    dialect: Dialect,
    schema: &str,
    table: &str,
    filters: &[FilterCondition],
    sorts: &[SortSpec],
    limit: u32,
    offset: u64,
) -> Result<BoundSql, SqlError> {
    let mut bound = build_table_select(dialect, schema, table, filters, sorts)?;
    // LIMIT/OFFSET are always integers we control — safe to inline.
    bound
        .sql
        .push_str(&format!(" LIMIT {limit} OFFSET {offset}"));
    Ok(bound)
}

fn append_where(
    dialect: Dialect,
    sql: &mut String,
    params: &mut Vec<Value>,
    filters: &[FilterCondition],
) -> Result<(), SqlError> {
    let active: Vec<&FilterCondition> = filters
        .iter()
        .filter(|f| !f.column.trim().is_empty())
        .filter(|f| f.op.needs_value() || matches!(f.op, FilterOp::IsNull | FilterOp::IsNotNull))
        .filter(|f| {
            if f.op.needs_value() {
                !f.value.is_empty() || matches!(f.op, FilterOp::In)
            } else {
                true
            }
        })
        .collect();
    // Empty IN list is invalid — skip those rows.
    let active: Vec<&FilterCondition> = active
        .into_iter()
        .filter(|f| {
            if f.op == FilterOp::In {
                !split_in_list(&f.value).is_empty()
            } else {
                true
            }
        })
        .collect();
    if active.is_empty() {
        return Ok(());
    }
    sql.push_str(" WHERE ");
    for (i, filter) in active.iter().enumerate() {
        if i > 0 {
            sql.push_str(" AND ");
        }
        append_condition(dialect, sql, params, filter)?;
    }
    Ok(())
}

fn append_order_by(dialect: Dialect, sql: &mut String, sorts: &[SortSpec]) -> Result<(), SqlError> {
    let active: Vec<&SortSpec> = sorts
        .iter()
        .filter(|s| !s.column.trim().is_empty())
        .collect();
    if active.is_empty() {
        return Ok(());
    }
    sql.push_str(" ORDER BY ");
    for (i, sort) in active.iter().enumerate() {
        if i > 0 {
            sql.push_str(", ");
        }
        sql.push_str(&quote_ident(dialect, sort.column.trim())?);
        sql.push(' ');
        sql.push_str(sort.direction.sql());
    }
    Ok(())
}

fn append_condition(
    dialect: Dialect,
    sql: &mut String,
    params: &mut Vec<Value>,
    filter: &FilterCondition,
) -> Result<(), SqlError> {
    let col = quote_ident(dialect, filter.column.trim())?;
    let type_hint = filter.column_type.as_deref();
    match filter.op {
        FilterOp::Eq => {
            let ph = placeholder(dialect, params.len());
            sql.push_str(&format!("{col} = {ph}"));
            params.push(parse_filter_value(&filter.value, type_hint));
        }
        FilterOp::Ne => {
            let ph = placeholder(dialect, params.len());
            sql.push_str(&format!("{col} <> {ph}"));
            params.push(parse_filter_value(&filter.value, type_hint));
        }
        FilterOp::Lt => {
            let ph = placeholder(dialect, params.len());
            sql.push_str(&format!("{col} < {ph}"));
            params.push(parse_filter_value(&filter.value, type_hint));
        }
        FilterOp::Le => {
            let ph = placeholder(dialect, params.len());
            sql.push_str(&format!("{col} <= {ph}"));
            params.push(parse_filter_value(&filter.value, type_hint));
        }
        FilterOp::Gt => {
            let ph = placeholder(dialect, params.len());
            sql.push_str(&format!("{col} > {ph}"));
            params.push(parse_filter_value(&filter.value, type_hint));
        }
        FilterOp::Ge => {
            let ph = placeholder(dialect, params.len());
            sql.push_str(&format!("{col} >= {ph}"));
            params.push(parse_filter_value(&filter.value, type_hint));
        }
        FilterOp::Contains => {
            let ph = placeholder(dialect, params.len());
            let op = like_op(dialect, false);
            sql.push_str(&format!("{col} {op} {ph} ESCAPE '\\'"));
            let pattern = format!("%{}%", escape_like(&filter.value));
            params.push(Value::text(pattern));
        }
        FilterOp::NotContains => {
            let ph = placeholder(dialect, params.len());
            let op = like_op(dialect, true);
            sql.push_str(&format!("{col} {op} {ph} ESCAPE '\\'"));
            let pattern = format!("%{}%", escape_like(&filter.value));
            params.push(Value::text(pattern));
        }
        FilterOp::StartsWith => {
            let ph = placeholder(dialect, params.len());
            let op = like_op(dialect, false);
            sql.push_str(&format!("{col} {op} {ph} ESCAPE '\\'"));
            let pattern = format!("{}%", escape_like(&filter.value));
            params.push(Value::text(pattern));
        }
        FilterOp::EndsWith => {
            let ph = placeholder(dialect, params.len());
            let op = like_op(dialect, false);
            sql.push_str(&format!("{col} {op} {ph} ESCAPE '\\'"));
            let pattern = format!("%{}", escape_like(&filter.value));
            params.push(Value::text(pattern));
        }
        FilterOp::IsNull => {
            sql.push_str(&format!("{col} IS NULL"));
        }
        FilterOp::IsNotNull => {
            sql.push_str(&format!("{col} IS NOT NULL"));
        }
        FilterOp::In => {
            let items = split_in_list(&filter.value);
            if items.is_empty() {
                return Err(SqlError::EmptyIdent);
            }
            sql.push_str(&format!("{col} IN ("));
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    sql.push_str(", ");
                }
                let ph = placeholder(dialect, params.len());
                sql.push_str(&ph);
                params.push(parse_filter_value(item, type_hint));
            }
            sql.push(')');
        }
    }
    Ok(())
}

fn split_in_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Human-readable preview of generated SQL (placeholders, not bound values).
pub fn display_bound_sql(bound: &BoundSql) -> String {
    if bound.params.is_empty() {
        bound.sql.clone()
    } else {
        format!(
            "{}  -- {} bound param{}",
            bound.sql,
            bound.params.len(),
            if bound.params.len() == 1 { "" } else { "s" }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_and_binds_equality_per_dialect() {
        let filters = [FilterCondition::new("name", FilterOp::Eq, "Ada")];
        let pg = build_table_select(Dialect::Postgres, "public", "books", &filters, &[]).unwrap();
        assert_eq!(
            pg.sql,
            "SELECT * FROM \"public\".\"books\" WHERE \"name\" = $1"
        );
        assert_eq!(pg.params, vec![Value::text("Ada")]);

        let my = build_table_select(Dialect::Mysql, "sample", "books", &filters, &[]).unwrap();
        assert_eq!(my.sql, "SELECT * FROM `sample`.`books` WHERE `name` = ?");
        assert_eq!(my.params, vec![Value::text("Ada")]);

        let sl = build_table_select(Dialect::Sqlite, "main", "books", &filters, &[]).unwrap();
        assert_eq!(
            sl.sql,
            "SELECT * FROM \"main\".\"books\" WHERE \"name\" = ?"
        );
    }

    #[test]
    fn like_escaping_and_ilike_on_postgres() {
        let filters = [FilterCondition::new("title", FilterOp::Contains, r"a%_b\c")];
        let pg = build_table_select(Dialect::Postgres, "public", "t", &filters, &[]).unwrap();
        assert!(pg.sql.contains("ILIKE $1 ESCAPE '\\'"));
        assert_eq!(pg.params[0], Value::text(r"%a\%\_b\\c%"));

        let my = build_table_select(Dialect::Mysql, "db", "t", &filters, &[]).unwrap();
        assert!(my.sql.contains("LIKE ? ESCAPE '\\'"));
        assert_eq!(my.params[0], Value::text(r"%a\%\_b\\c%"));

        let starts = [FilterCondition::new("title", FilterOp::StartsWith, "Hi")];
        let pg = build_table_select(Dialect::Postgres, "public", "t", &starts, &[]).unwrap();
        assert_eq!(pg.params[0], Value::text("Hi%"));

        let ends = [FilterCondition::new("title", FilterOp::EndsWith, "bye")];
        let pg = build_table_select(Dialect::Postgres, "public", "t", &ends, &[]).unwrap();
        assert_eq!(pg.params[0], Value::text("%bye"));

        let not = [FilterCondition::new("title", FilterOp::NotContains, "x")];
        let pg = build_table_select(Dialect::Postgres, "public", "t", &not, &[]).unwrap();
        assert!(pg.sql.contains("NOT ILIKE"));
    }

    #[test]
    fn null_ops_have_no_params() {
        let filters = [
            FilterCondition::new("name", FilterOp::IsNull, ""),
            FilterCondition::new("rating", FilterOp::IsNotNull, "ignored"),
        ];
        let pg = build_table_select(Dialect::Postgres, "public", "t", &filters, &[]).unwrap();
        assert_eq!(
            pg.sql,
            "SELECT * FROM \"public\".\"t\" WHERE \"name\" IS NULL AND \"rating\" IS NOT NULL"
        );
        assert!(pg.params.is_empty());
    }

    #[test]
    fn in_list_binds_each_item_in_order() {
        let filters = [FilterCondition::new("id", FilterOp::In, "1, 2, 3").with_type("integer")];
        let pg = build_table_select(Dialect::Postgres, "public", "t", &filters, &[]).unwrap();
        assert_eq!(
            pg.sql,
            "SELECT * FROM \"public\".\"t\" WHERE \"id\" IN ($1, $2, $3)"
        );
        assert_eq!(pg.params, vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
    }

    #[test]
    fn sort_and_page_append_order_limit() {
        let sorts = [SortSpec::desc("id"), SortSpec::asc("name")];
        let page =
            build_table_page(Dialect::Postgres, "public", "books", &[], &sorts, 100, 200).unwrap();
        assert_eq!(
            page.sql,
            "SELECT * FROM \"public\".\"books\" ORDER BY \"id\" DESC, \"name\" ASC LIMIT 100 OFFSET 200"
        );
        assert!(page.params.is_empty());
    }

    #[test]
    fn injection_attempt_is_a_bound_literal() {
        let evil = "1; DROP TABLE books;--";
        let filters = [FilterCondition::new("name", FilterOp::Eq, evil)];
        let pg = build_table_select(Dialect::Postgres, "public", "books", &filters, &[]).unwrap();
        assert!(!pg.sql.contains("DROP"));
        assert!(pg.sql.ends_with("= $1"));
        assert_eq!(pg.params, vec![Value::text(evil)]);
    }

    #[test]
    fn type_aware_parsing() {
        assert_eq!(parse_filter_value("42", Some("integer")), Value::Int(42));
        assert_eq!(
            parse_filter_value("true", Some("boolean")),
            Value::Bool(true)
        );
        assert_eq!(
            parse_filter_value("1815-12-10", Some("date")),
            Value::Date(NaiveDate::from_ymd_opt(1815, 12, 10).unwrap())
        );
        let u = "550e8400-e29b-41d4-a716-446655440000";
        assert_eq!(
            parse_filter_value(u, Some("uuid")),
            Value::Uuid(Uuid::parse_str(u).unwrap())
        );
        // Unknown / failed parse → text
        assert_eq!(
            parse_filter_value("not-a-number", Some("integer")),
            Value::text("not-a-number")
        );
    }

    #[test]
    fn count_reuses_where_params() {
        let filters = [FilterCondition::new("name", FilterOp::Eq, "Ada")];
        let count = build_table_count(Dialect::Mysql, "sample", "books", &filters).unwrap();
        assert_eq!(
            count.sql,
            "SELECT COUNT(*) AS sela_count FROM `sample`.`books` WHERE `name` = ?"
        );
        assert_eq!(count.params, vec![Value::text("Ada")]);
    }

    #[test]
    fn quotes_weird_identifiers() {
        let filters = [FilterCondition::new(r#"a"b"#, FilterOp::Eq, "x")];
        let pg = build_table_select(Dialect::Postgres, "public", "t", &filters, &[]).unwrap();
        assert!(pg.sql.contains(r#""a""b""#));
    }

    #[test]
    fn and_combines_multiple_conditions() {
        let filters = [
            FilterCondition::new("a", FilterOp::Eq, "1").with_type("int"),
            FilterCondition::new("b", FilterOp::Contains, "%"),
        ];
        let pg = build_table_select(Dialect::Postgres, "public", "t", &filters, &[]).unwrap();
        assert!(pg.sql.contains(" AND "));
        assert_eq!(pg.params.len(), 2);
        assert_eq!(pg.params[0], Value::Int(1));
        assert_eq!(pg.params[1], Value::text(r"%\%%"));
    }
}
