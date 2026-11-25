//! Format a rectangular grid selection for the clipboard (US-14).
//!
//! Copy works on typed [`CopyCell`] values, not display strings. A text cell
//! whose contents are the four characters `NULL` is never confused with a SQL
//! NULL — only [`CopyCell::Null`] becomes the NULL keyword / JSON null.

use crate::quote_ident;
use crate::selection::SelectionRect;
use crate::{Dialect, SqlError, Value};

/// Clipboard formats offered for a grid selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyFormat {
    /// Tab-separated values (default for `secondary-c`).
    Tsv,
    /// RFC-style CSV with quotes when needed.
    Csv,
    /// JSON array of objects keyed by column name.
    Json,
    /// One `INSERT INTO … VALUES (…);` statement.
    Insert,
}

/// How a non-null scalar should be emitted in INSERT / JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyScalarKind {
    /// Quoted string literal (text, uuid, dates, bytes hex, json text, …).
    Text,
    /// Unquoted numeric literal in INSERT; JSON number when parseable.
    Number,
    /// Unquoted `true` / `false` in INSERT; JSON boolean.
    Bool,
}

/// One cell in a copy selection. Carries type so formatters never treat the
/// display string `"NULL"` as a SQL NULL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyCell {
    Null,
    Value(String, CopyScalarKind),
    /// Decode-error or unsupported-type placeholder (`<TYPE>`, …).
    Decode(String),
    /// Row (or cell) not yet in the result window.
    NotLoaded,
    /// Overlay insert/duplicate column the user has not set. Formats omit it
    /// (INSERT leaves the column out; JSON omits the key; TSV/CSV empty field)
    /// instead of inventing NULL/`''` or refusing as unloaded (#99).
    Unset,
}

/// Why a copy was refused. The UI shows [`CopyError::status_message`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyError {
    /// Selection includes unloaded rows.
    NotLoaded { rows: usize },
    /// INSERT refused because a decode/unsupported cell cannot be re-inserted.
    DecodeInInsert { column: String },
    /// INSERT refused on MySQL/SQLite because a number is NaN or ±Infinity.
    NonFiniteInInsert { column: String },
    /// Identifier quoting failed.
    Ident(String),
}

impl CopyError {
    pub fn status_message(&self) -> String {
        match self {
            Self::NotLoaded { rows } => {
                format!("{rows} rows not loaded yet; scroll or narrow the selection")
            }
            Self::DecodeInInsert { column } => {
                format!(
                    "Cannot copy INSERT: column `{column}` has a decode error or unsupported type"
                )
            }
            Self::NonFiniteInInsert { column } => {
                format!(
                    "Cannot copy as INSERT: column \"{column}\" has a non-finite number (NaN/Infinity)"
                )
            }
            Self::Ident(msg) => msg.clone(),
        }
    }
}

impl From<SqlError> for CopyError {
    fn from(err: SqlError) -> Self {
        Self::Ident(err.to_string())
    }
}

/// Escape a single CSV field (RFC 4180-ish).
pub fn escape_csv_field(value: &str) -> String {
    let needs_quotes = value.contains(',')
        || value.contains('"')
        || value.contains('\n')
        || value.contains('\r')
        || value.starts_with(' ')
        || value.ends_with(' ');
    if !needs_quotes {
        return value.to_string();
    }
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        if ch == '"' {
            out.push('"');
        }
        out.push(ch);
    }
    out.push('"');
    out
}

/// Escape a SQL string literal. Always produces a quoted string — never the
/// bare keyword `NULL`. On MySQL, backslashes are doubled (`\` → `\\`).
pub fn escape_sql_string(value: &str, dialect: Dialect) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for ch in value.chars() {
        match ch {
            '\'' => {
                out.push('\'');
                out.push('\'');
            }
            '\\' if matches!(dialect, Dialect::Mysql) => {
                out.push('\\');
                out.push('\\');
            }
            _ => out.push(ch),
        }
    }
    out.push('\'');
    out
}

/// Map a typed grid [`Value`] into a [`CopyCell`].
pub fn value_to_copy_cell(value: &Value) -> CopyCell {
    if value.is_null() {
        return CopyCell::Null;
    }
    if value.is_decode_error() || value.is_unsupported_placeholder() {
        return CopyCell::Decode(value.display().into_owned());
    }
    let kind = match value {
        Value::Bool(_) => CopyScalarKind::Bool,
        Value::Int(_) | Value::UInt(_) | Value::Float(_) | Value::Decimal(_) => {
            CopyScalarKind::Number
        }
        _ => CopyScalarKind::Text,
    };
    CopyCell::Value(value.display().into_owned(), kind)
}

