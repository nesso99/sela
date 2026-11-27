//! Popover editor for long cell values: multi-line text and JSON (#72, v0.6.0).
//!
//! Enter inserts a newline. Ctrl/Cmd+Enter accepts, Escape cancels. The
//! buttons dispatch the same actions as the keys, so keyboard and mouse share
//! one path through `Workspace`.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState};
use gpui_kit::component::{h_flex, ActiveTheme, Sizable as _};
use gpui_kit::prelude::*;
use gpui_kit::{div, px, Action, App, Entity, IntoElement, Window};

use super::{AcceptCellEditor, CancelCellEditor, SetCellNull};

pub(crate) const LONG_VALUE_KEY_CONTEXT: &str = "CellEditorPopover";

/// What the popover needs to draw; the text itself lives in the editor entity.
pub(crate) struct LongValuePopover {
    pub title: String,
    pub error: Option<String>,
    pub anchor_left: f32,
    pub anchor_top: f32,
}

fn dispatch(
    action: fn() -> Box<dyn Action>,
) -> impl Fn(&gpui_kit::ClickEvent, &mut Window, &mut App) + 'static {
    move |_, window, cx| window.dispatch_action(action(), cx)
}

pub(crate) fn long_value_popover(
    input: &Entity<EditorState>,
    info: LongValuePopover,
    cx: &App,
) -> impl IntoElement {
    div()
        .id("long-value-popover")
        .absolute()
        .top(px(info.anchor_top))
        .left(px(info.anchor_left))
        .w(px(420.))
        .flex()
        .flex_col()
        .gap_2()
        .p_2()
        .border_1()
        .border_color(cx.theme().border)
        .rounded(px(6.))
        .bg(cx.theme().background)
        .key_context(LONG_VALUE_KEY_CONTEXT)
        .child(
            div()
                .text_sm()
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .child(info.title),
        )
        .child(
            div().h(px(180.)).child(
                Editor::new(input)
                    .size_full()
                    .appearance(true)
                    .bordered(true),
            ),
        )
        .when_some(info.error, |col, err| {
            col.child(div().text_xs().text_color(cx.theme().danger).child(err))
        })
        .child(
            h_flex()
                .justify_between()
                .child(
                    Button::new("long-value-null")
                        .xsmall()
                        .ghost()
                        .label("NULL")
                        .tooltip("Store NULL")
                        .on_click(dispatch(|| Box::new(SetCellNull))),
                )
                .child(
                    h_flex()
                        .gap_1()
                        .child(
                            Button::new("long-value-cancel")
                                .xsmall()
                                .ghost()
                                .label("Cancel")
                                .tooltip("Cancel (Escape)")
                                .on_click(dispatch(|| Box::new(CancelCellEditor))),
                        )
                        .child(
                            Button::new("long-value-accept")
                                .xsmall()
                                .primary()
                                .label("Accept")
                                .tooltip("Accept (Ctrl/Cmd+Enter)")
                                .on_click(dispatch(|| Box::new(AcceptCellEditor))),
                        ),
                ),
        )
}
