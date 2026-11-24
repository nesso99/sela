//! App quit: confirm when a query is running, then GPUI-bounded shutdown.

use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::WindowExt as _;
use gpui_kit::prelude::*;
use gpui_kit::{Context, Subscription, Window, SHUTDOWN_TIMEOUT};

use sela_config::HistoryStore;
use sela_core::DatabaseSession;
use sela_drivers::drive;

use super::Workspace;

impl Workspace {
    /// True when any tab has in-flight work (SQL run, Count, structure, connect, …).
    pub(super) fn any_query_running(&self) -> bool {
        self.views.values().any(|view| view.busy)
    }

    /// Single entry for Cmd/Ctrl+Q, File ▸ Quit / Quit Sela, and (on Linux/Windows)
    /// window close / Alt+F4.
    pub(crate) fn request_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.quit_confirm_open {
            // secondary-q while the quit dialog is open must not quit.
            return;
        }
        let pending = self.pending_edit_count();
        let busy = self.any_query_running();
        if busy || pending > 0 {
            self.show_quit_confirm(window, cx, busy, pending);
            return;
        }
        self.perform_quit(cx);
    }

    fn pending_edit_count(&self) -> usize {
        self.views
            .values()
            .filter_map(|v| v.edit.as_ref())
            .map(|e| e.pending_counts().total())
            .sum()
    }

    fn show_quit_confirm(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        busy: bool,
        pending: usize,
    ) {
        self.quit_confirm_open = true;
        let this = cx.entity();
        let description = match (busy, pending > 0) {
            (true, true) => format!(
                "A query is still running. Quitting will cancel it. {pending} pending changes will be discarded."
            ),
            (true, false) => "A query is still running. Quitting will cancel it.".into(),
            (false, true) => format!("{pending} pending changes will be discarded."),
            (false, false) => "Quit Sela?".into(),
        };
        window.open_alert_dialog(cx, move |alert, _, _| {
            let this_ok = this.clone();
            let this_cancel = this.clone();
            let this_quit = this.clone();
            alert
                .title("Quit Sela?")
                .description(description.clone())
                // Enter dismisses (safer default). Esc uses on_cancel. Quit is a
                // plain button, not Primary, and is not bound to Enter.
                .on_ok(move |_, _, cx| {
                    this_ok.update(cx, |workspace, _| {
                        workspace.quit_confirm_open = false;
                    });
                    true
                })
                .on_cancel(move |_, _, cx| {
                    this_cancel.update(cx, |workspace, _| {
                        workspace.quit_confirm_open = false;
                    });
                    true
                })
                .footer(
                    DialogFooter::new()
                        .child(
                            Button::new("quit-dialog-cancel")
                                .label("Cancel")
                                .primary()
                                .on_click({
                                    let this = this.clone();
                                    move |_, window, cx| {
                                        this.update(cx, |workspace, _| {
                                            workspace.quit_confirm_open = false;
                                        });
                                        window.close_dialog(cx);
                                    }
                                }),
                        )
                        .child(Button::new("quit-dialog-quit").label("Quit").on_click(
                            move |_, window, cx| {
                                window.close_dialog(cx);
                                this_quit.update(cx, |workspace, cx| {
                                    workspace.quit_confirm_open = false;
                                    workspace.perform_quit(cx);
                                });
                            },
                        )),
                )
        });
    }

    /// Cancel in-flight work immediately, then ask GPUI to quit.
    ///
    /// Pool close and history checkpoint run from [`Context::on_app_quit`],
    /// which GPUI polls for up to [`SHUTDOWN_TIMEOUT`] (200 ms) inside
    /// [`App::shutdown`].
    pub(super) fn perform_quit(&mut self, cx: &mut Context<Self>) {
        self.tabs.cancel_all();
        for view in self.views.values_mut() {
            view.busy = false;
        }
        cx.quit();
    }

    /// Register the shutdown future GPUI awaits on quit (capped at
    /// [`SHUTDOWN_TIMEOUT`]).
    pub(super) fn subscribe_app_quit(cx: &mut Context<Self>) -> Subscription {
        let _ = SHUTDOWN_TIMEOUT; // documented contract; enforced by GPUI
        cx.on_app_quit(|this, _cx| {
            this.tabs.cancel_all();
            for view in this.views.values_mut() {
                view.busy = false;
            }
            let session = this.session.take();
            let history = this.history_store.take();
            async move {
                run_shutdown(session, history).await;
            }
        })
    }
}

/// Close the session pool and flush history. The caller (GPUI `on_app_quit`)
/// caps how long this future may run via [`SHUTDOWN_TIMEOUT`].
pub(crate) async fn run_shutdown(
    session: Option<Arc<dyn DatabaseSession>>,
    history: Option<Arc<HistoryStore>>,
) {
    let _ = drive(async move {
        if let Some(session) = session {
            session.close().await;
        }
        if let Some(history) = history {
            match Arc::try_unwrap(history) {
                Ok(store) => {
                    let _ = store.checkpoint_and_close();
                }
                Err(shared) => {
                    let _ = shared.checkpoint();
                }
            }
        }
        Ok(())
    })
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use sela_config::HistoryStore;
    use sela_core::{HistoryEntry, HistoryOutcome};

    #[tokio::test]
    async fn run_shutdown_checkpoints_history() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.sqlite");
        let store = HistoryStore::open(&path).unwrap().store;
        let started = Utc.with_ymd_and_hms(2026, 4, 1, 0, 0, 0).unwrap();
        store
            .insert(&HistoryEntry {
                id: "q1".into(),
                connection_id: "c1".into(),
                connection_name: "demo".into(),
                database: "db".into(),
                sql: "select 1".into(),
                started_at: started,
                duration_ms: 1,
                rows_label: "1".into(),
                outcome: HistoryOutcome::Success,
                error_message: String::new(),
            })
            .unwrap();
        let history = Some(Arc::new(store));
        run_shutdown(None, history).await;
        let reopened = HistoryStore::open(&path).unwrap().store;
        assert_eq!(reopened.count(None).unwrap(), 1);
    }

    #[tokio::test]
    async fn run_shutdown_with_nothing_to_close() {
        run_shutdown(None, None).await;
    }
}
