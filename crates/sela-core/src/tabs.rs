//! Open table / query tabs: open, close, switch, and dedupe.
//!
//! Pure domain state — no GPUI. Each tab carries its own request counter and
//! [`CancellationToken`] so closing a tab cancels its running query. The
//! `dirty` flag is the M2 hook for "prompt before close with pending edits".

use tokio_util::sync::CancellationToken;

use serde::{Deserialize, Serialize};

use crate::filter::{FilterCondition, SortSpec};

/// Stable identity for a tab within one app session (not persisted).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TabId(u64);

impl TabId {
    pub fn get(self) -> u64 {
        self.0
    }
}

/// What the tab shows: a browsed table, or a free-form SQL editor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TabKind {
    Table { schema: String, table: String },
    Query { sql: String },
}

impl TabKind {
    pub fn title(&self) -> String {
        match self {
            TabKind::Table { schema, table } => format!("{schema}.{table}"),
            TabKind::Query { sql } => {
                let trimmed = sql.trim();
                if trimmed.is_empty() {
                    "Query".into()
                } else {
                    let one_line: String = trimmed
                        .lines()
                        .next()
                        .unwrap_or(trimmed)
                        .chars()
                        .take(40)
                        .collect();
                    if trimmed.chars().count() > one_line.chars().count() {
                        format!("{one_line}…")
                    } else {
                        one_line
                    }
                }
            }
        }
    }

    pub fn is_same_table(&self, schema: &str, table: &str) -> bool {
        matches!(
            self,
            TabKind::Table {
                schema: s,
                table: t,
            } if s == schema && t == table
        )
    }
}

/// One open tab: connection, kind, and cancel/request bookkeeping.
#[derive(Debug)]
pub struct Tab {
    pub id: TabId,
    pub connection_id: String,
    pub kind: TabKind,
    /// Monotonic id for ignoring stale async completions on this tab.
    pub request: u64,
    cancel: Option<CancellationToken>,
    /// M2 hook: true when the tab has unsaved edits that should prompt on close.
    pub dirty: bool,
}

impl Tab {
    fn new(id: TabId, connection_id: String, kind: TabKind) -> Self {
        Self {
            id,
            connection_id,
            kind,
            request: 0,
            cancel: None,
            dirty: false,
        }
    }

    pub fn cancel_token(&self) -> Option<&CancellationToken> {
        self.cancel.as_ref()
    }

    pub fn bump_request(&mut self) -> u64 {
        self.request = self.request.wrapping_add(1);
        self.request
    }

    pub fn set_cancel(&mut self, token: CancellationToken) {
        if let Some(prev) = self.cancel.take() {
            prev.cancel();
        }
        self.cancel = Some(token);
    }

    pub fn cancel_running(&mut self) {
        if let Some(token) = self.cancel.take() {
            token.cancel();
        }
    }

    pub fn clear_cancel(&mut self) {
        self.cancel = None;
    }
}

/// Ordered list of tabs plus the active index.
#[derive(Debug, Default)]
pub struct TabList {
    tabs: Vec<Tab>,
    active: Option<usize>,
    next_id: u64,
}

impl TabList {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.tabs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    pub fn active_index(&self) -> Option<usize> {
        self.active
    }

    pub fn active(&self) -> Option<&Tab> {
        self.active.and_then(|ix| self.tabs.get(ix))
    }

    pub fn active_mut(&mut self) -> Option<&mut Tab> {
        self.active.and_then(|ix| self.tabs.get_mut(ix))
    }

    pub fn get(&self, id: TabId) -> Option<&Tab> {
        self.tabs.iter().find(|tab| tab.id == id)
    }

