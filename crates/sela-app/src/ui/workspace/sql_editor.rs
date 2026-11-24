//! SQL editor column: header, find bar, splitter, and Editor body.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor, Enter, Escape, Input};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Disableable as _, Sizable as _};
use gpui_kit::prelude::*;
use gpui_kit::{div, px, Context, DragMoveEvent, IntoElement, Render, Window};

use super::tab_view::{DEFAULT_EDITOR_HEIGHT, MIN_EDITOR_HEIGHT};
use super::Workspace;

struct EditorDrag;

impl Render for EditorDrag {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().h(px(5.)).w_full()
    }
}

impl Workspace {
    pub(super) fn toggle_editor_collapsed(&mut self, cx: &mut Context<Self>) {
        let Some(tab_id) = self.tabs.active().map(|t| t.id) else {
            return;
        };
        if let Some(view) = self.view_mut(tab_id) {
            view.editor_collapsed = !view.editor_collapsed;
        }
        cx.notify();
    }

    pub(super) fn reset_editor_height(&mut self, cx: &mut Context<Self>) {
        let Some(tab_id) = self.tabs.active().map(|t| t.id) else {
            return;
        };
        if let Some(view) = self.view_mut(tab_id) {
            view.editor_height = DEFAULT_EDITOR_HEIGHT;
            view.editor_collapsed = false;
        }
        cx.notify();
    }

    fn editor_max_height(&self, window: &Window) -> f32 {
        let bounds = window.bounds().size;
        let chrome = 44. + 28. + 28. + 44.;
        let body = f32::from(bounds.height) - chrome;
        (body * 0.7).max(MIN_EDITOR_HEIGHT)
    }

    pub(super) fn sql_editor(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        self.sync_active_sql_decorations(cx);
        let this = cx.entity();
        let connected = self.session.is_some();
        let view = self.active_view();
        let busy = view.map(|v| v.busy).unwrap_or(false);
        let sql = view.map(|v| v.sql.clone());
        let find = view.map(|v| v.find_input.clone());
        let find_open = view.map(|v| v.find_open).unwrap_or(false);
        let collapsed = view.map(|v| v.editor_collapsed).unwrap_or(false);
        let column_h = view
            .map(|v| v.editor_body_height())
            .unwrap_or(DEFAULT_EDITOR_HEIGHT);
        let max_h = self.editor_max_height(window);

        v_flex()
            .key_context("SqlEditor")
            .h(px(column_h))
            .border_t_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .id("sql-editor-split")
                    .h(px(5.))
                    .w_full()
                    .bg(cx.theme().border)
                    .cursor_row_resize()
                    .on_click({
                        let this = this.clone();
                        move |event, _, cx| {
                            if event.click_count() == 2 {
                                this.update(cx, |workspace, cx| workspace.reset_editor_height(cx));
                            }
                        }
                    })
                    .on_drag(EditorDrag, |_, _offset, _, cx| cx.new(|_| EditorDrag))
                    .on_drag_move({
                        let this = this.clone();
                        move |event: &DragMoveEvent<EditorDrag>, window, cx| {
                            let mouse_y = f32::from(event.event.position.y);
                            let window_h = f32::from(window.bounds().size.height);
                            // Status bar sits below the editor column.
                            let status_h = 28.;
                            this.update(cx, |workspace, cx| {
                                let Some(tab_id) = workspace.tabs.active().map(|t| t.id) else {
                                    return;
                                };
                                if let Some(view) = workspace.view_mut(tab_id) {
                                    view.editor_collapsed = false;
                                    let next = (window_h - mouse_y - status_h)
                                        .clamp(MIN_EDITOR_HEIGHT, max_h);
                                    view.editor_height = next;
                                }
                                cx.notify();
                            });
                        }
                    }),
            )
            .child(
                h_flex()
                    .px_3()
                    .h(px(28.))
                    .items_center()
                    .justify_between()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child({
                                let this = this.clone();
                                Button::new("editor-collapse")
                                    .xsmall()
                                    .ghost()
                                    .label(if collapsed { "▸" } else { "▾" })
                                    .on_click(move |_, _, cx| {
                                        this.update(cx, |workspace, cx| {
                                            workspace.toggle_editor_collapsed(cx)
                                        });
                                    })
                            })
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("SQL"),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .when(find_open, |row| {
                                let find = find.clone().unwrap();
                                let this = this.clone();
                                row.child(
                                    h_flex()
                                        .w(px(240.))
                                        .gap_1()
                                        .items_center()
                                        .on_action({
                                            let this = this.clone();
                                            move |_: &Enter, _, cx| {
                                                this.update(cx, |workspace, cx| {
                                                    workspace.refresh_find_matches(cx);
                                                    workspace.find_step(1, cx);
                                                });
                                            }
                                        })
                                        .on_action({
                                            let this = this.clone();
                                            move |_: &Escape, _, cx| {
                                                this.update(cx, |workspace, cx| {
                                                    workspace.close_sql_find(cx);
                                                });
                                            }
                                        })
                                        .child(Input::new(&find)),
                                )
                            })
                            .child({
                                let this = this.clone();
                                Button::new("run-query")
                                    .small()
                                    .primary()
                                    .label(if busy { "Running…" } else { "Run" })
                                    .disabled(busy || !connected || sql.is_none())
                                    .on_click(move |_, _, cx| {
                                        this.update(cx, |workspace, cx| workspace.run_editor(cx));
                                    })
                            })
                            .child({
                                let this = this;
                                Button::new("stop-query")
                                    .small()
                                    .label("Stop")
                                    .disabled(!busy || !connected)
                                    .on_click(move |_, _, cx| {
                                        this.update(cx, |workspace, cx| workspace.stop_query(cx));
                                    })
                            }),
                    ),
            )
            .when(!collapsed, |column| {
                column.when_some(sql, |column, sql| {
                    column.child(
                        div().flex_1().min_h(px(0.)).px_2().pb_2().child(
                            Editor::new(&sql)
                                .size_full()
                                .appearance(true)
                                .bordered(true),
                        ),
                    )
                })
            })
    }
}
