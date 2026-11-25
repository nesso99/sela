//! Sidebar grouping and duplicate naming (M2-5).

use crate::connection::SavedConnection;
use crate::ConnectionColor;

/// One row in the connection sidebar after grouping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidebarConnectionNode<'a> {
    /// Ungrouped connection at the top of the list.
    Connection(&'a SavedConnection),
    /// One-level group. Members stay in file order.
    Group {
        name: &'a str,
        members: Vec<&'a SavedConnection>,
    },
}

/// Build the sidebar layout: ungrouped connections first (file order), then
/// groups in the order each group's first member appears in the file.
pub fn sidebar_connection_layout(
    connections: &[SavedConnection],
) -> Vec<SidebarConnectionNode<'_>> {
    let mut out = Vec::new();
    let mut seen_groups: Vec<&str> = Vec::new();

    for conn in connections {
        let group = conn.group.trim();
        if group.is_empty() {
            out.push(SidebarConnectionNode::Connection(conn));
        } else if !seen_groups.contains(&group) {
            seen_groups.push(group);
        }
    }

    for group_name in seen_groups {
        let members: Vec<&SavedConnection> = connections
            .iter()
            .filter(|c| c.group.trim() == group_name)
            .collect();
        if !members.is_empty() {
            out.push(SidebarConnectionNode::Group {
                name: group_name,
                members,
            });
        }
    }
    out
}

/// Next duplicate display name: `{name} copy`, then `{name} copy 2`, …
pub fn next_duplicate_name<'a>(
    base_name: &str,
    existing_names: impl IntoIterator<Item = &'a str>,
) -> String {
    let names: std::collections::HashSet<&str> = existing_names.into_iter().collect();
    let first = format!("{base_name} copy");
    if !names.contains(first.as_str()) {
        return first;
    }
    for n in 2usize.. {
        let candidate = format!("{base_name} copy {n}");
        if !names.contains(candidate.as_str()) {
            return candidate;
        }
    }
    unreachable!()
}

/// Duplicate a saved connection without copying the password / keychain secret.
///
/// Copies engine, host, port, user, database, file, TLS, record_history, color,
/// label, group, and safe mode. Assigns a new id and [`next_duplicate_name`].
/// Sets [`SavedConnection::needs_password`] when the engine uses a password so
/// connect asks for a secret (the password is never copied and no keychain item
/// is written). SQLite and any engine without a password field stay false.
pub fn duplicate_connection(
    source: &SavedConnection,
    existing_names: impl IntoIterator<Item = impl AsRef<str>>,
) -> SavedConnection {
    let names: Vec<String> = existing_names
        .into_iter()
        .map(|s| s.as_ref().to_string())
        .collect();
    let mut copy = source.clone();
    copy.id = uuid::Uuid::new_v4().to_string();
    copy.name = next_duplicate_name(&source.name, names.iter().map(|s| s.as_str()));
    copy.needs_password = source.engine.uses_password();
    copy
}

/// Status line after Duplicate. Only engines with a password field ask the user
/// to type one; SQLite gets a short confirmation.
pub fn duplicate_status_message(name: &str, needs_password: bool) -> String {
    if needs_password {
        format!("Duplicated {name}. Type the password to connect.")
    } else {
        format!("Duplicated {name}.")
    }
}

/// Distinct non-empty group names in file order (first appearance).
pub fn existing_group_names(connections: &[SavedConnection]) -> Vec<String> {
    let mut out = Vec::new();
    for conn in connections {
        let g = conn.group.trim();
        if !g.is_empty() && !out.iter().any(|x: &String| x == g) {
            out.push(g.to_string());
        }
    }
    out
}

