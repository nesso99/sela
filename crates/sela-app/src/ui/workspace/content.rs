//! Tab / content area: toolbar, results grid, and SQL editor.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::Textarea;
use gpui_kit::component::table::DataTable;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Disableable as _, Sizable as _};
use gpui_kit::prelude::*;
use gpui_kit::{div, px, Context, IntoElement};

use super::Workspace;

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
        h_flex()
            .h(px(44.))
            .px_3()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                v_flex().child(self.source_label()).child(
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
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(page_label.to_string()),
                    )
                    .child({
                        let this = this.clone();
                        Button::new("page-prev")
                            .xsmall()
                            .label("Previous")
                            .disabled(!can_prev || self.busy || !connected)
                            .on_click(move |_, _, cx| {
                                this.update(cx, |workspace, cx| workspace.step_page(-1, cx));
                            })
                    })
                    .child({
                        let this = this;
                        Button::new("page-next")
                            .xsmall()
                            .label("Next")
                            .disabled(!can_next || self.busy || !connected)
                            .on_click(move |_, _, cx| {
                                this.update(cx, |workspace, cx| workspace.step_page(1, cx));
                            })
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
        div()
            .flex_1()
            .min_h(px(0.))
            .overflow_hidden()
            .child(DataTable::new(&self.grid).stripe(true).bordered(true))
    }

    pub(super) fn sql_editor(&self, cx: &Context<Self>, connected: bool) -> impl IntoElement {
        let this = cx.entity();
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
                                    .label(if self.busy { "Running…" } else { "Run" })
                                    .disabled(self.busy || !connected)
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
                                    .disabled(!self.busy || !connected)
                                    .tooltip("Cancel the running query (Ctrl+. / ⌘+.)")
                                    .on_click(move |_, _, cx| {
                                        this.update(cx, |workspace, cx| workspace.stop_query(cx));
                                    })
                            })
                            .when(self.truncated && self.can_load_more && !self.busy, |row| {
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
                                matches!(self.source, super::GridSource::Table { .. })
                                    && matches!(
                                        self.row_count_kind,
                                        sela_core::RowCountKind::Estimated
                                            | sela_core::RowCountKind::Unknown
                                    )
                                    && !self.busy,
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
            .child(
                div().flex_1().px_2().pb_2().child(
                    Textarea::new(&self.sql).font_family(cx.theme().mono_font_family.clone()),
                ),
            )
    }
}
