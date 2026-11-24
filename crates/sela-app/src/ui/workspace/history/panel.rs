//! History panel layout and row rendering.

use std::path::Path;
use std::rc::Rc;

use chrono::Local;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::Input;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Disableable as _, Sizable as _};
use gpui_kit::prelude::*;
use gpui_kit::{div, px, size, Context, DragMoveEvent, Entity, IntoElement, SharedString, Window};
use sela_core::{
    format_history_duration, format_history_when, history_sql_preview, HistoryEntry, HistoryOutcome,
};

use super::{
    HistoryDrag, HistoryScope, DEFAULT_HISTORY_HEIGHT, HISTORY_ROW_HEIGHT, MIN_HISTORY_HEIGHT,
};
use crate::ui::workspace::Workspace;

impl Workspace {
    pub(in super::super) fn history_panel(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        if !self.history_open {
            return div().id("history-panel-closed").into_any_element();
        }

        let this = cx.entity();
        let max_h = self.history_max_height(window);
        let height = self.history_height.clamp(MIN_HISTORY_HEIGHT, max_h);
        let scope_this_enabled = self.history_scope_connection_id().is_some();
        let scope = self.history_scope;
        let outcome = self.history_outcome;
        let rows = self.history_rows.clone();
        let selected = self.history_selected;
        let search = self.history_search.clone();
        let empty_text = self.history_empty_message();
        let error_detail = selected
            .and_then(|ix| rows.get(ix))
            .filter(|e| e.outcome == HistoryOutcome::Error)
            .map(|e| e.error_message.clone())
            .filter(|m| !m.is_empty());
        let show_conn_cols = matches!(scope, HistoryScope::All);
        let sizes = Rc::new(vec![size(px(400.), px(HISTORY_ROW_HEIGHT)); rows.len()]);

        v_flex()
            .id("history-panel")
            .key_context("HistoryPanel")
            .h(px(height))
            .border_t_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .child(
                div()
                    .id("history-split")
                    .h(px(5.))
                    .w_full()
                    .cursor_row_resize()
                    .child(div().h(px(1.)).w_full().bg(cx.theme().border))
                    .on_click({
                        let this = this.clone();
                        move |event, _, cx| {
                            if event.click_count() == 2 {
                                this.update(cx, |ws, cx| {
                                    ws.history_height = DEFAULT_HISTORY_HEIGHT;
                                    cx.notify();
                                });
                            }
                        }
                    })
                    .on_drag(HistoryDrag, |_, _offset, _, cx| cx.new(|_| HistoryDrag))
                    .on_drag_move({
                        let this = this.clone();
                        move |event: &DragMoveEvent<HistoryDrag>, window, cx| {
                            let mouse_y = f32::from(event.event.position.y);
                            let window_h = f32::from(window.bounds().size.height);
                            let status_h = 28.;
                            this.update(cx, |ws, cx| {
                                let max_h = ws.history_max_height(window);
                                ws.history_height = (window_h - mouse_y - status_h)
                                    .clamp(MIN_HISTORY_HEIGHT, max_h);
                                cx.notify();
                            });
                        }
                    }),
            )
            .child(
                h_flex()
                    .h(px(28.))
                    .px_2()
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().muted)
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child("History"),
                    )
                    .child({
                        let this = this.clone();
                        Button::new("history-scope-this")
                            .xsmall()
                            .label("This connection")
                            .disabled(!scope_this_enabled)
                            .when(matches!(scope, HistoryScope::ThisConnection), |b| {
                                b.primary()
                            })
                            .when(!matches!(scope, HistoryScope::ThisConnection), |b| {
                                b.ghost()
                            })
                            .on_click(move |_, _, cx| {
                                this.update(cx, |ws, cx| {
                                    ws.history_scope = HistoryScope::ThisConnection;
                                    ws.refresh_history(cx);
                                });
                            })
                    })
                    .child({
                        let this = this.clone();
                        Button::new("history-scope-all")
                            .xsmall()
                            .label("All")
                            .when(matches!(scope, HistoryScope::All), |b| b.primary())
                            .when(!matches!(scope, HistoryScope::All), |b| b.ghost())
                            .on_click(move |_, _, cx| {
                                this.update(cx, |ws, cx| {
                                    ws.history_scope = HistoryScope::All;
                                    ws.refresh_history(cx);
                                });
                            })
                    })
                    .child(
                        Button::new("history-outcome")
                            .xsmall()
                            .ghost()
                            .label(outcome_label(outcome))
                            .dropdown_menu({
                                let this = this.clone();
                                move |menu, _, _| {
                                    menu.item(outcome_item(&this, None, "All"))
                                        .item(outcome_item(
                                            &this,
                                            Some(HistoryOutcome::Success),
                                            "Success",
                                        ))
                                        .item(outcome_item(
                                            &this,
                                            Some(HistoryOutcome::Error),
                                            "Error",
                                        ))
                                        .item(outcome_item(
                                            &this,
                                            Some(HistoryOutcome::Cancelled),
                                            "Cancelled",
                                        ))
                                }
                            }),
                    )
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w(px(120.))
                            .max_w(px(280.))
                            .child(Input::new(&search).appearance(true)),
                    )
                    .child({
                        let this = this.clone();
                        Button::new("history-clear")
                            .xsmall()
                            .ghost()
                            .label("Clear history")
                            .on_click(move |_, window, cx| {
                                this.update(cx, |ws, cx| ws.confirm_clear_history(window, cx));
                            })
                    }),
            )
            .when_some(error_detail, |col, msg| {
                col.child(
                    div()
                        .h(px(22.))
                        .px_3()
                        .text_xs()
                        .truncate()
                        .text_color(cx.theme().danger)
                        .child(msg),
                )
            })
            .when_some(self.history_panel_error.clone(), |col, msg| {
                col.child(
                    div()
                        .flex_1()
                        .px_3()
                        .py_2()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("Could not open query history: {msg}")),
                )
            })
            .when(
                self.history_panel_error.is_none() && rows.is_empty(),
                |col| {
                    col.child(
                        h_flex()
                            .flex_1()
                            .px_3()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(empty_text.clone()),
                            )
                            .when(self.history_show_turn_on(), |row| {
                                let this = this.clone();
                                row.child(
                                    Button::new("history-turn-on")
                                        .xsmall()
                                        .label("Turn on")
                                        .on_click(move |_, _, cx| {
                                            this.update(cx, |ws, cx| ws.turn_on_history(cx));
                                        }),
                                )
                            }),
                    )
                },
            )
            .when(
                self.history_panel_error.is_none() && !rows.is_empty(),
                |col| {
                    let this_list = this.clone();
                    col.child(div().flex_1().min_h(px(0.)).overflow_hidden().child(
                        gpui_kit::component::v_virtual_list(
                            this_list.clone(),
                            "history-rows",
                            sizes,
                            move |ws, range, _window, cx| {
                                range
                                    .map(|ix| {
                                        ws.render_history_row(
                                            ix,
                                            show_conn_cols,
                                            selected == Some(ix),
                                            cx,
                                        )
                                        .into_any_element()
                                    })
                                    .collect()
                            },
                        ),
                    ))
                },
            )
            .into_any_element()
    }

    fn render_history_row(
        &self,
        ix: usize,
        show_conn_cols: bool,
        selected: bool,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let this = cx.entity();
        let Some(entry) = self.history_rows.get(ix).cloned() else {
            return div().h(px(HISTORY_ROW_HEIGHT)).into_any_element();
        };
        let id = entry.id.clone();
        let display_name = self.history_display_name(&entry);
        let when = format_history_when(entry.started_at, Local::now());
        let duration = format_history_duration(entry.duration_ms);
        let rows_label = if entry.outcome == HistoryOutcome::Error {
            "—".into()
        } else {
            entry.rows_label.clone()
        };
        let preview = history_sql_preview(&entry.sql);
        let mark = outcome_mark(entry.outcome);
        let mark_color = match entry.outcome {
            HistoryOutcome::Success => cx.theme().success,
            HistoryOutcome::Error => cx.theme().danger,
            HistoryOutcome::Cancelled => cx.theme().muted_foreground,
        };
        let conn_alive = self.connections.iter().any(|c| c.id == entry.connection_id);

        h_flex()
            .id(("history-row", ix))
            .h(px(HISTORY_ROW_HEIGHT))
            .px_2()
            .gap_2()
            .items_center()
            .w_full()
            .when(selected, |row| row.bg(cx.theme().accent.opacity(0.25)))
            .on_mouse_down(gpui_kit::MouseButton::Left, {
                let this = this.clone();
                let id = id.clone();
                move |event, window, cx| {
                    this.update(cx, |ws, cx| {
                        ws.select_history_id(&id);
                        if event.click_count == 2 {
                            ws.history_open_in_tab(window, cx);
                        }
                        cx.notify();
                    });
                }
            })
            .child(
                div()
                    .w(px(8.))
                    .h(px(8.))
                    .rounded_full()
                    .when(entry.outcome != HistoryOutcome::Cancelled, |d| {
                        d.bg(mark_color)
                    })
                    .when(entry.outcome == HistoryOutcome::Cancelled, |d| {
                        d.border_1().border_color(mark_color)
                    })
                    .child(mark),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .font_family("monospace")
                    .text_xs()
                    .truncate()
                    .child(preview),
            )
            .when(show_conn_cols, |row| {
                row.child(
                    div()
                        .w(px(100.))
                        .text_xs()
                        .truncate()
                        .text_color(cx.theme().muted_foreground)
                        .child(display_name),
                )
                .child(
                    div()
                        .w(px(90.))
                        .text_xs()
                        .truncate()
                        .text_color(cx.theme().muted_foreground)
                        .child(history_database_label(&entry)),
                )
            })
            .child(
                div()
                    .w(px(72.))
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(when),
            )
            .child(
                div()
                    .w(px(64.))
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(duration),
            )
            .child(
                div()
                    .w(px(88.))
                    .text_xs()
                    .truncate()
                    .text_color(cx.theme().muted_foreground)
                    .child(rows_label),
            )
            .child(
                Button::new(("history-row-menu", ix))
                    .xsmall()
                    .ghost()
                    .label("…")
                    .tab_stop(false)
                    .dropdown_menu({
                        let this = this.clone();
                        let id = id.clone();
                        move |menu, _, _| {
                            let open = {
                                let this = this.clone();
                                let id = id.clone();
                                PopupMenuItem::new("Open in new tab")
                                    .disabled(!conn_alive)
                                    .on_click(move |_, window, cx| {
                                        this.update(cx, |ws, cx| {
                                            ws.select_history_id(&id);
                                            ws.history_open_in_tab(window, cx);
                                        });
                                    })
                            };
                            let insert = {
                                let this = this.clone();
                                let id = id.clone();
                                PopupMenuItem::new("Insert into editor").on_click(
                                    move |_, window, cx| {
                                        this.update(cx, |ws, cx| {
                                            ws.select_history_id(&id);
                                            ws.history_insert_into_editor(window, cx);
                                        });
                                    },
                                )
                            };
                            let copy = {
                                let this = this.clone();
                                let id = id.clone();
                                PopupMenuItem::new("Copy").on_click(move |_, _, cx| {
                                    this.update(cx, |ws, cx| {
                                        ws.select_history_id(&id);
                                        ws.history_copy(cx);
                                    });
                                })
                            };
                            let rerun = {
                                let this = this.clone();
                                let id = id.clone();
                                PopupMenuItem::new("Rerun").disabled(!conn_alive).on_click(
                                    move |_, window, cx| {
                                        this.update(cx, |ws, cx| {
                                            ws.select_history_id(&id);
                                            ws.history_rerun(window, cx);
                                        });
                                    },
                                )
                            };
                            let delete = {
                                let this = this.clone();
                                let id = id.clone();
                                PopupMenuItem::new("Delete").on_click(move |_, _, cx| {
                                    this.update(cx, |ws, cx| {
                                        ws.select_history_id(&id);
                                        ws.history_delete_selected(cx);
                                    });
                                })
                            };
                            menu.item(open)
                                .item(insert)
                                .item(copy)
                                .item(rerun)
                                .item(delete)
                        }
                    }),
            )
            .into_any_element()
    }
}

fn outcome_label(outcome: Option<HistoryOutcome>) -> &'static str {
    match outcome {
        None => "All",
        Some(HistoryOutcome::Success) => "Success",
        Some(HistoryOutcome::Error) => "Error",
        Some(HistoryOutcome::Cancelled) => "Cancelled",
    }
}

fn outcome_mark(outcome: HistoryOutcome) -> SharedString {
    match outcome {
        HistoryOutcome::Success => "●".into(),
        HistoryOutcome::Error => "✕".into(),
        HistoryOutcome::Cancelled => "–".into(),
    }
}

fn outcome_item(
    workspace: &Entity<Workspace>,
    outcome: Option<HistoryOutcome>,
    label: &str,
) -> PopupMenuItem {
    let this = workspace.clone();
    PopupMenuItem::new(label.to_string()).on_click(move |_, _, cx| {
        this.update(cx, |ws, cx| {
            ws.history_outcome = outcome;
            ws.refresh_history(cx);
        });
    })
}

fn history_database_label(entry: &HistoryEntry) -> String {
    let path = Path::new(&entry.database);
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(entry.database.as_str())
        .to_string()
}
