//! Streaming export of query/table rows (M3-3 / US-12).
//!
//! Writers never buffer the full result: each row is formatted and flushed as
//! it arrives. Callers check [`CancellationToken`] between rows and report
//! progress via a callback or channel. Clipboard copy of a selection remains
//! in [`crate::copy_format`]; this module is for full-result / full-table export.

use std::io::{self, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::copy_format::escape_sql_string;
use crate::error::DbError;
use crate::session::DatabaseSession;
use crate::sql::{quote_ident, SqlError};
use crate::types::{ColumnMeta, Dialect, StreamBatch, StreamOutcome, Value, STREAM_BATCH_SIZE};

/// How binary (`BYTEA` / `BLOB`) cells are encoded in CSV and JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BinaryEncoding {
    /// Lowercase hex without a prefix (`deadbeef`).
    #[default]
    Hex,
    /// Standard Base64 (no newlines).
    Base64,
}

/// CSV export options (RFC 4180 quoting; configurable delimiter / header / NULL).
///
/// Default NULL encoding is `\N` (provisional per v0.3 EX-08). That stays distinct
/// from `''` (empty field / `""`) and from the four-character text `NULL`. An empty
/// [`Self::null_repr`] writes an unquoted empty field; configure the importer to match.
/// Clipboard copy still uses the keyword `NULL` via [`crate::copy_format`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CsvExportOptions {
    /// Field delimiter (default `,`).
    pub delimiter: u8,
    /// Write a header line of column names first.
    pub include_header: bool,
    /// Text written for SQL NULL (default `"\\N"`).
    pub null_repr: String,
    pub binary: BinaryEncoding,
}

impl Default for CsvExportOptions {
    fn default() -> Self {
        Self {
            delimiter: b',',
            include_header: true,
            null_repr: "\\N".into(),
            binary: BinaryEncoding::Hex,
        }
    }
}

/// JSON export shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum JsonExportMode {
    /// A single JSON array of objects (`[{…},{…}]`).
    #[default]
    Array,
    /// Newline-delimited JSON (one object per line).
    Ndjson,
}

/// JSON export options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonExportOptions {
    pub mode: JsonExportMode,
    pub binary: BinaryEncoding,
}

impl Default for JsonExportOptions {
    fn default() -> Self {
        Self {
            mode: JsonExportMode::Array,
            binary: BinaryEncoding::Hex,
        }
    }
}

/// Default row cap for one SQL `INSERT` statement ([`STREAM_BATCH_SIZE`] = 100).
pub const SQL_INSERT_DEFAULT_BATCH_ROWS: usize = STREAM_BATCH_SIZE;

/// Default byte budget for one SQL `INSERT` statement (~1 MiB of statement text).
pub const SQL_INSERT_DEFAULT_BATCH_BYTES: usize = 1024 * 1024;

/// SQL `INSERT` export options (dialect-correct quoting; batched statements).
///
/// A statement is flushed when **either** [`Self::batch_size`] rows accumulate
/// **or** the pending statement text would exceed [`Self::batch_max_bytes`]
/// (whichever comes first). Defaults: 100 rows / 1 MiB.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlInsertExportOptions {
    pub dialect: Dialect,
    /// `schema.table` or bare `table`.
    pub table: String,
    /// Max rows per `INSERT` statement (default [`SQL_INSERT_DEFAULT_BATCH_ROWS`]).
    pub batch_size: usize,
    /// Max bytes of statement text (prefix + VALUES) before flushing
    /// (default [`SQL_INSERT_DEFAULT_BATCH_BYTES`]).
    pub batch_max_bytes: usize,
}

impl SqlInsertExportOptions {
    pub fn new(dialect: Dialect, table: impl Into<String>) -> Self {
        Self {
            dialect,
            table: table.into(),
            batch_size: SQL_INSERT_DEFAULT_BATCH_ROWS,
            batch_max_bytes: SQL_INSERT_DEFAULT_BATCH_BYTES,
        }
    }
}

/// Failure while exporting.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExportError {
    #[error("{0}")]
    Io(String),
    #[error("export cancelled")]
    Cancelled,
    #[error("{0}")]
    Ident(String),
    #[error("Cannot export INSERT: column `{column}` has a decode error or unsupported type")]
    DecodeInInsert { column: String },
    #[error("Cannot export INSERT: column `{column}` has a non-finite number (NaN/Infinity)")]
    NonFiniteInInsert { column: String },
    #[error("{0}")]
    Query(String),
}

impl From<io::Error> for ExportError {
    fn from(err: io::Error) -> Self {
        Self::Io(err.to_string())
    }
}

impl From<SqlError> for ExportError {
    fn from(err: SqlError) -> Self {
        Self::Ident(err.to_string())
    }
}

