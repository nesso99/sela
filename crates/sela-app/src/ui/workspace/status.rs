//! Bottom status bar.

use gpui_kit::component::{h_flex, ActiveTheme};
use gpui_kit::prelude::*;
use gpui_kit::{px, Context, IntoElement};

use super::Workspace;

impl Workspace {
    pub(super) fn status_bar(&self, cx: &Context<Self>) -> impl IntoElement {
        h_flex()
            .h(px(28.))
            .px_3()
            .items_center()
            .justify_between()
            .border_t_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().muted)
            .text_xs()
            .child(self.status.clone())
            .child(self.source_label())
    }
}
