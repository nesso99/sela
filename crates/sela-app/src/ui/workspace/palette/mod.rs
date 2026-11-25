//! Command palette + quick open (M3-4 / ux/12-command-palette.md).

mod fuzzy;

pub(crate) use fuzzy::rank_items;

use std::collections::VecDeque;

use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme};
use gpui_kit::prelude::*;
use gpui_kit::{
    div, px, AnyElement, App, Context, Entity, Focusable as _, IntoElement, TestSupportExt as _,
    Window,
};

use sela_config::{ThemePreference, MAX_FONT_SIZE, MIN_FONT_SIZE};
use sela_core::{LoadedSchema, SafeMode};

use super::Workspace;

pub(crate) const PALETTE_KEY_CONTEXT: &str = "CommandPalette";
pub(crate) const PALETTE_WIDTH: f32 = 520.;
pub(crate) const PALETTE_MAX_HEIGHT: f32 = 420.;
const RECENT_CAP: usize = 10;

/// Empty table-mode row when disconnected / no catalog (PAL-09).
pub(crate) const NO_TABLES_ROW: &str = "No tables — connect and refresh schema.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PaletteMode {
    Tables,
    Actions,
}

/// How action mode was entered (affects backspace → tables).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActionEntry {
    Chord,
    /// Typed `>` as the first character in table mode.
    LeadingGt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum PaletteAction {
    NewConnection,
    Connect,
    Disconnect,
    NewQueryTab,
    RunQuery,
    RefreshSchema,
    Export,
    ImportCsv,
    ToggleTheme,
    FontBigger,
    FontSmaller,
    ToggleSafeMode,
    Quit,
    PreviewSql,
    Commit,
    Discard,
    Stop,
    ToggleFilter,
    ToggleHistory,
}

impl PaletteAction {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::NewConnection => "New connection",
            Self::Connect => "Connect",
            Self::Disconnect => "Disconnect",
            Self::NewQueryTab => "New query tab",
            Self::RunQuery => "Run query",
            Self::RefreshSchema => "Refresh schema",
            Self::Export => "Export…",
            Self::ImportCsv => "Import CSV…",
            Self::ToggleTheme => "Toggle theme",
            Self::FontBigger => "Font bigger",
            Self::FontSmaller => "Font smaller",
            Self::ToggleSafeMode => "Toggle safe mode",
            Self::Quit => "Quit",
            Self::PreviewSql => "Preview SQL",
            Self::Commit => "Commit",
            Self::Discard => "Discard",
            Self::Stop => "Stop",
            Self::ToggleFilter => "Toggle filter",
            Self::ToggleHistory => "Toggle history",
        }
    }

    pub(crate) fn shortcut(self) -> Option<&'static str> {
        match self {
            Self::NewQueryTab => Some("Ctrl/⌘+T"),
            Self::RunQuery => Some("Ctrl/⌘+Enter"),
            Self::Quit => Some("Ctrl/⌘+Q"),
            Self::PreviewSql => Some("Ctrl/⌘+Shift+V"),
            Self::Commit => Some("Ctrl/⌘+S"),
            Self::Discard => Some("Ctrl/⌘+Shift+Z"),
            Self::Stop => Some("Ctrl/⌘+."),
            Self::ToggleFilter => Some("Ctrl/⌘+F"),
            Self::ToggleHistory => Some("Ctrl/⌘+Shift+H"),
            Self::FontBigger => Some("Ctrl/⌘+="),
            Self::FontSmaller => Some("Ctrl/⌘+-"),
            _ => None,
        }
    }

    /// Primary actions listed in ux/12 (PAL-03 order).
    pub(crate) fn all() -> &'static [PaletteAction] {
        &[
            Self::NewConnection,
            Self::Connect,
            Self::Disconnect,
            Self::NewQueryTab,
            Self::RunQuery,
            Self::RefreshSchema,
            Self::Export,
            Self::ImportCsv,
            Self::ToggleTheme,
            Self::FontBigger,
            Self::FontSmaller,
            Self::ToggleSafeMode,
            Self::Quit,
            Self::PreviewSql,
            Self::Commit,
            Self::Discard,
            Self::Stop,
            Self::ToggleFilter,
            Self::ToggleHistory,
        ]
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TableRow {
    pub schema: String,
    pub table: String,
    /// Right-side muted label when names collide across schemas.
    pub disambiguator: Option<String>,
}