impl From<DbError> for ExportError {
    fn from(err: DbError) -> Self {
        match err {
            DbError::Cancelled { .. } => Self::Cancelled,
            other => Self::Query(other.to_string()),
        }
    }
}

/// Escape one CSV field for `delimiter` (RFC 4180: quote when needed; `"` → `""`).
///
/// Empty strings are always quoted (`""`) so they stay distinct from an empty
/// NULL field when [`CsvExportOptions::null_repr`] is empty (EX-08).
pub fn escape_csv_field_delim(value: &str, delimiter: u8) -> String {
    let delim = delimiter as char;
    let needs_quotes = value.is_empty()
        || value.contains(delim)
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

/// Encode bytes for CSV/JSON text cells.
pub fn encode_binary(bytes: &[u8], encoding: BinaryEncoding) -> String {
    match encoding {
        BinaryEncoding::Hex => bytes.iter().map(|b| format!("{b:02x}")).collect(),
        BinaryEncoding::Base64 => base64_encode(bytes),
    }
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Format a cell for CSV (NULL → configured text; binary → hex/base64; else display).
pub fn format_csv_cell(value: &Value, null_repr: &str, binary: BinaryEncoding) -> String {
    match value {
        Value::Null => null_repr.to_string(),
        Value::Bytes(bytes) => encode_binary(bytes, binary),
        other => other.display().into_owned(),
    }
}

/// Format a cell as a JSON value (typed where possible; binary as a string).
pub fn format_json_cell(value: &Value, binary: BinaryEncoding) -> serde_json::Value {
    match value {
        Value::Null => serde_json::Value::Null,
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Int(n) => serde_json::Value::Number((*n).into()),
        Value::UInt(n) => serde_json::Value::Number((*n).into()),
        Value::Float(f) => serde_json::Number::from_f64(*f)
            .map(serde_json::Value::Number)
            .unwrap_or_else(|| serde_json::Value::String(value.display().into_owned())),
        Value::Decimal(d) => {
            let text = d.normalize().to_string();
            text.parse::<serde_json::Number>()
                .map(serde_json::Value::Number)
                .unwrap_or_else(|_| serde_json::Value::String(text))
        }
        Value::Bytes(bytes) => serde_json::Value::String(encode_binary(bytes, binary)),
        Value::Json(text) => serde_json::from_str(text)
            .unwrap_or_else(|_| serde_json::Value::String(text.to_string())),
        other => serde_json::Value::String(other.display().into_owned()),
    }
}

/// Build one JSON object with keys in `columns` order (stable across builds).
fn format_json_object_row(
    columns: &[String],
    row: &[Value],
    binary: BinaryEncoding,
) -> Result<String, ExportError> {
    let mut out = String::from('{');
    for (i, key) in columns.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let key_json =
            serde_json::to_string(key).map_err(|err| ExportError::Io(err.to_string()))?;
        out.push_str(&key_json);
        out.push(':');
        let cell = row.get(i).unwrap_or(&Value::Null);
        let value_json = serde_json::to_string(&format_json_cell(cell, binary))
            .map_err(|err| ExportError::Io(err.to_string()))?;
        out.push_str(&value_json);
    }
    out.push('}');
    Ok(out)
}

/// Dialect-correct SQL literal for one value (full binary; never truncated).
pub fn value_to_export_sql_literal(value: &Value, dialect: Dialect) -> Result<String, ExportError> {
    match value {
        Value::Null => Ok("NULL".into()),
        Value::Bool(b) => Ok(b.to_string()),
        Value::Int(n) => Ok(n.to_string()),
        Value::UInt(n) => Ok(n.to_string()),
        Value::Float(f) => {
            if f.is_finite() {
                Ok(format_finite_float(*f))
            } else {
                match dialect {
                    Dialect::Postgres => Ok(postgres_non_finite_literal(*f).into()),
                    Dialect::Mysql | Dialect::Sqlite => Err(ExportError::NonFiniteInInsert {
                        column: String::new(),
                    }),
                }
            }
        }
        Value::Decimal(d) => Ok(d.normalize().to_string()),
        Value::Bytes(bytes) => Ok(bytes_to_sql_literal(bytes, dialect)),
        Value::Text(t) | Value::Json(t) | Value::Other { text: t, .. } => {
            if value.is_decode_error() || value.is_unsupported_placeholder() {
                return Err(ExportError::DecodeInInsert {
                    column: String::new(),
                });
            }
            Ok(escape_sql_string(t, dialect))
        }
        Value::Uuid(u) => Ok(escape_sql_string(&u.to_string(), dialect)),
        Value::Date(_)
        | Value::Time(_)
        | Value::Timestamp(_)
        | Value::TimestampTz(_)
        | Value::Interval(_) => Ok(escape_sql_string(&value.display(), dialect)),
    }
}

fn format_finite_float(value: f64) -> String {
    let text = format!("{value}");
    if text.contains('.') {
        text
    } else {
        format!("{value:.1}")
    }
}

fn postgres_non_finite_literal(value: f64) -> &'static str {
    if value.is_nan() {
        "'NaN'"
    } else if value.is_sign_negative() {
        "'-Infinity'"
    } else {
        "'Infinity'"
    }
}

