//! Status / banner copy for export and CSV import (UX 11 / QA EX-* IM-*).
//!
//! Pure helpers so headless tests can assert the strings without a live window.

use std::path::Path;
use std::time::Duration;

use sela_core::{
    ExportError, ImportError, EXPORT_CANCELLED_STATUS, IMPORT_CANCELLED_STATUS,
    IMPORT_REFUSED_READ_ONLY_STATUS,
};

/// Concurrent export refusal (EX-13).
pub const EXPORT_ALREADY_RUNNING: &str = "An export is already running.";

/// Concurrent import refusal (mirror of EX-13 for import).
pub const IMPORT_ALREADY_RUNNING: &str = "An import is already running.";

/// Whole-table scope disabled on SQL-only tabs (EX-04).
pub const WHOLE_TABLE_DISABLED_REASON: &str = "Open a table to export the whole table.";

/// Failure status when export aborts after a visible partial (EX-06).
pub const EXPORT_FAILED_STATUS: &str = "Export failed.";

/// Format the in-flight export status line (EX-05).
pub fn export_progress_status(done: u64, total: Option<u64>, elapsed: Duration) -> String {
    let secs = elapsed.as_secs_f32();
    match total {
        Some(total) => format!("Exporting · {done} / {total} rows written · {secs:.1} s"),
        None => format!("Exporting · {done} rows written · {secs:.1} s"),
    }
}

/// Status note when exporting a capped SQL result (EX-01).
pub fn export_capped_note(n: u64) -> String {
    format!("Exporting the loaded result (capped at {n} rows).")
}

/// Success status after export (EX-05).
pub fn export_success_status(rows: u64, path: &Path, ms: u128) -> String {
    format!("Exported {rows} rows · {} · {ms} ms", path.display())
}

/// Map an [`ExportError`] to (banner, status). Never includes row payloads.
pub fn format_export_failure(err: &ExportError) -> (String, String) {
    match err {
        ExportError::Cancelled => (
            EXPORT_CANCELLED_STATUS.into(),
            EXPORT_CANCELLED_STATUS.into(),
        ),
        ExportError::DecodeInInsert { column } => (
            format!("Export failed: decode/unsupported cell in column `{column}`."),
            EXPORT_FAILED_STATUS.into(),
        ),
        ExportError::NonFiniteInInsert { column } => (
            format!("Export failed: non-finite number in column `{column}`."),
            EXPORT_FAILED_STATUS.into(),
        ),
        other => (
            format!("Export failed: {other}"),
            EXPORT_FAILED_STATUS.into(),
        ),
    }
}

/// Format the in-flight import status line (IM-01 / IM-07).
pub fn import_progress_status(done: u64, total: u64, elapsed: Duration) -> String {
    let secs = elapsed.as_secs_f32();
    format!("Importing · {done} / {total} rows · {secs:.1} s")
}

/// Confirm-writes title (IM-05). `{n}` is the pre-scanned data-row count.
pub fn import_confirm_title(n: u64, schema: &str, table: &str) -> String {
    format!("Import {n} rows into {schema}.{table}?")
}

/// Pending-edits confirm before the safe-mode gate (IM-09).
pub fn import_discard_pending_title() -> &'static str {
    "Import and discard pending edits?"
}

pub fn import_discard_pending_body(n: usize, schema: &str, table: &str) -> String {
    format!(
        "{n} pending changes on {schema}.{table} will be discarded. The database is not changed until import runs."
    )
}

/// Map an [`ImportError`] to (banner, status). Never includes full row data.
pub fn format_import_failure(err: &ImportError) -> (String, String) {
    match err {
        ImportError::Cancelled => (
            IMPORT_CANCELLED_STATUS.into(),
            IMPORT_CANCELLED_STATUS.into(),
        ),
        ImportError::RefuseReadOnly => (
            IMPORT_REFUSED_READ_ONLY_STATUS.into(),
            IMPORT_REFUSED_READ_ONLY_STATUS.into(),
        ),
        ImportError::Row { line, message } => {
            let banner = format!("Import failed at row {line}: {message}. No rows were written.");
            (banner.clone(), banner)
        }
        ImportError::InvalidUtf8 { line } if *line == 0 => (
            "Could not parse CSV: invalid UTF-8.".into(),
            "Could not parse CSV: invalid UTF-8.".into(),
        ),
        ImportError::InvalidUtf8 { line } => {
            let banner =
                format!("Import failed at row {line}: invalid UTF-8. No rows were written.");
            (banner.clone(), banner)
        }
        ImportError::Io(detail) | ImportError::Mapping(detail) | ImportError::Schema(detail) => {
            let banner = format!("Could not parse CSV: {detail}.");
            (banner.clone(), banner)
        }
        other => {
            let banner = format!("Import failed: {other}. No rows were written.");
            (banner.clone(), banner)
        }
    }
}

/// Success status after import.
pub fn import_success_status(rows: u64, ms: u128) -> String {
    format!("Imported {rows} rows · {ms} ms")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    #[test]
    fn ex05_progress_with_and_without_total() {
        let s = export_progress_status(12, None, Duration::from_millis(1500));
        assert!(s.contains("Exporting · 12 rows written"));
        assert!(s.contains("1.5 s"));
        let s = export_progress_status(3, Some(10), Duration::from_secs(2));
        assert!(s.contains("3 / 10"));
    }

    #[test]
    fn ex01_capped_note() {
        assert_eq!(
            export_capped_note(1000),
            "Exporting the loaded result (capped at 1000 rows)."
        );
    }

    #[test]
    fn ex06_cancel_and_fail_copy() {
        let (b, s) = format_export_failure(&ExportError::Cancelled);
        assert_eq!(b, EXPORT_CANCELLED_STATUS);
        assert_eq!(s, EXPORT_CANCELLED_STATUS);
        let (b, s) = format_export_failure(&ExportError::Io("disk full".into()));
        assert!(b.contains("Export failed"));
        assert_eq!(s, EXPORT_FAILED_STATUS);
    }

    #[test]
    fn ex13_already_running() {
        assert_eq!(EXPORT_ALREADY_RUNNING, "An export is already running.");
    }

    #[test]
    fn im05_confirm_title_uses_prescan_n() {
        assert_eq!(
            import_confirm_title(250, "public", "books"),
            "Import 250 rows into public.books?"
        );
    }

    #[test]
    fn im06_row_error_includes_line() {
        let (b, _) = format_import_failure(&ImportError::Row {
            line: 7,
            message: "invalid integer".into(),
        });
        assert!(b.contains("row 7"));
        assert!(b.contains("No rows were written."));
        assert!(!b.contains("password"));
    }

    #[test]
    fn im09_pending_copy() {
        assert_eq!(
            import_discard_pending_title(),
            "Import and discard pending edits?"
        );
        let body = import_discard_pending_body(3, "public", "books");
        assert!(body.contains("3 pending changes on public.books"));
    }

    #[test]
    fn success_paths_do_not_embed_row_payloads() {
        let path = PathBuf::from("/tmp/out.csv");
        let s = export_success_status(2, &path, 40);
        assert!(s.contains("Exported 2 rows"));
        assert!(s.contains("/tmp/out.csv"));
        assert!(!s.contains("NULL"));
    }
}
