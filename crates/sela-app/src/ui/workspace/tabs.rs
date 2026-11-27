//! Tab bar UI and open / close / switch / persist lifecycle.
//!
//! Closing a dirty tab will prompt in M2; [`Workspace::request_close_tab`] is
//! the hook. Dedicated per-tab DB connections are a follow-up — tabs share the
//! workspace pool today and remember their `connection_id` for restore.

use std::collections::HashMap;

use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::{h_flex, ActiveTheme, IconName, Sizable as _};
use gpui_kit::prelude::*;
use gpui_kit::{div, px, Context, IntoElement, Window};

use sela_core::{ConnectionColor, TabId, TabKind, TabList};

use super::super::theme::SelaPalette;
use super::tab_view::TabView;
use super::{GridSource, Workspace};

impl Workspace {
    pub(super) fn tab_bar(&self, cx: &Context<Self>) -> impl IntoElement {
        let this = cx.entity();
        let selected = self.tabs.active_index().unwrap_or(0);
        let items: Vec<(TabId, String)> = self
            .tabs
            .tabs()
            .iter()
            .map(|tab| {
                let mut title = tab.kind.title();
                if tab.dirty {
                    title.push_str(" •");
                }
                (tab.id, title)
            })
            .collect();

        let conn_color = self
            .active_connection()
            .map(|c| c.color)
            .unwrap_or(ConnectionColor::None);
        let tab_strip = SelaPalette::for_mode(cx.theme().mode).connection_color(conn_color);
        h_flex()
            .w_full()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().muted)
            .relative()
            .child(
                TabBar::new("sela-tabs")
                    .w_full()
                    .underline()
                    .selected_index(selected)
                    .when(items.is_empty(), |bar| {
                        bar.child(Tab::new().label("No tabs").disabled(true))
                    })
                    .children(items.into_iter().enumerate().map(|(ix, (id, title))| {
                        let close_id = id;
                        Tab::new().label(title).suffix({
                            let this = this.clone();
                            Button::new(("close-tab", ix))
                                .icon(IconName::Close)
                                .xsmall()
                                .custom(
                                    ButtonCustomVariant::new(cx)
                                        .foreground(cx.theme().secondary_foreground)
                                        .hover(*cx.theme().tokens.secondary_hover)
                                        .active(*cx.theme().tokens.secondary_active),
                                )
                                .tab_stop(false)
                                .tooltip("Close tab")
                                .on_click(move |_, window, cx| {
                                    this.update(cx, |workspace, cx| {
                                        workspace.request_close_tab(close_id, window, cx);
                                    });
                                })
                        })
                    }))
                    .on_click({
                        let this = this.clone();
                        move |index, window, cx| {
                            this.update(cx, |workspace, cx| {
                                workspace.activate_tab_index(*index, Some(window), cx);
                            });
                        }
                    })
                    .suffix({
                        let this = this;
                        Button::new("new-query-tab")
                            .icon(IconName::Plus)
                            .xsmall()
                            .ghost()
                            .tooltip("New query tab (Ctrl+T / ⌘+T)")
                            .on_click(move |_, window, cx| {
                                this.update(cx, |workspace, cx| {
                                    workspace.new_query_tab(window, cx);
                                });
                            })
                    }),
            )
            .when_some(tab_strip, |row, color| {
                // 2px bottom edge in the connection color (every tab shares one connection).
                row.child(
                    div()
                        .absolute()
                        .bottom_0()
                        .left_0()
                        .right_0()
                        .h(px(2.))
                        .bg(color),
                )
            })
    }

    pub(super) fn new_query_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let connection_id = self.active_id.clone().unwrap_or_default();
        let id = self.tabs.open_query(connection_id, "");
        self.attach_view(id, "", window, cx);
        self.persist_tabs(cx);
        cx.notify();
    }

    pub(super) fn activate_tab_index(
        &mut self,
        index: usize,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        if !self.tabs.activate_index(index) {
            return;
        }
        self.on_tab_activated(window, cx);
        self.persist_tabs(cx);
        cx.notify();
    }

    pub(super) fn activate_tab_number(
        &mut self,
        number: usize,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        // Cmd/Ctrl+1 → first tab (index 0).
        if number == 0 {
            return;
        }
        self.activate_tab_index(number - 1, window, cx);
    }

    /// Close a tab; confirm when it has pending grid edits (08-preview-commit.md).
    pub(super) fn request_close_tab(
        &mut self,
        id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.tabs.should_prompt_before_close(id) {
            self.show_close_tab_confirm(id, window, cx);
            return;
        }
        self.close_tab(id, cx);
    }

    fn show_close_tab_confirm(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        if self.close_tab_confirm_open {
            return;
        }
        let Some(view) = self.views.get(&id) else {
            self.close_tab(id, cx);
            return;
        };
        let n = view
            .edit
            .as_ref()
            .map(|e| e.pending_counts().total())
            .unwrap_or(0);
        let table = view
            .edit
            .as_ref()
            .map(|e| e.schema_table_label())
            .unwrap_or_else(|| view.source_label());
        self.close_tab_confirm_open = true;
        let this = cx.entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let this_ok = this.clone();
            let this_cancel = this.clone();
            let this_close = this.clone();
            alert
                .title(format!("Close {table}?"))
                .description(format!(
                    "{n} pending changes will be discarded. The database is not changed."
                ))
                .on_ok(move |_, _, cx| {
                    this_ok.update(cx, |workspace, _| {
                        workspace.close_tab_confirm_open = false;
                    });
                    true
                })
                .on_cancel(move |_, _, cx| {
                    this_cancel.update(cx, |workspace, _| {
                        workspace.close_tab_confirm_open = false;
                    });
                    true
                })
                .footer(
                    gpui_kit::component::dialog::DialogFooter::new()
                        .child(
                            gpui_kit::component::button::Button::new("close-tab-cancel")
                                .label("Cancel")
                                .primary()
                                .on_click({
                                    let this = this.clone();
                                    move |_, window, cx| {
                                        this.update(cx, |workspace, _| {
                                            workspace.close_tab_confirm_open = false;
                                        });
                                        window.close_dialog(cx);
                                    }
                                }),
                        )
                        .child(
                            gpui_kit::component::button::Button::new("close-tab-close")
                                .label("Close tab")
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                    this_close.update(cx, |workspace, cx| {
                                        workspace.close_tab_confirm_open = false;
                                        workspace.close_tab(id, cx);
                                    });
                                }),
                        ),
                )
        });
    }

    pub(super) fn close_active_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.tabs.active().map(|tab| tab.id) else {
            return;
        };
        self.request_close_tab(id, window, cx);
    }

    pub(super) fn close_tab(&mut self, id: TabId, cx: &mut Context<Self>) {
        let Some(_removed) = self.tabs.close(id) else {
            return;
        };
        self.views.remove(&id);
        self.on_tab_activated(None, cx);
        self.persist_tabs(cx);
        cx.notify();
    }

    /// Open or focus a table tab for the active connection.
    pub(super) fn open_table_tab(
        &mut self,
        schema: String,
        table: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(connection_id) = self.active_id.clone() else {
            return;
        };
        let (id, created) = self
            .tabs
            .open_table(connection_id, schema.clone(), table.clone());
        if created {
            self.attach_view(id, "", window, cx);
            let kind = self.relation_kind_for(&schema, &table);
            if let Some(view) = self.views.get_mut(&id) {
                view.source = GridSource::Table {
                    schema: schema.clone(),
                    table: table.clone(),
                    kind,
                };
                view.table_pane = super::TablePane::Data;
                view.structure_error = None;
            }
            self.persist_tabs(cx);
            self.load_source_for(id, cx);
            self.ensure_edit_session_loading(id, cx);
        } else {
            self.on_tab_activated(Some(window), cx);
            self.persist_tabs(cx);
            cx.notify();
        }
    }

    pub(super) fn attach_view(
        &mut self,
        id: TabId,
        sql_text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = TabView::new(window, cx, sql_text);
        let grid = view.grid.clone();
        let sql = view.sql.clone();
        self.install_sql_completions(&sql, cx);
        self.views.insert(id, view);
        let subscription = cx.observe_in(&grid, window, move |this, grid, window, cx| {
            let pending_pages = !grid.read(cx).delegate().pending_pages.is_empty();
            let pending_sort = grid.read(cx).delegate().pending_sort;
            let pending_edit = grid.read(cx).delegate().pending_edit_cell;
            if pending_pages {
                this.fetch_pending_pages_for(id, cx);
            }
            if let Some((col_ix, shift)) = pending_sort {
                grid.update(cx, |state, _| {
                    state.delegate_mut().pending_sort = None;
                });
                this.apply_column_sort(id, col_ix, shift, cx);
            }
            if pending_edit.is_some() {
                grid.update(cx, |state, _| {
                    state.delegate_mut().pending_edit_cell = None;
                });
                if this.tabs.active().map(|t| t.id) == Some(id) {
                    this.begin_cell_edit(window, cx);
                }
            }
        });
        let selection = cx.subscribe_in(
            &grid,
            window,
            move |_this, grid, event: &gpui_kit::component::table::TableEvent, window, cx| {
                use gpui_kit::component::table::TableEvent;
                match event {
                    TableEvent::SelectCell(row, col) => {
                        // Prefer Shift captured on cell mouse-down / Shift+arrow
                        // before set_selected_cell; fall back to window modifiers.
                        // Select-All sets suppress so a deferred SelectCell from
                        // set_selected_cell cannot collapse the full range.
                        let window_shift = window.modifiers().shift;
                        grid.update(cx, |state, cx| {
                            state
                                .delegate_mut()
                                .apply_select_cell_event(*row, *col, window_shift);
                            // Re-paint so selection_rect wash updates for the range.
                            state.refresh(cx);
                        });
                        // Close inspect when the focused cell changes.
                        if let Some(view) = _this.views.get_mut(&id) {
                            view.cell_inspect = None;
                        }
                    }
                    TableEvent::SelectRow(_) | TableEvent::SelectColumn(_) => {
                        grid.update(cx, |state, cx| {
                            state.delegate_mut().clear_selection_range();
                            state.refresh(cx);
                        });
                        if let Some(view) = _this.views.get_mut(&id) {
                            view.cell_inspect = None;
                        }
                    }
                    TableEvent::ClearSelection => {
                        grid.update(cx, |state, cx| {
                            state.delegate_mut().clear_selection_range();
                            state.refresh(cx);
                        });
                        if let Some(view) = _this.views.get_mut(&id) {
                            view.cell_inspect = None;
                        }
                    }
                    TableEvent::ColumnWidthsChanged(widths) => {
                        grid.update(cx, |state, _| {
                            state.delegate_mut().apply_column_widths(widths);
                        });
                    }
                    _ => {}
                }
            },
        );
        self._subscriptions.push(subscription);
        self._subscriptions.push(selection);
    }

    pub(super) fn on_tab_activated(&mut self, window: Option<&mut Window>, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.active() else {
            return;
        };
        let connection_id = tab.connection_id.clone();
        let tab_id = tab.id;
        let kind = tab.kind.clone();
        // Retarget "This connection" scope when the active tab changes.
        if self.history_open
            && matches!(
                self.history_scope,
                super::history::HistoryScope::ThisConnection
            )
        {
            if connection_id.is_empty() {
                self.history_scope = super::history::HistoryScope::All;
            }
            self.refresh_history(cx);
        }
        if !connection_id.is_empty() && self.active_id.as_deref() != Some(connection_id.as_str()) {
            self.connect_id(connection_id, window, cx);
            return;
        }
        // Reload a restored table tab that has not fetched yet, or a table tab
        // whose grid still holds a SQL result after reconnect (#98).
        if let TabKind::Table { schema, table } = kind {
            let has_session = self.session.is_some();
            let needs_load = self.views.get(&tab_id).is_some_and(|view| {
                table_tab_needs_load_on_activate(
                    &view.source,
                    view.page_len,
                    view.busy,
                    has_session,
                )
            });
            let kind = self.relation_kind_for(&schema, &table);
            if let Some(view) = self.views.get_mut(&tab_id) {
                view.source = GridSource::Table {
                    schema: schema.clone(),
                    table: table.clone(),
                    kind,
                };
            }
            if needs_load {
                self.load_source_for(tab_id, cx);
            }
            self.ensure_edit_session_loading(tab_id, cx);
        }
    }

    pub(super) fn persist_tabs(&mut self, cx: &Context<Self>) {
        let ids: Vec<TabId> = self.tabs.tabs().iter().map(|tab| tab.id).collect();
        for id in ids {
            let is_query = matches!(
                self.tabs.get(id).map(|tab| &tab.kind),
                Some(TabKind::Query { .. })
            );
            if !is_query {
                continue;
            }
            let sql = self
                .views
                .get(&id)
                .map(|view| view.sql.read(cx).value().to_string())
                .unwrap_or_default();
            if let Some(model) = self.tabs.get_mut(id) {
                model.kind = TabKind::Query { sql };
            }
        }
        let mut saved = self.tabs.to_persisted();
        for (ix, tab) in self.tabs.tabs().iter().enumerate() {
            if let Some(view) = self.views.get(&tab.id) {
                if let Some(item) = saved.tabs.get_mut(ix) {
                    item.filters = view.filters.clone();
                    item.sorts = view.sorts.clone();
                    item.filter_bar_open = view.filter_bar_open;
                }
            }
        }
        if let Err(err) = self.tab_store.save(&saved) {
            if let Some(view) = self.active_view_mut() {
                view.error = Some(err.to_string());
            }
        }
    }

    pub(super) fn restore_tabs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let saved = self.tab_store.load().unwrap_or_default();
        let filter_state: Vec<_> = saved
            .tabs
            .iter()
            .map(|t| (t.filters.clone(), t.sorts.clone(), t.filter_bar_open))
            .collect();
        self.tabs = TabList::from_persisted(saved);
        self.views = HashMap::new();
        let snapshot: Vec<(TabId, TabKind)> = self
            .tabs
            .tabs()
            .iter()
            .map(|tab| (tab.id, tab.kind.clone()))
            .collect();
        for (ix, (id, kind)) in snapshot.into_iter().enumerate() {
            let sql = match &kind {
                TabKind::Query { sql } => sql.as_str(),
                TabKind::Table { .. } => "",
            };
            self.attach_view(id, sql, window, cx);
            let (filters, sorts, filter_bar_open) =
                filter_state.get(ix).cloned().unwrap_or_default();
            if let TabKind::Table { schema, table } = kind {
                let seeds = if filter_bar_open || !filters.is_empty() {
                    if filters.is_empty() {
                        vec![sela_core::FilterCondition::new(
                            "",
                            sela_core::FilterOp::Eq,
                            "",
                        )]
                    } else {
                        filters.clone()
                    }
                } else {
                    Vec::new()
                };
                let mut rows = Vec::new();
                for seed in &seeds {
                    rows.push(super::filter_bar::FilterRowUi::new(window, cx, seed));
                }
                if let Some(view) = self.views.get_mut(&id) {
                    view.source = GridSource::Table {
                        schema,
                        table,
                        kind: None,
                    };
                    view.filters = filters;
                    view.sorts = sorts;
                    view.filter_bar_open = filter_bar_open;
                    view.filter_rows = rows;
                    view.status = "Reconnect to load this table.".into();
                }
            }
        }
    }

    pub(super) fn active_view(&self) -> Option<&TabView> {
        let id = self.tabs.active()?.id;
        self.views.get(&id)
    }

    pub(super) fn active_view_mut(&mut self) -> Option<&mut TabView> {
        let id = self.tabs.active()?.id;
        self.views.get_mut(&id)
    }

    pub(super) fn view_mut(&mut self, id: TabId) -> Option<&mut TabView> {
        self.views.get_mut(&id)
    }

    pub(super) fn empty_content(&self, cx: &Context<Self>) -> impl IntoElement {
        div()
            .flex_1()
            .flex()
            .items_center()
            .justify_center()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child("Open a table from the sidebar, or press Ctrl+T / ⌘+T for a query tab.")
    }
}

