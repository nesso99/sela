//! Export dialog (560px) — format, scope, path, CSV options (UX 11).

use std::path::{Path, PathBuf};

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Disableable as _, Sizable as _};
use gpui_kit::prelude::*;
#[cfg(test)]
use gpui_kit::App;
use gpui_kit::{div, Context, Entity, EventEmitter, IntoElement, Render, SharedString, Window};

use sela_core::{
    CsvExportOptions, CsvNullEncoding, Dialect, ExportFormat, JsonExportMode, JsonExportOptions,
    SqlInsertExportOptions,
};

use crate::ui::file_picker::{
    apply_browse_outcome_to_error, classify_new_path_result, FilePickerOutcome,
};

use super::messages::WHOLE_TABLE_DISABLED_REASON;

/// Export format choice in the dialog (UX: CSV / JSON / SQL INSERT).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExportFormatChoice {
    #[default]
    Csv,
    Json,
    SqlInsert,
}

impl ExportFormatChoice {
    pub fn label(self) -> &'static str {
        match self {
            Self::Csv => "CSV",
            Self::Json => "JSON",
            Self::SqlInsert => "SQL INSERT",
        }
    }
}

/// Scope: current result vs whole table (EX-01 / EX-04).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExportScope {
    #[default]
    CurrentResult,
    WholeTable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CsvDelimiterChoice {
    #[default]
    Comma,
    Semicolon,
    Tab,
}

impl CsvDelimiterChoice {
    pub fn as_byte(self) -> u8 {
        match self {
            Self::Comma => b',',
            Self::Semicolon => b';',
            Self::Tab => b'\t',
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Comma => ",",
            Self::Semicolon => ";",
            Self::Tab => "Tab",
        }
    }
}

#[derive(Clone)]
pub enum ExportDialogEvent {
    Export(ExportRequest),
    Cancel,
}

/// Validated payload when the user confirms Export.
#[derive(Debug, Clone)]
pub struct ExportRequest {
    pub format: ExportFormat,
    pub scope: ExportScope,
    pub path: PathBuf,
}

/// Context for enabling Whole table and building SQL INSERT options.
#[derive(Debug, Clone)]
pub struct ExportDialogContext {
    /// True when the active tab is a table Data pane (not SQL-only).
    pub table_data: bool,
    pub schema: Option<String>,
    pub table: Option<String>,
    pub dialect: Dialect,
    /// When set, SQL INSERT uses placeholder `table` + trailing comment.
    pub sql_result_tab: bool,
}

pub struct ExportDialog {
    ctx: ExportDialogContext,
    format: ExportFormatChoice,
    scope: ExportScope,
    path: Entity<InputState>,
    delimiter: CsvDelimiterChoice,
    include_header: bool,
    null_empty_field: bool,
    utf8_bom: bool,
    error: Option<String>,
    error_from_file_picker: bool,
    /// Browse result applied on the next render (async task has no Window).
    pending_browse_path: Option<String>,
}

impl ExportDialog {
    pub fn new(ctx: ExportDialogContext, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            ctx,
            format: ExportFormatChoice::Csv,
            scope: ExportScope::CurrentResult,
            path: cx.new(|cx| InputState::new(window, cx).placeholder("/path/to/export.csv")),
            delimiter: CsvDelimiterChoice::Comma,
            include_header: true,
            null_empty_field: true,
            utf8_bom: false,
            error: None,
            error_from_file_picker: false,
            pending_browse_path: None,
        }
    }

    /// Inject a destination path (headless tests; native Browse also fills this field).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn set_path(
        &mut self,
        path: impl Into<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let path = path.into();
        self.path.update(cx, |input, cx| {
            input.set_value(&path, window, cx);
        });
        cx.notify();
    }

    fn browse_save_path(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let typed = self.path.read(cx).value().trim().to_string();
        let (dir, suggested) = export_browse_defaults(&typed);
        let receiver = cx.prompt_for_new_path(&dir, suggested.as_deref());
        cx.spawn(async move |this, async_cx| {
            let outcome = classify_new_path_result(receiver.await);
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
        }
    }

    #[cfg(test)]
    pub fn test_path(&self, cx: &App) -> String {
        self.path.read(cx).value().to_string()
    }

    #[cfg(test)]
    pub fn test_format(&self) -> ExportFormatChoice {
        self.format
    }

    #[cfg(test)]
    pub fn test_scope(&self) -> ExportScope {
        self.scope
    }

    fn apply_browse_outcome(&mut self, outcome: FilePickerOutcome) {
        wire_export_browse(
            &mut self.error,
            &mut self.error_from_file_picker,
            &mut self.pending_browse_path,
            outcome,
        );
    }

    pub fn whole_table_enabled(&self) -> bool {
        self.ctx.table_data
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn whole_table_disabled_reason(&self) -> Option<&'static str> {
        if self.ctx.table_data {
            None
        } else {
            Some(WHOLE_TABLE_DISABLED_REASON)
        }
    }

    fn build_format(&self) -> ExportFormat {
        match self.format {
            ExportFormatChoice::Csv => ExportFormat::Csv(CsvExportOptions {
                delimiter: self.delimiter.as_byte(),
                include_header: self.include_header,
                null_encoding: if self.null_empty_field {
                    CsvNullEncoding::EmptyField
                } else {
                    CsvNullEncoding::LiteralBackslashN
                },
                utf8_bom: self.utf8_bom,
                ..CsvExportOptions::default()
            }),
            ExportFormatChoice::Json => ExportFormat::Json(JsonExportOptions {
                mode: JsonExportMode::Array,
                ..JsonExportOptions::default()
            }),
            ExportFormatChoice::SqlInsert => {
                let opts = if self.ctx.sql_result_tab {
                    SqlInsertExportOptions::placeholder(self.ctx.dialect)
                } else if let (Some(schema), Some(table)) =
                    (self.ctx.schema.as_deref(), self.ctx.table.as_deref())
                {
                    SqlInsertExportOptions::with_schema(self.ctx.dialect, schema, table)
                } else {
                    SqlInsertExportOptions::placeholder(self.ctx.dialect)
                };
                ExportFormat::SqlInsert(opts)
            }
        }
    }

    fn confirm_export(&mut self, cx: &mut Context<Self>) {
        let path = self.path.read(cx).value().trim().to_string();
        if path.is_empty() {
            self.error = Some("Choose a file path.".into());
            self.error_from_file_picker = false;
            cx.notify();
            return;
        }
        if self.scope == ExportScope::WholeTable && !self.ctx.table_data {
            self.error = Some(WHOLE_TABLE_DISABLED_REASON.into());
            self.error_from_file_picker = false;
            cx.notify();
            return;
        }
        self.error = None;
        self.error_from_file_picker = false;
        cx.emit(ExportDialogEvent::Export(ExportRequest {
            format: self.build_format(),
            scope: self.scope,
            path: PathBuf::from(path),
        }));
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        cx.emit(ExportDialogEvent::Cancel);
    }
}

