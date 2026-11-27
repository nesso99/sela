//! Content area: toolbar, results grid, and SQL editor.
//!
//! Tab chrome lives in `tabs.rs`. Structure pane UI (M1-6) is per active tab.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::table::DataTable;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Disableable as _, Sizable as _, Size};
use gpui_kit::prelude::*;
use gpui_kit::{div, px, Context, IntoElement, Window};

use sela_config::{content_row_height, DEFAULT_FONT_SIZE};
use sela_core::{ColumnKind, ConnectionColor};

use super::super::long_value_popover::{
    clamp_popover_anchor, long_value_popover, LongValuePopover, LongValuePopoverInput,
};
use super::super::theme::SelaPalette;
use super::{TablePane, Workspace};

impl Workspace {
    pub(super) fn toolbar(
        &self,
        cx: &Context<Self>,
        page_label: &str,
        can_prev: bool,
        can_next: bool,
        connected: bool,
    ) -> impl IntoElement {
        let this = cx.entity();
        let busy = self.active_view().map(|view| view.busy).unwrap_or(false);
        let table_pane = self
            .active_view()
            .map(|view| view.table_pane)
            .unwrap_or(TablePane::Data);
        let label = self
            .active_view()
            .map(|view| view.source_label())
            .unwrap_or_else(|| "No table open".into());
        let conn_color = self
            .active_connection()
            .map(|c| c.color)
            .unwrap_or(ConnectionColor::None);
        let palette = SelaPalette::for_mode(cx.theme().mode);
        let strip = palette.connection_color(conn_color);
        let (endpoint, safe_mode_segment) = self
            .active_connection()
            .map(|connection| {
                (
                    format!(
                        "{} · {}",
                        connection.engine.label(),
                        connection.endpoint_label()
                    ),
                    Some(connection.safe_mode.toolbar_segment()),
                )
            })
            .unwrap_or_else(|| ("Choose a connection to browse its tables.".into(), None));
        h_flex()
            .h(px(44.))
            .items_stretch()
            .border_b_1()
            .border_color(cx.theme().border)
            .when_some(strip, |row, color| {
                row.child(div().w(px(3.)).flex_shrink_0().bg(color))
            })
            .child(
                h_flex()
                    .flex_1()
                    .h_full()
                    .px_3()
                    .items_center()
                    .justify_between()
                    .child(
                        v_flex().min_w(px(0.)).flex_1().child(label).child(
                            h_flex()
                                .min_w(px(0.))
                                .w_full()
                                .gap_1()
                                .items_center()
                                .child(
                                    // Endpoint ellipsizes before the Safe: segment.
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .min_w(px(0.))
                                        .flex_1()
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_ellipsis()
                                        .child(endpoint),
                                )
                                .when_some(safe_mode_segment, |row, safe| {
                                    row.child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .flex_shrink_0()
                                            .child(format!("· {safe}")),
                                    )
                                }),
                        ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .when(matches!(table_pane, TablePane::Data), |row| {
                                let this_filter = this.clone();
                                let this_prev = this.clone();
                                let this_next = this.clone();
                                let filter_open = self
                                    .active_view()
                                    .map(|view| view.filter_bar_open)
                                    .unwrap_or(false);
                                let filter_applied = self
                                    .active_view()
                                    .map(|view| !view.filters.is_empty())
                                    .unwrap_or(false);
                                row.when(self.table_open(), |row| {
                                    row.child(
                                        Button::new("filter-toggle")
                                            .xsmall()
                                            .label(if filter_open {
                                                "Filter ▾"
                                            } else {
                                                "Filter"
                                            })
                                            .primary()
                                            // Stay selected when a filter is applied even if the bar is closed.
                                            .when(!filter_open && !filter_applied, |button| {
                                                button.ghost()
                                            })
                                            .disabled(!connected)
                                            .tooltip("Toggle filter bar (Ctrl/⌘+F)")
                                            .on_click(move |_, window, cx| {
                                                this_filter.update(cx, |workspace, cx| {
                                                    workspace.toggle_filter_bar(window, cx)
                                                });
                                            }),
                                    )
                                })
                                .when(self.table_open(), |row| {
                                    let this_ins = this.clone();
                                    let this_dup = this.clone();
                                    let this_del = this.clone();
                                    let safe_mode = self.active_safe_mode();
                                    let edit_ro = self
                                        .active_view()
                                        .and_then(|v| v.edit.as_ref())
                                        .is_none_or(|e| !e.is_editable_for(safe_mode));
                                    let ro_tip = self
                                        .active_view()
                                        .and_then(|v| v.edit.as_ref())
                                        .and_then(|e| e.strip_line_for(safe_mode))
                                        .unwrap_or("This table is read-only.");
                                    row.child(
                                        Button::new("edit-insert")
                                            .xsmall()
                                            .ghost()
                                            .label("Insert")
                                            .disabled(!connected || edit_ro || busy)
                                            .tooltip(if edit_ro {
                                                ro_tip
                                            } else {
                                                "Insert row (Ctrl/⌘+I)"
                                            })
                                            .on_click(move |_, _, cx| {
                                                this_ins.update(cx, |workspace, cx| {
                                                    workspace.insert_edit_row(cx)
                                                });
                                            }),
                                    )
                                    .child(
                                        Button::new("edit-duplicate")
                                            .xsmall()
                                            .ghost()
                                            .label("Duplicate")
                                            .disabled(!connected || edit_ro || busy)
                                            .tooltip(if edit_ro {
                                                ro_tip
                                            } else {
                                                "Duplicate row (Ctrl/⌘+D)"
                                            })
                                            .on_click(move |_, _, cx| {
                                                this_dup.update(cx, |workspace, cx| {
                                                    workspace.duplicate_edit_row(cx)
                                                });
                                            }),
                                    )
                                    .child(
                                        Button::new("edit-delete")
                                            .xsmall()
                                            .ghost()
                                            .label("Delete")
                                            .disabled(!connected || edit_ro || busy)
                                            .tooltip(if edit_ro {
                                                ro_tip
                                            } else {
                                                "Delete / restore row (Ctrl/⌘+Backspace)"
                                            })
                                            .on_click(move |_, _, cx| {
                                                this_del.update(cx, |workspace, cx| {
                                                    workspace.delete_edit_row(cx)
                                                });
                                            }),
                                    )
                                })
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(page_label.to_string()),
                                )
                                .child(
                                    Button::new("page-prev")
                                        .xsmall()
                                        .label("Previous")
                                        .disabled(!can_prev || busy || !connected)
                                        .on_click(move |_, _, cx| {
                                            this_prev.update(cx, |workspace, cx| {
                                                workspace.step_page(-1, cx)
                                            });
                                        }),
                                )
                                .child(
                                    Button::new("page-next")
                                        .xsmall()
                                        .label("Next")
                                        .disabled(!can_next || busy || !connected)
                                        .on_click(move |_, _, cx| {
                                            this_next.update(cx, |workspace, cx| {
                                                workspace.step_page(1, cx)
                                            });
                                        }),
                                )
                            })
                            .when(self.table_open(), |row| {
                                let this_data = this.clone();
                                let this_structure = this.clone();
                                let this_refresh = this.clone();
                                let on_data = matches!(table_pane, TablePane::Data);
                                let on_structure = matches!(table_pane, TablePane::Structure);
                                row.child(
                            h_flex()
                                .gap_1()
                                .when(on_structure, |row| {
                                    row.child(
                                        Button::new("pane-structure-refresh")
                                            .xsmall()
                                            .label("Refresh")
                                            .disabled(!connected || busy)
                                            .tooltip("Reload structure for this table")
                                            .on_click(move |_, _, cx| {
                                                this_refresh.update(cx, |workspace, cx| {
                                                    workspace.refresh_structure(cx)
                                                });
                                            }),
                                    )
                                })
                                .child(
                                    Button::new("pane-data")
                                        .xsmall()
                                        .label("Data")
                                        .primary()
                                        .when(!on_data, |button| button.ghost())
                                        .disabled(!connected)
                                        .tooltip("Show table rows (Ctrl/⌘+Shift+[)")
                                        .on_click(move |_, _, cx| {
                                            this_data.update(cx, |workspace, cx| {
                                                workspace.show_table_data(cx)
                                            });
                                        }),
                                )
                                .child(
                                    Button::new("pane-structure")
                                        .xsmall()
                                        .label("Structure")
                                        .primary()
                                        .when(!on_structure, |button| button.ghost())
                                        .disabled(!connected)
                                        .tooltip("Show columns, indexes, FKs, DDL (Ctrl/⌘+Shift+])")
                                        .on_click(move |_, _, cx| {
                                            this_structure.update(cx, |workspace, cx| {
                                                workspace.show_table_structure(cx)
                                            });
                                        }),
                                )
                        )
                            }),
                    ),
            )
    }

    pub(super) fn error_banner(&self, cx: &Context<Self>, error: String) -> impl IntoElement {
        let palette = super::super::theme::SelaPalette::for_mode(cx.theme().mode);
        let this = cx.entity();
        let show_refresh = self
            .active_view()
            .and_then(|v| v.edit.as_ref())
            .is_some_and(|e| e.commit_conflict);
        h_flex()
            .max_h(px(60.))
            .overflow_y_scrollbar()
            .px_3()
            .py_2()
            .gap_2()
            .items_start()
            .justify_between()
            .bg(palette.status_error_wash)
            .text_color(palette.status_error)
            .child(div().text_sm().flex_1().min_w(px(0.)).child(error))
            .when(show_refresh, |row| {
                row.child(
                    Button::new("conflict-refresh")
                        .xsmall()
                        .ghost()
                        .label("Refresh")
                        .tooltip("Drop pending edits and reload this page")
                        .on_click({
                            let this = this.clone();
                            move |_, _, cx| {
                                this.update(cx, |workspace, cx| {
                                    workspace.refresh_after_commit_conflict(cx);
                                });
                            }
                        }),
                )
            })
    }

    /// Non-error status when no tab is open (e.g. Duplicate).
    pub(super) fn workspace_status_banner(
        &self,
        cx: &Context<Self>,
        status: String,
    ) -> impl IntoElement {
        div()
            .px_3()
            .py_2()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(status)
    }

    /// Approximate results-area viewport for popover placement (relative origin).
    pub(crate) fn results_area_viewport(&self, window: &Window) -> (f32, f32) {
        let bounds = window.bounds().size;
        const SIDEBAR: f32 = 292.;
        const TITLE: f32 = 44.;
        const TAB_BAR: f32 = 28.;
        const TOOLBAR: f32 = 36.;
        const FILTER_BAR: f32 = 72.;
        const PENDING_BAR: f32 = 26.;
        const RESULT_STRIP: f32 = 24.;
        const STATUS_BAR: f32 = 28.;

        let mut chrome_above = TITLE + TAB_BAR + TOOLBAR;
        if let Some(view) = self.active_view() {
            if view.filter_bar_open {
                chrome_above += FILTER_BAR;
            }
            if view.edit.as_ref().is_some_and(|e| e.changeset.is_some()) {
                chrome_above += PENDING_BAR;
            }
        }
        if self.banner_error().is_some() {
            chrome_above += 40.;
        }

        let mut chrome_below = RESULT_STRIP + STATUS_BAR;
        if let Some(view) = self.active_view() {
            chrome_below += view.editor_body_height();
        }

        let width = (f32::from(bounds.width) - SIDEBAR).max(0.);
        let height = (f32::from(bounds.height) - chrome_above - chrome_below).max(0.);
        (width.max(100.), height.max(100.))
    }

    pub(super) fn results_area(&self, window: &Window, cx: &Context<Self>) -> impl IntoElement {
        let this = cx.entity();
        let grid = self.active_view().map(|view| view.grid.clone());
        let inspect = self
            .active_view()
            .and_then(|view| view.cell_inspect.clone());
        let viewport = self.results_area_viewport(window);
        let long_value = self.active_view().and_then(|view| {
            let ed = view.edit.as_ref()?.editor.as_ref().filter(|e| e.popover)?;
            let (anchor_left, anchor_top) = clamp_popover_anchor(
                view.popover_anchor.0,
                view.popover_anchor.1,
                viewport.0,
                viewport.1,
            );
            let input = if matches!(ed.kind, ColumnKind::Json) {
                LongValuePopoverInput::Json(&view.popover_editor)
            } else {
                LongValuePopoverInput::Text(&view.popover_textarea)
            };
            let info = LongValuePopover {
                title: format!(
                    "{} · {}",
                    ed.column,
                    super::edit_session::kind_label(ed.kind)
                ),
                error: ed.validation_error.clone(),
                anchor_left,
                anchor_top,
            };
            Some((input, info))
        });
        let row_h = content_row_height(self.font_size);
        let table_size = if self.font_size <= DEFAULT_FONT_SIZE {
            Size::XSmall
        } else {
            Size::Size(px(f32::from(row_h)))
        };
        div()
            .flex_1()
            .min_h(px(0.))
            .overflow_hidden()
            .relative()
            .when_some(grid, |area, grid| {
                area.child(
                    DataTable::new(&grid)
                        .stripe(true)
                        .bordered(true)
                        .with_size(table_size),
                )
            })
            .when_some(long_value, |area, (input, info)| {
                area.child(long_value_popover(input, info, cx))
            })
            .when_some(inspect, |area, info| {
                let title = if info.decode_error {
                    "Decode error · not NULL".to_string()
                } else {
                    format!("{} · {}", info.column, info.type_name)
                };
                let subtitle = if info.decode_error {
                    format!("{} · {}", info.column, info.type_name)
                } else {
                    String::new()
                };
                area.child(
                    div()
                        .absolute()
                        .top(px(info.anchor_top))
                        .left(px(info.anchor_left))
                        .max_w(px(420.))
                        .max_h(px(280.))
                        .flex()
                        .flex_col()
                        .border_1()
                        .border_color(cx.theme().border)
                        .rounded(px(6.))
                        .bg(cx.theme().background)
                        .key_context("CellInspect")
                        .child(
                            h_flex()
                                .px_3()
                                .pt_2()
                                .pb_1()
                                .items_center()
                                .justify_between()
                                .gap_2()
                                .child(
                                    v_flex()
                                        .gap_0()
                                        .child(
                                            div()
                                                .text_sm()
                                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                                .child(title),
                                        )
                                        .when(!subtitle.is_empty(), |col| {
                                            col.child(
                                                div()
                                                    .text_xs()
                                                    .text_color(cx.theme().muted_foreground)
                                                    .child(subtitle),
                                            )
                                        }),
                                )
                                .child(
                                    Button::new("inspect-close")
                                        .xsmall()
                                        .ghost()
                                        .label("×")
                                        .tooltip("Close inspect (Escape)")
                                        .on_click({
                                            let this = this.clone();
                                            move |_, _, cx| {
                                                this.update(cx, |workspace, cx| {
                                                    if let Some(view) = workspace.active_view_mut()
                                                    {
                                                        view.cell_inspect = None;
                                                    }
                                                    cx.notify();
                                                });
                                            }
                                        }),
                                ),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_h(px(0.))
                                .px_3()
                                .pb_2()
                                .overflow_y_scrollbar()
                                .font_family(cx.theme().mono_font_family.clone())
                                .text_xs()
                                .child(info.body),
                        ),
                )
            })
    }
}
