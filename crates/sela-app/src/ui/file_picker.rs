//! Classify native file-picker async results (export, import, SSH Browse).

use std::path::PathBuf;

/// Outcome of a single-path save/open dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FilePickerOutcome {
    Picked(PathBuf),
    Cancelled,
    Failed(String),
}

/// Prefix marking an error raised by Browse (so cancel does not wipe other errors).
pub(crate) const FILE_PICKER_ERROR_MARKER: &str = "FILE_PICKER:";

const ONESHOT_CANCELED: &str = "oneshot canceled";

/// `prompt_for_new_path` receiver result (`Ok(Ok(Some|None|Err))`).
pub(crate) fn classify_new_path_result(
    result: Result<Result<Option<PathBuf>, impl ToString>, impl ToString>,
) -> FilePickerOutcome {
    match result {
        Ok(Ok(Some(path))) => FilePickerOutcome::Picked(path),
        Ok(Ok(None)) => FilePickerOutcome::Cancelled,
        Ok(Err(err)) => FilePickerOutcome::Failed(normalize_picker_failure(err.to_string())),
        Err(err) => FilePickerOutcome::Failed(normalize_picker_failure(err.to_string())),
    }
}

/// `prompt_for_paths` receiver result (first path wins when multiple).
pub(crate) fn classify_paths_result(
    result: Result<Result<Option<Vec<PathBuf>>, impl ToString>, impl ToString>,
) -> FilePickerOutcome {
    match result {
        Ok(Ok(Some(paths))) => paths
            .into_iter()
            .next()
            .map(FilePickerOutcome::Picked)
            .unwrap_or(FilePickerOutcome::Cancelled),
        Ok(Ok(None)) => FilePickerOutcome::Cancelled,
        Ok(Err(err)) => FilePickerOutcome::Failed(normalize_picker_failure(err.to_string())),
        Err(err) => FilePickerOutcome::Failed(normalize_picker_failure(err.to_string())),
    }
}

fn normalize_picker_failure(message: String) -> String {
    if message.eq_ignore_ascii_case(ONESHOT_CANCELED) {
        "File picker closed unexpectedly".into()
    } else {
        message
    }
}

pub(crate) fn is_picker_error(message: &str) -> bool {
    message.starts_with(FILE_PICKER_ERROR_MARKER)
}

fn wrap_picker_error(message: String) -> String {
    format!("{FILE_PICKER_ERROR_MARKER}{message}")
}

/// Apply a browse outcome: update `error` only on failure; clear picker errors on success.
pub(crate) fn apply_browse_outcome_to_error(
    error: &mut Option<String>,
    outcome: FilePickerOutcome,
    on_picked: impl FnOnce(PathBuf),
) {
    match outcome {
        FilePickerOutcome::Picked(path) => {
            on_picked(path);
            if error.as_ref().is_some_and(|e| is_picker_error(e)) {
                *error = None;
            }
        }
        FilePickerOutcome::Cancelled => {}
        FilePickerOutcome::Failed(message) => {
            *error = Some(wrap_picker_error(message));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_new_path_picked_cancelled_failed() {
        assert_eq!(
            classify_new_path_result(Ok::<_, &str>(Ok::<_, &str>(Some(PathBuf::from(
                "/tmp/out.csv",
            ))))),
            FilePickerOutcome::Picked(PathBuf::from("/tmp/out.csv")),
        );
        assert_eq!(
            classify_new_path_result(Ok::<_, &str>(Ok::<_, &str>(None))),
            FilePickerOutcome::Cancelled
        );
        assert_eq!(
            classify_new_path_result(Ok::<_, &str>(Err::<_, &str>("portal missing"))),
            FilePickerOutcome::Failed("portal missing".into())
        );
        assert_eq!(
            classify_new_path_result(Err::<Result<Option<PathBuf>, &str>, _>(
                ONESHOT_CANCELED.to_string(),
            )),
            FilePickerOutcome::Failed("File picker closed unexpectedly".into())
        );
    }

    #[test]
    fn classify_paths_picked_cancelled_failed() {
        assert_eq!(
            classify_paths_result(Ok::<_, &str>(Ok::<_, &str>(Some(vec![PathBuf::from(
                "/a",
            )])))),
            FilePickerOutcome::Picked(PathBuf::from("/a")),
        );
        assert_eq!(
            classify_paths_result(Ok::<_, &str>(Ok::<_, &str>(None))),
            FilePickerOutcome::Cancelled
        );
        assert_eq!(
            classify_paths_result(Ok::<_, &str>(Err::<_, &str>("Couldn't open file picker",))),
            FilePickerOutcome::Failed("Couldn't open file picker".into())
        );
    }

    #[test]
    fn browse_cancel_preserves_unrelated_error() {
        let mut err = Some("Choose a file path.".into());
        apply_browse_outcome_to_error(&mut err, FilePickerOutcome::Cancelled, |_| {});
        assert_eq!(err.as_deref(), Some("Choose a file path."));
    }

    #[test]
    fn browse_failed_sets_marked_error() {
        let mut err = None;
        apply_browse_outcome_to_error(
            &mut err,
            FilePickerOutcome::Failed("portal missing".into()),
            |_| {},
        );
        assert!(err.as_ref().is_some_and(|e| is_picker_error(e)));
        assert!(err.unwrap().contains("portal missing"));
    }

    #[test]
    fn browse_picked_clears_only_picker_error() {
        let mut err = Some(wrap_picker_error("old".into()));
        apply_browse_outcome_to_error(
            &mut err,
            FilePickerOutcome::Picked(PathBuf::from("/x")),
            |_| {},
        );
        assert!(err.is_none());
        let mut validation = Some("Choose a file path.".into());
        apply_browse_outcome_to_error(
            &mut validation,
            FilePickerOutcome::Picked(PathBuf::from("/x")),
            |_| {},
        );
        assert_eq!(validation.as_deref(), Some("Choose a file path."));
    }
}
