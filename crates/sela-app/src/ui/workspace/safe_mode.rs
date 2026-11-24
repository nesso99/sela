//! Safe-mode UI: toolbar label, editor/history gating, and confirm dialogs.
//!
//! Dialogs reuse the quit pattern: Cancel is primary; Enter and Escape cancel.
//! See `docs/design/ux/09-safe-mode.md`.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::WindowExt as _;
use gpui_kit::prelude::*;
use gpui_kit::{Context, Window};

use sela_core::{
    classify_statement_safety, decide_editor_gate, truncate_one_line, Dialect, EditorGate,
    SafeMode, StatementSafety,
};

use super::Workspace;

/// Refusal copy for the editor banner / status (and history Rerun).
pub(super) const READ_ONLY_BANNER: &str = "Read-only connection. The statement was not run.";
pub(super) const READ_ONLY_STATUS: &str = "Read-only · statement not run";

impl Workspace {
    pub(super) fn active_safe_mode(&self) -> SafeMode {
        self.active_connection()
            .map(|c| c.safe_mode)
            .unwrap_or(SafeMode::Off)
    }

    /// Push the connection's safe-mode level onto the active tab's edit session.
    pub(super) fn sync_active_edit_safe_mode(&mut self) {
        let mode = self.active_safe_mode();
        if let Some(edit) = self.active_view_mut().and_then(|v| v.edit.as_mut()) {
            edit.apply_connection_safe_mode(mode);
        }
    }

    /// Classify each statement and decide whether to run, refuse, or confirm.
    pub(super) fn gate_statements(
        &self,
        statements: &[String],
        dialect: Dialect,
    ) -> (Vec<(String, StatementSafety)>, EditorGate) {
        let classified: Vec<(String, StatementSafety)> = statements
            .iter()
            .map(|sql| {
                let safety = classify_statement_safety(sql, dialect);
                (sql.clone(), safety)
            })
            .collect();
        let gate = decide_editor_gate(self.active_safe_mode(), &classified);
        (classified, gate)
    }

    pub(super) fn refuse_read_only_run(
        &mut self,
        tab_id: sela_core::TabId,
        cx: &mut Context<Self>,
    ) {
        if let Some(view) = self.view_mut(tab_id) {
            view.error = Some(READ_ONLY_BANNER.into());
            view.status = READ_ONLY_STATUS.into();
            view.local_sql_error = None;
        }
        cx.notify();
    }

    /// Show a safe-mode confirm; on Continue, run `statements` on `tab_id`.
    pub(super) fn show_safe_mode_confirm(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        tab_id: sela_core::TabId,
        statements: Vec<String>,
        gate: EditorGate,
    ) {
        if self.safe_mode_confirm_open {
            return;
        }
        let Some((title, description, continue_label)) = dialog_copy(&gate) else {
            return;
        };
        self.safe_mode_confirm_open = true;
        let this = cx.entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let this_ok = this.clone();
            let this_cancel = this.clone();
            let this_continue = this.clone();
            let statements_continue = statements.clone();
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
                            Button::new("safe-mode-cancel")
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
                        .child(
                            Button::new("safe-mode-continue")
                                .label(continue_label)
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                    this_continue.update(cx, |workspace, cx| {
                                        workspace.safe_mode_confirm_open = false;
                                        workspace.begin_sql_run(
                                            tab_id,
                                            statements_continue.clone(),
                                            cx,
                                        );
                                    });
                                }),
                        ),
                )
        });
    }

    /// History Rerun confirm for a write (or no-WHERE / parse-failed). On continue,
    /// opens a query tab and runs.
    pub(super) fn show_history_safe_mode_confirm(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        connection_id: String,
        sql: String,
        gate: EditorGate,
    ) {
        if self.safe_mode_confirm_open {
            return;
        }
        let Some((title, description, continue_label)) = dialog_copy(&gate) else {
            return;
        };
        self.safe_mode_confirm_open = true;
        let this = cx.entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let this_ok = this.clone();
            let this_cancel = this.clone();
            let this_continue = this.clone();
            let connection_id = connection_id.clone();
            let sql = sql.clone();
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
                            Button::new("history-safe-mode-cancel")
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
                        .child(
                            Button::new("history-safe-mode-continue")
                                .label(continue_label)
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                    this_continue.update(cx, |workspace, cx| {
                                        workspace.safe_mode_confirm_open = false;
                                        workspace.open_query_tab_with_sql(
                                            &connection_id,
                                            &sql,
                                            true,
                                            window,
                                            cx,
                                        );
                                    });
                                }),
                        ),
                )
        });
    }
}