fn bytes_to_sql_literal(bytes: &[u8], dialect: Dialect) -> String {
    let hex: String = bytes.iter().map(|b| format!("{b:02X}")).collect();
    match dialect {
        Dialect::Postgres => format!("'\\x{hex}'"),
        Dialect::Mysql | Dialect::Sqlite => format!("X'{hex}'"),
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

fn column_names(columns: &[ColumnMeta]) -> Vec<String> {
    columns.iter().map(|c| c.name.clone()).collect()
}

fn check_cancel(cancel: &CancellationToken) -> Result<(), ExportError> {
    if cancel.is_cancelled() {
        Err(ExportError::Cancelled)
    } else {
        Ok(())
    }
}

/// Streaming CSV writer.
pub struct CsvExporter<W: Write> {
    writer: W,
    options: CsvExportOptions,
    rows_written: u64,
    header_done: bool,
}

impl<W: Write> CsvExporter<W> {
    pub fn new(writer: W, options: CsvExportOptions) -> Self {
        Self {
            writer,
            options,
            rows_written: 0,
            header_done: false,
        }
    }

    pub fn rows_written(&self) -> u64 {
        self.rows_written
    }

    pub fn write_header(&mut self, columns: &[String]) -> Result<(), ExportError> {
        if !self.options.include_header || self.header_done {
            self.header_done = true;
            return Ok(());
        }
        self.write_line(columns.iter().map(|s| s.as_str()))?;
        self.header_done = true;
        Ok(())
    }

    pub fn write_row(&mut self, row: &[Value]) -> Result<(), ExportError> {
        self.write_value_row(row)?;
        self.rows_written += 1;
        Ok(())
    }

    fn write_line<'a>(&mut self, fields: impl Iterator<Item = &'a str>) -> Result<(), ExportError> {
        let delim = self.options.delimiter as char;
        for (i, field) in fields.enumerate() {
            if i > 0 {
                write!(self.writer, "{delim}")?;
            }
            // Empty null_repr must stay an unquoted empty field (EX-08), so do not
            // force-quote empty strings when they are the NULL sentinel itself.
            if field.is_empty() && self.options.null_repr.is_empty() {
                // write nothing between delimiters
            } else {
                write!(
                    self.writer,
                    "{}",
                    escape_csv_field_delim(field, self.options.delimiter)
                )?;
            }
        }
        self.writer.write_all(b"\n")?;
        Ok(())
    }

    fn write_value_row(&mut self, row: &[Value]) -> Result<(), ExportError> {
        let delim = self.options.delimiter as char;
        for (i, value) in row.iter().enumerate() {
            if i > 0 {
                write!(self.writer, "{delim}")?;
            }
            match value {
                Value::Null if self.options.null_repr.is_empty() => {
                    // Unquoted empty field.
                }
                Value::Null => {
                    write!(
                        self.writer,
                        "{}",
                        escape_csv_field_delim(&self.options.null_repr, self.options.delimiter)
                    )?;
                }
                Value::Bytes(bytes) => {
                    let encoded = encode_binary(bytes, self.options.binary);
                    write!(
                        self.writer,
                        "{}",
                        escape_csv_field_delim(&encoded, self.options.delimiter)
                    )?;
                }
                other => {
                    let text = other.display().into_owned();
                    write!(
                        self.writer,
                        "{}",
                        escape_csv_field_delim(&text, self.options.delimiter)
                    )?;
                }
            }
        }
        self.writer.write_all(b"\n")?;
        Ok(())
    }

    pub fn into_inner(self) -> W {
        self.writer
    }
}

/// Streaming JSON writer (array or NDJSON).
pub struct JsonExporter<W: Write> {
    writer: W,
    options: JsonExportOptions,
    columns: Vec<String>,
    rows_written: u64,
    started: bool,
    finished: bool,
}

impl<W: Write> JsonExporter<W> {
    pub fn new(writer: W, options: JsonExportOptions) -> Self {
        Self {
            writer,
            options,
            columns: Vec::new(),
            rows_written: 0,
            started: false,
            finished: false,
        }
    }

    pub fn rows_written(&self) -> u64 {
        self.rows_written
    }

    pub fn write_header(&mut self, columns: &[String]) -> Result<(), ExportError> {
        self.columns = columns.to_vec();
        if matches!(self.options.mode, JsonExportMode::Array) && !self.started {
            self.writer.write_all(b"[\n")?;
            self.started = true;
        }
        Ok(())
    }

    pub fn write_row(&mut self, row: &[Value]) -> Result<(), ExportError> {
        if matches!(self.options.mode, JsonExportMode::Array) && !self.started {
            self.writer.write_all(b"[\n")?;
            self.started = true;
        }
        // Emit keys in column order. Do not use `serde_json::Map`: workspace
        // features may enable `preserve_order`, which would make key order
        // differ across `cargo test -p sela-core` vs `cargo test --workspace`.
        let text = format_json_object_row(&self.columns, row, self.options.binary)?;
        match self.options.mode {
            JsonExportMode::Array => {
                if self.rows_written > 0 {
                    self.writer.write_all(b",\n")?;
                }
                write!(self.writer, "  {text}")?;
            }
            JsonExportMode::Ndjson => {
                self.writer.write_all(text.as_bytes())?;
                self.writer.write_all(b"\n")?;
            }
        }
        self.rows_written += 1;
        Ok(())
    }

    /// Close a JSON array (no-op for NDJSON). Call after the last row.
    pub fn finish(&mut self) -> Result<(), ExportError> {
        if self.finished {
            return Ok(());
        }
        if matches!(self.options.mode, JsonExportMode::Array) {
            if !self.started {
                self.writer.write_all(b"[]\n")?;
            } else {
                self.writer.write_all(b"\n]\n")?;
            }
        }
        self.finished = true;
        Ok(())
    }

    pub fn into_inner(mut self) -> Result<W, ExportError> {
        self.finish()?;
        Ok(self.writer)
    }
}

/// Streaming batched `INSERT` writer.
pub struct SqlInsertExporter<W: Write> {
    writer: W,
    options: SqlInsertExportOptions,
    quoted_table: String,
    quoted_columns: Vec<String>,
    /// Cached `INSERT INTO … (cols) VALUES` prefix length for byte budgeting.
    prefix_len: usize,
    pending: Vec<String>,
    /// Bytes of VALUES rows already pending (including commas/newlines between them).
    pending_values_bytes: usize,
    rows_written: u64,
    header_done: bool,
}

impl<W: Write> SqlInsertExporter<W> {
    pub fn new(writer: W, options: SqlInsertExportOptions) -> Result<Self, ExportError> {
        let quoted_table = quote_table_ref(options.dialect, &options.table)?;
        Ok(Self {
            writer,
            options,
            quoted_table,
            quoted_columns: Vec::new(),
            prefix_len: 0,
            pending: Vec::new(),
            pending_values_bytes: 0,
            rows_written: 0,
            header_done: false,
        })
    }

    pub fn rows_written(&self) -> u64 {
        self.rows_written
    }

    pub fn write_header(&mut self, columns: &[String]) -> Result<(), ExportError> {
        let mut quoted = Vec::with_capacity(columns.len());
        for name in columns {
            quoted.push(quote_ident(self.options.dialect, name)?);
        }
        self.quoted_columns = quoted;
        // "INSERT INTO {table} ({cols}) VALUES\n"
        self.prefix_len = "INSERT INTO ".len()
            + self.quoted_table.len()
            + " (".len()
            + self.quoted_columns.join(", ").len()
            + ") VALUES\n".len();
        self.header_done = true;
        Ok(())
    }

    pub fn write_row(&mut self, row: &[Value]) -> Result<(), ExportError> {
        if !self.header_done {
            return Err(ExportError::Io(
                "SQL INSERT export requires write_header before write_row".into(),
            ));
        }
        let mut literals = Vec::with_capacity(self.quoted_columns.len());
        for (i, _) in self.quoted_columns.iter().enumerate() {
            let cell = row.get(i).unwrap_or(&Value::Null);
            match value_to_export_sql_literal(cell, self.options.dialect) {
                Ok(lit) => literals.push(lit),
                Err(ExportError::DecodeInInsert { .. }) => {
                    let column = self
                        .quoted_columns
                        .get(i)
                        .cloned()
                        .unwrap_or_else(|| format!("column {i}"));
                    return Err(ExportError::DecodeInInsert { column });
                }
                Err(ExportError::NonFiniteInInsert { .. }) => {
                    let column = self
                        .quoted_columns
                        .get(i)
                        .cloned()
                        .unwrap_or_else(|| format!("column {i}"));
                    return Err(ExportError::NonFiniteInInsert { column });
                }
                Err(other) => return Err(other),
            }
        }
        let row_sql = format!("({})", literals.join(", "));
        let max_rows = self.options.batch_size.max(1);
        let max_bytes = self.options.batch_max_bytes.max(1);

        // Bytes if we append this row: previous values + optional ",\n" + row + trailing ";\n".
        let sep = if self.pending.is_empty() { 0 } else { 2 }; // ",\n"
        let projected = self.prefix_len + self.pending_values_bytes + sep + row_sql.len() + 2;

        // Flush first when the new row would exceed the byte budget and we already
        // have at least one pending row. A single oversized row still ships alone.
        if !self.pending.is_empty() && projected > max_bytes {
            self.flush_batch()?;
        }

        if self.pending.is_empty() {
            self.pending_values_bytes = row_sql.len();
        } else {
            self.pending_values_bytes += 2 + row_sql.len(); // ",\n" + row
        }
        self.pending.push(row_sql);
        self.rows_written += 1;

        if self.pending.len() >= max_rows {
            self.flush_batch()?;
        } else {
            // Also flush when the pending statement (with trailing `;\n`) hits the
            // byte cap even without adding another row.
            let current = self.prefix_len + self.pending_values_bytes + 2;
            if current >= max_bytes {
                self.flush_batch()?;
            }
        }
        Ok(())
    }

    fn flush_batch(&mut self) -> Result<(), ExportError> {
        if self.pending.is_empty() {
            return Ok(());
        }
        writeln!(
            self.writer,
            "INSERT INTO {} ({}) VALUES",
            self.quoted_table,
            self.quoted_columns.join(", ")
        )?;
        for (i, row) in self.pending.iter().enumerate() {
            if i > 0 {
                self.writer.write_all(b",\n")?;
            }
            self.writer.write_all(row.as_bytes())?;
        }
        self.writer.write_all(b";\n")?;
        self.pending.clear();
        self.pending_values_bytes = 0;
        Ok(())
    }

    pub fn finish(&mut self) -> Result<(), ExportError> {
        self.flush_batch()
    }

    pub fn into_inner(mut self) -> Result<W, ExportError> {
        self.finish()?;
        Ok(self.writer)
    }
}

/// Unified export format for the streaming helpers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportFormat {
    Csv(CsvExportOptions),
    Json(JsonExportOptions),
    SqlInsert(SqlInsertExportOptions),
}

