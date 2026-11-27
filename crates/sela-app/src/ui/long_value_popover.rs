//! Popover editor for long cell values: multi-line text and JSON (#72, v0.6.0).
//!
//! Enter inserts a newline. Ctrl/Cmd+Enter accepts, Escape cancels. The
//! buttons dispatch the same actions as the keys, so keyboard and mouse share
//! one path through `Workspace`.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState, Textarea, TextareaState};
use gpui_kit::component::{h_flex, ActiveTheme, Sizable as _};
use gpui_kit::prelude::*;
use gpui_kit::{div, px, Action, App, Entity, IntoElement, Window};

use super::theme::SelaPalette;
use super::{AcceptCellEditor, CancelCellEditor, SetCellNull};

pub(crate) const LONG_VALUE_KEY_CONTEXT: &str = "CellEditorPopover";

/// Popover width and approximate height (title, editor, buttons, padding).
pub(crate) const LONG_VALUE_POPOVER_WIDTH: f32 = 420.;
pub(crate) const LONG_VALUE_POPOVER_HEIGHT: f32 = 270.;

const POPOVER_INSET: f32 = 8.;

/// Keep the popover box inside the results-area viewport.
pub(crate) fn clamp_popover_anchor(
    anchor_left: f32,
    anchor_top: f32,
    area_width: f32,
    area_height: f32,
) -> (f32, f32) {
    let max_left = (area_width - LONG_VALUE_POPOVER_WIDTH - POPOVER_INSET).max(POPOVER_INSET);
    let max_top = (area_height - LONG_VALUE_POPOVER_HEIGHT - POPOVER_INSET).max(POPOVER_INSET);
    (
        anchor_left.clamp(POPOVER_INSET, max_left),
        anchor_top.clamp(POPOVER_INSET, max_top),
    )
}

/// Which widget owns the draft text.
pub(crate) enum LongValuePopoverInput<'a> {
    Text(&'a Entity<TextareaState>),
    Json(&'a Entity<EditorState>),
}

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
    input: LongValuePopoverInput<'_>,
    info: LongValuePopover,
    cx: &App,
) -> impl IntoElement {
    let palette = SelaPalette::for_mode(cx.theme().mode);
    let mono = cx.theme().mono_font_family.clone();
    let field = match input {
        LongValuePopoverInput::Text(state) => div()
            .h(px(180.))
            .font_family(mono.clone())
            .child(
                Textarea::new(state)
                    .size_full()
                    .appearance(true)
                    .bordered(true),
            )
            .into_any_element(),
        LongValuePopoverInput::Json(state) => div()
            .h(px(180.))
            .font_family(mono)
            .child(
                Editor::new(state)
                    .size_full()
                    .appearance(true)
                    .bordered(true),
            )
            .into_any_element(),
    };
    div()
        .id("long-value-popover")
        .absolute()
        .top(px(info.anchor_top))
        .left(px(info.anchor_left))
        .w(px(LONG_VALUE_POPOVER_WIDTH))
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
        .child(field)
        .when_some(info.error, |col, err| {
            col.child(div().text_xs().text_color(palette.status_error).child(err))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_keeps_popover_inside_viewport() {
        let (left, top) = clamp_popover_anchor(500., 400., 600., 500.);
        assert!((left - (600. - LONG_VALUE_POPOVER_WIDTH - POPOVER_INSET)).abs() < f32::EPSILON);
        assert!((top - (500. - LONG_VALUE_POPOVER_HEIGHT - POPOVER_INSET)).abs() < f32::EPSILON);
    }

    #[test]
    fn clamp_preserves_cell_anchor_when_room() {
        let (left, top) = clamp_popover_anchor(40., 50., 900., 600.);
        assert!((left - 40.).abs() < f32::EPSILON);
        assert!((top - 50.).abs() < f32::EPSILON);
    }
}
