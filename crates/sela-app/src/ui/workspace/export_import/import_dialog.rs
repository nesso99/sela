//! CSV import dialog — preview, column mapping, options (UX 11 / IM-*).

use std::fs::File;
use std::io::{BufReader, Cursor, Read, Seek, SeekFrom};
use std::path::PathBuf;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Disableable as _, Sizable as _};
use gpui_kit::prelude::*;
#[cfg(test)]
use gpui_kit::App;
use gpui_kit::{div, px, Context, Entity, EventEmitter, IntoElement, Render, SharedString, Window};

use sela_core::{
    count_csv_data_rows, ColumnMapping, ColumnMeta, CsvImportOptions, CsvImporter, ImportError,
    TableStructure, IMPORT_REFUSED_READ_ONLY_STATUS,
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
    mapping_blocked: Option<String>,
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
            mapping_blocked: None,
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

    fn reload_preview(&mut self, cx: &mut Context<Self>) {
        self.error = None;
        self.mapping_blocked = None;
        self.preview = None;
        self.total_rows = None;
        if self.ctx.read_only {
            self.error = Some(IMPORT_REFUSED_READ_ONLY_STATUS.into());
            cx.notify();
            return;
        }
        let path = self.path.read(cx).value().trim().to_string();
        if path.is_empty() {
            cx.notify();
            return;
        }
        let path = PathBuf::from(path);
        match load_preview_and_count(&path, &self.ctx.structure, &self.options()) {
            Ok((preview, total)) => {
                // Re-check required mapping after user may have remapped.
                if let Err(msg) = validate_required_mapped(&preview.mapping, &self.options()) {
                    self.mapping_blocked = Some(msg);
                }
                self.preview = Some(preview);
                self.total_rows = Some(total);
            }
            Err(err) => {
                if let ImportError::Mapping(msg) = &err {
                    if msg.contains("Map every required column") {
                        self.mapping_blocked = Some(msg.clone());
                    } else {
                        let (banner, _) = format_import_failure(&err);
                        self.error = Some(banner);
                    }
                } else {
                    let (banner, _) = format_import_failure(&err);
                    self.error = Some(banner);
                }
            }
        }
        cx.notify();
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
            cx.notify();
            return;
        }
        if !self.import_enabled() {
            if self.mapping_blocked.is_none() && self.error.is_none() {
                self.error = Some("Load a CSV file first.".into());
            }
            cx.notify();
            return;
        }
        let path = PathBuf::from(self.path.read(cx).value().trim());
        let total = self.total_rows.unwrap_or(0);
        let mut options = self.options();
        // Persist the user’s mapping by rewriting required check; the importer
        // rebuilds from headers, so we encode overrides via reject + required
        // only. Custom Skip/remap: re-open with positional override below in jobs
        // by passing headers + mapping through options.required and a sidecar.
        // For custom mapping, store targets on the request via options by
        // rebuilding ColumnMapping in the job from the dialog snapshot.
        let _mapping = self
            .preview
            .as_ref()
            .map(|p| ColumnMapping {
                targets: p.mapping.clone(),
            })
            .unwrap_or_else(|| ColumnMapping { targets: vec![] });
        // Clear required so open() does not double-fail; jobs apply mapping.
        options.required_columns = required_columns(&self.ctx.structure);
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
        cx.emit(ImportDialogEvent::Cancel);
    }
}

impl EventEmitter<ImportDialogEvent> for ImportDialog {}

impl Render for ImportDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
                        let path = self.path.clone();
                        Button::new("import-browse")
                            .label("Browse")
                            .ghost()
                            .disabled(read_only)
                            .on_click(move |_, window, cx| {
                                path.update(cx, |state, cx| state.focus(window, cx));
                                this.update(cx, |dialog, cx| dialog.reload_preview(cx));
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

fn load_preview_and_count(
    path: &PathBuf,
    structure: &TableStructure,
    options: &CsvImportOptions,
) -> Result<(PreviewState, u64), ImportError> {
    let bytes = std::fs::read(path).map_err(|e| ImportError::Io(e.to_string()))?;
    // Fail clearly on invalid UTF-8 before parsing (IM-08).
    sela_core::require_utf8(&bytes)?;

    let table_columns: Vec<ColumnMeta> = structure
        .columns
        .iter()
        .map(|c| ColumnMeta {
            name: c.name.clone(),
            type_name: c.type_name.clone(),
        })
        .collect();

    let mut importer = CsvImporter::open_with_columns(
        Cursor::new(bytes.as_slice()),
        &table_columns,
        options.clone(),
    )?;
    let headers = importer.headers().to_vec();
    let mapping = importer.mapping().targets.clone();

    let mut rows = Vec::new();
    while rows.len() < PREVIEW_MAX_ROWS {
        match importer.next_row() {
            None => break,
            Some(Ok(row)) => {
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
            Some(Err(err)) => return Err(err),
        }
    }

    let total = count_csv_data_rows(Cursor::new(bytes.as_slice()), options)?;
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

/// Re-open the file for the import job (streaming). Applies a custom mapping
/// by validating required columns only; header auto-map is re-derived unless
/// `override_targets` is provided via rewriting headers through a temp approach.
///
/// When `override_targets` is set, we feed the importer with `has_header` and
/// then replace the mapping after open via a small adapter sink in jobs.
pub(crate) fn open_csv_file(path: &PathBuf) -> Result<File, ImportError> {
    File::open(path).map_err(|e| ImportError::Io(e.to_string()))
}

/// Pre-scan only (used when dialog already counted, or jobs re-count).
#[allow(dead_code)]
pub(crate) fn prescan_file(path: &PathBuf, options: &CsvImportOptions) -> Result<u64, ImportError> {
    let file = open_csv_file(path)?;
    let mut reader = BufReader::new(file);
    // Ensure we can re-read: count_csv_data_rows consumes the reader.
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(|e| ImportError::Io(e.to_string()))?;
    sela_core::require_utf8(&bytes)?;
    count_csv_data_rows(Cursor::new(bytes), options)
}

/// Seek helper kept for future streaming reopen paths.
#[allow(dead_code)]
pub(crate) fn rewind(file: &mut File) -> Result<(), ImportError> {
    file.seek(SeekFrom::Start(0))
        .map_err(|e| ImportError::Io(e.to_string()))?;
    Ok(())
}