impl TableRow {
    pub(crate) fn display_label(&self) -> String {
        format!("{}.{}", self.schema, self.table)
    }
}

#[derive(Debug, Clone)]
pub(crate) enum PaletteItem {
    Table(TableRow),
    Action {
        action: PaletteAction,
        disabled_reason: Option<String>,
    },
    /// Muted empty-state row (not runnable).
    EmptyHint(&'static str),
}

impl PaletteItem {
    pub(crate) fn label(&self) -> String {
        match self {
            Self::Table(t) => t.display_label(),
            Self::Action { action, .. } => action.label().to_string(),
            Self::EmptyHint(s) => (*s).to_string(),
        }
    }

    pub(crate) fn enabled(&self) -> bool {
        match self {
            Self::Table(_) => true,
            Self::Action {
                disabled_reason, ..
            } => disabled_reason.is_none(),
            Self::EmptyHint(_) => false,
        }
    }

    pub(crate) fn right_label(&self) -> Option<String> {
        match self {
            Self::Table(t) => t.disambiguator.clone(),
            Self::Action {
                action,
                disabled_reason,
            } => disabled_reason
                .clone()
                .or_else(|| action.shortcut().map(str::to_string)),
            Self::EmptyHint(_) => None,
        }
    }
}

/// Snapshot of workspace state used to enable/disable actions (testable).
#[derive(Debug, Clone, Default)]
pub(crate) struct ActionContext {
    pub connected: bool,
    /// Sidebar tree highlight is a saved connection id (not the live session).
    pub sidebar_connection_id: Option<String>,
    pub live_connection_id: Option<String>,
    pub editor_has_sql: bool,
    pub query_busy: bool,
    pub has_exportable: bool,
    pub table_data_open: bool,
    pub read_only: bool,
    pub font_size: u8,
    pub has_pending: bool,
    pub committing: bool,
    pub can_commit: bool,
}

impl ActionContext {
    pub(crate) fn disabled_reason(&self, action: PaletteAction) -> Option<&'static str> {
        match action {
            PaletteAction::NewConnection
            | PaletteAction::ToggleTheme
            | PaletteAction::Quit
            | PaletteAction::ToggleHistory => None,
            PaletteAction::Connect => {
                let Some(sel) = self.sidebar_connection_id.as_deref() else {
                    return Some("Select a connection");
                };
                if self.live_connection_id.as_deref() == Some(sel) && self.connected {
                    Some("Already connected")
                } else {
                    None
                }
            }
            PaletteAction::Disconnect
            | PaletteAction::NewQueryTab
            | PaletteAction::RefreshSchema
            | PaletteAction::ToggleSafeMode => {
                if self.connected {
                    None
                } else {
                    Some("Not connected")
                }
            }
            PaletteAction::RunQuery => {
                if self.query_busy {
                    Some("Query running")
                } else if !self.editor_has_sql {
                    Some("Editor empty")
                } else {
                    None
                }
            }
            PaletteAction::Export => {
                if self.has_exportable {
                    None
                } else {
                    Some("Nothing to export")
                }
            }
            PaletteAction::ImportCsv => {
                if !self.table_data_open {
                    Some("Open a table")
                } else if self.read_only {
                    Some("Import is disabled in Read-only mode.")
                } else {
                    None
                }
            }
            PaletteAction::FontBigger => {
                if self.font_size >= MAX_FONT_SIZE {
                    Some("Already at maximum")
                } else {
                    None
                }
            }
            PaletteAction::FontSmaller => {
                if self.font_size <= MIN_FONT_SIZE {
                    Some("Already at minimum")
                } else {
                    None
                }
            }
            PaletteAction::PreviewSql | PaletteAction::Discard => {
                if self.has_pending {
                    None
                } else {
                    Some("No pending changes")
                }
            }
            PaletteAction::Commit => {
                if self.committing {
                    Some("Query running")
                } else if !self.has_pending {
                    Some("No pending changes")
                } else if !self.can_commit {
                    Some("Commit unavailable")
                } else {
                    None
                }
            }
            PaletteAction::Stop => {
                if self.query_busy || self.committing {
                    None
                } else {
                    Some("Nothing running")
                }
            }
            PaletteAction::ToggleFilter => {
                if self.table_data_open {
                    None
                } else {
                    Some("Open a table")
                }
            }
        }
    }
}

