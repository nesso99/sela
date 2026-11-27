//! Streaming CSV import sink (IM-07, v0.7.0).
//!
//! Rows flow `CsvImporter` → one `INSERT` per row → bounded channel →
//! [`DatabaseSession::apply_statement_stream`] (single transaction). Memory use
//! is bounded by [`IMPORT_STREAM_CAPACITY`] statements, not by file size.
//!
//! Rollback rules match the buffered path: the transaction commits only after
//! the producer sends [`PlanStreamItem::End`]. A parse error, cancel, or a
//! database error (e.g. duplicate key) drops the transaction, so nothing is
//! written.

use std::collections::BTreeMap;
use std::io::Read;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::change_plan::{ChangePlan, PlanOp, PlannedStatement};
use crate::change_set::{ChangeSet, TableRef};
use crate::csv_import::{CsvImporter, ImportError};
use crate::session::DatabaseSession;
use crate::sql::{bind_placeholder, qualify, quote_ident};
use crate::structure::TableStructure;
use crate::types::{Dialect, Value};

/// Max `INSERT` statements buffered between the CSV reader and the database.
pub const IMPORT_STREAM_CAPACITY: usize = 64;

/// One message on a statement stream.
#[derive(Debug, Clone, PartialEq)]
pub enum PlanStreamItem {
    Statement(PlannedStatement),
    /// Producer finished cleanly: the sink may commit. A channel that closes
    /// without this marker is rolled back.
    End,
}

/// Turns one mapped CSV row into an `INSERT` statement.
pub enum ImportStatementBuilder {
    /// Table with a primary key (same SQL as a queued grid insert).
    ChangeSet(ChangeSet),
    /// Table without a primary key: insert exactly the mapped columns.
    InsertOnly { schema: String, table: String },
}

impl ImportStatementBuilder {
    pub fn for_structure(structure: &TableStructure) -> Self {
        match ChangeSet::from_structure(structure) {
            Ok(cs) => Self::ChangeSet(cs),
            Err(_) => Self::InsertOnly {
                schema: structure.schema.clone(),
                table: structure.table.clone(),
            },
        }
    }

    pub fn table(&self) -> TableRef {
        match self {
            Self::ChangeSet(cs) => cs.table().clone(),
            Self::InsertOnly { schema, table } => TableRef::new(schema.clone(), table.clone()),
        }
    }

    /// `None` when the row has no values and the table has no primary key.
    pub fn statement(
        &self,
        dialect: Dialect,
        values: &BTreeMap<String, Value>,
    ) -> Result<Option<PlannedStatement>, ImportError> {
        match self {
            Self::ChangeSet(cs) => ChangePlan::insert_statement(cs, dialect, values)
                .map(Some)
                .map_err(|e| ImportError::Sink(e.to_string())),
            Self::InsertOnly { schema, table } => {
                insert_only_statement(dialect, schema, table, values)
            }
        }
    }
}

fn insert_only_statement(
    dialect: Dialect,
    schema: &str,
    table: &str,
    values: &BTreeMap<String, Value>,
) -> Result<Option<PlannedStatement>, ImportError> {
    if values.is_empty() {
        return Ok(None);
    }
    let sink = |e: crate::sql::SqlError| ImportError::Sink(e.to_string());
    let mut sql = format!(
        "INSERT INTO {} (",
        qualify(dialect, schema, table).map_err(sink)?
    );
    let mut params = Vec::with_capacity(values.len());
    for (i, (col, value)) in values.iter().enumerate() {
        if i > 0 {
            sql.push_str(", ");
        }
        sql.push_str(&quote_ident(dialect, col).map_err(sink)?);
        params.push(value.clone());
    }
    sql.push_str(") VALUES (");
    for i in 0..params.len() {
        if i > 0 {
            sql.push_str(", ");
        }
        sql.push_str(&bind_placeholder(dialect, i));
    }
    sql.push(')');
    Ok(Some(PlannedStatement {
        op: PlanOp::Insert,
        sql,
        params,
        pk: Vec::new(),
        unchecked_optimistic: Vec::new(),
    }))
}

