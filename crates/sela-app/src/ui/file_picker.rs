//! Classify native file-picker async results (export, import, SSH Browse).

use std::path::PathBuf;

/// Outcome of a single-path save/open dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FilePickerOutcome {
    Picked(PathBuf),
    Cancelled,
    Failed(String),
}

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

/// Apply a browse outcome: update banner text only on failure; clear stale picker errors on pick.
pub(crate) fn apply_browse_outcome_to_error(
    error: &mut Option<String>,
    error_from_file_picker: &mut bool,
    outcome: FilePickerOutcome,
    on_picked: impl FnOnce(PathBuf),
) {
    match outcome {
        FilePickerOutcome::Picked(path) => {
            on_picked(path);
            if *error_from_file_picker {
                *error = None;
                *error_from_file_picker = false;
            }
        }
        FilePickerOutcome::Cancelled => {}
        FilePickerOutcome::Failed(message) => {
            *error = Some(message);
            *error_from_file_picker = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wire(error: &mut Option<String>, from_picker: &mut bool, outcome: FilePickerOutcome) {
        apply_browse_outcome_to_error(error, from_picker, outcome, |_| {});
    }

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
        let mut from_picker = false;
        wire(&mut err, &mut from_picker, FilePickerOutcome::Cancelled);
        assert_eq!(err.as_deref(), Some("Choose a file path."));
        assert!(!from_picker);
    }

    #[test]
    fn browse_failed_sets_clean_display_message() {
        let mut err = None;
        let mut from_picker = false;
        wire(
            &mut err,
            &mut from_picker,
            FilePickerOutcome::Failed("Couldn't open file picker".into()),
        );
        assert!(from_picker);
        assert_eq!(err.as_deref(), Some("Couldn't open file picker"));
        assert!(!err.as_ref().unwrap().contains("FILE_PICKER"));
    }

    #[test]
    fn browse_picked_clears_only_picker_error() {
        let mut err = Some("portal missing".into());
        let mut from_picker = true;
        apply_browse_outcome_to_error(
            &mut err,
            &mut from_picker,
            FilePickerOutcome::Picked(PathBuf::from("/x")),
            |_| {},
        );
        assert!(err.is_none());
        assert!(!from_picker);

        let mut validation = Some("Choose a file path.".into());
        let mut from_picker = false;
        apply_browse_outcome_to_error(
            &mut validation,
            &mut from_picker,
            FilePickerOutcome::Picked(PathBuf::from("/x")),
            |_| {},
        );
        assert_eq!(validation.as_deref(), Some("Choose a file path."));
    }
}
