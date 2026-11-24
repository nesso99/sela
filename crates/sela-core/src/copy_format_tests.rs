//! Unit tests for [`super`] copy formatters.

use super::*;
use crate::selection::CellPos;
use std::sync::Arc;

fn text(s: &str) -> CopyCell {
    CopyCell::Value(s.into(), CopyScalarKind::Text)
}

fn number(s: &str) -> CopyCell {
    CopyCell::Value(s.into(), CopyScalarKind::Number)
}

#[test]
fn csv_escaping() {
    assert_eq!(escape_csv_field("plain"), "plain");
    assert_eq!(escape_csv_field("a,b"), "\"a,b\"");
    assert_eq!(escape_csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
    assert_eq!(escape_csv_field("a\nb"), "\"a\nb\"");
}

#[test]
fn sql_string_escaping_never_treats_null_text_as_keyword() {
    assert_eq!(escape_sql_string("NULL", Dialect::Sqlite), "'NULL'");
    assert_eq!(escape_sql_string("a'b", Dialect::Postgres), "'a''b'");
    assert_eq!(escape_sql_string(r"a\b", Dialect::Postgres), r"'a\b'");
    assert_eq!(escape_sql_string(r"a\b", Dialect::Mysql), r"'a\\b'");
}

#[test]
fn text_null_vs_real_null_in_insert_json_csv() {
    let headers = vec!["note".into()];
    let rows = vec![vec![CopyCell::Null], vec![text("NULL")]];

    let insert = format_selection(
        &headers,
        &rows,
        CopyFormat::Insert,
        Some("t"),
        Dialect::Sqlite,
    )
    .unwrap();
    assert!(insert.contains("(NULL)"));
    assert!(insert.contains("('NULL')"));

    let json = format_selection(&headers, &rows, CopyFormat::Json, None, Dialect::Sqlite).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(parsed[0]["note"].is_null());
    assert_eq!(parsed[1]["note"], "NULL");

    let csv = format_selection(&headers, &rows, CopyFormat::Csv, None, Dialect::Sqlite).unwrap();
    assert_eq!(csv, "note\nNULL\nNULL\n");
}

#[test]
fn mysql_backslash_in_insert() {
    let headers = vec!["path".into()];
    let rows = vec![vec![text(r"C:\tmp")]];
    let sql = format_selection(
        &headers,
        &rows,
        CopyFormat::Insert,
        Some("files"),
        Dialect::Mysql,
    )
    .unwrap();
    assert!(sql.contains(r"'C:\\tmp'"));
    assert!(sql.starts_with("INSERT INTO `files` (`path`) VALUES\n"));
}

#[test]
fn decode_cell_refuses_insert() {
    let headers = vec!["id".into(), "geom".into()];
    let rows = vec![vec![number("1"), CopyCell::Decode("<point>".into())]];
    let err = format_selection(
        &headers,
        &rows,
        CopyFormat::Insert,
        Some("t"),
        Dialect::Postgres,
    )
    .unwrap_err();
    assert!(matches!(
        &err,
        CopyError::DecodeInInsert { column } if column == "geom"
    ));
    assert!(err.status_message().contains("`geom`"));
}

#[test]
fn unloaded_row_refuses_any_format() {
    let headers = vec!["id".into()];
    let rows = vec![vec![number("1")], vec![CopyCell::NotLoaded]];
    for format in [
        CopyFormat::Tsv,
        CopyFormat::Csv,
        CopyFormat::Json,
        CopyFormat::Insert,
    ] {
        let err =
            format_selection(&headers, &rows, format, Some("t"), Dialect::Sqlite).unwrap_err();
        assert_eq!(err, CopyError::NotLoaded { rows: 1 });
        assert_eq!(
            err.status_message(),
            "1 rows not loaded yet; scroll or narrow the selection"
        );
    }
}

#[test]
fn insert_placeholder_adds_replace_comment() {
    let headers = vec!["id".into()];
    let rows = vec![vec![number("1")]];
    let sql = format_selection(&headers, &rows, CopyFormat::Insert, None, Dialect::Sqlite).unwrap();
    assert!(sql.starts_with("INSERT INTO \"table\" (\"id\") VALUES\n"));
    assert!(sql.ends_with(";\n-- replace table name\n"));
}

#[test]
fn numbers_and_bools_unquoted_in_insert() {
    let headers = vec!["n".into(), "ok".into()];
    let rows = vec![vec![
        number("42"),
        CopyCell::Value("true".into(), CopyScalarKind::Bool),
    ]];
    let sql = format_selection(
        &headers,
        &rows,
        CopyFormat::Insert,
        Some("t"),
        Dialect::Postgres,
    )
    .unwrap();
    assert!(sql.contains("(42, true)"));
}

#[test]
fn postgres_insert_quotes_non_finite_floats() {
    let headers = vec!["x".into(), "y".into(), "z".into()];
    let rows = vec![vec![number("NaN"), number("inf"), number("-inf")]];
    let sql = format_selection(
        &headers,
        &rows,
        CopyFormat::Insert,
        Some("t"),
        Dialect::Postgres,
    )
    .unwrap();
    assert!(sql.contains("('NaN', 'Infinity', '-Infinity')"));
    // Also accept Postgres spellings as input display text.
    let rows2 = vec![vec![number("Infinity"), number("-Infinity")]];
    let sql2 = format_selection(
        &["a".into(), "b".into()],
        &rows2,
        CopyFormat::Insert,
        Some("t"),
        Dialect::Postgres,
    )
    .unwrap();
    assert!(sql2.contains("('Infinity', '-Infinity')"));
}

#[test]
fn mysql_insert_refuses_non_finite_floats() {
    let headers = vec!["id".into(), "x".into()];
    let rows = vec![vec![number("1"), number("NaN")]];
    let err = format_selection(
        &headers,
        &rows,
        CopyFormat::Insert,
        Some("t"),
        Dialect::Mysql,
    )
    .unwrap_err();
    assert!(matches!(
        &err,
        CopyError::NonFiniteInInsert { column } if column == "x"
    ));
    assert_eq!(
        err.status_message(),
        "Cannot copy as INSERT: column \"x\" has a non-finite number (NaN/Infinity)"
    );
}

#[test]
fn sqlite_insert_refuses_non_finite_floats() {
    let headers = vec!["score".into()];
    let rows = vec![vec![number("Infinity")]];
    let err = format_selection(
        &headers,
        &rows,
        CopyFormat::Insert,
        Some("t"),
        Dialect::Sqlite,
    )
    .unwrap_err();
    assert!(matches!(
        &err,
        CopyError::NonFiniteInInsert { column } if column == "score"
    ));
    assert!(err.status_message().contains("\"score\""));
    assert!(err.status_message().contains("non-finite number"));
}

#[test]
fn json_emits_string_for_non_finite_numbers() {
    // serde_json::Number rejects NaN/±Infinity; we emit a JSON string instead of
    // invalid JSON or null, using the cell's display text (Rust f64 Display).
    let headers = vec!["x".into()];
    for text in ["NaN", "inf", "-inf"] {
        let rows = vec![vec![number(text)]];
        let json =
            format_selection(&headers, &rows, CopyFormat::Json, None, Dialect::Sqlite).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed[0]["x"], text);
        assert!(parsed[0]["x"].is_string());
    }
    // Finite floats stay JSON numbers.
    let rows = vec![vec![number("1.5")]];
    let json = format_selection(&headers, &rows, CopyFormat::Json, None, Dialect::Sqlite).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed[0]["x"], 1.5);
}