impl EventEmitter<ExportDialogEvent> for ExportDialog {}

impl Render for ExportDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.flush_pending_browse(window, cx);
        let this = cx.entity();
        let whole_enabled = self.whole_table_enabled();
        let show_csv = self.format == ExportFormatChoice::Csv;

        v_flex()
            .w_full()
            .gap_2()
            .p_1()
            .child(section_label("Format", cx))
            .child(
                h_flex().gap_1().children(
                    [
                        ExportFormatChoice::Csv,
                        ExportFormatChoice::Json,
                        ExportFormatChoice::SqlInsert,
                    ]
                    .into_iter()
                    .map(|choice| {
                        let this = this.clone();
                        let selected = self.format == choice;
                        Button::new(SharedString::from(format!(
                            "export-format-{}",
                            choice.label()
                        )))
                        .label(choice.label())
                        .xsmall()
                        .when(!selected, |b| b.ghost())
                        .when(selected, |b| b.primary())
                        .on_click(move |_, _, cx| {
                            this.update(cx, |dialog, cx| {
                                dialog.format = choice;
                                cx.notify();
                            });
                        })
                    }),
                ),
            )
            .child(section_label("Scope", cx))
            .child(
                h_flex()
                    .gap_1()
                    .child({
                        let this = this.clone();
                        let selected = self.scope == ExportScope::CurrentResult;
                        Button::new("export-scope-current")
                            .label("Current result")
                            .xsmall()
                            .when(!selected, |b| b.ghost())
                            .when(selected, |b| b.primary())
                            .on_click(move |_, _, cx| {
                                this.update(cx, |dialog, cx| {
                                    dialog.scope = ExportScope::CurrentResult;
                                    cx.notify();
                                });
                            })
                    })
                    .child({
                        let this = this.clone();
                        let selected = self.scope == ExportScope::WholeTable;
                        Button::new("export-scope-whole")
                            .label("Whole table")
                            .xsmall()
                            .disabled(!whole_enabled)
                            .when(!selected, |b| b.ghost())
                            .when(selected && whole_enabled, |b| b.primary())
                            .tooltip(if whole_enabled {
                                "Export every row of the table (honours filter/sort)"
                            } else {
                                WHOLE_TABLE_DISABLED_REASON
                            })
                            .on_click(move |_, _, cx| {
                                this.update(cx, |dialog, cx| {
                                    if dialog.ctx.table_data {
                                        dialog.scope = ExportScope::WholeTable;
                                        cx.notify();
                                    }
                                });
                            })
                    }),
            )
            .when(!whole_enabled, |col| {
                col.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(WHOLE_TABLE_DISABLED_REASON),
                )
            })
            .child(section_label("File", cx))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .child(Input::new(&self.path).id("export-path-input")),
                    )
                    .child({
                        let this = this.clone();
                        Button::new("export-browse")
                            .label("Browse")
                            .ghost()
                            .on_click(move |_, window, cx| {
                                this.update(cx, |dialog, cx| {
                                    dialog.browse_save_path(window, cx);
                                });
                            })
                    }),
            )
            .when(show_csv, |col| {
                col.child(section_label("CSV options", cx))
                    .child(
                        h_flex()
                            .gap_1()
                            .child(div().text_xs().child("Delimiter:"))
                            .children(
                                [
                                    CsvDelimiterChoice::Comma,
                                    CsvDelimiterChoice::Semicolon,
                                    CsvDelimiterChoice::Tab,
                                ]
                                .into_iter()
                                .map(|choice| {
                                    let this = this.clone();
                                    let selected = self.delimiter == choice;
                                    Button::new(SharedString::from(format!(
                                        "export-delim-{}",
                                        choice.label()
                                    )))
                                    .label(choice.label())
                                    .xsmall()
                                    .when(!selected, |b| b.ghost())
                                    .when(selected, |b| b.primary())
                                    .on_click(
                                        move |_, _, cx| {
                                            this.update(cx, |dialog, cx| {
                                                dialog.delimiter = choice;
                                                cx.notify();
                                            });
                                        },
                                    )
                                }),
                            ),
                    )
                    .child({
                        let this = this.clone();
                        let on = self.include_header;
                        Button::new("export-header")
                            .label(if on {
                                "Header row: on"
                            } else {
                                "Header row: off"
                            })
                            .xsmall()
                            .ghost()
                            .on_click(move |_, _, cx| {
                                this.update(cx, |dialog, cx| {
                                    dialog.include_header = !dialog.include_header;
                                    cx.notify();
                                });
                            })
                    })
                    .child({
                        let this = this.clone();
                        let empty = self.null_empty_field;
                        Button::new("export-null")
                            .label(if empty {
                                "NULL as: Empty field"
                            } else {
                                "NULL as: Literal \\N"
                            })
                            .xsmall()
                            .ghost()
                            .on_click(move |_, _, cx| {
                                this.update(cx, |dialog, cx| {
                                    dialog.null_empty_field = !dialog.null_empty_field;
                                    cx.notify();
                                });
                            })
                    })
                    .child({
                        let this = this.clone();
                        let bom = self.utf8_bom;
                        Button::new("export-bom")
                            .label(if bom {
                                "UTF-8 with BOM: on"
                            } else {
                                "UTF-8 with BOM: off"
                            })
                            .xsmall()
                            .ghost()
                            .on_click(move |_, _, cx| {
                                this.update(cx, |dialog, cx| {
                                    dialog.utf8_bom = !dialog.utf8_bom;
                                    cx.notify();
                                });
                            })
                    })
            })
            .when_some(self.error.clone(), |col, err| {
                col.child(div().text_xs().text_color(cx.theme().danger).child(err))
            })
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .mt_2()
                    .child({
                        let this = this.clone();
                        Button::new("export-dialog-cancel")
                            .label("Cancel")
                            .ghost()
                            .on_click(move |_, _, cx| {
                                this.update(cx, |dialog, cx| dialog.cancel(cx));
                            })
                    })
                    .child({
                        let this = this.clone();
                        Button::new("export-dialog-export")
                            .label("Export")
                            .primary()
                            .on_click(move |_, _, cx| {
                                this.update(cx, |dialog, cx| dialog.confirm_export(cx));
                            })
                    }),
            )
    }
}