    pub fn get_mut(&mut self, id: TabId) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|tab| tab.id == id)
    }

    pub fn index_of(&self, id: TabId) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.id == id)
    }

    /// Open a table tab, or focus the existing one for the same connection+table.
    ///
    /// Returns `(id, created)`.
    pub fn open_table(
        &mut self,
        connection_id: impl Into<String>,
        schema: impl Into<String>,
        table: impl Into<String>,
    ) -> (TabId, bool) {
        let connection_id = connection_id.into();
        let schema = schema.into();
        let table = table.into();
        if let Some(ix) = self.tabs.iter().position(|tab| {
            tab.connection_id == connection_id && tab.kind.is_same_table(&schema, &table)
        }) {
            self.active = Some(ix);
            return (self.tabs[ix].id, false);
        }
        let id = self.alloc_id();
        self.tabs.push(Tab::new(
            id,
            connection_id,
            TabKind::Table { schema, table },
        ));
        self.active = Some(self.tabs.len() - 1);
        (id, true)
    }

    /// Always create a new query tab (Cmd/Ctrl+T).
    pub fn open_query(
        &mut self,
        connection_id: impl Into<String>,
        sql: impl Into<String>,
    ) -> TabId {
        let id = self.alloc_id();
        self.tabs.push(Tab::new(
            id,
            connection_id.into(),
            TabKind::Query { sql: sql.into() },
        ));
        self.active = Some(self.tabs.len() - 1);
        id
    }

    pub fn activate(&mut self, id: TabId) -> bool {
        if let Some(ix) = self.index_of(id) {
            self.active = Some(ix);
            true
        } else {
            false
        }
    }

    /// Activate by 0-based index (Cmd/Ctrl+1 → index 0).
    pub fn activate_index(&mut self, index: usize) -> bool {
        if index < self.tabs.len() {
            self.active = Some(index);
            true
        } else {
            false
        }
    }

    /// Whether closing should prompt for unsaved edits (M2). Never true until
    /// editing sets [`Tab::dirty`].
    pub fn should_prompt_before_close(&self, id: TabId) -> bool {
        self.get(id).is_some_and(|tab| tab.dirty)
    }

    /// Close a tab, cancelling its running query. Focus moves to the tab that
    /// took this index (the old right neighbour), or the new last tab.
    pub fn close(&mut self, id: TabId) -> Option<Tab> {
        let ix = self.index_of(id)?;
        let mut removed = self.tabs.remove(ix);
        removed.cancel_running();
        self.active = if self.tabs.is_empty() {
            None
        } else if ix < self.tabs.len() {
            Some(ix)
        } else {
            Some(self.tabs.len() - 1)
        };
        Some(removed)
    }

    pub fn close_active(&mut self) -> Option<Tab> {
        let id = self.active()?.id;
        self.close(id)
    }

    /// Snapshot for disk (no passwords, no tokens).
    ///
    /// Callers should merge filter/sort from the UI [`PersistedTab`] fields
    /// before save; this copies kind + connection only.
    pub fn to_persisted(&self) -> PersistedTabs {
        PersistedTabs {
            tabs: self
                .tabs
                .iter()
                .map(|tab| PersistedTab {
                    connection_id: tab.connection_id.clone(),
                    kind: tab.kind.clone(),
                    filters: Vec::new(),
                    sorts: Vec::new(),
                    filter_bar_open: false,
                })
                .collect(),
            active: self.active,
        }
    }

    /// Restore from disk. Invalid active index is clamped; empty input is fine.
    pub fn from_persisted(saved: PersistedTabs) -> Self {
        let mut list = Self::new();
        for item in saved.tabs {
            let id = list.alloc_id();
            list.tabs.push(Tab::new(id, item.connection_id, item.kind));
        }
        list.active = if list.tabs.is_empty() {
            None
        } else {
            Some(saved.active.unwrap_or(0).min(list.tabs.len() - 1))
        };
        list
    }

    fn alloc_id(&mut self) -> TabId {
        let id = TabId(self.next_id);
        self.next_id = self.next_id.wrapping_add(1);
        id
    }
}