#[test]
fn tsv_csv_keep_non_finite_display_text() {
    let headers = vec!["x".into()];
    let rows = vec![vec![number("NaN")], vec![number("inf")]];
    let tsv = format_selection(&headers, &rows, CopyFormat::Tsv, None, Dialect::Sqlite).unwrap();
    assert_eq!(tsv, "x\nNaN\ninf\n");
    let csv = format_selection(&headers, &rows, CopyFormat::Csv, None, Dialect::Sqlite).unwrap();
    assert_eq!(csv, "x\nNaN\ninf\n");
}

#[test]
fn value_to_copy_cell_preserves_text_null() {
    assert_eq!(value_to_copy_cell(&Value::Null), CopyCell::Null);
    assert_eq!(
        value_to_copy_cell(&Value::text("NULL")),
        CopyCell::Value("NULL".into(), CopyScalarKind::Text)
    );
    assert_eq!(
        value_to_copy_cell(&Value::decode_error("TEXT")),
        CopyCell::Decode("<decode error: TEXT>".into())
    );
    assert_eq!(
        value_to_copy_cell(&Value::Other {
            type_name: "point".into(),
            text: Arc::from("<point>"),
        }),
        CopyCell::Decode("<point>".into())
    );
}

#[test]
fn extract_matrix_slices_rectangle() {
    let columns = vec!["a".into(), "b".into(), "c".into()];
    let rows = vec![
        vec![text("1"), text("2"), text("3")],
        vec![text("4"), text("5"), text("6")],
        vec![text("7"), text("8"), text("9")],
    ];
    let rect = SelectionRect::from_corners(CellPos::new(0, 1), CellPos::new(1, 2));
    let (headers, body) = extract_selection_matrix(&columns, &rows, rect).unwrap();
    assert_eq!(headers, vec!["b", "c"]);
    assert_eq!(
        body,
        vec![vec![text("2"), text("3")], vec![text("5"), text("6")]]
    );
}
