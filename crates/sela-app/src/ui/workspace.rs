//! Main window: saved connections, schema tree, result grid, and the SQL editor.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Textarea, TextareaState};
use gpui_kit::component::table::{DataTable, TableState};
use gpui_kit::component::tree::{Tree, TreeEvent, TreeItem, TreeState};
use gpui_kit::component::{
    h_flex, list::ListItem, v_flex, ActiveTheme, Disableable as _, Sizable as _, WindowExt as _,
};
use gpui_kit::prelude::*;
use gpui_kit::{
    div, px, App, Context, Entity, IntoElement, Render, SharedString, Subscription, Window,
};

use sela_config::{
    CachingSecrets, ConfigStore, KeyringStore, PasswordPersistence, SavedConnection,
};
use sela_core::{
    preview_table, run_user_sql, DatabaseSession, DbError, Engine, LoadedSchema, RelationKind,
    StatementOutcome, TableInfo, PAGE_SIZE,
};
use sela_drivers::{connect, create_sample_database, drive, load_catalog};

use super::form::{ConnectionDraft, ConnectionForm, FormEvent};
use super::grid::GridDelegate;
use super::RunQuery;

const SEP: char = '\u{1f}';

pub struct Workspace {
    store: ConfigStore,
    secrets: CachingSecrets<KeyringStore>,
    connections: Vec<SavedConnection>,
    active_id: Option<String>,
    session: Option<Arc<dyn DatabaseSession>>,
    catalog: Vec<LoadedSchema>,
    expanded: HashSet<String>,
    tree: Entity<TreeState>,
    grid: Entity<TableState<GridDelegate>>,
    sql: Entity<TextareaState>,
    source: GridSource,
    page_offset: u64,
    page_len: usize,
    total_rows: Option<u64>,
    paged: bool,
    status: String,
    error: Option<String>,
    note: Option<String>,
    busy: bool,
    request: u64,
    last_selection: Option<String>,
    _subscriptions: Vec<Subscription>,
}

#[derive(Clone)]
enum GridSource {
    Idle,
    Table { schema: String, table: String },
    Sql(String),
}

enum NodeId {
    Connection(String),
    Table { schema: String, table: String },
}

impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let store = ConfigStore::with_path(ConfigStore::default_path());
        let (connections, error) = match store.load() {
            Ok(connections) => (connections, None),
            Err(err) => (Vec::new(), Some(err.to_string())),
        };
        let tree = cx.new(|cx| TreeState::new(cx));
        let grid = cx.new(|cx| {
            TableState::new(GridDelegate::empty(), window, cx)
                .sortable(false)
                .col_movable(false)
        });
        let sql = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(6)
                .placeholder("SELECT * FROM books\n-- Ctrl+Enter or ⌘+Enter runs the statement")
        });
        let tree_events = cx.subscribe(&tree, |this, _, event: &TreeEvent, cx| {
            match event {
                TreeEvent::Expanded(id) => {
                    this.expanded.insert(id.to_string());
                }
                TreeEvent::Collapsed(id) => {
                    this.expanded.remove(id.as_ref());
                }
            }
            cx.notify();
        });
        let tree_selection = cx.observe(&tree, |this, tree, cx| {
            let selected = tree
                .read(cx)
                .selected_item()
                .map(|item| item.id.to_string());
            this.on_selection(selected, cx);
        });
        let mut workspace = Self {
            store,
            secrets: CachingSecrets::new(KeyringStore),
            connections,
            active_id: None,
            session: None,
            catalog: Vec::new(),
            expanded: HashSet::new(),
            tree,
            grid,
            sql,
            source: GridSource::Idle,
            page_offset: 0,
            page_len: 0,
            total_rows: None,
            paged: false,
            status: "No database connected".into(),
            error,
            note: None,
            busy: false,
            request: 0,
            last_selection: None,
            _subscriptions: vec![tree_events, tree_selection],
        };
        workspace.rebuild_tree(cx);
        workspace
    }

    fn bump(&mut self) -> u64 {
        self.request = self.request.wrapping_add(1);
        self.request
    }

    fn spawn_work<T, Done>(
        &mut self,
        cx: &mut Context<Self>,
        work: impl std::future::Future<Output = Result<T, DbError>> + Send + 'static,
        done: Done,
    ) where
        T: Send + 'static,
        Done: FnOnce(&mut Self, Result<T, DbError>, &mut Context<Self>) + 'static,
    {
        let request = self.bump();
        self.busy = true;
        cx.notify();
        cx.spawn(async move |this, async_cx| {
            let result = drive(work).await;
            this.update(async_cx, |this, cx| {
                if this.request != request {
                    return;
                }
                this.busy = false;
                done(this, result, cx);
            })
            .ok();
        })
        .detach();
    }

    fn rebuild_tree(&mut self, cx: &mut Context<Self>) {
        let items = self.tree_items();
        self.tree.update(cx, |tree, cx| {
            tree.set_items(items, cx);
        });
        self.last_selection = None;
    }

    fn tree_items(&self) -> Vec<TreeItem> {
        self.connections
            .iter()
            .map(|connection| self.connection_item(connection))
            .collect()
    }

    fn connection_item(&self, connection: &SavedConnection) -> TreeItem {
        let key = conn_key(&connection.id);
        let active = self.active_id.as_deref() == Some(connection.id.as_str());
        let mark = if active && self.session.is_some() {
            "●"
        } else {
            "○"
        };
        let mut item = TreeItem::new(
            key.clone(),
            format!("{mark}  {}  {}", connection.engine.short(), connection.name),
        );
        if active {
            let children = self.catalog.iter().map(|schema| {
                let schema_key = schema_key(&connection.id, &schema.name);
                let mut schema_item = TreeItem::new(schema_key.clone(), schema.name.clone())
                    .expanded(self.expanded.contains(&schema_key));
                if schema.tables.is_empty() {
                    schema_item = schema_item.child(
                        TreeItem::new(format!("n{SEP}{schema_key}"), "No tables").disabled(true),
                    );
                } else {
                    schema_item = schema_item.children(
                        schema
                            .tables
                            .iter()
                            .map(|table| table_item(&connection.id, &schema.name, table)),
                    );
                }
                schema_item
            });
            item = item
                .children(children)
                .expanded(self.expanded.contains(&key));
        }
        item
    }

    fn on_selection(&mut self, id: Option<String>, cx: &mut Context<Self>) {
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

    fn connect_id(&mut self, id: String, cx: &mut Context<Self>) {
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

    fn open_table(&mut self, schema: String, table: String, cx: &mut Context<Self>) {
        self.source = GridSource::Table { schema, table };
        self.page_offset = 0;
        self.load_source(cx);
    }

    fn run_editor(&mut self, cx: &mut Context<Self>) {
        let sql = self.sql.read(cx).value().to_string();
        if sql.trim().is_empty() {
            self.status = "Write a SQL statement, then run it.".into();
            self.error = None;
            cx.notify();
            return;
        }
        self.source = GridSource::Sql(sql);
        self.page_offset = 0;
        self.load_source(cx);
    }

    fn load_source(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            self.error = Some("Connect to a database before running SQL.".into());
            self.status = "Not connected".into();
            cx.notify();
            return;
        };
        let source = self.source.clone();
        let offset = self.page_offset;
        self.error = None;
        self.note = None;
        self.status = "Running…".into();
        self.grid.update(cx, |state, cx| {
            state.delegate_mut().loading = true;
            state.refresh(cx);
        });
        self.spawn_work(
            cx,
            async move { run_source(session, source, offset).await },
            |this, result, cx| this.apply_outcome(result, cx),
        );
    }

    fn apply_outcome(
        &mut self,
        result: Result<Option<StatementOutcome>, DbError>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(Some(StatementOutcome::Rows(rows))) => {
                self.page_len = rows.page.rows.len();
                self.total_rows = rows.total_rows;
                self.paged = rows.paged;
                self.note = rows.note.clone();
                let start = if self.page_len == 0 {
                    0
                } else {
                    self.page_offset + 1
                };
                let end = self.page_offset + self.page_len as u64;
                let ms = rows.duration.as_millis();
                self.status = match rows.total_rows {
                    Some(total) => format!("{total} rows · {ms} ms · {start}–{end}"),
                    None => format!("{} rows · {ms} ms", self.page_len),
                };
                if let Some(note) = &rows.note {
                    self.status = format!("{} · {note}", self.status);
                }
                self.grid.update(cx, |state, cx| {
                    state.delegate_mut().show_page(rows.page);
                    state.refresh(cx);
                });
            }
            Ok(Some(StatementOutcome::Executed(executed))) => {
                self.page_len = 0;
                self.total_rows = None;
                self.paged = false;
                self.note = None;
                let message = format!(
                    "{} rows affected · {} ms",
                    executed.rows_affected,
                    executed.duration.as_millis()
                );
                self.status = message.clone();
                self.grid.update(cx, |state, cx| {
                    state.delegate_mut().show_message(message);
                    state.refresh(cx);
                });
            }
            Ok(None) => {}
            Err(err) => {
                self.status = "Query failed".into();
                self.error = Some(err.to_string());
                self.grid.update(cx, |state, cx| {
                    state.delegate_mut().loading = false;
                    state.refresh(cx);
                });
            }
        }
        cx.notify();
    }

    fn step_page(&mut self, delta: i64, cx: &mut Context<Self>) {
        if delta < 0 {
            if self.page_offset == 0 {
                return;
            }
            self.page_offset = self.page_offset.saturating_sub(PAGE_SIZE as u64);
        } else if self.can_go_next() {
            self.page_offset += PAGE_SIZE as u64;
        } else {
            return;
        }
        self.load_source(cx);
    }

    fn can_go_next(&self) -> bool {
        if self.page_len == 0 {
            return false;
        }
        if let Some(total) = self.total_rows {
            self.page_offset + (self.page_len as u64) < total
        } else {
            self.paged && self.page_len as u32 == PAGE_SIZE
        }
    }

    fn open_form(
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

    fn save_draft(&mut self, draft: ConnectionDraft, cx: &mut Context<Self>) {
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

    fn confirm_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    fn delete_connection(&mut self, id: &str, cx: &mut Context<Self>) {
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

    fn disconnect(&mut self, cx: &mut Context<Self>) {
        self.session = None;
        self.catalog.clear();
        self.source = GridSource::Idle;
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

    fn refresh_catalog(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            return;
        };
        let active = self.active_id.clone();
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

    fn create_sample(&mut self, cx: &mut Context<Self>) {
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

    fn source_label(&self) -> String {
        match &self.source {
            GridSource::Idle => "No table open".into(),
            GridSource::Table { schema, table } => format!("{schema}.{table}"),
            GridSource::Sql(_) => "SQL result".into(),
        }
    }

    fn active_connection(&self) -> Option<&SavedConnection> {
        self.active_id
            .as_ref()
            .and_then(|id| self.connections.iter().find(|item| &item.id == id))
    }
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.entity();
        let connected = self.session.is_some();
        let can_next = self.can_go_next();
        let can_prev = self.page_offset > 0;
        let page_number = self.page_offset / PAGE_SIZE as u64 + 1;
        let page_label = match self.total_rows {
            Some(total) if self.paged => {
                let pages = total.div_ceil(PAGE_SIZE as u64).max(1);
                format!("Page {page_number} of {pages}")
            }
            _ => format!("Page {page_number} · {PAGE_SIZE} rows"),
        };
        let endpoint = self
            .active_connection()
            .map(|connection| {
                format!(
                    "{} · {}",
                    connection.engine.label(),
                    connection.endpoint_label()
                )
            })
            .unwrap_or_else(|| "Choose a connection to browse its tables.".into());

        div()
            .id("sela-root")
            .size_full()
            .flex()
            .flex_row()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_action({
                let this = this.clone();
                move |_: &RunQuery, _, cx| {
                    this.update(cx, |workspace, cx| workspace.run_editor(cx));
                }
            })
            .child(self.sidebar(cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_w(px(0.))
                    .h_full()
                    .child(self.toolbar(cx, &endpoint, &page_label, can_prev, can_next, connected))
                    .when_some(self.error.clone(), |column, error| {
                        column.child(
                            div()
                                .px_3()
                                .py_2()
                                .text_sm()
                                .bg(cx.theme().danger)
                                .text_color(cx.theme().danger_foreground)
                                .child(error),
                        )
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_h(px(0.))
                            .overflow_hidden()
                            .child(DataTable::new(&self.grid).stripe(true).bordered(true)),
                    )
                    .child(
                        v_flex()
                            .h(px(176.))
                            .border_t_1()
                            .border_color(cx.theme().border)
                            .child(
                                h_flex()
                                    .px_3()
                                    .h(px(28.))
                                    .items_center()
                                    .justify_between()
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child("SQL"),
                                    )
                                    .child({
                                        let this = this.clone();
                                        Button::new("run-query")
                                            .small()
                                            .primary()
                                            .label(if self.busy { "Running…" } else { "Run" })
                                            .disabled(self.busy || !connected)
                                            .tooltip("Run the statement (Ctrl+Enter)")
                                            .on_click(move |_, _, cx| {
                                                this.update(cx, |workspace, cx| {
                                                    workspace.run_editor(cx)
                                                });
                                            })
                                    }),
                            )
                            .child(
                                div().flex_1().px_2().pb_2().child(
                                    Textarea::new(&self.sql)
                                        .font_family(cx.theme().mono_font_family.clone()),
                                ),
                            ),
                    )
                    .child(
                        h_flex()
                            .h(px(28.))
                            .px_3()
                            .items_center()
                            .justify_between()
                            .border_t_1()
                            .border_color(cx.theme().border)
                            .bg(cx.theme().muted)
                            .text_xs()
                            .child(self.status.clone())
                            .child(self.source_label()),
                    ),
            )
    }
}

impl Workspace {
    fn sidebar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.entity();
        let has_active = self.active_id.is_some();
        v_flex()
            .w(px(292.))
            .h_full()
            .flex_shrink_0()
            .border_r_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().muted)
            .child(
                v_flex().px_3().pt_3().pb_2().gap_1().child(
                    div().text_lg().child("Sela"),
                ).child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("PostgreSQL, MySQL, and SQLite"),
                ),
            )
            .child(
                h_flex().px_3().pb_2().gap_2().child({
                    let this = this.clone();
                    Button::new("new-connection")
                        .small()
                        .primary()
                        .label("New")
                        .on_click(move |_, window, cx| {
                            this.update(cx, |workspace, cx| workspace.open_form(None, window, cx));
                        })
                }).child({
                    let this = this.clone();
                    Button::new("sample-library")
                        .small()
                        .label("Sample library")
                        .on_click(move |_, _, cx| {
                            this.update(cx, |workspace, cx| workspace.create_sample(cx));
                        })
                }),
            )
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .child(Tree::new(&self.tree, render_tree_entry).size_full()),
            )
            .when(self.connections.is_empty(), |sidebar| {
                sidebar.child(
                    div()
                        .px_3()
                        .pb_3()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("No saved connections yet. Create one, or open the sample SQLite library."),
                )
            })
            .child(
                h_flex()
                    .px_3()
                    .py_2()
                    .gap_1()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child({
                        let this = this.clone();
                        Button::new("edit-connection")
                            .xsmall()
                            .ghost()
                            .label("Edit")
                            .disabled(!has_active)
                            .on_click(move |_, window, cx| {
                                this.update(cx, |workspace, cx| {
                                    let existing = workspace.active_connection().cloned();
                                    if let Some(existing) = existing {
                                        workspace.open_form(Some(existing), window, cx);
                                    }
                                });
                            })
                    })
                    .child({
                        let this = this.clone();
                        Button::new("delete-connection")
                            .xsmall()
                            .ghost()
                            .label("Delete")
                            .disabled(!has_active)
                            .on_click(move |_, window, cx| {
                                this.update(cx, |workspace, cx| workspace.confirm_delete(window, cx));
                            })
                    })
                    .child({
                        let this = this.clone();
                        Button::new("refresh-catalog")
                            .xsmall()
                            .ghost()
                            .label("Refresh")
                            .disabled(self.session.is_none())
                            .on_click(move |_, _, cx| {
                                this.update(cx, |workspace, cx| workspace.refresh_catalog(cx));
                            })
                    })
                    .child({
                        let this = this.clone();
                        Button::new("disconnect")
                            .xsmall()
                            .ghost()
                            .label("Disconnect")
                            .disabled(self.session.is_none())
                            .on_click(move |_, _, cx| {
                                this.update(cx, |workspace, cx| {
                                    workspace.disconnect(cx);
                                    workspace.rebuild_tree(cx);
                                });
                            })
                    }),
            )
    }

    fn toolbar(
        &self,
        cx: &Context<Self>,
        endpoint: &str,
        page_label: &str,
        can_prev: bool,
        can_next: bool,
        connected: bool,
    ) -> impl IntoElement {
        let this = cx.entity();
        h_flex()
            .h(px(44.))
            .px_3()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                v_flex().child(self.source_label()).child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(endpoint.to_string()),
                ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(page_label.to_string()),
                    )
                    .child({
                        let this = this.clone();
                        Button::new("page-prev")
                            .xsmall()
                            .label("Previous")
                            .disabled(!can_prev || self.busy || !connected)
                            .on_click(move |_, _, cx| {
                                this.update(cx, |workspace, cx| workspace.step_page(-1, cx));
                            })
                    })
                    .child({
                        let this = this;
                        Button::new("page-next")
                            .xsmall()
                            .label("Next")
                            .disabled(!can_next || self.busy || !connected)
                            .on_click(move |_, _, cx| {
                                this.update(cx, |workspace, cx| workspace.step_page(1, cx));
                            })
                    }),
            )
    }
}