fn section_label(text: &str, cx: &Context<ExportDialog>) -> impl IntoElement {
    div()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(text.to_string())
}

/// Initial directory + suggested name for `prompt_for_new_path`.
fn export_browse_defaults(typed: &str) -> (PathBuf, Option<String>) {
    if typed.is_empty() {
        let dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        return (dir, Some("export.csv".into()));
    }
    let path = PathBuf::from(typed);
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let suggested = path
        .file_name()
        .and_then(|s| s.to_str())
        .map(str::to_string);
    (dir, suggested)
}

fn wire_export_browse(
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

    use super::wire_export_browse;
    use crate::ui::file_picker::FilePickerOutcome;

    #[test]
    fn export_browse_failed_sets_clean_display_error() {
        let mut err = None;
        let mut from_picker = false;
        let mut pending = None;
        wire_export_browse(
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
    fn export_browse_cancel_keeps_validation_error() {
        let mut err = Some("Choose a file path.".into());
        let mut from_picker = false;
        let mut pending = None;
        wire_export_browse(
            &mut err,
            &mut from_picker,
            &mut pending,
            FilePickerOutcome::Cancelled,
        );
        assert_eq!(err.as_deref(), Some("Choose a file path."));
        assert!(!from_picker);
    }

    #[test]
    fn export_browse_picked_clears_picker_error_only() {
        let mut err = Some("Couldn't open file picker".into());
        let mut from_picker = true;
        let mut pending = None;
        wire_export_browse(
            &mut err,
            &mut from_picker,
            &mut pending,
            FilePickerOutcome::Picked(PathBuf::from("/tmp/x.csv")),
        );
        assert!(err.is_none());
        assert!(!from_picker);
        assert_eq!(pending.as_deref(), Some("/tmp/x.csv"));
    }
}
