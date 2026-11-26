//! Classify native file-picker async results (export, import, SSH Browse).

use std::path::PathBuf;

/// Outcome of a single-path save/open dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FilePickerOutcome {
    Picked(PathBuf),
    Cancelled,
    Failed(String),
}

/// `prompt_for_new_path` receiver result (`Ok(Ok(Some|None|Err))`).
pub(crate) fn classify_new_path_result(
    result: Result<Result<Option<PathBuf>, impl ToString>, impl ToString>,
) -> FilePickerOutcome {
    match result {
        Ok(Ok(Some(path))) => FilePickerOutcome::Picked(path),
        Ok(Ok(None)) => FilePickerOutcome::Cancelled,
        Ok(Err(err)) => FilePickerOutcome::Failed(err.to_string()),
        Err(err) => FilePickerOutcome::Failed(err.to_string()),
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
        Ok(Err(err)) => FilePickerOutcome::Failed(err.to_string()),
        Err(err) => FilePickerOutcome::Failed(err.to_string()),
    }
}

/// Returns a user-facing error when the picker fails; cancel is silent.
pub(crate) fn apply_file_picker_outcome(
    outcome: FilePickerOutcome,
    on_picked: impl FnOnce(PathBuf),
) -> Option<String> {
    match outcome {
        FilePickerOutcome::Picked(path) => {
            on_picked(path);
            None
        }
        FilePickerOutcome::Cancelled => None,
        FilePickerOutcome::Failed(message) => Some(message),
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
                "task dropped".to_string(),
            )),
            FilePickerOutcome::Failed("task dropped".into())
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
    fn apply_outcome_picked_cancelled_failed() {
        assert!(
            apply_file_picker_outcome(FilePickerOutcome::Picked(PathBuf::from("/x")), |_| {},)
                .is_none()
        );
        assert!(apply_file_picker_outcome(FilePickerOutcome::Cancelled, |_| {}).is_none());
        assert_eq!(
            apply_file_picker_outcome(FilePickerOutcome::Failed("portal".into()), |_| {},)
                .as_deref(),
            Some("portal")
        );
    }
}
