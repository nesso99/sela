//! Golden snapshots for M3-3 export (EX-07 / EX-02 / EX-03).
//!
//! Expected files live under `tests/export_snapshots/`. Normalize CRLF so a
//! Windows checkout still matches (see `.gitattributes`).

use std::path::PathBuf;

use sela_core::{
    export_rows, BinaryEncoding, CsvExportOptions, Dialect, ExportFormat, JsonExportMode,
    JsonExportOptions, SqlInsertExportOptions, Value,
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
            table: "public.books".into(),
            batch_size: 2,
        }));
        assert_snapshot(name, &text);
    }
}
