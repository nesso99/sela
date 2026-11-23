//! Tree node ids and catalog-tree rendering helpers.

use gpui_kit::component::list::ListItem;
use gpui_kit::component::tree::{TreeEntry, TreeItem};
use gpui_kit::prelude::*;
use gpui_kit::{px, App, Window};

use sela_core::{RelationKind, TableInfo};

pub(super) const SEP: char = '\u{1f}';

pub(super) enum NodeId {
    Connection(String),
    Table { schema: String, table: String },
}

pub(super) fn render_tree_entry(
    _: usize,
    entry: &TreeEntry,
    _: bool,
    _: &mut Window,
    _: &mut App,
) -> ListItem {
    let depth = entry.depth() as f32;
    let marker = if entry.is_folder() {
        if entry.is_expanded() {
            "▾"
        } else {
            "▸"
        }
    } else if entry.item().id.starts_with('t') {
        "·"
    } else {
        ""
    };
    let label = if marker.is_empty() {
        entry.item().label.to_string()
    } else {
        format!("{marker}  {}", entry.item().label)
    };
    ListItem::new(entry.item().id.clone())
        .pl(px(8. + depth * 14.))
        .child(label)
}

pub(super) fn table_item(connection: &str, schema: &str, table: &TableInfo) -> TreeItem {
    let suffix = match table.kind {
        RelationKind::View => "  ·  view",
        RelationKind::Table => "",
    };
    TreeItem::new(
        table_key(connection, schema, &table.name),
        format!("{}{suffix}", table.name),
    )
}

pub(super) fn conn_key(id: &str) -> String {
    format!("c{SEP}{id}")
}

pub(super) fn schema_key(connection: &str, schema: &str) -> String {
    format!("s{SEP}{connection}{SEP}{schema}")
}

pub(super) fn table_key(connection: &str, schema: &str, table: &str) -> String {
    format!("t{SEP}{connection}{SEP}{schema}{SEP}{table}")
}

pub(super) fn parse_node(id: &str) -> Option<NodeId> {
    let mut parts = id.split(SEP);
    match parts.next()? {
        "c" => Some(NodeId::Connection(parts.next()?.to_string())),
        "t" => {
            let _connection = parts.next()?;
            let schema = parts.next()?.to_string();
            let table = parts.collect::<Vec<_>>().join(&SEP.to_string());
            if table.is_empty() {
                None
            } else {
                Some(NodeId::Table { schema, table })
            }
        }
        _ => None,
    }
}