fn render_tree_entry(
    _: usize,
    entry: &gpui_kit::component::tree::TreeEntry,
    _: bool,
    _: &mut Window,
    _: &mut App,
) -> ListItem {
    let depth = entry.depth() as f32;
    let marker = if entry.is_folder() {
        if entry.is_expanded() {
            "▾"
        } else {
            "▸"
        }
    } else if entry.item().id.starts_with('t') {
        "·"
    } else {
        ""
    };
    let label = if marker.is_empty() {
        entry.item().label.to_string()
    } else {
        format!("{marker}  {}", entry.item().label)
    };
    ListItem::new(entry.item().id.clone())
        .pl(px(8. + depth * 14.))
        .child(label)
}

fn table_item(connection: &str, schema: &str, table: &TableInfo) -> TreeItem {
    let suffix = match table.kind {
        RelationKind::View => "  ·  view",
        RelationKind::Table => "",
    };
    TreeItem::new(
        table_key(connection, schema, &table.name),
        format!("{}{suffix}", table.name),
    )
}

fn conn_key(id: &str) -> String {
    format!("c{SEP}{id}")
}

fn schema_key(connection: &str, schema: &str) -> String {
    format!("s{SEP}{connection}{SEP}{schema}")
}

fn table_key(connection: &str, schema: &str, table: &str) -> String {
    format!("t{SEP}{connection}{SEP}{schema}{SEP}{table}")
}