/// Build table rows from the schema cache; mark schema when names collide (PAL-10).
pub(crate) fn table_rows_from_catalog(catalog: &[LoadedSchema]) -> Vec<TableRow> {
    let mut name_counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for schema in catalog {
        for table in &schema.tables {
            *name_counts.entry(table.name.as_str()).or_insert(0) += 1;
        }
    }
    let mut rows = Vec::new();
    for schema in catalog {
        for table in &schema.tables {
            let collide = name_counts.get(table.name.as_str()).copied().unwrap_or(0) > 1;
            rows.push(TableRow {
                schema: schema.name.clone(),
                table: table.name.clone(),
                disambiguator: if collide {
                    Some(schema.name.clone())
                } else {
                    None
                },
            });
        }
    }
    rows
}

pub(crate) struct PaletteState {
    pub open: bool,
    pub mode: PaletteMode,
    action_entry: Option<ActionEntry>,
    pub query: Entity<InputState>,
    pub query_text: String,
    pub selected: usize,
    /// Session recent tables (`schema.table`).
    recent_tables: VecDeque<String>,
    /// Session recent action labels.
    recent_actions: VecDeque<PaletteAction>,
    /// Focus to restore on close (PAL-07).
    restore_focus: Option<gpui_kit::FocusHandle>,
}

impl PaletteState {
    pub fn new(window: &mut Window, cx: &mut App) -> Self {
        let query = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Search tables or type > for actions")
        });
        Self {
            open: false,
            mode: PaletteMode::Tables,
            action_entry: None,
            query,
            query_text: String::new(),
            selected: 0,
            recent_tables: VecDeque::new(),
            recent_actions: VecDeque::new(),
            restore_focus: None,
        }
    }

    pub fn clear_query(&mut self, window: &mut Window, cx: &mut App) {
        self.query_text.clear();
        self.query.update(cx, |input, cx| {
            input.set_value("", window, cx);
        });
        self.selected = 0;
    }

    fn push_recent_table(&mut self, label: String) {
        self.recent_tables.retain(|t| t != &label);
        self.recent_tables.push_front(label);
        while self.recent_tables.len() > RECENT_CAP {
            self.recent_tables.pop_back();
        }
    }

    fn push_recent_action(&mut self, action: PaletteAction) {
        self.recent_actions.retain(|a| *a != action);
        self.recent_actions.push_front(action);
        while self.recent_actions.len() > RECENT_CAP {
            self.recent_actions.pop_back();
        }
    }
}

/// Visible rows for the current mode + query (PAL-02…PAL-04, PAL-09).
pub(crate) fn visible_items(
    mode: PaletteMode,
    query: &str,
    catalog: &[LoadedSchema],
    recent_tables: &VecDeque<String>,
    recent_actions: &VecDeque<PaletteAction>,
    action_ctx: &ActionContext,
) -> Vec<(PaletteItem, Vec<usize>)> {
    match mode {
        PaletteMode::Tables => {
            let rows = table_rows_from_catalog(catalog);
            if rows.is_empty() {
                return vec![(PaletteItem::EmptyHint(NO_TABLES_ROW), Vec::new())];
            }
            let mut tagged: Vec<(TableRow, String, bool)> = rows
                .into_iter()
                .map(|row| {
                    let label = row.display_label();
                    let recent = recent_tables.iter().any(|r| r == &label);
                    (row, label, recent)
                })
                .collect();
            // Stable recent-first order for empty query.
            tagged.sort_by(|a, b| b.2.cmp(&a.2).then(a.1.cmp(&b.1)));
            let ranked = rank_items(
                query,
                tagged
                    .iter()
                    .map(|(row, label, recent)| (row.clone(), label.as_str(), *recent)),
            );
            ranked
                .into_iter()
                .map(|r| (PaletteItem::Table(r.item), r.indices))
                .collect()
        }
        PaletteMode::Actions => {
            let mut tagged: Vec<(PaletteAction, bool)> = PaletteAction::all()
                .iter()
                .copied()
                .map(|a| {
                    let recent = recent_actions.iter().any(|r| *r == a);
                    (a, recent)
                })
                .collect();
            tagged.sort_by_key(|a| std::cmp::Reverse(a.1));
            let ranked = rank_items(
                query,
                tagged.iter().map(|(a, recent)| (*a, a.label(), *recent)),
            );
            ranked
                .into_iter()
                .map(|r| {
                    let reason = action_ctx.disabled_reason(r.item).map(str::to_string);
                    (
                        PaletteItem::Action {
                            action: r.item,
                            disabled_reason: reason,
                        },
                        r.indices,
                    )
                })
                .collect()
        }
    }
}