/// Whether activating a `TabKind::Table` should call `load_source_for`.
///
/// Restored empty tabs (`Idle` / `Table` with `page_len == 0`) need a first
/// fetch. A table tab that last ran SQL still has `GridSource::Sql` and a
/// non-empty grid; resetting the source to `Table` without reloading leaves a
/// stale SQL result under the table title (#98).
pub(crate) fn table_tab_needs_load_on_activate(
    source: &GridSource,
    page_len: usize,
    busy: bool,
    has_session: bool,
) -> bool {
    if busy || !has_session {
        return false;
    }
    match source {
        GridSource::Idle | GridSource::Table { .. } => page_len == 0,
        GridSource::Sql(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::{table_tab_needs_load_on_activate, GridSource};
    use sela_core::RelationKind;

    fn table_source() -> GridSource {
        GridSource::Table {
            schema: "public".into(),
            table: "edit_pk".into(),
            kind: Some(RelationKind::Table),
        }
    }

    #[test]
    fn table_tab_reload_when_source_is_sql_after_reconnect() {
        // #98: SELECT in a table tab → source Sql, page_len > 0; Save reconnect
        // activates the tab and must reload, not only flip source back to Table.
        assert!(table_tab_needs_load_on_activate(
            &GridSource::Sql("SELECT 1".into()),
            1,
            false,
            true,
        ));
        // Empty restored table still loads.
        assert!(table_tab_needs_load_on_activate(
            &table_source(),
            0,
            false,
            true,
        ));
        // Already-loaded table with Table source does not reload.
        assert!(!table_tab_needs_load_on_activate(
            &table_source(),
            5,
            false,
            true,
        ));
        // No session / busy → do not load.
        assert!(!table_tab_needs_load_on_activate(
            &GridSource::Sql("SELECT 1".into()),
            1,
            false,
            false,
        ));
        assert!(!table_tab_needs_load_on_activate(
            &GridSource::Sql("SELECT 1".into()),
            1,
            true,
            true,
        ));
    }
}
