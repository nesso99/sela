//! CSV import dialog — preview, column mapping, options (UX 11 / IM-*).

use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Disableable as _, Sizable as _};
use gpui_kit::prelude::*;
#[cfg(test)]
use gpui_kit::App;
use gpui_kit::{
    div, px, Context, Entity, EventEmitter, IntoElement, PathPromptOptions, Render, SharedString,
    Window,
};
use tokio_util::sync::CancellationToken;

use sela_core::{
    ColumnMeta, CsvImportOptions, CsvImporter, ImportError, TableStructure,
    IMPORT_REFUSED_READ_ONLY_STATUS,
};

use crate::ui::file_picker::{
    apply_browse_outcome_to_error, classify_paths_result, FilePickerOutcome,
};

use super::messages::format_import_failure;

const PREVIEW_MAX_ROWS: usize = 100;

#[derive(Clone)]
pub enum ImportDialogEvent {
    /// Pre-scan succeeded; caller runs pending-edits / safe-mode gates then import.
    Ready(ImportRequest),
    Cancel,
}

#[derive(Debug, Clone)]
pub struct ImportRequest {
    pub path: PathBuf,
    pub options: CsvImportOptions,
    /// Pre-scanned data-row count for Confirm copy / progress `{total}` (IM-05).
    pub total_rows: u64,
    pub schema: String,
    pub table: String,
}

#[derive(Debug, Clone)]
pub struct ImportDialogContext {
    pub schema: String,
    pub table: String,
    pub structure: TableStructure,
    /// When true, Import is disabled with Read-only copy (IM-05).
    pub read_only: bool,
}

struct PreviewState {
    headers: Vec<String>,
    /// Parallel to headers: mapped table column or None = Skip.
    mapping: Vec<Option<String>>,
    /// First up to 100 data rows (display text only — never log).
    rows: Vec<Vec<String>>,
    table_columns: Vec<ColumnMeta>,
}

pub struct ImportDialog {
    ctx: ImportDialogContext,
    path: Entity<InputState>,
    has_header: bool,
    empty_field_is_null: bool,
    preview: Option<PreviewState>,
    total_rows: Option<u64>,
    error: Option<String>,
    error_from_file_picker: bool,
    mapping_blocked: Option<String>,
    /// Cancel the in-flight background preview / pre-scan.
    preview_cancel: Option<CancellationToken>,
    preview_generation: u64,
    /// Browse result applied on the next render (async task has no Window).
    pending_browse_path: Option<String>,
    preview_loading: bool,
}

