//! Table filter / sort SQL builder (M1-3).
//!
//! Builds `SELECT * FROM … WHERE … ORDER BY …` with dialect-quoted identifiers
//! and **bound parameters only** — filter values are never string-interpolated
//! into SQL. LIKE wildcards in user values are escaped.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::column_kind::ColumnKind;
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
    /// SQL `LIKE` / Postgres `ILIKE` with user-supplied wildcards (`%` / `_`).
    Like,
    /// SQL `NOT LIKE` / Postgres `NOT ILIKE`.
    NotLike,
    /// Convenience: wraps the value in `%…%` and escapes wildcards (kept for
    /// older `tabs.json`; not offered in the M1-3 filter menu).
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
            Self::Like => "LIKE",
            Self::NotLike => "NOT LIKE",
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
        FilterOp::Like,
        FilterOp::NotLike,
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

    /// Cycle within operators legal for `kind` (falls back to [`Self::next`] for Other).
    pub fn next_for_kind(self, kind: ColumnKind) -> Self {
        kind.next_op(self)
    }

    pub fn is_pattern(self) -> bool {
        matches!(
            self,
            Self::Like
                | Self::NotLike
                | Self::Contains
                | Self::NotContains
                | Self::StartsWith
                | Self::EndsWith
        )
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

/// Escape character for `LIKE … ESCAPE '…'` clauses.
///
/// Uses `!` (not `\`) so the SQL fragment `ESCAPE '!'` is a well-formed string
/// literal on MySQL whether or not `NO_BACKSLASH_ESCAPES` is set. Backslash as
/// the escape char breaks MySQL's default string parsing (`ESCAPE '\'` leaves
/// an unterminated literal).
pub const LIKE_ESCAPE: char = '!';

/// Escape `!`, `%`, and `_` for [`LIKE_ESCAPE`] patterns (convenience ops that
/// treat the user value as a literal substring).
pub fn escape_like(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            LIKE_ESCAPE | '%' | '_' => {
                out.push(LIKE_ESCAPE);
                out.push(ch);
            }
            other => out.push(other),
        }
    }
    out
}

/// Normalize a UX `LIKE` / `NOT LIKE` pattern for [`LIKE_ESCAPE`].
///
/// `%` and `_` stay wildcards. Valid user escapes (`!%`, `!_`, `!!`) pass through
/// unchanged so the bound value matches the documented pattern. A dangling `!`
/// (trailing, or before an ordinary character) is treated as a literal `!` by
/// emitting `!!`, which keeps the `ESCAPE '!'` clause valid on every engine.
pub fn escape_like_meta(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != LIKE_ESCAPE {
            out.push(ch);
            continue;
        }
        match chars.peek().copied() {
            Some('%') | Some('_') | Some(LIKE_ESCAPE) => {
                out.push(LIKE_ESCAPE);
                out.push(chars.next().expect("peeked"));
            }
            Some(_) | None => {
                // Dangling escape → literal `!`.
                out.push(LIKE_ESCAPE);
                out.push(LIKE_ESCAPE);
            }
        }
    }
    out
}

fn like_escape_sql() -> String {
    format!("ESCAPE '{LIKE_ESCAPE}'")
}

