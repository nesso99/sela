//! Read-only Structure pane for the open table (columns, indexes, FKs, DDL).
//!
//! Cache and pane mode live on [`super::tab_view::TabView`]. Loads go through
//! [`Workspace::spawn_work`] with the tab id so closing the tab cancels the
//! request and a late completion never paints another tab.

use std::collections::HashMap;

use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme};
use gpui_kit::prelude::*;
use gpui_kit::{div, px, Context, IntoElement};

use sela_core::{ColumnStructure, ForeignKeyInfo, IndexInfo, TabId, TableStructure};

use super::{GridSource, TablePane, Workspace};

impl Workspace {
    pub(super) fn table_open(&self) -> bool {
        self.active_view()
            .is_some_and(|view| matches!(view.source, GridSource::Table { .. }))
    }

    pub(super) fn show_table_data(&mut self, cx: &mut Context<Self>) {
        if !self.table_open() {
            return;
        }
        let Some(view) = self.active_view_mut() else {
            return;
        };
        if view.table_pane != TablePane::Data {
            view.table_pane = TablePane::Data;
            cx.notify();
        }
    }

    pub(super) fn show_table_structure(&mut self, cx: &mut Context<Self>) {
        let Some(tab_id) = self.tabs.active().map(|tab| tab.id) else {
            return;
        };
        let Some(view) = self.views.get(&tab_id) else {
            return;
        };
        let GridSource::Table { schema, table, .. } = &view.source else {
            return;
        };
        let schema = schema.clone();
        let table = table.clone();
        let key = structure_key(&schema, &table);
        let cached = view.structure_cache.contains_key(&key);
        if let Some(view) = self.view_mut(tab_id) {
            view.table_pane = TablePane::Structure;
            if cached {
                view.status = format!("Structure · {schema}.{table}");
                cx.notify();
                return;
            }
        }
        self.load_structure(tab_id, schema, table, false, cx);
    }

    pub(super) fn refresh_structure(&mut self, cx: &mut Context<Self>) {
        let Some(tab_id) = self.tabs.active().map(|tab| tab.id) else {
            return;
        };
        let Some(view) = self.views.get(&tab_id) else {
            return;
        };
        let GridSource::Table { schema, table, .. } = &view.source else {
            return;
        };
        let schema = schema.clone();
        let table = table.clone();
        if let Some(view) = self.view_mut(tab_id) {
            view.structure_cache.remove(&structure_key(&schema, &table));
            view.table_pane = TablePane::Structure;
        }
        self.load_structure(tab_id, schema, table, true, cx);
    }

    pub(super) fn clear_all_structure_caches(&mut self) {
        for view in self.views.values_mut() {
            view.clear_structure();
        }
    }

    fn load_structure(
        &mut self,
        tab_id: TabId,
        schema: String,
        table: String,
        force: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.session.clone() else {
            if let Some(view) = self.view_mut(tab_id) {
                view.structure_error = Some("Connect to a database first.".into());
            }
            cx.notify();
            return;
        };
        let key = structure_key(&schema, &table);
        if !force {
            if let Some(view) = self.views.get(&tab_id) {
                if view.structure_cache.contains_key(&key) {
                    return;
                }
            }
        }
        if let Some(view) = self.view_mut(tab_id) {
            view.structure_error = None;
            view.status = format!("Loading structure · {schema}.{table}…");
        }
        let expected_schema = schema.clone();
        let expected_table = table.clone();
        self.spawn_work(
            Some(tab_id),
            cx,
            move |_| async move { session.describe_table(&schema, &table).await },
            move |this, result, cx| {
                // Tab closed → spawn_work already returned before calling us.
                // Same tab switched table → ignore. Never paint into another tab:
                // we only mutate `views[tab_id]`.
                let still_current = this.views.get(&tab_id).is_some_and(|view| {
                    matches!(
                        &view.source,
                        GridSource::Table { schema, table, .. }
                            if schema == &expected_schema && table == &expected_table
                    )
                });
                match result {
                    Ok(structure) => {
                        let key = structure_key(&structure.schema, &structure.table);
                        if let Some(view) = this.view_mut(tab_id) {
                            view.structure_cache.insert(key, structure);
                            if still_current {
                                view.status =
                                    format!("Structure · {expected_schema}.{expected_table}");
                                view.structure_error = None;
                            }
                        }
                        if still_current {
                            cx.notify();
                        }
                    }
                    Err(err) => {
                        if still_current {
                            if let Some(view) = this.view_mut(tab_id) {
                                view.structure_error = Some(err.to_string());
                                view.status = "Structure failed".into();
                            }
                            cx.notify();
                        }
                    }
                }
            },
        );
    }

    pub(super) fn structure_area(&self, cx: &Context<Self>) -> impl IntoElement {
        let view = self.active_view();
        div()
            .flex_1()
            .min_h(px(0.))
            .overflow_hidden()
            .child(match view.map(|v| &v.source) {
                Some(GridSource::Table { schema, table, .. }) => {
                    let key = structure_key(schema, table);
                    let view = view.expect("matched Table source");
                    if let Some(err) = &view.structure_error {
                        status_copy(err, cx).into_any_element()
                    } else if let Some(structure) = view.structure_cache.get(&key) {
                        render_structure(structure, cx).into_any_element()
                    } else if view.busy {
                        status_copy("Loading structure…", cx).into_any_element()
                    } else {
                        status_copy("Switch to Structure to load columns, indexes, and DDL.", cx)
                            .into_any_element()
                    }
                }
                _ => status_copy("Open a table to view its structure.", cx).into_any_element(),
            })
    }
}