impl ImportDialog {
    pub fn new(ctx: ImportDialogContext, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            ctx,
            path: cx.new(|cx| InputState::new(window, cx).placeholder("/path/to/import.csv")),
            has_header: true,
            empty_field_is_null: true,
            preview: None,
            total_rows: None,
            error: None,
            error_from_file_picker: false,
            mapping_blocked: None,
            preview_cancel: None,
            preview_generation: 0,
            pending_browse_path: None,
            preview_loading: false,
        }
    }

    /// Inject a source path and load preview (headless tests).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn set_path_and_load(
        &mut self,
        path: impl Into<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let path = path.into();
        self.path.update(cx, |input, cx| {
            input.set_value(&path, window, cx);
        });
        self.reload_preview(cx);
    }

    #[cfg(test)]
    pub fn test_path(&self, cx: &App) -> String {
        self.path.read(cx).value().to_string()
    }

    #[cfg(test)]
    pub fn test_preview_headers(&self) -> Option<&[String]> {
        self.preview.as_ref().map(|p| p.headers.as_slice())
    }

    #[cfg(test)]
    pub fn test_mapping(&self) -> Option<&[Option<String>]> {
        self.preview.as_ref().map(|p| p.mapping.as_slice())
    }

    #[cfg(test)]
    pub fn test_total_rows(&self) -> Option<u64> {
        self.total_rows
    }

    #[cfg(test)]
    pub fn test_error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    fn apply_browse_outcome(&mut self, outcome: FilePickerOutcome) {
        wire_import_browse(
            &mut self.error,
            &mut self.error_from_file_picker,
            &mut self.pending_browse_path,
            outcome,
        );
    }

    #[cfg(test)]
    pub fn test_mapping_blocked(&self) -> Option<&str> {
        self.mapping_blocked.as_deref()
    }

    pub fn import_enabled(&self) -> bool {
        !self.ctx.read_only
            && self.preview.is_some()
            && self.total_rows.is_some()
            && self.mapping_blocked.is_none()
            && self.error.is_none()
            && !self.preview_loading
    }

    fn options(&self) -> CsvImportOptions {
        let required = required_columns(&self.ctx.structure);
        CsvImportOptions {
            has_header: self.has_header,
            empty_field_is_null: self.empty_field_is_null,
            required_columns: required,
            ..CsvImportOptions::default()
        }
    }

    fn cancel_preview_task(&mut self) {
        if let Some(token) = self.preview_cancel.take() {
            token.cancel();
        }
    }

    fn reload_preview(&mut self, cx: &mut Context<Self>) {
        self.cancel_preview_task();
        self.error = None;
        self.error_from_file_picker = false;
        self.mapping_blocked = None;
        self.preview = None;
        self.total_rows = None;
        self.preview_loading = false;
        if self.ctx.read_only {
            self.error = Some(IMPORT_REFUSED_READ_ONLY_STATUS.into());
            self.error_from_file_picker = false;
            cx.notify();
            return;
        }
        let path = self.path.read(cx).value().trim().to_string();
        if path.is_empty() {
            cx.notify();
            return;
        }
        let path = PathBuf::from(path);
        let structure = self.ctx.structure.clone();
        let options = self.options();
        let cancel = CancellationToken::new();
        self.preview_cancel = Some(cancel.clone());
        self.preview_generation = self.preview_generation.wrapping_add(1);
        let generation = self.preview_generation;
        self.preview_loading = true;
        cx.notify();

        cx.spawn(async move |this, async_cx| {
            let result = async_cx
                .background_executor()
                .spawn(async move {
                    load_preview_and_count_streaming(&path, &structure, &options, &cancel)
                })
                .await;
            this.update(async_cx, |this, cx| {
                if this.preview_generation != generation {
                    return;
                }
                this.preview_loading = false;
                this.preview_cancel = None;
                match result {
                    Ok((preview, total)) => {
                        if let Err(msg) =
                            validate_required_mapped(&preview.mapping, &this.options())
                        {
                            this.mapping_blocked = Some(msg);
                        }
                        this.preview = Some(preview);
                        this.total_rows = Some(total);
                    }
                    Err(ImportError::Cancelled) => {
                        // Dialog closed or superseded — leave empty.
                    }
                    Err(err) => {
                        if let ImportError::Mapping(msg) = &err {
                            if msg.contains("Map every required column") {
                                this.mapping_blocked = Some(msg.clone());
                            } else {
                                let (banner, _) = format_import_failure(&err);
                                this.error = Some(banner);
                                this.error_from_file_picker = false;
                            }
                        } else {
                            let (banner, _) = format_import_failure(&err);
                            this.error = Some(banner);
                            this.error_from_file_picker = false;
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn browse_csv(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.ctx.read_only {
            return;
        }
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose CSV to import".into()),
        });
        cx.spawn(async move |this, async_cx| {
            let outcome = classify_paths_result(receiver.await);
            this.update(async_cx, |dialog, cx| {
                dialog.apply_browse_outcome(outcome);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn flush_pending_browse(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = self.pending_browse_path.take() {
            self.path.update(cx, |input, cx| {
                input.set_value(&path, window, cx);
            });
            self.reload_preview(cx);
        }
    }

    fn set_mapping_target(
        &mut self,
        csv_ix: usize,
        target: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let opts = self.options();
        let Some(preview) = self.preview.as_mut() else {
            return;
        };
        if let Some(slot) = preview.mapping.get_mut(csv_ix) {
            *slot = target;
        }
        // Drop duplicate targets: if another CSV col maps to the same name, Skip it.
        if let Some(Some(name)) = preview.mapping.get(csv_ix).cloned() {
            for (i, slot) in preview.mapping.iter_mut().enumerate() {
                if i != csv_ix && slot.as_deref() == Some(name.as_str()) {
                    *slot = None;
                }
            }
        }
        let mapping = preview.mapping.clone();
        self.mapping_blocked = validate_required_mapped(&mapping, &opts).err();
        cx.notify();
    }

    fn confirm_import(&mut self, cx: &mut Context<Self>) {
        if self.ctx.read_only {
            self.error = Some(IMPORT_REFUSED_READ_ONLY_STATUS.into());
            self.error_from_file_picker = false;
            cx.notify();
            return;
        }
        if !self.import_enabled() {
            if self.mapping_blocked.is_none() && self.error.is_none() {
                self.error = Some("Load a CSV file first.".into());
                self.error_from_file_picker = false;
            }
            cx.notify();
            return;
        }
        let path = PathBuf::from(self.path.read(cx).value().trim());
        let total = self.total_rows.unwrap_or(0);
        let mut options = self.options();
        options.required_columns = required_columns(&self.ctx.structure);
        self.cancel_preview_task();
        cx.emit(ImportDialogEvent::Ready(ImportRequest {
            path,
            options,
            total_rows: total,
            schema: self.ctx.schema.clone(),
            table: self.ctx.table.clone(),
        }));
    }

    /// Mapping snapshot for the import job (after Ready).
    pub fn take_mapping_targets(&self) -> Option<Vec<Option<String>>> {
        self.preview.as_ref().map(|p| p.mapping.clone())
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        self.cancel_preview_task();
        cx.emit(ImportDialogEvent::Cancel);
    }
}

impl EventEmitter<ImportDialogEvent> for ImportDialog {}

impl Render for ImportDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.flush_pending_browse(window, cx);

        let this = cx.entity();
        let read_only = self.ctx.read_only;
        let import_enabled = self.import_enabled();

        v_flex()
            .w_full()
            .gap_2()
            .p_1()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "Import into {}.{}",
                        self.ctx.schema, self.ctx.table
                    )),
            )
            .when(read_only, |col| {
                col.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(IMPORT_REFUSED_READ_ONLY_STATUS),
                )
            })
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .child(Input::new(&self.path).id("import-path-input")),
                    )
                    .child({
                        let this = this.clone();
                        Button::new("import-browse")
                            .label("Browse")
                            .ghost()
                            .disabled(read_only)
                            .on_click(move |_, window, cx| {
                                this.update(cx, |dialog, cx| dialog.browse_csv(window, cx));
                            })
                    })
                    .child({
                        let this = this.clone();
                        Button::new("import-load")
                            .label("Load")
                            .ghost()
                            .disabled(read_only)
                            .on_click(move |_, _, cx| {
                                this.update(cx, |dialog, cx| dialog.reload_preview(cx));
                            })
                    }),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child({
                        let this = this.clone();
                        let on = self.has_header;
                        Button::new("import-header")
                            .label(if on {
                                "First row is header: on"
                            } else {
                                "First row is header: off"
                            })
                            .xsmall()
                            .ghost()
                            .disabled(read_only)
                            .on_click(move |_, _, cx| {
                                this.update(cx, |dialog, cx| {
                                    dialog.has_header = !dialog.has_header;
                                    dialog.reload_preview(cx);
                                });
                            })
                    })
                    .child({
                        let this = this.clone();
                        let on = self.empty_field_is_null;
                        Button::new("import-empty-null")
                            .label(if on {
                                "Empty field = NULL: on"
                            } else {
                                "Empty field = NULL: off"
                            })
                            .xsmall()
                            .ghost()
                            .disabled(read_only)
                            .on_click(move |_, _, cx| {
                                this.update(cx, |dialog, cx| {
                                    dialog.empty_field_is_null = !dialog.empty_field_is_null;
                                    dialog.reload_preview(cx);
                                });
                            })
                    }),
            )
            .when(self.preview_loading, |col| {
                col.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("Scanning CSV…"),
                )
            })
            .when_some(self.total_rows, |col, n| {
                col.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("{n} data rows (pre-scanned)")),
                )
            })
            .when_some(self.mapping_blocked.clone(), |col, msg| {
                col.child(div().text_xs().text_color(cx.theme().danger).child(msg))
            })
            .when_some(self.error.clone(), |col, err| {
                col.child(div().text_xs().text_color(cx.theme().danger).child(err))
            })
            .when_some(self.preview.as_ref(), |col, preview| {
                let table_cols = preview.table_columns.clone();
                col.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("Column mapping"),
                )
                .child(
                    v_flex()
                        .gap_1()
                        .children(preview.headers.iter().enumerate().map(|(ix, header)| {
                            let this = this.clone();
                            let current = preview.mapping.get(ix).cloned().flatten();
                            let label = current.clone().unwrap_or_else(|| "Skip".into());
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(div().w(px(140.)).text_xs().child(header.clone()))
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child("→"),
                                )
                                .child({
                                    let this_skip = this.clone();
                                    Button::new(SharedString::from(format!("import-map-{ix}-skip")))
                                        .label("Skip")
                                        .xsmall()
                                        .when(current.is_some(), |b| b.ghost())
                                        .when(current.is_none(), |b| b.primary())
                                        .on_click(move |_, _, cx| {
                                            this_skip.update(cx, |dialog, cx| {
                                                dialog.set_mapping_target(ix, None, cx);
                                            });
                                        })
                                })
                                .children(table_cols.iter().map(|col| {
                                    let this = this.clone();
                                    let name = col.name.clone();
                                    let selected = current.as_deref() == Some(name.as_str());
                                    Button::new(SharedString::from(format!(
                                        "import-map-{ix}-{}",
                                        col.name
                                    )))
                                    .label(col.name.clone())
                                    .xsmall()
                                    .when(!selected, |b| b.ghost())
                                    .when(selected, |b| b.primary())
                                    .on_click(
                                        move |_, _, cx| {
                                            this.update(cx, |dialog, cx| {
                                                dialog.set_mapping_target(
                                                    ix,
                                                    Some(name.clone()),
                                                    cx,
                                                );
                                            });
                                        },
                                    )
                                }))
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(label),
                                )
                        })),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!(
                            "Preview (first {} data rows)",
                            preview.rows.len().min(PREVIEW_MAX_ROWS)
                        )),
                )
                .child(
                    div()
                        .id("import-preview-scroll")
                        .h(px(160.))
                        .w_full()
                        .border_1()
                        .border_color(cx.theme().border)
                        .p_1()
                        .overflow_y_scrollbar()
                        .font_family(cx.theme().mono_font_family.clone())
                        .child(
                            v_flex().gap_1().children(
                                preview
                                    .rows
                                    .iter()
                                    .map(|row| div().text_xs().child(row.join(" | "))),
                            ),
                        ),
                )
            })
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .mt_2()
                    .child({
                        let this = this.clone();
                        Button::new("import-dialog-cancel")
                            .label("Cancel")
                            .ghost()
                            .on_click(move |_, _, cx| {
                                this.update(cx, |dialog, cx| dialog.cancel(cx));
                            })
                    })
                    .child({
                        let this = this.clone();
                        Button::new("import-dialog-import")
                            .label("Import")
                            .primary()
                            .disabled(!import_enabled)
                            .tooltip(if read_only {
                                IMPORT_REFUSED_READ_ONLY_STATUS
                            } else {
                                self.mapping_blocked
                                    .as_deref()
                                    .unwrap_or("Import CSV into this table")
                            })
                            .on_click(move |_, _, cx| {
                                this.update(cx, |dialog, cx| dialog.confirm_import(cx));
                            })
                    }),
            )
    }
}