/// Validate one draft filter row before Apply. Returns `Ok(())` or a short UI message.
///
/// Empty text for `=` / `≠` on a text column is valid (binds `''`). Pattern ops
/// require a non-empty value. Never treats the characters `NULL` as SQL NULL on
/// text; on non-text, that literal is rejected with "Use IS NULL".
pub fn validate_filter_condition(filter: &FilterCondition) -> Result<(), String> {
    if filter.column.trim().is_empty() {
        return Err("Enter a column".into());
    }
    let kind = ColumnKind::from_type_name(filter.column_type.as_deref().unwrap_or(""));
    if !kind.allows(filter.op) {
        return Err(format!(
            "{} is not valid for {}",
            filter.op.label(),
            filter.column_type.as_deref().unwrap_or("this column")
        ));
    }
    if matches!(filter.op, FilterOp::IsNull | FilterOp::IsNotNull) {
        return Ok(());
    }
    if filter.op.is_pattern() {
        if filter.value.is_empty() {
            return Err("Enter a pattern".into());
        }
        return Ok(());
    }
    if filter.op == FilterOp::In {
        let items = split_in_list(&filter.value);
        if items.is_empty() {
            return Err("IN needs a value".into());
        }
        if items.len() > 100 {
            return Err("At most 100 values".into());
        }
        for (i, item) in items.iter().enumerate() {
            if item.is_empty() {
                return Err(format!("Item {} is empty", i + 1));
            }
            validate_parsed_value(item, kind, i + 1)?;
        }
        return Ok(());
    }
    // = / != / comparisons
    if filter.value.is_empty() {
        return match kind {
            ColumnKind::Text | ColumnKind::Other => Ok(()),
            _ => Err("Enter a value".into()),
        };
    }
    if filter.value.trim().eq_ignore_ascii_case("null") && !matches!(kind, ColumnKind::Text) {
        return Err("Use IS NULL".into());
    }
    validate_parsed_value(&filter.value, kind, 0)
}

fn validate_parsed_value(raw: &str, kind: ColumnKind, item: usize) -> Result<(), String> {
    let prefix = if item > 0 {
        format!("Item {item} is not ")
    } else {
        String::new()
    };
    match kind {
        ColumnKind::Bool => {
            if parse_bool(raw).is_none() {
                return Err(if item > 0 {
                    format!("{prefix}a boolean")
                } else {
                    "Not a boolean".into()
                });
            }
        }
        ColumnKind::Int => {
            if raw.trim().parse::<i64>().is_err() {
                return Err(if item > 0 {
                    format!("{prefix}an integer")
                } else {
                    "Not an integer".into()
                });
            }
        }
        ColumnKind::Float | ColumnKind::Decimal => {
            if raw
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|n| n.is_finite())
                .is_none()
            {
                return Err(if item > 0 {
                    format!("{prefix}a number")
                } else {
                    "Not a number".into()
                });
            }
        }
        ColumnKind::Date => {
            if NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d").is_err() {
                return Err("Not a date (YYYY-MM-DD)".into());
            }
        }
        ColumnKind::Uuid => {
            if Uuid::parse_str(raw.trim()).is_err() {
                return Err(if item > 0 {
                    format!("{prefix}a UUID")
                } else {
                    "Not a UUID".into()
                });
            }
        }
        ColumnKind::Json => {
            if serde_json::from_str::<serde_json::Value>(raw.trim()).is_err() {
                return Err("Not valid JSON".into());
            }
        }
        ColumnKind::Text
        | ColumnKind::Time
        | ColumnKind::Timestamp
        | ColumnKind::Interval
        | ColumnKind::Bytes
        | ColumnKind::Other => {}
    }
    Ok(())
}

/// Parse a filter value using an optional column type hint.
///
/// Tries int / float / bool / date / uuid when the type name suggests it;
/// otherwise (or on parse failure) returns [`Value::Text`]. Never returns
/// [`Value::Null`] — use `IS NULL` for null checks.
pub fn parse_filter_value(raw: &str, type_name: Option<&str>) -> Value {
    let kind = ColumnKind::from_type_name(type_name.unwrap_or(""));
    match kind {
        ColumnKind::Bool => {
            if let Some(b) = parse_bool(raw) {
                return Value::Bool(b);
            }
        }
        ColumnKind::Int => {
            if let Ok(n) = raw.trim().parse::<i64>() {
                return Value::Int(n);
            }
        }
        ColumnKind::Float | ColumnKind::Decimal => {
            if let Ok(n) = raw.trim().parse::<f64>() {
                return Value::Float(n);
            }
        }
        ColumnKind::Uuid => {
            if let Ok(u) = Uuid::parse_str(raw.trim()) {
                return Value::Uuid(u);
            }
        }
        ColumnKind::Date => {
            if let Ok(d) = NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d") {
                return Value::Date(d);
            }
        }
        ColumnKind::Json => {
            // Bind as text; the engine validates JSON.
            return Value::text(raw);
        }
        ColumnKind::Text
        | ColumnKind::Time
        | ColumnKind::Timestamp
        | ColumnKind::Interval
        | ColumnKind::Bytes
        | ColumnKind::Other => {}
    }
    // Untyped / failed parse: prefer int when unambiguous and not clearly text.
    if matches!(kind, ColumnKind::Other) {
        if let Some(b) = parse_bool(raw) {
            return Value::Bool(b);
        }
        if let Ok(n) = raw.trim().parse::<i64>() {
            return Value::Int(n);
        }
    }
    Value::text(raw)
}

