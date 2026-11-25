//! Streaming CSV import (M3-3 / US-12).
//!
//! Readers take any [`std::io::Read`] (or an in-memory `&[u8]` / `Cursor`) so
//! tests never touch the real filesystem. Column mapping is validated against
//! an injectable [`TableSchemaSource`]. Parsed rows feed the existing insert
//! path via [`ImportRowSink`] (e.g. [`ChangeSet`] or a collecting test sink).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{self, BufRead, BufReader, Read};

use chrono::NaiveDate;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::change_set::{ChangeError, ChangeSet};
use crate::column_kind::ColumnKind;
use crate::connection::SafeMode;
use crate::export::{encode_binary, BinaryEncoding, CsvExportOptions};
use crate::safe_mode::{decide_grid_commit_gate, GridCommitGate};
use crate::schema_source::{SchemaSourceError, TableSchemaSource};
use crate::types::{ColumnMeta, Value};

/// How CSV NULL cells are recognized on import (must match export `null_repr`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CsvImportOptions {
    pub delimiter: u8,
    /// First row is column headers (default true).
    pub has_header: bool,
    /// Field texts treated as SQL NULL (default: `"NULL"` only).
    pub null_reprs: Vec<String>,
    /// How binary columns are decoded from CSV text.
    pub binary: BinaryEncoding,
    /// When true, every header must map to a table column (default true).
    pub reject_unknown_headers: bool,
    /// Table columns that must appear in the CSV (e.g. NOT NULL without default).
    pub required_columns: Vec<String>,
}

impl Default for CsvImportOptions {
    fn default() -> Self {
        Self {
            delimiter: b',',
            has_header: true,
            // Match default CSV export: `\N` = SQL NULL (EX-08 / IM-02).
            null_reprs: vec!["\\N".into()],
            binary: BinaryEncoding::Hex,
            reject_unknown_headers: true,
            required_columns: Vec::new(),
        }
    }
}

impl CsvImportOptions {
    /// Options that round-trip with the default CSV exporter.
    pub fn matching_export(export: &CsvExportOptions) -> Self {
        Self {
            delimiter: export.delimiter,
            has_header: export.include_header,
            null_reprs: vec![export.null_repr.clone()],
            binary: export.binary,
            reject_unknown_headers: true,
            required_columns: Vec::new(),
        }
    }
}

/// One successfully mapped import row ready for [`ChangeSet::insert_row`].
#[derive(Debug, Clone, PartialEq)]
pub struct ImportRow {
    /// 1-based line number in the CSV source (header is line 1 when present).
    pub line: u64,
    pub values: BTreeMap<String, Value>,
}

/// Per-row or parse failure with a line number.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("line {line}: {message}")]
pub struct ImportRowError {
    pub line: u64,
    pub message: String,
}

impl ImportRowError {
    pub fn new(line: u64, message: impl Into<String>) -> Self {
        Self {
            line,
            message: message.into(),
        }
    }
}

/// Import-level failure (before/while streaming).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ImportError {
    #[error("{0}")]
    Io(String),
    #[error("import cancelled")]
    Cancelled,
    #[error("{0}")]
    Schema(String),
    #[error("{0}")]
    Mapping(String),
    #[error("line {line}: {message}")]
    Row { line: u64, message: String },
    #[error("import refused: connection is read-only")]
    RefuseReadOnly,
    #[error("import requires confirmation (safe mode)")]
    NeedsConfirm,
    #[error("{0}")]
    Sink(String),
    #[error("invalid UTF-8 in CSV input")]
    InvalidUtf8,
}

impl From<io::Error> for ImportError {
    fn from(err: io::Error) -> Self {
        if err.kind() == io::ErrorKind::InvalidData {
            Self::InvalidUtf8
        } else {
            Self::Io(err.to_string())
        }
    }
}

impl From<SchemaSourceError> for ImportError {
    fn from(err: SchemaSourceError) -> Self {
        Self::Schema(err.to_string())
    }
}

impl From<ImportRowError> for ImportError {
    fn from(err: ImportRowError) -> Self {
        Self::Row {
            line: err.line,
            message: err.message,
        }
    }
}

impl From<ChangeError> for ImportError {
    fn from(err: ChangeError) -> Self {
        Self::Sink(err.to_string())
    }
}

