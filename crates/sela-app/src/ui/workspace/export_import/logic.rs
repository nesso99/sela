//! Pure helpers for export/import cancel routing, completion gates, and temp files.
//!
//! Kept free of GPUI so unit tests can exercise the real decision logic without
//! a host that re-implements cancel.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::jobs::TransferKind;

/// Whether Stop / `secondary-.` should cancel the background transfer instead of
/// (or before) cancelling an active-tab query.
///
/// Cancel the transfer only when one is running **and** the active tab has no
/// running query. A query on the active tab always wins Stop.
pub fn should_cancel_transfer_on_stop(
    transfer_active: bool,
    active_tab_has_running_query: bool,
) -> bool {
    transfer_active && !active_tab_has_running_query
}

/// True when the active tab's `busy` flag is from a query, not from the transfer
/// owned by that same tab.
pub fn active_tab_has_running_query(active_tab_busy: bool, transfer_on_active_tab: bool) -> bool {
    active_tab_busy && !transfer_on_active_tab
}

/// Whether a finishing transfer still owns the workspace slot (generation + kind).
pub fn transfer_completion_applies(
    job_generation: u64,
    job_kind: TransferKind,
    expected_generation: u64,
    expected_kind: TransferKind,
) -> bool {
    job_generation == expected_generation && job_kind == expected_kind
}

/// Same-directory temp path for a streaming export (never the destination).
pub fn export_temp_path(dest: &Path) -> PathBuf {
    let name = dest
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("export");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    dest.with_file_name(format!(".{name}.sela-tmp-{}-{nanos}", std::process::id()))
}

/// On cancel/failure, delete only the temp file — never the user destination.
pub fn export_paths_to_discard_on_failure(tmp: &Path) -> Vec<PathBuf> {
    vec![tmp.to_path_buf()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn stop_cancels_transfer_only_without_active_query() {
        assert!(should_cancel_transfer_on_stop(true, false));
        assert!(!should_cancel_transfer_on_stop(true, true));
        assert!(!should_cancel_transfer_on_stop(false, false));
        assert!(!should_cancel_transfer_on_stop(false, true));
    }

    #[test]
    fn query_busy_on_active_tab_is_not_transfer() {
        assert!(active_tab_has_running_query(true, false));
        assert!(!active_tab_has_running_query(true, true));
        assert!(!active_tab_has_running_query(false, false));
        assert!(!active_tab_has_running_query(false, true));
    }

    #[test]
    fn completion_gate_requires_generation_and_kind() {
        assert!(transfer_completion_applies(
            3,
            TransferKind::Export,
            3,
            TransferKind::Export
        ));
        assert!(!transfer_completion_applies(
            3,
            TransferKind::Export,
            4,
            TransferKind::Export
        ));
        assert!(!transfer_completion_applies(
            3,
            TransferKind::Export,
            3,
            TransferKind::Import
        ));
    }

    #[test]
    fn export_temp_is_sibling_never_destination() {
        let dest = PathBuf::from("/tmp/out.csv");
        let tmp = export_temp_path(&dest);
        assert_eq!(tmp.parent(), dest.parent());
        assert_ne!(tmp, dest);
        assert!(tmp
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains("sela-tmp"));
        let discard = export_paths_to_discard_on_failure(&tmp);
        assert_eq!(discard, vec![tmp]);
        assert!(!discard.iter().any(|p| p == &dest));
    }

    #[test]
    fn pre_existing_destination_survives_discard_list() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("keep.csv");
        fs::write(&dest, b"original").unwrap();
        let tmp = export_temp_path(&dest);
        fs::write(&tmp, b"partial").unwrap();
        for path in export_paths_to_discard_on_failure(&tmp) {
            let _ = fs::remove_file(path);
        }
        assert_eq!(fs::read(&dest).unwrap(), b"original");
        assert!(!tmp.exists());
    }
}
