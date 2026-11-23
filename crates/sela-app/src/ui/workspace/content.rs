//! Content area: toolbar, results grid, and SQL editor.
//!
//! Tab chrome lives in `tabs.rs`. Structure pane UI (M1-6) is per active tab.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::Textarea;
use gpui_kit::component::table::DataTable;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Disableable as _, Sizable as _};
use gpui_kit::prelude::*;
use gpui_kit::{div, px, Context, IntoElement};

use super::{TablePane, Workspace};

impl Workspace {
    pub(super) fn toolbar(
        &self,
        cx: &Context<Self>,
        endpoint: &str,
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
        h_flex()
            .h(px(44.))
            .px_3()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                v_flex().child(label).child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(endpoint.to_string()),
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
                        let this_next = this;
                        let filter_open = self
                            .active_view()
                            .map(|view| view.filter_bar_open)
                            .unwrap_or(false);
                        row.when(self.table_open(), |row| {
                            row.child(
                                Button::new("filter-toggle")
                                    .xsmall()
                                    .label(if filter_open { "Filter ▾" } else { "Filter" })
                                    .primary()
                                    .when(!filter_open, |button| button.ghost())
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
        div()
            .px_3()
            .py_2()
            .text_sm()
            .bg(cx.theme().danger)
            .text_color(cx.theme().danger_foreground)
            .child(error)
    }

    pub(super) fn results_area(&self) -> impl IntoElement {
        let grid = self.active_view().map(|view| view.grid.clone());
        div()
            .flex_1()
            .min_h(px(0.))
            .overflow_hidden()
            .when_some(grid, |area, grid| {
                area.child(DataTable::new(&grid).stripe(true).bordered(true))
            })
    }

    pub(super) fn sql_editor(&self, cx: &Context<Self>, connected: bool) -> impl IntoElement {
        let this = cx.entity();
        let view = self.active_view();
        let busy = view.map(|v| v.busy).unwrap_or(false);
        let truncated = view.map(|v| v.truncated).unwrap_or(false);
        let can_load_more = view.map(|v| v.can_load_more).unwrap_or(false);
        let is_table = view
            .map(|v| matches!(v.source, super::GridSource::Table { .. }))
            .unwrap_or(false);
        let row_count_kind = view
            .map(|v| v.row_count_kind)
            .unwrap_or(sela_core::RowCountKind::Unknown);
        let sql = view.map(|v| v.sql.clone());

        v_flex()
            .h(px(176.))
            .border_t_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .px_3()
                    .h(px(28.))
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("SQL"),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child({
                                let this = this.clone();
                                Button::new("run-query")
                                    .small()
                                    .primary()
                                    .label(if busy { "Running…" } else { "Run" })
                                    .disabled(busy || !connected || sql.is_none())
                                    .tooltip("Run the statement (Ctrl+Enter / ⌘+Enter)")
                                    .on_click(move |_, _, cx| {
                                        this.update(cx, |workspace, cx| workspace.run_editor(cx));
                                    })
                            })
                            .child({
                                let this = this.clone();
                                Button::new("stop-query")
                                    .small()
                                    .label("Stop")
                                    .disabled(!busy || !connected)
                                    .tooltip("Cancel the running query (Ctrl+. / ⌘+.)")
                                    .on_click(move |_, _, cx| {
                                        this.update(cx, |workspace, cx| workspace.stop_query(cx));
                                    })
                            })
                            .when(truncated && can_load_more && !busy, |row| {
                                let this = this.clone();
                                row.child(
                                    Button::new("load-more")
                                        .small()
                                        .label("Load more")
                                        .disabled(!connected)
                                        .on_click(move |_, _, cx| {
                                            this.update(cx, |workspace, cx| {
                                                workspace.load_more(cx)
                                            });
                                        }),
                                )
                            })
                            .when(
                                is_table
                                    && matches!(
                                        row_count_kind,
                                        sela_core::RowCountKind::Estimated
                                            | sela_core::RowCountKind::Unknown
                                    )
                                    && !busy,
                                |row| {
                                    let this = this;
                                    row.child(
                                        Button::new("exact-count")
                                            .small()
                                            .label("Exact count")
                                            .disabled(!connected)
                                            .on_click(move |_, _, cx| {
                                                this.update(cx, |workspace, cx| {
                                                    workspace.request_exact_count(cx)
                                                });
                                            }),
                                    )
                                },
                            ),
                    ),
            )
            .when_some(sql, |column, sql| {
                column.child(
                    div().flex_1().px_2().pb_2().child(
                        Textarea::new(&sql).font_family(cx.theme().mono_font_family.clone()),
                    ),
                )
            })
    }
}