/// Apply a keystroke against the query string (mode switches via `>`).
pub(crate) fn apply_query_edit(
    mode: PaletteMode,
    action_entry: Option<ActionEntry>,
    text: &str,
) -> (PaletteMode, Option<ActionEntry>, String) {
    match mode {
        PaletteMode::Tables => {
            if let Some(rest) = text.strip_prefix('>') {
                (
                    PaletteMode::Actions,
                    Some(ActionEntry::LeadingGt),
                    rest.to_string(),
                )
            } else {
                (PaletteMode::Tables, None, text.to_string())
            }
        }
        PaletteMode::Actions => {
            if action_entry == Some(ActionEntry::LeadingGt) && text.is_empty() {
                // Caller may still be mid-backspace; empty action query via `>` → tables.
                (PaletteMode::Tables, None, String::new())
            } else {
                (PaletteMode::Actions, action_entry, text.to_string())
            }
        }
    }
}

impl Workspace {
    pub(super) fn action_context_for_palette(&self, cx: &App) -> ActionContext {
        let view = self.active_view();
        let edit = view.and_then(|v| v.edit.as_ref());
        let connected = self.session.is_some();
        let live = self.active_id.clone();
        let sidebar = self.selected_connection_id.clone();
        let editor_has_sql = view
            .map(|v| !v.sql.read(cx).value().trim().is_empty())
            .unwrap_or(false);
        let query_busy = view.map(|v| v.busy).unwrap_or(false);
        let committing = edit.map(|e| e.committing).unwrap_or(false);
        let has_pending = edit.map(|e| e.is_dirty()).unwrap_or(false);
        let safe = self.active_safe_mode();
        let can_commit = edit
            .map(|e| e.commit_blocked_reason_for(safe).is_none() && !query_busy && !committing)
            .unwrap_or(false);
        let table_data_open = view.is_some_and(|v| {
            matches!(v.source, super::GridSource::Table { .. })
                && matches!(v.table_pane, super::TablePane::Data)
        });
        let has_exportable = view.is_some_and(|v| {
            matches!(
                v.source,
                super::GridSource::Table { .. } | super::GridSource::Sql(_)
            ) && v.page_len > 0
        });
        ActionContext {
            connected,
            sidebar_connection_id: sidebar,
            live_connection_id: live,
            editor_has_sql,
            query_busy,
            has_exportable,
            table_data_open,
            read_only: safe == SafeMode::ReadOnly,
            font_size: self.font_size,
            has_pending,
            committing,
            can_commit,
        }
    }

    pub(super) fn palette_items(&self, cx: &App) -> Vec<(PaletteItem, Vec<usize>)> {
        let Some(palette) = self.palette.as_ref() else {
            return Vec::new();
        };
        let ctx = self.action_context_for_palette(cx);
        let query = match palette.mode {
            PaletteMode::Actions => palette
                .query_text
                .strip_prefix('>')
                .unwrap_or(&palette.query_text),
            PaletteMode::Tables => palette.query_text.as_str(),
        };
        // Leading `>` is stripped into mode; stored query_text is already without `>` when in actions via `>`.
        let filter = if palette.mode == PaletteMode::Actions
            && palette.action_entry == Some(ActionEntry::LeadingGt)
        {
            palette.query_text.as_str()
        } else {
            query
        };
        visible_items(
            palette.mode,
            filter,
            &self.catalog,
            &palette.recent_tables,
            &palette.recent_actions,
            &ctx,
        )
    }

    #[cfg(test)]
    pub(crate) fn palette_is_open(&self) -> bool {
        self.palette.as_ref().is_some_and(|p| p.open)
    }

