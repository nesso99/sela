//! Golden snapshots for M3-3 export (EX-07 / EX-02 / EX-03).
//!
//! Expected files live under `tests/export_snapshots/`. Normalize CRLF so a
//! Windows checkout still matches (see `.gitattributes`).
//!
//! JSON keys are emitted in column order (not `serde_json::Map` iteration order),
//! so workspace `preserve_order` features cannot change the fixtures.

use std::path::PathBuf;

use sela_core::{
    export_rows, BinaryEncoding, CsvExportOptions, Dialect, ExportFormat, JsonExportMode,
    JsonExportOptions, SqlInsertExportOptions, Value, SQL_INSERT_DEFAULT_BATCH_BYTES,
    SQL_INSERT_DEFAULT_BATCH_ROWS,
};
use tokio_util::sync::CancellationToken;

fn snapshot_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/export_snapshots")
}

fn assert_snapshot(name: &str, actual: &str) {
    let path = snapshot_dir().join(name);
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    // Normalize line endings so a CRLF checkout (e.g. Windows) still matches.
    let expected = expected.replace("\r\n", "\n");
    let actual = actual.replace("\r\n", "\n");
    assert_eq!(
        actual.trim_end(),
        expected.trim_end(),
        "snapshot mismatch for {}\n--- actual ---\n{actual}\n--- expected ---\n{expected}",
        path.display()
    );
}

fn fixture_columns() -> Vec<String> {
    vec![
        "id".into(),
        "title".into(),
        "note".into(),
        "blob".into(),
        "flag".into(),
    ]
}

fn fixture_rows() -> Vec<Vec<Value>> {
    vec![
        vec![
            Value::Int(1),
            Value::text("a,b"),
            Value::Null,
            Value::Bytes(vec![0xde, 0xad, 0xbe, 0xef]),
            Value::Bool(true),
        ],
        vec![
            Value::Int(2),
            Value::text("say \"hi\"\nthere"),
            Value::text(""),
            Value::Bytes(vec![]),
            Value::Bool(false),
        ],
        vec![
            Value::Int(3),
            Value::text("café"),
            Value::text("NULL"),
            Value::Bytes(vec![0x00]),
            Value::Null,
        ],
        vec![
            Value::Int(4),
            Value::text("cr\ronly"),
            Value::text("plain"),
            Value::Bytes(vec![0xff]),
            Value::Bool(true),
        ],
    ]
}

fn export_to_string(format: ExportFormat) -> String {
    let mut out = Vec::new();
    export_rows(
        &mut out,
        &fixture_columns(),
        fixture_rows(),
        &format,
        &CancellationToken::new(),
        |_| {},
    )
    .unwrap();
    String::from_utf8(out).unwrap()
}

/// EX-07 / EX-08 / EX-09 golden CSV.
#[test]
fn ex07_ex08_ex09_csv_golden() {
    let text = export_to_string(ExportFormat::Csv(CsvExportOptions::default()));
    assert_snapshot("mixed.csv", &text);
}

/// EX-02 golden JSON array.
#[test]
fn ex02_json_array_golden() {
    let text = export_to_string(ExportFormat::Json(JsonExportOptions {
        mode: JsonExportMode::Array,
        binary: BinaryEncoding::Hex,
    }));
    assert_snapshot("mixed.json", &text);
}

