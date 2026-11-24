//! History panel open/close, search refresh, and row actions.

use chrono::Utc;
use gpui_kit::component::WindowExt as _;
use gpui_kit::{ClipboardItem, Context, Focusable as _, Window};
use sela_config::HistoryStore;
use sela_core::{
    history_rerun_needs_confirm, HistoryEntry, HistoryFilter, HistoryOutcome, HISTORY_ROW_CAP,
};

use super::{HistoryFocusRestore, HistoryScope, MIN_HISTORY_HEIGHT};
use crate::ui::workspace::Workspace;

impl Workspace {
    pub(in super::super) fn history_max_height(&self, window: &Window) -> f32 {
        let bounds = window.bounds().size;
        let chrome = 44. + 28. + 28. + 176. + 28.;
        let body = f32::from(bounds.height) - chrome;
        (body * 0.5).max(MIN_HISTORY_HEIGHT)
    }

    pub(in super::super) fn history_scope_connection_id(&self) -> Option<String> {
        self.tabs
            .active()
            .map(|tab| tab.connection_id.clone())
            .filter(|id| !id.is_empty())
    }

    pub(in super::super) fn history_display_name(&self, entry: &HistoryEntry) -> String {
        self.connections
            .iter()
            .find(|c| c.id == entry.connection_id)
            .map(|c| c.name.clone())
            .unwrap_or_else(|| entry.connection_name.clone())
    }

    pub(in super::super) fn history_empty_message(&self) -> String {
        if self.history_reset_notice {
            return "History was reset because the file could not be read.".into();
        }
        if !self.history_search_text.is_empty() || self.history_outcome.is_some() {
            return "No queries match.".into();
        }
        if matches!(self.history_scope, HistoryScope::ThisConnection) {
            if let Some(id) = self.history_scope_connection_id() {
                if let Some(conn) = self.connections.iter().find(|c| c.id == id) {
                    if !conn.record_history {
                        return format!("History is off for {}.", conn.name);
                    }
                }
            }
        }
        "Queries you run are listed here.".into()
    }

    pub(in super::super) fn history_show_turn_on(&self) -> bool {
        if !matches!(self.history_scope, HistoryScope::ThisConnection) {
            return false;
        }
        let Some(id) = self.history_scope_connection_id() else {
            return false;
        };
        self.connections
            .iter()
            .find(|c| c.id == id)
            .is_some_and(|c| !c.record_history)
            && self.history_search_text.is_empty()
            && self.history_outcome.is_none()
            && !self.history_reset_notice
    }