    #[cfg(test)]
    pub(crate) fn palette_mode(&self) -> Option<PaletteMode> {
        self.palette.as_ref().filter(|p| p.open).map(|p| p.mode)
    }

    pub(super) fn open_palette(
        &mut self,
        mode: PaletteMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.palette.is_none() {
            let state = PaletteState::new(window, cx);
            let query = state.query.clone();
            let sub = cx.subscribe_in(&query, window, |this, _input, event, window, cx| {
                use gpui_kit::component::input::InputEvent;
                match event {
                    InputEvent::Change => this.sync_palette_query(window, cx),
                    InputEvent::PressEnter { .. } => this.palette_confirm(window, cx),
                    _ => {}
                }
            });
            self._subscriptions.push(sub);
            self.palette = Some(state);
        }
        let Some(palette) = self.palette.as_mut() else {
            return;
        };
        if palette.open && palette.mode == mode {
            // Second chord of the same mode toggles closed (PAL-07).
            self.close_palette(window, cx);
            return;
        }
        if palette.open && palette.mode != mode {
            palette.mode = mode;
            palette.action_entry = if mode == PaletteMode::Actions {
                Some(ActionEntry::Chord)
            } else {
                None
            };
            palette.clear_query(window, cx);
            self.focus_palette_search(window, cx);
            cx.notify();
            return;
        }
        // Opening fresh.
        let restore = window.focused(cx);
        let palette = self.palette.as_mut().unwrap();
        palette.open = true;
        palette.mode = mode;
        palette.action_entry = if mode == PaletteMode::Actions {
            Some(ActionEntry::Chord)
        } else {
            None
        };
        palette.restore_focus = restore;
        palette.clear_query(window, cx);
        self.focus_palette_search(window, cx);
        cx.notify();
    }

