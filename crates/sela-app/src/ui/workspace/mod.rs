//! Main window: saved connections, schema tree, result grid, and the SQL editor.

mod content;
mod filter_bar;
mod history;
mod query;
mod result_strip;
mod sidebar;
mod sql_editor;
mod sql_find;
mod sql_gutter;
mod sql_run;
mod state;
mod status;
mod structure;
mod tab_view;
mod tabs;
mod tree;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gpui_kit::component::input::InputState;
use gpui_kit::component::tree::{TreeEvent, TreeState};
use gpui_kit::component::{v_flex, ActiveTheme};
use gpui_kit::prelude::*;
use gpui_kit::{div, px, Context, Entity, IntoElement, Render, Subscription, Window};

use sela_config::{
    CachingSecrets, ConfigStore, HistoryStore, KeyringStore, SavedConnection, TabStore,
};
use sela_core::{DatabaseSession, HistoryEntry, HistoryOutcome, LoadedSchema, TabId, TabList};

use super::{
    ActivateTab, CancelQuery, CloseTab, EscapeFilterBar, EscapeHistory, FindInSql, FindNext,
    FindPrevious, FocusHistorySearch, HistoryCopy, HistoryDelete, HistoryInsert, HistoryOpen,
    HistoryRerun, HistorySelectDown, HistorySelectUp, InsertFilterRow, NewQueryTab,
    RemoveFilterRow, RunAllSql, RunQuery, ShowTableData, ShowTableStructure, ToggleEditor,
    ToggleFilterBar, ToggleHistory,
};
use history::{HistoryFocusRestore, HistoryScope, DEFAULT_HISTORY_HEIGHT};
use tab_view::TabView;

pub struct Workspace {
    store: ConfigStore,
    tab_store: TabStore,
    history_store: Option<Arc<HistoryStore>>,
    secrets: CachingSecrets<KeyringStore>,
    connections: Vec<SavedConnection>,
    active_id: Option<String>,
    session: Option<Arc<dyn DatabaseSession>>,
    catalog: Vec<LoadedSchema>,
    expanded: HashSet<String>,
    tree: Entity<TreeState>,
    tabs: TabList,
    views: HashMap<TabId, TabView>,
    /// Workspace-level load error (e.g. connections.json), shown when no tab error.
    boot_error: Option<String>,
    last_selection: Option<String>,
    history_open: bool,
    history_height: f32,
    history_scope: HistoryScope,
    history_outcome: Option<HistoryOutcome>,
    history_search: Entity<InputState>,
    history_search_text: String,
    history_rows: Vec<HistoryEntry>,
    history_selected: Option<usize>,
    history_request: u64,
    history_panel_error: Option<String>,
    history_reset_notice: bool,
    history_write_failed_once: bool,
    history_focus_restore: HistoryFocusRestore,
    _subscriptions: Vec<Subscription>,
}

#[derive(Clone)]
pub(super) enum GridSource {
    Idle,
    Table { schema: String, table: String },
    Sql(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TablePane {
    Data,
    Structure,
}

impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let store = ConfigStore::with_path(ConfigStore::default_path());
        let tab_store = TabStore::with_path(TabStore::default_path());
        let (connections, boot_error) = match store.load() {
            Ok(connections) => (connections, None),
            Err(err) => (Vec::new(), Some(err.to_string())),
        };
        let (history_store, history_reset_notice, history_panel_error) =
            match HistoryStore::open_default() {
                Ok(open) => (Some(Arc::new(open.store)), open.was_reset, None),
                Err(err) => (None, false, Some(err.to_string())),
            };
        let tree = cx.new(|cx| TreeState::new(cx));
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
        let tree_selection = cx.observe_in(&tree, window, |this, tree, window, cx| {
            let selected = tree
                .read(cx)
                .selected_item()
                .map(|item| item.id.to_string());
            this.on_selection(selected, window, cx);
        });
        let history_search = cx.new(|cx| InputState::new(window, cx).placeholder("Search SQL"));
        let history_search_events =
            cx.subscribe_in(&history_search, window, |this, input, event, window, cx| {
                use gpui_kit::component::input::InputEvent;
                match event {
                    InputEvent::Change => {
                        this.history_search_text = input.read(cx).value().to_string();
                        this.refresh_history(cx);
                    }
                    InputEvent::PressEnter { .. } => {
                        this.history_open_in_tab(window, cx);
                    }
                    _ => {}
                }
            });
        let mut workspace = Self {
            store,
            tab_store,
            history_store,
            secrets: CachingSecrets::new(KeyringStore),
            connections,
            active_id: None,
            session: None,
            catalog: Vec::new(),
            expanded: HashSet::new(),
            tree,
            tabs: TabList::new(),
            views: HashMap::new(),
            boot_error,
            last_selection: None,
            history_open: false,
            history_height: DEFAULT_HISTORY_HEIGHT,
            history_scope: HistoryScope::ThisConnection,
            history_outcome: None,
            history_search,
            history_search_text: String::new(),
            history_rows: Vec::new(),
            history_selected: None,
            history_request: 0,
            history_panel_error,
            history_reset_notice,
            history_write_failed_once: false,
            history_focus_restore: HistoryFocusRestore::None,
            _subscriptions: vec![tree_events, tree_selection, history_search_events],
        };
        workspace.restore_tabs(window, cx);
        workspace.rebuild_tree(cx);
        // Reconnect the active restored tab's connection when possible.
        if let Some(connection_id) = workspace
            .tabs
            .active()
            .map(|tab| tab.connection_id.clone())
            .filter(|id| !id.is_empty())
        {
            workspace.connect_id(connection_id, cx);
        }
        workspace
    }

