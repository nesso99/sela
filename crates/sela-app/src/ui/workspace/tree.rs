//! Tree node ids and catalog-tree rendering helpers.

use std::collections::HashMap;

use gpui_kit::component::list::ListItem;
use gpui_kit::component::tree::{TreeEntry, TreeItem};
use gpui_kit::component::{h_flex, ActiveTheme};
use gpui_kit::prelude::*;
use gpui_kit::{div, px, App, Hsla, Window};

use sela_core::{ConnectionColor, RelationKind, TableInfo};

use super::super::theme::SelaPalette;

pub(super) const SEP: char = '\u{1f}';

pub(super) enum NodeId {
    Connection(String),
    Table { schema: String, table: String },
}

/// Color and label looked up from saved connections (keyed by connection id).
pub(super) type ConnDisplayLookup = HashMap<String, (ConnectionColor, String)>;

pub(super) fn render_tree_entry(
    _: usize,
    entry: &TreeEntry,
    _: bool,
    _: &mut Window,
    cx: &mut App,
    lookup: &ConnDisplayLookup,
) -> ListItem {
    let depth = entry.depth() as f32;
    let id = entry.item().id.as_str();
    let marker = if entry.is_folder() {
        if entry.is_expanded() {
            "▾"
        } else {
            "▸"
        }
    } else if id.starts_with('t') {
        "·"
    } else {
        ""
    };

    let palette = SelaPalette::for_mode(cx.theme().mode);
    let conn_id = connection_id_from_selection(id);
    let meta = conn_id.and_then(|cid| lookup.get(&cid));
    let swatch = meta.and_then(|(color, _)| palette.connection_color(*color));
    let conn_label = meta
        .map(|(_, label)| label.as_str())
        .filter(|s| !s.is_empty());

    let label_text = if marker.is_empty() {
        entry.item().label.to_string()
    } else {
        format!("{marker}  {}", entry.item().label)
    };

    ListItem::new(entry.item().id.clone())
        .pl(px(8. + depth * 14.))
        .child(
            h_flex()
                .items_center()
                .gap_1()
                .when_some(swatch, |row, color| {
                    row.child(connection_swatch(color, cx.theme().border))
                })
                .child(div().child(label_text))
                .when_some(conn_label.map(str::to_string), |row, tag| {
                    row.child(div().text_color(cx.theme().muted_foreground).child(tag))
                }),
        )
}

fn connection_swatch(fill: Hsla, border: Hsla) -> impl IntoElement {
    div()
        .size(px(8.))
        .flex_shrink_0()
        .border_1()
        .border_color(border)
        .bg(fill)
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

/// Stable tree id for a connection. Must stay `c{SEP}{id}` so expand/collapse
/// and Connect/Refresh (`conn_key`) agree across rebuilds.
pub(super) fn conn_key(id: &str) -> String {
    format!("c{SEP}{id}")
}

pub(super) fn group_key(name: &str) -> String {
    format!("g{SEP}{name}")
}

pub(super) fn is_group_key(id: &str) -> bool {
    id.split(SEP).next() == Some("g")
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
        // Group rows only expand/collapse; they are not connections.
        "g" => None,
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

/// Update `selected_connection_id` from a tree selection id.
///
/// Group rows clear the selection so Edit / Duplicate / Delete disable.
/// Connection / schema / table nodes set it. Other ids leave it unchanged.
pub(super) fn apply_selection_to_edit_target(
    selected_connection_id: &mut Option<String>,
    tree_id: &str,
) {
    if is_group_key(tree_id) {
        *selected_connection_id = None;
    } else if let Some(connection_id) = connection_id_from_selection(tree_id) {
        *selected_connection_id = Some(connection_id);
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
        assert_eq!(connection_id_from_selection(&group_key("Work")), None);
    }

    #[test]
    fn edit_target_prefers_tree_selection_over_active_id() {
        // Mirrors Workspace::selected_or_active_connection: selection wins.
        let selected = connection_id_from_selection(&conn_key("mysql-qa"));
        let active = Some("pg-qa".to_string());
        let target = selected.or(active);
        assert_eq!(target.as_deref(), Some("mysql-qa"));
    }

    #[test]
    fn conn_key_is_stable_without_color_or_label() {
        // Expand/collapse and Connect insert `conn_key(id)`; the tree item id
        // must match so a collapsed connection stays collapsed after rebuild.
        let id = "warehouse";
        assert_eq!(conn_key(id), format!("c{SEP}{id}"));
        assert!(matches!(
            parse_node(&conn_key(id)),
            Some(NodeId::Connection(ref s)) if s == id
        ));
        // Color/label live in the lookup map, not the tree id.
        let mut lookup = ConnDisplayLookup::new();
        lookup.insert(id.into(), (ConnectionColor::Red, "prod".into()));
        assert_eq!(
            lookup.get(id).map(|(c, l)| (*c, l.as_str())),
            Some((ConnectionColor::Red, "prod"))
        );
    }

    #[test]
    fn group_selection_clears_edit_target() {
        let mut selected = Some("pg-qa".to_string());
        apply_selection_to_edit_target(&mut selected, &group_key("Work"));
        assert_eq!(selected, None);

        apply_selection_to_edit_target(&mut selected, &conn_key("mysql-qa"));
        assert_eq!(selected.as_deref(), Some("mysql-qa"));

        // Schema under another connection retargets Edit.
        apply_selection_to_edit_target(&mut selected, &schema_key("pg-qa", "public"));
        assert_eq!(selected.as_deref(), Some("pg-qa"));
    }
}
