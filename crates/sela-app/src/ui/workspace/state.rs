//! Connection lifecycle: connect, save/edit/delete, sample library, catalog refresh.

use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::WindowExt as _;
use gpui_kit::prelude::*;
use gpui_kit::{px, Context, SharedString, Window};

use sela_config::{
    keychain_unavailable_dialog_warning, memory_only_password_warning, PasswordPersistence,
    SavedConnection,
};
use sela_core::{
    count_tabs_requiring_close_confirm, duplicate_connection, DatabaseSession, DbError, Engine,
    LoadedSchema,
};
use sela_drivers::{connect, create_sample_database, load_catalog};

use super::super::form::{ConnectionDraft, ConnectionForm, FormEvent};
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
            Ok(Some(password)) => password,
            Ok(None) => {
                // Duplicate (and any row with no keychain item) has no secret.
                // Network engines must ask; SQLite has no password field.
                if needs_password_prompt(saved.engine) {
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
                String::new()
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
        let saved_id = saved.id.clone();
        let saved_name = saved.name.clone();
        self.active_id = Some(saved_id.clone());
        self.selected_connection_id = Some(saved_id.clone());
        self.session = None;
        self.catalog.clear();
        if let Some(view) = self.active_view_mut() {
            view.status = format!("Connecting to {saved_name}…");
        }
        self.rebuild_tree(cx);
        let tab_id = self.tabs.active().map(|tab| tab.id);
        self.spawn_work(
            tab_id,
            cx,
            |_| async move { open_session(saved, password).await },
            move |this, result, cx| match result {
                Ok((session, catalog)) => {
                    this.session = Some(session);
                    this.catalog = catalog;
                    this.expanded.insert(conn_key(&saved_id));
                    if let Some(schema) = this.catalog.first() {
                        this.expanded.insert(schema_key(&saved_id, &schema.name));
                    }
                    let count: usize = this.catalog.iter().map(|schema| schema.tables.len()).sum();
                    let mut status = format!("Connected to {saved_name} · {count} tables");
                    if let Some(warning) = &this.keychain_session_warning {
                        status = format!("{status} · {warning}");
                    }
                    // Successful connect must not leave a prior auth failure banner.
                    this.clear_connection_banner_error();
                    this.workspace_status = None;
                    if let Some(view) = this.active_view_mut() {
                        view.status = status;
                    }
                    this.rebuild_tree(cx);
                    // Load the active table tab now that the session is ready.
                    this.on_tab_activated(None, cx);
                }
                Err(err) => {
                    this.session = None;
                    this.catalog.clear();
                    if let Some(view) = this.active_view_mut() {
                        view.status = "Connection failed".into();
                        view.error = Some(err.to_string());
                    } else {
                        this.boot_error = Some(err.to_string());
                    }
                    this.rebuild_tree(cx);
                }
            },
        );
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
        let title: SharedString = if existing.is_some() {
            "Edit connection".into()
        } else {
            "New connection".into()
        };
        let form = cx.new(|cx| {
            ConnectionForm::new(
                existing,
                password,
                warning,
                keychain_unavailable,
                window,
                cx,
            )
        });
        let subscription =
            cx.subscribe_in(&form, window, |this, _, event, window, cx| match event {
                FormEvent::Submit(draft) => {
                    this.save_draft((**draft).clone(), Some(window), cx);
                    window.close_dialog(cx);
                }
                FormEvent::Cancel => window.close_dialog(cx),
            });
        self._subscriptions.push(subscription);
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title(title.clone())
                .width(px(560.))
                .child(form.clone())
        });
    }

    /// Duplicate the selected (or active) connection without copying the password.
    pub(super) fn duplicate_selected_connection(&mut self, cx: &mut Context<Self>) {
        let Some(source) = self.selected_or_active_connection().cloned() else {
            return;
        };
        let names: Vec<String> = self.connections.iter().map(|c| c.name.clone()).collect();
        let copy = duplicate_connection(&source, &names);
        let source_name = source.name.clone();
        self.connections.push(copy);
        if let Err(err) = self.store.save(&self.connections) {
            if let Some(view) = self.active_view_mut() {
                view.error = Some(err.to_string());
            } else {
                self.boot_error = Some(err.to_string());
            }
        }
        let status = format!("Duplicated {source_name}. Type the password to connect.");
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
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        let id = draft.connection.id.clone();
        let persistence = self.secrets.set(&id, &draft.password);
        if let Some(existing) = self.connections.iter_mut().find(|item| item.id == id) {
            *existing = draft.connection;
        } else {
            self.connections.push(draft.connection);
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
                None
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
        self.session = None;
        self.rebuild_tree(cx);
        self.connect_id(id, window, cx);
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
        self.secrets.delete(id);
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

async fn open_session(
    saved: SavedConnection,
    password: String,
) -> Result<(Arc<dyn DatabaseSession>, Vec<LoadedSchema>), DbError> {
    let session = connect(&saved, &password).await?;
    let catalog = load_catalog(session.as_ref()).await?;
    Ok((Arc::from(session), catalog))
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

/// Network engines need a typed password when no keychain/memory secret exists.
pub(super) fn needs_password_prompt(engine: Engine) -> bool {
    engine != Engine::Sqlite
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
    fn network_engines_need_password_prompt_sqlite_does_not() {
        assert!(needs_password_prompt(Engine::Postgres));
        assert!(needs_password_prompt(Engine::Mysql));
        assert!(!needs_password_prompt(Engine::Sqlite));
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