/// Safe-mode gate for CSV import (a write). Reuses the grid-commit decision.
pub fn decide_import_gate(mode: SafeMode) -> GridCommitGate {
    decide_grid_commit_gate(mode)
}

/// Refuse or require confirm before streaming rows (IM-05).
pub fn ensure_import_allowed(mode: SafeMode, confirmed: bool) -> Result<(), ImportError> {
    match decide_import_gate(mode) {
        GridCommitGate::Allow => Ok(()),
        GridCommitGate::RefuseReadOnly => Err(ImportError::RefuseReadOnly),
        GridCommitGate::Confirm if confirmed => Ok(()),
        GridCommitGate::Confirm => Err(ImportError::NeedsConfirm),
    }
}

/// Maps CSV fields onto table columns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnMapping {
    /// Parallel to CSV columns: `Some(table_col)` or `None` to skip.
    pub targets: Vec<Option<String>>,
}

impl ColumnMapping {
    /// Build a mapping by matching CSV headers to table column names (exact).
    pub fn from_headers(
        headers: &[String],
        table_columns: &[ColumnMeta],
        options: &CsvImportOptions,
    ) -> Result<Self, ImportError> {
        let table_names: BTreeSet<&str> = table_columns.iter().map(|c| c.name.as_str()).collect();
        let mut targets = Vec::with_capacity(headers.len());
        let mut seen = BTreeSet::new();
        for h in headers {
            if table_names.contains(h.as_str()) {
                if !seen.insert(h.clone()) {
                    return Err(ImportError::Mapping(format!("duplicate CSV header `{h}`")));
                }
                targets.push(Some(h.clone()));
            } else if options.reject_unknown_headers {
                return Err(ImportError::Mapping(format!(
                    "unknown CSV header `{h}` is not a column of the target table"
                )));
            } else {
                targets.push(None);
            }
        }
        for req in &options.required_columns {
            if !seen.contains(req) {
                return Err(ImportError::Mapping(format!(
                    "required column `{req}` is missing from the CSV header"
                )));
            }
        }
        Ok(Self { targets })
    }

    /// Positional mapping when there is no header row (CSV col i → table col i).
    pub fn positional(table_columns: &[ColumnMeta]) -> Self {
        Self {
            targets: table_columns.iter().map(|c| Some(c.name.clone())).collect(),
        }
    }
}

/// Receives mapped rows so import can feed [`ChangeSet`] or a test collector.
pub trait ImportRowSink {
    fn push_row(&mut self, row: ImportRow) -> Result<(), ImportError>;
}

impl ImportRowSink for ChangeSet {
    fn push_row(&mut self, row: ImportRow) -> Result<(), ImportError> {
        self.insert_row(row.values)?;
        Ok(())
    }
}

/// Collecting sink for unit tests (no DB).
#[derive(Debug, Default, Clone)]
pub struct CollectingImportSink {
    pub rows: Vec<ImportRow>,
}

impl ImportRowSink for CollectingImportSink {
    fn push_row(&mut self, row: ImportRow) -> Result<(), ImportError> {
        self.rows.push(row);
        Ok(())
    }
}

/// Sink that records rows and can be told to fail (simulates rollback boundary).
#[derive(Debug, Default)]
pub struct TransactionalImportSink {
    pub committed: Vec<ImportRow>,
    pub staging: Vec<ImportRow>,
    pub committed_flag: bool,
}

impl TransactionalImportSink {
    pub fn begin(&mut self) {
        self.staging.clear();
        self.committed_flag = false;
    }

    pub fn commit(&mut self) {
        self.committed.append(&mut self.staging);
        self.committed_flag = true;
    }

    pub fn rollback(&mut self) {
        self.staging.clear();
        self.committed_flag = false;
    }
}

impl ImportRowSink for TransactionalImportSink {
    fn push_row(&mut self, row: ImportRow) -> Result<(), ImportError> {
        self.staging.push(row);
        Ok(())
    }
}

