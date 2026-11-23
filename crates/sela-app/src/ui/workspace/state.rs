//! Connection lifecycle: connect, save/edit/delete, sample library, catalog refresh.

use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::component::WindowExt as _;
use gpui_kit::prelude::*;
use gpui_kit::{px, Context, SharedString, Window};

use sela_config::{PasswordPersistence, SavedConnection};
use sela_core::{DatabaseSession, DbError, Engine, LoadedSchema};
use sela_drivers::{connect, create_sample_database, load_catalog};

use super::super::form::{ConnectionDraft, ConnectionForm, FormEvent};
use super::tree::{conn_key, parse_node, schema_key, NodeId};
use super::{GridSource, Workspace};

impl Workspace {
    pub(super) fn on_selection(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        if id == self.last_selection {
            return;
        }
        self.last_selection = id.clone();
        let Some(id) = id else {
            return;
        };
        match parse_node(&id) {
            Some(NodeId::Connection(connection_id)) => self.connect_id(connection_id, cx),
            Some(NodeId::Table { schema, table }) => self.open_table(schema, table, cx),
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
        let password = match self.secrets.get(&saved.id) {
            Ok(password) => password,
            Err(err) => {
                self.error = Some(format!(
                    "Could not read the password from the OS keychain: {err}"
                ));
                self.status = "Connection failed".into();
                cx.notify();
                return;
            }
        };
        let saved_id = saved.id.clone();
        let saved_name = saved.name.clone();
        self.active_id = Some(saved_id.clone());
        self.session = None;
        self.catalog.clear();
        self.source = GridSource::Idle;
        self.error = None;
        self.status = format!("Connecting to {saved_name}…");
        self.rebuild_tree(cx);
        self.spawn_work(
            cx,
            async move { open_session(saved, password).await },
            move |this, result, cx| match result {
                Ok((session, catalog)) => {
                    this.session = Some(session);
                    this.catalog = catalog;
                    this.expanded.insert(conn_key(&saved_id));
                    if let Some(schema) = this.catalog.first() {
                        this.expanded.insert(schema_key(&saved_id, &schema.name));
                    }
                    let count: usize = this.catalog.iter().map(|schema| schema.tables.len()).sum();
                    this.status = format!("Connected to {saved_name} · {count} tables");
                    this.error = None;
                    this.rebuild_tree(cx);
                }
                Err(err) => {
                    this.session = None;
                    this.catalog.clear();
                    this.status = "Connection failed".into();
                    this.error = Some(err.to_string());
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
        let (password, warning) = match existing.as_ref() {
            Some(connection) => match self.secrets.get(&connection.id) {
                Ok(password) => (password, None),
                Err(err) => (
                    String::new(),
                    Some(format!("The saved password could not be read: {err}")),
                ),
            },
            None => (String::new(), None),
        };
        let title: SharedString = if existing.is_some() {
            "Edit connection".into()
        } else {
            "New connection".into()
        };
        let form = cx.new(|cx| ConnectionForm::new(existing, password, warning, window, cx));
        let subscription =
            cx.subscribe_in(&form, window, |this, _, event, window, cx| match event {
                FormEvent::Submit(draft) => {
                    this.save_draft(draft.clone(), cx);
                    window.close_dialog(cx);
                }
                FormEvent::Cancel => window.close_dialog(cx),
            });
        self._subscriptions.push(subscription);
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title(title.clone())
                .width(px(460.))
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
        self.note = match persistence {
            PasswordPersistence::Keychain => Some("Password saved in the OS keychain.".into()),
            PasswordPersistence::MemoryOnly(message) => Some(format!(
                "The OS keychain is unavailable, so the password is kept for this session only. {message}"
            )),
            PasswordPersistence::Empty => None,
        };
        if let Err(err) = self.store.save(&self.connections) {
            self.error = Some(err.to_string());
        }
        self.session = None;
        self.rebuild_tree(cx);
        self.connect_id(id, cx);
    }

    pub(super) fn confirm_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.active_id.clone() else {
            return;
        };
        let Some(connection) = self.connections.iter().find(|item| item.id == id).cloned() else {
            return;
        };
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
        if self.active_id.as_deref() == Some(id) {
            self.disconnect(cx);
        }
        if let Err(err) = self.store.save(&self.connections) {
            self.error = Some(err.to_string());
        }
        self.rebuild_tree(cx);
        cx.notify();
    }

    pub(super) fn disconnect(&mut self, cx: &mut Context<Self>) {
        self.session = None;
        self.catalog.clear();
        self.source = GridSource::Idle;
        self.table_pane = super::TablePane::Data;
        self.clear_structure_cache();
        self.page_offset = 0;
        self.page_len = 0;
        self.total_rows = None;
        self.status = "Disconnected".into();
        self.grid.update(cx, |state, cx| {
            state
                .delegate_mut()
                .show_message("Open a table, or run a query.");
            state.refresh(cx);
        });
    }

    pub(super) fn refresh_catalog(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            return;
        };
        let active = self.active_id.clone();
        self.clear_structure_cache();
        self.status = "Refreshing tables…".into();
        self.spawn_work(
            cx,
            async move { load_catalog(session.as_ref()).await },
            move |this, result, cx| match result {
                Ok(catalog) => {
                    this.catalog = catalog;
                    if let Some(id) = active {
                        this.expanded.insert(conn_key(&id));
                    }
                    this.status = "Tables refreshed".into();
                    this.rebuild_tree(cx);
                }
                Err(err) => {
                    this.error = Some(err.to_string());
                    this.status = "Refresh failed".into();
                    cx.notify();
                }
            },
        );
    }

    pub(super) fn create_sample(&mut self, cx: &mut Context<Self>) {
        let path = sample_database_path(&self.store);
        self.status = "Creating the sample library…".into();
        self.error = None;
        self.spawn_work(
            cx,
            async move { ensure_sample(&path).await },
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
                        this.error = Some(err.to_string());
                    }
                    this.rebuild_tree(cx);
                    this.connect_id(id, cx);
                }
                Err(err) => {
                    this.error = Some(err.to_string());
                    this.status = "Could not create the sample library".into();
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
