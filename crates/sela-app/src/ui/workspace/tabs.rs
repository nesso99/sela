//! Tab bar UI and open / close / switch / persist lifecycle.
//!
//! Closing a dirty tab will prompt in M2; [`Workspace::request_close_tab`] is
//! the hook. Dedicated per-tab DB connections are a follow-up — tabs share the
//! workspace pool today and remember their `connection_id` for restore.

use std::collections::HashMap;

use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{h_flex, ActiveTheme, IconName, Sizable as _};
use gpui_kit::prelude::*;
use gpui_kit::{div, Context, IntoElement, Window};

use sela_core::{TabId, TabKind, TabList};

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
            .map(|tab| (tab.id, tab.kind.title()))
            .collect();

        h_flex()
            .w_full()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().muted)
            .child(
                TabBar::new("sela-tabs")
                    .w_full()
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
                                .on_click(move |_, _, cx| {
                                    this.update(cx, |workspace, cx| {
                                        workspace.request_close_tab(close_id, cx);
                                    });
                                })
                        })
                    }))
                    .on_click({
                        let this = this.clone();
                        move |index, _, cx| {
                            this.update(cx, |workspace, cx| {
                                workspace.activate_tab_index(*index, cx);
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
    }

    pub(super) fn new_query_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let connection_id = self.active_id.clone().unwrap_or_default();
        let id = self.tabs.open_query(connection_id, "");
        self.attach_view(id, "", window, cx);
        self.persist_tabs(cx);
        cx.notify();
    }

    pub(super) fn activate_tab_index(&mut self, index: usize, cx: &mut Context<Self>) {
        if !self.tabs.activate_index(index) {
            return;
        }
        self.on_tab_activated(cx);
        self.persist_tabs(cx);
        cx.notify();
    }

    pub(super) fn activate_tab_number(&mut self, number: usize, cx: &mut Context<Self>) {
        // Cmd/Ctrl+1 → first tab (index 0).
        if number == 0 {
            return;
        }
        self.activate_tab_index(number - 1, cx);
    }

    /// M2 hook: if the tab is dirty, prompt before closing. Editing does not
    /// exist yet, so this always closes today.
    pub(super) fn request_close_tab(&mut self, id: TabId, cx: &mut Context<Self>) {
        if self.tabs.should_prompt_before_close(id) {
            // M2: open an alert dialog ("Discard unsaved changes?") then call
            // `close_tab` on confirm. Until cell editing lands, dirty is never set.
            self.close_tab(id, cx);
            return;
        }
        self.close_tab(id, cx);
    }

    pub(super) fn close_active_tab(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.tabs.active().map(|tab| tab.id) else {
            return;
        };
        self.request_close_tab(id, cx);
    }

    pub(super) fn close_tab(&mut self, id: TabId, cx: &mut Context<Self>) {
        let Some(_removed) = self.tabs.close(id) else {
            return;
        };
        self.views.remove(&id);
        self.on_tab_activated(cx);
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
            if let Some(view) = self.views.get_mut(&id) {
                view.source = GridSource::Table {
                    schema: schema.clone(),
                    table: table.clone(),
                };
                view.table_pane = super::TablePane::Data;
                view.structure_error = None;
            }
            self.persist_tabs(cx);
            self.load_source_for(id, cx);
        } else {
            self.on_tab_activated(cx);
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
        self.views.insert(id, view);
        let subscription = cx.observe(&grid, move |this, grid, cx| {
            let pending_pages = !grid.read(cx).delegate().pending_pages.is_empty();
            let pending_sort = grid.read(cx).delegate().pending_sort;
            if pending_pages {
                this.fetch_pending_pages_for(id, cx);
            }
            if let Some((col_ix, shift)) = pending_sort {
                grid.update(cx, |state, _| {
                    state.delegate_mut().pending_sort = None;
                });
                this.apply_column_sort(id, col_ix, shift, cx);
            }
        });
        let selection = cx.subscribe_in(
            &grid,
            window,
            move |_this, grid, event: &gpui_kit::component::table::TableEvent, window, cx| {
                use gpui_kit::component::table::TableEvent;
                match event {
                    TableEvent::SelectCell(row, col) => {
                        let shift = window.modifiers().shift;
                        grid.update(cx, |state, _| {
                            state.delegate_mut().note_cell_focus(*row, *col, shift);
                        });
                    }
                    TableEvent::SelectRow(_) | TableEvent::SelectColumn(_) => {
                        grid.update(cx, |state, _| {
                            state.delegate_mut().clear_selection_range();
                        });
                    }
                    TableEvent::ClearSelection => {
                        grid.update(cx, |state, _| {
                            state.delegate_mut().clear_selection_range();
                        });
                    }
                    _ => {}
                }
            },
        );
        self._subscriptions.push(subscription);
        self._subscriptions.push(selection);
    }

    pub(super) fn on_tab_activated(&mut self, cx: &mut Context<Self>) {
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
            self.connect_id(connection_id, cx);
            return;
        }
        // Reload a restored table tab that has not fetched yet.
        if let TabKind::Table { schema, table } = kind {
            let needs_load = self.views.get(&tab_id).is_some_and(|view| {
                view.page_len == 0
                    && !view.busy
                    && self.session.is_some()
                    && matches!(&view.source, GridSource::Idle | GridSource::Table { .. })
            });
            if let Some(view) = self.views.get_mut(&tab_id) {
                view.source = GridSource::Table {
                    schema: schema.clone(),
                    table: table.clone(),
                };
            }
            if needs_load {
                self.load_source_for(tab_id, cx);
            }
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
                    view.source = GridSource::Table { schema, table };
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