fn parse_bool(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" | "t" | "yes" | "y" | "1" => Some(true),
        "false" | "f" | "no" | "n" | "0" => Some(false),
        _ => None,
    }
}

/// Labels offered by the bool filter value menu (index 0 = true, 1 = false).
pub const BOOL_FILTER_VALUES: &[&str] = &["true", "false"];

/// Map a stored / typed bool filter string to a menu index (`0` = true, `1` = false).
///
/// Accepts the same aliases as Apply validation (`t`/`1`, `f`/`0`, …). Empty or
/// unrecognized input returns `None` (the menu stays unselected).
pub fn bool_filter_menu_index(raw: &str) -> Option<usize> {
    match parse_bool(raw) {
        Some(true) => Some(0),
        Some(false) => Some(1),
        None => None,
    }
}

/// Canonical menu label (`"true"` / `"false"`) for a bool filter string.
pub fn bool_filter_menu_value(raw: &str) -> Option<&'static str> {
    bool_filter_menu_index(raw).map(|ix| BOOL_FILTER_VALUES[ix])
}

/// Draft filter row used for keyboard insert/remove logic (no GPUI).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterDraft {
    pub column: String,
    pub op: FilterOp,
    pub value: String,
}

impl FilterDraft {
    pub fn empty() -> Self {
        Self {
            column: String::new(),
            op: FilterOp::Eq,
            value: String::new(),
        }
    }

    /// True when the row has no column name and no value (operator ignored).
    pub fn is_blank(&self) -> bool {
        self.column.trim().is_empty() && self.value.is_empty()
    }
}

/// Insert an empty draft row after `after`. Returns the new row's index.
///
/// When `rows` is empty, inserts at 0. `after` past the last index appends.
pub fn insert_filter_draft_after(rows: &mut Vec<FilterDraft>, after: usize) -> usize {
    let at = if rows.is_empty() {
        0
    } else {
        after.saturating_add(1).min(rows.len())
    };
    rows.insert(at, FilterDraft::empty());
    at
}

/// Remove the draft at `index`, or clear the sole row in place (UX §10).
///
/// Removing the only row leaves one empty draft; it does not leave `rows` empty.
pub fn remove_filter_draft(rows: &mut Vec<FilterDraft>, index: usize) {
    if rows.len() <= 1 {
        rows.clear();
        rows.push(FilterDraft::empty());
        return;
    }
    if index < rows.len() {
        rows.remove(index);
    }
}

/// True when every draft row is blank (used by Escape to hide an unused bar).
pub fn filter_drafts_are_empty(rows: &[FilterDraft]) -> bool {
    rows.is_empty() || rows.iter().all(FilterDraft::is_blank)
}

/// Whether `secondary-backspace` should remove (or clear) the focused filter row.
///
/// `focused_text_input` is `Some(contents)` when focus is in a column or value
/// text field, or `None` when focus is on the row itself / the bool Select /
/// another non-text control.
///
/// On macOS, ⌘⌫ means delete-to-line-start inside a text field. Return `false`
/// when that field is non-empty so the input can handle the key. Return `true`
/// when the focused text field is empty, or when focus is not in a text field.
pub fn should_remove_filter_row_on_backspace(focused_text_input: Option<&str>) -> bool {
    match focused_text_input {
        None => true,
        Some(text) => text.is_empty(),
    }
}

