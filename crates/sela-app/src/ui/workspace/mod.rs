//! Main window: saved connections, schema tree, result grid, and the SQL editor.

mod content;
mod copy_selection;
mod edit_actions;
pub(crate) mod edit_session;
mod edit_ui;
mod filter_bar;
mod history;
mod query;
mod quit;
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
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, TitleBar};
use gpui_kit::prelude::*;
use gpui_kit::{div, px, Context, Entity, FocusHandle, IntoElement, Render, Subscription, Window};

use super::decorations::{should_show_client_title_bar, should_suppress_title_bar_window_menu};

use sela_config::{
    warn_if_mock_credential_store, CachingSecrets, ConfigStore, HistoryStore, KeyringStore,
    SavedConnection, SettingsStore, TabStore, ThemePreference,
};
use sela_core::{
    CopyFormat, DatabaseSession, HistoryEntry, HistoryOutcome, LoadedSchema, TabId, TabList,
};

use super::{
    AcceptCellEditor, ActivateTab, CancelCellEditor, CancelQuery, CloseTab, CommitChanges,
    CopyAsCsv, CopyAsInsert, CopyAsJson, CopyAsTsv, CopySelection, DecreaseFontSize, DeleteEditRow,
    DiscardChanges, DuplicateEditRow, EditCell, EscapeFilterBar, EscapeHistory,
    ExtendGridSelectionDown, ExtendGridSelectionLeft, ExtendGridSelectionRight,
    ExtendGridSelectionUp, FindInSql, FindNext, FindPrevious, FocusHistorySearch, HistoryCopy,
    HistoryDelete, HistoryInsert, HistoryOpen, HistoryRerun, HistorySelectDown, HistorySelectUp,
    IncreaseFontSize, InsertEditRow, InsertFilterRow, NewQueryTab, PreviewChanges, Quit,
    RemoveFilterRow, ResetFontSize, ResultStripNext, ResultStripPrev, RunAllSql, RunQuery,
    SelectAllGrid, SetCellNull, ShowTableData, ShowTableStructure, ToggleEditor, ToggleFilterBar,
    ToggleHistory,
};
use history::{HistoryFocusRestore, HistoryScope, DEFAULT_HISTORY_HEIGHT};
use tab_view::TabView;

/// What to do after the user confirms closing tabs that have typed SQL or a busy query.
#[derive(Clone)]
pub(super) enum PendingCloseTabs {
    Connect { to_id: String },
    Disconnect,
}

pub struct Workspace {
    store: ConfigStore,
    tab_store: TabStore,
    history_store: Option<Arc<HistoryStore>>,
    settings: SettingsStore,
    theme_preference: ThemePreference,
    font_size: u8,
    /// True when the status menu may offer System (window appearance available).
    system_theme_available: bool,
    result_strip_focus: FocusHandle,
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
    /// Shown in the status bar when a password is held in memory only this session.
    keychain_session_warning: Option<String>,
    last_selection: Option<String>,
    /// Connection id last chosen in the tree. Survives `rebuild_tree`, which
    /// clears `last_selection` so Edit / Delete still target the clicked row.
    selected_connection_id: Option<String>,
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
    /// True while the "Quit Sela?" confirm dialog is open (blocks repeat Quit).
    quit_confirm_open: bool,
    /// True while the Discard pending changes confirm is open.
    discard_confirm_open: bool,
    /// True while the close-tab-with-pending confirm is open.
    close_tab_confirm_open: bool,
    /// True while the Switch/Disconnect close-tabs confirm is open.
    close_tabs_confirm_open: bool,
    /// Action to run after the user confirms closing tabs with typed SQL / busy work.
    pending_close_tabs: Option<PendingCloseTabs>,
    /// Updated each frame from `window.window_decorations()` for the CSD title bar.
    client_decorations: bool,
    #[cfg(not(target_os = "macos"))]
    menu_bar: Entity<gpui_kit::component::menu::AppMenuBar>,
    _subscriptions: Vec<Subscription>,
}

