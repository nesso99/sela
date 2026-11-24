//! Tree node ids and catalog-tree rendering helpers.

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

pub(super) fn render_tree_entry(
    _: usize,
    entry: &TreeEntry,
    _: bool,
    _: &mut Window,
    cx: &mut App,
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
    let meta = connection_meta_from_id(id);
    let swatch = meta
        .as_ref()
        .and_then(|m| palette.connection_color(m.color));
    let conn_label = meta
        .as_ref()
        .map(|m| m.label.as_str())
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

struct ConnMeta {
    color: ConnectionColor,
    label: String,
}

fn connection_meta_from_id(id: &str) -> Option<ConnMeta> {
    let mut parts = id.split(SEP);
    match parts.next()? {
        "c" => {
            let _connection = parts.next()?;
            let color = ConnectionColor::parse(parts.next().unwrap_or("none"));
            let label = parts.next().unwrap_or("").to_string();
            Some(ConnMeta { color, label })
        }
        _ => None,
    }
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

/// Connection tree id that embeds color and optional label for rendering.
pub(super) fn conn_key_with_meta(id: &str, color: ConnectionColor, label: &str) -> String {
    format!("c{SEP}{id}{SEP}{}{SEP}{}", color.as_str(), label.trim())
}

pub(super) fn group_key(name: &str) -> String {
    format!("g{SEP}{name}")
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
            connection_id_from_selection(&conn_key_with_meta(id, ConnectionColor::Red, "prod"))
                .as_deref(),
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
    fn color_and_label_embedded_in_conn_key_parses() {
        let id = conn_key_with_meta("x", ConnectionColor::Teal, "qa");
        let meta = connection_meta_from_id(&id).unwrap();
        assert_eq!(meta.color, ConnectionColor::Teal);
        assert_eq!(meta.label, "qa");
        assert!(matches!(
            parse_node(&id),
            Some(NodeId::Connection(ref s)) if s == "x"
        ));
    }
}