    pub(in super::super) fn toggle_history_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.history_open {
            self.close_history_panel(window, cx);
        } else {
            self.open_history_panel(window, cx);
        }
    }

    pub(in super::super) fn open_history_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.editor_field_focused(window, cx) {
            self.history_focus_restore = HistoryFocusRestore::Editor;
        } else {
            self.history_focus_restore = HistoryFocusRestore::Grid;
        }
        self.history_open = true;
        if self.history_scope_connection_id().is_none() {
            self.history_scope = HistoryScope::All;
        }
        self.refresh_history(cx);
        let search = self.history_search.clone();
        search.update(cx, |state, cx| {
            state.focus(window, cx);
        });
        cx.notify();
    }

    pub(in super::super) fn close_history_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.history_open = false;
        match self.history_focus_restore {
            HistoryFocusRestore::Editor => {
                if let Some(view) = self.active_view() {
                    let sql = view.sql.clone();
                    sql.update(cx, |state, cx| {
                        state.focus(window, cx);
                    });
                }
            }
            HistoryFocusRestore::Grid => {
                if let Some(view) = self.active_view() {
                    let grid = view.grid.clone();
                    grid.update(cx, |state, cx| {
                        state.focus_handle(cx).focus(window, cx);
                    });
                }
            }
            HistoryFocusRestore::None => {}
        }
        cx.notify();
    }

    pub(in super::super) fn escape_history_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.history_search_text.is_empty() {
            self.history_search_text.clear();
            let search = self.history_search.clone();
            search.update(cx, |state, cx| {
                state.set_value("", window, cx);
            });
            self.refresh_history(cx);
            return;
        }
        self.close_history_panel(window, cx);
    }

    pub(in super::super) fn focus_history_search(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.history_open {
            return;
        }
        let search = self.history_search.clone();
        search.update(cx, |state, cx| {
            state.focus(window, cx);
        });
    }

    pub(in super::super) fn history_search_focused(
        &self,
        window: &Window,
        cx: &gpui_kit::App,
    ) -> bool {
        self.history_open
            && self
                .history_search
                .read(cx)
                .focus_handle(cx)
                .is_focused(window)
    }

    pub(in super::super) fn refresh_history(&mut self, cx: &mut Context<Self>) {
        let Some(store) = self.history_store.clone() else {
            return;
        };
        let connection_id = match self.history_scope {
            HistoryScope::ThisConnection => self.history_scope_connection_id(),
            HistoryScope::All => None,
        };
        let filter = HistoryFilter {
            connection_id,
            outcome: self.history_outcome,
            search: self.history_search_text.clone(),
        };
        self.history_request = self.history_request.wrapping_add(1);
        let request = self.history_request;
        cx.spawn(async move |this, async_cx| {
            let rows = async_cx
                .background_executor()
                .spawn(async move { store.query(&filter, HISTORY_ROW_CAP) })
                .await;
            this.update(async_cx, |ws, cx| {
                if ws.history_request != request {
                    return;
                }
                match rows {
                    Ok(rows) => {
                        ws.history_rows = rows;
                        if ws
                            .history_selected
                            .is_some_and(|ix| ix >= ws.history_rows.len())
                        {
                            ws.history_selected = ws.history_rows.len().checked_sub(1);
                        }
                        if ws.history_selected.is_none() && !ws.history_rows.is_empty() {
                            ws.history_selected = Some(0);
                        }
                        ws.history_panel_error = None;
                    }
                    Err(err) => {
                        ws.history_panel_error = Some(err.to_string());
                        ws.history_rows.clear();
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(in super::super) fn select_history_id(&mut self, id: &str) {
        self.history_selected = self.history_rows.iter().position(|e| e.id == id);
    }

    pub(in super::super) fn history_move_selection(
        &mut self,
        delta: isize,
        cx: &mut Context<Self>,
    ) {
        if self.history_rows.is_empty() {
            return;
        }
        let len = self.history_rows.len() as isize;
        let current = self.history_selected.unwrap_or(0) as isize;
        let next = (current + delta).clamp(0, len - 1) as usize;
        self.history_selected = Some(next);
        cx.notify();
    }

    pub(in super::super) fn selected_history_entry(&self) -> Option<&HistoryEntry> {
        self.history_selected
            .and_then(|ix| self.history_rows.get(ix))
    }

    pub(in super::super) fn history_open_in_tab(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = self.selected_history_entry().cloned() else {
            return;
        };
        if !self.connections.iter().any(|c| c.id == entry.connection_id) {
            return;
        }
        self.open_query_tab_with_sql(&entry.connection_id, &entry.sql, false, window, cx);
    }

    pub(in super::super) fn history_insert_into_editor(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = self.selected_history_entry().cloned() else {
            return;
        };
        let Some(view) = self.active_view() else {
            return;
        };
        let sql = view.sql.clone();
        sql.update(cx, |state, cx| {
            let range = state.selected_range();
            if range.is_empty() {
                state.insert(entry.sql.clone(), window, cx);
            } else {
                state.replace(entry.sql.clone(), window, cx);
            }
        });
    }

    pub(in super::super) fn history_copy(&mut self, cx: &mut Context<Self>) {
        let Some(entry) = self.selected_history_entry() else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(entry.sql.clone()));
    }

    pub(in super::super) fn history_delete_selected(&mut self, cx: &mut Context<Self>) {
        let Some(entry) = self.selected_history_entry().cloned() else {
            return;
        };
        let Some(store) = self.history_store.clone() else {
            return;
        };
        let id = entry.id;
        if let Err(err) = store.delete(&id) {
            self.note_history_write_failure(&err.to_string(), cx);
            return;
        }
        self.refresh_history(cx);
    }

    pub(in super::super) fn history_rerun(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.selected_history_entry().cloned() else {
            return;
        };
        if !self.connections.iter().any(|c| c.id == entry.connection_id) {
            return;
        }
        if history_rerun_needs_confirm(&entry.sql) {
            let this = cx.entity();
            let sql = entry.sql.clone();
            let connection_id = entry.connection_id.clone();
            window.open_alert_dialog(cx, move |alert, _, _| {
                let this = this.clone();
                let sql = sql.clone();
                let connection_id = connection_id.clone();
                alert
                    .title("Rerun this statement?")
                    .description(sql.clone())
                    .confirm()
                    .ok_text("Rerun")
                    .cancel_variant(gpui_kit::component::button::ButtonVariant::Primary)
                    .on_ok(move |_, window, cx| {
                        this.update(cx, |ws, cx| {
                            ws.open_query_tab_with_sql(&connection_id, &sql, true, window, cx);
                        });
                        true
                    })
            });
            return;
        }
        self.open_query_tab_with_sql(&entry.connection_id, &entry.sql, true, window, cx);
    }

    pub(in super::super) fn open_query_tab_with_sql(
        &mut self,
        connection_id: &str,
        sql: &str,
        run: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active_id.as_deref() != Some(connection_id) {
            self.connect_id(connection_id.to_string(), cx);
        }
        let id = self.tabs.open_query(connection_id.to_string(), sql);
        self.attach_view(id, sql, window, cx);
        self.persist_tabs(cx);
        if run {
            self.begin_sql_run(id, vec![sql.to_string()], cx);
        }
        cx.notify();
    }

    pub(in super::super) fn confirm_clear_history(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let this = cx.entity();
        let scoped = matches!(self.history_scope, HistoryScope::ThisConnection)
            .then(|| self.history_scope_connection_id())
            .flatten();
        // UX doc specifies the This-connection description; All only names the
        // title and OK label, so the All body mirrors that wording.
        let (title, description) = if let Some(id) = scoped.clone() {
            let name = self
                .connections
                .iter()
                .find(|c| c.id == id)
                .map(|c| c.name.as_str())
                .unwrap_or("this connection");
            (
                format!("Clear history for {name}?"),
                "Queries stored for this connection are removed. The database is left untouched."
                    .to_string(),
            )
        } else {
            (
                "Clear all query history?".into(),
                "Queries stored for every connection are removed. The database is left untouched."
                    .to_string(),
            )
        };
        window.open_alert_dialog(cx, move |alert, _, _| {
            let this = this.clone();
            let scoped = scoped.clone();
            alert
                .title(title.clone())
                .description(description.clone())
                .confirm()
                .ok_text("Clear history")
                .ok_variant(gpui_kit::component::button::ButtonVariant::Danger)
                .on_ok(move |_, _, cx| {
                    this.update(cx, |ws, cx| {
                        ws.clear_history(scoped.as_deref(), cx);
                    });
                    true
                })
        });
    }

    pub(in super::super) fn clear_history(
        &mut self,
        connection_id: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        let Some(store) = self.history_store.clone() else {
            return;
        };
        if let Err(err) = store.clear(connection_id) {
            self.note_history_write_failure(&err.to_string(), cx);
            return;
        }
        self.refresh_history(cx);
    }

    pub(in super::super) fn turn_on_history(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.history_scope_connection_id() else {
            return;
        };
        if let Some(conn) = self.connections.iter_mut().find(|c| c.id == id) {
            conn.record_history = true;
        }
        if let Err(err) = self.store.save(&self.connections) {
            if let Some(view) = self.active_view_mut() {
                view.error = Some(err.to_string());
            }
        }
        self.refresh_history(cx);
        cx.notify();
    }

    pub(in super::super) fn note_history_write_failure(
        &mut self,
        message: &str,
        cx: &mut Context<Self>,
    ) {
        if !self.history_write_failed_once {
            self.history_write_failed_once = true;
            if let Some(view) = self.active_view_mut() {
                view.status = "Query history is not being saved.".into();
            }
        }
        let _ = message;
        cx.notify();
    }

    pub(in super::super) fn record_finished_sql(
        &mut self,
        tab_id: sela_core::TabId,
        sql: &str,
        outcome: HistoryOutcome,
        rows_label: String,
        error_message: String,
        cx: &mut Context<Self>,
    ) {
        if !sela_core::should_record_history_sql(sql) {
            return;
        }
        let Some(tab) = self.tabs.get(tab_id) else {
            return;
        };
        let connection_id = tab.connection_id.clone();
        if connection_id.is_empty() {
            return;
        }
        let Some(conn) = self.connections.iter().find(|c| c.id == connection_id) else {
            return;
        };
        if !conn.record_history {
            return;
        }
        let Some(store) = self.history_store.clone() else {
            return;
        };
        let duration_ms = self
            .views
            .get(&tab_id)
            .and_then(|v| v.query_started)
            .map(|t| t.elapsed().as_millis() as u64)
            .unwrap_or(0);
        let database = match conn.engine {
            sela_core::Engine::Sqlite => conn.file_path.clone(),
            _ => conn.database.clone(),
        };
        let entry = HistoryEntry {
            id: HistoryStore::new_entry_id(),
            connection_id,
            connection_name: conn.name.clone(),
            database,
            sql: sql.to_string(),
            started_at: Utc::now() - chrono::Duration::milliseconds(duration_ms as i64),
            duration_ms,
            rows_label,
            outcome,
            error_message,
        };
        let open = self.history_open;
        cx.spawn(async move |this, async_cx| {
            let result = async_cx
                .background_executor()
                .spawn(async move { store.insert(&entry) })
                .await;
            this.update(async_cx, |ws, cx| {
                if let Err(err) = result {
                    ws.note_history_write_failure(&err.to_string(), cx);
                } else if open {
                    ws.refresh_history(cx);
                }
            })
            .ok();
        })
        .detach();
    }
}
