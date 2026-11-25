//! Grid editing view-model (M2-1 / M2-3): ChangeSet overlay, markers, discard, commit state.
//!
//! Pure logic with no GPUI dependency so unit tests can drive dirty tracking,
//! row markers, discard, and failed-commit retention without a window.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use rust_decimal::Decimal;
use sela_core::{
    bind_placeholder, escape_sql_string, value_to_copy_cell, CellChange, ChangeError, ChangePlan,
    ChangeSet, ColumnKind, ColumnStructure, CopyCell, CopyScalarKind, DbError, Dialect, PlanOp,
    PlannedStatement, RelationKind, RowChange, SafeMode, TableStructure, Value,
};
use uuid::Uuid;

/// Why the Data grid refuses edits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReadOnlyReason {
    NoPrimaryKey,
    View,
    SqlResult,
    /// Structure not loaded yet (transient; strip stays hidden).
    Loading,
}

impl ReadOnlyReason {
    pub fn strip_line(&self) -> Option<&'static str> {
        match self {
            Self::NoPrimaryKey => Some("Read-only · this table has no primary key."),
            Self::View => Some("Read-only · views are read-only."),
            Self::SqlResult => None, // status only, no strip
            Self::Loading => None,
        }
    }

    pub fn status_line(&self) -> &'static str {
        match self {
            Self::NoPrimaryKey => "Read-only · this table has no primary key.",
            Self::View => "Read-only · views are read-only.",
            Self::SqlResult => "SQL results are read-only. Open the table to edit.",
            Self::Loading => "Loading table structure…",
        }
    }
}

/// Gutter / wash state for one logical row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RowMarker {
    Clean,
    Inserted,
    Duplicated,
    Dirty,
    Deleted,
}

impl RowMarker {
    pub fn gutter_glyph(self) -> Option<&'static str> {
        match self {
            Self::Clean => None,
            Self::Inserted => Some("+"),
            Self::Duplicated => Some("++"),
            Self::Dirty => Some("•"),
            Self::Deleted => Some("−"),
        }
    }

    pub fn gutter_tooltip(self) -> Option<&'static str> {
        match self {
            Self::Clean => None,
            Self::Inserted => Some("New row"),
            Self::Duplicated => Some("Duplicated row"),
            Self::Dirty => Some("Edited row"),
            Self::Deleted => Some("Deleted row"),
        }
    }

    pub fn is_insert_wash(self) -> bool {
        matches!(self, Self::Inserted | Self::Duplicated)
    }
}

/// Identity of a row in the editable grid (server page or overlay insert).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum EditRowId {
    /// Absolute index into the current [`sela_core::ResultWindow`] page.
    Server {
        row_ix: usize,
    },
    Overlay {
        insert_id: u64,
    },
}

/// Open in-cell editor draft.
#[derive(Debug, Clone)]
pub(crate) struct CellEditor {
    pub row: EditRowId,
    pub column: String,
    pub col_ix: usize,
    pub kind: ColumnKind,
    /// Text the user is typing. Ignored when [`Self::is_null`].
    pub draft: String,
    /// Explicit NULL (ghost NULL button / bool menu). Distinct from `draft == ""`.
    pub is_null: bool,
    /// Display string when the editor opened (for unchanged no-op).
    pub original_display: String,
    /// Whether the cell was NULL when the editor opened.
    pub started_as_null: bool,
    /// Whether the cell was unset (absent from an insert) when the editor opened.
    /// An empty accept on an unset cell is a no-op — leave the column out (#77).
    pub started_unset: bool,
    /// Set only by the ghost NULL button. Distinguishes explicit NULL from an
    /// empty text accept that must store `''` (#77).
    pub forced_null: bool,
    /// Captured primary key for server rows (stable across page reload).
    pub pk: Option<Vec<Value>>,
    /// Original server / pending value used for ChangeSet merge (`old_value`).
    pub old_value: Option<Value>,
    pub validation_error: Option<String>,
    #[allow(dead_code)]
    pub popover: bool,
}

/// Counts shown on the 26px pending bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct PendingCounts {
    pub updated: usize,
    pub inserted: usize,
    pub deleted: usize,
}

impl PendingCounts {
    pub fn total(self) -> usize {
        self.updated + self.inserted + self.deleted
    }

    pub fn summary_left(self) -> String {
        let total = self.total();
        let mut parts = Vec::new();
        if self.updated > 0 {
            parts.push(format!("{} updated", self.updated));
        }
        if self.inserted > 0 {
            parts.push(format!("{} inserted", self.inserted));
        }
        if self.deleted > 0 {
            parts.push(format!("{} deleted", self.deleted));
        }
        if parts.is_empty() {
            format!("{total} pending")
        } else {
            format!("{total} pending · {}", parts.join(", "))
        }
    }
}

/// Overlay insert kept beside the ResultWindow (not written into pages).
#[derive(Debug, Clone)]
pub(crate) struct OverlayRow {
    pub insert_id: u64,
    pub duplicated: bool,
    /// Source row PK for Duplicate placement. When that PK is on the current
    /// page, the overlay paints under it; otherwise at the end of the page.
    /// `None` (blank Insert) always paints after the loaded page.
    /// Stable across paging/filtering — never a page index (07-grid-editing.md).
    pub after_source_pk: Option<Vec<Value>>,
}

/// One visual grid row: a server page row or an overlay insert/duplicate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VisualRowRef {
    Server { row_ix: usize },
    Overlay { overlay_ix: usize },
}

/// Resolve each overlay to a page-local `after_server_row` for the current page.
/// Missing / off-page PKs become `None` (end of page). Never drops an overlay.
pub(crate) fn resolve_overlay_anchors(
    overlays: &[OverlayRow],
    page_pks: &[Option<Vec<Value>>],
) -> Vec<Option<usize>> {
    overlays
        .iter()
        .map(|overlay| {
            overlay.after_source_pk.as_ref().and_then(|pk| {
                page_pks
                    .iter()
                    .position(|page_pk| page_pk.as_ref() == Some(pk))
            })
        })
        .collect()
}

/// Build the visual order from resolved page anchors (`None` = end of page).
///
/// Empty `after_server_rows` returns the identity map. An out-of-range
/// `Some(ix)` (≥ `server_count`) is treated as end-of-page — never dropped.
pub(crate) fn visual_row_map(
    server_count: usize,
    after_server_rows: &[Option<usize>],
) -> Vec<VisualRowRef> {
    if after_server_rows.is_empty() {
        return (0..server_count)
            .map(|row_ix| VisualRowRef::Server { row_ix })
            .collect();
    }
    let mut out = Vec::with_capacity(server_count + after_server_rows.len());
    let mut placed = vec![false; after_server_rows.len()];
    for server_ix in 0..server_count {
        out.push(VisualRowRef::Server { row_ix: server_ix });
        for (overlay_ix, after) in after_server_rows.iter().enumerate() {
            if *after == Some(server_ix) {
                out.push(VisualRowRef::Overlay { overlay_ix });
                placed[overlay_ix] = true;
            }
        }
    }
    for (overlay_ix, after) in after_server_rows.iter().enumerate() {
        if placed[overlay_ix] {
            continue;
        }
        // None, or Some(out of range): end of page — never drop.
        match after {
            None | Some(_) => out.push(VisualRowRef::Overlay { overlay_ix }),
        }
    }
    out
}

/// Overlays + current-page PKs → visual order (Duplicate under source when present).
pub(crate) fn visual_row_map_for_overlays(
    server_count: usize,
    overlays: &[OverlayRow],
    page_pks: &[Option<Vec<Value>>],
) -> Vec<VisualRowRef> {
    let anchors = resolve_overlay_anchors(overlays, page_pks);
    visual_row_map(server_count, &anchors)
}