/// Write rows from an iterator into `writer` without buffering the whole set.
///
/// Checks `cancel` before each row. Invokes `on_progress` with the running
/// row count after every successful row write.
pub fn export_rows<W, I, F>(
    writer: &mut W,
    columns: &[String],
    rows: I,
    format: &ExportFormat,
    cancel: &CancellationToken,
    mut on_progress: F,
) -> Result<u64, ExportError>
where
    W: Write,
    I: IntoIterator<Item = Vec<Value>>,
    F: FnMut(u64),
{
    check_cancel(cancel)?;
    match format {
        ExportFormat::Csv(opts) => {
            let mut exp = CsvExporter::new(writer, opts.clone());
            exp.write_header(columns)?;
            for row in rows {
                check_cancel(cancel)?;
                exp.write_row(&row)?;
                on_progress(exp.rows_written());
            }
            Ok(exp.rows_written())
        }
        ExportFormat::Json(opts) => {
            let mut exp = JsonExporter::new(&mut *writer, opts.clone());
            exp.write_header(columns)?;
            for row in rows {
                check_cancel(cancel)?;
                exp.write_row(&row)?;
                on_progress(exp.rows_written());
            }
            exp.finish()?;
            Ok(exp.rows_written())
        }
        ExportFormat::SqlInsert(opts) => {
            let mut exp = SqlInsertExporter::new(&mut *writer, opts.clone())?;
            exp.write_header(columns)?;
            for row in rows {
                check_cancel(cancel)?;
                exp.write_row(&row)?;
                on_progress(exp.rows_written());
            }
            exp.finish()?;
            Ok(exp.rows_written())
        }
    }
}

