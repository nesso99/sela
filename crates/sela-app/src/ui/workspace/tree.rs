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

/// Connection id from a tree selection (`c` / `s` / `t` node), if any.
pub(super) fn connection_id_from_selection(id: &str) -> Option<String> {
    let mut parts = id.split(SEP);
    match parts.next()? {
        "c" | "s" | "t" => {
            let connection = parts.next()?.to_string();
            if connection.is_empty() {
                None
            } else {
                Some(connection)
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_id_from_selection_reads_connection_schema_and_table_nodes() {
        let id = "abc-123";
        assert_eq!(
            connection_id_from_selection(&conn_key(id)).as_deref(),
            Some(id)
        );
        assert_eq!(
            connection_id_from_selection(&schema_key(id, "public")).as_deref(),
            Some(id)
        );
        assert_eq!(
            connection_id_from_selection(&table_key(id, "public", "books")).as_deref(),
            Some(id)
        );
        assert_eq!(connection_id_from_selection("n\u{1f}noise"), None);
    }

    #[test]
    fn edit_target_prefers_tree_selection_over_active_id() {
        // Mirrors Workspace::selected_or_active_connection: selection wins.
        let selected = connection_id_from_selection(&conn_key("mysql-qa"));
        let active = Some("pg-qa".to_string());
        let target = selected.or(active);
        assert_eq!(target.as_deref(), Some("mysql-qa"));
    }
}
