//! Connections / catalog tree sidebar.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::tree::{Tree, TreeItem};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Disableable as _, Sizable as _};
use gpui_kit::prelude::*;
use gpui_kit::{div, px, Context, IntoElement};

use sela_config::SavedConnection;
use sela_core::{sidebar_connection_layout, SidebarConnectionNode};

use super::tree::{
    conn_key, group_key, is_group_key, render_tree_entry, schema_key, table_item,
    ConnDisplayLookup, SEP,
};
use super::Workspace;

impl Workspace {
    pub(super) fn rebuild_tree(&mut self, cx: &mut Context<Self>) {
        let items = self.tree_items();
        self.tree.update(cx, |tree, cx| {
            tree.set_items(items, cx);
        });
        self.last_selection = None;
    }

    fn conn_display_lookup(&self) -> ConnDisplayLookup {
        self.connections
            .iter()
            .map(|c| (c.id.clone(), (c.color, c.label.clone())))
            .collect()
    }

    fn tree_items(&self) -> Vec<TreeItem> {
        sidebar_connection_layout(&self.connections)
            .into_iter()
            .flat_map(|node| match node {
                SidebarConnectionNode::Connection(connection) => {
                    vec![self.connection_item(connection)]
                }
                SidebarConnectionNode::Group { name, members } => {
                    let key = group_key(name);
                    let children = members
                        .into_iter()
                        .map(|connection| self.connection_item(connection));
                    vec![TreeItem::new(key.clone(), name.to_string())
                        .children(children)
                        .expanded(self.expanded.contains(&key))]
                }
            })
            .collect()
    }

    fn connection_item(&self, connection: &SavedConnection) -> TreeItem {
        let key = conn_key(&connection.id);
        let expanded = self.expanded.contains(&key);
        let active = self.active_id.as_deref() == Some(connection.id.as_str());
        let mark = if active && self.session.is_some() {
            "●"
        } else {
            "○"
        };
        let label = format!("{mark}  {}  {}", connection.engine.short(), connection.name);
        let mut item = TreeItem::new(key.clone(), label);
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
            item = item.children(children).expanded(expanded);
        }
        item
    }

    pub(super) fn sidebar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.entity();
        // Group rows clear selected_connection_id; keep Edit/Dup/Delete off even
        // when an active session connection would otherwise be the fallback.
        let has_edit_target = !self.last_selection.as_deref().is_some_and(is_group_key)
            && self.selected_or_active_connection().is_some();
        v_flex()
            .w(px(292.))
            .h_full()
            .flex_shrink_0()
            .border_r_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().sidebar)
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
            .child({
                let lookup = self.conn_display_lookup();
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .child(
                        Tree::new(&self.tree, move |ix, entry, selected, window, cx| {
                            render_tree_entry(ix, entry, selected, window, cx, &lookup)
                        })
                        .size_full(),
                    )
            })
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
                            .disabled(!has_edit_target)
                            .on_click(move |_, window, cx| {
                                this.update(cx, |workspace, cx| {
                                    let existing = workspace.selected_or_active_connection().cloned();
                                    if let Some(existing) = existing {
                                        workspace.open_form(Some(existing), window, cx);
                                    }
                                });
                            })
                    })
                    .child({
                        let this = this.clone();
                        Button::new("duplicate-connection")
                            .xsmall()
                            .ghost()
                            .label("Duplicate")
                            .disabled(!has_edit_target)
                            .on_click(move |_, _, cx| {
                                this.update(cx, |workspace, cx| {
                                    workspace.duplicate_selected_connection(cx);
                                });
                            })
                    })
                    .child({
                        let this = this.clone();
                        Button::new("delete-connection")
                            .xsmall()
                            .ghost()
                            .label("Delete")
                            .disabled(!has_edit_target)
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
                            .on_click(move |_, window, cx| {
                                this.update(cx, |workspace, cx| {
                                    workspace.request_disconnect(window, cx);
                                });
                            })
                    }),
            )
    }
}