/// Per-tab editing session.
#[derive(Debug, Clone)]
pub(crate) struct EditSession {
    #[allow(dead_code)]
    pub relation_kind: RelationKind,
    pub read_only: Option<ReadOnlyReason>,
    pub changeset: Option<ChangeSet>,
    pub generated_columns: HashSet<String>,
    pub column_types: HashMap<String, String>,
    pub dialect: Dialect,
    pub overlay: Vec<OverlayRow>,
    /// Cell updates preserved when a dirty server row is marked deleted.
    preserved_updates: BTreeMap<PkKey, BTreeMap<String, CellChange>>,
    pub editor: Option<CellEditor>,
    pub preview_open: bool,
    pub committing: bool,
    /// Monotonic id so a commit completion can ignore a superseded attempt.
    pub commit_generation: u64,
    /// After a failed commit: pending bar shows "Commit failed. Changes kept."
    pub commit_failed: bool,
    /// Latest connection safe-mode level (synced from the workspace before edits/commit).
    pub connection_safe_mode: SafeMode,
}

/// Newtype so `Vec<Value>` can be a BTreeMap key via display+Debug-stable order.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct PkKey(Vec<String>);

impl PkKey {
    fn from_values(pk: &[Value]) -> Self {
        Self(pk.iter().map(|v| v.display().into_owned()).collect())
    }
}

impl EditSession {
    /// SQL result tab — never editable; no strip.
    pub fn sql_result() -> Self {
        Self {
            relation_kind: RelationKind::Table,
            read_only: Some(ReadOnlyReason::SqlResult),
            changeset: None,
            generated_columns: HashSet::new(),
            column_types: HashMap::new(),
            dialect: Dialect::Sqlite,
            overlay: Vec::new(),
            preserved_updates: BTreeMap::new(),
            editor: None,
            preview_open: false,
            committing: false,
            commit_generation: 0,
            commit_failed: false,
            connection_safe_mode: SafeMode::Off,
        }
    }

    /// Idle / unknown — wait for structure.
    pub fn loading(dialect: Dialect, relation_kind: RelationKind) -> Self {
        let read_only = if relation_kind == RelationKind::View {
            Some(ReadOnlyReason::View)
        } else {
            Some(ReadOnlyReason::Loading)
        };
        Self {
            relation_kind,
            read_only,
            changeset: None,
            generated_columns: HashSet::new(),
            column_types: HashMap::new(),
            dialect,
            overlay: Vec::new(),
            preserved_updates: BTreeMap::new(),
            editor: None,
            preview_open: false,
            committing: false,
            commit_generation: 0,
            commit_failed: false,
            connection_safe_mode: SafeMode::Off,
        }
    }

    /// Build from introspection. No PK → read-only strip.
    pub fn from_structure(
        structure: &TableStructure,
        dialect: Dialect,
        relation_kind: RelationKind,
    ) -> Self {
        if relation_kind == RelationKind::View {
            return Self {
                relation_kind,
                read_only: Some(ReadOnlyReason::View),
                changeset: None,
                generated_columns: HashSet::new(),
                column_types: column_type_map(structure),
                dialect,
                overlay: Vec::new(),
                preserved_updates: BTreeMap::new(),
                editor: None,
                preview_open: false,
                committing: false,
                commit_generation: 0,
                commit_failed: false,
                connection_safe_mode: SafeMode::Off,
            };
        }
        let generated = detect_generated_columns(structure, dialect);
        let column_types = column_type_map(structure);
        match ChangeSet::from_structure(structure) {
            Ok(cs) => Self {
                relation_kind,
                read_only: None,
                changeset: Some(cs),
                generated_columns: generated,
                column_types,
                dialect,
                overlay: Vec::new(),
                preserved_updates: BTreeMap::new(),
                editor: None,
                preview_open: false,
                committing: false,
                commit_generation: 0,
                commit_failed: false,
                connection_safe_mode: SafeMode::Off,
            },
            Err(ChangeError::NoPrimaryKey) => Self {
                relation_kind,
                read_only: Some(ReadOnlyReason::NoPrimaryKey),
                changeset: None,
                generated_columns: generated,
                column_types,
                dialect,
                overlay: Vec::new(),
                preserved_updates: BTreeMap::new(),
                editor: None,
                preview_open: false,
                committing: false,
                commit_generation: 0,
                commit_failed: false,
                connection_safe_mode: SafeMode::Off,
            },
            Err(_) => Self {
                relation_kind,
                read_only: Some(ReadOnlyReason::NoPrimaryKey),
                changeset: None,
                generated_columns: generated,
                column_types,
                dialect,
                overlay: Vec::new(),
                preserved_updates: BTreeMap::new(),
                editor: None,
                preview_open: false,
                committing: false,
                commit_generation: 0,
                commit_failed: false,
                connection_safe_mode: SafeMode::Off,
            },
        }
    }

    /// Editable when structure allows and safe mode is not Read-only.
    pub fn is_editable_for(&self, mode: SafeMode) -> bool {
        self.read_only.is_none() && self.changeset.is_some() && mode != SafeMode::ReadOnly
    }

    pub fn apply_connection_safe_mode(&mut self, mode: SafeMode) {
        self.connection_safe_mode = mode;
    }

    pub fn is_dirty(&self) -> bool {
        self.changeset.as_ref().is_some_and(|cs| !cs.is_empty()) || !self.overlay.is_empty()
    }

    pub fn pending_counts(&self) -> PendingCounts {
        let Some(cs) = &self.changeset else {
            return PendingCounts::default();
        };
        let mut counts = PendingCounts::default();
        for change in cs.changes() {
            match change {
                RowChange::Update { cells, .. } if !cells.is_empty() => counts.updated += 1,
                RowChange::Insert { .. } => counts.inserted += 1,
                RowChange::Delete { .. } => counts.deleted += 1,
                RowChange::Update { .. } => {}
            }
        }
        counts
    }

