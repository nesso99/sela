//! Bottom status bar.

use gpui_kit::component::button::Button;
use gpui_kit::component::{h_flex, ActiveTheme, Disableable as _, Sizable as _};
use gpui_kit::prelude::*;
use gpui_kit::{px, Context, IntoElement};

use sela_core::RowCountKind;

use super::Workspace;

impl Workspace {
    pub(super) fn status_bar(&self, cx: &Context<Self>) -> impl IntoElement {
        let this = cx.entity();
        let view = self.active_view();
        let status = view.map(|v| v.status.clone()).unwrap_or_default();
        let label = view
            .map(|v| v.source_label())
            .unwrap_or_else(|| "No table open".into());
        let connected = self.session.is_some();
        let busy = view.map(|v| v.busy).unwrap_or(false);
        let show_count = view.is_some_and(|v| {
            matches!(v.source, super::GridSource::Table { .. })
                && matches!(
                    v.row_count_kind,
                    RowCountKind::Estimated | RowCountKind::Unknown
                )
                && !v.busy
        });
        let filtered = view.is_some_and(|v| !v.filters.is_empty());
        let count_tooltip = if filtered {
            "Filtered results are not estimated. Count runs COUNT(*) for this filter."
        } else {
            "No estimate for this table. Count runs COUNT(*)."
        };

        h_flex()
            .h(px(28.))
            .px_3()
            .items_center()
            .justify_between()
            .border_t_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().muted)
            .text_xs()
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(status)
                    .when(show_count, |row| {
                        row.child({
                            Button::new("status-count")
                                .xsmall()
                                .label("Count")
                                .disabled(!connected || busy)
                                .tooltip(count_tooltip)
                                .on_click(move |_, _, cx| {
                                    this.update(cx, |workspace, cx| {
                                        workspace.request_exact_count(cx)
                                    });
                                })
                        })
                    }),
            )
            .child(label)
    }
}