/// Parse a CSV field into a typed [`Value`] using column kind + null sentinels.
pub fn parse_import_field(
    raw: &str,
    type_name: &str,
    options: &CsvImportOptions,
) -> Result<Value, String> {
    if options.null_reprs.iter().any(|n| n == raw) {
        return Ok(Value::Null);
    }
    let kind = ColumnKind::from_type_name(type_name);
    match kind {
        ColumnKind::Bool => {
            let t = raw.trim();
            if t.eq_ignore_ascii_case("true") || t == "1" || t.eq_ignore_ascii_case("t") {
                return Ok(Value::Bool(true));
            }
            if t.eq_ignore_ascii_case("false") || t == "0" || t.eq_ignore_ascii_case("f") {
                return Ok(Value::Bool(false));
            }
            Err(format!("cannot parse bool from `{raw}`"))
        }
        ColumnKind::Int => raw
            .trim()
            .parse::<i64>()
            .map(Value::Int)
            .map_err(|_| format!("cannot parse integer from `{raw}`")),
        ColumnKind::Float => raw
            .trim()
            .parse::<f64>()
            .map(Value::Float)
            .map_err(|_| format!("cannot parse float from `{raw}`")),
        ColumnKind::Decimal => raw
            .trim()
            .parse::<rust_decimal::Decimal>()
            .map(Value::Decimal)
            .map_err(|_| format!("cannot parse decimal from `{raw}`")),
        ColumnKind::Uuid => Uuid::parse_str(raw.trim())
            .map(Value::Uuid)
            .map_err(|_| format!("cannot parse uuid from `{raw}`")),
        ColumnKind::Date => NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d")
            .map(Value::Date)
            .map_err(|_| format!("cannot parse date from `{raw}`")),
        ColumnKind::Json => Ok(Value::json_raw(raw)),
        ColumnKind::Bytes => decode_bytes(raw, options.binary).map(Value::Bytes),
        ColumnKind::Text
        | ColumnKind::Time
        | ColumnKind::Timestamp
        | ColumnKind::Interval
        | ColumnKind::Other => Ok(Value::text(raw)),
    }
}

fn decode_bytes(raw: &str, encoding: BinaryEncoding) -> Result<Vec<u8>, String> {
    let t = raw.trim();
    match encoding {
        BinaryEncoding::Hex => {
            let hex = t
                .strip_prefix("\\x")
                .or_else(|| t.strip_prefix("\\X"))
                .or_else(|| t.strip_prefix("0x"))
                .or_else(|| t.strip_prefix("0X"))
                .unwrap_or(t);
            if hex.len() % 2 != 0 {
                return Err("hex binary value has an odd length".into());
            }
            (0..hex.len())
                .step_by(2)
                .map(|i| {
                    u8::from_str_radix(&hex[i..i + 2], 16)
                        .map_err(|_| format!("invalid hex in binary field at offset {i}"))
                })
                .collect()
        }
        BinaryEncoding::Base64 => {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD
                .decode(t)
                .map_err(|e| format!("invalid base64: {e}"))
        }
    }
}

fn type_lookup(columns: &[ColumnMeta]) -> HashMap<String, String> {
    columns
        .iter()
        .map(|c| (c.name.clone(), c.type_name.clone()))
        .collect()
}

/// Map one CSV record into an [`ImportRow`].
pub fn map_record(
    line: u64,
    fields: &[String],
    mapping: &ColumnMapping,
    type_by_name: &HashMap<String, String>,
    options: &CsvImportOptions,
) -> Result<ImportRow, ImportRowError> {
    if fields.len() != mapping.targets.len() {
        return Err(ImportRowError::new(
            line,
            format!(
                "expected {} field(s), found {}",
                mapping.targets.len(),
                fields.len()
            ),
        ));
    }
    let mut values = BTreeMap::new();
    for (field, target) in fields.iter().zip(mapping.targets.iter()) {
        let Some(col) = target else {
            continue;
        };
        let type_name = type_by_name.get(col).map(|s| s.as_str()).unwrap_or("");
        let value = parse_import_field(field, type_name, options)
            .map_err(|message| ImportRowError::new(line, format!("column `{col}`: {message}")))?;
        values.insert(col.clone(), value);
    }
    Ok(ImportRow { line, values })
}

/// Streaming CSV importer over any [`Read`] source.
pub struct CsvImporter<R> {
    reader: csv::Reader<R>,
    options: CsvImportOptions,
    mapping: ColumnMapping,
    type_by_name: HashMap<String, String>,
    line: u64,
    headers: Vec<String>,
}

impl<R: Read> CsvImporter<R> {
    /// Create an importer after resolving headers + column mapping via `schema`.
    pub fn open(
        reader: R,
        schema: &dyn TableSchemaSource,
        table_schema: &str,
        table: &str,
        options: CsvImportOptions,
    ) -> Result<Self, ImportError> {
        let table_columns = schema.columns(table_schema, table)?;
        Self::open_with_columns(reader, &table_columns, options)
    }