    /// Read-only strip, including safe-mode Read-only when structure would allow edits.
    pub fn strip_line_for(&self, mode: SafeMode) -> Option<&'static str> {
        if let Some(line) = self.read_only.as_ref().and_then(|r| r.strip_line()) {
            return Some(line);
        }
        if mode == SafeMode::ReadOnly {
            return Some("Read-only · safe mode on this connection.");
        }
        None
    }

    /// Commit disabled reasons, including safe-mode Read-only.
    pub fn commit_blocked_reason_for(&self, mode: SafeMode) -> Option<&'static str> {
        if mode == SafeMode::ReadOnly {
            return Some("Safe mode is Read-only.");
        }
        if self.committing {
            return Some("Committing…");
        }
        let Some(cs) = &self.changeset else {
            return Some("This table is read-only.");
        };
        if cs.is_empty() {
            return Some("No pending changes.");
        }
        for change in cs.changes() {
            if let RowChange::Insert { values, .. } = change {
                for pk in cs.pk_columns() {
                    if self.generated_columns.contains(pk) {
                        continue;
                    }
                    if !values.contains_key(pk) {
                        return Some("Set a primary key for the new row.");
                    }
                }
            }
        }
        // Empty insert values would fail ChangePlan::build.
        for change in cs.changes() {
            if let RowChange::Insert { values, .. } = change {
                if values.is_empty() {
                    return Some("Set a primary key for the new row.");
                }
            }
        }
        None
    }

    pub fn row_marker_for_server(&self, pk: &[Value]) -> RowMarker {
        let Some(cs) = &self.changeset else {
            return RowMarker::Clean;
        };
        for change in cs.changes() {
            match change {
                RowChange::Delete { pk: deleted } if pk_match(deleted, pk) => {
                    return RowMarker::Deleted;
                }
                RowChange::Update { pk: updated, cells }
                    if pk_match(updated, pk) && !cells.is_empty() =>
                {
                    return RowMarker::Dirty;
                }
                _ => {}
            }
        }
        RowMarker::Clean
    }

    pub fn row_marker_for_overlay(&self, insert_id: u64) -> RowMarker {
        if self
            .overlay
            .iter()
            .any(|o| o.insert_id == insert_id && o.duplicated)
        {
            RowMarker::Duplicated
        } else {
            RowMarker::Inserted
        }
    }

    /// Pending display value for a server cell, if dirty.
    pub fn pending_server_value(&self, pk: &[Value], column: &str) -> Option<&Value> {
        let cs = self.changeset.as_ref()?;
        for change in cs.changes() {
            match change {
                RowChange::Update { pk: updated, cells } if pk_match(updated, pk) => {
                    return cells.get(column).map(|c| &c.new_value);
                }
                RowChange::Delete { pk: deleted } if pk_match(deleted, pk) => {
                    // Show preserved edit under strikethrough when present.
                    let key = PkKey::from_values(pk);
                    return self
                        .preserved_updates
                        .get(&key)
                        .and_then(|cells| cells.get(column).map(|c| &c.new_value));
                }
                _ => {}
            }
        }
        None
    }

    /// Original value for a dirty cell (triangle tooltip).
    pub fn original_server_value(&self, pk: &[Value], column: &str) -> Option<&Value> {
        let cs = self.changeset.as_ref()?;
        for change in cs.changes() {
            if let RowChange::Update { pk: updated, cells } = change {
                if pk_match(updated, pk) {
                    return cells.get(column).map(|c| &c.old_value);
                }
            }
        }
        let key = PkKey::from_values(pk);
        self.preserved_updates
            .get(&key)
            .and_then(|cells| cells.get(column).map(|c| &c.old_value))
    }

    #[allow(dead_code)]
    pub fn is_server_cell_dirty(&self, pk: &[Value], column: &str) -> bool {
        matches!(self.row_marker_for_server(pk), RowMarker::Dirty)
            && self.pending_server_value(pk, column).is_some()
    }

    pub fn overlay_value(&self, insert_id: u64, column: &str) -> Option<&Value> {
        let cs = self.changeset.as_ref()?;
        for change in cs.changes() {
            if let RowChange::Insert { id, values } = change {
                if *id == insert_id {
                    return values.get(column);
                }
            }
        }
        None
    }

    pub fn is_generated(&self, column: &str) -> bool {
        self.generated_columns.contains(column)
    }

    pub fn column_kind(&self, column: &str) -> ColumnKind {
        ColumnKind::from_type_name(
            self.column_types
                .get(column)
                .map(|s| s.as_str())
                .unwrap_or(""),
        )
    }

    pub fn cell_editable(
        &self,
        column: &str,
        value: Option<&Value>,
        row_deleted: bool,
    ) -> Result<(), &'static str> {
        self.cell_editable_for(self.connection_safe_mode, column, value, row_deleted)
    }

    pub fn cell_editable_for(
        &self,
        mode: SafeMode,
        column: &str,
        value: Option<&Value>,
        row_deleted: bool,
    ) -> Result<(), &'static str> {
        if self.committing {
            return Err("Committing…");
        }
        if mode == SafeMode::ReadOnly {
            return Err("Read-only · safe mode on this connection.");
        }
        if !(self.read_only.is_none() && self.changeset.is_some()) {
            return Err(self
                .read_only
                .as_ref()
                .map(|r| r.status_line())
                .unwrap_or("This table is read-only."));
        }
        if row_deleted {
            return Err("This row is marked for delete. Choose Delete again to restore it.");
        }
        if self.is_generated(column) {
            return Err("Filled on commit.");
        }
        let kind = self.column_kind(column);
        if matches!(kind, ColumnKind::Bytes | ColumnKind::Other) {
            return Err(if matches!(kind, ColumnKind::Bytes) {
                "This cell is read-only. Use SQL to change it."
            } else {
                "This cell is read-only."
            });
        }
        if let Some(v) = value {
            if v.is_decode_error() || v.is_unsupported_placeholder() {
                return Err("This cell is read-only.");
            }
            // Multi-line / CR cells stay read-only until a popover editor exists.
            let display = v.display();
            if display.contains('\n') || display.contains('\r') {
                return Err("Use SQL to change it.");
            }
        }
        Ok(())
    }

    fn mutation_allowed(&self) -> Result<(), String> {
        self.mutation_allowed_for(self.connection_safe_mode)
    }

    fn mutation_allowed_for(&self, mode: SafeMode) -> Result<(), String> {
        if self.committing {
            return Err("Committing…".into());
        }
        if mode == SafeMode::ReadOnly {
            return Err("Read-only · safe mode on this connection.".into());
        }
        if !(self.read_only.is_none() && self.changeset.is_some()) {
            return Err(self
                .read_only
                .as_ref()
                .map(|r| r.status_line().to_string())
                .unwrap_or_else(|| "This table is read-only.".into()));
        }
        Ok(())
    }

    /// Open the in-cell editor. Captures PK + old value at begin time.
    #[allow(clippy::too_many_arguments)]
    pub fn begin_edit(
        &mut self,
        row: EditRowId,
        column: String,
        col_ix: usize,
        server_value: Option<&Value>,
        row_deleted: bool,
        pk: Option<Vec<Value>>,
        old_value: Option<Value>,
    ) -> Result<(), &'static str> {
        self.cell_editable(&column, server_value, row_deleted)?;
        let kind = self.column_kind(&column);
        // Unset (absent from an insert) is not NULL: empty draft, is_null false.
        // Real NULL stays is_null so an empty accept can store '' on text (#77).
        let started_unset = server_value.is_none();
        let (draft, is_null) = match server_value {
            None => (String::new(), false),
            Some(Value::Null) => (String::new(), true),
            Some(v) => (v.display().into_owned(), false),
        };
        let original_display = draft.clone();
        let started_as_null = is_null;
        let popover = matches!(kind, ColumnKind::Json);
        self.editor = Some(CellEditor {
            row,
            column,
            col_ix,
            kind,
            draft,
            is_null,
            original_display,
            started_as_null,
            started_unset,
            forced_null: false,
            pk,
            old_value,
            validation_error: None,
            popover,
        });
        Ok(())
    }

    pub fn cancel_editor(&mut self) {
        self.editor = None;
    }

    pub fn set_editor_null(&mut self) {
        if let Some(ed) = &mut self.editor {
            ed.is_null = true;
            ed.forced_null = true;
            ed.draft.clear();
            ed.validation_error = None;
        }
    }

    /// Accept the open editor into the ChangeSet. Returns Err with status text on validation failure.
    ///
    /// Uses the PK / old value captured at [`Self::begin_edit`]. Unchanged draft
    /// (same display + NULL state) closes without writing.
    pub fn accept_editor(&mut self) -> Result<(), String> {
        if self.committing {
            return Err("Committing…".into());
        }
        let mut editor = self
            .editor
            .as_ref()
            .ok_or_else(|| "No open editor.".to_string())?
            .clone();
        // Empty accept on an unset insert cell: leave the column out of INSERT
        // (DEFAULT applies). Ghost NULL still forces an explicit NULL (#77).
        if editor.started_unset && editor.draft.is_empty() && !editor.forced_null && !editor.is_null
        {
            self.editor = None;
            return Ok(());
        }
        // Text: empty field is `''`, not NULL. Opening on NULL leaves `is_null`
        // until accept; a plain empty accept (not the ghost NULL button) stores
        // the empty string and marks the cell dirty (07-grid-editing.md / #77).
        if matches!(editor.kind, ColumnKind::Text)
            && editor.is_null
            && editor.draft.is_empty()
            && !editor.forced_null
        {
            editor.is_null = false;
        }
        // Unchanged cell: close without set_cell (avoids Bool↔Int / Text↔typed drift).
        if editor.is_null == editor.started_as_null
            && (editor.is_null || editor.draft == editor.original_display)
        {
            self.editor = None;
            return Ok(());
        }
        let value = if editor.is_null {
            Value::Null
        } else {
            match parse_edit_value(&editor.draft, editor.kind, self.dialect) {
                Ok(v) => v,
                Err(msg) => {
                    if let Some(ed) = &mut self.editor {
                        ed.validation_error = Some(msg.clone());
                    }
                    return Err(msg);
                }
            }
        };
        match &editor.row {
            EditRowId::Server { .. } => {
                let pk = editor
                    .pk
                    .as_deref()
                    .ok_or_else(|| "Missing primary key.".to_string())?;
                let old = editor.old_value.clone().unwrap_or(Value::Null);
                let cs = self
                    .changeset
                    .as_mut()
                    .ok_or_else(|| "This table is read-only.".to_string())?;
                cs.set_cell(pk, &editor.column, old, value)
                    .map_err(|e| e.to_string())?;
            }
            EditRowId::Overlay { insert_id } => {
                let cs = self
                    .changeset
                    .as_mut()
                    .ok_or_else(|| "This table is read-only.".to_string())?;
                cs.set_insert_cell(*insert_id, &editor.column, value)
                    .map_err(|e| e.to_string())?;
            }
        }
        self.editor = None;
        self.commit_failed = false;
        Ok(())
    }

    pub fn insert_blank_row(&mut self) -> Result<u64, String> {
        self.mutation_allowed()?;
        let cs = self.changeset.as_mut().unwrap();
        let id = cs.insert_row(BTreeMap::new()).map_err(|e| e.to_string())?;
        self.overlay.push(OverlayRow {
            insert_id: id,
            duplicated: false,
            after_source_pk: None,
        });
        self.commit_failed = false;
        Ok(id)
    }

    /// Duplicate a server row: copy non-generated values; leave PK unset.
    ///
    /// `after_source_pk` is the source row's primary key so the overlay paints
    /// under that row when it is on the current page, otherwise at the end
    /// (07-grid-editing.md / #79). Anchored by stable row identity, not a page index.
    pub fn duplicate_server_row(
        &mut self,
        columns: &[(String, Value)],
        pk_columns: &[String],
        after_source_pk: Vec<Value>,
    ) -> Result<u64, String> {
        self.mutation_allowed()?;
        let mut values = BTreeMap::new();
        for (name, value) in columns {
            if self.generated_columns.contains(name) {
                continue;
            }
            if pk_columns.iter().any(|p| p == name) {
                continue;
            }
            values.insert(name.clone(), value.clone());
        }
        let cs = self.changeset.as_mut().unwrap();
        let id = cs.insert_row(values).map_err(|e| e.to_string())?;
        self.overlay.push(OverlayRow {
            insert_id: id,
            duplicated: true,
            after_source_pk: Some(after_source_pk),
        });
        self.commit_failed = false;
        Ok(id)
    }

    pub fn delete_or_restore_server(&mut self, pk: &[Value]) -> Result<RowMarker, String> {
        self.mutation_allowed()?;
        let marker = self.row_marker_for_server(pk);
        let cs = self.changeset.as_mut().unwrap();
        if marker == RowMarker::Deleted {
            // Undelete: restore preserved cell edits.
            cs.undelete_row(pk).map_err(|e| e.to_string())?;
            let key = PkKey::from_values(pk);
            if let Some(cells) = self.preserved_updates.remove(&key) {
                for (col, cell) in cells {
                    cs.set_cell(pk, &col, cell.old_value, cell.new_value)
                        .map_err(|e| e.to_string())?;
                }
            }
            self.commit_failed = false;
            return Ok(self.row_marker_for_server(pk));
        }
        // Preserve pending updates before delete replaces them.
        if let Some(RowChange::Update { cells, .. }) = cs.changes().iter().find(|c| match c {
            RowChange::Update { pk: p, .. } => pk_match(p, pk),
            _ => false,
        }) {
            self.preserved_updates
                .insert(PkKey::from_values(pk), cells.clone());
        }
        cs.delete_row(pk).map_err(|e| e.to_string())?;
        self.commit_failed = false;
        Ok(RowMarker::Deleted)
    }

    pub fn delete_overlay(&mut self, insert_id: u64) -> Result<(), String> {
        self.mutation_allowed()?;
        let cs = self.changeset.as_mut().unwrap();
        cs.delete_insert(insert_id).map_err(|e| e.to_string())?;
        self.overlay.retain(|o| o.insert_id != insert_id);
        self.commit_failed = false;
        Ok(())
    }

    /// Discard all pending edits (after confirm). Keeps editability / structure.
    pub fn discard(&mut self) {
        if let Some(cs) = &mut self.changeset {
            cs.clear();
        }
        self.overlay.clear();
        self.preserved_updates.clear();
        self.editor = None;
        self.preview_open = false;
        self.commit_failed = false;
        self.committing = false;
    }

    /// Mark this session as committing and bump the generation id. Returns the id
    /// the caller must match when the async commit completes.
    pub fn begin_commit(&mut self) -> u64 {
        self.committing = true;
        self.commit_failed = false;
        self.commit_generation = self.commit_generation.wrapping_add(1);
        self.commit_generation
    }

    /// Successful commit: clear set and close sheet. Caller reloads the page.
    pub fn note_commit_success(&mut self, generation: u64) -> bool {
        if self.commit_generation != generation {
            return false;
        }
        self.discard();
        true
    }

    /// Failed commit: keep edits; do not reload. Caller sets banner/status.
    pub fn note_commit_failure(&mut self, generation: u64) -> bool {
        if self.commit_generation != generation {
            return false;
        }
        self.committing = false;
        self.commit_failed = true;
        true
    }

    pub fn build_plan(&self) -> Result<ChangePlan, ChangeError> {
        let cs = self.changeset.as_ref().ok_or(ChangeError::NoPrimaryKey)?;
        ChangePlan::build(cs, self.dialect)
    }

    pub fn preview_sheet_text(&self) -> Result<String, String> {
        let plan = self.build_plan().map_err(|e| e.to_string())?;
        Ok(format_preview_sheet(&plan))
    }

    #[allow(dead_code)]
    pub fn overlay_row_count(&self) -> usize {
        self.overlay.len()
    }

    pub fn schema_table_label(&self) -> String {
        self.changeset
            .as_ref()
            .map(|cs| format!("{}.{}", cs.table().schema, cs.table().table))
            .unwrap_or_else(|| "table".into())
    }
}

