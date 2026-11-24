//! Result-set strip between the grid and the SQL editor.
//!
//! The strip is a tab stop (`ResultStrip` key context). Left/Right and the
//! Prev/Next buttons move between stored SQL result snapshots.

use gpui_kit::component::button::Button;
use gpui_kit::component::{h_flex, ActiveTheme, Disableable as _, Sizable as _};
use gpui_kit::prelude::*;
use gpui_kit::{div, px, Context, IntoElement};

use super::{GridSource, Workspace};

impl Workspace {
    pub(super) fn step_result_strip(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(tab_id) = self.tabs.active().map(|t| t.id) else {
            return;
        };
        let Some(view) = self.views.get(&tab_id) else {
            return;
        };
        let Some(next) = sela_core::step_result_index(
            view.active_result_index,
            view.result_snapshots.len(),
            delta,
        ) else {
            return;
        };
        self.show_result_snapshot(tab_id, next, cx);
    }

    pub(super) fn result_strip(&self, cx: &Context<Self>) -> impl IntoElement {
        let this = cx.entity();
        let view = self.active_view();
        let count = view.map(|v| v.result_snapshots.len()).unwrap_or(0);
        if count == 0 {
            return div().into_any_element();
        }
        let is_sql = view.is_some_and(|v| matches!(v.source, GridSource::Sql(_)));
        if !is_sql {
            return div().into_any_element();
        }
        let active = view.map(|v| v.active_result_index).unwrap_or(0);
        let statement_index = view
            .and_then(|v| v.result_snapshots.get(active))
            .map(|s| s.statement_index)
            .unwrap_or(1);
        let failed = view
            .and_then(|v| v.result_snapshots.get(active))
            .map(|s| s.failed)
            .unwrap_or(false);
        let label = if failed {
            format!(
                "Result {} of {} · Statement {statement_index} failed",
                active + 1,
                count
            )
        } else {
            format!(
                "Result {} of {count} · statement {statement_index}",
                active + 1
            )
        };
        let can_prev = active > 0;
        let can_next = active + 1 < count;
        let prev_ix = active.saturating_sub(1);
        let next_ix = active + 1;
        let focus = self.result_strip_focus.clone();

        h_flex()
            .id("result-strip")
            .key_context("ResultStrip")
            .track_focus(&focus)
            .tab_index(0)
            .tab_stop(true)
            .h(px(22.))
            .px_3()
            .items_center()
            .justify_between()
            .border_t_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().status_bar)
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(label)
            .child(
                h_flex()
                    .gap_1()
                    .child({
                        let this = this.clone();
                        Button::new("result-prev")
                            .xsmall()
                            .label("Prev")
                            .disabled(!can_prev)
                            .tab_index(0)
                            .tooltip("Previous result (Left when the strip is focused)")
                            .on_click(move |_, _, cx| {
                                this.update(cx, |workspace, cx| {
                                    if let Some(id) = workspace.tabs.active().map(|t| t.id) {
                                        workspace.show_result_snapshot(id, prev_ix, cx);
                                    }
                                });
                            })
                    })
                    .child({
                        let this = this;
                        Button::new("result-next")
                            .xsmall()
                            .label("Next")
                            .disabled(!can_next)
                            .tab_index(0)
                            .tooltip("Next result (Right when the strip is focused)")
                            .on_click(move |_, _, cx| {
                                this.update(cx, |workspace, cx| {
                                    if let Some(id) = workspace.tabs.active().map(|t| t.id) {
                                        workspace.show_result_snapshot(id, next_ix, cx);
                                    }
                                });
                            })
                    }),
            )
            .into_any_element()
    }
}
