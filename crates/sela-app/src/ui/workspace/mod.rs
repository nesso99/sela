//! Main window: saved connections, schema tree, result grid, and the SQL editor.

mod content;
mod copy_selection;
mod edit_actions;
pub(crate) mod edit_session;
pub(crate) mod edit_ui;
pub(crate) mod export_import;
mod filter_bar;
mod history;
pub(crate) mod palette;
mod query;
mod quit;
mod result_strip;
mod safe_mode;
mod sidebar;
mod sql_editor;
mod sql_find;
mod sql_gutter;
mod sql_run;
mod state;
mod status;
mod structure;
pub(crate) mod tab_view;
pub(crate) mod tabs;
mod tree;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
#[cfg(test)]
use std::time::Instant;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::InputState;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::tree::{TreeEvent, TreeState};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Sizable as _, TitleBar};
use gpui_kit::prelude::*;
#[cfg(test)]
use gpui_kit::Focusable as _;
use gpui_kit::{
    div, px, App, Context, Entity, FocusHandle, IntoElement, Render, Subscription, Window,
};

use super::decorations::{should_show_client_title_bar, should_suppress_title_bar_window_menu};

#[cfg(test)]
use sela_config::MemorySecretStore;
use sela_config::{
    warn_if_mock_credential_store, CachingSecrets, ConfigStore, HistoryStore, KeyringStore,
    SavedConnection, SecretStore, SettingsStore, TabStore, ThemePreference,
};
use sela_core::{
    CopyFormat, DatabaseSession, HistoryEntry, HistoryOutcome, LoadedSchema, TabId, TabList,
};

use super::{
    AcceptCellEditor, ActivateTab, CancelCellEditor, CancelQuery, CloseTab, CommitChanges,
    CopyAsCsv, CopyAsInsert, CopyAsJson, CopyAsTsv, CopySelection, DecreaseFontSize, DeleteEditRow,
    DiscardChanges, DuplicateEditRow, EditCell, EscapeFilterBar, EscapeHistory, EscapePalette,
    ExportData, ExtendGridSelectionDown, ExtendGridSelectionLeft, ExtendGridSelectionRight,
    ExtendGridSelectionUp, FindInSql, FindNext, FindPrevious, FocusHistorySearch, HistoryCopy,
    HistoryDelete, HistoryInsert, HistoryOpen, HistoryRerun, HistorySelectDown, HistorySelectUp,
    ImportCsv, IncreaseFontSize, InsertEditRow, InsertFilterRow, InspectCell, NewQueryTab,
    PaletteConfirm, PaletteSelectDown, PaletteSelectUp, PreviewChanges, Quit, RemoveFilterRow,
    ResetFontSize, ResultStripNext, ResultStripPrev, RunAllSql, RunQuery, SelectAllGrid,
    SetCellNull, ShowTableData, ShowTableStructure, ToggleCommandPalette, ToggleEditor,
    ToggleFilterBar, ToggleHistory, ToggleQuickOpen,
};
use export_import::{ExportDialog, ImportDialog, TransferJob};
use history::{HistoryFocusRestore, HistoryScope, DEFAULT_HISTORY_HEIGHT};
use tab_view::TabView;

/// What to do after the user confirms closing tabs that have typed SQL or a busy query.
#[derive(Clone)]
pub(super) enum PendingCloseTabs {
    Connect { to_id: String },
    Disconnect,
}

/// Where a first-seen SSH host key challenge came from (SSH-18).
pub(super) enum HostKeyOrigin {
    /// Test connection: reject/accept must only touch the form Test line.
    Test {
        form: Entity<super::form::ConnectionForm>,
        generation: u64,
        via_ssh: bool,
    },
    /// Save & connect / sidebar connect: reject fails the connect; accept finishes it.
    Connect {
        saved_id: String,
        saved_name: String,
        via_ssh: bool,
    },
}

impl HostKeyOrigin {
    pub(super) fn is_test(&self) -> bool {
        matches!(self, Self::Test { .. })
    }
}

/// First-seen SSH host key waiting for Accept & save / Cancel (SSH-18).
pub(super) struct PendingUnknownHostKey {
    pub(super) challenge: Box<sela_drivers::UnknownSshHostKeyChallenge>,
    pub(super) origin: HostKeyOrigin,
}