/// Format a rectangular block of typed cells.
///
/// For TSV/CSV the header row is always written first. JSON uses headers as
/// object keys. INSERT uses `table_name` when present; when absent it uses the
/// placeholder `table` and appends `-- replace table name`.
pub fn format_selection(
    headers: &[String],
    rows: &[Vec<CopyCell>],
    format: CopyFormat,
    table_name: Option<&str>,
    dialect: Dialect,
) -> Result<String, CopyError> {
    let not_loaded = count_not_loaded_rows(rows);
    if not_loaded > 0 {
        return Err(CopyError::NotLoaded { rows: not_loaded });
    }
    match format {
        CopyFormat::Tsv => Ok(format_delimited(headers, rows, '\t', false)),
        CopyFormat::Csv => Ok(format_delimited(headers, rows, ',', true)),
        CopyFormat::Json => Ok(format_json(headers, rows)),
        CopyFormat::Insert => format_insert(headers, rows, table_name, dialect),
    }
}

fn count_not_loaded_rows(rows: &[Vec<CopyCell>]) -> usize {
    rows.iter()
        .filter(|row| row.iter().any(|c| matches!(c, CopyCell::NotLoaded)))
        .count()
}

fn format_delimited(headers: &[String], rows: &[Vec<CopyCell>], sep: char, csv: bool) -> String {
    let mut out = String::new();
    write_text_line(&mut out, headers.iter().map(|s| s.as_str()), sep, csv);
    for row in rows {
        let fields: Vec<&str> = row.iter().map(cell_tsv_csv_text).collect();
        write_text_line(&mut out, fields.into_iter(), sep, csv);
    }
    out
}

fn cell_tsv_csv_text(cell: &CopyCell) -> &str {
    match cell {
        CopyCell::Null => "NULL",
        CopyCell::Value(text, _) | CopyCell::Decode(text) => text.as_str(),
        // Callers refuse NotLoaded before reaching here. Unset → empty field.
        CopyCell::NotLoaded | CopyCell::Unset => "",
    }
}

fn write_text_line<'a>(
    out: &mut String,
    fields: impl Iterator<Item = &'a str>,
    sep: char,
    csv: bool,
) {
    for (i, field) in fields.enumerate() {
        if i > 0 {
            out.push(sep);
        }
        if csv {
            out.push_str(&escape_csv_field(field));
        } else {
            out.push_str(field);
        }
    }
    out.push('\n');
}

fn format_json(headers: &[String], rows: &[Vec<CopyCell>]) -> String {
    let mut arr = Vec::with_capacity(rows.len());
    for row in rows {
        let mut obj = serde_json::Map::new();
        for (i, key) in headers.iter().enumerate() {
            let cell = row.get(i).cloned().unwrap_or(CopyCell::Null);
            if let Some(value) = cell_to_json(&cell) {
                obj.insert(key.clone(), value);
            }
        }
        arr.push(serde_json::Value::Object(obj));
    }
    serde_json::to_string_pretty(&serde_json::Value::Array(arr)).unwrap_or_else(|_| "[]".into())
}

fn cell_to_json(cell: &CopyCell) -> Option<serde_json::Value> {
    Some(match cell {
        CopyCell::Null => serde_json::Value::Null,
        CopyCell::Value(text, CopyScalarKind::Bool) => {
            serde_json::Value::Bool(text.eq_ignore_ascii_case("true"))
        }
        CopyCell::Value(text, CopyScalarKind::Number) => {
            parse_json_number(text).unwrap_or_else(|| serde_json::Value::String(text.clone()))
        }
        CopyCell::Value(text, CopyScalarKind::Text) | CopyCell::Decode(text) => {
            serde_json::Value::String(text.clone())
        }
        // Defensive: NotLoaded is refused before format_json.
        CopyCell::NotLoaded => serde_json::Value::Null,
        // Omit unset overlay columns from the object (#99).
        CopyCell::Unset => return None,
    })
}