    /// Open with an already-fetched column list (tests / callers with metadata).
    pub fn open_with_columns(
        reader: R,
        table_columns: &[ColumnMeta],
        options: CsvImportOptions,
    ) -> Result<Self, ImportError> {
        let mut builder = csv::ReaderBuilder::new();
        builder
            .delimiter(options.delimiter)
            .has_headers(false)
            .flexible(false);
        let mut csv_reader = builder.from_reader(reader);

        let (headers, mapping, line) = if options.has_header {
            let record = csv_reader
                .records()
                .next()
                .ok_or_else(|| ImportError::Mapping("CSV is empty (missing header row)".into()))?
                .map_err(|e| ImportError::Io(e.to_string()))?;
            let headers: Vec<String> = record.iter().map(|s| s.to_string()).collect();
            let mapping = ColumnMapping::from_headers(&headers, table_columns, &options)?;
            (headers, mapping, 1u64)
        } else {
            let mapping = ColumnMapping::positional(table_columns);
            let headers: Vec<String> = table_columns.iter().map(|c| c.name.clone()).collect();
            (headers, mapping, 0u64)
        };

        Ok(Self {
            reader: csv_reader,
            options,
            mapping,
            type_by_name: type_lookup(table_columns),
            line,
            headers,
        })
    }

    pub fn headers(&self) -> &[String] {
        &self.headers
    }

    pub fn mapping(&self) -> &ColumnMapping {
        &self.mapping
    }

    /// Next mapped row, or `None` at end of stream. Row errors include line numbers.
    pub fn next_row(&mut self) -> Option<Result<ImportRow, ImportRowError>> {
        match self.reader.records().next() {
            None => None,
            Some(Err(err)) => {
                self.line += 1;
                Some(Err(ImportRowError::new(self.line, err.to_string())))
            }
            Some(Ok(record)) => {
                self.line += 1;
                let fields: Vec<String> = record.iter().map(|s| s.to_string()).collect();
                Some(map_record(
                    self.line,
                    &fields,
                    &self.mapping,
                    &self.type_by_name,
                    &self.options,
                ))
            }
        }
    }
}

/// Stream all rows into `sink`, honouring cancel and reporting progress.
///
/// On cancel or hard error, returns the error; the caller rolls back the sink
/// (see [`TransactionalImportSink`]). Per-row type/parse errors are returned
/// immediately (IM-06: no silent success).
#[allow(clippy::too_many_arguments)]
pub fn import_csv_into<R, S, F>(
    reader: R,
    schema: &dyn TableSchemaSource,
    table_schema: &str,
    table: &str,
    options: CsvImportOptions,
    sink: &mut S,
    cancel: &CancellationToken,
    mut on_progress: F,
) -> Result<u64, ImportError>
where
    R: Read,
    S: ImportRowSink,
    F: FnMut(u64),
{
    if cancel.is_cancelled() {
        return Err(ImportError::Cancelled);
    }
    let mut importer = CsvImporter::open(reader, schema, table_schema, table, options)?;
    let mut rows = 0u64;
    while let Some(item) = importer.next_row() {
        if cancel.is_cancelled() {
            return Err(ImportError::Cancelled);
        }
        let row = item?;
        sink.push_row(row)?;
        rows += 1;
        on_progress(rows);
    }
    Ok(rows)
}

/// Validate that a byte slice is valid UTF-8 before parsing (IM-08).
pub fn require_utf8(bytes: &[u8]) -> Result<&str, ImportError> {
    std::str::from_utf8(bytes).map_err(|_| ImportError::InvalidUtf8)
}

/// Read a UTF-8 CSV from any [`Read`], failing clearly on invalid encoding.
pub fn read_utf8_source<R: Read>(mut reader: R) -> Result<String, ImportError> {
    let mut buf = Vec::new();
    reader.read_to_end(&mut buf)?;
    require_utf8(&buf).map(|s| s.to_string())
}

/// Line-oriented helper: detect BOM and strip a leading UTF-8 BOM if present.
pub fn strip_utf8_bom(text: &str) -> &str {
    text.strip_prefix('\u{feff}').unwrap_or(text)
}

