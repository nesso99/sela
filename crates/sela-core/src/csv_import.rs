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
use crate::export::{
    encode_binary, escape_csv_field_delim, escape_csv_field_delim_ex, BinaryEncoding,
    CsvExportOptions,
};
use crate::safe_mode::{decide_grid_commit_gate, GridCommitGate};
use crate::schema_source::{SchemaSourceError, TableSchemaSource};
use crate::types::{ColumnMeta, Value};

/// How CSV NULL cells are recognized on import (must match export NULL encoding).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CsvImportOptions {
    pub delimiter: u8,
    /// First row is column headers (default true; IM-03).
    pub has_header: bool,
    /// When true (default), an **unquoted** empty field is SQL NULL (IM-02 / UX).
    /// A quoted empty field (`""`) stays `''`. Additional sentinels (e.g. `\N`) go in
    /// [`Self::null_reprs`].
    pub empty_field_is_null: bool,
    /// Extra field texts treated as SQL NULL (e.g. `"\\N"` when export used that encoding).
    pub null_reprs: Vec<String>,
    /// How binary columns are decoded from CSV text.
    pub binary: BinaryEncoding,
    /// When true, unknown headers error. Default **false**: unmatched → Skip (IM-03).
    pub reject_unknown_headers: bool,
    /// Required table columns that must be mapped (NOT NULL, no default, not generated).
    pub required_columns: Vec<String>,
}

impl Default for CsvImportOptions {
    fn default() -> Self {
        Self {
            delimiter: b',',
            has_header: true,
            empty_field_is_null: true,
            null_reprs: Vec::new(),
            binary: BinaryEncoding::Hex,
            reject_unknown_headers: false,
            required_columns: Vec::new(),
        }
    }
}

impl CsvImportOptions {
    /// Options that round-trip with a CSV exporter configuration.
    pub fn matching_export(export: &CsvExportOptions) -> Self {
        use crate::export::CsvNullEncoding;
        let (empty_field_is_null, null_reprs) = match export.null_encoding {
            CsvNullEncoding::EmptyField => (true, Vec::new()),
            CsvNullEncoding::LiteralBackslashN => (false, vec!["\\N".into()]),
        };
        Self {
            delimiter: export.delimiter,
            has_header: export.include_header,
            empty_field_is_null,
            null_reprs,
            binary: export.binary,
            reject_unknown_headers: false,
            required_columns: Vec::new(),
        }
    }
}

/// One CSV field with quote awareness (so empty ≠ `""` for NULL round-trip).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CsvField {
    pub text: String,
    pub quoted: bool,
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
    #[error("Import cancelled.")]
    Cancelled,
    #[error("{0}")]
    Schema(String),
    #[error("{0}")]
    Mapping(String),
    #[error("line {line}: {message}")]
    Row { line: u64, message: String },
    #[error("Import is disabled in Read-only mode.")]
    RefuseReadOnly,
    #[error("import requires confirmation (safe mode)")]
    NeedsConfirm,
    #[error("{0}")]
    Sink(String),
    #[error("line {line}: invalid UTF-8 in CSV input")]
    InvalidUtf8 { line: u64 },
}