/// NOT NULL, no default, not generated (IM-03).
pub(crate) fn required_columns(structure: &TableStructure) -> Vec<String> {
    structure
        .columns
        .iter()
        .filter(|c| !c.nullable && c.default.is_none() && !c.generated)
        .map(|c| c.name.clone())
        .collect()
}

fn validate_required_mapped(
    mapping: &[Option<String>],
    options: &CsvImportOptions,
) -> Result<(), String> {
    for req in &options.required_columns {
        let mapped = mapping
            .iter()
            .flatten()
            .any(|c| c.eq_ignore_ascii_case(req));
        if !mapped {
            return Err("Map every required column or cancel.".into());
        }
    }
    Ok(())
}

/// Stream preview rows and a full data-row count without materializing the file.
fn load_preview_and_count_streaming(
    path: &PathBuf,
    structure: &TableStructure,
    options: &CsvImportOptions,
    cancel: &CancellationToken,
) -> Result<(PreviewState, u64), ImportError> {
    if cancel.is_cancelled() {
        return Err(ImportError::Cancelled);
    }
    let file = File::open(path).map_err(|e| ImportError::Io(e.to_string()))?;
    let reader = BufReader::new(file);

    let table_columns: Vec<ColumnMeta> = structure
        .columns
        .iter()
        .map(|c| ColumnMeta {
            name: c.name.clone(),
            type_name: c.type_name.clone(),
        })
        .collect();

    let mut importer = CsvImporter::open_with_columns(reader, &table_columns, options.clone())?;
    let headers = importer.headers().to_vec();
    let mapping = importer.mapping().targets.clone();

    let mut rows = Vec::new();
    let mut total = 0u64;
    while let Some(item) = importer.next_row() {
        if cancel.is_cancelled() {
            return Err(ImportError::Cancelled);
        }
        let row = item?;
        total += 1;
        if rows.len() < PREVIEW_MAX_ROWS {
            let mut display = Vec::with_capacity(headers.len());
            for target in &mapping {
                match target {
                    Some(col) => {
                        let cell = row
                            .values
                            .get(col)
                            .map(|v| match v {
                                sela_core::Value::Null => String::new(),
                                other => other.display().into_owned(),
                            })
                            .unwrap_or_default();
                        display.push(cell);
                    }
                    None => display.push(String::new()),
                }
            }
            rows.push(display);
        }
    }

    Ok((
        PreviewState {
            headers,
            mapping,
            rows,
            table_columns,
        },
        total,
    ))
}