impl std::fmt::Debug for PendingUnknownHostKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingUnknownHostKey")
            .field("origin_is_test", &self.origin.is_test())
            .field("fingerprint", &self.challenge.fingerprint)
            .finish_non_exhaustive()
    }
}

/// Accept or reject a first-seen host key (SSH-18).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HostKeyDecision {
    Accept,
    Reject,
}

/// Where to route Accept / Reject for a first-seen host key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HostKeyRoute {
    /// Update the form Test line only; leave the live session alone.
    TestReject,
    /// Accept the key, then update the form Test line.
    TestAccept,
    /// Fail the connect attempt (do not touch an unrelated live session via Test).
    ConnectReject,
    /// Accept the key and finish connecting the workspace session.
    ConnectAccept,
}

/// Pure routing for SSH-18 Accept / Reject × Test / Connect.
pub(super) fn route_unknown_host_key(is_test: bool, decision: HostKeyDecision) -> HostKeyRoute {
    match (is_test, decision) {
        (true, HostKeyDecision::Reject) => HostKeyRoute::TestReject,
        (true, HostKeyDecision::Accept) => HostKeyRoute::TestAccept,
        (false, HostKeyDecision::Reject) => HostKeyRoute::ConnectReject,
        (false, HostKeyDecision::Accept) => HostKeyRoute::ConnectAccept,
    }
}

/// Whether a live SSH-tunneled session should be treated as dropped (SSH-11).
///
/// Only transport-class failures ([`DbError::Connection`], [`DbError::Ssh`]) count.
/// Query typos and auth failures must not disconnect the user.
pub(super) fn is_tunnel_drop(err: &sela_core::DbError, via_ssh: bool) -> bool {
    if !via_ssh {
        return false;
    }
    matches!(
        err,
        sela_core::DbError::Connection { .. } | sela_core::DbError::Ssh { .. }
    )
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
    secrets: CachingSecrets<Box<dyn SecretStore>>,
    connections: Vec<SavedConnection>,
    active_id: Option<String>,
    session: Option<Arc<dyn DatabaseSession>>,
    /// True when the live session was opened through an SSH tunnel (SSH-11 banner).
    session_via_ssh: bool,
    catalog: Vec<LoadedSchema>,
    expanded: HashSet<String>,
    tree: Entity<TreeState>,
    tabs: TabList,
    views: HashMap<TabId, TabView>,
    /// Workspace-level load error (e.g. connections.json), shown when no tab error.
    boot_error: Option<String>,
    /// Transient status when no tab is open (e.g. Duplicate with an empty pane).
    workspace_status: Option<String>,
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
    /// True while a safe-mode confirm dialog is open (blocks stacked confirms).
    safe_mode_confirm_open: bool,
    /// True while the Discard pending changes confirm is open.
    discard_confirm_open: bool,
    /// True while the close-tab-with-pending confirm is open.
    close_tab_confirm_open: bool,
    /// True while the Switch/Disconnect close-tabs confirm is open.
    close_tabs_confirm_open: bool,
    /// Action to run after the user confirms closing tabs with typed SQL / busy work.
    pending_close_tabs: Option<PendingCloseTabs>,
    /// True while an import Confirm / discard-pending dialog is open.
    import_confirm_open: bool,
    /// Active export / import background job (one per window; EX-13).
    transfer_job: Option<TransferJob>,
    transfer_generation: u64,
    /// Test-only: when set, the next `run_export` parks until cancel (EX-06 Stop).
    #[cfg(test)]
    test_export_hold: Option<std::sync::Arc<export_import::jobs::ExportHold>>,
    /// Open export dialog entity (tests may inject a path).
    export_dialog: Option<Entity<ExportDialog>>,
    /// Open import dialog entity (tests may inject a path).
    import_dialog: Option<Entity<ImportDialog>>,
    /// SSH-18 first-seen host key waiting for a Window to open the confirm.
    pending_unknown_host_key: Option<PendingUnknownHostKey>,
    /// SSH-07 changed host key body waiting for a Window to open the alert.
    pending_host_key_mismatch: Option<String>,
    /// Connection form currently open (for Test result updates).
    connection_form: Option<Entity<super::form::ConnectionForm>>,
    /// Generation of the in-flight Test (stale completions ignored).
    connection_test_generation: u64,
    /// Updated each frame from `window.window_decorations()` for the CSD title bar.
    client_decorations: bool,
    /// Command palette / quick open (M3-4). Created lazily on first open.
    palette: Option<palette::PaletteState>,
    _subscriptions: Vec<Subscription>,
}