    fn active_connection(&self) -> Option<&SavedConnection> {
        self.active_id
            .as_ref()
            .and_then(|id| self.connections.iter().find(|item| &item.id == id))
    }

    /// Dialect for statement splitting / quoting in the SQL editor.
    pub(super) fn active_dialect(&self) -> sela_core::Dialect {
        self.session
            .as_ref()
            .map(|session| session.dialect())
            .or_else(|| self.active_connection().map(|c| c.engine.dialect()))
            .unwrap_or(sela_core::Dialect::Sqlite)
    }

    fn banner_error(&self) -> Option<String> {
        self.active_view()
            .and_then(|view| view.error.clone())
            .or_else(|| self.boot_error.clone())
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.entity();
        let connected = self.session.is_some();
        let can_next = self.can_go_next();
        let can_prev = self
            .active_view()
            .map(|view| view.page_offset > 0)
            .unwrap_or(false);
        let page_offset = self.active_view().map(|view| view.page_offset).unwrap_or(0);
        let page_len = self.active_view().map(|view| view.page_len).unwrap_or(0);
        let total_rows = self.active_view().and_then(|view| view.total_rows);
        let paged = self.active_view().map(|view| view.paged).unwrap_or(false);
        let page_number = page_offset / sela_core::PAGE_SIZE as u64 + 1;
        let page_label = match total_rows {
            Some(total) if paged => {
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
        let has_tabs = !self.tabs.is_empty();
        let table_pane = self
            .active_view()
            .map(|view| view.table_pane)
            .unwrap_or(TablePane::Data);
        let _ = page_len;

        div()
            .id("sela-root")
            .size_full()
            .flex()
            .flex_row()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_action({
                let this = this.clone();
                move |_: &RunQuery, window, cx| {
                    // Scope secondary-enter: filter fields Apply; otherwise Run in the editor.
                    this.update(cx, |workspace, cx| {
                        if workspace.filter_field_focused(window, cx) {
                            workspace.apply_filters(cx);
                        } else if workspace.editor_field_focused(window, cx) {
                            workspace.run_editor(cx);
                        }
                    });
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &RunAllSql, window, cx| {
                    this.update(cx, |workspace, cx| {
                        if workspace.editor_field_focused(window, cx) {
                            workspace.run_all_editor(cx);
                        }
                    });
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &FindInSql, window, cx| {
                    this.update(cx, |workspace, cx| {
                        if workspace.editor_field_focused(window, cx) {
                            workspace.toggle_sql_find(window, cx);
                        }
                    });
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &FindNext, window, cx| {
                    this.update(cx, |workspace, cx| {
                        if workspace.editor_field_focused(window, cx) {
                            workspace.refresh_find_matches(cx);
                            workspace.find_step(1, cx);
                        }
                    });
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &FindPrevious, window, cx| {
                    this.update(cx, |workspace, cx| {
                        if workspace.editor_field_focused(window, cx) {
                            workspace.refresh_find_matches(cx);
                            workspace.find_step(-1, cx);
                        }
                    });
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &ToggleEditor, window, cx| {
                    this.update(cx, |workspace, cx| {
                        if workspace.editor_field_focused(window, cx) {
                            workspace.toggle_editor_collapsed(cx);
                        }
                    });
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &CancelQuery, _, cx| {
                    this.update(cx, |workspace, cx| workspace.stop_query(cx));
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &NewQueryTab, window, cx| {
                    this.update(cx, |workspace, cx| workspace.new_query_tab(window, cx));
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &CloseTab, _, cx| {
                    this.update(cx, |workspace, cx| workspace.close_active_tab(cx));
                }
            })
            .on_action({
                let this = this.clone();
                move |action: &ActivateTab, _, cx| {
                    let number = action.0;
                    this.update(cx, |workspace, cx| {
                        workspace.activate_tab_number(number, cx);
                    });
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &ShowTableData, _, cx| {
                    this.update(cx, |workspace, cx| workspace.show_table_data(cx));
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &ShowTableStructure, _, cx| {
                    this.update(cx, |workspace, cx| workspace.show_table_structure(cx));
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &ToggleFilterBar, window, cx| {
                    this.update(cx, |workspace, cx| {
                        // SqlEditor also binds secondary-f to FindInSql; do not open the
                        // filter bar when the editor (or its find field) has focus.
                        if workspace.editor_field_focused(window, cx) {
                            return;
                        }
                        workspace.toggle_filter_bar(window, cx);
                    });
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &InsertFilterRow, window, cx| {
                    this.update(cx, |workspace, cx| {
                        workspace.insert_filter_row_after_focused(window, cx)
                    });
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &RemoveFilterRow, window, cx| {
                    this.update(cx, |workspace, cx| {
                        workspace.remove_focused_filter_row(window, cx)
                    });
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &EscapeFilterBar, window, cx| {
                    this.update(cx, |workspace, cx| workspace.escape_filter_bar(window, cx));
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &ToggleHistory, window, cx| {
                    this.update(cx, |workspace, cx| {
                        workspace.toggle_history_panel(window, cx)
                    });
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &FocusHistorySearch, window, cx| {
                    this.update(cx, |workspace, cx| {
                        workspace.focus_history_search(window, cx)
                    });
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &HistorySelectUp, _, cx| {
                    this.update(cx, |workspace, cx| workspace.history_move_selection(-1, cx));
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &HistorySelectDown, _, cx| {
                    this.update(cx, |workspace, cx| workspace.history_move_selection(1, cx));
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &HistoryOpen, window, cx| {
                    this.update(cx, |workspace, cx| {
                        workspace.history_open_in_tab(window, cx)
                    });
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &HistoryInsert, window, cx| {
                    this.update(cx, |workspace, cx| {
                        workspace.history_insert_into_editor(window, cx)
                    });
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &HistoryCopy, window, cx| {
                    this.update(cx, |workspace, cx| {
                        if workspace.history_search_focused(window, cx) {
                            return;
                        }
                        workspace.history_copy(cx);
                    });
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &HistoryRerun, window, cx| {
                    this.update(cx, |workspace, cx| workspace.history_rerun(window, cx));
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &HistoryDelete, _, cx| {
                    this.update(cx, |workspace, cx| workspace.history_delete_selected(cx));
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &EscapeHistory, window, cx| {
                    this.update(cx, |workspace, cx| {
                        workspace.escape_history_panel(window, cx)
                    });
                }
            })
            .child(self.sidebar(cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_w(px(0.))
                    .h_full()
                    .child(self.tab_bar(cx))
                    .when(has_tabs, |column| {
                        column
                            .child(self.toolbar(
                                cx,
                                &endpoint,
                                &page_label,
                                can_prev,
                                can_next,
                                connected,
                            ))
                            .when(matches!(table_pane, TablePane::Data), |column| {
                                column.child(self.filter_bar(cx, connected))
                            })
                            .when_some(self.banner_error(), |column, error| {
                                column.child(self.error_banner(cx, error))
                            })
                            .child(match table_pane {
                                TablePane::Data => self.results_area().into_any_element(),
                                TablePane::Structure => self.structure_area(cx).into_any_element(),
                            })
                            .when(matches!(table_pane, TablePane::Data), |column| {
                                column.child(self.predicate_strip(cx))
                            })
                            .child(self.result_strip(cx))
                            .child(self.sql_editor(window, cx))
                            .child(self.history_panel(window, cx))
                            .child(self.status_bar(cx))
                    })
                    .when(!has_tabs, |column| {
                        column
                            .when_some(self.boot_error.clone(), |column, error| {
                                column.child(self.error_banner(cx, error))
                            })
                            .child(self.empty_content(cx))
                    }),
            )
    }
}