fn parse_node(id: &str) -> Option<NodeId> {
    let mut parts = id.split(SEP);
    match parts.next()? {
        "c" => Some(NodeId::Connection(parts.next()?.to_string())),
        "t" => {
            let _connection = parts.next()?;
            let schema = parts.next()?.to_string();
            let table = parts.collect::<Vec<_>>().join(&SEP.to_string());
            if table.is_empty() {
                None
            } else {
                Some(NodeId::Table { schema, table })
            }
        }
        _ => None,
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

async fn run_source(
    session: Arc<dyn DatabaseSession>,
    source: GridSource,
    offset: u64,
) -> Result<Option<StatementOutcome>, DbError> {
    match source {
        GridSource::Idle => Ok(None),
        GridSource::Table { schema, table } => {
            preview_table(session.as_ref(), &schema, &table, PAGE_SIZE, offset)
                .await
                .map(Some)
        }
        GridSource::Sql(sql) => run_user_sql(session.as_ref(), &sql, PAGE_SIZE, offset)
            .await
            .map(Some),
    }
}

async fn ensure_sample(path: &std::path::Path) -> Result<PathBuf, DbError> {
    if !path.exists() {
        create_sample_database(path).await?;
    }
    Ok(path.to_path_buf())
}

fn sample_database_path(store: &ConfigStore) -> PathBuf {
    store
        .path()
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .join("sample.sqlite")
}