/// On-disk shape for the open tab list (`tabs.json`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedTabs {
    #[serde(default)]
    pub tabs: Vec<PersistedTab>,
    #[serde(default)]
    pub active: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedTab {
    pub connection_id: String,
    pub kind: TabKind,
    /// Table-tab filter conditions (serde-default empty for older tabs.json).
    #[serde(default)]
    pub filters: Vec<FilterCondition>,
    /// Table-tab sort specs (serde-default empty).
    #[serde(default)]
    pub sorts: Vec<SortSpec>,
    /// Whether the filter bar was open when the session was saved.
    #[serde(default)]
    pub filter_bar_open: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_table_dedupes_same_connection_and_table() {
        let mut tabs = TabList::new();
        let (a, created_a) = tabs.open_table("c1", "public", "books");
        let (b, created_b) = tabs.open_table("c1", "public", "books");
        assert!(created_a);
        assert!(!created_b);
        assert_eq!(a, b);
        assert_eq!(tabs.len(), 1);

        let (c, created_c) = tabs.open_table("c1", "public", "authors");
        assert!(created_c);
        assert_ne!(a, c);
        assert_eq!(tabs.len(), 2);

        let (d, created_d) = tabs.open_table("c2", "public", "books");
        assert!(created_d);
        assert_ne!(a, d);
        assert_eq!(tabs.len(), 3);
    }

    #[test]
    fn open_query_always_creates() {
        let mut tabs = TabList::new();
        let a = tabs.open_query("c1", "");
        let b = tabs.open_query("c1", "");
        assert_ne!(a, b);
        assert_eq!(tabs.len(), 2);
        assert_eq!(tabs.active().map(|t| t.id), Some(b));
    }

    #[test]
    fn switch_and_close_focuses_neighbour() {
        let mut tabs = TabList::new();
        let (t0, _) = tabs.open_table("c1", "public", "a");
        let (t1, _) = tabs.open_table("c1", "public", "b");
        let (t2, _) = tabs.open_table("c1", "public", "c");
        assert_eq!(tabs.active().map(|t| t.id), Some(t2));

        assert!(tabs.activate(t0));
        assert_eq!(tabs.active_index(), Some(0));

        assert!(tabs.activate_index(1));
        assert_eq!(tabs.active().map(|t| t.id), Some(t1));

        tabs.close(t1);
        assert_eq!(tabs.len(), 2);
        // Closed middle → focus the tab that slid into its index (former t2).
        assert_eq!(tabs.active().map(|t| t.id), Some(t2));

        tabs.close(t2);
        assert_eq!(tabs.active().map(|t| t.id), Some(t0));

        tabs.close(t0);
        assert!(tabs.is_empty());
        assert_eq!(tabs.active_index(), None);
    }

    #[test]
    fn closing_cancels_the_tabs_token() {
        let mut tabs = TabList::new();
        let id = tabs.open_query("c1", "select 1");
        let token = CancellationToken::new();
        let watch = token.clone();
        tabs.get_mut(id).unwrap().set_cancel(token);
        assert!(!watch.is_cancelled());

        tabs.close(id);
        assert!(watch.is_cancelled());
        assert!(tabs.is_empty());
    }

    /// Structure loads (and queries) share the per-tab cancel token + request id.
    /// Closing must cancel the token and remove the tab so a late completion
    /// cannot apply (`Workspace::spawn_work` returns early when `get(id)` is None).
    #[test]
    fn closing_drops_pending_structure_or_query_load() {
        let mut tabs = TabList::new();
        let (a, _) = tabs.open_table("c1", "public", "books");
        let (b, _) = tabs.open_table("c1", "public", "authors");

        let token_a = CancellationToken::new();
        let watch_a = token_a.clone();
        tabs.get_mut(a).unwrap().bump_request();
        tabs.get_mut(a).unwrap().set_cancel(token_a);

        let token_b = CancellationToken::new();
        let watch_b = token_b.clone();
        let request_b = tabs.get_mut(b).unwrap().bump_request();
        tabs.get_mut(b).unwrap().set_cancel(token_b);

        // Close tab A while its structure load is in flight.
        tabs.close(a);
        assert!(watch_a.is_cancelled());
        assert!(tabs.get(a).is_none());
        // Tab B's pending load is untouched and still addressable by id.
        assert!(!watch_b.is_cancelled());
        assert_eq!(tabs.get(b).map(|t| t.request), Some(request_b));
        assert!(tabs.get(b).unwrap().cancel_token().is_some());
    }

    #[test]
    fn dirty_hook_gates_prompt_before_close() {
        let mut tabs = TabList::new();
        let id = tabs.open_query("c1", "");
        assert!(!tabs.should_prompt_before_close(id));
        tabs.get_mut(id).unwrap().dirty = true;
        assert!(tabs.should_prompt_before_close(id));
    }

    #[test]
    fn persisted_round_trip_keeps_kind_and_active() {
        let mut tabs = TabList::new();
        tabs.open_table("c1", "public", "books");
        tabs.open_query("c1", "select 1");
        tabs.activate_index(0);
        let saved = tabs.to_persisted();
        assert_eq!(saved.tabs.len(), 2);
        assert_eq!(saved.active, Some(0));

        let restored = TabList::from_persisted(saved.clone());
        assert_eq!(restored.to_persisted(), saved);
        assert_eq!(
            restored.active().map(|t| t.kind.clone()),
            Some(TabKind::Table {
                schema: "public".into(),
                table: "books".into(),
            })
        );
    }

    #[test]
    fn from_persisted_clamps_active_and_accepts_empty() {
        let empty = TabList::from_persisted(PersistedTabs::default());
        assert!(empty.is_empty());

        let restored = TabList::from_persisted(PersistedTabs {
            tabs: vec![PersistedTab {
                connection_id: "c1".into(),
                kind: TabKind::Query {
                    sql: "select 1".into(),
                },
                filters: Vec::new(),
                sorts: Vec::new(),
                filter_bar_open: false,
            }],
            active: Some(99),
        });
        assert_eq!(restored.active_index(), Some(0));
    }

    #[test]
    fn persisted_tab_defaults_missing_filter_fields() {
        let json =
            r#"{"connection_id":"c","kind":{"type":"table","schema":"public","table":"books"}}"#;
        let tab: PersistedTab = serde_json::from_str(json).unwrap();
        assert!(tab.filters.is_empty());
        assert!(tab.sorts.is_empty());
        assert!(!tab.filter_bar_open);
    }
}