/// Whether a color paints a swatch / strip (not `none`).
pub fn connection_color_is_visible(color: ConnectionColor) -> bool {
    !color.is_none()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Engine;

    fn conn(name: &str, group: &str) -> SavedConnection {
        let mut c = SavedConnection::new(Engine::Sqlite);
        c.name = name.into();
        c.group = group.into();
        c.file_path = "/tmp/a.sqlite".into();
        c
    }

    #[test]
    fn group_ordering_ungrouped_then_first_appearance() {
        let connections = vec![
            conn("A", ""),
            conn("B", "Work"),
            conn("C", ""),
            conn("D", "Home"),
            conn("E", "Work"),
        ];
        let layout = sidebar_connection_layout(&connections);
        assert_eq!(layout.len(), 4);
        match &layout[0] {
            SidebarConnectionNode::Connection(c) => assert_eq!(c.name, "A"),
            _ => panic!("expected A"),
        }
        match &layout[1] {
            SidebarConnectionNode::Connection(c) => assert_eq!(c.name, "C"),
            _ => panic!("expected C"),
        }
        match &layout[2] {
            SidebarConnectionNode::Group { name, members } => {
                assert_eq!(*name, "Work");
                assert_eq!(
                    members.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
                    vec!["B", "E"]
                );
            }
            _ => panic!("expected Work"),
        }
        match &layout[3] {
            SidebarConnectionNode::Group { name, members } => {
                assert_eq!(*name, "Home");
                assert_eq!(members.len(), 1);
                assert_eq!(members[0].name, "D");
            }
            _ => panic!("expected Home"),
        }
    }

    #[test]
    fn empty_groups_are_not_shown() {
        // Empty group string is ungrouped; no empty-named parent.
        let connections = vec![conn("A", "   ")];
        let layout = sidebar_connection_layout(&connections);
        assert!(matches!(layout[0], SidebarConnectionNode::Connection(_)));
        assert_eq!(layout.len(), 1);
    }

    #[test]
    fn duplicate_name_sequence() {
        assert_eq!(next_duplicate_name("Prod", []), "Prod copy");
        assert_eq!(next_duplicate_name("Prod", ["Prod copy"]), "Prod copy 2");
        assert_eq!(
            next_duplicate_name("Prod", ["Prod copy", "Prod copy 2"]),
            "Prod copy 3"
        );
    }

    #[test]
    fn duplicate_excludes_password_and_gets_new_id() {
        let mut source = SavedConnection::new(Engine::Postgres);
        source.name = "Warehouse".into();
        source.username = "sela".into();
        source.database = "sample".into();
        source.color = ConnectionColor::Red;
        source.label = "prod".into();
        source.group = "Work".into();
        source.safe_mode = crate::SafeMode::Confirm;
        source.record_history = false;
        let copy = duplicate_connection(&source, ["Warehouse"]);
        assert_ne!(copy.id, source.id);
        assert_eq!(copy.name, "Warehouse copy");
        assert_eq!(copy.engine, source.engine);
        assert_eq!(copy.host, source.host);
        assert_eq!(copy.port, source.port);
        assert_eq!(copy.username, source.username);
        assert_eq!(copy.database, source.database);
        assert_eq!(copy.tls, source.tls);
        assert_eq!(copy.record_history, source.record_history);
        assert_eq!(copy.color, ConnectionColor::Red);
        assert_eq!(copy.label, "prod");
        assert_eq!(copy.group, "Work");
        assert_eq!(copy.safe_mode, crate::SafeMode::Confirm);
        assert!(copy.needs_password);
        assert!(!source.needs_password);
        // Password is not a field on SavedConnection; duplicate does not
        // create a keychain item (caller must not copy secrets).
    }

    #[test]
    fn duplicate_sqlite_never_sets_needs_password() {
        let mut source = SavedConnection::new(Engine::Sqlite);
        source.name = "SQLite QA".into();
        source.file_path = "/tmp/qa.sqlite".into();
        let copy = duplicate_connection(&source, ["SQLite QA"]);
        assert_eq!(copy.name, "SQLite QA copy");
        assert!(!copy.needs_password);
        assert!(!copy.should_prompt_for_password(None));
        assert_eq!(
            duplicate_status_message(&copy.name, copy.needs_password),
            "Duplicated SQLite QA copy."
        );
    }

    #[test]
    fn duplicate_mysql_sets_needs_password() {
        let source = SavedConnection::new(Engine::Mysql);
        let copy = duplicate_connection(&source, ["x"]);
        assert!(copy.needs_password);
        assert!(copy.should_prompt_for_password(None));
    }

    #[test]
    fn duplicate_still_prompts_when_secret_absent() {
        let source = SavedConnection::new(Engine::Postgres);
        let copy = duplicate_connection(&source, ["x"]);
        assert!(copy.should_prompt_for_password(None));
        assert!(!copy.should_prompt_for_password(Some("")));
        assert!(!copy.should_prompt_for_password(Some("secret")));
    }

    #[test]
    fn duplicate_status_sentence() {
        // docs/design/ux/01-main-window.md — shown on the tab status or
        // workspace status when no tab is open.
        assert_eq!(
            duplicate_status_message("Red Prod", true),
            "Duplicated Red Prod. Type the password to connect."
        );
        assert_eq!(
            duplicate_status_message("Local file", false),
            "Duplicated Local file."
        );
    }

    #[test]
    fn engine_uses_password_matches_dialog_fields() {
        assert!(Engine::Postgres.uses_password());
        assert!(Engine::Mysql.uses_password());
        assert!(!Engine::Sqlite.uses_password());
    }
}