#[derive(Clone)]
pub(super) enum GridSource {
    Idle,
    Table {
        schema: String,
        table: String,
        kind: sela_core::RelationKind,
    },
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
        let settings = SettingsStore::with_path(SettingsStore::default_path());
        let app_settings = settings.load();
        let theme_preference = app_settings.theme;
        let font_size = app_settings.font_size;
        let system_theme_available = super::theme::system_appearance_available(window);
        super::theme::install_sela_themes(theme_preference, font_size, Some(window), cx);
        // keyring v3 falls back to an in-process mock when no platform feature
        // is enabled; fail loudly so passwords are not silently non-persistent.
        warn_if_mock_credential_store();
        // Follow OS appearance only while the preference is System.
        let this = cx.weak_entity();
        let appearance = window.observe_window_appearance(move |window, cx| {
            let Some(entity) = this.upgrade() else {
                return;
            };
            let follow = entity.read(cx).theme_preference == ThemePreference::System;
            if follow {
                super::theme::apply_preference(ThemePreference::System, Some(window), cx);
                entity.update(cx, |_, cx| cx.notify());
            }
        });
        let mut workspace = Self {
            store,
            tab_store,
            history_store,
            settings,
            theme_preference,
            font_size,
            system_theme_available,
            result_strip_focus: cx.focus_handle(),
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
            keychain_session_warning: None,
            last_selection: None,
            selected_connection_id: None,
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
            quit_confirm_open: false,
            discard_confirm_open: false,
            close_tab_confirm_open: false,
            close_tabs_confirm_open: false,
            pending_close_tabs: None,
            client_decorations: false,
            #[cfg(not(target_os = "macos"))]
            menu_bar: gpui_kit::component::menu::AppMenuBar::new(cx),
            _subscriptions: vec![
                tree_events,
                tree_selection,
                history_search_events,
                appearance,
                Self::subscribe_app_quit(cx),
            ],
        };
        // Linux/Windows: OS close / Alt+F4 go through the same quit path.
        // macOS: closing the window is allowed; the app stays running until Quit
        // (GPUI QuitMode::Default → Explicit on macOS).
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        {
            let entity = cx.entity();
            window.on_window_should_close(cx, move |window, cx| {
                entity.update(cx, |workspace, cx| workspace.request_quit(window, cx));
                false
            });
        }
        workspace.restore_tabs(window, cx);
        workspace.rebuild_tree(cx);
        // Reconnect the active restored tab's connection when possible.
        if let Some(connection_id) = workspace
            .tabs
            .active()
            .map(|tab| tab.connection_id.clone())
            .filter(|id| !id.is_empty())
        {
            workspace.connect_id(connection_id, None, cx);
        }
        workspace
    }

    fn active_connection(&self) -> Option<&SavedConnection> {
        self.active_id
            .as_ref()
            .and_then(|id| self.connections.iter().find(|item| &item.id == id))
    }

    /// Connection the sidebar Edit / Delete buttons should target: the tree
    /// selection when it names a connection (or a child under one), else the
    /// active session connection.
    fn selected_or_active_connection(&self) -> Option<&SavedConnection> {
        self.selected_connection_id
            .as_ref()
            .and_then(|id| self.connections.iter().find(|item| &item.id == id))
            .or_else(|| self.active_connection())
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
        state::resolve_banner_error(
            self.boot_error.as_deref(),
            self.active_view().and_then(|view| view.error.as_deref()),
        )
    }

    /// Linux/Windows in-window File menu under server-side decorations.
    /// Height is fixed because `AppMenuBar` paints `size_full()`; an unbounded
    /// slot swallows the column. When client-side decorations are active, the
    /// menu lives inside [`Self::client_title_bar`] instead.
    #[cfg(not(target_os = "macos"))]
    fn menu_bar_row(&self, cx: &Context<Self>) -> impl IntoElement {
        div()
            .id("sela-menu-bar")
            .w_full()
            .h(px(32.))
            .flex_shrink_0()
            .border_b_1()
            .border_color(cx.theme().title_bar_border)
            .bg(cx.theme().title_bar)
            .child(self.menu_bar.clone())
    }

    /// Client-side title bar (GNOME Wayland and other CSD sessions): app title,
    /// window controls, drag-to-move, and double-click maximize/restore via
    /// gpui-component `TitleBar`. Menu items sit in the bar so they keep working
    /// without starting a drag (`TitleBar` honors `prevent_default` on controls).
    ///
    /// A capture-phase right mouse down stops the kit's Linux CSD overlay that
    /// would call `show_window_menu` (product: no title-bar right-click menu).
    fn client_title_bar(&self, cx: &Context<Self>) -> impl IntoElement {
        let this = cx.entity();
        div()
            .id("sela-csd-title-wrap")
            .w_full()
            .capture_any_mouse_down(|event, window, cx| {
                if should_suppress_title_bar_window_menu(event.button) {
                    window.prevent_default();
                    cx.stop_propagation();
                }
            })
            .child(
                TitleBar::new()
                    .on_close_window(move |_, window, cx| {
                        this.update(cx, |workspace, cx| {
                            workspace.request_quit(window, cx);
                        });
                    })
                    .child(
                        h_flex()
                            .id("sela-csd-title")
                            .items_center()
                            .gap_2()
                            .h_full()
                            .flex_1()
                            .min_w(px(0.))
                            .map(|row| {
                                #[cfg(not(target_os = "macos"))]
                                {
                                    row.child(self.menu_bar.clone())
                                }
                                #[cfg(target_os = "macos")]
                                {
                                    row
                                }
                            })
                            .child(div().id("sela-csd-app-title").child("Sela")),
                    ),
            )
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
        let page_label = format_page_label(page_number, page_len, total_rows, paged);
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
        self.client_decorations = should_show_client_title_bar(window.window_decorations());
        let client_title_bar = self.client_decorations;

        div()
            .id("sela-root")
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_action({
                let this = this.clone();
                move |_: &Quit, window, cx| {
                    this.update(cx, |workspace, cx| workspace.request_quit(window, cx));
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &IncreaseFontSize, window, cx| {
                    this.update(cx, |workspace, cx| workspace.bump_font_size(1, window, cx));
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &DecreaseFontSize, window, cx| {
                    this.update(cx, |workspace, cx| workspace.bump_font_size(-1, window, cx));
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &ResetFontSize, window, cx| {
                    this.update(cx, |workspace, cx| workspace.reset_font_size(window, cx));
                }
            })
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
                move |_: &CloseTab, window, cx| {
                    this.update(cx, |workspace, cx| workspace.close_active_tab(window, cx));
                }
            })
            .on_action({
                let this = this.clone();
                move |action: &ActivateTab, window, cx| {
                    let number = action.0;
                    this.update(cx, |workspace, cx| {
                        workspace.activate_tab_number(number, Some(window), cx);
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
            .on_action({
                let this = this.clone();
                move |_: &CopySelection, _, cx| {
                    this.update(cx, |workspace, cx| {
                        workspace.copy_selection(CopyFormat::Tsv, cx)
                    });
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &SelectAllGrid, _, cx| {
                    this.update(cx, |workspace, cx| workspace.select_all_grid(cx));
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &ExtendGridSelectionUp, _, cx| {
                    this.update(cx, |workspace, cx| {
                        workspace.extend_grid_selection(-1, 0, cx)
                    });
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &ExtendGridSelectionDown, _, cx| {
                    this.update(cx, |workspace, cx| {
                        workspace.extend_grid_selection(1, 0, cx)
                    });
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &ExtendGridSelectionLeft, _, cx| {
                    this.update(cx, |workspace, cx| {
                        workspace.extend_grid_selection(0, -1, cx)
                    });
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &ExtendGridSelectionRight, _, cx| {
                    this.update(cx, |workspace, cx| {
                        workspace.extend_grid_selection(0, 1, cx)
                    });
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &CopyAsTsv, _, cx| {
                    this.update(cx, |workspace, cx| {
                        workspace.copy_selection(CopyFormat::Tsv, cx)
                    });
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &CopyAsCsv, _, cx| {
                    this.update(cx, |workspace, cx| {
                        workspace.copy_selection(CopyFormat::Csv, cx)
                    });
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &CopyAsJson, _, cx| {
                    this.update(cx, |workspace, cx| {
                        workspace.copy_selection(CopyFormat::Json, cx)
                    });
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &CopyAsInsert, _, cx| {
                    this.update(cx, |workspace, cx| {
                        workspace.copy_selection(CopyFormat::Insert, cx)
                    });
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &ResultStripPrev, _, cx| {
                    this.update(cx, |workspace, cx| workspace.step_result_strip(-1, cx));
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &ResultStripNext, _, cx| {
                    this.update(cx, |workspace, cx| workspace.step_result_strip(1, cx));
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &EditCell, window, cx| {
                    this.update(cx, |workspace, cx| workspace.begin_cell_edit(window, cx));
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &CancelCellEditor, _, cx| {
                    this.update(cx, |workspace, cx| workspace.cancel_cell_editor(cx));
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &AcceptCellEditor, _, cx| {
                    this.update(cx, |workspace, cx| {
                        if let Err(msg) = workspace.accept_cell_editor(cx) {
                            if let Some(view) = workspace.active_view_mut() {
                                view.status = msg;
                            }
                            cx.notify();
                        }
                    });
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &SetCellNull, window, cx| {
                    this.update(cx, |workspace, cx| {
                        workspace.set_editor_null_and_accept(window, cx);
                    });
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &InsertEditRow, _, cx| {
                    this.update(cx, |workspace, cx| workspace.insert_edit_row(cx));
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &DuplicateEditRow, _, cx| {
                    this.update(cx, |workspace, cx| workspace.duplicate_edit_row(cx));
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &DeleteEditRow, _, cx| {
                    this.update(cx, |workspace, cx| workspace.delete_edit_row(cx));
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &PreviewChanges, _, cx| {
                    this.update(cx, |workspace, cx| workspace.toggle_preview_sheet(cx));
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &CommitChanges, window, cx| {
                    this.update(cx, |workspace, cx| {
                        workspace.request_commit_changes(window, cx);
                    });
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &DiscardChanges, window, cx| {
                    this.update(cx, |workspace, cx| {
                        workspace.request_discard_changes(window, cx);
                    });
                }
            })
            .map(|root| {
                if client_title_bar {
                    root.child(self.client_title_bar(cx))
                } else {
                    #[cfg(not(target_os = "macos"))]
                    {
                        root.child(self.menu_bar_row(cx))
                    }
                    #[cfg(target_os = "macos")]
                    {
                        root
                    }
                }
            })
            .child(
                div()
                    .id("sela-body")
                    .flex_1()
                    .min_h(px(0.))
                    .flex()
                    .flex_row()
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
                                    .when(matches!(table_pane, TablePane::Data), |column| {
                                        column.child(self.pending_bar(cx))
                                    })
                                    .when(matches!(table_pane, TablePane::Data), |column| {
                                        column.child(self.read_only_strip(cx))
                                    })
                                    .when_some(self.banner_error(), |column, error| {
                                        column.child(self.error_banner(cx, error))
                                    })
                                    .child(match table_pane {
                                        TablePane::Data => self.results_area(cx).into_any_element(),
                                        TablePane::Structure => {
                                            self.structure_area(cx).into_any_element()
                                        }
                                    })
                                    .when(matches!(table_pane, TablePane::Data), |column| {
                                        column.child(self.preview_sheet(cx))
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
                    ),
            )
    }
}

/// Toolbar page label for table previews and SQL results (#61).
///
/// - Paged table with a known total → `Page N of M`
/// - Non-paged / SQL result with a known total → `Page N · {total} rows`
/// - Unknown total → `Page N · {page_len} rows` (actual rows on this page)
pub(crate) fn format_page_label(
    page_number: u64,
    page_len: usize,
    total_rows: Option<u64>,
    paged: bool,
) -> String {
    match (total_rows, paged) {
        (Some(total), true) => {
            let pages = total.div_ceil(sela_core::PAGE_SIZE as u64).max(1);
            format!("Page {page_number} of {pages}")
        }
        (Some(total), false) => format!("Page {page_number} · {total} rows"),
        (None, _) => format!("Page {page_number} · {page_len} rows"),
    }
}

#[cfg(test)]
mod page_label_tests {
    use super::format_page_label;

    #[test]
    fn paged_table_shows_page_of_pages() {
        assert_eq!(format_page_label(1, 100, Some(2500), true), "Page 1 of 25");
        assert_eq!(format_page_label(2, 100, Some(150), true), "Page 2 of 2");
    }

    #[test]
    fn sql_result_shows_actual_row_count() {
        assert_eq!(format_page_label(1, 5, Some(5), false), "Page 1 · 5 rows");
        assert_eq!(
            format_page_label(1, 10, Some(10), false),
            "Page 1 · 10 rows"
        );
        assert_eq!(format_page_label(1, 1, Some(1), false), "Page 1 · 1 rows");
        assert_eq!(format_page_label(1, 0, Some(0), false), "Page 1 · 0 rows");
    }

    #[test]
    fn unknown_total_uses_page_len_not_hardcoded_100() {
        assert_eq!(format_page_label(1, 37, None, true), "Page 1 · 37 rows");
        assert_eq!(format_page_label(1, 0, None, false), "Page 1 · 0 rows");
    }

    #[test]
    fn multi_page_sql_cap_still_shows_delivered_count() {
        // Streamed SQL capped at 1000 still reports the delivered total.
        assert_eq!(
            format_page_label(1, 1000, Some(1000), false),
            "Page 1 · 1000 rows"
        );
    }
}