/// Blocking producer: read rows, send one `INSERT` each, then [`PlanStreamItem::End`].
///
/// Returns the number of data rows read. If the receiver is gone (database
/// error) it stops quietly and returns the count so far; the consumer's error
/// is the one to report. `on_row` is called with the running row count.
pub fn stream_import_rows<R: Read>(
    importer: &mut CsvImporter<R>,
    builder: &ImportStatementBuilder,
    dialect: Dialect,
    tx: &async_channel::Sender<PlanStreamItem>,
    cancel: &CancellationToken,
    done: &AtomicU64,
    mut on_row: impl FnMut(u64),
) -> Result<u64, ImportError> {
    let mut rows = 0u64;
    while let Some(item) = importer.next_row() {
        if cancel.is_cancelled() {
            return Err(ImportError::Cancelled);
        }
        let row = item?;
        rows += 1;
        done.store(rows, Ordering::Relaxed);
        on_row(rows);
        if let Some(stmt) = builder.statement(dialect, &row.values)? {
            if tx.send_blocking(PlanStreamItem::Statement(stmt)).is_err() {
                return Ok(rows);
            }
        }
    }
    if cancel.is_cancelled() {
        return Err(ImportError::Cancelled);
    }
    let _ = tx.send_blocking(PlanStreamItem::End);
    Ok(rows)
}

