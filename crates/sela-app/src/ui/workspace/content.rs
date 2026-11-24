//! Content area: toolbar, results grid, and SQL editor.
//!
//! Tab chrome lives in `tabs.rs`. Structure pane UI (M1-6) is per active tab.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::table::DataTable;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Disableable as _, Sizable as _};
use gpui_kit::prelude::*;
use gpui_kit::{div, px, Context, IntoElement};

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
            .px_3()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(cx.theme().border)
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
                    .when(self.table_open(), |row| {
                        let this_data = this.clone();
                        let this_structure = this.clone();
                        let this_refresh = this.clone();
                        let on_data = matches!(table_pane, TablePane::Data);
                        let on_structure = matches!(table_pane, TablePane::Structure);
                        row.child(
                            h_flex()
                                .gap_1()
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
                                }),
                        )
                    })
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
                                    .label(if filter_open { "Filter ▾" } else { "Filter" })
                                    .primary()
                                    // Stay selected when a filter is applied even if the bar is closed.
                                    .when(!filter_open && !filter_applied, |button| button.ghost())
                                    .disabled(!connected)
                                    .tooltip("Toggle filter bar (Ctrl/⌘+F)")
                                    .on_click(move |_, window, cx| {
                                        this_filter.update(cx, |workspace, cx| {
                                            workspace.toggle_filter_bar(window, cx)
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
                                    this_prev
                                        .update(cx, |workspace, cx| workspace.step_page(-1, cx));
                                }),
                        )
                        .child(
                            Button::new("page-next")
                                .xsmall()
                                .label("Next")
                                .disabled(!can_next || busy || !connected)
                                .on_click(move |_, _, cx| {
                                    this_next
                                        .update(cx, |workspace, cx| workspace.step_page(1, cx));
                                }),
                        )
                    }),
            )
    }

    pub(super) fn error_banner(&self, cx: &Context<Self>, error: String) -> impl IntoElement {
        let palette = super::super::theme::SelaPalette::for_mode(cx.theme().mode);
        div()
            .max_h(px(60.))
            .overflow_y_scrollbar()
            .px_3()
            .py_2()
            .text_sm()
            .bg(palette.status_error_wash)
            .text_color(palette.status_error)
            .child(error)
    }

    pub(super) fn results_area(&self) -> impl IntoElement {
        let grid = self.active_view().map(|view| view.grid.clone());
        div()
            .flex_1()
            .min_h(px(0.))
            .overflow_hidden()
            .when_some(grid, |area, grid| {
                area.child(DataTable::new(&grid).stripe(true).bordered(true).xsmall())
            })
    }
}