fn column_type_map(structure: &TableStructure) -> HashMap<String, String> {
    structure
        .columns
        .iter()
        .map(|c| (c.name.clone(), c.type_name.clone()))
        .collect()
}

/// Generated / identity columns are not editable (07-grid-editing.md).
pub(crate) fn detect_generated_columns(
    structure: &TableStructure,
    dialect: Dialect,
) -> HashSet<String> {
    let pk = structure.primary_key_columns().unwrap_or_default();
    let mut out = HashSet::new();
    for col in &structure.columns {
        if col.generated || column_looks_generated(col) {
            out.insert(col.name.clone());
            continue;
        }
        // SQLite INTEGER PRIMARY KEY rowid alias (single-column INTEGER PK).
        if dialect == Dialect::Sqlite
            && pk.len() == 1
            && pk[0] == col.name
            && col.type_name.trim().eq_ignore_ascii_case("INTEGER")
        {
            out.insert(col.name.clone());
        }
    }
    out
}

fn column_looks_generated(col: &ColumnStructure) -> bool {
    let t = col.type_name.to_ascii_uppercase();
    let d = col.default.as_deref().unwrap_or("").to_ascii_uppercase();
    t.contains("SERIAL")
        || t.contains("IDENTITY")
        || t.contains("AUTO_INCREMENT")
        || t.contains("AUTOINCREMENT")
        || d.contains("GENERATED")
        || d.contains("NEXTVAL")
        || d.contains("AUTOINCREMENT")
}