/// Human-readable `WHERE … ORDER BY …` fragment for the status strip (placeholders only).
pub fn format_predicate_preview(
    dialect: Dialect,
    schema: &str,
    table: &str,
    filters: &[FilterCondition],
    sorts: &[SortSpec],
) -> String {
    match build_table_select(dialect, schema, table, filters, sorts) {
        Ok(bound) => {
            let sql = bound.sql;
            // Drop `SELECT * FROM …` — keep WHERE / ORDER BY only.
            if let Some(ix) = sql.find(" WHERE ") {
                sql[ix + 1..].trim().to_string()
            } else if let Some(ix) = sql.find(" ORDER BY ") {
                sql[ix + 1..].trim().to_string()
            } else {
                String::new()
            }
        }
        Err(err) => err.to_string(),
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
                // Empty `=` / `≠` binds `''` (valid on text). Patterns require a
                // non-empty value (validated before Apply). Empty IN is dropped below.
                !f.value.is_empty() || matches!(f.op, FilterOp::Eq | FilterOp::Ne | FilterOp::In)
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
        FilterOp::Like => {
            let ph = placeholder(dialect, params.len());
            let esc = like_escape_sql();
            // UX asks for LIKE (not ILIKE). `%` / `_` stay wildcards.
            sql.push_str(&format!("{col} LIKE {ph} {esc}"));
            params.push(Value::text(escape_like_meta(&filter.value)));
        }
        FilterOp::NotLike => {
            let ph = placeholder(dialect, params.len());
            let esc = like_escape_sql();
            sql.push_str(&format!("{col} NOT LIKE {ph} {esc}"));
            params.push(Value::text(escape_like_meta(&filter.value)));
        }
        FilterOp::Contains => {
            let ph = placeholder(dialect, params.len());
            let op = like_op(dialect, false);
            let esc = like_escape_sql();
            sql.push_str(&format!("{col} {op} {ph} {esc}"));
            let pattern = format!("%{}%", escape_like(&filter.value));
            params.push(Value::text(pattern));
        }
        FilterOp::NotContains => {
            let ph = placeholder(dialect, params.len());
            let op = like_op(dialect, true);
            let esc = like_escape_sql();
            sql.push_str(&format!("{col} {op} {ph} {esc}"));
            let pattern = format!("%{}%", escape_like(&filter.value));
            params.push(Value::text(pattern));
        }
        FilterOp::StartsWith => {
            let ph = placeholder(dialect, params.len());
            let op = like_op(dialect, false);
            let esc = like_escape_sql();
            sql.push_str(&format!("{col} {op} {ph} {esc}"));
            let pattern = format!("{}%", escape_like(&filter.value));
            params.push(Value::text(pattern));
        }
        FilterOp::EndsWith => {
            let ph = placeholder(dialect, params.len());
            let op = like_op(dialect, false);
            let esc = like_escape_sql();
            sql.push_str(&format!("{col} {op} {ph} {esc}"));
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
        // `%`, `_`, `!` (escape char), and `\` must all be literal in the pattern.
        let filters = [FilterCondition::new(
            "title",
            FilterOp::Contains,
            r"a%_b!c\d",
        )];
        let pg = build_table_select(Dialect::Postgres, "public", "t", &filters, &[]).unwrap();
        assert!(pg.sql.contains("ILIKE $1 ESCAPE '!'"));
        assert_eq!(pg.params[0], Value::text(r"%a!%!_b!!c\d%"));

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
    fn mysql_like_uses_bang_escape_not_backslash() {
        // MySQL string literals treat `\` as an escape by default, so `ESCAPE '\'`
        // is unterminated SQL. Always emit `ESCAPE '!'` and escape values with `!`.
        let filters = [FilterCondition::new(
            "name",
            FilterOp::Contains,
            r"a%_b!c\d",
        )];
        let my = build_table_select(Dialect::Mysql, "sample", "t", &filters, &[]).unwrap();
        assert_eq!(
            my.sql,
            "SELECT * FROM `sample`.`t` WHERE `name` LIKE ? ESCAPE '!'"
        );
        assert_eq!(my.params, vec![Value::text(r"%a!%!_b!!c\d%")]);
        assert!(
            !my.sql.contains(r"ESCAPE '\'"),
            "backslash ESCAPE breaks MySQL string parsing"
        );

        let starts = [FilterCondition::new("name", FilterOp::StartsWith, "%")];
        let my = build_table_select(Dialect::Mysql, "sample", "t", &starts, &[]).unwrap();
        assert!(my.sql.ends_with("ESCAPE '!'"));
        assert_eq!(my.params[0], Value::text("!%%"));

        let sl = build_table_select(Dialect::Sqlite, "main", "t", &filters, &[]).unwrap();
        assert!(sl.sql.contains("LIKE ? ESCAPE '!'"));
        assert_eq!(sl.params[0], Value::text(r"%a!%!_b!!c\d%"));
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
        assert_eq!(pg.params[1], Value::text(r"%!%%"));
    }

    #[test]
    fn validate_empty_text_eq_and_reject_null_literal_on_int() {
        let text_eq = FilterCondition::new("title", FilterOp::Eq, "").with_type("text");
        assert!(validate_filter_condition(&text_eq).is_ok());

        let int_null = FilterCondition::new("id", FilterOp::Eq, "NULL").with_type("int4");
        assert_eq!(
            validate_filter_condition(&int_null).unwrap_err(),
            "Use IS NULL"
        );

        let like_empty = FilterCondition::new("title", FilterOp::Like, "").with_type("varchar");
        assert_eq!(
            validate_filter_condition(&like_empty).unwrap_err(),
            "Enter a pattern"
        );

        let like_on_int = FilterCondition::new("id", FilterOp::Like, "%").with_type("integer");
        assert!(validate_filter_condition(&like_on_int).is_err());

        // Empty text `=` binds `''` and must appear in WHERE.
        let empty_eq = [FilterCondition::new("title", FilterOp::Eq, "").with_type("text")];
        let pg = build_table_select(Dialect::Postgres, "public", "t", &empty_eq, &[]).unwrap();
        assert_eq!(
            pg.sql,
            "SELECT * FROM \"public\".\"t\" WHERE \"title\" = $1"
        );
        assert_eq!(pg.params, vec![Value::text("")]);

        // LIKE keeps `%` / `_` as wildcards; user escapes pass through.
        let like = [FilterCondition::new("title", FilterOp::Like, r"100!%")];
        let pg = build_table_select(Dialect::Postgres, "public", "t", &like, &[]).unwrap();
        assert!(pg.sql.contains("LIKE $1 ESCAPE '!'"));
        assert!(!pg.sql.contains("ILIKE"));
        assert_eq!(pg.params[0], Value::text(r"100!%"));

        let not_like = [FilterCondition::new("title", FilterOp::NotLike, r"a!_b")];
        let pg = build_table_select(Dialect::Postgres, "public", "t", &not_like, &[]).unwrap();
        assert!(pg.sql.contains("NOT LIKE $1 ESCAPE '!'"));
        assert!(!pg.sql.contains("ILIKE"));
        assert_eq!(pg.params[0], Value::text(r"a!_b"));
    }

    #[test]
    fn escape_like_meta_passes_user_escapes_and_normalizes_dangling() {
        assert_eq!(escape_like_meta("100!%"), "100!%");
        assert_eq!(escape_like_meta("a!_b"), "a!_b");
        assert_eq!(escape_like_meta("wow!!"), "wow!!");
        assert_eq!(escape_like_meta("%"), "%");
        assert_eq!(escape_like_meta("_"), "_");
        // Dangling `!` before an ordinary character → literal `!`.
        assert_eq!(escape_like_meta("a%!b"), "a%!!b");
        assert_eq!(escape_like_meta("!x"), "!!x");
        // Trailing dangling `!`.
        assert_eq!(escape_like_meta("hello!"), "hello!!");
        assert_eq!(escape_like_meta("!"), "!!");
        // Legacy contains/starts/ends still fully escape literals.
        assert_eq!(escape_like("a%!b"), "a!%!!b");
        assert_eq!(escape_like("100%"), "100!%");
    }

    #[test]
    fn predicate_preview_omits_select() {
        let filters = [FilterCondition::new("name", FilterOp::Eq, "Ada")];
        let sorts = [SortSpec::asc("id")];
        let preview = format_predicate_preview(Dialect::Postgres, "public", "t", &filters, &sorts);
        assert!(preview.starts_with("WHERE "));
        assert!(preview.contains("ORDER BY"));
        assert!(!preview.contains("SELECT"));
    }

    #[test]
    fn bool_menu_maps_aliases_to_true_false() {
        assert_eq!(bool_filter_menu_index("true"), Some(0));
        assert_eq!(bool_filter_menu_index("TRUE"), Some(0));
        assert_eq!(bool_filter_menu_index("t"), Some(0));
        assert_eq!(bool_filter_menu_index("1"), Some(0));
        assert_eq!(bool_filter_menu_index("false"), Some(1));
        assert_eq!(bool_filter_menu_index("f"), Some(1));
        assert_eq!(bool_filter_menu_index("0"), Some(1));
        assert_eq!(bool_filter_menu_index(""), None);
        assert_eq!(bool_filter_menu_index("maybe"), None);
        assert_eq!(bool_filter_menu_value("yes"), Some("true"));
        assert_eq!(bool_filter_menu_value("no"), Some("false"));
        assert_eq!(BOOL_FILTER_VALUES, &["true", "false"]);

        let ok = FilterCondition::new("flag", FilterOp::Eq, "true").with_type("bool");
        assert!(validate_filter_condition(&ok).is_ok());
        let empty = FilterCondition::new("flag", FilterOp::Eq, "").with_type("bool");
        assert_eq!(
            validate_filter_condition(&empty).unwrap_err(),
            "Enter a value"
        );
        let bad = FilterCondition::new("flag", FilterOp::Eq, "maybe").with_type("bool");
        assert_eq!(
            validate_filter_condition(&bad).unwrap_err(),
            "Not a boolean"
        );
        assert_eq!(
            parse_filter_value("false", Some("boolean")),
            Value::Bool(false)
        );
    }

    #[test]
    fn filter_draft_insert_and_remove_match_ux() {
        let mut rows = vec![FilterDraft {
            column: "a".into(),
            op: FilterOp::Eq,
            value: "1".into(),
        }];
        let ix = insert_filter_draft_after(&mut rows, 0);
        assert_eq!(ix, 1);
        assert_eq!(rows.len(), 2);
        assert!(rows[1].is_blank());

        // Remove the blank row; the original stays.
        remove_filter_draft(&mut rows, 1);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].column, "a");

        // Sole row: clear in place.
        remove_filter_draft(&mut rows, 0);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].is_blank());
        assert!(filter_drafts_are_empty(&rows));

        let ix = insert_filter_draft_after(&mut rows, 0);
        assert_eq!(ix, 1);
        rows[0].column = "x".into();
        assert!(!filter_drafts_are_empty(&rows));
    }

    #[test]
    fn secondary_backspace_defers_to_nonempty_text_input() {
        // Focus not in a text field → remove the row.
        assert!(should_remove_filter_row_on_backspace(None));
        // Empty column/value field → remove the row.
        assert!(should_remove_filter_row_on_backspace(Some("")));
        // Non-empty text → leave it for delete-to-line-start (macOS ⌘⌫).
        assert!(!should_remove_filter_row_on_backspace(Some("Ada")));
        assert!(!should_remove_filter_row_on_backspace(Some(" ")));
    }
}