/// Stream `importer` into `session` inside one transaction. Returns rows imported.
///
/// The reader runs on a blocking thread; the transaction runs on the caller's
/// task. Cancel and every error roll back (nothing is written).
pub async fn run_streaming_import<R>(
    session: &dyn DatabaseSession,
    dialect: Dialect,
    structure: &TableStructure,
    mut importer: CsvImporter<R>,
    cancel: CancellationToken,
    done: Arc<AtomicU64>,
) -> Result<u64, ImportError>
where
    R: Read + Send + 'static,
{
    let builder = ImportStatementBuilder::for_structure(structure);
    let table = builder.table();
    let (tx, rx) = async_channel::bounded(IMPORT_STREAM_CAPACITY);

    let producer_cancel = cancel.clone();
    let producer = tokio::task::spawn_blocking(move || {
        stream_import_rows(
            &mut importer,
            &builder,
            dialect,
            &tx,
            &producer_cancel,
            &done,
            |_| {},
        )
    });

    let applied = session
        .apply_statement_stream(dialect, &table, rx, cancel.clone())
        .await;
    // `rx` was moved into the sink and dropped, so a blocked producer wakes.
    let produced = producer
        .await
        .map_err(|e| ImportError::Sink(e.to_string()))?;

    match produced {
        Err(e) => Err(e),
        Ok(rows) => match applied {
            Ok(_) => Ok(rows),
            Err(crate::DbError::Cancelled { .. }) => Err(ImportError::Cancelled),
            Err(e) => Err(ImportError::Sink(e.to_string())),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::sync::atomic::AtomicUsize;

    use super::*;
    use crate::csv_import::CsvImportOptions;
    use crate::types::ColumnMeta;

    fn builder() -> ImportStatementBuilder {
        ImportStatementBuilder::InsertOnly {
            schema: "main".into(),
            table: "t".into(),
        }
    }

    fn importer(rows: usize) -> CsvImporter<Cursor<Vec<u8>>> {
        let mut body = String::from("id,title\n");
        for i in 0..rows {
            body.push_str(&format!("{i},row{i}\n"));
        }
        let cols = vec![
            ColumnMeta {
                name: "id".into(),
                type_name: "integer".into(),
            },
            ColumnMeta {
                name: "title".into(),
                type_name: "text".into(),
            },
        ];
        CsvImporter::open_with_columns(
            Cursor::new(body.into_bytes()),
            &cols,
            CsvImportOptions::default(),
        )
        .unwrap()
    }

    /// IM-07: the reader never runs more than the channel capacity ahead of the sink.
    #[test]
    fn producer_is_backpressured_to_channel_capacity() {
        const CAP: usize = 4;
        const ROWS: usize = 5_000;
        let (tx, rx) = async_channel::bounded(CAP);
        let done = Arc::new(AtomicU64::new(0));
        let consumed = Arc::new(AtomicUsize::new(0));
        let max_ahead = Arc::new(AtomicUsize::new(0));

        let consumer = {
            let (done, consumed, max_ahead) = (done.clone(), consumed.clone(), max_ahead.clone());
            std::thread::spawn(move || {
                let mut ended = false;
                while let Ok(item) = rx.recv_blocking() {
                    match item {
                        PlanStreamItem::Statement(_) => {
                            let read = done.load(Ordering::Relaxed) as usize;
                            let seen = consumed.fetch_add(1, Ordering::Relaxed) + 1;
                            max_ahead.fetch_max(read.saturating_sub(seen), Ordering::Relaxed);
                        }
                        PlanStreamItem::End => ended = true,
                    }
                }
                ended
            })
        };

        let rows = stream_import_rows(
            &mut importer(ROWS),
            &builder(),
            Dialect::Sqlite,
            &tx,
            &CancellationToken::new(),
            &done,
            |_| {},
        )
        .unwrap();
        drop(tx);
        assert_eq!(rows, ROWS as u64);
        assert!(consumer.join().unwrap(), "End marker sent");
        assert_eq!(consumed.load(Ordering::Relaxed), ROWS);
        // channel capacity + the row being built + the one just received
        assert!(
            max_ahead.load(Ordering::Relaxed) <= CAP + 2,
            "reader ran {} rows ahead of the sink",
            max_ahead.load(Ordering::Relaxed)
        );
    }

    #[test]
    fn cancel_stops_producer_without_end_marker() {
        let (tx, rx) = async_channel::bounded(IMPORT_STREAM_CAPACITY);
        let cancel = CancellationToken::new();
        cancel.cancel();
        let err = stream_import_rows(
            &mut importer(10),
            &builder(),
            Dialect::Sqlite,
            &tx,
            &cancel,
            &AtomicU64::new(0),
            |_| {},
        )
        .unwrap_err();
        assert_eq!(err, ImportError::Cancelled);
        drop(tx);
        assert!(rx.try_recv().is_err(), "nothing may reach the sink");
    }

    #[test]
    fn parse_error_sends_no_end_marker() {
        let (tx, rx) = async_channel::bounded(IMPORT_STREAM_CAPACITY);
        let mut cols_importer = {
            let cols = vec![ColumnMeta {
                name: "id".into(),
                type_name: "integer".into(),
            }];
            CsvImporter::open_with_columns(
                Cursor::new(b"id\n1\n2\nnope\n".to_vec()),
                &cols,
                CsvImportOptions::default(),
            )
            .unwrap()
        };
        let err = stream_import_rows(
            &mut cols_importer,
            &builder(),
            Dialect::Sqlite,
            &tx,
            &CancellationToken::new(),
            &AtomicU64::new(0),
            |_| {},
        )
        .unwrap_err();
        assert!(matches!(err, ImportError::Row { line: 4, .. }), "{err:?}");
        drop(tx);
        let mut ends = 0;
        while let Ok(item) = rx.try_recv() {
            ends += usize::from(item == PlanStreamItem::End);
        }
        assert_eq!(ends, 0);
    }

    #[test]
    fn closed_receiver_stops_producer_quietly() {
        let (tx, rx) = async_channel::bounded(1);
        drop(rx);
        let rows = stream_import_rows(
            &mut importer(100),
            &builder(),
            Dialect::Sqlite,
            &tx,
            &CancellationToken::new(),
            &AtomicU64::new(0),
            |_| {},
        )
        .unwrap();
        assert_eq!(rows, 1, "stops at the first failed send");
    }
}
