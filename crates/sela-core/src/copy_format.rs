//! Format a rectangular grid selection for the clipboard (US-14).

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

/// Escape a SQL string literal (`'` → `''`). `NULL` stays the keyword.
pub fn escape_sql_literal(value: &str) -> String {
    if value == "NULL" {
        return "NULL".into();
    }
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for ch in value.chars() {
        if ch == '\'' {
            out.push('\'');
        }
        out.push(ch);
    }
    out.push('\'');
    out
}

/// Format a rectangular block of display strings.
///
/// `headers` must cover every column index that appears in `rows`. For TSV/CSV
/// the header row is always written first. JSON uses headers as object keys.
/// INSERT uses `table_name` (qualified or bare) and dialect quoting.
pub fn format_selection(
    headers: &[String],
    rows: &[Vec<String>],
    format: CopyFormat,
    table_name: Option<&str>,
    dialect: Dialect,
) -> Result<String, SqlError> {
    match format {
        CopyFormat::Tsv => Ok(format_delimited(headers, rows, '\t', false)),
        CopyFormat::Csv => Ok(format_delimited(headers, rows, ',', true)),
        CopyFormat::Json => Ok(format_json(headers, rows)),
        CopyFormat::Insert => format_insert(headers, rows, table_name, dialect),
    }
}

fn format_delimited(headers: &[String], rows: &[Vec<String>], sep: char, csv: bool) -> String {
    let mut out = String::new();
    write_line(&mut out, headers, sep, csv);
    for row in rows {
        write_line(&mut out, row, sep, csv);
    }
    out
}

fn write_line(out: &mut String, fields: &[String], sep: char, csv: bool) {
    for (i, field) in fields.iter().enumerate() {
        if i > 0 {
            out.push(sep);
        }
        if csv {
            out.push_str(&escape_csv_field(field));
        } else {
            // TSV: keep tabs/newlines as-is so paste into spreadsheets matches
            // the raw cell text; callers already use Value::display().
            out.push_str(field);
        }
    }
    out.push('\n');
}

fn format_json(headers: &[String], rows: &[Vec<String>]) -> String {
    let mut arr = Vec::with_capacity(rows.len());
    for row in rows {
        let mut obj = serde_json::Map::new();
        for (i, key) in headers.iter().enumerate() {
            let raw = row.get(i).map(|s| s.as_str()).unwrap_or("NULL");
            let value = if raw == "NULL" {
                serde_json::Value::Null
            } else {
                serde_json::Value::String(raw.to_string())
            };
            obj.insert(key.clone(), value);
        }
        arr.push(serde_json::Value::Object(obj));
    }
    serde_json::to_string_pretty(&serde_json::Value::Array(arr)).unwrap_or_else(|_| "[]".into())
}

fn format_insert(
    headers: &[String],
    rows: &[Vec<String>],
    table_name: Option<&str>,
    dialect: Dialect,
) -> Result<String, SqlError> {
    let table = table_name.unwrap_or("table");
    let quoted_table = quote_table_ref(dialect, table)?;
    let cols: Result<Vec<_>, _> = headers.iter().map(|h| quote_ident(dialect, h)).collect();
    let cols = cols?;
    let mut out = String::new();
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
            let raw = row.get(ci).map(|s| s.as_str()).unwrap_or("NULL");
            out.push_str(&escape_sql_literal(raw));
        }
        out.push(')');
    }
    out.push_str(";\n");
    Ok(out)
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

/// Slice headers and cell display strings for a selection rectangle.
pub fn extract_selection_matrix(
    columns: &[String],
    rows: &[Vec<String>],
    rect: SelectionRect,
) -> Option<(Vec<String>, Vec<Vec<String>>)> {
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
            cells.push(row.get(col).cloned().unwrap_or_else(|| "NULL".into()));
        }
        out_rows.push(cells);
    }
    Some((headers, out_rows))
}

/// Display string for a typed value used in copy (never truncated).
pub fn value_copy_text(value: &Value) -> String {
    value.display().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selection::CellPos;

    #[test]
    fn csv_escaping() {
        assert_eq!(escape_csv_field("plain"), "plain");
        assert_eq!(escape_csv_field("a,b"), "\"a,b\"");
        assert_eq!(escape_csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(escape_csv_field("a\nb"), "\"a\nb\"");
    }

    #[test]
    fn sql_literal_escaping() {
        assert_eq!(escape_sql_literal("NULL"), "NULL");
        assert_eq!(escape_sql_literal("a'b"), "'a''b'");
        assert_eq!(escape_sql_literal("ok"), "'ok'");
    }

    #[test]
    fn tsv_and_csv_round_trip_shape() {
        let headers = vec!["id".into(), "name".into()];
        let rows = vec![
            vec!["1".into(), "Ada".into()],
            vec!["2".into(), "a,b".into()],
        ];
        let tsv =
            format_selection(&headers, &rows, CopyFormat::Tsv, None, Dialect::Sqlite).unwrap();
        assert_eq!(tsv, "id\tname\n1\tAda\n2\ta,b\n");

        let csv =
            format_selection(&headers, &rows, CopyFormat::Csv, None, Dialect::Sqlite).unwrap();
        assert_eq!(csv, "id,name\n1,Ada\n2,\"a,b\"\n");
    }

    #[test]
    fn json_null_and_strings() {
        let headers = vec!["id".into(), "note".into()];
        let rows = vec![vec!["1".into(), "NULL".into()]];
        let json =
            format_selection(&headers, &rows, CopyFormat::Json, None, Dialect::Postgres).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed[0]["id"], "1");
        assert!(parsed[0]["note"].is_null());
    }

    #[test]
    fn insert_quotes_identifiers_and_literals() {
        let headers = vec!["id".into(), "name".into()];
        let rows = vec![vec!["1".into(), "O'Hara".into()]];
        let sql = format_selection(
            &headers,
            &rows,
            CopyFormat::Insert,
            Some("public.users"),
            Dialect::Postgres,
        )
        .unwrap();
        assert!(sql.starts_with("INSERT INTO \"public\".\"users\" (\"id\", \"name\") VALUES\n"));
        assert!(sql.contains("('1', 'O''Hara')"));
        assert!(sql.ends_with(";\n"));
    }

    #[test]
    fn extract_matrix_slices_rectangle() {
        let columns = vec!["a".into(), "b".into(), "c".into()];
        let rows = vec![
            vec!["1".into(), "2".into(), "3".into()],
            vec!["4".into(), "5".into(), "6".into()],
            vec!["7".into(), "8".into(), "9".into()],
        ];
        let rect = SelectionRect::from_corners(CellPos::new(0, 1), CellPos::new(1, 2));
        let (headers, body) = extract_selection_matrix(&columns, &rows, rect).unwrap();
        assert_eq!(headers, vec!["b", "c"]);
        assert_eq!(body, vec![vec!["2", "3"], vec!["5", "6"]]);
    }
}