fn dialog_copy(gate: &EditorGate) -> Option<(String, String, &'static str)> {
    match gate {
        EditorGate::Allow | EditorGate::RefuseReadOnly => None,
        EditorGate::ConfirmWrite {
            write_sqls,
            has_reads,
        } => {
            let title = "Run this write?".into();
            let mut body = if write_sqls.len() == 1 {
                write_sqls[0].clone()
            } else {
                write_sqls
                    .iter()
                    .map(|s| truncate_one_line(s, 120).into_owned())
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            if *has_reads {
                body.push_str("\n\nReads in this batch will run only if you continue.");
            }
            Some((title, body, "Run"))
        }
        EditorGate::ConfirmNoWhere {
            kind,
            target_table,
            sql,
        } => {
            let title = "This statement has no WHERE".into();
            let target = target_table
                .as_deref()
                .map(|t| format!("every row in {t}"))
                .unwrap_or_else(|| "every row it targets".into());
            let body = format!(
                "This {} changes {}. It is not limited by a WHERE clause.\n\n{}",
                kind.verb(),
                target,
                sql
            );
            Some((title, body, "Run anyway"))
        }
        EditorGate::ConfirmParseFailed { sql } => {
            let title = "Sela could not parse this statement".into();
            let body =
                format!("It was not run. You can run it anyway, or cancel and edit it.\n\n{sql}");
            Some((title, body, "Run anyway"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sela_core::{decide_editor_gate, decide_grid_commit_gate, GridCommitGate, NoWhereKind};

    #[test]
    fn view_model_editor_gate_matches_core() {
        let stmts = [(
            "UPDATE t SET a = 1 WHERE id = 1".into(),
            StatementSafety {
                is_write: true,
                parse_failed: false,
                no_where: None,
                target_table: Some("t".into()),
            },
        )];
        assert_eq!(decide_editor_gate(SafeMode::Off, &stmts), EditorGate::Allow);
        assert!(matches!(
            decide_editor_gate(SafeMode::Confirm, &stmts),
            EditorGate::ConfirmWrite { .. }
        ));
        assert_eq!(
            decide_editor_gate(SafeMode::ReadOnly, &stmts),
            EditorGate::RefuseReadOnly
        );
    }

    #[test]
    fn view_model_grid_commit_gate() {
        assert_eq!(
            decide_grid_commit_gate(SafeMode::Off),
            GridCommitGate::Allow
        );
        assert_eq!(
            decide_grid_commit_gate(SafeMode::Confirm),
            GridCommitGate::Confirm
        );
        assert_eq!(
            decide_grid_commit_gate(SafeMode::ReadOnly),
            GridCommitGate::RefuseReadOnly
        );
    }

    #[test]
    fn no_where_dialog_copy() {
        let gate = EditorGate::ConfirmNoWhere {
            kind: NoWhereKind::Delete,
            target_table: Some("books".into()),
            sql: "DELETE FROM books".into(),
        };
        let (title, body, label) = dialog_copy(&gate).unwrap();
        assert_eq!(title, "This statement has no WHERE");
        assert!(body.contains("DELETE"));
        assert!(body.contains("books"));
        assert_eq!(label, "Run anyway");
    }

    #[test]
    fn parse_failed_dialog_copy() {
        let gate = EditorGate::ConfirmParseFailed {
            sql: "NOT SQL".into(),
        };
        let (title, body, label) = dialog_copy(&gate).unwrap();
        assert!(title.contains("could not parse"));
        assert!(body.contains("NOT SQL"));
        assert_eq!(label, "Run anyway");
    }
}
