//! Pending bar, Preview SQL sheet, Discard confirm, and Commit actions (M2-3).

use std::time::Instant;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Disableable as _, Sizable as _};
use gpui_kit::prelude::*;
use gpui_kit::{div, px, Context, IntoElement, Window};

use sela_core::{
    decide_grid_commit_gate, truncate_one_line, ChangePlan, DbError, GridCommitGate, TabId,
};
use sela_drivers::drive;

use super::edit_session::format_commit_failure;
use super::Workspace;

impl Workspace {
    /// 26px pending-changes bar (08-preview-commit.md).
    pub(super) fn pending_bar(&self, cx: &Context<Self>) -> impl IntoElement {
        let this = cx.entity();
        let view = self.active_view();
        let Some(session) = view.and_then(|v| v.edit.as_ref()) else {
            return div().into_any_element();
        };
        if !session.is_dirty() {
            return div().into_any_element();
        }
        let safe_mode = self.active_safe_mode();
        let counts = session.pending_counts();
        let left = if session.commit_failed {
            format!("{} · Commit failed. Changes kept.", counts.summary_left())
        } else {
            counts.summary_left()
        };
        let busy = view.map(|v| v.busy).unwrap_or(false) || session.committing;
        let commit_block = session.commit_blocked_reason_for(safe_mode);
        let commit_disabled = busy || commit_block.is_some();
        let commit_tooltip = commit_block.unwrap_or("Commit pending changes (Ctrl/⌘+S)");
        let commit_label = if session.committing {
            "Committing…"
        } else {
            "Commit"
        };
        let palette = super::super::theme::SelaPalette::for_mode(cx.theme().mode);

        h_flex()
            .h(px(26.))
            .px_2()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().tokens.tab_bar)
            .child(
                div()
                    .text_xs()
                    .text_color(if session.commit_failed {
                        palette.status_error
                    } else {
                        cx.theme().foreground
                    })
                    .child(left),
            )
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(
                        Button::new("pending-preview")
                            .xsmall()
                            .ghost()
                            .label("Preview")
                            .tooltip("Preview SQL (Ctrl/⌘+Shift+P)")
                            .on_click({
                                let this = this.clone();
                                move |_, _, cx| {
                                    this.update(cx, |workspace, cx| {
                                        workspace.toggle_preview_sheet(cx);
                                    });
                                }
                            }),
                    )
                    .child(
                        Button::new("pending-discard")
                            .xsmall()
                            .ghost()
                            .label("Discard")
                            .disabled(busy)
                            .tooltip("Discard pending changes (Ctrl/⌘+Shift+Z)")
                            .on_click({
                                let this = this.clone();
                                move |_, window, cx| {
                                    this.update(cx, |workspace, cx| {
                                        workspace.request_discard_changes(window, cx);
                                    });
                                }
                            }),
                    )
                    .child(
                        Button::new("pending-commit")
                            .xsmall()
                            .primary()
                            .label(commit_label)
                            .disabled(commit_disabled)
                            .tooltip(commit_tooltip)
                            .on_click({
                                let this = this.clone();
                                move |_, window, cx| {
                                    this.update(cx, |workspace, cx| {
                                        workspace.request_commit_changes(window, cx);
                                    });
                                }
                            }),
                    ),
            )
            .into_any_element()
    }

    /// Read-only explanation strip (07-grid-editing.md).
    pub(super) fn read_only_strip(&self, cx: &Context<Self>) -> impl IntoElement {
        let mode = self.active_safe_mode();
        let line = self
            .active_view()
            .and_then(|v| v.edit.as_ref())
            .and_then(|s| s.strip_line_for(mode));
        let Some(line) = line else {
            return div().into_any_element();
        };
        let palette = super::super::theme::SelaPalette::for_mode(cx.theme().mode);
        div()
            .h(px(26.))
            .px_2()
            .flex()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().tokens.tab_bar)
            .text_xs()
            .text_color(palette.status_info)
            .child(line)
            .into_any_element()
    }

    /// Preview SQL sheet (240px from the bottom of the content column).
    pub(super) fn preview_sheet(&self, cx: &Context<Self>) -> impl IntoElement {
        let this = cx.entity();
        let view = self.active_view();
        let Some(session) = view.and_then(|v| v.edit.as_ref()) else {
            return div().into_any_element();
        };
        if !session.preview_open {
            return div().into_any_element();
        }
        let title = format!("Preview SQL · {}", session.schema_table_label());
        let body = session
            .preview_sheet_text()
            .unwrap_or_else(|e| format!("-- preview failed: {e}"));
        let safe_mode = self.active_safe_mode();
        let busy = view.map(|v| v.busy).unwrap_or(false) || session.committing;
        let commit_block = session.commit_blocked_reason_for(safe_mode);
        let commit_disabled = busy || commit_block.is_some();
        let commit_label = if session.committing {
            "Committing…"
        } else {
            "Commit"
        };

        v_flex()
            .h(px(240.))
            .border_t_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .child(
                v_flex()
                    .px_3()
                    .pt_2()
                    .pb_1()
                    .gap_1()
                    .child(
                        h_flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                                    .child(title),
                            )
                            .child(
                                Button::new("preview-close")
                                    .xsmall()
                                    .ghost()
                                    .label("×")
                                    .tooltip("Close preview (Escape)")
                                    .on_click({
                                        let this = this.clone();
                                        move |_, _, cx| {
                                            this.update(cx, |workspace, cx| {
                                                workspace.close_preview_sheet(cx);
                                            });
                                        }
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                "The database receives bound parameters. Values are listed beside each statement.",
                            ),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .px_3()
                    .overflow_y_scrollbar()
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_xs()
                    .child(body),
            )
            .child(
                h_flex()
                    .h(px(32.))
                    .px_3()
                    .items_center()
                    .justify_end()
                    .gap_2()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(
                        Button::new("preview-discard")
                            .xsmall()
                            .ghost()
                            .label("Discard")
                            .disabled(busy)
                            .on_click({
                                let this = this.clone();
                                move |_, window, cx| {
                                    this.update(cx, |workspace, cx| {
                                        workspace.request_discard_changes(window, cx);
                                    });
                                }
                            }),
                    )
                    .child(
                        Button::new("preview-commit")
                            .xsmall()
                            .primary()
                            .label(commit_label)
                            .disabled(commit_disabled)
                            .on_click({
                                let this = this.clone();
                                move |_, window, cx| {
                                    this.update(cx, |workspace, cx| {
                                        workspace.request_commit_changes(window, cx);
                                    });
                                }
                            }),
                    ),
            )
            .into_any_element()
    }

    pub(super) fn toggle_preview_sheet(&mut self, cx: &mut Context<Self>) {
        let Some(view) = self.active_view_mut() else {
            return;
        };
        let Some(edit) = view.edit.as_mut() else {
            return;
        };
        if !edit.is_dirty() {
            return;
        }
        edit.preview_open = !edit.preview_open;
        cx.notify();
    }

    pub(super) fn close_preview_sheet(&mut self, cx: &mut Context<Self>) {
        if let Some(edit) = self.active_view_mut().and_then(|v| v.edit.as_mut()) {
            edit.preview_open = false;
            cx.notify();
        }
    }

    pub(super) fn request_discard_changes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.discard_confirm_open {
            return;
        }
        let Some(view) = self.active_view() else {
            return;
        };
        let Some(edit) = view.edit.as_ref() else {
            return;
        };
        if !edit.is_dirty() || view.busy || edit.committing {
            return;
        }
        // Editor open: secondary-shift-z is left to the field (redo on macOS).
        if edit.editor.is_some() {
            return;
        }
        let n = edit.pending_counts().total();
        let table = edit.schema_table_label();
        self.discard_confirm_open = true;
        let this = cx.entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let this_ok = this.clone();
            let this_cancel = this.clone();
            let this_discard = this.clone();
            alert
                .title("Discard pending changes?")
                .description(format!(
                    "{n} edits on {table} will be dropped. The database is not changed."
                ))
                .on_ok(move |_, _, cx| {
                    this_ok.update(cx, |workspace, _| {
                        workspace.discard_confirm_open = false;
                    });
                    true
                })
                .on_cancel(move |_, _, cx| {
                    this_cancel.update(cx, |workspace, _| {
                        workspace.discard_confirm_open = false;
                    });
                    true
                })
                .footer(
                    DialogFooter::new()
                        .child(
                            Button::new("discard-dialog-cancel")
                                .label("Cancel")
                                .primary()
                                .on_click({
                                    let this = this.clone();
                                    move |_, window, cx| {
                                        this.update(cx, |workspace, _| {
                                            workspace.discard_confirm_open = false;
                                        });
                                        window.close_dialog(cx);
                                    }
                                }),
                        )
                        .child(
                            Button::new("discard-dialog-discard")
                                .label("Discard")
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                    this_discard.update(cx, |workspace, cx| {
                                        workspace.discard_confirm_open = false;
                                        workspace.perform_discard_changes(cx);
                                    });
                                }),
                        ),
                )
        });
    }

    pub(super) fn perform_discard_changes(&mut self, cx: &mut Context<Self>) {
        let Some(tab_id) = self.tabs.active().map(|t| t.id) else {
            return;
        };
        if let Some(view) = self.view_mut(tab_id) {
            if let Some(edit) = view.edit.as_mut() {
                edit.discard();
            }
            self.sync_tab_dirty(tab_id);
            self.sync_grid_edit_paint(tab_id, cx);
            cx.notify();
        }
    }

    pub(super) fn request_commit_changes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_active_edit_safe_mode();
        // Accept open cell editor first (08-preview-commit.md).
        if self
            .active_view()
            .and_then(|v| v.edit.as_ref())
            .is_some_and(|e| e.editor.is_some())
        {
            if let Err(msg) = self.accept_cell_editor(cx) {
                if let Some(view) = self.active_view_mut() {
                    view.status = msg;
                }
                cx.notify();
                return;
            }
        }
        let mode = self.active_safe_mode();
        let Some(view) = self.active_view() else {
            return;
        };
        let Some(edit) = view.edit.as_ref() else {
            return;
        };
        if edit.commit_blocked_reason_for(mode).is_some() || view.busy {
            return;
        }
        let Ok(plan) = edit.build_plan() else {
            return;
        };
        if plan.is_empty() {
            return;
        }
        match decide_grid_commit_gate(mode) {
            GridCommitGate::Allow => self.perform_commit_changes(plan, cx),
            GridCommitGate::RefuseReadOnly => {
                // Commit button should already be disabled; keep edits.
                if let Some(view) = self.active_view_mut() {
                    view.status = "Safe mode is Read-only.".into();
                }
                cx.notify();
            }
            GridCommitGate::Confirm => {
                self.show_grid_commit_confirm(window, cx, plan);
            }
        }
    }

    fn show_grid_commit_confirm(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        plan: ChangePlan,
    ) {
        if self.safe_mode_confirm_open {
            return;
        }
        let n = plan.statements.len();
        let table = self
            .active_view()
            .and_then(|v| v.edit.as_ref())
            .map(|e| e.schema_table_label())
            .unwrap_or_else(|| "table".into());
        let first = plan
            .statements
            .first()
            .map(|s| truncate_one_line(&s.sql, 120).into_owned())
            .unwrap_or_default();
        let title = format!("Commit {n} changes?");
        let description = format!("{n} statements on {table}.\n\n{first}");
        self.safe_mode_confirm_open = true;
        let this = cx.entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let this_ok = this.clone();
            let this_cancel = this.clone();
            let this_commit = this.clone();
            let plan_commit = plan.clone();
            alert
                .title(title.clone())
                .description(description.clone())
                .on_ok(move |_, _, cx| {
                    this_ok.update(cx, |workspace, _| {
                        workspace.safe_mode_confirm_open = false;
                    });
                    true
                })
                .on_cancel(move |_, _, cx| {
                    this_cancel.update(cx, |workspace, _| {
                        workspace.safe_mode_confirm_open = false;
                    });
                    true
                })
                .footer(
                    DialogFooter::new()
                        .child(
                            Button::new("grid-commit-cancel")
                                .label("Cancel")
                                .primary()
                                .on_click({
                                    let this = this.clone();
                                    move |_, window, cx| {
                                        this.update(cx, |workspace, _| {
                                            workspace.safe_mode_confirm_open = false;
                                        });
                                        window.close_dialog(cx);
                                    }
                                }),
                        )
                        .child(Button::new("grid-commit-ok").label("Commit").on_click(
                            move |_, window, cx| {
                                window.close_dialog(cx);
                                this_commit.update(cx, |workspace, cx| {
                                    workspace.safe_mode_confirm_open = false;
                                    workspace.perform_commit_changes(plan_commit.clone(), cx);
                                });
                            },
                        )),
                )
        });
    }

    fn perform_commit_changes(&mut self, plan: ChangePlan, cx: &mut Context<Self>) {
        let Some(tab_id) = self.tabs.active().map(|t| t.id) else {
            return;
        };
        let Some(session) = self.session.clone() else {
            return;
        };
        let statement_count = plan.statements.len();
        let Some(generation) = self
            .view_mut(tab_id)
            .and_then(|v| v.edit.as_mut())
            .map(|edit| edit.begin_commit())
        else {
            return;
        };
        cx.notify();
        let started = Instant::now();
        // Commit runs outside the tab request gate so sort/filter/paging cannot
        // bump request and drop the result (leaving committing=true forever).
        cx.spawn(async move |this, async_cx| {
            let result = drive(async move { session.apply_changes(&plan).await }).await;
            this.update(async_cx, |this, cx| {
                let Some(view) = this.views.get(&tab_id) else {
                    return;
                };
                let Some(edit) = view.edit.as_ref() else {
                    return;
                };
                if edit.commit_generation != generation {
                    return;
                }
                match result {
                    Ok(applied) => {
                        let ms = started.elapsed().as_millis();
                        let statements = applied.statements_executed;
                        if let Some(view) = this.view_mut(tab_id) {
                            if let Some(edit) = view.edit.as_mut() {
                                if !edit.note_commit_success(generation) {
                                    return;
                                }
                            }
                            view.error = None;
                            view.status = format!("Committed · {statements} statements · {ms} ms");
                            view.post_commit_reload = true;
                        }
                        this.sync_tab_dirty(tab_id);
                        this.sync_grid_edit_paint(tab_id, cx);
                        this.reload_table_page_after_commit(tab_id, cx);
                    }
                    Err(err) => {
                        let (banner, status) = format_commit_failure(&err, statement_count);
                        if let Some(view) = this.view_mut(tab_id) {
                            if let Some(edit) = view.edit.as_mut() {
                                if !edit.note_commit_failure(generation) {
                                    return;
                                }
                            }
                            view.error = Some(banner);
                            view.last_error_sqlstate = match &err {
                                DbError::ChangeConflict { sqlstate, .. } => sqlstate.clone(),
                                _ => err.sqlstate().map(|s| s.to_string()),
                            };
                            view.status = status;
                        }
                        this.sync_tab_dirty(tab_id);
                        this.sync_grid_edit_paint(tab_id, cx);
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    fn reload_table_page_after_commit(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        // Reuse the table page loader. On reload failure after success, keep ChangeSet cleared.
        let Some(view) = self.views.get(&tab_id) else {
            return;
        };
        if !matches!(view.source, super::GridSource::Table { .. }) {
            cx.notify();
            return;
        }
        self.load_source_for(tab_id, cx);
    }

    pub(super) fn sync_tab_dirty(&mut self, tab_id: TabId) {
        let dirty = self
            .views
            .get(&tab_id)
            .and_then(|v| v.edit.as_ref())
            .is_some_and(|e| e.is_dirty());
        if let Some(tab) = self.tabs.get_mut(tab_id) {
            tab.dirty = dirty;
        }
    }
}