fn parse_json_number(text: &str) -> Option<serde_json::Value> {
    if let Ok(n) = text.parse::<i64>() {
        return Some(serde_json::Value::Number(n.into()));
    }
    if let Ok(n) = text.parse::<u64>() {
        return Some(serde_json::Value::Number(n.into()));
    }
    let f: f64 = text.parse().ok()?;
    // Non-finite floats are not valid JSON numbers; callers fall back to a JSON string.
    serde_json::Number::from_f64(f).map(serde_json::Value::Number)
}

/// Classify a numeric display string as NaN / ±Infinity when it is not a finite literal.
///
/// Matches Rust `f64` Display (`NaN`, `inf`, `-inf`) and the Postgres spellings
/// (`Infinity`, `-Infinity`).
fn classify_non_finite(text: &str) -> Option<NonFiniteKind> {
    let t = text.trim();
    if t.eq_ignore_ascii_case("nan") {
        return Some(NonFiniteKind::Nan);
    }
    if t.eq_ignore_ascii_case("inf")
        || t.eq_ignore_ascii_case("+inf")
        || t.eq_ignore_ascii_case("infinity")
        || t.eq_ignore_ascii_case("+infinity")
    {
        return Some(NonFiniteKind::PosInf);
    }
    if t.eq_ignore_ascii_case("-inf") || t.eq_ignore_ascii_case("-infinity") {
        return Some(NonFiniteKind::NegInf);
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NonFiniteKind {
    Nan,
    PosInf,
    NegInf,
}

impl NonFiniteKind {
    /// Postgres float4/float8/numeric accept these as quoted string literals.
    fn postgres_sql_literal(self) -> &'static str {
        match self {
            Self::Nan => "'NaN'",
            Self::PosInf => "'Infinity'",
            Self::NegInf => "'-Infinity'",
        }
    }
}

fn format_insert(
    headers: &[String],
    rows: &[Vec<CopyCell>],
    table_name: Option<&str>,
    dialect: Dialect,
) -> Result<String, CopyError> {
    if let Some(column) = first_decode_column(headers, rows) {
        return Err(CopyError::DecodeInInsert { column });
    }
    if matches!(dialect, Dialect::Mysql | Dialect::Sqlite) {
        if let Some(column) = first_non_finite_column(headers, rows) {
            return Err(CopyError::NonFiniteInInsert { column });
        }
    }
    let placeholder = table_name.is_none();
    let table = table_name.unwrap_or("table");
    let quoted_table = quote_table_ref(dialect, table)?;
    // Unset overlay columns are omitted per row (same as preview insert) so a
    // duplicated PK stays out of the statement (#99). When any row omits a
    // column, emit one INSERT per row so column lists can differ.
    let any_unset = rows
        .iter()
        .any(|row| row.iter().any(|c| matches!(c, CopyCell::Unset)));
    let mut out = String::new();
    if any_unset {
        for (ri, row) in rows.iter().enumerate() {
            if ri > 0 {
                out.push('\n');
            }
            out.push_str(&format_insert_one_row(
                headers,
                row,
                &quoted_table,
                dialect,
            )?);
        }
    } else {
        let cols: Result<Vec<_>, _> = headers.iter().map(|h| quote_ident(dialect, h)).collect();
        let cols = cols?;
        out.push_str("INSERT INTO ");
        out.push_str(&quoted_table);
        out.push_str(" (");
        out.push_str(&cols.join(", "));
        out.push_str(") VALUES\n");
        for (ri, row) in rows.iter().enumerate() {
            if ri > 0 {
                out.push_str(",\n");
            }
            out.push('(');
            for (ci, _) in headers.iter().enumerate() {
                if ci > 0 {
                    out.push_str(", ");
                }
                let cell = row.get(ci).cloned().unwrap_or(CopyCell::Null);
                out.push_str(&cell_to_sql_literal(&cell, dialect));
            }
            out.push(')');
        }
        out.push_str(";\n");
    }
    if placeholder {
        out.push_str("-- replace table name\n");
    }
    Ok(out)
}

/// One INSERT omitting [`CopyCell::Unset`] columns.
///
/// All-unset: Postgres/SQLite use `DEFAULT VALUES`; MySQL/MariaDB reject that
/// form and need `() VALUES ()`.
fn format_insert_one_row(
    headers: &[String],
    row: &[CopyCell],
    quoted_table: &str,
    dialect: Dialect,
) -> Result<String, CopyError> {
    let mut cols = Vec::new();
    let mut literals = Vec::new();
    for (ci, name) in headers.iter().enumerate() {
        let cell = row.get(ci).cloned().unwrap_or(CopyCell::Null);
        if matches!(cell, CopyCell::Unset) {
            continue;
        }
        cols.push(quote_ident(dialect, name)?);
        literals.push(cell_to_sql_literal(&cell, dialect));
    }
    let mut out = String::new();
    out.push_str("INSERT INTO ");
    out.push_str(quoted_table);
    if cols.is_empty() {
        match dialect {
            Dialect::Mysql => out.push_str(" () VALUES ();\n"),
            Dialect::Postgres | Dialect::Sqlite => out.push_str(" DEFAULT VALUES;\n"),
        }
        return Ok(out);
    }
    out.push_str(" (");
    out.push_str(&cols.join(", "));
    out.push_str(") VALUES (");
    out.push_str(&literals.join(", "));
    out.push_str(");\n");
    Ok(out)
}

fn first_decode_column(headers: &[String], rows: &[Vec<CopyCell>]) -> Option<String> {
    for row in rows {
        for (ci, cell) in row.iter().enumerate() {
            if matches!(cell, CopyCell::Decode(_)) {
                return Some(column_name(headers, ci));
            }
        }
    }
    None
}

fn first_non_finite_column(headers: &[String], rows: &[Vec<CopyCell>]) -> Option<String> {
    for row in rows {
        for (ci, cell) in row.iter().enumerate() {
            if let CopyCell::Value(text, CopyScalarKind::Number) = cell {
                if classify_non_finite(text).is_some() {
                    return Some(column_name(headers, ci));
                }
            }
        }
    }
    None
}

fn column_name(headers: &[String], ci: usize) -> String {
    headers
        .get(ci)
        .cloned()
        .unwrap_or_else(|| format!("column {ci}"))
}

fn cell_to_sql_literal(cell: &CopyCell, dialect: Dialect) -> String {
    match cell {
        CopyCell::Null => "NULL".into(),
        CopyCell::Value(text, CopyScalarKind::Number) => {
            if let Some(kind) = classify_non_finite(text) {
                // MySQL/SQLite refuse earlier; Postgres needs quoted specials.
                match dialect {
                    Dialect::Postgres => kind.postgres_sql_literal().into(),
                    Dialect::Mysql | Dialect::Sqlite => "NULL".into(),
                }
            } else {
                text.clone()
            }
        }
        CopyCell::Value(text, CopyScalarKind::Bool) => text.clone(),
        CopyCell::Value(text, CopyScalarKind::Text) => escape_sql_string(text, dialect),
        // Refused earlier for INSERT; Unset is omitted by format_insert_one_row.
        CopyCell::Decode(_) | CopyCell::NotLoaded | CopyCell::Unset => "NULL".into(),
    }
}

fn quote_table_ref(dialect: Dialect, table: &str) -> Result<String, SqlError> {
    if let Some((schema, name)) = table.split_once('.') {
        Ok(format!(
            "{}.{}",
            quote_ident(dialect, schema)?,
            quote_ident(dialect, name)?
        ))
    } else {
        quote_ident(dialect, table)
    }
}

/// Slice headers and typed cells for a selection rectangle.
pub fn extract_selection_matrix(
    columns: &[String],
    rows: &[Vec<CopyCell>],
    rect: SelectionRect,
) -> Option<(Vec<String>, Vec<Vec<CopyCell>>)> {
    let (min_row, max_row, min_col, max_col) = rect.normalized();
    if columns.is_empty() || min_col >= columns.len() {
        return None;
    }
    let max_col = max_col.min(columns.len() - 1);
    let headers: Vec<String> = columns[min_col..=max_col].to_vec();
    if rows.is_empty() || min_row >= rows.len() {
        return Some((headers, Vec::new()));
    }
    let max_row = max_row.min(rows.len() - 1);
    let mut out_rows = Vec::with_capacity(max_row - min_row + 1);
    for row in &rows[min_row..=max_row] {
        let mut cells = Vec::with_capacity(max_col - min_col + 1);
        for col in min_col..=max_col {
            cells.push(row.get(col).cloned().unwrap_or(CopyCell::Null));
        }
        out_rows.push(cells);
    }
    Some((headers, out_rows))
}

#[cfg(test)]
#[path = "copy_format_tests.rs"]
mod tests;
