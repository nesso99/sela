//! Main window: saved connections, schema tree, result grid, and the SQL editor.

mod content;
mod query;
mod sidebar;
mod state;
mod status;
mod tree;

use std::collections::HashSet;
use std::sync::Arc;

use gpui_kit::component::input::TextareaState;
use gpui_kit::component::table::TableState;
use gpui_kit::component::tree::{TreeEvent, TreeState};
use gpui_kit::component::{v_flex, ActiveTheme};
use gpui_kit::prelude::*;
use gpui_kit::{div, px, Context, Entity, IntoElement, Render, Subscription, Window};

use sela_config::{CachingSecrets, ConfigStore, KeyringStore, SavedConnection};
use sela_core::{DatabaseSession, LoadedSchema};

use super::grid::GridDelegate;
use super::RunQuery;

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
        let page_number = self.page_offset / sela_core::PAGE_SIZE as u64 + 1;
        let page_label = match self.total_rows {
            Some(total) if self.paged => {
                let pages = total.div_ceil(sela_core::PAGE_SIZE as u64).max(1);
                format!("Page {page_number} of {pages}")
            }
            _ => format!("Page {page_number} · {} rows", sela_core::PAGE_SIZE),
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
                        column.child(self.error_banner(cx, error))
                    })
                    .child(self.results_area())
                    .child(self.sql_editor(cx, connected))
                    .child(self.status_bar(cx)),
            )
    }
}