#[derive(Clone)]
pub(crate) enum GridSource {
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

/// Paths and secret store for a Workspace (production defaults or temp test dirs).
pub(crate) struct WorkspacePaths {
    pub connections: std::path::PathBuf,
    pub tabs: std::path::PathBuf,
    pub settings: std::path::PathBuf,
    pub history: std::path::PathBuf,
}

impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        warn_if_mock_credential_store();
        Self::new_with(
            WorkspacePaths {
                connections: ConfigStore::default_path(),
                tabs: TabStore::default_path(),
                settings: SettingsStore::default_path(),
                history: HistoryStore::default_path(),
            },
            Box::new(KeyringStore),
            window,
            cx,
        )
    }

    /// Headless UI tests: temp config dirs + in-memory secrets (no OS keychain).
    #[cfg(test)]
    pub(crate) fn new_for_tests(
        dir: &std::path::Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_with(
            WorkspacePaths {
                connections: dir.join("connections.json"),
                tabs: dir.join("tabs.json"),
                settings: dir.join("settings.json"),
                history: dir.join("history.sqlite"),
            },
            Box::new(MemorySecretStore::default()),
            window,
            cx,
        )
    }

    fn new_with(
        paths: WorkspacePaths,
        secrets: Box<dyn SecretStore>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let store = ConfigStore::with_path(paths.connections);
        let tab_store = TabStore::with_path(paths.tabs);
        let (connections, boot_error) = match store.load() {
            Ok(connections) => (connections, None),
            Err(err) => (Vec::new(), Some(err.to_string())),
        };
        let (history_store, history_reset_notice, history_panel_error) =
            match HistoryStore::open(&paths.history) {
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
        let settings = SettingsStore::with_path(paths.settings);
        let app_settings = settings.load();
        let theme_preference = app_settings.theme;
        let font_size = app_settings.font_size;
        let system_theme_available = super::theme::system_appearance_available(window);
        super::theme::install_sela_themes(theme_preference, font_size, Some(window), cx);
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
            secrets: CachingSecrets::new(secrets),
            connections,
            active_id: None,
            session: None,
            session_via_ssh: false,
            catalog: Vec::new(),
            expanded: HashSet::new(),
            tree,
            tabs: TabList::new(),
            views: HashMap::new(),
            boot_error,
            workspace_status: None,
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
            safe_mode_confirm_open: false,
            discard_confirm_open: false,
            close_tab_confirm_open: false,
            close_tabs_confirm_open: false,
            pending_close_tabs: None,
            import_confirm_open: false,
            transfer_job: None,
            transfer_generation: 0,
            #[cfg(test)]
            test_export_hold: None,
            export_dialog: None,
            import_dialog: None,
            pending_unknown_host_key: None,
            pending_host_key_mismatch: None,
            connection_form: None,
            connection_test_generation: 0,
            client_decorations: false,
            palette: None,
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

    #[cfg(test)]
    pub(crate) fn test_active_safe_mode(&self) -> sela_core::SafeMode {
        self.active_safe_mode()
    }

    #[cfg(test)]
    pub(crate) fn test_active_id(&self) -> Option<&str> {
        self.active_id.as_deref()
    }

    #[cfg(test)]
    pub(crate) fn test_is_connected(&self) -> bool {
        self.session.is_some()
    }

    #[cfg(test)]
    pub(crate) fn test_catalog_table_count(&self) -> usize {
        self.catalog.iter().map(|s| s.tables.len()).sum()
    }

    #[cfg(test)]
    pub(crate) fn test_tab_count(&self) -> usize {
        self.tabs.len()
    }

    #[cfg(test)]
    pub(crate) fn test_active_table(&self) -> Option<(String, String)> {
        match &self.active_view()?.source {
            GridSource::Table { schema, table, .. } => Some((schema.clone(), table.clone())),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn test_pending_dirty(&self) -> bool {
        self.active_view()
            .and_then(|v| v.edit.as_ref())
            .is_some_and(|e| e.is_dirty())
    }

    #[cfg(test)]
    pub(crate) fn test_theme_preference(&self) -> ThemePreference {
        self.theme_preference
    }

    #[cfg(test)]
    pub(crate) fn test_new_query_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.new_query_tab(window, cx);
    }

    #[cfg(test)]
    pub(crate) fn test_focus_sql_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(view) = self.active_view() else {
            return;
        };
        view.sql.read(cx).focus_handle(cx).focus(window, cx);
    }

    #[cfg(test)]
    pub(crate) fn test_focus_grid(
        &mut self,
        row: usize,
        col: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(view) = self.active_view() else {
            return;
        };
        view.grid.update(cx, |state, cx| {
            state.set_selected_cell(row, col, cx);
        });
        view.grid.read(cx).focus_handle(cx).focus(window, cx);
    }

    #[cfg(test)]
    pub(crate) fn test_focus_filter_bar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Open the filter bar so a text input in the grid scope has focus.
        // toggle_filter_bar focuses the first column field when opening.
        if self.active_view().is_some_and(|v| v.filter_bar_open) {
            self.toggle_filter_bar(window, cx); // close
        }
        self.toggle_filter_bar(window, cx); // open + focus
    }

    /// Same path as the PreviewChanges `on_action` handler (for guard coverage).
    #[cfg(test)]
    pub(crate) fn test_handle_preview_changes(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.handle_preview_changes(window, cx);
    }

    #[cfg(test)]
    pub(crate) fn test_editor_focused(&self, window: &Window, cx: &App) -> bool {
        self.editor_field_focused(window, cx)
    }

    #[cfg(test)]
    pub(crate) fn test_text_input_focused(&self, window: &Window, cx: &App) -> bool {
        self.text_input_focused(window, cx)
    }

    #[cfg(test)]
    pub(crate) fn test_connection_dialog_open(&self) -> bool {
        self.connection_form.is_some()
    }

    #[cfg(test)]
    pub(crate) fn test_preview_open(&self) -> bool {
        self.active_view()
            .and_then(|v| v.edit.as_ref())
            .is_some_and(|e| e.preview_open)
    }

    #[cfg(test)]
    pub(crate) fn test_safe_toolbar_segment(&self) -> Option<String> {
        self.active_connection()
            .map(|c| c.safe_mode.toolbar_segment())
    }

    /// Set the palette search text and run the same sync path as InputEvent::Change.
    #[cfg(test)]
    pub(crate) fn test_set_palette_query(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(palette) = self.palette.as_ref() else {
            return;
        };
        let query = palette.query.clone();
        query.update(cx, |input, cx| {
            input.set_value(text, window, cx);
        });
        self.sync_palette_query(window, cx);
    }

    /// Run the InputEvent::Change sync path after headless `window.input`.
    #[cfg(test)]
    pub(crate) fn test_sync_palette_query(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_palette_query(window, cx);
    }

    #[cfg(test)]
    pub(crate) fn test_seed_sqlite_connection(
        &mut self,
        name: &str,
        file_path: &std::path::Path,
        cx: &mut Context<Self>,
    ) -> String {
        let mut connection = SavedConnection::new(sela_core::Engine::Sqlite);
        connection.name = name.into();
        connection.file_path = file_path.display().to_string();
        connection.host.clear();
        let id = connection.id.clone();
        self.connections.push(connection);
        let _ = self.store.save(&self.connections);
        self.selected_connection_id = Some(id.clone());
        self.rebuild_tree(cx);
        id
    }

    /// Retarget the active saved connection as Postgres + SSH (closed bastion port)
    /// while leaving the live session in place so palette actions stay enabled.
    /// Reconnect then exercises `proceed_connect_id` → SSH `ConnectOutcome` (#122).
    #[cfg(test)]
    pub(crate) fn test_retarget_active_as_postgres_ssh(&mut self) -> Option<String> {
        use sela_core::{Engine, SshAuthMethod, SshSettings};
        let id = self.active_id.clone()?;
        let conn = self.connections.iter_mut().find(|c| c.id == id)?;
        conn.engine = Engine::Postgres;
        conn.host = "127.0.0.1".into();
        conn.port = 5432;
        conn.username = "sela".into();
        conn.database = "sela".into();
        conn.file_path.clear();
        conn.ssh = SshSettings {
            enabled: true,
            host: "127.0.0.1".into(),
            port: 1,
            user: "tunnel".into(),
            auth: SshAuthMethod::Password,
            private_key_path: String::new(),
        };
        let _ = self.store.save(&self.connections);
        let _ = self
            .secrets
            .set(&sela_config::ssh_password_account(&id), "tunnel-secret");
        Some(id)
    }

    #[cfg(test)]
    pub(crate) fn test_view_error(&self) -> Option<String> {
        self.active_view()
            .and_then(|v| v.error.clone())
            .or_else(|| self.boot_error.clone())
    }

    #[cfg(test)]
    pub(crate) fn test_view_status(&self) -> Option<String> {
        self.active_view()
            .map(|v| v.status.clone())
            .filter(|s| !s.is_empty())
            .or_else(|| self.workspace_status.clone())
    }

    /// Mark the live session as SSH-tunneled so SSH-11 drop handling applies.
    #[cfg(test)]
    pub(crate) fn test_mark_session_via_ssh(&mut self) {
        assert!(
            self.session.is_some(),
            "test_mark_session_via_ssh requires a live session"
        );
        self.session_via_ssh = true;
    }

    /// SSH-11: surface a tunneled connection drop (banner + status + drop session).
    #[cfg(test)]
    pub(crate) fn test_simulate_ssh_tunnel_drop(&mut self, cx: &mut Context<Self>) {
        let err = sela_core::DbError::connection("connection reset by peer");
        self.note_possible_ssh_tunnel_drop(&err, cx);
    }

    #[cfg(test)]
    pub(crate) fn test_open_form(
        &mut self,
        existing: Option<SavedConnection>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_form(existing, window, cx);
    }

    #[cfg(test)]
    pub(crate) fn test_connection_form(&self) -> Option<Entity<super::form::ConnectionForm>> {
        self.connection_form.clone()
    }

    /// Snapshot of a secret account (for SSH-16 no-persist assertions). Never log it.
    #[cfg(test)]
    pub(crate) fn test_secret_present(&self, account: &str) -> bool {
        self.secrets
            .get_optional(account)
            .ok()
            .flatten()
            .is_some_and(|v| !v.is_empty())
    }

    #[cfg(test)]
    pub(crate) fn test_set_secret(&self, account: &str, secret: &str) {
        let _ = self.secrets.set(account, secret);
    }

    #[cfg(test)]
    pub(crate) fn test_connections_path(&self) -> std::path::PathBuf {
        self.store.path().to_path_buf()
    }

    #[cfg(test)]
    pub(crate) fn test_connect(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.connect_id(id, Some(window), cx);
    }

    #[cfg(test)]
    pub(crate) fn test_inject_pending_edit(&mut self, cx: &mut Context<Self>) {
        use sela_core::{ColumnStructure, Dialect, IndexInfo, RelationKind, TableStructure, Value};
        let Some(view) = self.active_view_mut() else {
            return;
        };
        let structure = TableStructure {
            schema: "main".into(),
            table: "books".into(),
            columns: vec![
                ColumnStructure {
                    name: "id".into(),
                    type_name: "INTEGER".into(),
                    nullable: false,
                    default: None,
                    comment: None,
                    generated: false,
                },
                ColumnStructure {
                    name: "title".into(),
                    type_name: "TEXT".into(),
                    nullable: true,
                    default: None,
                    comment: None,
                    generated: false,
                },
            ],
            indexes: vec![IndexInfo {
                name: "books_pkey".into(),
                columns: vec!["id".into()],
                unique: true,
                primary: true,
                definition: None,
            }],
            foreign_keys: vec![],
            ddl: String::new(),
        };
        let mut edit = edit_session::EditSession::from_structure(
            &structure,
            Dialect::Sqlite,
            RelationKind::Table,
        );
        let pk = vec![Value::Int(1)];
        let _ = edit.changeset.as_mut().unwrap().set_cell(
            &pk,
            "title",
            Value::text("a"),
            Value::text("b"),
        );
        view.edit = Some(edit);
        view.source = GridSource::Table {
            schema: "main".into(),
            table: "books".into(),
            kind: RelationKind::Table,
        };
        view.table_pane = TablePane::Data;
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn test_connection_safe_mode(&self, id: &str) -> Option<sela_core::SafeMode> {
        self.connections
            .iter()
            .find(|c| c.id == id)
            .map(|c| c.safe_mode)
    }

    #[cfg(test)]
    pub(crate) fn test_persisted_safe_mode(&self, id: &str) -> Option<sela_core::SafeMode> {
        self.store
            .load()
            .ok()?
            .into_iter()
            .find(|c| c.id == id)
            .map(|c| c.safe_mode)
    }

    #[cfg(test)]
    pub(crate) fn test_open_table(
        &mut self,
        schema: &str,
        table: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_table_tab(schema.to_string(), table.to_string(), window, cx);
    }

    #[cfg(test)]
    pub(crate) fn test_has_transfer_job(&self) -> bool {
        self.transfer_job.is_some()
    }

    #[cfg(test)]
    pub(crate) fn test_transfer_generation(&self) -> u64 {
        self.transfer_generation
    }

    #[cfg(test)]
    pub(crate) fn test_transfer_job_generation(&self) -> Option<u64> {
        self.transfer_job.as_ref().map(|j| j.generation)
    }

    #[cfg(test)]
    pub(crate) fn test_active_status(&self) -> Option<String> {
        self.active_view().map(|v| v.status.clone())
    }

    /// Arm the test-only export hold so `run_export` parks until cancel.
    #[cfg(test)]
    pub(crate) fn test_arm_export_hold(&mut self) {
        self.test_export_hold = Some(std::sync::Arc::new(
            export_import::jobs::ExportHold::default(),
        ));
    }

    #[cfg(test)]
    pub(crate) fn test_clear_export_hold(&mut self) {
        self.test_export_hold = None;
    }

    #[cfg(test)]
    pub(crate) fn test_export_hold_reached(&self) -> bool {
        self.test_export_hold.as_ref().is_some_and(|h| h.reached())
    }

    /// Start a real export job (exercises `transfer_generation` bump + cancel token).
    #[cfg(test)]
    pub(crate) fn test_start_export(&mut self, path: std::path::PathBuf, cx: &mut Context<Self>) {
        use export_import::export_dialog::{ExportRequest, ExportScope};
        use sela_core::{CsvExportOptions, ExportFormat};
        self.start_export(
            ExportRequest {
                format: ExportFormat::Csv(CsvExportOptions::default()),
                scope: ExportScope::WholeTable,
                path,
            },
            cx,
        );
    }

    /// Start a real import job against `path` (exercises the import generation bump).
    #[cfg(test)]
    pub(crate) fn test_start_import(
        &mut self,
        path: std::path::PathBuf,
        schema: &str,
        table: &str,
        cx: &mut Context<Self>,
    ) {
        use export_import::import_dialog::ImportRequest;
        use sela_core::CsvImportOptions;
        self.start_import(
            ImportRequest {
                path,
                options: CsvImportOptions {
                    has_header: true,
                    ..CsvImportOptions::default()
                },
                total_rows: 1,
                schema: schema.into(),
                table: table.into(),
            },
            None,
            cx,
        );
    }

    /// Advance the job slot's generation so a later `finish_*` with the old
    /// generation is treated as stale (covers the completion gate).
    #[cfg(test)]
    pub(crate) fn test_supersede_transfer_generation(&mut self) {
        if let Some(job) = self.transfer_job.as_mut() {
            job.generation = job.generation.wrapping_add(1);
            self.transfer_generation = job.generation;
        } else {
            self.transfer_generation = self.transfer_generation.wrapping_add(1);
        }
    }

    #[cfg(test)]
    pub(crate) fn test_finish_export(
        &mut self,
        generation: u64,
        dest: std::path::PathBuf,
        result: Result<u64, sela_core::ExportError>,
        cx: &mut Context<Self>,
    ) {
        use export_import::logic::export_temp_path;
        let tab_id = self
            .transfer_job
            .as_ref()
            .map(|j| j.tab_id)
            .or_else(|| self.tabs.active().map(|t| t.id))
            .expect("active tab for finish_export");
        let tmp = export_temp_path(&dest);
        self.finish_export(tab_id, generation, dest, tmp, Instant::now(), result, cx);
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
    /// Height is fixed so the row does not swallow the column. Uses `h_flex` so
    /// the File trigger sizes to its label (left-aligned); a block row made the
    /// ghost Button stretch full-width and `Anchor::TopLeft` put the menu at
    /// x=0 (#101). When client-side decorations are active, the menu lives
    /// inside [`Self::client_title_bar`] instead.
    #[cfg(not(target_os = "macos"))]
    pub(crate) fn menu_bar_row(cx: &App) -> impl IntoElement {
        h_flex()
            .id("sela-menu-bar")
            .w_full()
            .h(px(32.))
            .flex_shrink_0()
            .border_b_1()
            .border_color(cx.theme().title_bar_border)
            .bg(cx.theme().title_bar)
            .px_1()
            .child(Self::file_menu_button())
    }

    /// File ▸ Quit, anchored to the File label via DropdownMenu/Popover (#101).
    /// `AppMenuBar`'s deferred `anchored()` popup was painted at the window
    /// origin, so Quit overlapped the sidebar title and missed hit-tests.
    #[cfg(not(target_os = "macos"))]
    pub(crate) fn file_menu_button() -> impl IntoElement {
        Button::new("file-menu")
            .small()
            .ghost()
            .label("File")
            .dropdown_menu(|menu, _, _| {
                menu.separator()
                    .item(PopupMenuItem::new("Quit").action(Box::new(Quit)))
            })
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
                                    row.child(Self::file_menu_button())
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
        // SSH-07 / SSH-18 dialogs are queued from async connect/test completions.
        self.show_host_key_mismatch_dialog_if_pending(window, cx);
        self.show_unknown_host_key_dialog_if_pending(window, cx);
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
            .relative()
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
                            workspace.run_editor(window, cx);
                        }
                    });
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &RunAllSql, window, cx| {
                    this.update(cx, |workspace, cx| {
                        if workspace.editor_field_focused(window, cx) {
                            workspace.run_all_editor(window, cx);
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
                move |_: &ExportData, window, cx| {
                    this.update(cx, |workspace, cx| {
                        workspace.open_export_dialog(window, cx);
                    });
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &ImportCsv, window, cx| {
                    this.update(cx, |workspace, cx| {
                        workspace.open_import_dialog(window, cx);
                    });
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
                move |_: &InspectCell, _, cx| {
                    this.update(cx, |workspace, cx| workspace.inspect_focused_cell(cx));
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
                move |_: &PreviewChanges, window, cx| {
                    this.update(cx, |workspace, cx| {
                        workspace.handle_preview_changes(window, cx);
                    });
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
            .on_action({
                let this = this.clone();
                move |_: &ToggleQuickOpen, window, cx| {
                    this.update(cx, |workspace, cx| {
                        workspace.open_palette(palette::PaletteMode::Tables, window, cx);
                    });
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &ToggleCommandPalette, window, cx| {
                    this.update(cx, |workspace, cx| {
                        workspace.open_palette(palette::PaletteMode::Actions, window, cx);
                    });
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &EscapePalette, window, cx| {
                    this.update(cx, |workspace, cx| workspace.close_palette(window, cx));
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &PaletteSelectUp, _, cx| {
                    this.update(cx, |workspace, cx| workspace.palette_move_selection(-1, cx));
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &PaletteSelectDown, _, cx| {
                    this.update(cx, |workspace, cx| workspace.palette_move_selection(1, cx));
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &PaletteConfirm, window, cx| {
                    this.update(cx, |workspace, cx| workspace.palette_confirm(window, cx));
                }
            })
            .map(|root| {
                if client_title_bar {
                    root.child(self.client_title_bar(cx))
                } else {
                    #[cfg(not(target_os = "macos"))]
                    {
                        root.child(Self::menu_bar_row(cx))
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
                                    .when_some(self.workspace_status.clone(), |column, status| {
                                        column.child(self.workspace_status_banner(cx, status))
                                    })
                                    .child(self.empty_content(cx))
                            }),
                    )
                    .child(self.command_palette_overlay(cx)),
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