    pub(super) fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(palette) = self.palette.as_mut() else {
            return;
        };
        if !palette.open {
            return;
        }
        palette.open = false;
        palette.clear_query(window, cx);
        let restore = palette.restore_focus.take();
        if let Some(handle) = restore {
            handle.focus(window, cx);
        }
        cx.notify();
    }

    fn focus_palette_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(palette) = self.palette.as_ref() {
            palette.query.read(cx).focus_handle(cx).focus(window, cx);
        }
    }

    pub(super) fn sync_palette_query(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(palette) = self.palette.as_mut() else {
            return;
        };
        if !palette.open {
            return;
        }
        let text = palette.query.read(cx).value().to_string();
        let (mode, entry, stored) = apply_query_edit(palette.mode, palette.action_entry, &text);
        // When switching tables → actions via `>`, rewrite the input without `>`.
        if mode == PaletteMode::Actions
            && palette.mode == PaletteMode::Tables
            && text.starts_with('>')
        {
            palette.query.update(cx, |input, cx| {
                input.set_value(&stored, window, cx);
            });
        }
        palette.mode = mode;
        palette.action_entry = entry;
        palette.query_text = stored;
        palette.selected = 0;
        cx.notify();
    }

    pub(super) fn palette_move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let items_len = self.palette_items(cx).len();
        if items_len == 0 {
            return;
        }
        let Some(palette) = self.palette.as_mut() else {
            return;
        };
        let len = items_len as isize;
        let next = (palette.selected as isize + delta).rem_euclid(len) as usize;
        palette.selected = next;
        cx.notify();
    }

    pub(super) fn palette_confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let items = self.palette_items(cx);
        let Some(palette) = self.palette.as_ref() else {
            return;
        };
        let Some((item, _)) = items.get(palette.selected) else {
            return;
        };
        if !item.enabled() {
            return;
        }
        match item.clone() {
            PaletteItem::Table(row) => {
                let label = row.display_label();
                if let Some(p) = self.palette.as_mut() {
                    p.push_recent_table(label);
                }
                self.close_palette(window, cx);
                self.open_table_tab(row.schema, row.table, window, cx);
            }
            PaletteItem::Action { action, .. } => {
                if let Some(p) = self.palette.as_mut() {
                    p.push_recent_action(action);
                }
                self.close_palette(window, cx);
                self.run_palette_action(action, window, cx);
            }
            PaletteItem::EmptyHint(_) => {}
        }
    }

    pub(super) fn run_palette_action(
        &mut self,
        action: PaletteAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            PaletteAction::NewConnection => self.open_form(None, window, cx),
            PaletteAction::Connect => {
                if let Some(id) = self.selected_connection_id.clone() {
                    self.connect_id(id, Some(window), cx);
                }
            }
            PaletteAction::Disconnect => self.request_disconnect(window, cx),
            PaletteAction::NewQueryTab => self.new_query_tab(window, cx),
            PaletteAction::RunQuery => self.run_editor(window, cx),
            PaletteAction::RefreshSchema => self.refresh_catalog(cx),
            PaletteAction::Export | PaletteAction::ImportCsv => {
                // M3-3 UI not shipped yet; actions stay reachable but dimmed when N/A.
                if let Some(view) = self.active_view_mut() {
                    view.status = match action {
                        PaletteAction::Export => "Export UI is not available yet.".into(),
                        _ => "Import UI is not available yet.".into(),
                    };
                }
                cx.notify();
            }
            PaletteAction::ToggleTheme => {
                let next = match self.theme_preference {
                    ThemePreference::Light => ThemePreference::Dark,
                    ThemePreference::Dark if self.system_theme_available => ThemePreference::System,
                    ThemePreference::Dark => ThemePreference::Light,
                    ThemePreference::System => ThemePreference::Light,
                };
                self.set_theme_preference(next, window, cx);
            }
            PaletteAction::FontBigger => self.bump_font_size(1, window, cx),
            PaletteAction::FontSmaller => self.bump_font_size(-1, window, cx),
            PaletteAction::ToggleSafeMode => self.cycle_safe_mode_from_palette(window, cx),
            PaletteAction::Quit => self.request_quit(window, cx),
            PaletteAction::PreviewSql => self.toggle_preview_sheet(cx),
            PaletteAction::Commit => self.request_commit_changes(window, cx),
            PaletteAction::Discard => self.request_discard_changes(window, cx),
            PaletteAction::Stop => self.stop_query(cx),
            PaletteAction::ToggleFilter => self.toggle_filter_bar(window, cx),
            PaletteAction::ToggleHistory => self.toggle_history_panel(window, cx),
        }
    }

    /// Off → Confirm writes → Read-only → Off; persist, keep pending, reconnect (PAL-12).
    pub(super) fn cycle_safe_mode_from_palette(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.active_id.clone() else {
            return;
        };
        let Some(conn) = self.connections.iter_mut().find(|c| c.id == id) else {
            return;
        };
        conn.safe_mode = match conn.safe_mode {
            SafeMode::Off => SafeMode::Confirm,
            SafeMode::Confirm => SafeMode::ReadOnly,
            SafeMode::ReadOnly => SafeMode::Off,
        };
        let new_mode = conn.safe_mode;
        if let Err(err) = self.store.save(&self.connections) {
            if let Some(view) = self.active_view_mut() {
                view.error = Some(err.to_string());
            }
            cx.notify();
            return;
        }
        self.sync_active_edit_safe_mode();
        if let Some(view) = self.active_view_mut() {
            view.status = format!("Safe mode → {}. Reconnecting…", new_mode.label());
        }
        // Force reconnect so server-side Read-only applies; keep same-connection tabs.
        self.session = None;
        self.proceed_connect_id(id, Some(window), cx);
        cx.notify();
    }

    pub(super) fn text_input_focused(&self, window: &Window, cx: &App) -> bool {
        if self.editor_field_focused(window, cx) || self.filter_field_focused(window, cx) {
            return true;
        }
        if self.history_search_focused(window, cx) {
            return true;
        }
        if self
            .active_view()
            .is_some_and(|v| v.cell_editor_focused(window, cx))
        {
            return true;
        }
        if let Some(palette) = self.palette.as_ref() {
            if palette.open && palette.query.read(cx).focus_handle(cx).is_focused(window) {
                return true;
            }
        }
        false
    }

    /// Centered overlay (~520×420). Backdrop click closes (PAL-01, PAL-07).
    pub(super) fn command_palette_overlay(&self, cx: &Context<Self>) -> impl IntoElement {
        let Some(palette) = self.palette.as_ref() else {
            return div().into_any_element();
        };
        if !palette.open {
            return div().into_any_element();
        }
        let this = cx.entity();
        let items = self.palette_items(cx);
        let selected = palette.selected.min(items.len().saturating_sub(1));
        let mode_label = match palette.mode {
            PaletteMode::Tables => "Tables",
            PaletteMode::Actions => "Actions",
        };
        let query = palette.query.clone();

        div()
            .id("command-palette-root")
            .test_support()
            .absolute()
            .inset_0()
            .flex()
            .items_start()
            .justify_center()
            .pt(px(80.))
            .bg(gpui_kit::hsla(0., 0., 0., 0.35))
            .child(
                div()
                    .id("command-palette")
                    .test_support()
                    .key_context(PALETTE_KEY_CONTEXT)
                    .w(px(PALETTE_WIDTH))
                    .max_h(px(PALETTE_MAX_HEIGHT))
                    .flex()
                    .flex_col()
                    .border_1()
                    .border_color(cx.theme().border)
                    .rounded(px(6.))
                    .bg(cx.theme().background)
                    .shadow_md()
                    // Stop clicks on the panel (search, rows, header) from
                    // reaching the backdrop / parent dismiss path (PAL-07).
                    .occlude()
                    .on_mouse_down_out({
                        let this = this.clone();
                        move |_, window, cx| {
                            this.update(cx, |workspace, cx| {
                                workspace.close_palette(window, cx);
                            });
                        }
                    })
                    .on_action({
                        let this = this.clone();
                        move |_: &super::super::PaletteSelectUp, _, cx| {
                            this.update(cx, |ws, cx| ws.palette_move_selection(-1, cx));
                        }
                    })
                    .on_action({
                        let this = this.clone();
                        move |_: &super::super::PaletteSelectDown, _, cx| {
                            this.update(cx, |ws, cx| ws.palette_move_selection(1, cx));
                        }
                    })
                    .on_action({
                        let this = this.clone();
                        move |_: &super::super::PaletteConfirm, window, cx| {
                            this.update(cx, |ws, cx| ws.palette_confirm(window, cx));
                        }
                    })
                    .on_action({
                        let this = this.clone();
                        move |_: &super::super::EscapePalette, window, cx| {
                            this.update(cx, |ws, cx| ws.close_palette(window, cx));
                        }
                    })
                    .child(
                        h_flex()
                            .px_2()
                            .pt_2()
                            .pb_1()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(mode_label),
                            ),
                    )
                    .child(
                        div()
                            .px_2()
                            .pb_2()
                            .child(Input::new(&query).id("palette-search")),
                    )
                    .child(
                        v_flex()
                            .id("palette-list")
                            .test_support()
                            .flex_1()
                            .min_h(px(0.))
                            .max_h(px(PALETTE_MAX_HEIGHT - 72.))
                            .overflow_y_scrollbar()
                            .pb_1()
                            .children(items.into_iter().enumerate().map(
                                |(ix, (item, indices))| {
                                    let this = this.clone();
                                    let enabled = item.enabled();
                                    let label = item.label();
                                    let right = item.right_label();
                                    let muted = !enabled;
                                    let selected_row = ix == selected;
                                    div()
                                        .id(("palette-row", ix))
                                        .test_support()
                                        .px_2()
                                        .py_1()
                                        .mx_1()
                                        .rounded(px(4.))
                                        .when(selected_row, |row| row.bg(cx.theme().tokens.tab_bar))
                                        .when(muted, |row| {
                                            row.text_color(cx.theme().muted_foreground)
                                        })
                                        .on_mouse_down(gpui_kit::MouseButton::Left, {
                                            let this = this.clone();
                                            move |_, window, cx| {
                                                this.update(cx, |ws, cx| {
                                                    if let Some(p) = ws.palette.as_mut() {
                                                        p.selected = ix;
                                                    }
                                                    ws.palette_confirm(window, cx);
                                                });
                                            }
                                        })
                                        .child(
                                            h_flex()
                                                .w_full()
                                                .items_center()
                                                .justify_between()
                                                .gap_2()
                                                .child(highlight_label(&label, &indices, muted, cx))
                                                .when_some(right, |row, text| {
                                                    row.child(
                                                        div()
                                                            .text_xs()
                                                            .text_color(cx.theme().muted_foreground)
                                                            .child(text),
                                                    )
                                                }),
                                        )
                                },
                            )),
                    ),
            )
            .into_any_element()
    }
}