/// Peek whether `reader` starts with a UTF-8 BOM (does not consume beyond BOM).
pub fn peek_utf8_bom<R: BufRead>(reader: &mut R) -> Result<bool, ImportError> {
    let buf = reader.fill_buf()?;
    Ok(buf.starts_with(&[0xEF, 0xBB, 0xBF]))
}

/// Encode a value back to CSV text using the same rules as the exporter (round-trip helpers).
pub fn value_to_csv_field(value: &Value, options: &CsvExportOptions) -> String {
    match value {
        Value::Null => options.null_repr.clone(),
        Value::Bytes(b) => encode_binary(b, options.binary),
        other => other.display().into_owned(),
    }
}

/// Convenience: wrap a string slice as a [`std::io::Cursor`] reader.
pub fn cursor_from_str(s: &str) -> io::Cursor<&[u8]> {
    io::Cursor::new(s.as_bytes())
}

/// BufReader helper for tests that inject CSV text.
pub fn buf_reader_from_str(s: &str) -> BufReader<io::Cursor<&[u8]>> {
    BufReader::new(cursor_from_str(s))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::{export_rows, ExportFormat};
    use crate::schema_source::StaticTableSchema;
    use crate::types::ColumnMeta;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn books_schema() -> StaticTableSchema {
        StaticTableSchema::new().with_table(
            "public",
            "books",
            vec![
                ColumnMeta {
                    name: "id".into(),
                    type_name: "INTEGER".into(),
                },
                ColumnMeta {
                    name: "title".into(),
                    type_name: "TEXT".into(),
                },
                ColumnMeta {
                    name: "note".into(),
                    type_name: "TEXT".into(),
                },
            ],
        )
    }

    #[test]
    fn im03_header_mapping_and_unknown_header() {
        let schema = books_schema();
        let cols = schema.columns("public", "books").unwrap();
        let ok = ColumnMapping::from_headers(
            &["id".into(), "title".into()],
            &cols,
            &CsvImportOptions::default(),
        )
        .unwrap();
        assert_eq!(ok.targets, vec![Some("id".into()), Some("title".into())]);

        let err = ColumnMapping::from_headers(
            &["id".into(), "nope".into()],
            &cols,
            &CsvImportOptions::default(),
        )
        .unwrap_err();
        assert!(matches!(err, ImportError::Mapping(_)));
    }

    #[test]
    fn im03_missing_required_column() {
        let schema = books_schema();
        let cols = schema.columns("public", "books").unwrap();
        let opts = CsvImportOptions {
            required_columns: vec!["id".into()],
            ..CsvImportOptions::default()
        };
        let err = ColumnMapping::from_headers(&["title".into()], &cols, &opts).unwrap_err();
        assert!(matches!(err, ImportError::Mapping(m) if m.contains("required")));
    }

    #[test]
    fn im01_im06_stream_rows_and_type_error_with_line() {
        let schema = books_schema();
        let csv = "id,title,note\n1,Alpha,\\N\nx,Bad,\\N\n";
        let mut sink = CollectingImportSink::default();
        let cancel = CancellationToken::new();
        let err = import_csv_into(
            cursor_from_str(csv),
            &schema,
            "public",
            "books",
            CsvImportOptions::default(),
            &mut sink,
            &cancel,
            |_| {},
        )
        .unwrap_err();
        // First row may have been pushed before the bad row failed.
        assert_eq!(sink.rows.len(), 1);
        assert_eq!(sink.rows[0].values.get("note"), Some(&Value::Null));
        match err {
            ImportError::Row { line, message } => {
                assert_eq!(line, 3);
                assert!(message.contains("integer") || message.contains("column `id`"));
            }
            other => panic!("expected row error, got {other:?}"),
        }
    }

    #[test]
    fn im02_export_import_round_trip_null_quote_unicode() {
        let export_opts = CsvExportOptions::default();
        let cols = vec!["id".into(), "title".into(), "note".into()];
        let rows = vec![
            vec![Value::Int(1), Value::text("a,b"), Value::Null],
            vec![
                Value::Int(2),
                Value::text("say \"hi\"\nthere"),
                Value::text(""),
            ],
            vec![Value::Int(3), Value::text("café"), Value::text("NULL")],
        ];
        let mut out = Vec::new();
        export_rows(
            &mut out,
            &cols,
            rows.clone(),
            &ExportFormat::Csv(export_opts.clone()),
            &CancellationToken::new(),
            |_| {},
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();

        let schema = books_schema();
        let mut sink = CollectingImportSink::default();
        let n = import_csv_into(
            cursor_from_str(&text),
            &schema,
            "public",
            "books",
            CsvImportOptions::matching_export(&export_opts),
            &mut sink,
            &CancellationToken::new(),
            |_| {},
        )
        .unwrap();
        assert_eq!(n, 3);
        assert_eq!(sink.rows[0].values.get("note"), Some(&Value::Null));
        assert_eq!(sink.rows[1].values.get("note"), Some(&Value::text("")));
        assert_eq!(sink.rows[2].values.get("title"), Some(&Value::text("café")));
        assert_eq!(sink.rows[2].values.get("note"), Some(&Value::text("NULL")));
        assert_eq!(sink.rows[0].values.get("title"), Some(&Value::text("a,b")));
    }

    #[test]
    fn im04_im07_cancel_rolls_back_transactional_sink() {
        let schema = books_schema();
        let csv = "id,title,note\n1,a,\\N\n2,b,\\N\n3,c,\\N\n";
        let mut sink = TransactionalImportSink::default();
        sink.begin();
        let cancel = CancellationToken::new();
        let progress = Arc::new(AtomicUsize::new(0));
        let progress2 = progress.clone();
        let cancel2 = cancel.clone();
        let err = import_csv_into(
            cursor_from_str(csv),
            &schema,
            "public",
            "books",
            CsvImportOptions::default(),
            &mut sink,
            &cancel,
            |n| {
                progress2.store(n as usize, Ordering::Relaxed);
                if n >= 1 {
                    cancel2.cancel();
                }
            },
        )
        .unwrap_err();
        assert_eq!(err, ImportError::Cancelled);
        assert!(!sink.staging.is_empty() || progress.load(Ordering::Relaxed) >= 1);
        sink.rollback();
        assert!(sink.committed.is_empty());
        assert!(sink.staging.is_empty());
        assert!(!sink.committed_flag);
    }

    #[test]
    fn im05_safe_mode_gates_import() {
        assert_eq!(decide_import_gate(SafeMode::Off), GridCommitGate::Allow);
        assert_eq!(
            decide_import_gate(SafeMode::Confirm),
            GridCommitGate::Confirm
        );
        assert_eq!(
            decide_import_gate(SafeMode::ReadOnly),
            GridCommitGate::RefuseReadOnly
        );
        assert!(ensure_import_allowed(SafeMode::Off, false).is_ok());
        assert!(ensure_import_allowed(SafeMode::Confirm, true).is_ok());
        assert_eq!(
            ensure_import_allowed(SafeMode::Confirm, false).unwrap_err(),
            ImportError::NeedsConfirm
        );
        assert_eq!(
            ensure_import_allowed(SafeMode::ReadOnly, true).unwrap_err(),
            ImportError::RefuseReadOnly
        );
    }

    #[test]
    fn im08_invalid_utf8_fails_clearly() {
        let bad = b"id,title\n1,\xff\xfe\n";
        assert_eq!(require_utf8(bad).unwrap_err(), ImportError::InvalidUtf8);
        let err = read_utf8_source(&bad[..]).unwrap_err();
        assert_eq!(err, ImportError::InvalidUtf8);
    }

    #[test]
    fn im08_utf8_and_bom_ok() {
        let with_bom = "\u{feff}id,title,note\n1,a,\\N\n";
        let stripped = strip_utf8_bom(with_bom);
        let schema = books_schema();
        let mut sink = CollectingImportSink::default();
        let n = import_csv_into(
            cursor_from_str(stripped),
            &schema,
            "public",
            "books",
            CsvImportOptions::default(),
            &mut sink,
            &CancellationToken::new(),
            |_| {},
        )
        .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn feeds_changeset_insert_path() {
        use crate::change_set::{ChangeSet, TableRef};
        let schema = books_schema();
        let csv = "id,title,note\n1,Hello,\\N\n";
        let mut cs = ChangeSet::try_new(
            TableRef::new("public", "books"),
            vec!["id".into()],
            vec!["id".into(), "title".into(), "note".into()],
        )
        .unwrap();
        let n = import_csv_into(
            cursor_from_str(csv),
            &schema,
            "public",
            "books",
            CsvImportOptions::default(),
            &mut cs,
            &CancellationToken::new(),
            |_| {},
        )
        .unwrap();
        assert_eq!(n, 1);
        assert_eq!(cs.changes().len(), 1);
    }
}