fn pk_match(a: &[Value], b: &[Value]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b.iter()).all(|(x, y)| match (x, y) {
            (Value::Float(l), Value::Float(r)) if l.is_nan() && r.is_nan() => true,
            _ => x == y,
        })
}

/// Parse editor text into a [`Value`]. Empty text for Text → `''`; other kinds error.
pub(crate) fn parse_edit_value(
    raw: &str,
    kind: ColumnKind,
    dialect: Dialect,
) -> Result<Value, String> {
    let type_label = kind_label(kind);
    match kind {
        ColumnKind::Text => Ok(Value::text(raw)),
        ColumnKind::Bool => {
            if let Some(b) = parse_bool(raw) {
                Ok(Value::Bool(b))
            } else {
                Err(format!("Enter a valid {type_label}, or set NULL."))
            }
        }
        ColumnKind::Int => raw
            .trim()
            .parse::<i64>()
            .map(Value::Int)
            .map_err(|_| format!("Enter a valid {type_label}, or set NULL.")),
        ColumnKind::Float => {
            let n: f64 = raw
                .trim()
                .parse()
                .map_err(|_| format!("Enter a valid {type_label}, or set NULL."))?;
            if !n.is_finite() {
                match dialect {
                    Dialect::Postgres => Ok(Value::Float(n)),
                    Dialect::Mysql | Dialect::Sqlite => {
                        Err(format!("Enter a valid {type_label}, or set NULL."))
                    }
                }
            } else {
                Ok(Value::Float(n))
            }
        }
        ColumnKind::Decimal => Decimal::from_str_exact(raw.trim())
            .or_else(|_| Decimal::from_str(raw.trim()))
            .map(Value::Decimal)
            .map_err(|_| format!("Enter a valid {type_label}, or set NULL.")),
        ColumnKind::Uuid => Uuid::parse_str(raw.trim())
            .map(Value::Uuid)
            .map_err(|_| format!("Enter a valid {type_label}, or set NULL.")),
        ColumnKind::Date => chrono::NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d")
            .map(Value::Date)
            .map_err(|_| format!("Enter a valid {type_label}, or set NULL.")),
        ColumnKind::Json => {
            serde_json::from_str::<serde_json::Value>(raw.trim())
                .map_err(|_| format!("Enter a valid {type_label}, or set NULL."))?;
            Ok(Value::json_raw(raw.trim()))
        }
        ColumnKind::Time => parse_time_value(raw.trim())
            .ok_or_else(|| format!("Enter a valid {type_label}, or set NULL.")),
        ColumnKind::Timestamp => parse_timestamp_value(raw.trim())
            .ok_or_else(|| format!("Enter a valid {type_label}, or set NULL.")),
        ColumnKind::Interval => {
            if raw.trim().is_empty() {
                Err(format!("Enter a valid {type_label}, or set NULL."))
            } else {
                // Interval grammar is engine-specific; keep readable text until typed parse.
                Ok(Value::Other {
                    type_name: "interval".into(),
                    text: Arc::from(raw.trim()),
                })
            }
        }
        ColumnKind::Bytes | ColumnKind::Other => Err("This cell is read-only.".into()),
    }
}

fn parse_time_value(raw: &str) -> Option<Value> {
    use chrono::NaiveTime;
    const FORMATS: &[&str] = &["%H:%M:%S%.f", "%H:%M:%S", "%H:%M"];
    for fmt in FORMATS {
        if let Ok(t) = NaiveTime::parse_from_str(raw, fmt) {
            return Some(Value::Time(t));
        }
    }
    None
}

fn parse_timestamp_value(raw: &str) -> Option<Value> {
    use chrono::{DateTime, NaiveDateTime};
    if let Ok(dt) = DateTime::parse_from_rfc3339(raw) {
        return Some(Value::TimestampTz(dt));
    }
    if let Ok(dt) = DateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S%.f%z") {
        return Some(Value::TimestampTz(dt));
    }
    const FORMATS: &[&str] = &[
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
    ];
    for fmt in FORMATS {
        if let Ok(dt) = NaiveDateTime::parse_from_str(raw, fmt) {
            return Some(Value::Timestamp(dt));
        }
    }
    None
}

fn kind_label(kind: ColumnKind) -> &'static str {
    match kind {
        ColumnKind::Bool => "bool",
        ColumnKind::Int => "integer",
        ColumnKind::Float => "float",
        ColumnKind::Decimal => "decimal",
        ColumnKind::Date => "date",
        ColumnKind::Time => "time",
        ColumnKind::Timestamp => "timestamp",
        ColumnKind::Interval => "interval",
        ColumnKind::Text => "text",
        ColumnKind::Uuid => "uuid",
        ColumnKind::Json => "json",
        ColumnKind::Bytes => "bytes",
        ColumnKind::Other => "value",
    }
}

fn parse_bool(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" | "t" | "yes" | "y" | "1" => Some(true),
        "false" | "f" | "no" | "n" | "0" => Some(false),
        _ => None,
    }
}

use std::str::FromStr;

/// Format the Preview SQL sheet: placeholders + bound values (08-preview-commit.md).
pub(crate) fn format_preview_sheet(plan: &ChangePlan) -> String {
    let mut out = String::new();
    for stmt in &plan.statements {
        out.push_str(&preview_comment(stmt));
        out.push('\n');
        out.push_str(&stmt.sql);
        out.push('\n');
        for (i, param) in stmt.params.iter().enumerate() {
            let ph = bind_placeholder(plan.dialect, i);
            out.push_str(&ph);
            out.push_str(" = ");
            out.push_str(&param_display(param, plan.dialect));
            out.push('\n');
        }
        out.push('\n');
    }
    out
}

fn preview_comment(stmt: &PlannedStatement) -> String {
    let op = match stmt.op {
        PlanOp::Delete => "delete",
        PlanOp::Update => "update",
        PlanOp::Insert => "insert",
    };
    if stmt.pk.is_empty() {
        format!("-- {op}")
    } else {
        let key = stmt
            .pk
            .iter()
            .map(|v| v.display().into_owned())
            .collect::<Vec<_>>()
            .join(", ");
        format!("-- {op} · {key}")
    }
}

fn param_display(value: &Value, dialect: Dialect) -> String {
    match value_to_copy_cell(value) {
        CopyCell::Null => "NULL".into(),
        CopyCell::Value(text, CopyScalarKind::Number | CopyScalarKind::Bool) => text,
        CopyCell::Value(text, CopyScalarKind::Text) => escape_sql_string(&text, dialect),
        CopyCell::Decode(text) => escape_sql_string(&text, dialect),
        CopyCell::NotLoaded | CopyCell::Unset => "NULL".into(),
    }
}