/// EX-02: big NUMERIC as exact raw number; nested JSON verbatim (key order kept).
#[test]
fn ex02_json_decimal_and_nested_json_golden() {
    use rust_decimal::Decimal;
    use std::str::FromStr;
    let cols = vec!["amount".into(), "payload".into()];
    let rows = vec![vec![
        Value::Decimal(Decimal::from_str("1234567890123456789.123456789").unwrap()),
        Value::json_raw(r#"{"z":1,"big":12345678901234567890,"nested":{"b":2,"a":1}}"#),
    ]];
    let mut out = Vec::new();
    export_rows(
        &mut out,
        &cols,
        rows,
        &ExportFormat::Json(JsonExportOptions::default()),
        &CancellationToken::new(),
        |_| {},
    )
    .unwrap();
    assert_snapshot("precision.json", &String::from_utf8(out).unwrap());
}

fn multiline_json_cell() -> Value {
    // Pretty-printed JSON as PG `json` may store it (raw newlines outside strings).
    Value::json_raw("{\n  \"a\": 1,\n  \"b\": 2\n}")
}

/// EX-02: multi-line JSON cell in a JSON array (newlines → spaces).
#[test]
fn ex02_json_array_multiline_cell_golden() {
    let mut out = Vec::new();
    export_rows(
        &mut out,
        &["payload".into()],
        vec![vec![multiline_json_cell()]],
        &ExportFormat::Json(JsonExportOptions {
            mode: JsonExportMode::Array,
            binary: BinaryEncoding::Hex,
        }),
        &CancellationToken::new(),
        |_| {},
    )
    .unwrap();
    assert_snapshot("multiline_cell.json", &String::from_utf8(out).unwrap());
}

/// EX-02: multi-line JSON cell in NDJSON stays one record per line.
#[test]
fn ex02_json_ndjson_multiline_cell_golden() {
    let mut out = Vec::new();
    export_rows(
        &mut out,
        &["payload".into()],
        vec![
            vec![multiline_json_cell()],
            vec![Value::json_raw(r#"{"x":1}"#)],
        ],
        &ExportFormat::Json(JsonExportOptions {
            mode: JsonExportMode::Ndjson,
            binary: BinaryEncoding::Hex,
        }),
        &CancellationToken::new(),
        |_| {},
    )
    .unwrap();
    let text = String::from_utf8(out).unwrap();
    assert_eq!(
        text.lines().count(),
        2,
        "expected 2 NDJSON records:\n{text}"
    );
    assert_snapshot("multiline_cell.ndjson", &text);
}

/// EX-02 NDJSON.
#[test]
fn ex02_json_ndjson_golden() {
    let text = export_to_string(ExportFormat::Json(JsonExportOptions {
        mode: JsonExportMode::Ndjson,
        binary: BinaryEncoding::Base64,
    }));
    assert_snapshot("mixed.ndjson", &text);
}

/// EX-03 INSERT golden per dialect.
#[test]
fn ex03_sql_insert_golden_all_dialects() {
    for (dialect, name) in [
        (Dialect::Postgres, "mixed.postgres.sql"),
        (Dialect::Mysql, "mixed.mysql.sql"),
        (Dialect::Sqlite, "mixed.sqlite.sql"),
    ] {
        let text = export_to_string(ExportFormat::SqlInsert(SqlInsertExportOptions {
            dialect,
            schema: Some("public".into()),
            table: "books".into(),
            batch_size: 2,
            batch_max_bytes: SQL_INSERT_DEFAULT_BATCH_BYTES,
            replace_table_comment: false,
        }));
        assert_snapshot(name, &text);
    }
}

/// EX-03: SQL-result placeholder target + replace comment.
#[test]
fn ex03_sql_insert_placeholder_replace_comment() {
    let text = export_to_string(ExportFormat::SqlInsert(
        SqlInsertExportOptions::placeholder(Dialect::Postgres),
    ));
    assert!(text.contains("INSERT INTO \"table\""));
    assert!(text.contains("-- replace table name"));
}

/// EX-12: flush at the row cap (100 by default) even when under the byte budget.
#[test]
fn ex12_sql_insert_flushes_at_row_cap() {
    let cols = vec!["id".into()];
    let rows: Vec<Vec<Value>> = (1..=SQL_INSERT_DEFAULT_BATCH_ROWS + 3)
        .map(|i| vec![Value::Int(i as i64)])
        .collect();
    let mut out = Vec::new();
    export_rows(
        &mut out,
        &cols,
        rows,
        &ExportFormat::SqlInsert(SqlInsertExportOptions {
            dialect: Dialect::Sqlite,
            schema: None,
            table: "t".into(),
            batch_size: SQL_INSERT_DEFAULT_BATCH_ROWS,
            batch_max_bytes: SQL_INSERT_DEFAULT_BATCH_BYTES,
            replace_table_comment: false,
        }),
        &CancellationToken::new(),
        |_| {},
    )
    .unwrap();
    let text = String::from_utf8(out).unwrap();
    assert_snapshot("batch_rows.sqlite.sql", &text);
    assert_eq!(text.matches("INSERT INTO").count(), 2);
    // First statement has 100 value rows; second has 3.
    let first = text.split("INSERT INTO").nth(1).unwrap();
    assert_eq!(first.matches("\n(").count(), SQL_INSERT_DEFAULT_BATCH_ROWS);
}

/// EX-12: flush when the pending statement would exceed ~1 MiB, even under the row cap.
#[test]
fn ex12_sql_insert_flushes_at_byte_cap() {
    // Wide text so a handful of rows exceed a small byte budget.
    let wide = "x".repeat(200);
    let cols = vec!["id".into(), "payload".into()];
    let rows: Vec<Vec<Value>> = (1..=5)
        .map(|i| vec![Value::Int(i), Value::text(wide.clone())])
        .collect();
    let mut out = Vec::new();
    export_rows(
        &mut out,
        &cols,
        rows,
        &ExportFormat::SqlInsert(SqlInsertExportOptions {
            dialect: Dialect::Postgres,
            schema: Some("public".into()),
            table: "wide".into(),
            batch_size: SQL_INSERT_DEFAULT_BATCH_ROWS,
            // Tight budget: prefix + two wide rows fits; a third forces a flush.
            batch_max_bytes: 500,
            replace_table_comment: false,
        }),
        &CancellationToken::new(),
        |_| {},
    )
    .unwrap();
    let text = String::from_utf8(out).unwrap();
    assert_snapshot("batch_bytes.postgres.sql", &text);
    assert!(
        text.matches("INSERT INTO").count() >= 2,
        "byte budget should split into multiple INSERT statements:\n{text}"
    );
    for stmt in text.split("INSERT INTO").skip(1) {
        let bytes = format!("INSERT INTO{stmt}").len();
        assert!(
            bytes <= 500 + 250,
            "statement roughly within budget (allow one oversized row): {bytes} bytes\nINSERT INTO{stmt}"
        );
    }
}
