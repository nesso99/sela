//! Connection lifecycle: connect, save/edit/delete, sample library, catalog refresh.

use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::component::WindowExt as _;
use gpui_kit::prelude::*;
use gpui_kit::{px, Context, SharedString, Window};

use sela_config::{
    keychain_unavailable_dialog_warning, memory_only_password_warning, PasswordPersistence,
    SavedConnection,
};
use sela_core::{DatabaseSession, DbError, Engine, LoadedSchema};
use sela_drivers::{connect, create_sample_database, load_catalog};

use super::super::form::{ConnectionDraft, ConnectionForm, FormEvent};
use super::tree::{conn_key, connection_id_from_selection, parse_node, schema_key, NodeId};
use super::{GridSource, Workspace};

// `open_table` lives in tabs.rs as `open_table_tab`.

impl Workspace {
    pub(super) fn on_selection(
        &mut self,
        id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if id == self.last_selection {
            return;
        }
        self.last_selection = id.clone();
        let Some(id) = id else {
            return;
        };
        if let Some(connection_id) = connection_id_from_selection(&id) {
            self.selected_connection_id = Some(connection_id);
        }
        match parse_node(&id) {
            Some(NodeId::Connection(connection_id)) => self.connect_id(connection_id, cx),
            Some(NodeId::Table { schema, table }) => self.open_table_tab(schema, table, window, cx),
            None => {}
        }
    }

    pub(super) fn connect_id(&mut self, id: String, cx: &mut Context<Self>) {
        if self.active_id.as_deref() == Some(id.as_str()) && self.session.is_some() {
            return;
        }
        let Some(saved) = self.connections.iter().find(|item| item.id == id).cloned() else {
            return;
        };
        // One live session at a time: cancel and close tabs for every other
        // connection so on_tab_activated cannot snap the session back.
        let foreign: Vec<String> = self
            .tabs
            .tabs()
            .iter()
            .filter(|tab| !tab.connection_id.is_empty() && tab.connection_id != id)
            .map(|tab| tab.connection_id.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        for previous in foreign {
            self.close_tabs_for_connection(&previous, cx);
        }
        let password = match self.secrets.get(&saved.id) {
            Ok(password) => password,
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
            view.error = None;
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
                    if let Some(view) = this.active_view_mut() {
                        view.status = status;
                        view.error = None;
                    }
                    this.rebuild_tree(cx);
                    // Load the active table tab now that the session is ready.
                    this.on_tab_activated(cx);
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
                    this.save_draft((**draft).clone(), cx);
                    window.close_dialog(cx);
                }
                FormEvent::Cancel => window.close_dialog(cx),
            });
        self._subscriptions.push(subscription);
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title(title.clone())
                .width(px(520.))
                .child(form.clone())
        });
    }

    pub(super) fn save_draft(&mut self, draft: ConnectionDraft, cx: &mut Context<Self>) {
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
        self.connect_id(id, cx);
    }

    pub(super) fn confirm_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(connection) = self.selected_or_active_connection().cloned() else {
            return;
        };
        let id = connection.id.clone();
        let this = cx.entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let this = this.clone();
            let id = id.clone();
            alert
                .title(format!("Delete {}?", connection.name))
                .description(
                    "The saved connection is removed. The database itself is left untouched.",
                )
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
            self.disconnect(cx);
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

    pub(super) fn disconnect(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.active_id.clone() {
            self.close_tabs_for_connection(&id, cx);
        }
        self.session = None;
        self.catalog.clear();
        self.active_id = None;
        self.clear_all_structure_caches();
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
                        this.connect_id(existing.id, cx);
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
                    this.connect_id(id, cx);
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