/// Stream a query through a session into an export writer (minimal driver glue).
///
/// Uses [`DatabaseSession::stream_query`] with no row cap so the whole result
/// can be exported. Progress is the number of data rows written so far.
#[allow(clippy::too_many_arguments)]
pub async fn export_query<W, F>(
    session: &dyn DatabaseSession,
    sql: &str,
    params: &[Value],
    writer: &mut W,
    format: &ExportFormat,
    cancel: CancellationToken,
    mut on_progress: F,
) -> Result<(u64, StreamOutcome), ExportError>
where
    W: Write,
    F: FnMut(u64),
{
    check_cancel(&cancel)?;
    let (tx, rx) = async_channel::bounded::<StreamBatch>(4);
    let stream = session.stream_query(
        sql,
        params,
        STREAM_BATCH_SIZE,
        None,
        true,
        cancel.clone(),
        tx,
    );

    // Drive export on the consumer side while the producer fills the channel.
    let export = async {
        let mut columns: Vec<String> = Vec::new();
        let mut header_written = false;
        let mut rows_written = 0u64;

        // Local exporters holding &mut writer — we branch once after first columns.
        enum Active<'a, W: Write> {
            Csv(CsvExporter<&'a mut W>),
            Json(JsonExporter<&'a mut W>),
            Sql(SqlInsertExporter<&'a mut W>),
        }

        let mut active: Option<Active<'_, W>> = None;

        while let Ok(batch) = rx.recv().await {
            check_cancel(&cancel)?;
            if let Some(cols) = batch.columns {
                columns = column_names(&cols);
            }
            if !header_written && !columns.is_empty() {
                active = Some(match format {
                    ExportFormat::Csv(opts) => {
                        let mut exp = CsvExporter::new(&mut *writer, opts.clone());
                        exp.write_header(&columns)?;
                        Active::Csv(exp)
                    }
                    ExportFormat::Json(opts) => {
                        let mut exp = JsonExporter::new(&mut *writer, opts.clone());
                        exp.write_header(&columns)?;
                        Active::Json(exp)
                    }
                    ExportFormat::SqlInsert(opts) => {
                        let mut exp = SqlInsertExporter::new(&mut *writer, opts.clone())?;
                        exp.write_header(&columns)?;
                        Active::Sql(exp)
                    }
                });
                header_written = true;
            }
            let Some(exp) = active.as_mut() else {
                continue;
            };
            for row in batch.rows {
                check_cancel(&cancel)?;
                match exp {
                    Active::Csv(e) => e.write_row(&row)?,
                    Active::Json(e) => e.write_row(&row)?,
                    Active::Sql(e) => e.write_row(&row)?,
                }
                rows_written += 1;
                on_progress(rows_written);
            }
        }

        if let Some(exp) = active.as_mut() {
            match exp {
                Active::Csv(_) => {}
                Active::Json(e) => e.finish()?,
                Active::Sql(e) => e.finish()?,
            }
        } else if matches!(
            format,
            ExportFormat::Json(JsonExportOptions {
                mode: JsonExportMode::Array,
                ..
            })
        ) {
            // Empty result → `[]`.
            writer.write_all(b"[]\n")?;
        }

        Ok::<u64, ExportError>(rows_written)
    };

    let (outcome, rows) = tokio::join!(stream, export);
    let outcome = outcome?;
    let rows = rows?;
    Ok((rows, outcome))
}

/// Helper used by tests: export with a live atomic progress counter.
pub fn export_rows_with_progress_counter<W, I>(
    writer: &mut W,
    columns: &[String],
    rows: I,
    format: &ExportFormat,
    cancel: &CancellationToken,
    progress: &Arc<AtomicUsize>,
) -> Result<u64, ExportError>
where
    W: Write,
    I: IntoIterator<Item = Vec<Value>>,
{
    export_rows(writer, columns, rows, format, cancel, |n| {
        progress.store(n as usize, Ordering::Relaxed);
    })
}

#[cfg(test)]
mod tests {
    //! Unit coverage for v0.3 plan IDs EX-01..EX-11 and EX-06 cancel.
    //! Driver / headless UI layers for those IDs are out of this core-only PR.
    use super::*;
    use std::sync::Arc;

    fn sample_columns() -> Vec<String> {
        vec!["id".into(), "name".into(), "note".into()]
    }

    fn sample_rows() -> Vec<Vec<Value>> {
        vec![
            vec![Value::Int(1), Value::text("a,b"), Value::Null],
            vec![
                Value::Int(2),
                Value::text("say \"hi\""),
                Value::text("NULL"),
            ],
            vec![
                Value::Int(3),
                Value::text("unicöde"),
                Value::Bytes(vec![0xde, 0xad]),
            ],
        ]
    }

    /// EX-01 / EX-10: stream every row into an injectable `Write` with progress.
    #[test]
    fn ex01_ex10_csv_streams_all_rows_with_progress() {
        let cancel = CancellationToken::new();
        let progress = Arc::new(AtomicUsize::new(0));
        let mut out = Vec::new();
        let n = export_rows_with_progress_counter(
            &mut out,
            &sample_columns(),
            sample_rows(),
            &ExportFormat::Csv(CsvExportOptions::default()),
            &cancel,
            &progress,
        )
        .unwrap();
        assert_eq!(n, 3);
        assert_eq!(progress.load(Ordering::Relaxed), 3);
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("id,name,note\n"));
        assert!(text.contains("\"a,b\""));
        assert!(text.contains("dead"));
    }

    #[test]
    fn csv_custom_null_and_delimiter() {
        let cancel = CancellationToken::new();
        let mut out = Vec::new();
        let opts = CsvExportOptions {
            delimiter: b';',
            include_header: false,
            null_repr: String::new(),
            binary: BinaryEncoding::Hex,
        };
        export_rows(
            &mut out,
            &sample_columns(),
            vec![vec![Value::Int(1), Value::text("x"), Value::Null]],
            &ExportFormat::Csv(opts),
            &cancel,
            |_| {},
        )
        .unwrap();
        // null_repr "" → unquoted empty field (not `""`).
        assert_eq!(String::from_utf8(out).unwrap(), "1;x;\n");
    }

    /// EX-02: JSON array; SQL NULL → JSON null (not the string "NULL").
    #[test]
    fn ex02_json_array_and_ndjson_null() {
        let cancel = CancellationToken::new();
        let cols = vec!["id".into(), "ok".into(), "note".into()];
        let rows = vec![
            vec![Value::Int(1), Value::Bool(true), Value::Null],
            vec![Value::Int(2), Value::Null, Value::text("NULL")],
        ];

        let mut arr = Vec::new();
        export_rows(
            &mut arr,
            &cols,
            rows.clone(),
            &ExportFormat::Json(JsonExportOptions::default()),
            &cancel,
            |_| {},
        )
        .unwrap();
        let arr_text = String::from_utf8(arr).unwrap();
        assert!(arr_text.starts_with("[\n"));
        assert!(arr_text.trim_end().ends_with(']'));
        assert!(arr_text.contains("\"ok\":true"));
        assert!(arr_text.contains("\"ok\":null"));
        assert!(arr_text.contains("\"note\":null"));
        assert!(arr_text.contains("\"note\":\"NULL\""));

        let mut nd = Vec::new();
        export_rows(
            &mut nd,
            &cols,
            rows,
            &ExportFormat::Json(JsonExportOptions {
                mode: JsonExportMode::Ndjson,
                binary: BinaryEncoding::Hex,
            }),
            &cancel,
            |_| {},
        )
        .unwrap();
        let nd_text = String::from_utf8(nd).unwrap();
        assert_eq!(nd_text.lines().count(), 2);
        assert!(!nd_text.contains('['));
    }

    /// EX-03: SQL INSERT batches, quoting, dialect literals.
    #[test]
    fn ex03_sql_insert_batches_and_quotes() {
        let cancel = CancellationToken::new();
        let cols = vec!["id".into(), "name".into()];
        let rows = vec![
            vec![Value::Int(1), Value::text("O'Reilly")],
            vec![Value::Int(2), Value::Null],
            vec![Value::Int(3), Value::text("c")],
        ];
        let mut out = Vec::new();
        export_rows(
            &mut out,
            &cols,
            rows,
            &ExportFormat::SqlInsert(SqlInsertExportOptions {
                dialect: Dialect::Postgres,
                table: "public.items".into(),
                batch_size: 2,
                batch_max_bytes: SQL_INSERT_DEFAULT_BATCH_BYTES,
            }),
            &cancel,
            |_| {},
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("INSERT INTO \"public\".\"items\""));
        assert!(text.contains("'O''Reilly'"));
        assert_eq!(text.matches("INSERT INTO").count(), 2);
        assert!(text.contains("NULL"));
    }

    #[test]
    fn ex03_sql_mysql_bytes_and_backslash() {
        let cancel = CancellationToken::new();
        let cols = vec!["b".into(), "t".into()];
        let rows = vec![vec![Value::Bytes(vec![0x00, 0xff]), Value::text("a\\b")]];
        let mut out = Vec::new();
        export_rows(
            &mut out,
            &cols,
            rows,
            &ExportFormat::SqlInsert(SqlInsertExportOptions::new(Dialect::Mysql, "t")),
            &cancel,
            |_| {},
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("X'00FF'"));
        assert!(text.contains("'a\\\\b'"));
    }

    /// EX-06: cancel before any row / mid-stream.
    #[test]
    fn ex06_cancel_before_rows() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let mut out = Vec::new();
        let err = export_rows(
            &mut out,
            &sample_columns(),
            sample_rows(),
            &ExportFormat::Csv(CsvExportOptions::default()),
            &cancel,
            |_| {},
        )
        .unwrap_err();
        assert_eq!(err, ExportError::Cancelled);
        assert!(out.is_empty());
    }

    #[test]
    fn ex06_cancel_mid_stream_reports_partial_progress() {
        let cancel = CancellationToken::new();
        let progress = Arc::new(AtomicUsize::new(0));
        let mut out = Vec::new();
        let cols = sample_columns();
        let rows = sample_rows();
        let mut first = true;
        let err = export_rows(
            &mut out,
            &cols,
            rows.into_iter().inspect(|_| {
                if first {
                    first = false;
                } else {
                    cancel.cancel();
                }
            }),
            &ExportFormat::Csv(CsvExportOptions::default()),
            &cancel,
            |n| progress.store(n as usize, Ordering::Relaxed),
        )
        .unwrap_err();
        assert_eq!(err, ExportError::Cancelled);
        assert_eq!(progress.load(Ordering::Relaxed), 1);
        // Partial bytes may exist; success was not returned.
        assert!(!out.is_empty());
    }

    /// EX-07: comma, quote, newline, CR inside fields.
    #[test]
    fn ex07_csv_quoting_special_chars() {
        let cancel = CancellationToken::new();
        let mut out = Vec::new();
        export_rows(
            &mut out,
            &["c".into()],
            vec![
                vec![Value::text("a,b")],
                vec![Value::text("say \"hi\"")],
                vec![Value::text("line1\nline2")],
                vec![Value::text("cr\ronly")],
            ],
            &ExportFormat::Csv(CsvExportOptions::default()),
            &cancel,
            |_| {},
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("\"a,b\""));
        assert!(text.contains("\"say \"\"hi\"\"\""));
        assert!(text.contains("\"line1\nline2\""));
        assert!(text.contains("\"cr\ronly\""));
    }

    /// EX-08: NULL ≠ '' in CSV, JSON, INSERT.
    #[test]
    fn ex08_null_distinct_from_empty_string() {
        let cancel = CancellationToken::new();
        let cols = vec!["n".into()];
        let rows = vec![vec![Value::Null], vec![Value::text("")]];

        let mut csv = Vec::new();
        export_rows(
            &mut csv,
            &cols,
            rows.clone(),
            &ExportFormat::Csv(CsvExportOptions::default()),
            &cancel,
            |_| {},
        )
        .unwrap();
        // NULL → `\N`; '' → quoted empty `""`.
        assert_eq!(String::from_utf8(csv).unwrap(), "n\n\\N\n\"\"\n");

        let mut json = Vec::new();
        export_rows(
            &mut json,
            &cols,
            rows.clone(),
            &ExportFormat::Json(JsonExportOptions::default()),
            &cancel,
            |_| {},
        )
        .unwrap();
        let j = String::from_utf8(json).unwrap();
        assert!(j.contains("\"n\":null"));
        assert!(j.contains("\"n\":\"\""));

        let mut sql = Vec::new();
        export_rows(
            &mut sql,
            &cols,
            rows,
            &ExportFormat::SqlInsert(SqlInsertExportOptions::new(Dialect::Sqlite, "t")),
            &cancel,
            |_| {},
        )
        .unwrap();
        let s = String::from_utf8(sql).unwrap();
        assert!(s.contains("(NULL)"));
        assert!(s.contains("('')"));
    }

    /// EX-09: UTF-8 write (provisional default).
    #[test]
    fn ex09_utf8_write() {
        let cancel = CancellationToken::new();
        let mut out = Vec::new();
        export_rows(
            &mut out,
            &["t".into()],
            vec![vec![Value::text("café 日本語")]],
            &ExportFormat::Csv(CsvExportOptions::default()),
            &cancel,
            |_| {},
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("café 日本語"));
    }

    /// EX-11: INSERT refuses decode; CSV/JSON keep display text (not NULL).
    #[test]
    fn ex11_decode_refused_in_insert_kept_in_csv_json() {
        let cancel = CancellationToken::new();
        let cols = vec!["c".into()];
        let rows = vec![vec![Value::decode_error("bytea")]];

        let err = export_rows(
            &mut Vec::new(),
            &cols,
            rows.clone(),
            &ExportFormat::SqlInsert(SqlInsertExportOptions::new(Dialect::Postgres, "t")),
            &cancel,
            |_| {},
        )
        .unwrap_err();
        assert!(matches!(err, ExportError::DecodeInInsert { .. }));

        let mut csv = Vec::new();
        export_rows(
            &mut csv,
            &cols,
            rows.clone(),
            &ExportFormat::Csv(CsvExportOptions::default()),
            &cancel,
            |_| {},
        )
        .unwrap();
        let csv_text = String::from_utf8(csv).unwrap();
        assert!(csv_text.contains("decode error"));
        assert!(!csv_text.lines().nth(1).unwrap().eq("NULL"));

        let mut json = Vec::new();
        export_rows(
            &mut json,
            &cols,
            rows,
            &ExportFormat::Json(JsonExportOptions::default()),
            &cancel,
            |_| {},
        )
        .unwrap();
        let j = String::from_utf8(json).unwrap();
        assert!(j.contains("decode error"));
        assert!(!j.contains(":null"));
    }

    #[test]
    fn ex03_sql_refuses_non_finite_on_mysql() {
        let cancel = CancellationToken::new();
        let err = export_rows(
            &mut Vec::new(),
            &["x".into()],
            vec![vec![Value::Float(f64::NAN)]],
            &ExportFormat::SqlInsert(SqlInsertExportOptions::new(Dialect::Mysql, "t")),
            &cancel,
            |_| {},
        )
        .unwrap_err();
        assert!(matches!(err, ExportError::NonFiniteInInsert { .. }));
    }

    #[test]
    fn ex03_postgres_quotes_non_finite() {
        let cancel = CancellationToken::new();
        let mut out = Vec::new();
        export_rows(
            &mut out,
            &["x".into()],
            vec![vec![Value::Float(f64::INFINITY)]],
            &ExportFormat::SqlInsert(SqlInsertExportOptions::new(Dialect::Postgres, "t")),
            &cancel,
            |_| {},
        )
        .unwrap();
        assert!(String::from_utf8(out).unwrap().contains("'Infinity'"));
    }
}
