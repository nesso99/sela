//! Connection lifecycle: connect, save/edit/delete, sample library, catalog refresh.

use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::WindowExt as _;
use gpui_kit::prelude::*;
use gpui_kit::{px, Context, SharedString, Window};

use sela_config::{
    keychain_unavailable_dialog_warning, memory_only_password_warning, ssh_passphrase_account,
    ssh_password_account, PasswordPersistence, SavedConnection,
};
use sela_core::{
    count_tabs_requiring_close_confirm, duplicate_connection, duplicate_status_message,
    DatabaseSession, DbError, Engine, LoadedSchema,
};
use sela_drivers::{
    connect_with_auth, create_sample_database, load_catalog, ConnectAuth, ConnectOutcome,
    SshAuthSecrets, UnknownSshHostKeyChallenge,
};
use sela_ssh::{
    db_error_is_host_key_mismatch, host_key_changed_title, unknown_host_key_body,
    unknown_host_key_title,
};

use super::super::form::{ConnectionDraft, ConnectionForm, FormEvent, FormSecrets};
use super::tree::{apply_selection_to_edit_target, conn_key, parse_node, schema_key, NodeId};
use super::{GridSource, Workspace};

// `open_table` lives in tabs.rs as `open_table_tab`.

impl Workspace {
    pub(super) fn on_selection(
        &mut self,
        id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.close_tabs_confirm_open {
            return;
        }
        if id == self.last_selection {
            return;
        }
        self.last_selection = id.clone();
        let Some(id) = id else {
            return;
        };
        apply_selection_to_edit_target(&mut self.selected_connection_id, &id);
        match parse_node(&id) {
            Some(NodeId::Connection(connection_id)) => {
                self.connect_id(connection_id, Some(window), cx)
            }
            Some(NodeId::Table { schema, table }) => self.open_table_tab(schema, table, window, cx),
            None => {}
        }
    }

    /// Connect to `id`. When `window` is set (Switch from the sidebar / tab bar),
    /// closing other connections' tabs with typed SQL or a busy query asks first.
    pub(super) fn connect_id(
        &mut self,
        id: String,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        if self.active_id.as_deref() == Some(id.as_str()) && self.session.is_some() {
            return;
        }
        if self.connections.iter().all(|item| item.id != id) {
            return;
        }
        let foreign = self.foreign_connection_ids(&id);
        if let Some((_confirm_id, name, tab_count, pending)) =
            self.first_close_confirm_target(&foreign, cx)
        {
            if let Some(window) = window {
                self.pending_close_tabs = Some(super::PendingCloseTabs::Connect { to_id: id });
                self.show_close_tabs_confirm(window, cx, &name, tab_count, pending);
                return;
            }
        }
        self.proceed_connect_id(id, window, cx);
    }

    fn proceed_connect_id(
        &mut self,
        id: String,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        let Some(saved) = self.connections.iter().find(|item| item.id == id).cloned() else {
            return;
        };
        // One live session at a time: cancel and close tabs for every other
        // connection so on_tab_activated cannot snap the session back.
        let foreign = self.foreign_connection_ids(&id);
        for previous in foreign {
            self.close_tabs_for_connection(&previous, cx);
        }
        // Drop a stale connection-error banner before the new attempt (and on
        // switch). Query / save errors on other paths set these again as needed.
        self.clear_connection_banner_error();
        self.workspace_status = None;
        let password = match self.secrets.get_optional(&saved.id) {
            Ok(optional) => {
                // Only Duplicate (needs_password) asks when no secret exists.
                // Empty-password / trust / peer rows have needs_password false and
                // connect with "" even when the keychain item is absent after restart.
                if saved.should_prompt_for_password(optional.as_deref()) {
                    if let Some(window) = window {
                        self.open_form(Some(saved), window, cx);
                        return;
                    }
                    let message = "Type the password to connect.".to_string();
                    if let Some(view) = self.active_view_mut() {
                        view.status = message;
                    } else {
                        self.workspace_status = Some(message);
                    }
                    cx.notify();
                    return;
                }
                optional.unwrap_or_default()
            }
            Err(err) => {
                let message = format!("Could not read the password from the OS keychain: {err}");
                if let Some(view) = self.active_view_mut() {
                    view.error = Some(message);
                    view.status = "Connection failed".into();
                } else {
                    self.boot_error = Some(message);
                }
                cx.notify();
                return;
            }
        };
        let auth = match self.connect_auth_for(&saved, password) {
            Ok(auth) => auth,
            Err(err) => {
                if let Some(view) = self.active_view_mut() {
                    view.error = Some(err);
                    view.status = "Connection failed".into();
                } else {
                    self.boot_error = Some(err);
                }
                cx.notify();
                return;
            }
        };
        let via_ssh = saved.ssh.enabled;
        let saved_id = saved.id.clone();
        let saved_name = saved.name.clone();
        self.active_id = Some(saved_id.clone());
        self.selected_connection_id = Some(saved_id.clone());
        self.session = None;
        self.session_via_ssh = false;
        self.catalog.clear();
        if let Some(view) = self.active_view_mut() {
            view.status = format!("Connecting to {saved_name}…");
        }
        self.rebuild_tree(cx);
        let tab_id = self.tabs.active().map(|tab| tab.id);
        let workspace = cx.entity();
        self.spawn_work(
            tab_id,
            cx,
            |_| async move { open_session_outcome(saved, auth).await },
            move |this, result, cx| match result {
                Ok(ConnectSession::Ready { session, catalog }) => {
                    this.finish_successful_connect(
                        session,
                        catalog,
                        &saved_id,
                        &saved_name,
                        via_ssh,
                        cx,
                    );
                }
                Ok(ConnectSession::UnknownHostKey(challenge)) => {
                    let _ = workspace;
                    this.show_unknown_host_key_dialog(challenge, saved_id, saved_name, via_ssh, cx);
                }
                Err(err) => {
                    this.handle_connect_failure(err, cx);
                }
            },
        );
    }