/// Status / banner text for a failed commit (keeps edits; no reload).
///
/// Banner and status both number statements from 1 (08-preview-commit.md).
pub(crate) fn format_commit_failure(err: &DbError, statement_count: usize) -> (String, String) {
    match err {
        DbError::ChangeConflict {
            statement_index,
            message,
            ..
        } => {
            let n = statement_index + 1;
            let banner = message.clone();
            let status =
                format!("Commit failed · statement {n} of {statement_count} · changes kept");
            (banner, status)
        }
        _ => (err.to_string(), "Commit failed · changes kept".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sela_core::{ColumnStructure, IndexInfo, TableRef};

    fn books_structure() -> TableStructure {
        TableStructure {
            schema: "public".into(),
            table: "books".into(),
            columns: vec![
                ColumnStructure {
                    name: "id".into(),
                    type_name: "integer".into(),
                    nullable: false,
                    default: Some("nextval('books_id_seq')".into()),
                    comment: None,
                    generated: false,
                },
                ColumnStructure {
                    name: "title".into(),
                    type_name: "text".into(),
                    nullable: true,
                    default: None,
                    comment: None,
                    generated: false,
                },
                ColumnStructure {
                    name: "pages".into(),
                    type_name: "integer".into(),
                    nullable: true,
                    default: None,
                    comment: None,
                    generated: false,
                },
            ],
            indexes: vec![IndexInfo {
                name: "books_pkey".into(),
                columns: vec!["id".into()],
                unique: true,
                primary: true,
                definition: None,
            }],
            foreign_keys: vec![],
            ddl: String::new(),
        }
    }

    fn no_pk_structure() -> TableStructure {
        let mut s = books_structure();
        s.indexes.clear();
        s
    }

    #[test]
    fn no_primary_key_is_read_only_with_strip() {
        let session =
            EditSession::from_structure(&no_pk_structure(), Dialect::Postgres, RelationKind::Table);
        assert!(!session.is_editable_for(session.connection_safe_mode));
        assert_eq!(
            session.strip_line_for(session.connection_safe_mode),
            Some("Read-only · this table has no primary key.")
        );
    }

    #[test]
    fn view_is_read_only() {
        let session =
            EditSession::from_structure(&books_structure(), Dialect::Postgres, RelationKind::View);
        assert_eq!(
            session.strip_line_for(session.connection_safe_mode),
            Some("Read-only · views are read-only.")
        );
    }

    #[test]
    fn dirty_tracking_and_row_markers() {
        let mut session =
            EditSession::from_structure(&books_structure(), Dialect::Postgres, RelationKind::Table);
        let pk = vec![Value::Int(1)];
        assert_eq!(session.row_marker_for_server(&pk), RowMarker::Clean);

        session
            .changeset
            .as_mut()
            .unwrap()
            .set_cell(&pk, "title", Value::text("old"), Value::text("new"))
            .unwrap();
        assert_eq!(session.row_marker_for_server(&pk), RowMarker::Dirty);
        assert!(session.is_server_cell_dirty(&pk, "title"));
        assert_eq!(
            session.pending_server_value(&pk, "title"),
            Some(&Value::text("new"))
        );
        assert_eq!(session.pending_counts().updated, 1);
        assert_eq!(session.pending_counts().total(), 1);
        assert!(session.is_dirty());

        // Revert to original clears dirty.
        session
            .changeset
            .as_mut()
            .unwrap()
            .set_cell(&pk, "title", Value::text("old"), Value::text("old"))
            .unwrap();
        assert_eq!(session.row_marker_for_server(&pk), RowMarker::Clean);
        assert!(!session.is_dirty());
    }

    #[test]
    fn insert_duplicate_delete_overlay_markers() {
        let mut session =
            EditSession::from_structure(&books_structure(), Dialect::Postgres, RelationKind::Table);
        let id = session.insert_blank_row().unwrap();
        assert_eq!(session.row_marker_for_overlay(id), RowMarker::Inserted);
        assert_eq!(session.pending_counts().inserted, 1);

        let dup = session
            .duplicate_server_row(
                &[
                    ("id".into(), Value::Int(3)),
                    ("title".into(), Value::text("Hobbit")),
                    ("pages".into(), Value::Int(310)),
                ],
                &["id".into()],
                vec![Value::Int(3)],
            )
            .unwrap();
        assert_eq!(session.row_marker_for_overlay(dup), RowMarker::Duplicated);
        // PK left unset on duplicate.
        assert!(session.overlay_value(dup, "id").is_none());
        assert_eq!(
            session.overlay_value(dup, "title"),
            Some(&Value::text("Hobbit"))
        );
        let dup_overlay = session.overlay.iter().find(|o| o.insert_id == dup).unwrap();
        assert_eq!(
            dup_overlay.after_source_pk.as_deref(),
            Some([Value::Int(3)].as_slice())
        );
        // Insert stays after the page; duplicate sits under server row 2 (pk=3).
        let page_pks = [
            Some(vec![Value::Int(1)]),
            Some(vec![Value::Int(2)]),
            Some(vec![Value::Int(3)]),
            Some(vec![Value::Int(4)]),
        ];
        let map = visual_row_map_for_overlays(4, &session.overlay, &page_pks);
        assert_eq!(
            map,
            vec![
                VisualRowRef::Server { row_ix: 0 },
                VisualRowRef::Server { row_ix: 1 },
                VisualRowRef::Server { row_ix: 2 },
                VisualRowRef::Overlay { overlay_ix: 1 },
                VisualRowRef::Server { row_ix: 3 },
                VisualRowRef::Overlay { overlay_ix: 0 },
            ]
        );

        session.delete_overlay(id).unwrap();
        assert!(!session.overlay.iter().any(|o| o.insert_id == id));
    }

    #[test]
    fn duplicate_overlay_sits_under_source_row() {
        let mut session =
            EditSession::from_structure(&books_structure(), Dialect::Postgres, RelationKind::Table);
        session.insert_blank_row().unwrap();
        session
            .duplicate_server_row(
                &[
                    ("id".into(), Value::Int(1)),
                    ("title".into(), Value::text("a")),
                ],
                &["id".into()],
                vec![Value::Int(1)],
            )
            .unwrap();
        session
            .duplicate_server_row(
                &[
                    ("id".into(), Value::Int(3)),
                    ("title".into(), Value::text("c")),
                ],
                &["id".into()],
                vec![Value::Int(3)],
            )
            .unwrap();
        // overlay: [insert, dup@pk1, dup@pk3]
        let page_pks = [
            Some(vec![Value::Int(1)]),
            Some(vec![Value::Int(2)]),
            Some(vec![Value::Int(3)]),
        ];
        let map = visual_row_map_for_overlays(3, &session.overlay, &page_pks);
        assert_eq!(
            map,
            vec![
                VisualRowRef::Server { row_ix: 0 },
                VisualRowRef::Overlay { overlay_ix: 1 },
                VisualRowRef::Server { row_ix: 1 },
                VisualRowRef::Server { row_ix: 2 },
                VisualRowRef::Overlay { overlay_ix: 2 },
                VisualRowRef::Overlay { overlay_ix: 0 },
            ]
        );
    }

    #[test]
    fn out_of_range_anchor_places_overlay_at_end() {
        // Page shrunk / short last page: never drop the overlay.
        let map = visual_row_map(3, &[Some(50), None]);
        assert_eq!(
            map,
            vec![
                VisualRowRef::Server { row_ix: 0 },
                VisualRowRef::Server { row_ix: 1 },
                VisualRowRef::Server { row_ix: 2 },
                VisualRowRef::Overlay { overlay_ix: 0 },
                VisualRowRef::Overlay { overlay_ix: 1 },
            ]
        );
    }

    #[test]
    fn anchor_pk_missing_from_page_places_at_end() {
        let overlays = [OverlayRow {
            insert_id: 1,
            duplicated: true,
            after_source_pk: Some(vec![Value::Int(99)]),
        }];
        let page_pks = [Some(vec![Value::Int(1)]), Some(vec![Value::Int(2)])];
        let anchors = resolve_overlay_anchors(&overlays, &page_pks);
        assert_eq!(anchors, vec![None]);
        let map = visual_row_map_for_overlays(2, &overlays, &page_pks);
        assert_eq!(
            map,
            vec![
                VisualRowRef::Server { row_ix: 0 },
                VisualRowRef::Server { row_ix: 1 },
                VisualRowRef::Overlay { overlay_ix: 0 },
            ]
        );
    }

    #[test]
    fn visual_row_map_identity_when_no_overlays() {
        assert_eq!(
            visual_row_map(3, &[]),
            vec![
                VisualRowRef::Server { row_ix: 0 },
                VisualRowRef::Server { row_ix: 1 },
                VisualRowRef::Server { row_ix: 2 },
            ]
        );
    }

    #[test]
    fn empty_accept_on_unset_insert_cell_is_noop() {
        let mut session =
            EditSession::from_structure(&books_structure(), Dialect::Postgres, RelationKind::Table);
        let id = session.insert_blank_row().unwrap();
        // title is unset on the blank insert (DEFAULT would apply).
        session
            .begin_edit(
                EditRowId::Overlay { insert_id: id },
                "title".into(),
                1,
                None,
                false,
                None,
                None,
            )
            .unwrap();
        assert!(session.editor.as_ref().unwrap().started_unset);
        assert!(!session.editor.as_ref().unwrap().is_null);
        session.accept_editor().unwrap();
        assert!(session.overlay_value(id, "title").is_none());
    }

    #[test]
    fn delete_preserves_edits_and_undelete_restores() {
        let mut session =
            EditSession::from_structure(&books_structure(), Dialect::Postgres, RelationKind::Table);
        let pk = vec![Value::Int(2)];
        session
            .changeset
            .as_mut()
            .unwrap()
            .set_cell(&pk, "title", Value::text("a"), Value::text("b"))
            .unwrap();
        assert_eq!(session.row_marker_for_server(&pk), RowMarker::Dirty);

        session.delete_or_restore_server(&pk).unwrap();
        assert_eq!(session.row_marker_for_server(&pk), RowMarker::Deleted);
        // Pending counts: delete only (update replaced).
        assert_eq!(session.pending_counts().deleted, 1);
        assert_eq!(session.pending_counts().updated, 0);
        // Preserved value still visible for paint.
        assert_eq!(
            session.pending_server_value(&pk, "title"),
            Some(&Value::text("b"))
        );

        session.delete_or_restore_server(&pk).unwrap();
        assert_eq!(session.row_marker_for_server(&pk), RowMarker::Dirty);
        assert_eq!(
            session.pending_server_value(&pk, "title"),
            Some(&Value::text("b"))
        );
    }

    #[test]
    fn discard_clears_all_pending() {
        let mut session =
            EditSession::from_structure(&books_structure(), Dialect::Postgres, RelationKind::Table);
        let pk = vec![Value::Int(1)];
        session
            .changeset
            .as_mut()
            .unwrap()
            .set_cell(&pk, "title", Value::text("a"), Value::text("b"))
            .unwrap();
        session.insert_blank_row().unwrap();
        session.preview_open = true;
        session.commit_failed = true;
        assert!(session.is_dirty());

        session.discard();
        assert!(!session.is_dirty());
        assert!(session.overlay.is_empty());
        assert!(!session.preview_open);
        assert!(!session.commit_failed);
        assert!(session.changeset.as_ref().unwrap().is_empty());
    }

    #[test]
    fn failed_commit_keeps_edits() {
        let mut session =
            EditSession::from_structure(&books_structure(), Dialect::Postgres, RelationKind::Table);
        let pk = vec![Value::Int(1)];
        session
            .changeset
            .as_mut()
            .unwrap()
            .set_cell(&pk, "title", Value::text("a"), Value::text("b"))
            .unwrap();
        let gen = session.begin_commit();
        assert!(session.note_commit_failure(gen));
        assert!(session.commit_failed);
        assert!(!session.committing);
        assert!(session.is_dirty());
        assert_eq!(
            session.pending_server_value(&pk, "title"),
            Some(&Value::text("b"))
        );
    }

    #[test]
    fn null_and_empty_text_are_distinct() {
        assert_eq!(
            parse_edit_value("", ColumnKind::Text, Dialect::Sqlite).unwrap(),
            Value::text("")
        );
        assert!(parse_edit_value("", ColumnKind::Int, Dialect::Sqlite).is_err());
        let mut session =
            EditSession::from_structure(&books_structure(), Dialect::Postgres, RelationKind::Table);
        session
            .begin_edit(
                EditRowId::Server { row_ix: 0 },
                "title".into(),
                1,
                Some(&Value::Null),
                false,
                Some(vec![Value::Int(1)]),
                Some(Value::Null),
            )
            .unwrap();
        assert!(session.editor.as_ref().unwrap().is_null);
        // Empty accept on a NULL text cell stores '' (#77).
        session.accept_editor().unwrap();
        assert_eq!(
            session.pending_server_value(&[Value::Int(1)], "title"),
            Some(&Value::text(""))
        );
    }

    #[test]
    fn ghost_null_button_keeps_null_on_null_cell() {
        let mut session =
            EditSession::from_structure(&books_structure(), Dialect::Postgres, RelationKind::Table);
        session
            .begin_edit(
                EditRowId::Server { row_ix: 0 },
                "title".into(),
                1,
                Some(&Value::Null),
                false,
                Some(vec![Value::Int(1)]),
                Some(Value::Null),
            )
            .unwrap();
        session.set_editor_null();
        session.accept_editor().unwrap();
        assert!(!session.is_server_cell_dirty(&[Value::Int(1)], "title"));
    }

    #[test]
    fn ghost_null_button_nulls_non_null_text() {
        let mut session =
            EditSession::from_structure(&books_structure(), Dialect::Postgres, RelationKind::Table);
        session
            .begin_edit(
                EditRowId::Server { row_ix: 0 },
                "title".into(),
                1,
                Some(&Value::text("hello")),
                false,
                Some(vec![Value::Int(1)]),
                Some(Value::text("hello")),
            )
            .unwrap();
        session.set_editor_null();
        session.accept_editor().unwrap();
        assert_eq!(
            session.pending_server_value(&[Value::Int(1)], "title"),
            Some(&Value::Null)
        );
    }

    #[test]
    fn gutter_tooltip_strings_match_ux() {
        assert_eq!(RowMarker::Inserted.gutter_tooltip(), Some("New row"));
        assert_eq!(
            RowMarker::Duplicated.gutter_tooltip(),
            Some("Duplicated row")
        );
        assert_eq!(RowMarker::Dirty.gutter_tooltip(), Some("Edited row"));
        assert_eq!(RowMarker::Deleted.gutter_tooltip(), Some("Deleted row"));
        assert_eq!(RowMarker::Clean.gutter_tooltip(), None);
    }

    #[test]
    fn commit_blocked_until_pk_set_on_insert() {
        let mut session =
            EditSession::from_structure(&books_structure(), Dialect::Postgres, RelationKind::Table);
        // id is generated (nextval) — does not block.
        session.insert_blank_row().unwrap();
        // Empty insert with only generated PK still blocks via EmptyInsert / empty values.
        assert!(session
            .commit_blocked_reason_for(session.connection_safe_mode)
            .is_some());

        // Manual non-generated PK table.
        let mut structure = books_structure();
        structure.columns[0].default = None;
        structure.columns[0].type_name = "text".into();
        let mut session =
            EditSession::from_structure(&structure, Dialect::Postgres, RelationKind::Table);
        assert!(!session.is_generated("id"));
        let id = session.insert_blank_row().unwrap();
        assert_eq!(
            session.commit_blocked_reason_for(session.connection_safe_mode),
            Some("Set a primary key for the new row.")
        );
        session
            .changeset
            .as_mut()
            .unwrap()
            .set_insert_cell(id, "id", Value::text("x"))
            .unwrap();
        assert_eq!(
            session.commit_blocked_reason_for(session.connection_safe_mode),
            None
        );

        session.apply_connection_safe_mode(SafeMode::ReadOnly);
        assert_eq!(
            session.commit_blocked_reason_for(session.connection_safe_mode),
            Some("Safe mode is Read-only.")
        );
        assert_eq!(
            session.strip_line_for(session.connection_safe_mode),
            Some("Read-only · safe mode on this connection.")
        );
        assert!(!session.is_editable_for(session.connection_safe_mode));
        session.apply_connection_safe_mode(SafeMode::Confirm);
        assert_eq!(
            session.commit_blocked_reason_for(session.connection_safe_mode),
            None
        );
        assert!(session.is_editable_for(session.connection_safe_mode));
    }

    #[test]
    fn preview_sheet_lists_placeholders_and_values() {
        let mut cs = ChangeSet::try_new(
            TableRef::new("public", "books"),
            vec!["id".into()],
            vec!["id".into(), "title".into()],
        )
        .unwrap();
        cs.set_cell(
            &[Value::Int(3)],
            "title",
            Value::text("old"),
            Value::text("The Hobbit"),
        )
        .unwrap();
        let plan = ChangePlan::build(&cs, Dialect::Postgres).unwrap();
        let text = format_preview_sheet(&plan);
        assert!(text.contains("-- update · 3"));
        assert!(text.contains("$1"));
        assert!(text.contains("The Hobbit"));
        assert!(text.contains("WHERE"));
    }

    #[test]
    fn change_conflict_status_names_statement() {
        let err =
            DbError::change_conflict(&TableRef::new("public", "books"), 1, 0, &[Value::Int(9)]);
        let (banner, status) = format_commit_failure(&err, 4);
        assert!(banner.contains("No row matched public.books for this key."));
        assert!(status.contains("statement 2 of 4"));
        assert!(status.contains("changes kept"));
    }

    #[test]
    fn cancel_editor_leaves_changeset() {
        let mut session =
            EditSession::from_structure(&books_structure(), Dialect::Postgres, RelationKind::Table);
        session
            .begin_edit(
                EditRowId::Server { row_ix: 0 },
                "title".into(),
                1,
                Some(&Value::text("a")),
                false,
                Some(vec![Value::Int(1)]),
                Some(Value::text("a")),
            )
            .unwrap();
        if let Some(ed) = &mut session.editor {
            ed.draft = "changed".into();
        }
        session.cancel_editor();
        assert!(session.editor.is_none());
        assert!(!session.is_dirty());
    }

    #[test]
    fn edits_rejected_while_committing() {
        let mut session =
            EditSession::from_structure(&books_structure(), Dialect::Postgres, RelationKind::Table);
        let _ = session.begin_commit();
        assert_eq!(
            session.begin_edit(
                EditRowId::Server { row_ix: 0 },
                "title".into(),
                1,
                Some(&Value::text("a")),
                false,
                Some(vec![Value::Int(1)]),
                Some(Value::text("a")),
            ),
            Err("Committing…")
        );
        assert!(session.insert_blank_row().is_err());
        assert!(session.delete_or_restore_server(&[Value::Int(1)]).is_err());
    }

    #[test]
    fn accepting_unchanged_cell_is_noop() {
        let mut session =
            EditSession::from_structure(&books_structure(), Dialect::Postgres, RelationKind::Table);
        session
            .begin_edit(
                EditRowId::Server { row_ix: 0 },
                "title".into(),
                1,
                Some(&Value::text("same")),
                false,
                Some(vec![Value::Int(1)]),
                Some(Value::text("same")),
            )
            .unwrap();
        session.accept_editor().unwrap();
        assert!(!session.is_dirty());
        assert!(session.editor.is_none());
    }

    #[test]
    fn editor_keeps_pk_across_accept() {
        let mut session =
            EditSession::from_structure(&books_structure(), Dialect::Postgres, RelationKind::Table);
        let pk = vec![Value::Int(42)];
        session
            .begin_edit(
                EditRowId::Server { row_ix: 0 },
                "title".into(),
                1,
                Some(&Value::text("old")),
                false,
                Some(pk.clone()),
                Some(Value::text("old")),
            )
            .unwrap();
        if let Some(ed) = &mut session.editor {
            ed.draft = "new".into();
        }
        session.accept_editor().unwrap();
        assert_eq!(
            session.pending_server_value(&pk, "title"),
            Some(&Value::text("new"))
        );
    }

    #[test]
    fn parsing_per_type() {
        assert_eq!(
            parse_edit_value("12:30:00", ColumnKind::Time, Dialect::Postgres).unwrap(),
            Value::Time(chrono::NaiveTime::from_hms_opt(12, 30, 0).unwrap())
        );
        assert!(matches!(
            parse_edit_value(
                "2024-01-02 03:04:05",
                ColumnKind::Timestamp,
                Dialect::Postgres
            )
            .unwrap(),
            Value::Timestamp(_)
        ));
        assert!(matches!(
            parse_edit_value(
                "2024-01-02T03:04:05Z",
                ColumnKind::Timestamp,
                Dialect::Postgres
            )
            .unwrap(),
            Value::TimestampTz(_)
        ));
        assert_eq!(
            parse_edit_value("2024-01-02", ColumnKind::Date, Dialect::Postgres).unwrap(),
            Value::Date(chrono::NaiveDate::from_ymd_opt(2024, 1, 2).unwrap())
        );
        assert!(matches!(
            parse_edit_value(
                "550e8400-e29b-41d4-a716-446655440000",
                ColumnKind::Uuid,
                Dialect::Postgres
            )
            .unwrap(),
            Value::Uuid(_)
        ));
    }

    #[test]
    fn generated_column_detection_uses_flag() {
        let mut structure = books_structure();
        structure.columns[0].generated = true;
        structure.columns[0].default = None;
        structure.columns[0].type_name = "integer".into();
        let session =
            EditSession::from_structure(&structure, Dialect::Postgres, RelationKind::Table);
        assert!(session.is_generated("id"));
        // Without the flag or SERIAL/nextval heuristics, plain integer PK is not generated
        // except SQLite INTEGER PRIMARY KEY alias (not this dialect path).
        let mut plain = books_structure();
        plain.columns[0].generated = false;
        plain.columns[0].default = None;
        plain.columns[0].type_name = "integer".into();
        let session = EditSession::from_structure(&plain, Dialect::Postgres, RelationKind::Table);
        assert!(!session.is_generated("id"));
    }

    #[test]
    fn multiline_cell_is_read_only() {
        let session =
            EditSession::from_structure(&books_structure(), Dialect::Postgres, RelationKind::Table);
        let err = session
            .cell_editable("title", Some(&Value::text("a\nb")), false)
            .unwrap_err();
        assert_eq!(err, "Use SQL to change it.");
    }

    #[test]
    fn pending_summary_omits_zero_counts() {
        let counts = PendingCounts {
            updated: 2,
            inserted: 1,
            deleted: 0,
        };
        assert_eq!(counts.summary_left(), "3 pending · 2 updated, 1 inserted");
    }
}