impl From<io::Error> for ImportError {
    fn from(err: io::Error) -> Self {
        if err.kind() == io::ErrorKind::InvalidData {
            Self::InvalidUtf8 { line: 0 }
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

/// Status copy when import is refused in Read-only mode (IM-05).
pub const IMPORT_REFUSED_READ_ONLY_STATUS: &str = "Import is disabled in Read-only mode.";

/// Status copy when import is cancelled (IM-04).
pub const IMPORT_CANCELLED_STATUS: &str = "Import cancelled.";

/// Pre-scan data-row count for Confirm copy / progress `{total}` (IM-05 / IM-07).
///
/// Counts every row after the header when `has_header` is on, otherwise every
/// row. Keeps a counter only — does not materialize cells. Fails on I/O or
/// malformed CSV the same way import would before the first mapped row.
pub fn count_csv_data_rows<R: Read>(
    reader: R,
    options: &CsvImportOptions,
) -> Result<u64, ImportError> {
    let mut reader = BufReader::new(reader);
    strip_leading_utf8_bom(&mut reader)?;
    let mut buf = Vec::new();
    let mut count = 0u64;
    if options.has_header && read_csv_record(&mut reader, options.delimiter, &mut buf)?.is_none() {
        return Ok(0);
    }
    while read_csv_record(&mut reader, options.delimiter, &mut buf)?.is_some() {
        count += 1;
    }
    Ok(count)
}

/// Maps CSV fields onto table columns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnMapping {
    /// Parallel to CSV columns: `Some(table_col)` or `None` to skip.
    pub targets: Vec<Option<String>>,
}

impl ColumnMapping {
    /// Build a mapping by matching CSV headers to table column names.
    ///
    /// Matching is **case-insensitive** (IM-03). Prefer an exact-case match when
    /// several table columns collide on case. Unmatched headers become Skip unless
    /// [`CsvImportOptions::reject_unknown_headers`] is set.
    pub fn from_headers(
        headers: &[String],
        table_columns: &[ColumnMeta],
        options: &CsvImportOptions,
    ) -> Result<Self, ImportError> {
        let mut targets = Vec::with_capacity(headers.len());
        let mut seen_table: BTreeSet<String> = BTreeSet::new();
        for h in headers {
            if let Some(col) = match_header_to_column(h, table_columns) {
                if !seen_table.insert(col.clone()) {
                    return Err(ImportError::Mapping(format!(
                        "duplicate CSV header mapping onto `{col}`"
                    )));
                }
                targets.push(Some(col));
            } else if options.reject_unknown_headers {
                return Err(ImportError::Mapping(format!(
                    "unknown CSV header `{h}` is not a column of the target table"
                )));
            } else {
                targets.push(None); // Skip (IM-03)
            }
        }
        for req in &options.required_columns {
            let mapped = seen_table.iter().any(|c| c.eq_ignore_ascii_case(req));
            if !mapped {
                return Err(ImportError::Mapping(
                    "Map every required column or cancel.".into(),
                ));
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
#[cfg(test)]
#[derive(Debug, Default, Clone)]
struct CollectingImportSink {
    rows: Vec<ImportRow>,
}

#[cfg(test)]
impl ImportRowSink for CollectingImportSink {
    fn push_row(&mut self, row: ImportRow) -> Result<(), ImportError> {
        self.rows.push(row);
        Ok(())
    }
}

/// Sink that records rows and can be told to fail (simulates rollback boundary).
#[cfg(test)]
#[derive(Debug, Default)]
struct TransactionalImportSink {
    committed: Vec<ImportRow>,
    staging: Vec<ImportRow>,
    committed_flag: bool,
}

#[cfg(test)]
impl TransactionalImportSink {
    fn begin(&mut self) {
        self.staging.clear();
        self.committed_flag = false;
    }

    fn rollback(&mut self) {
        self.staging.clear();
        self.committed_flag = false;
    }
}

#[cfg(test)]
impl ImportRowSink for TransactionalImportSink {
    fn push_row(&mut self, row: ImportRow) -> Result<(), ImportError> {
        self.staging.push(row);
        Ok(())
    }
}

fn match_header_to_column(header: &str, table_columns: &[ColumnMeta]) -> Option<String> {
    let mut insensitive: Option<&str> = None;
    for col in table_columns {
        if col.name == header {
            return Some(col.name.clone());
        }
        if col.name.eq_ignore_ascii_case(header) {
            insensitive.get_or_insert(col.name.as_str());
        }
    }
    insensitive.map(str::to_string)
}

/// Parse a CSV field into a typed [`Value`] using column kind + null sentinels.
pub fn parse_import_field(
    raw: &str,
    type_name: &str,
    options: &CsvImportOptions,
) -> Result<Value, String> {
    parse_import_field_ex(
        &CsvField {
            text: raw.to_string(),
            quoted: false,
        },
        type_name,
        options,
    )
}

/// Like [`parse_import_field`], honouring whether the field was quoted (IM-02).
pub fn parse_import_field_ex(
    field: &CsvField,
    type_name: &str,
    options: &CsvImportOptions,
) -> Result<Value, String> {
    let raw = field.text.as_str();
    // Sentinels (e.g. `\N`) only match **unquoted** fields so a quoted `"\N"`
    // stays the text value (EX-08 / IM-02).
    if !field.quoted && options.null_reprs.iter().any(|n| n == raw) {
        return Ok(Value::Null);
    }
    if raw.is_empty() {
        if options.empty_field_is_null && !field.quoted {
            return Ok(Value::Null);
        }
        return Ok(Value::text(""));
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
    let tagged: Vec<CsvField> = fields
        .iter()
        .map(|t| CsvField {
            text: t.clone(),
            quoted: false,
        })
        .collect();
    map_record_fields(line, &tagged, mapping, type_by_name, options)
}

/// Map quote-aware fields into an [`ImportRow`].
pub fn map_record_fields(
    line: u64,
    fields: &[CsvField],
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
        let value = parse_import_field_ex(field, type_name, options)
            .map_err(|message| ImportRowError::new(line, format!("column `{col}`: {message}")))?;
        values.insert(col.clone(), value);
    }
    Ok(ImportRow { line, values })
}

/// Strip a leading UTF-8 BOM (`EF BB BF`) if present (IM-08).
fn strip_leading_utf8_bom<R: BufRead>(reader: &mut R) -> Result<(), ImportError> {
    let buf = reader.fill_buf()?;
    if buf.starts_with(&[0xEF, 0xBB, 0xBF]) {
        reader.consume(3);
    }
    Ok(())
}

/// RFC 4180 record reader that tracks whether each field was quoted (IM-02).
///
/// Uses buffered reads. A `"` only starts a quoted field at field start; a
/// mid-field quote errors instead of swallowing the rest of the file.
fn read_csv_record<R: BufRead>(
    reader: &mut R,
    delim: u8,
    buf: &mut Vec<u8>,
) -> Result<Option<(Vec<CsvField>, u64)>, ImportError> {
    buf.clear();
    let mut lines_consumed = 0u64;
    let mut in_quotes = false;
    let mut saw_any = false;
    let mut tmp = [0u8; 1];
    loop {
        match reader.read(&mut tmp)? {
            0 => {
                if !saw_any {
                    return Ok(None);
                }
                if in_quotes {
                    return Err(ImportError::Io("unterminated quoted CSV field".into()));
                }
                break;
            }
            1 => {
                saw_any = true;
                let b = tmp[0];
                if b == b'"' {
                    if in_quotes {
                        buf.push(b);
                        // Look ahead for escaped quote.
                        let mut next = [0u8; 1];
                        match reader.read(&mut next)? {
                            1 if next[0] == b'"' => {
                                buf.push(b'"');
                            }
                            1 => {
                                in_quotes = false;
                                if next[0] == b'\n' {
                                    lines_consumed += 1;
                                    break;
                                }
                                if next[0] == b'\r' {
                                    let mut n2 = [0u8; 1];
                                    match reader.read(&mut n2)? {
                                        1 if n2[0] == b'\n' => {
                                            lines_consumed += 1;
                                            break;
                                        }
                                        1 => {
                                            buf.push(b'\r');
                                            buf.push(n2[0]);
                                        }
                                        _ => {
                                            lines_consumed += 1;
                                            break;
                                        }
                                    }
                                } else {
                                    buf.push(next[0]);
                                }
                            }
                            _ => break,
                        }
                    } else {
                        // Quote may only open a field at the start of a field.
                        let at_field_start = buf.is_empty() || buf.last() == Some(&delim);
                        if !at_field_start {
                            return Err(ImportError::Io(
                                "unquoted CSV field contains a quote; quote the whole field or escape it"
                                    .into(),
                            ));
                        }
                        buf.push(b);
                        in_quotes = true;
                    }
                } else if (b == b'\n' || b == b'\r') && !in_quotes {
                    if b == b'\r' {
                        let mut next = [0u8; 1];
                        if reader.read(&mut next)? == 1 && next[0] != b'\n' {
                            buf.push(b'\r');
                            buf.push(next[0]);
                            continue;
                        }
                    }
                    lines_consumed += 1;
                    break;
                } else {
                    if b == b'\n' {
                        lines_consumed += 1;
                    }
                    buf.push(b);
                }
            }
            _ => unreachable!(),
        }
    }
    if lines_consumed == 0 {
        lines_consumed = 1;
    }
    let fields = parse_csv_fields(buf, delim)?;
    Ok(Some((fields, lines_consumed)))
}

fn parse_csv_fields(raw: &[u8], delim: u8) -> Result<Vec<CsvField>, ImportError> {
    let mut fields = Vec::new();
    let mut i = 0;
    let bytes = raw;
    if bytes.is_empty() {
        return Ok(vec![CsvField {
            text: String::new(),
            quoted: false,
        }]);
    }
    while i < bytes.len() {
        if bytes[i] == b'"' {
            let mut out: Vec<u8> = Vec::new();
            i += 1;
            loop {
                if i >= bytes.len() {
                    return Err(ImportError::Io("unterminated quoted CSV field".into()));
                }
                if bytes[i] == b'"' {
                    if i + 1 < bytes.len() && bytes[i + 1] == b'"' {
                        out.push(b'"');
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                out.push(bytes[i]);
                i += 1;
            }
            let text = String::from_utf8(out).map_err(|_| ImportError::InvalidUtf8 { line: 0 })?;
            fields.push(CsvField { text, quoted: true });
            if i < bytes.len() {
                if bytes[i] != delim {
                    return Err(ImportError::Io(
                        "unexpected characters after quoted CSV field".into(),
                    ));
                }
                i += 1;
                if i == bytes.len() {
                    // Trailing delimiter → empty field.
                    fields.push(CsvField {
                        text: String::new(),
                        quoted: false,
                    });
                }
            }
        } else {
            let start = i;
            while i < bytes.len() && bytes[i] != delim {
                i += 1;
            }
            let text = std::str::from_utf8(&bytes[start..i])
                .map_err(|_| ImportError::InvalidUtf8 { line: 0 })?
                .to_string();
            fields.push(CsvField {
                text,
                quoted: false,
            });
            if i < bytes.len() {
                i += 1; // skip delim
                if i == bytes.len() {
                    fields.push(CsvField {
                        text: String::new(),
                        quoted: false,
                    });
                }
            }
        }
    }
    Ok(fields)
}

/// Streaming CSV importer over any [`Read`] source.
pub struct CsvImporter<R> {
    reader: BufReader<R>,
    options: CsvImportOptions,
    mapping: ColumnMapping,
    type_by_name: HashMap<String, String>,
    line: u64,
    headers: Vec<String>,
    buf: Vec<u8>,
    eof: bool,
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
    ///
    /// Strips a leading UTF-8 BOM so the first header is not `\u{feff}id` (IM-08).
    pub fn open_with_columns(
        reader: R,
        table_columns: &[ColumnMeta],
        options: CsvImportOptions,
    ) -> Result<Self, ImportError> {
        let mut reader = BufReader::new(reader);
        strip_leading_utf8_bom(&mut reader)?;
        let mut buf = Vec::new();
        let (headers, mapping, line) = if options.has_header {
            let (fields, lines) = match read_csv_record(&mut reader, options.delimiter, &mut buf) {
                Ok(Some(v)) => v,
                Ok(None) => {
                    return Err(ImportError::Mapping(
                        "CSV is empty (missing header row)".into(),
                    ));
                }
                Err(ImportError::InvalidUtf8 { .. }) => {
                    return Err(ImportError::InvalidUtf8 { line: 1 });
                }
                Err(err) => return Err(err),
            };
            let headers: Vec<String> = fields.iter().map(|f| f.text.clone()).collect();
            let mapping = ColumnMapping::from_headers(&headers, table_columns, &options)?;
            (headers, mapping, lines)
        } else {
            let mapping = ColumnMapping::positional(table_columns);
            let headers: Vec<String> = table_columns.iter().map(|c| c.name.clone()).collect();
            (headers, mapping, 0u64)
        };

        Ok(Self {
            reader,
            options,
            mapping,
            type_by_name: type_lookup(table_columns),
            line,
            headers,
            buf,
            eof: false,
        })
    }

    pub fn headers(&self) -> &[String] {
        &self.headers
    }

    pub fn mapping(&self) -> &ColumnMapping {
        &self.mapping
    }

    /// Replace the column mapping (dialog remaps, including when there is no header).
    ///
    /// Call after [`Self::open_with_columns`]. Does not re-read the file; the next
    /// [`Self::next_row`] uses `mapping` for field → column binding.
    pub fn with_mapping(mut self, mapping: ColumnMapping) -> Self {
        self.mapping = mapping;
        self
    }

    /// Next mapped row, or `None` at end of stream. Row errors include line numbers.
    /// Encoding failures ([`ImportError::InvalidUtf8`]) are returned as-is (IM-08).
    pub fn next_row(&mut self) -> Option<Result<ImportRow, ImportError>> {
        if self.eof {
            return None;
        }
        match read_csv_record(&mut self.reader, self.options.delimiter, &mut self.buf) {
            Ok(None) => {
                self.eof = true;
                None
            }
            Ok(Some((fields, lines))) => {
                self.line += lines;
                Some(
                    map_record_fields(
                        self.line,
                        &fields,
                        &self.mapping,
                        &self.type_by_name,
                        &self.options,
                    )
                    .map_err(ImportError::from),
                )
            }
            Err(ImportError::InvalidUtf8 { .. }) => {
                // Record that failed to parse starts on the next line after
                // whatever was already consumed (header / prior rows).
                Some(Err(ImportError::InvalidUtf8 {
                    line: self.line + 1,
                }))
            }
            Err(ImportError::Row { line, message }) => {
                Some(Err(ImportError::Row { line, message }))
            }
            Err(err) => {
                self.line += 1;
                Some(Err(ImportError::Row {
                    line: self.line,
                    message: err.to_string(),
                }))
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
///
/// Line is `0` when the failure is not tied to a CSV record yet.
pub fn require_utf8(bytes: &[u8]) -> Result<&str, ImportError> {
    std::str::from_utf8(bytes).map_err(|_| ImportError::InvalidUtf8 { line: 0 })
}

/// Read a UTF-8 CSV from any [`Read`], failing clearly on invalid encoding.
///
/// Test-only: materializes the whole input. Production import streams via
/// [`CsvImporter`] / [`import_csv_into`] instead (IM-07).
#[cfg(test)]
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
///
/// Text equal to the NULL sentinel is force-quoted so it stays distinct from NULL (EX-08).
pub fn value_to_csv_field(value: &Value, options: &CsvExportOptions) -> String {
    let null_repr = options.null_repr();
    match value {
        Value::Null if null_repr.is_empty() => String::new(),
        Value::Null => escape_csv_field_delim(null_repr, options.delimiter),
        Value::Bytes(b) => {
            escape_csv_field_delim(&encode_binary(b, options.binary), options.delimiter)
        }
        other => {
            let text = other.display().into_owned();
            let force_quote = !null_repr.is_empty() && text == null_repr;
            escape_csv_field_delim_ex(&text, options.delimiter, force_quote)
        }
    }
}

/// Convenience: wrap a string slice as a [`std::io::Cursor`] reader.
#[cfg(test)]
fn cursor_from_str(s: &str) -> io::Cursor<&[u8]> {
    io::Cursor::new(s.as_bytes())
}

/// BufReader helper for tests that inject CSV text.
#[cfg(test)]
#[allow(dead_code)]
fn buf_reader_from_str(s: &str) -> BufReader<io::Cursor<&[u8]>> {
    BufReader::new(cursor_from_str(s))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::{export_rows, CsvExportOptions, CsvNullEncoding, ExportFormat};
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
    fn im03_header_mapping_case_insensitive_and_skip_unknown() {
        let schema = books_schema();
        let cols = schema.columns("public", "books").unwrap();
        let ok = ColumnMapping::from_headers(
            &["ID".into(), "Title".into(), "extra".into()],
            &cols,
            &CsvImportOptions::default(),
        )
        .unwrap();
        assert_eq!(
            ok.targets,
            vec![Some("id".into()), Some("title".into()), None]
        );

        let err = ColumnMapping::from_headers(
            &["id".into(), "nope".into()],
            &cols,
            &CsvImportOptions {
                reject_unknown_headers: true,
                ..CsvImportOptions::default()
            },
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
        assert!(matches!(err, ImportError::Mapping(m) if m.contains("Map every required column")));
    }

    /// When `First row is header` is off, dialog remaps must still bind fields
    /// (positional defaults are wrong for swapped columns).
    #[test]
    fn im03_with_mapping_without_header() {
        let schema = books_schema();
        let cols = schema.columns("public", "books").unwrap();
        let opts = CsvImportOptions {
            has_header: false,
            required_columns: vec!["id".into()],
            ..CsvImportOptions::default()
        };
        // CSV order is title, id — opposite of table column order.
        let mut importer =
            CsvImporter::open_with_columns(cursor_from_str("Alpha,1\nBeta,2\n"), &cols, opts)
                .unwrap();
        importer = importer.with_mapping(ColumnMapping {
            targets: vec![Some("title".into()), Some("id".into())],
        });
        let row = importer.next_row().unwrap().unwrap();
        assert_eq!(row.values.get("title"), Some(&Value::text("Alpha")));
        assert_eq!(row.values.get("id"), Some(&Value::Int(1)));
    }

    /// Mapping override must go through the CSV reader (quoted newlines stay intact).
    #[test]
    fn im03_with_mapping_preserves_quoted_newlines() {
        let schema = books_schema();
        let cols = schema.columns("public", "books").unwrap();
        let opts = CsvImportOptions {
            has_header: true,
            required_columns: vec!["id".into()],
            ..CsvImportOptions::default()
        };
        let mut importer = CsvImporter::open_with_columns(
            cursor_from_str("ID,Title,note\n1,\"line1\nline2\",\n"),
            &cols,
            opts,
        )
        .unwrap();
        importer = importer.with_mapping(ColumnMapping {
            targets: vec![Some("id".into()), Some("title".into()), Some("note".into())],
        });
        let row = importer.next_row().unwrap().unwrap();
        assert_eq!(row.values.get("title"), Some(&Value::text("line1\nline2")));
    }

    #[test]
    fn im01_im06_stream_rows_and_type_error_with_line() {
        let schema = books_schema();
        // Empty note field → NULL under default empty_field_is_null.
        let csv = "id,title,note\n1,Alpha,\nx,Bad,\n";
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
        let csv = "id,title,note\n1,a,\n2,b,\n3,c,\n";
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
        assert_eq!(err.to_string(), IMPORT_CANCELLED_STATUS);
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
        let refused = ensure_import_allowed(SafeMode::ReadOnly, true).unwrap_err();
        assert_eq!(refused, ImportError::RefuseReadOnly);
        assert_eq!(refused.to_string(), IMPORT_REFUSED_READ_ONLY_STATUS);
        assert_eq!(IMPORT_CANCELLED_STATUS, "Import cancelled.");
    }

    #[test]
    fn im05_im07_count_csv_data_rows_prescan() {
        let csv = "id,title,note\n1,a,\n2,\"b\nline\",\n3,c,\n";
        let n = count_csv_data_rows(cursor_from_str(csv), &CsvImportOptions::default()).unwrap();
        assert_eq!(n, 3);
        let n_no_header = count_csv_data_rows(
            cursor_from_str("1,a,\n2,b,\n"),
            &CsvImportOptions {
                has_header: false,
                ..CsvImportOptions::default()
            },
        )
        .unwrap();
        assert_eq!(n_no_header, 2);
    }

    #[test]
    fn im08_invalid_utf8_fails_clearly() {
        let bad = b"id,title\n1,\xff\xfe\n";
        assert_eq!(
            require_utf8(bad).unwrap_err(),
            ImportError::InvalidUtf8 { line: 0 }
        );
        let err = read_utf8_source(&bad[..]).unwrap_err();
        assert_eq!(err, ImportError::InvalidUtf8 { line: 0 });
    }

    #[test]
    fn im08_utf8_and_bom_ok() {
        // BOM still in the byte stream — importer must strip before header map.
        let mut with_bom = Vec::from(&b"\xEF\xBB\xBF"[..]);
        with_bom.extend_from_slice(b"id,title,note\n1,a,\n");
        let schema = books_schema();
        let mut sink = CollectingImportSink::default();
        let n = import_csv_into(
            io::Cursor::new(with_bom),
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
        assert_eq!(sink.rows[0].values.get("id"), Some(&Value::Int(1)));
        assert_eq!(sink.rows[0].values.get("title"), Some(&Value::text("a")));
    }

    #[test]
    fn im02_im08_quoted_non_ascii_round_trip() {
        let schema = books_schema();
        let csv = "id,title,note\n1,\"café, ok\",\n";
        let mut sink = CollectingImportSink::default();
        let n = import_csv_into(
            cursor_from_str(csv),
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
        assert_eq!(
            sink.rows[0].values.get("title"),
            Some(&Value::text("café, ok"))
        );
    }

    #[test]
    fn im08_quoted_invalid_utf8_rejected() {
        // 1,"caf" + invalid UTF-8 + '",'  — quoted field must not Latin-1 decode.
        let mut csv = Vec::from(&b"id,title,note\n1,\""[..]);
        csv.extend_from_slice(b"caf\xFF\xFE");
        csv.extend_from_slice(b"\",,\n");
        let schema = books_schema();
        let mut sink = CollectingImportSink::default();
        let err = import_csv_into(
            io::Cursor::new(csv),
            &schema,
            "public",
            "books",
            CsvImportOptions::default(),
            &mut sink,
            &CancellationToken::new(),
            |_| {},
        )
        .unwrap_err();
        // Header is line 1; bad data row is line 2.
        assert_eq!(err, ImportError::InvalidUtf8 { line: 2 });
    }

    #[test]
    fn value_to_csv_field_quotes_null_sentinel_text() {
        let opts = CsvExportOptions {
            null_encoding: CsvNullEncoding::LiteralBackslashN,
            ..CsvExportOptions::default()
        };
        assert_eq!(value_to_csv_field(&Value::Null, &opts), "\\N");
        assert_eq!(value_to_csv_field(&Value::text("\\N"), &opts), "\"\\N\"");
        assert_eq!(value_to_csv_field(&Value::text("plain"), &opts), "plain");
    }

    #[test]
    fn im02_backslash_n_sentinel_only_when_unquoted() {
        let schema = books_schema();
        let export_opts = CsvExportOptions {
            null_encoding: CsvNullEncoding::LiteralBackslashN,
            ..CsvExportOptions::default()
        };
        let cols = vec!["id".into(), "title".into(), "note".into()];
        let rows = vec![
            vec![Value::Int(1), Value::text("a"), Value::Null],
            vec![Value::Int(2), Value::text("b"), Value::text("\\N")],
        ];
        let mut out = Vec::new();
        export_rows(
            &mut out,
            &cols,
            rows,
            &ExportFormat::Csv(export_opts.clone()),
            &CancellationToken::new(),
            |_| {},
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("\\N"));
        assert!(text.contains("\"\\N\""));

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
        assert_eq!(n, 2);
        assert_eq!(sink.rows[0].values.get("note"), Some(&Value::Null));
        assert_eq!(sink.rows[1].values.get("note"), Some(&Value::text("\\N")));
    }

    #[test]
    fn feeds_changeset_insert_path() {
        use crate::change_set::{ChangeSet, TableRef};
        let schema = books_schema();
        let csv = "id,title,note\n1,Hello,\n";
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