    fn connect_auth_for(
        &self,
        saved: &SavedConnection,
        db_password: String,
    ) -> Result<ConnectAuth, String> {
        let mut auth = ConnectAuth {
            db_password,
            ssh: SshAuthSecrets::default(),
            ssh_paths: None,
        };
        if !saved.ssh.enabled {
            return Ok(auth);
        }
        let ssh_password = self
            .secrets
            .get_optional(&ssh_password_account(&saved.id))
            .map_err(|err| {
                format!("Could not read the SSH password from the OS keychain: {err}")
            })?;
        let ssh_passphrase = self
            .secrets
            .get_optional(&ssh_passphrase_account(&saved.id))
            .map_err(|err| {
                format!("Could not read the SSH passphrase from the OS keychain: {err}")
            })?;
        auth.ssh = SshAuthSecrets {
            password: ssh_password.filter(|s| !s.is_empty()),
            passphrase: ssh_passphrase.filter(|s| !s.is_empty()),
        };
        Ok(auth)
    }

    fn finish_successful_connect(
        &mut self,
        session: Arc<dyn DatabaseSession>,
        catalog: Vec<LoadedSchema>,
        saved_id: &str,
        saved_name: &str,
        via_ssh: bool,
        cx: &mut Context<Self>,
    ) {
        self.session = Some(session);
        self.session_via_ssh = via_ssh;
        self.catalog = catalog;
        self.expanded.insert(conn_key(saved_id));
        if let Some(schema) = self.catalog.first() {
            self.expanded.insert(schema_key(saved_id, &schema.name));
        }
        let count: usize = self.catalog.iter().map(|schema| schema.tables.len()).sum();
        let mut status = format!("Connected to {saved_name} · {count} tables");
        if let Some(warning) = &self.keychain_session_warning {
            status = format!("{status} · {warning}");
        }
        self.clear_connection_banner_error();
        self.workspace_status = None;
        if let Some(conn) = self.connections.iter_mut().find(|item| item.id == saved_id) {
            if conn.needs_password {
                conn.clear_needs_password();
                if let Err(err) = self.store.save(&self.connections) {
                    status = format!("{status} · {err}");
                }
            }
        }
        if let Some(view) = self.active_view_mut() {
            view.status = status;
        }
        self.rebuild_tree(cx);
        self.on_tab_activated(None, cx);
    }

    fn handle_connect_failure(&mut self, err: DbError, cx: &mut Context<Self>) {
        self.session = None;
        self.session_via_ssh = false;
        self.catalog.clear();
        if db_error_is_host_key_mismatch(&err) {
            // SSH-07: hard-error dialog; OK dismisses only.
            let message = err.to_string();
            let this = cx.entity();
            // Prefer a window from the entity when available; fall back to banner.
            if let Some(view) = self.active_view_mut() {
                view.status = "Connection failed".into();
                view.error = Some(message.clone());
            } else {
                self.boot_error = Some(message.clone());
            }
            // Open the dialog when we have a window via a deferred notify path.
            self.pending_host_key_mismatch = Some(message);
            let _ = this;
            self.rebuild_tree(cx);
            cx.notify();
            return;
        }
        if let Some(view) = self.active_view_mut() {
            view.status = "Connection failed".into();
            view.error = Some(err.to_string());
        } else {
            self.boot_error = Some(err.to_string());
        }
        self.rebuild_tree(cx);
    }

    /// Show the SSH-07 hard-error dialog (no override).
    pub(super) fn show_host_key_mismatch_dialog_if_pending(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(body) = self.pending_host_key_mismatch.take() else {
            return;
        };
        window.open_alert_dialog(cx, move |alert, _, _| {
            alert
                .title(host_key_changed_title())
                .description(body.clone())
                .ok_text("OK")
                .on_ok(|_, _, _| true)
        });
    }

    fn show_unknown_host_key_dialog(
        &mut self,
        challenge: Box<UnknownSshHostKeyChallenge>,
        saved_id: String,
        saved_name: String,
        via_ssh: bool,
        cx: &mut Context<Self>,
    ) {
        // Defer to a pending slot so we can open the dialog with a Window on the
        // next frame (spawn_work callbacks may not have a window).
        self.pending_unknown_host_key = Some(super::PendingUnknownHostKey {
            challenge,
            saved_id,
            saved_name,
            via_ssh,
        });
        cx.notify();
    }

