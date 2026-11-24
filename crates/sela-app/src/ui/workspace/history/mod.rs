//! Query history panel (M1-5): list, search, scope, and actions.

mod actions;
mod panel;

use gpui_kit::prelude::*;
use gpui_kit::{div, px, Context, IntoElement, Render, Window};

pub(super) const DEFAULT_HISTORY_HEIGHT: f32 = 196.;
pub(super) const MIN_HISTORY_HEIGHT: f32 = 96.;
pub(crate) const HISTORY_ROW_HEIGHT: f32 = 26.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HistoryScope {
    ThisConnection,
    All,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HistoryFocusRestore {
    Editor,
    Grid,
    None,
}

struct HistoryDrag;

impl Render for HistoryDrag {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().h(px(5.)).w_full()
    }
}