fn wire_import_browse(
    error: &mut Option<String>,
    error_from_file_picker: &mut bool,
    pending_browse_path: &mut Option<String>,
    outcome: FilePickerOutcome,
) {
    apply_browse_outcome_to_error(error, error_from_file_picker, outcome, |path| {
        *pending_browse_path = Some(path.to_string_lossy().to_string());
    });
}

#[cfg(test)]
mod browse_wiring_tests {
    use std::path::PathBuf;

    use super::wire_import_browse;
    use crate::ui::file_picker::FilePickerOutcome;

    #[test]
    fn import_browse_failed_sets_clean_display_error() {
        let mut err = None;
        let mut from_picker = false;
        let mut pending = None;
        wire_import_browse(
            &mut err,
            &mut from_picker,
            &mut pending,
            FilePickerOutcome::Failed("Couldn't open file picker".into()),
        );
        assert!(from_picker);
        assert_eq!(err.as_deref(), Some("Couldn't open file picker"));
        assert!(!err.as_ref().unwrap().contains("FILE_PICKER"));
    }

    #[test]
    fn import_browse_cancel_keeps_validation_error() {
        let mut err = Some("Missing file".into());
        let mut from_picker = false;
        let mut pending = None;
        wire_import_browse(
            &mut err,
            &mut from_picker,
            &mut pending,
            FilePickerOutcome::Cancelled,
        );
        assert_eq!(err.as_deref(), Some("Missing file"));
    }

    #[test]
    fn import_browse_picked_clears_picker_error_only() {
        let mut err = Some("Couldn't open file picker".into());
        let mut from_picker = true;
        let mut pending = None;
        wire_import_browse(
            &mut err,
            &mut from_picker,
            &mut pending,
            FilePickerOutcome::Picked(PathBuf::from("/tmp/a.csv")),
        );
        assert!(err.is_none());
        assert_eq!(pending.as_deref(), Some("/tmp/a.csv"));
    }
}
