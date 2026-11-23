//! Read-only Structure pane for the open table (columns, indexes, FKs, DDL).

use std::collections::HashMap;

use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme};
use gpui_kit::prelude::*;
use gpui_kit::{div, px, Context, IntoElement};

use sela_core::{ColumnStructure, ForeignKeyInfo, IndexInfo, TableStructure};

use super::{GridSource, TablePane, Workspace};

impl Workspace {
    pub(super) fn table_open(&self) -> bool {
        matches!(self.source, GridSource::Table { .. })
    }

    pub(super) fn show_table_data(&mut self, cx: &mut Context<Self>) {
        if !self.table_open() {
            return;
        }
        if self.table_pane != TablePane::Data {
            self.table_pane = TablePane::Data;
            cx.notify();
        }
    }

    pub(super) fn show_table_structure(&mut self, cx: &mut Context<Self>) {
        let GridSource::Table { schema, table } = &self.source else {
            return;
        };
        let schema = schema.clone();
        let table = table.clone();
        self.table_pane = TablePane::Structure;
        let key = structure_key(&schema, &table);
        if self.structure_cache.contains_key(&key) {
            self.status = format!("Structure · {schema}.{table}");
            cx.notify();
            return;
        }
        self.load_structure(schema, table, false, cx);
    }

    pub(super) fn refresh_structure(&mut self, cx: &mut Context<Self>) {
        let GridSource::Table { schema, table } = &self.source else {
            return;
        };
        let schema = schema.clone();
        let table = table.clone();
        self.structure_cache.remove(&structure_key(&schema, &table));
        self.table_pane = TablePane::Structure;
        self.load_structure(schema, table, true, cx);
    }

    pub(super) fn clear_structure_cache(&mut self) {
        self.structure_cache.clear();
        self.structure_error = None;
    }

    fn load_structure(
        &mut self,
        schema: String,
        table: String,
        force: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.session.clone() else {
            self.structure_error = Some("Connect to a database first.".into());
            cx.notify();
            return;
        };
        let key = structure_key(&schema, &table);
        if !force && self.structure_cache.contains_key(&key) {
            return;
        }
        self.structure_error = None;
        self.status = format!("Loading structure · {schema}.{table}…");
        self.spawn_work(
            cx,
            async move { session.describe_table(&schema, &table).await },
            move |this, result, cx| match result {
                Ok(structure) => {
                    let key = structure_key(&structure.schema, &structure.table);
                    this.status = format!("Structure · {}.{}", structure.schema, structure.table);
                    this.structure_error = None;
                    this.structure_cache.insert(key, structure);
                    cx.notify();
                }
                Err(err) => {
                    this.structure_error = Some(err.to_string());
                    this.status = "Structure failed".into();
                    cx.notify();
                }
            },
        );
    }

    pub(super) fn structure_area(&self, cx: &Context<Self>) -> impl IntoElement {
        div()
            .flex_1()
            .min_h(px(0.))
            .overflow_hidden()
            .child(match &self.source {
                GridSource::Table { schema, table } => {
                    let key = structure_key(schema, table);
                    if let Some(err) = &self.structure_error {
                        status_copy(err, cx).into_any_element()
                    } else if let Some(structure) = self.structure_cache.get(&key) {
                        render_structure(structure, cx).into_any_element()
                    } else if self.busy {
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

/// Cache type shared with [`Workspace`].
pub(super) type StructureCache = HashMap<String, TableStructure>;