    /// Present the SSH-18 first-seen host key confirm when one is pending.
    pub(super) fn show_unknown_host_key_dialog_if_pending(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pending) = self.pending_unknown_host_key.take() else {
            return;
        };
        let super::PendingUnknownHostKey {
            challenge,
            saved_id,
            saved_name,
            via_ssh,
        } = pending;
        let host = challenge.host.clone();
        let fingerprint = challenge.fingerprint.clone();
        let body = unknown_host_key_body(&host, &fingerprint);
        let challenge_slot = std::sync::Arc::new(std::sync::Mutex::new(Some(challenge)));
        let this = cx.entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let this_cancel = this.clone();
            let this_accept = this.clone();
            let challenge_cancel = challenge_slot.clone();
            let challenge_accept = challenge_slot.clone();
            let challenge_enter = challenge_slot.clone();
            let this_enter = this.clone();
            alert
                .title(unknown_host_key_title())
                .description(body.clone())
                // Enter / Escape cancel (quit-style; Cancel is primary).
                .on_ok(move |_, _, cx| {
                    reject_pending_host_key(&challenge_enter);
                    this_enter.update(cx, |workspace, cx| {
                        workspace.handle_connect_failure(
                            DbError::ssh("SSH host key was not accepted."),
                            cx,
                        );
                    });
                    true
                })
                .on_cancel({
                    let challenge_esc = challenge_slot.clone();
                    let this_esc = this.clone();
                    move |_, _, cx| {
                        reject_pending_host_key(&challenge_esc);
                        this_esc.update(cx, |workspace, cx| {
                            workspace.handle_connect_failure(
                                DbError::ssh("SSH host key was not accepted."),
                                cx,
                            );
                        });
                        true
                    }
                })
                .footer(
                    DialogFooter::new()
                        .child(
                            Button::new("ssh-host-key-cancel")
                                .label("Cancel")
                                .primary()
                                .on_click(move |_, window, cx| {
                                    reject_pending_host_key(&challenge_cancel);
                                    this_cancel.update(cx, |workspace, cx| {
                                        workspace.handle_connect_failure(
                                            DbError::ssh("SSH host key was not accepted."),
                                            cx,
                                        );
                                    });
                                    window.close_dialog(cx);
                                }),
                        )
                        .child({
                            let saved_id = saved_id.clone();
                            let saved_name = saved_name.clone();
                            Button::new("ssh-host-key-accept")
                                .label("Accept & save")
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                    let Some(ch) = challenge_accept
                                        .lock()
                                        .ok()
                                        .and_then(|mut slot| slot.take())
                                    else {
                                        return;
                                    };
                                    let saved_id = saved_id.clone();
                                    let saved_name = saved_name.clone();
                                    this_accept.update(cx, |workspace, cx| {
                                        workspace.accept_unknown_host_key(
                                            ch, saved_id, saved_name, via_ssh, cx,
                                        );
                                    });
                                })
                        }),
                )
        });
    }

    fn accept_unknown_host_key(
        &mut self,
        challenge: Box<UnknownSshHostKeyChallenge>,
        saved_id: String,
        saved_name: String,
        via_ssh: bool,
        cx: &mut Context<Self>,
    ) {
        // Test-connection path: Accept writes known_hosts and updates the Test line.
        if let Some((form, generation, test_via_ssh)) = self.pending_test_after_host_key.take() {
            let started = std::time::Instant::now();
            cx.spawn(async move |this, async_cx| {
                let result =
                    sela_drivers::drive(async move { challenge.accept_and_remember().await }).await;
                let ms = started.elapsed().as_millis() as u64;
                this.update(async_cx, |workspace, cx| {
                    if workspace.connection_test_generation != generation {
                        return;
                    }
                    match result {
                        Ok(session) => {
                            drop(session);
                            form.update(cx, |form, cx| {
                                form.set_test_result(
                                    super::super::form::ssh::TestResult::Success {
                                        ms,
                                        via_ssh: test_via_ssh,
                                    },
                                    cx,
                                );
                            });
                        }
                        Err(err) => {
                            form.update(cx, |form, cx| {
                                form.set_test_result(
                                    super::super::form::ssh::TestResult::Failure(err.to_string()),
                                    cx,
                                );
                            });
                        }
                    }
                })
                .ok();
            })
            .detach();
            return;
        }
        let tab_id = self.tabs.active().map(|tab| tab.id);
        self.spawn_work(
            tab_id,
            cx,
            |_| async move {
                let session = challenge.accept_and_remember().await?;
                let catalog = load_catalog(session.as_ref()).await?;
                Ok(ConnectSession::Ready {
                    session: Arc::from(session),
                    catalog,
                })
            },
            move |this, result, cx| match result {
                Ok(ConnectSession::Ready { session, catalog }) => {
                    this.finish_successful_connect(
                        session,
                        catalog,
                        &saved_id,
                        &saved_name,
                        via_ssh,
                        cx,
                    );
                }
                Ok(ConnectSession::UnknownHostKey(_)) => {
                    this.handle_connect_failure(
                        DbError::ssh("SSH host key challenge recurred unexpectedly."),
                        cx,
                    );
                }
                Err(err) => this.handle_connect_failure(err, cx),
            },
        );
    }

    /// When a tunneled session's DB connection dies, surface SSH-11 copy.
    pub(super) fn note_possible_ssh_tunnel_drop(&mut self, err: &DbError, cx: &mut Context<Self>) {
        if !self.session_via_ssh {
            return;
        }
        if !matches!(
            err,
            DbError::Connection { .. } | DbError::Internal { .. } | DbError::Query { .. }
        ) {
            return;
        }
        // Drop the DB session; tabs and pending edits stay (SSH-11).
        self.session = None;
        self.session_via_ssh = false;
        self.catalog.clear();
        if let Some(view) = self.active_view_mut() {
            view.error = Some("SSH tunnel closed. The connection was dropped.".into());
            view.status = "SSH tunnel closed.".into();
        } else {
            self.boot_error = Some("SSH tunnel closed. The connection was dropped.".into());
            self.workspace_status = Some("SSH tunnel closed.".into());
        }
        self.rebuild_tree(cx);
        cx.notify();
    }

    /// Clears the connection-error banner sources (boot + active tab).
    pub(super) fn clear_connection_banner_error(&mut self) {
        let active_id = self.tabs.active().map(|tab| tab.id);
        let view_error =
            active_id.and_then(|id| self.views.get_mut(&id).map(|view| &mut view.error));
        clear_connection_banner_sources(&mut self.boot_error, view_error);
    }

    fn foreign_connection_ids(&self, new_id: &str) -> Vec<String> {
        self.tabs
            .tabs()
            .iter()
            .filter(|tab| !tab.connection_id.is_empty() && tab.connection_id != new_id)
            .map(|tab| tab.connection_id.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    /// Editor SQL + busy + pending-grid-edits for each tab of `connection_id`.
    fn close_confirm_inputs_for(
        &self,
        connection_id: &str,
        cx: &Context<Self>,
    ) -> Vec<(String, bool, bool)> {
        self.tabs
            .tabs_for_connection(connection_id)
            .map(|tab| {
                let view = self.views.get(&tab.id);
                let sql = view
                    .map(|view| view.sql.read(cx).value().to_string())
                    .unwrap_or_default();
                let busy = view.is_some_and(|view| view.busy);
                let dirty = view
                    .and_then(|view| view.edit.as_ref())
                    .is_some_and(|edit| edit.is_dirty());
                (sql, busy, dirty)
            })
            .collect()
    }

    /// Pending grid-edit count across tabs for `connection_id`.
    fn pending_edit_count_for_connection(&self, connection_id: &str) -> usize {
        self.tabs
            .tabs_for_connection(connection_id)
            .filter_map(|tab| self.views.get(&tab.id))
            .filter_map(|view| view.edit.as_ref())
            .map(|edit| edit.pending_counts().total())
            .sum()
    }

    /// First connection among `ids` that has typed SQL, a busy query, or pending edits.
    /// Returns `(connection_id, display_name, tab_count, pending_edits)`.
    fn first_close_confirm_target(
        &self,
        ids: &[String],
        cx: &Context<Self>,
    ) -> Option<(String, String, usize, usize)> {
        for id in ids {
            let inputs = self.close_confirm_inputs_for(id, cx);
            let tab_count = inputs.len();
            if tab_count == 0 {
                continue;
            }
            let at_risk = count_tabs_requiring_close_confirm(
                inputs
                    .iter()
                    .map(|(sql, busy, dirty)| (sql.as_str(), *busy || *dirty)),
            );
            if at_risk == 0 {
                continue;
            }
            let pending = self.pending_edit_count_for_connection(id);
            let name = self
                .connections
                .iter()
                .find(|item| &item.id == id)
                .map(|item| item.name.clone())
                .unwrap_or_else(|| id.clone());
            return Some((id.clone(), name, tab_count, pending));
        }
        None
    }

    fn show_close_tabs_confirm(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        connection_name: &str,
        tab_count: usize,
        pending_edits: usize,
    ) {
        if self.close_tabs_confirm_open {
            return;
        }
        self.close_tabs_confirm_open = true;
        let title = if tab_count == 1 {
            format!("Close 1 tab for {connection_name}?")
        } else {
            format!("Close {tab_count} tabs for {connection_name}?")
        };
        let description = if pending_edits > 0 {
            format!(
                "Unsaved SQL in the editor will be lost. {pending_edits} pending changes will be discarded."
            )
        } else {
            "Unsaved SQL in the editor will be lost.".into()
        };
        let this = cx.entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let this_ok = this.clone();
            let this_cancel = this.clone();
            let this_close = this.clone();
            alert
                .title(title.clone())
                .description(description.clone())
                // Cancel is the safer default (Enter / primary). Esc uses on_cancel.
                .on_ok(move |_, _, cx| {
                    this_ok.update(cx, |workspace, _| {
                        workspace.close_tabs_confirm_open = false;
                        workspace.pending_close_tabs = None;
                    });
                    true
                })
                .on_cancel(move |_, _, cx| {
                    this_cancel.update(cx, |workspace, _| {
                        workspace.close_tabs_confirm_open = false;
                        workspace.pending_close_tabs = None;
                    });
                    true
                })
                .footer(
                    DialogFooter::new()
                        .child(
                            Button::new("close-tabs-cancel")
                                .label("Cancel")
                                .primary()
                                .on_click({
                                    let this = this.clone();
                                    move |_, window, cx| {
                                        this.update(cx, |workspace, _| {
                                            workspace.close_tabs_confirm_open = false;
                                            workspace.pending_close_tabs = None;
                                        });
                                        window.close_dialog(cx);
                                    }
                                }),
                        )
                        .child(
                            Button::new("close-tabs-confirm")
                                .label("Close tabs")
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                    this_close.update(cx, |workspace, cx| {
                                        workspace.close_tabs_confirm_open = false;
                                        match workspace.pending_close_tabs.take() {
                                            Some(super::PendingCloseTabs::Connect { to_id }) => {
                                                workspace.proceed_connect_id(
                                                    to_id,
                                                    Some(window),
                                                    cx,
                                                );
                                            }
                                            Some(super::PendingCloseTabs::Disconnect) => {
                                                workspace.perform_disconnect(cx);
                                                workspace.rebuild_tree(cx);
                                            }
                                            None => {}
                                        }
                                    });
                                }),
                        ),
                )
        });
    }

    pub(super) fn open_form(
        &mut self,
        existing: Option<SavedConnection>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (password, mut warning) = match existing.as_ref() {
            Some(connection) => match self.secrets.get(&connection.id) {
                Ok(password) => (password, None),
                Err(err) => (
                    String::new(),
                    Some(format!("The saved password could not be read: {err}")),
                ),
            },
            None => (String::new(), None),
        };
        let keychain_unavailable = match self.secrets.probe_keychain() {
            Ok(()) => false,
            Err(err) => {
                if warning.is_none() {
                    warning = Some(keychain_unavailable_dialog_warning(&err.to_string()));
                }
                true
            }
        };
        if !keychain_unavailable {
            if let Some(session_warning) = &self.keychain_session_warning {
                if warning.is_none() {
                    warning = Some(session_warning.clone());
                }
            }
        }
        let (ssh_password, ssh_passphrase) = match existing.as_ref() {
            Some(connection) => {
                let ssh_pw = self
                    .secrets
                    .get(&ssh_password_account(&connection.id))
                    .unwrap_or_default();
                let ssh_pp = self
                    .secrets
                    .get(&ssh_passphrase_account(&connection.id))
                    .unwrap_or_default();
                (ssh_pw, ssh_pp)
            }
            None => (String::new(), String::new()),
        };
        let title: SharedString = if existing.is_some() {
            "Edit connection".into()
        } else {
            "New connection".into()
        };
        let form = cx.new(|cx| {
            ConnectionForm::with_secrets(
                FormSecrets {
                    existing,
                    password,
                    ssh_password,
                    ssh_passphrase,
                    warning,
                    keychain_unavailable,
                },
                window,
                cx,
            )
        });
        self.connection_form = Some(form.clone());
        let subscription =
            cx.subscribe_in(&form, window, |this, _, event, window, cx| match event {
                FormEvent::Submit(draft) => {
                    this.connection_form = None;
                    this.save_draft((**draft).clone(), true, Some(window), cx);
                    window.close_dialog(cx);
                }
                FormEvent::SaveOnly(draft) => {
                    this.connection_form = None;
                    this.save_draft((**draft).clone(), false, Some(window), cx);
                    window.close_dialog(cx);
                }
                FormEvent::Test(draft) => {
                    this.run_connection_test((**draft).clone(), cx);
                }
                FormEvent::Cancel => {
                    if let Some(form) = this.connection_form.take() {
                        form.update(cx, |form, cx| form.cancel_test(cx));
                    }
                    window.close_dialog(cx);
                }
            });
        self._subscriptions.push(subscription);
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title(title.clone())
                .width(px(560.))
                // Single-line Input propagates Enter after PressEnter. The kit
                // dialog binds Enter → Confirm; default on_ok closes without
                // FormEvent::Submit. Keep the dialog open so Import URL Enter
                // only fills the form (05-connection-dialog.md); Save uses the
                // form button / FormEvent path (#96).
                .on_ok(|_, _, _| false)
                .child(form.clone())
        });
    }

    fn run_connection_test(&mut self, draft: ConnectionDraft, cx: &mut Context<Self>) {
        let generation = self
            .connection_form
            .as_ref()
            .map(|form| form.read(cx).test_generation())
            .unwrap_or(0);
        self.connection_test_generation = generation;
        let via_ssh = draft.connection.ssh.enabled;
        let mut auth = ConnectAuth {
            db_password: draft.password,
            ssh: SshAuthSecrets {
                password: if draft.ssh_password.is_empty() {
                    None
                } else {
                    Some(draft.ssh_password)
                },
                passphrase: if draft.ssh_passphrase.is_empty() {
                    None
                } else {
                    Some(draft.ssh_passphrase)
                },
            },
            ssh_paths: None,
        };
        // Prefer injectable paths from the open form (UI tests).
        if let Some(form) = &self.connection_form {
            if let Ok(paths) = form.read(cx).ssh_paths_or_default() {
                // Only override when the form has an explicit test override.
                let _ = paths;
            }
            auth.ssh_paths = form.read(cx).ssh_paths_override_for_test();
        }
        let connection = draft.connection;
        let started = std::time::Instant::now();
        let form = self.connection_form.clone();
        cx.spawn(async move |this, async_cx| {
            let result =
                sela_drivers::drive(async move { connect_with_auth(&connection, auth).await })
                    .await;
            let ms = started.elapsed().as_millis() as u64;
            this.update(async_cx, |workspace, cx| {
                let Some(form) = form else {
                    return;
                };
                if workspace.connection_test_generation != generation {
                    return;
                }
                match result {
                    Ok(ConnectOutcome::Ready(session)) => {
                        // Drop the test session immediately (does not replace workspace).
                        drop(session);
                        form.update(cx, |form, cx| {
                            form.set_test_result(
                                super::super::form::ssh::TestResult::Success { ms, via_ssh },
                                cx,
                            );
                        });
                    }
                    Ok(ConnectOutcome::UnknownSshHostKey(challenge)) => {
                        // Test path: stash challenge and open dialog on next paint.
                        workspace.pending_unknown_host_key = Some(super::PendingUnknownHostKey {
                            challenge,
                            saved_id: String::new(),
                            saved_name: String::new(),
                            via_ssh,
                        });
                        workspace.pending_test_after_host_key = Some((form, generation, via_ssh));
                        cx.notify();
                    }
                    Err(err) => {
                        if db_error_is_host_key_mismatch(&err) {
                            workspace.pending_host_key_mismatch = Some(err.to_string());
                            form.update(cx, |form, cx| {
                                form.set_test_result(
                                    super::super::form::ssh::TestResult::Failure(err.to_string()),
                                    cx,
                                );
                            });
                            cx.notify();
                        } else {
                            form.update(cx, |form, cx| {
                                form.set_test_result(
                                    super::super::form::ssh::TestResult::Failure(err.to_string()),
                                    cx,
                                );
                            });
                        }
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// Duplicate the selected (or active) connection without copying the password.
    pub(super) fn duplicate_selected_connection(&mut self, cx: &mut Context<Self>) {
        let Some(source) = self.selected_or_active_connection().cloned() else {
            return;
        };
        let names: Vec<String> = self.connections.iter().map(|c| c.name.clone()).collect();
        let copy = duplicate_connection(&source, &names);
        let source_name = source.name.clone();
        let status = duplicate_status_message(&source_name, copy.needs_password);
        self.connections.push(copy);
        if let Err(err) = self.store.save(&self.connections) {
            if let Some(view) = self.active_view_mut() {
                view.error = Some(err.to_string());
            } else {
                self.boot_error = Some(err.to_string());
            }
        }
        // With no tab open the status bar is not mounted; keep the sentence on
        // the workspace so the empty pane can show it (#88).
        if let Some(view) = self.active_view_mut() {
            view.status = status;
            self.workspace_status = None;
        } else {
            self.workspace_status = Some(status);
        }
        self.rebuild_tree(cx);
        cx.notify();
    }

    pub(super) fn save_draft(
        &mut self,
        draft: ConnectionDraft,
        reconnect: bool,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        let id = draft.connection.id.clone();
        let persistence = self.secrets.set(&id, &draft.password);
        // SSH secrets: same keychain path as the DB password (SSH-13).
        let _ = self
            .secrets
            .set(&ssh_password_account(&id), &draft.ssh_password);
        let ssh_pp = self
            .secrets
            .set(&ssh_passphrase_account(&id), &draft.ssh_passphrase);
        let mut connection = draft.connection;
        // Save always clears the Duplicate prompt flag (empty or non-empty secret).
        connection.clear_needs_password();
        if let Some(existing) = self.connections.iter_mut().find(|item| item.id == id) {
            *existing = connection;
        } else {
            self.connections.push(connection);
        }
        let status_note = match persistence {
            PasswordPersistence::Keychain => {
                self.keychain_session_warning = None;
                Some("Password saved in the OS keychain.".into())
            }
            PasswordPersistence::MemoryOnly(message) => {
                let warning = memory_only_password_warning(&message);
                self.keychain_session_warning = Some(warning.clone());
                Some(warning)
            }
            PasswordPersistence::Empty => {
                // Empty secret does not clear a prior memory-only warning for
                // other connections; only clear when this write was intentional empty.
                // Still surface memory-only when only SSH secrets fell back.
                if matches!(ssh_pp, PasswordPersistence::MemoryOnly(_)) {
                    let warning = memory_only_password_warning(
                        "The OS keychain is unavailable for SSH secrets.",
                    );
                    self.keychain_session_warning = Some(warning.clone());
                    Some(warning)
                } else {
                    None
                }
            }
        };
        if let Some(view) = self.active_view_mut() {
            // Put the keychain sentence in status (status bar reads view.status).
            if let Some(note) = status_note.clone() {
                view.status = note.clone();
                view.note = Some(note);
            } else {
                view.note = None;
            }
        }
        if let Err(err) = self.store.save(&self.connections) {
            if let Some(view) = self.active_view_mut() {
                view.error = Some(err.to_string());
            } else {
                self.boot_error = Some(err.to_string());
            }
        }
        self.rebuild_tree(cx);
        if reconnect {
            self.session = None;
            self.session_via_ssh = false;
            self.connect_id(id, window, cx);
        }
    }

    pub(super) fn confirm_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(connection) = self.selected_or_active_connection().cloned() else {
            return;
        };
        let id = connection.id.clone();
        let pending = self.pending_edit_count_for_connection(&id);
        let description = if pending > 0 {
            format!(
                "The saved connection is removed. The database itself is left untouched. {pending} pending changes will be discarded."
            )
        } else {
            "The saved connection is removed. The database itself is left untouched.".into()
        };
        let this = cx.entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let this = this.clone();
            let id = id.clone();
            alert
                .title(format!("Delete {}?", connection.name))
                .description(description.clone())
                .confirm()
                .ok_text("Delete")
                .ok_variant(gpui_kit::component::button::ButtonVariant::Danger)
                .on_ok(move |_, _, cx| {
                    this.update(cx, |workspace, cx| workspace.delete_connection(&id, cx));
                    true
                })
        });
    }

    pub(super) fn delete_connection(&mut self, id: &str, cx: &mut Context<Self>) {
        // SSH-13: drop DB password and SSH password/passphrase together.
        self.secrets.delete_connection(id);
        self.connections.retain(|item| item.id != id);
        if self.selected_connection_id.as_deref() == Some(id) {
            self.selected_connection_id = None;
        }
        self.close_tabs_for_connection(id, cx);
        if self.active_id.as_deref() == Some(id) {
            self.perform_disconnect(cx);
        }
        if let Err(err) = self.store.save(&self.connections) {
            if let Some(view) = self.active_view_mut() {
                view.error = Some(err.to_string());
            } else {
                self.boot_error = Some(err.to_string());
            }
        }
        self.rebuild_tree(cx);
        cx.notify();
    }

    /// Cancel running work on tabs for `connection_id`, close them, and persist.
    pub(super) fn close_tabs_for_connection(
        &mut self,
        connection_id: &str,
        cx: &mut Context<Self>,
    ) {
        let removed = self.tabs.close_for_connection(connection_id);
        for tab in &removed {
            self.views.remove(&tab.id);
        }
        if !removed.is_empty() {
            self.persist_tabs(cx);
        }
    }

    /// Disconnect from the sidebar. Confirms when the active connection's tabs
    /// have typed SQL or a busy query.
    pub(super) fn request_disconnect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.close_tabs_confirm_open {
            return;
        }
        let Some(id) = self.active_id.clone() else {
            self.perform_disconnect(cx);
            self.rebuild_tree(cx);
            return;
        };
        if let Some((_, name, tab_count, pending)) = self.first_close_confirm_target(&[id], cx) {
            self.pending_close_tabs = Some(super::PendingCloseTabs::Disconnect);
            self.show_close_tabs_confirm(window, cx, &name, tab_count, pending);
            return;
        }
        self.perform_disconnect(cx);
        self.rebuild_tree(cx);
    }

    pub(super) fn perform_disconnect(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.active_id.clone() {
            self.close_tabs_for_connection(&id, cx);
        }
        self.session = None;
        self.session_via_ssh = false;
        self.catalog.clear();
        self.active_id = None;
        self.clear_all_structure_caches();
        // Drop a stale connection-error banner so Disconnect does not leave it
        // over the empty content pane.
        self.clear_connection_banner_error();
        // After closing this connection's tabs there is usually no active view.
        if let Some(view) = self.active_view_mut() {
            view.source = GridSource::Idle;
            view.table_pane = super::TablePane::Data;
            view.page_offset = 0;
            view.page_len = 0;
            view.total_rows = None;
            view.status = "Disconnected".into();
            view.grid.update(cx, |state, cx| {
                state
                    .delegate_mut()
                    .show_message("Open a table, or run a query.");
                state.refresh(cx);
            });
        }
        cx.notify();
    }

    pub(super) fn refresh_catalog(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            return;
        };
        // Do not refresh (or clear structure caches) while a commit is in flight.
        if self
            .views
            .values()
            .any(|v| v.edit.as_ref().is_some_and(|e| e.committing))
        {
            if let Some(view) = self.active_view_mut() {
                view.status = "Committing…".into();
            }
            cx.notify();
            return;
        }
        let active = self.active_id.clone();
        self.clear_all_structure_caches();
        if let Some(view) = self.active_view_mut() {
            view.status = "Refreshing tables…".into();
        }
        let tab_id = self.tabs.active().map(|tab| tab.id);
        self.spawn_work(
            tab_id,
            cx,
            |_| async move { load_catalog(session.as_ref()).await },
            move |this, result, cx| match result {
                Ok(catalog) => {
                    this.catalog = catalog;
                    if let Some(id) = active {
                        this.expanded.insert(conn_key(&id));
                    }
                    if let Some(view) = this.active_view_mut() {
                        view.status = "Tables refreshed".into();
                    }
                    this.rebuild_tree(cx);
                }
                Err(err) => {
                    if let Some(view) = this.active_view_mut() {
                        view.error = Some(err.to_string());
                        view.status = "Refresh failed".into();
                    }
                    cx.notify();
                }
            },
        );
    }

    pub(super) fn create_sample(&mut self, cx: &mut Context<Self>) {
        let path = sample_database_path(&self.store);
        if let Some(view) = self.active_view_mut() {
            view.status = "Creating the sample library…".into();
            view.error = None;
        }
        let tab_id = self.tabs.active().map(|tab| tab.id);
        self.spawn_work(
            tab_id,
            cx,
            |_| async move { ensure_sample(&path).await },
            |this, result, cx| match result {
                Ok(path) => {
                    let path_text = path.display().to_string();
                    if let Some(existing) = this
                        .connections
                        .iter()
                        .find(|item| item.engine == Engine::Sqlite && item.file_path == path_text)
                        .cloned()
                    {
                        this.connect_id(existing.id, None, cx);
                        return;
                    }
                    let mut connection = SavedConnection::new(Engine::Sqlite);
                    connection.name = "Sample library".into();
                    connection.file_path = path_text;
                    connection.host.clear();
                    let id = connection.id.clone();
                    this.connections.push(connection);
                    if let Err(err) = this.store.save(&this.connections) {
                        if let Some(view) = this.active_view_mut() {
                            view.error = Some(err.to_string());
                        } else {
                            this.boot_error = Some(err.to_string());
                        }
                    }
                    this.rebuild_tree(cx);
                    this.connect_id(id, None, cx);
                }
                Err(err) => {
                    if let Some(view) = this.active_view_mut() {
                        view.error = Some(err.to_string());
                        view.status = "Could not create the sample library".into();
                    } else {
                        this.boot_error = Some(err.to_string());
                    }
                    cx.notify();
                }
            },
        );
    }
}

enum ConnectSession {
    Ready {
        session: Arc<dyn DatabaseSession>,
        catalog: Vec<LoadedSchema>,
    },
    UnknownHostKey(Box<UnknownSshHostKeyChallenge>),
}

async fn open_session_outcome(
    saved: SavedConnection,
    auth: ConnectAuth,
) -> Result<ConnectSession, DbError> {
    match connect_with_auth(&saved, auth).await? {
        ConnectOutcome::Ready(session) => {
            let catalog = load_catalog(session.as_ref()).await?;
            Ok(ConnectSession::Ready {
                session: Arc::from(session),
                catalog,
            })
        }
        ConnectOutcome::UnknownSshHostKey(challenge) => {
            Ok(ConnectSession::UnknownHostKey(challenge))
        }
    }
}

fn reject_pending_host_key(
    slot: &std::sync::Arc<std::sync::Mutex<Option<Box<UnknownSshHostKeyChallenge>>>>,
) {
    if let Ok(mut guard) = slot.lock() {
        if let Some(ch) = guard.take() {
            let _ = ch.reject();
        }
    }
}

async fn ensure_sample(path: &std::path::Path) -> Result<PathBuf, DbError> {
    if !path.exists() {
        create_sample_database(path).await?;
    }
    Ok(path.to_path_buf())
}

fn sample_database_path(store: &sela_config::ConfigStore) -> PathBuf {
    store
        .path()
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .join("sample.sqlite")
}

/// Banner text when tabs are open: active tab error, else workspace boot error.
pub(super) fn resolve_banner_error(
    boot_error: Option<&str>,
    active_view_error: Option<&str>,
) -> Option<String> {
    active_view_error
        .map(str::to_string)
        .or_else(|| boot_error.map(str::to_string))
}

/// Status line when no tab is open (Duplicate and similar).
pub(super) fn resolve_workspace_status(
    view_status: Option<&str>,
    workspace_status: Option<&str>,
) -> Option<String> {
    if let Some(status) = view_status {
        if !status.is_empty() {
            return Some(status.to_string());
        }
    }
    workspace_status
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Clears connection-lifecycle banner sources (boot + optional active tab).
/// Mirrors [`Workspace::clear_connection_banner_error`].
fn clear_connection_banner_sources(
    boot_error: &mut Option<String>,
    active_view_error: Option<&mut Option<String>>,
) {
    *boot_error = None;
    if let Some(error) = active_view_error {
        *error = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banner_prefers_active_view_error_over_boot() {
        assert_eq!(
            resolve_banner_error(Some("boot"), Some("view")).as_deref(),
            Some("view")
        );
        assert_eq!(
            resolve_banner_error(Some("boot"), None).as_deref(),
            Some("boot")
        );
        assert_eq!(resolve_banner_error(None, None), None);
    }

    #[test]
    fn clearing_connection_banner_drops_boot_and_view_errors() {
        let mut boot = Some("Access denied for user 'selaqa'".into());
        let mut view_err = Some("password authentication failed".into());
        clear_connection_banner_sources(&mut boot, Some(&mut view_err));
        assert!(boot.is_none());
        assert!(view_err.is_none());
        assert_eq!(
            resolve_banner_error(boot.as_deref(), view_err.as_deref()),
            None
        );
    }

    #[test]
    fn empty_password_restart_does_not_prompt_duplicate_does() {
        // Fresh CachingSecrets after set("") → None (restart). Flag decides prompt.
        use sela_config::{CachingSecrets, MemorySecretStore, PasswordPersistence};
        use sela_core::SavedConnection;

        let mut conn = SavedConnection::new(Engine::Postgres);
        let secrets = CachingSecrets::new(MemorySecretStore::default());
        assert_eq!(secrets.set(&conn.id, ""), PasswordPersistence::Empty);
        assert_eq!(secrets.get_optional(&conn.id).unwrap().as_deref(), Some(""));

        let restarted = CachingSecrets::new(MemorySecretStore::default());
        assert_eq!(restarted.get_optional(&conn.id).unwrap(), None);
        assert!(
            !conn.should_prompt_for_password(restarted.get_optional(&conn.id).unwrap().as_deref())
        );

        conn.needs_password = true;
        assert!(
            conn.should_prompt_for_password(restarted.get_optional(&conn.id).unwrap().as_deref())
        );
        conn.clear_needs_password();
        assert!(!conn.should_prompt_for_password(None));
    }

    #[test]
    fn workspace_status_fills_in_when_no_tab_status() {
        assert_eq!(
            resolve_workspace_status(None, Some("Duplicated Red. Type the password to connect."))
                .as_deref(),
            Some("Duplicated Red. Type the password to connect.")
        );
        assert_eq!(
            resolve_workspace_status(Some("Connected"), Some("Duplicated …")).as_deref(),
            Some("Connected")
        );
        assert_eq!(
            resolve_workspace_status(Some(""), Some("Duplicated …")).as_deref(),
            Some("Duplicated …")
        );
        assert_eq!(resolve_workspace_status(None, None), None);
    }

    #[test]
    fn clearing_connection_banner_with_no_active_view_clears_boot_only() {
        let mut boot = Some("Connection failed".into());
        clear_connection_banner_sources(&mut boot, None);
        assert!(boot.is_none());
    }
}