fn highlight_label(
    label: &str,
    indices: &[usize],
    muted: bool,
    cx: &Context<Workspace>,
) -> impl IntoElement {
    if indices.is_empty() {
        return div().child(label.to_string()).into_any_element();
    }
    let matched: std::collections::HashSet<usize> = indices.iter().copied().collect();
    let mut parts: Vec<AnyElement> = Vec::new();
    let mut buf = String::new();
    let mut buf_matched = false;
    let flush = |buf: &mut String,
                 buf_matched: bool,
                 parts: &mut Vec<AnyElement>,
                 muted: bool,
                 cx: &Context<Workspace>| {
        if buf.is_empty() {
            return;
        }
        let text = std::mem::take(buf);
        let el = if buf_matched && !muted {
            div()
                .font_weight(gpui_kit::FontWeight::BOLD)
                .text_color(cx.theme().accent)
                .child(text)
                .into_any_element()
        } else {
            div().child(text).into_any_element()
        };
        parts.push(el);
    };
    for (byte_ix, ch) in label.char_indices() {
        let is_m = matched.contains(&byte_ix);
        if !buf.is_empty() && is_m != buf_matched {
            flush(&mut buf, buf_matched, &mut parts, muted, cx);
            buf_matched = is_m;
        } else if buf.is_empty() {
            buf_matched = is_m;
        }
        buf.push(ch);
    }
    flush(&mut buf, buf_matched, &mut parts, muted, cx);
    h_flex().children(parts).into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sela_core::{RelationKind, TableInfo};

    fn catalog_two_books() -> Vec<LoadedSchema> {
        vec![
            LoadedSchema {
                name: "public".into(),
                tables: vec![TableInfo {
                    name: "books".into(),
                    kind: RelationKind::Table,
                }],
            },
            LoadedSchema {
                name: "other".into(),
                tables: vec![TableInfo {
                    name: "books".into(),
                    kind: RelationKind::Table,
                }],
            },
        ]
    }

    #[test]
    fn pal09_empty_catalog_shows_hint() {
        let items = visible_items(
            PaletteMode::Tables,
            "",
            &[],
            &VecDeque::new(),
            &VecDeque::new(),
            &ActionContext::default(),
        );
        assert_eq!(items.len(), 1);
        assert!(!items[0].0.enabled());
        assert_eq!(items[0].0.label(), NO_TABLES_ROW);
    }

    #[test]
    fn pal10_duplicate_table_names_disambiguated() {
        let rows = table_rows_from_catalog(&catalog_two_books());
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.disambiguator.is_some()));
        let labels: Vec<_> = rows.iter().map(|r| r.display_label()).collect();
        assert!(labels.contains(&"public.books".to_string()));
        assert!(labels.contains(&"other.books".to_string()));
    }

    #[test]
    fn pal06_connect_reasons() {
        let mut ctx = ActionContext::default();
        assert_eq!(
            ctx.disabled_reason(PaletteAction::Connect),
            Some("Select a connection")
        );
        ctx.sidebar_connection_id = Some("a".into());
        ctx.live_connection_id = Some("a".into());
        ctx.connected = true;
        assert_eq!(
            ctx.disabled_reason(PaletteAction::Connect),
            Some("Already connected")
        );
        ctx.sidebar_connection_id = Some("b".into());
        assert_eq!(ctx.disabled_reason(PaletteAction::Connect), None);
    }

    #[test]
    fn pal11_gt_switches_to_actions() {
        let (mode, entry, stored) = apply_query_edit(PaletteMode::Tables, None, ">run");
        assert_eq!(mode, PaletteMode::Actions);
        assert_eq!(entry, Some(ActionEntry::LeadingGt));
        assert_eq!(stored, "run");
        let (mode2, _, _) =
            apply_query_edit(PaletteMode::Actions, Some(ActionEntry::LeadingGt), "");
        assert_eq!(mode2, PaletteMode::Tables);
    }

    #[test]
    fn pal03_action_list_complete() {
        assert!(PaletteAction::all().len() >= 19);
        assert!(PaletteAction::all().contains(&PaletteAction::PreviewSql));
        assert!(PaletteAction::all().contains(&PaletteAction::ToggleSafeMode));
    }
}