fn structure_key(schema: &str, table: &str) -> String {
    format!("{schema}\0{table}")
}

fn status_copy(text: &str, cx: &Context<Workspace>) -> impl IntoElement {
    div()
        .size_full()
        .p_4()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(text.to_string())
}

fn render_structure(structure: &TableStructure, cx: &Context<Workspace>) -> impl IntoElement {
    let mono = cx.theme().mono_font_family.clone();
    div().size_full().overflow_y_scrollbar().p_3().child(
        v_flex()
            .gap_4()
            .child(section_title("Columns"))
            .child(columns_block(&structure.columns, cx))
            .child(section_title("Indexes"))
            .child(indexes_block(&structure.indexes, cx))
            .child(section_title("Foreign keys"))
            .child(foreign_keys_block(&structure.foreign_keys, cx))
            .child(section_title("DDL"))
            .child(
                div()
                    .p_3()
                    .rounded(px(6.))
                    .bg(cx.theme().muted)
                    .font_family(mono)
                    .text_xs()
                    .child(structure.ddl.clone()),
            ),
    )
}

fn section_title(label: &str) -> impl IntoElement {
    div().text_sm().child(label.to_string())
}

fn columns_block(columns: &[ColumnStructure], cx: &Context<Workspace>) -> impl IntoElement {
    let mut rows = v_flex().gap_1().child(header_row(
        &["Name", "Type", "Nullable", "Default", "Comment"],
        cx,
    ));
    if columns.is_empty() {
        rows = rows.child(empty_note("No columns.", cx));
    } else {
        for column in columns {
            rows = rows.child(data_row(
                &[
                    column.name.clone(),
                    column.type_name.clone(),
                    if column.nullable {
                        "YES".into()
                    } else {
                        "NO".into()
                    },
                    column.default.clone().unwrap_or_default(),
                    column.comment.clone().unwrap_or_default(),
                ],
                cx,
            ));
        }
    }
    rows
}

fn indexes_block(indexes: &[IndexInfo], cx: &Context<Workspace>) -> impl IntoElement {
    let mut rows = v_flex()
        .gap_1()
        .child(header_row(&["Name", "Columns", "Unique", "Primary"], cx));
    if indexes.is_empty() {
        rows = rows.child(empty_note("No indexes.", cx));
    } else {
        for index in indexes {
            rows = rows.child(data_row(
                &[
                    index.name.clone(),
                    index.columns.join(", "),
                    if index.unique {
                        "YES".into()
                    } else {
                        "NO".into()
                    },
                    if index.primary {
                        "YES".into()
                    } else {
                        "NO".into()
                    },
                ],
                cx,
            ));
        }
    }
    rows
}

fn foreign_keys_block(fks: &[ForeignKeyInfo], cx: &Context<Workspace>) -> impl IntoElement {
    let mut rows = v_flex().gap_1().child(header_row(
        &["Name", "Columns", "References", "On update", "On delete"],
        cx,
    ));
    if fks.is_empty() {
        rows = rows.child(empty_note("No foreign keys.", cx));
    } else {
        for fk in fks {
            let referenced = match &fk.ref_schema {
                Some(schema) => {
                    format!("{schema}.{} ({})", fk.ref_table, fk.ref_columns.join(", "))
                }
                None => format!("{} ({})", fk.ref_table, fk.ref_columns.join(", ")),
            };
            rows = rows.child(data_row(
                &[
                    fk.name.clone(),
                    fk.columns.join(", "),
                    referenced,
                    fk.on_update.clone().unwrap_or_default(),
                    fk.on_delete.clone().unwrap_or_default(),
                ],
                cx,
            ));
        }
    }
    rows
}

fn empty_note(text: &str, cx: &Context<Workspace>) -> impl IntoElement {
    div()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(text.to_string())
}

fn header_row(labels: &[&str], cx: &Context<Workspace>) -> impl IntoElement {
    h_flex()
        .gap_2()
        .px_2()
        .py_1()
        .border_b_1()
        .border_color(cx.theme().border)
        .children(labels.iter().map(|label| {
            div()
                .flex_1()
                .min_w(px(80.))
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child((*label).to_string())
        }))
}

fn data_row(values: &[String], cx: &Context<Workspace>) -> impl IntoElement {
    h_flex()
        .gap_2()
        .px_2()
        .py_1()
        .border_b_1()
        .border_color(cx.theme().border)
        .children(values.iter().map(|value| {
            div()
                .flex_1()
                .min_w(px(80.))
                .text_xs()
                .font_family(cx.theme().mono_font_family.clone())
                .child(value.clone())
        }))
}

/// Cache type shared with [`super::tab_view::TabView`].
pub(super) type StructureCache = HashMap<String, TableStructure>;
