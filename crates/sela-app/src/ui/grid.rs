//! Virtualized result grid backed by gpui-component's `DataTable`.
//!
//! Rows come from [`sela_core::ResultWindow`]: the grid asks by absolute index and
//! paints a placeholder while the page is loading. Display strings are truncated
//! and cached inside the window; copy uses the full [`Value::display`].
//!
//! Header sort: `TableState.sortable` must be on for gpui-component to deliver
//! header clicks (there is no separate click API). [`GridDelegate::perform_sort`]
//! never reorders local rows — it only records a pending server-side ORDER BY.
//!
//! Selection (M1-7): cell / row / column via DataTable; optional Shift range on
//! [`selection_rect`]. Copy formats live in [`sela_core::CopyFormat`].

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::Input;
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::table::{Column, ColumnSort, TableDelegate, TableSelection, TableState};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, Sizable as _};
use gpui_kit::prelude::*;
use gpui_kit::{div, px, App, IntoElement, MouseButton, SharedString, Window};

use std::collections::BTreeMap;

use sela_core::{
    value_to_copy_cell, CellPos, CellView, CopyCell, QueryPage, ResultWindow, RowState,
    SelectionRect, SortDirection, SortSpec, Value,
};

use super::theme::SelaPalette;
use super::workspace::edit_session::{visual_row_map, EditRowId, RowMarker, VisualRowRef};
use super::{
    CopyAsCsv, CopyAsInsert, CopyAsJson, CopyAsTsv, CopySelection, SetCellNull,
    CELL_EDITOR_KEY_CONTEXT,
};

/// Paint state synced from [`super::workspace::edit_session::EditSession`].
#[derive(Debug, Clone, Default)]
pub struct EditPaintState {
    pub server: BTreeMap<usize, EditPaintRow>,
    pub overlays: Vec<OverlayPaintRow>,
    pub editing: Option<(EditRowId, usize)>,
    pub editor_error: Option<String>,
    /// Open cell editor input (when present, render inside the focused cell).
    pub editor_input: Option<gpui_kit::Entity<gpui_kit::component::input::InputState>>,
}

#[derive(Debug, Clone)]
pub struct EditPaintRow {
    pub marker: RowMarker,
    pub dirty_display: BTreeMap<String, String>,
    pub original_display: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct OverlayPaintRow {
    #[allow(dead_code)]
    pub insert_id: u64,
    pub marker: RowMarker,
    /// Typed pending values (not display strings). Absent key = unset.
    pub raw: BTreeMap<String, Value>,
    /// `Some(server_row)` → paint under that server row; `None` → after the page.
    pub after_server_row: Option<usize>,
    pub required_unset: Vec<String>,
    pub generated: Vec<String>,
}

enum CellKind {
    Loading,
    Null,
    Decode(String),
    Normal,
}

pub struct GridDelegate {
    pub result: ResultWindow,
    /// Page indices requested by the last viewport touch (M1-2b will fetch these).
    pub pending_pages: Vec<usize>,
    /// Column header sort requested by the user (table tabs only).
    /// `shift` is true when the click was a Shift-click (multi-sort).
    pub pending_sort: Option<(usize, bool)>,
    /// When true, column headers offer sort clicks (table Data view only).
    pub sortable: bool,
    /// Current sort specs (drives header sort marks ▲1 / ▼2).
    pub sorts: Vec<SortSpec>,
    pub loading: bool,
    pub empty_message: SharedString,
    /// Anchor for Shift-extended rectangular selection.
    pub selection_anchor: Option<CellPos>,
    /// Active rectangular selection (overrides a lone cell when present).
    pub selection_rect: Option<SelectionRect>,
    /// Shift held on the last left mouse-down inside a cell. Prefer this over
    /// `window.modifiers()` when handling [`TableEvent::SelectCell`], because
    /// the click subscriber can run after modifiers have already changed.
    /// Also set by Shift+arrow handlers before they call `set_selected_cell`.
    pointer_shift: Option<bool>,
    /// When true, the next [`TableEvent::SelectCell`] must not call
    /// [`Self::note_cell_focus`] with `shift = false` (which would collapse an
    /// intentional range such as Select-All). Consumed by
    /// [`Self::take_suppress_select_collapse`].
    suppress_select_collapse: bool,
    /// M2-1 overlay / dirty / delete paint state (not written into ResultWindow).
    pub edit_paint: EditPaintState,
    /// Double-click requested edit for this cell (consumed by the workspace).
    pub pending_edit_cell: Option<(usize, usize)>,
    /// User-resized column widths keyed by result column name (session-local).
    column_widths: BTreeMap<String, f32>,
}

impl GridDelegate {
    pub(crate) fn visual_rows(&self) -> Vec<VisualRowRef> {
        let anchors: Vec<Option<usize>> = self
            .edit_paint
            .overlays
            .iter()
            .map(|o| o.after_server_row)
            .collect();
        visual_row_map(self.result.total_rows(), &anchors)
    }

    /// Copy cells for a visual (screen) row index — resolves overlays via
    /// [`Self::visual_rows`] so mid-page duplicates do not shift server rows.
    ///
    /// Overlay cells use typed [`Value`]s via [`value_to_copy_cell`]. An unset
    /// insert column (absent from [`OverlayPaintRow::raw`]) is
    /// [`CopyCell::Unset`] so formats omit it instead of inventing NULL/`''`
    /// or refusing as an unloaded page row (#99).
    pub(crate) fn copy_row_cells(&self, visual_row: usize) -> Option<Vec<CopyCell>> {
        let col_count = self.result.columns().len().max(1);
        match self.visual_rows().get(visual_row).copied()? {
            VisualRowRef::Server { row_ix } => match self.result.row(row_ix) {
                RowState::Ready(cells) => Some(cells.iter().map(value_to_copy_cell).collect()),
                RowState::Loading => Some(vec![CopyCell::NotLoaded; col_count]),
            },
            VisualRowRef::Overlay { overlay_ix } => {
                let overlay = self.edit_paint.overlays.get(overlay_ix)?;
                Some(
                    self.result
                        .columns()
                        .iter()
                        .map(|col| match overlay.raw.get(&col.name) {
                            None => CopyCell::Unset,
                            Some(value) => value_to_copy_cell(value),
                        })
                        .collect(),
                )
            }
        }
    }

    fn render_overlay_td(
        &self,
        visual_row: usize,
        overlay_ix: usize,
        col_ix: usize,
        col_name: &str,
        palette: &SelaPalette,
        cx: &gpui_kit::Context<TableState<Self>>,
    ) -> impl IntoElement {
        let overlay = self.edit_paint.overlays.get(overlay_ix);
        let marker = overlay.map(|o| o.marker).unwrap_or(RowMarker::Inserted);
        let value = overlay.and_then(|o| o.raw.get(col_name));
        let required = overlay.is_some_and(|o| o.required_unset.iter().any(|c| c == col_name));
        let generated = overlay.is_some_and(|o| o.generated.iter().any(|c| c == col_name));
        let (nullish, text) = if generated {
            (true, SharedString::from("Filled on commit."))
        } else if required && value.is_none() {
            (true, SharedString::from("Required"))
        } else if let Some(v) = value {
            (v.is_null(), SharedString::from(v.display().into_owned()))
        } else {
            (true, SharedString::from(""))
        };
        let gutter = if col_ix == 0 {
            marker.gutter_glyph()
        } else {
            None
        };
        let gutter_tip = if col_ix == 0 {
            marker.gutter_tooltip().map(str::to_string)
        } else {
            None
        };
        let insert_id = overlay.map(|o| o.insert_id);
        let editing_here = self.edit_paint.editing.as_ref().is_some_and(|(row, c)| {
            *c == col_ix
                && insert_id.is_some_and(
                    |id| matches!(row, EditRowId::Overlay { insert_id: rid } if *rid == id),
                )
        });
        let table = cx.entity();
        let table_shift = table.clone();
        div()
            .id(format!("grid-cell:{visual_row}:{col_ix}"))
            .h_full()
            .w_full()
            .px_2()
            .flex()
            .items_center()
            .gap_1()
            .truncate()
            .bg(palette.row_insert_bg)
            .text_color(if nullish {
                cx.theme().muted_foreground
            } else {
                cx.theme().foreground
            })
            .font_family(cx.theme().mono_font_family.clone())
            .when(nullish && !editing_here, |cell| cell.italic())
            .when_some(gutter, |cell, glyph| {
                cell.child({
                    let tip = gutter_tip.clone();
                    let mut g = div()
                        .id(("overlay-gutter", visual_row))
                        .w(px(16.))
                        .flex_shrink_0()
                        .text_xs()
                        .child(glyph);
                    if let Some(tip) = tip {
                        g = g
                            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx));
                    }
                    g
                })
            })
            .child({
                if editing_here {
                    if let Some(input) = &self.edit_paint.editor_input {
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .items_center()
                            .gap_1()
                            .key_context(CELL_EDITOR_KEY_CONTEXT)
                            .child(Input::new(input).id("cell-editor-input").h(px(22.)))
                            .child(
                                Button::new(("ghost-null-ov", visual_row))
                                    .xsmall()
                                    .ghost()
                                    .label("NULL")
                                    .tooltip("Store NULL")
                                    .on_click(|_, window, cx| {
                                        window.dispatch_action(Box::new(SetCellNull), cx);
                                    }),
                            )
                            .into_any_element()
                    } else {
                        div().child(text).into_any_element()
                    }
                } else {
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .truncate()
                        .child(text)
                        .into_any_element()
                }
            })
            .on_mouse_down(MouseButton::Left, move |event, _, cx| {
                let shift = event.modifiers.shift;
                table_shift.update(cx, |state, _| {
                    state.delegate_mut().note_pointer_shift(shift);
                });
            })
            .on_click({
                let table_dbl = table.clone();
                move |event, _, cx| {
                    if event.click_count() >= 2 {
                        table_dbl.update(cx, |state, cx| {
                            state.set_selected_cell(visual_row, col_ix, cx);
                            state
                                .delegate_mut()
                                .note_cell_focus(visual_row, col_ix, false);
                            state.delegate_mut().pending_edit_cell = Some((visual_row, col_ix));
                            state.refresh(cx);
                        });
                    }
                }
            })
            .context_menu(move |menu, _, menu_cx| {
                table.update(menu_cx, |state, cx| {
                    let selection = state.selection();
                    let range = state.delegate().selection_rect;
                    if !cell_in_selection(visual_row, col_ix, selection, range) {
                        state.set_selected_cell(visual_row, col_ix, cx);
                        state
                            .delegate_mut()
                            .note_cell_focus(visual_row, col_ix, false);
                    }
                });
                copy_context_menu(menu)
            })
    }

    pub fn empty() -> Self {
        Self {
            result: ResultWindow::empty(),
            pending_pages: Vec::new(),
            pending_sort: None,
            sortable: false,
            sorts: Vec::new(),
            loading: false,
            empty_message: "Open a table, or run a query.".into(),
            selection_anchor: None,
            selection_rect: None,
            pointer_shift: None,
            suppress_select_collapse: false,
            edit_paint: EditPaintState::default(),
            pending_edit_cell: None,
            column_widths: BTreeMap::new(),
        }
    }

    fn default_column_width(header: &str) -> f32 {
        (header.chars().count() as f32 * 7.5).clamp(140.0, 360.0)
    }

    fn prune_column_widths(&mut self) {
        let names: std::collections::HashSet<&str> = self
            .result
            .columns()
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        self.column_widths
            .retain(|name, _| names.contains(name.as_str()));
    }

    /// Persist widths from [`TableEvent::ColumnWidthsChanged`]; indices align
    /// with [`TableDelegate::column`] (data columns only — not the row header).
    pub fn apply_column_widths(&mut self, widths: &[gpui_kit::Pixels]) {
        let columns = self.result.columns();
        for (col_ix, width) in widths.iter().enumerate() {
            if let Some(meta) = columns.get(col_ix) {
                self.column_widths
                    .insert(meta.name.clone(), f32::from(*width));
            }
        }
    }

    /// Headless UI tests: read session column-width cache.
    #[cfg(test)]
    pub(crate) fn test_cached_column_width(&self, name: &str) -> Option<f32> {
        self.column_widths.get(name).copied()
    }

    pub fn set_loading_message(&mut self) {
        self.empty_message = "Running…".into();
    }

    pub fn show_page(&mut self, page: QueryPage) {
        self.result.replace_with_page(page);
        self.pending_pages.clear();
        self.loading = false;
        self.clear_selection_range();
        self.prune_column_widths();
        if self.result.total_rows() == 0 {
            self.empty_message = "The query returned no rows.".into();
        }
    }

    /// Install a finished page and hide the loading skeleton.
    ///
    /// Table Previous/Next and SQL results both materialize the current page at
    /// row 0 via [`Self::show_page`]. `page_offset` is only the SQL `OFFSET`
    /// used when fetching; it must not become a grid page index, or page 2+
    /// lands past the viewport and the grid shows placeholders (then page-1
    /// rows after a scroll-triggered refetch).
    pub fn install_loaded_page(
        &mut self,
        page: QueryPage,
        _paged: bool,
        _total_rows: Option<usize>,
        _page_offset: u64,
    ) {
        self.show_page(page);
    }

    pub fn show_message(&mut self, message: impl Into<SharedString>) {
        self.result = ResultWindow::empty();
        self.pending_pages.clear();
        self.loading = false;
        self.clear_selection_range();
        self.column_widths.clear();
        self.empty_message = message.into();
    }

    /// Clear a stale "Running…" empty state after the engine reports failure.
    ///
    /// When this request delivered no rows, replace the grid with the failed
    /// empty copy. When rows already arrived (stream then error), keep them and
    /// only clear [`Self::loading`].
    pub fn note_query_failed(&mut self) {
        if self.result.total_rows() == 0 {
            self.show_message("Query failed — see the error above.");
        } else {
            self.loading = false;
        }
    }

    /// Clear a stale "Running…" empty state after the user cancels (Stop).
    ///
    /// Same shape as [`Self::note_query_failed`]: with no rows, show the
    /// cancelled empty copy; with streamed rows, keep them and clear loading.
    pub fn note_query_cancelled(&mut self) {
        if self.result.total_rows() == 0 {
            self.show_message("Query cancelled.");
        } else {
            self.loading = false;
        }
    }

    pub fn clear_selection_range(&mut self) {
        self.selection_anchor = None;
        self.selection_rect = None;
        self.pointer_shift = None;
        self.suppress_select_collapse = false;
    }

    /// Remember whether Shift was held on the pointer press that is about to
    /// select a cell (see [`Self::take_shift_for_select`]).
    pub fn note_pointer_shift(&mut self, shift: bool) {
        self.pointer_shift = Some(shift);
    }

    /// Consume a pointer-captured Shift flag, if any.
    pub fn take_shift_for_select(&mut self) -> Option<bool> {
        self.pointer_shift.take()
    }

    /// Ignore the next non-Shift [`TableEvent::SelectCell`] collapse.
    ///
    /// `TableState::set_selected_cell` emits `SelectCell` after the current
    /// update returns; without this flag, that deferred handler would call
    /// [`Self::note_cell_focus`] with `shift = false` and wipe a range that was
    /// just set (Shift+arrow, right-click keep-range).
    pub fn suppress_next_select_collapse(&mut self) {
        self.suppress_select_collapse = true;
    }

    /// Consume the range-keep suppress flag.
    pub fn take_suppress_select_collapse(&mut self) -> bool {
        let flag = self.suppress_select_collapse;
        self.suppress_select_collapse = false;
        flag
    }

    /// Apply a [`TableEvent::SelectCell`] the same way the workspace subscriber
    /// does: honor suppress, then pointer-captured Shift, then `window_shift`.
    ///
    /// Unit tests use this to drive the real post-`set_selected_cell` order
    /// without a live GPUI window.
    pub fn apply_select_cell_event(&mut self, row: usize, col: usize, window_shift: bool) {
        if self.take_suppress_select_collapse() {
            return;
        }
        let shift = self.take_shift_for_select().unwrap_or(window_shift);
        self.note_cell_focus(row, col, shift);
    }

    /// Record a cell focus for Shift-range selection.
    pub fn note_cell_focus(&mut self, row: usize, col: usize, shift: bool) {
        let focus = CellPos::new(row, col);
        if shift {
            let (anchor, rect) = SelectionRect::extend_with_focus(self.selection_anchor, focus);
            self.selection_anchor = Some(anchor);
            self.selection_rect = Some(rect);
        } else {
            self.selection_anchor = Some(focus);
            self.selection_rect = Some(SelectionRect::cell(row, col));
        }
    }

    /// Select every loaded cell (Ctrl/Cmd+A).
    pub fn select_all_cells(&mut self) {
        let rows = self.visual_rows().len();
        let cols = self.result.columns().len();
        if rows == 0 || cols == 0 {
            self.clear_selection_range();
            return;
        }
        self.selection_anchor = Some(CellPos::new(0, 0));
        self.selection_rect = Some(SelectionRect::from_corners(
            CellPos::new(0, 0),
            CellPos::new(rows - 1, cols - 1),
        ));
        self.pointer_shift = None;
    }

    /// Move the focus cell by `(d_row, d_col)` and extend `selection_rect`.
    ///
    /// Used by Shift+arrow. Returns the new focus when the move (or a no-op at
    /// the edge that still refreshes the range) should call
    /// `TableState::set_selected_cell`. The caller must set
    /// [`Self::note_pointer_shift`]`(true)` before `set_selected_cell` so the
    /// deferred `SelectCell` subscriber extends instead of collapsing.
    pub fn focus_after_shift_step(
        &self,
        row: usize,
        col: usize,
        d_row: isize,
        d_col: isize,
    ) -> Option<(usize, usize)> {
        let rows = self.visual_rows().len();
        let cols = self.result.columns().len();
        if rows == 0 || cols == 0 {
            return None;
        }
        let new_row = (row as isize + d_row).clamp(0, rows as isize - 1) as usize;
        let new_col = (col as isize + d_col).clamp(0, cols as isize - 1) as usize;
        Some((new_row, new_col))
    }

    /// Whether `(row, col)` lies inside the active Shift-range (if any).
    pub fn cell_in_range(&self, row: usize, col: usize) -> bool {
        self.selection_rect.is_some_and(|rect| {
            let (min_r, max_r, min_c, max_c) = rect.normalized();
            row >= min_r && row <= max_r && col >= min_c && col <= max_c
        })
    }

    /// Resolve the rectangle to copy from DataTable selection + our range.
    pub fn copy_rect(&self, selection: TableSelection) -> Option<SelectionRect> {
        let rows = self.visual_rows().len();
        let cols = self.result.columns().len();
        if rows == 0 || cols == 0 {
            return None;
        }
        if let Some(rect) = self.selection_rect {
            if matches!(selection, TableSelection::Cell(_, _))
                || rect.row_count() * rect.col_count() > 1
            {
                return rect.clamp(rows, cols);
            }
        }
        match selection {
            TableSelection::None => self.selection_rect.and_then(|r| r.clamp(rows, cols)),
            TableSelection::Cell(row, col) => SelectionRect::cell(row, col).clamp(rows, cols),
            TableSelection::Row(row) => {
                SelectionRect::row(row, cols).and_then(|r| r.clamp(rows, cols))
            }
            TableSelection::Column(col) => {
                SelectionRect::column(col, rows).and_then(|r| r.clamp(rows, cols))
            }
        }
    }

    fn sort_mark(&self, name: &str) -> Option<String> {
        self.sorts.iter().enumerate().find_map(|(ix, s)| {
            if s.column == name {
                let arrow = match s.direction {
                    SortDirection::Asc => "▲",
                    SortDirection::Desc => "▼",
                };
                Some(format!("{arrow}{}", ix + 1))
            } else {
                None
            }
        })
    }

    fn column_sort_state(&self, name: &str) -> ColumnSort {
        match self.sorts.iter().find(|s| s.column == name) {
            Some(SortSpec {
                direction: SortDirection::Asc,
                ..
            }) => ColumnSort::Ascending,
            Some(SortSpec {
                direction: SortDirection::Desc,
                ..
            }) => ColumnSort::Descending,
            None => ColumnSort::Default,
        }
    }
}

impl TableDelegate for GridDelegate {
    fn columns_count(&self, _: &App) -> usize {
        self.result.columns().len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.visual_rows().len()
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        let meta = &self.result.columns()[col_ix];
        // Omit an empty type (SQLite undeclared / all-NULL expressions) so the
        // header is just the column name — never show "NULL" as a type.
        let header = match (self.sort_mark(&meta.name), meta.type_name.is_empty()) {
            (Some(mark), true) => format!("{} {mark}", meta.name),
            (Some(mark), false) => format!("{} {mark}    {}", meta.name, meta.type_name),
            (None, true) => meta.name.clone(),
            (None, false) => format!("{}   {}", meta.name, meta.type_name),
        };
        let width = self
            .column_widths
            .get(&meta.name)
            .copied()
            .unwrap_or_else(|| Self::default_column_width(&header));
        let mut col = Column::new(format!("{col_ix}:{}", meta.name), header)
            .width(px(width))
            .resizable(true)
            .movable(false);
        if self.sortable {
            // Enables the header click target; we ignore DataTable's own cycle
            // and rebuild ORDER BY from `sorts` / `pending_sort`.
            col = col.sort(self.column_sort_state(&meta.name));
        }
        col
    }

    fn perform_sort(
        &mut self,
        col_ix: usize,
        _sort: ColumnSort,
        window: &mut Window,
        cx: &mut gpui_kit::Context<TableState<Self>>,
    ) {
        if !self.sortable {
            return;
        }
        let shift = window.modifiers().shift;
        self.pending_sort = Some((col_ix, shift));
        cx.notify();
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut gpui_kit::Context<TableState<Self>>,
    ) -> impl IntoElement {
        let palette = SelaPalette::for_mode(cx.theme().mode);
        let col_name = self
            .result
            .columns()
            .get(col_ix)
            .map(|c| c.name.clone())
            .unwrap_or_default();

        let visual = self.visual_rows();
        let Some(resolved) = visual.get(row_ix).copied() else {
            return div().into_any_element();
        };
        if let VisualRowRef::Overlay { overlay_ix } = resolved {
            return self
                .render_overlay_td(row_ix, overlay_ix, col_ix, &col_name, &palette, cx)
                .into_any_element();
        }
        let VisualRowRef::Server { row_ix: server_row } = resolved else {
            return div().into_any_element();
        };

        let marker = self
            .edit_paint
            .server
            .get(&server_row)
            .map(|r| r.marker)
            .unwrap_or(RowMarker::Clean);
        let dirty_override = self
            .edit_paint
            .server
            .get(&server_row)
            .and_then(|r| r.dirty_display.get(&col_name).cloned());
        let show_triangle = marker == RowMarker::Dirty
            && self
                .edit_paint
                .server
                .get(&server_row)
                .is_some_and(|r| r.dirty_display.contains_key(&col_name));
        let triangle_tip = self
            .edit_paint
            .server
            .get(&server_row)
            .and_then(|r| r.original_display.get(&col_name))
            .map(|orig| format!("Edited · was {orig}"))
            .unwrap_or_else(|| "Edited".into());
        let gutter_tip = if col_ix == 0 {
            marker.gutter_tooltip().map(str::to_string)
        } else {
            None
        };

        // Classify before taking the truncated display cache (needs &mut).
        let kind = match self.result.cell_value(server_row, col_ix) {
            None => CellKind::Loading,
            Some(value) if value.is_null() && dirty_override.is_none() => CellKind::Null,
            Some(value) if value.is_decode_error() || value.is_unsupported_placeholder() => {
                CellKind::Decode(value.display().into_owned())
            }
            Some(_) => CellKind::Normal,
        };
        let is_decode = matches!(kind, CellKind::Decode(_));
        let (null, loading, text) = if let Some(display) = dirty_override {
            let is_null = display == "NULL";
            (is_null, false, SharedString::from(display))
        } else {
            match kind {
                CellKind::Loading => (false, true, SharedString::from("…")),
                CellKind::Null => (true, false, SharedString::from("NULL")),
                CellKind::Decode(decode) => {
                    (false, false, SharedString::from(format!("!{decode}")))
                }
                CellKind::Normal => {
                    let text = match self.result.cell(server_row, col_ix) {
                        CellView::Loading => SharedString::from("…"),
                        CellView::Ready { display, .. } => SharedString::from(display),
                    };
                    (false, false, text)
                }
            }
        };
        let in_range = self.cell_in_range(row_ix, col_ix);
        let deleted = marker == RowMarker::Deleted;
        let (color, decode_bg) = if is_decode {
            (palette.cell_decode_text, Some(palette.cell_decode_bg))
        } else if null || loading {
            (cx.theme().muted_foreground, None)
        } else {
            (cx.theme().foreground, None)
        };
        let gutter = if col_ix == 0 {
            marker.gutter_glyph()
        } else {
            None
        };
        let insert_wash = marker.is_insert_wash();
        // Cell selection installs a Right mouse-down that stop_propagation, so
        // DataTable's row-level TableDelegate::context_menu never opens for
        // cells. Attach the copy menu on the cell content (innermost wins).
        let table = cx.entity();
        let table_shift = table.clone();
        div()
            .id(format!("grid-cell:{row_ix}:{col_ix}"))
            .h_full()
            .w_full()
            .px_2()
            .flex()
            .items_center()
            .gap_1()
            .truncate()
            .text_color(if deleted {
                cx.theme().foreground
            } else {
                color
            })
            .font_family(cx.theme().mono_font_family.clone())
            .when(deleted, |cell| cell.line_through())
            .when_some(decode_bg, |cell, bg| cell.bg(bg))
            .when(insert_wash, |cell| cell.bg(palette.row_insert_bg))
            .when(in_range && !is_decode && !insert_wash, |cell| {
                cell.bg(cx.theme().tokens.table_active)
            })
            .when(null || loading, |cell| cell.italic())
            .when_some(gutter, |cell, glyph| {
                cell.child({
                    let tip = gutter_tip.clone();
                    let mut g = div()
                        .id(("row-gutter", row_ix))
                        .w(px(16.))
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(if deleted {
                            palette.status_error
                        } else {
                            cx.theme().foreground
                        })
                        .child(glyph);
                    if let Some(tip) = tip {
                        g = g
                            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx));
                    }
                    g
                })
            })
            .child({
                let editing_here = self.edit_paint.editing.as_ref().is_some_and(|(row, c)| {
                    *c == col_ix
                        && matches!(
                            row,
                            EditRowId::Server { row_ix: r } if *r == server_row
                        )
                });
                if editing_here {
                    if let Some(input) = &self.edit_paint.editor_input {
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .items_center()
                            .gap_1()
                            .key_context(CELL_EDITOR_KEY_CONTEXT)
                            .child(Input::new(input).id("cell-editor-input").h(px(22.)))
                            .child(
                                Button::new(("ghost-null", row_ix))
                                    .xsmall()
                                    .ghost()
                                    .label("NULL")
                                    .tooltip("Store NULL")
                                    .on_click(|_, window, cx| {
                                        window.dispatch_action(Box::new(SetCellNull), cx);
                                    }),
                            )
                            .into_any_element()
                    } else {
                        div().child(text).into_any_element()
                    }
                } else {
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .truncate()
                        .when(show_triangle && !deleted, |el| {
                            let tip = triangle_tip.clone();
                            el.child(
                                div()
                                    .id(format!("dirty-triangle:{row_ix}:{col_ix}"))
                                    .text_xs()
                                    .text_color(palette.cell_dirty_mark)
                                    .tooltip(move |window, cx| {
                                        Tooltip::new(tip.clone()).build(window, cx)
                                    })
                                    .child("▲"),
                            )
                        })
                        .child(text)
                        .into_any_element()
                }
            })
            .on_mouse_down(MouseButton::Left, move |event, _, cx| {
                let shift = event.modifiers.shift;
                table_shift.update(cx, |state, _| {
                    state.delegate_mut().note_pointer_shift(shift);
                });
            })
            .on_click({
                let table_dbl = table.clone();
                move |event, _, cx| {
                    if event.click_count() >= 2 {
                        // Double-click opens the cell editor (07-grid-editing.md).
                        table_dbl.update(cx, |state, cx| {
                            state.set_selected_cell(row_ix, col_ix, cx);
                            state.delegate_mut().note_cell_focus(row_ix, col_ix, false);
                            state.delegate_mut().pending_edit_cell = Some((row_ix, col_ix));
                            state.refresh(cx);
                        });
                    }
                }
            })
            .context_menu(move |menu, _, menu_cx| {
                table.update(menu_cx, |state, cx| {
                    let selection = state.selection();
                    let range = state.delegate().selection_rect;
                    if !cell_in_selection(row_ix, col_ix, selection, range) {
                        state.set_selected_cell(row_ix, col_ix, cx);
                        state.delegate_mut().note_cell_focus(row_ix, col_ix, false);
                    }
                });
                copy_context_menu(menu)
            })
            .into_any_element()
    }

    fn visible_rows_changed(
        &mut self,
        visible_range: std::ops::Range<usize>,
        _: &mut Window,
        cx: &mut gpui_kit::Context<TableState<Self>>,
    ) {
        // Viewport indices are visual; touch only the underlying server rows.
        let visual = self.visual_rows();
        let end = visible_range.end.min(visual.len());
        let start = visible_range.start.min(end);
        let mut server_start = usize::MAX;
        let mut server_end = 0usize;
        for resolved in &visual[start..end] {
            if let VisualRowRef::Server { row_ix } = *resolved {
                server_start = server_start.min(row_ix);
                server_end = server_end.max(row_ix + 1);
            }
        }
        self.pending_pages = if server_start < server_end {
            self.result.touch(server_start..server_end)
        } else {
            Vec::new()
        };
        if !self.pending_pages.is_empty() {
            cx.notify();
        }
    }

    fn loading(&self, _: &App) -> bool {
        self.loading
    }

    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut gpui_kit::Context<TableState<Self>>,
    ) -> impl IntoElement {
        div()
            .p_4()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(self.empty_message.clone())
    }

    fn cell_text(&self, row_ix: usize, col_ix: usize, _: &App) -> String {
        let visual = self.visual_rows();
        let Some(resolved) = visual.get(row_ix).copied() else {
            return String::new();
        };
        match resolved {
            VisualRowRef::Overlay { overlay_ix } => {
                let col_name = self
                    .result
                    .columns()
                    .get(col_ix)
                    .map(|c| c.name.as_str())
                    .unwrap_or("");
                self.edit_paint
                    .overlays
                    .get(overlay_ix)
                    .and_then(|o| o.raw.get(col_name))
                    .map(|v| v.display().into_owned())
                    .unwrap_or_default()
            }
            VisualRowRef::Server { row_ix: server_row } => match self.result.row(server_row) {
                RowState::Ready(cells) => {
                    let col_name = self
                        .result
                        .columns()
                        .get(col_ix)
                        .map(|c| c.name.clone())
                        .unwrap_or_default();
                    if let Some(display) = self
                        .edit_paint
                        .server
                        .get(&server_row)
                        .and_then(|r| r.dirty_display.get(&col_name))
                    {
                        return display.clone();
                    }
                    cells
                        .get(col_ix)
                        .map(|cell| cell.display().into_owned())
                        .unwrap_or_else(|| "NULL".into())
                }
                RowState::Loading => String::new(),
            },
        }
    }

    fn context_menu(
        &mut self,
        _row_ix: usize,
        menu: PopupMenu,
        _window: &mut Window,
        _cx: &mut gpui_kit::Context<TableState<Self>>,
    ) -> PopupMenu {
        // Row-header right-click path (when right_clicked_row is set).
        copy_context_menu(menu)
    }
}

/// Copy formats for the grid context menu (TC-COPY-01 / US-14).
///
/// Export… / Import CSV… are always listed; Import is a no-op with status copy
/// when the active tab is not table Data or safe mode is Read-only (handlers).
fn copy_context_menu(menu: PopupMenu) -> PopupMenu {
    use super::{DeleteEditRow, DuplicateEditRow, ExportData, ImportCsv, InsertEditRow};
    menu.item(PopupMenuItem::new("Copy").action(Box::new(CopySelection)))
        .separator()
        .item(PopupMenuItem::new("Copy as TSV").action(Box::new(CopyAsTsv)))
        .item(PopupMenuItem::new("Copy as CSV").action(Box::new(CopyAsCsv)))
        .item(PopupMenuItem::new("Copy as JSON").action(Box::new(CopyAsJson)))
        .item(PopupMenuItem::new("Copy as INSERT").action(Box::new(CopyAsInsert)))
        .separator()
        .item(PopupMenuItem::new("Export…").action(Box::new(ExportData)))
        .item(PopupMenuItem::new("Import CSV…").action(Box::new(ImportCsv)))
        .separator()
        .item(PopupMenuItem::new("Insert").action(Box::new(InsertEditRow)))
        .item(PopupMenuItem::new("Duplicate").action(Box::new(DuplicateEditRow)))
        .item(PopupMenuItem::new("Delete").action(Box::new(DeleteEditRow)))
}

/// Whether `(row, col)` is already covered by the DataTable selection or our
/// Shift-range. Right-click keeps the selection when this is true so a range
/// Copy still copies the whole range; otherwise the clicked cell is selected.
pub(crate) fn cell_in_selection(
    row: usize,
    col: usize,
    selection: TableSelection,
    selection_rect: Option<SelectionRect>,
) -> bool {
    if let Some(rect) = selection_rect {
        let (min_r, max_r, min_c, max_c) = rect.normalized();
        if row >= min_r && row <= max_r && col >= min_c && col <= max_c {
            return true;
        }
    }
    match selection {
        TableSelection::None => false,
        TableSelection::Cell(r, c) => r == row && c == col,
        TableSelection::Row(r) => r == row,
        TableSelection::Column(c) => c == col,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use sela_core::{format_selection, ColumnMeta, CopyFormat, Dialect, QueryPage, Value};

    use super::*;

    fn page(rows: Vec<Vec<Value>>) -> QueryPage {
        QueryPage {
            columns: vec![ColumnMeta {
                name: "title".into(),
                type_name: "TEXT".into(),
            }],
            rows,
            duration: Duration::from_millis(0),
        }
    }

    #[test]
    fn paged_table_page_clears_skeleton_and_shows_cells() {
        let mut grid = GridDelegate::empty();
        grid.loading = true;
        grid.install_loaded_page(
            page(vec![
                vec![Value::Text("Rust".into())],
                vec![Value::Text("SQLite".into())],
                vec![Value::Text("GPUI".into())],
            ]),
            true,
            None,
            0,
        );
        assert!(!grid.loading);
        assert_eq!(grid.result.total_rows(), 3);
        match grid.result.row(0) {
            RowState::Ready(cells) => assert_eq!(cells[0].display().as_ref(), "Rust"),
            RowState::Loading => panic!("first row stayed on the loading placeholder"),
        }
    }

    #[test]
    fn paged_table_page_two_shows_at_row_zero_not_absolute_offset() {
        // Regression #42: page_offset 100 must not insert into ResultWindow page 1
        // while the DataTable viewport stays at row 0 (placeholders, then page-1
        // data after a scroll refetch).
        let mut grid = GridDelegate::empty();
        grid.loading = true;
        grid.install_loaded_page(
            page(vec![
                vec![Value::Text("row-0101".into())],
                vec![Value::Text("row-0102".into())],
            ]),
            true,
            Some(2500),
            100,
        );
        assert!(!grid.loading);
        assert_eq!(grid.result.total_rows(), 2);
        match grid.result.row(0) {
            RowState::Ready(cells) => assert_eq!(cells[0].display().as_ref(), "row-0101"),
            RowState::Loading => panic!("page-2 rows landed past the viewport as placeholders"),
        }
        match grid.result.row(1) {
            RowState::Ready(cells) => assert_eq!(cells[0].display().as_ref(), "row-0102"),
            RowState::Loading => panic!("second page-2 row missing at index 1"),
        }
    }

    #[test]
    fn note_query_failed_replaces_running_empty_message() {
        let mut grid = GridDelegate::empty();
        grid.set_loading_message();
        grid.loading = true;
        grid.note_query_failed();
        assert!(!grid.loading);
        assert_eq!(
            grid.empty_message.as_ref(),
            "Query failed — see the error above."
        );
        assert_eq!(grid.result.total_rows(), 0);
    }

    #[test]
    fn note_query_failed_keeps_rows_already_delivered() {
        let mut grid = GridDelegate::empty();
        grid.show_page(page(vec![vec![Value::Text("kept".into())]]));
        grid.loading = true;
        grid.note_query_failed();
        assert!(!grid.loading);
        match grid.result.row(0) {
            RowState::Ready(cells) => assert_eq!(cells[0].display().as_ref(), "kept"),
            RowState::Loading => panic!("delivered rows were cleared on failure"),
        }
    }

    #[test]
    fn note_query_cancelled_replaces_running_empty_message() {
        let mut grid = GridDelegate::empty();
        grid.set_loading_message();
        grid.loading = true;
        grid.note_query_cancelled();
        assert!(!grid.loading);
        assert_eq!(grid.empty_message.as_ref(), "Query cancelled.");
        assert_eq!(grid.result.total_rows(), 0);
    }

    #[test]
    fn note_query_cancelled_keeps_rows_already_delivered() {
        let mut grid = GridDelegate::empty();
        grid.show_page(page(vec![vec![Value::Text("kept".into())]]));
        grid.loading = true;
        grid.note_query_cancelled();
        assert!(!grid.loading);
        match grid.result.row(0) {
            RowState::Ready(cells) => assert_eq!(cells[0].display().as_ref(), "kept"),
            RowState::Loading => panic!("delivered rows were cleared on cancel"),
        }
    }

    #[test]
    fn cell_in_selection_empty_is_outside() {
        assert!(!cell_in_selection(0, 0, TableSelection::None, None));
    }

    #[test]
    fn cell_in_selection_matches_single_cell() {
        assert!(cell_in_selection(
            1,
            2,
            TableSelection::Cell(1, 2),
            Some(SelectionRect::cell(1, 2))
        ));
        assert!(!cell_in_selection(
            0,
            0,
            TableSelection::Cell(1, 2),
            Some(SelectionRect::cell(1, 2))
        ));
    }

    #[test]
    fn cell_in_selection_keeps_range_member() {
        let range = SelectionRect::from_corners(CellPos::new(0, 0), CellPos::new(2, 2));
        assert!(cell_in_selection(
            1,
            1,
            TableSelection::Cell(0, 0),
            Some(range)
        ));
        assert!(!cell_in_selection(
            3,
            3,
            TableSelection::Cell(0, 0),
            Some(range)
        ));
    }

    #[test]
    fn cell_in_selection_row_and_column() {
        assert!(cell_in_selection(4, 0, TableSelection::Row(4), None));
        assert!(!cell_in_selection(3, 0, TableSelection::Row(4), None));
        assert!(cell_in_selection(0, 2, TableSelection::Column(2), None));
        assert!(!cell_in_selection(0, 1, TableSelection::Column(2), None));
    }

    #[test]
    fn copy_rect_none_when_nothing_selected() {
        let grid = GridDelegate::empty();
        // Empty result → None; with rows but no selection also None.
        assert!(grid.copy_rect(TableSelection::None).is_none());
        let mut grid = GridDelegate::empty();
        grid.show_page(page(vec![vec![Value::Text("a".into())]]));
        assert!(grid.copy_rect(TableSelection::None).is_none());
    }

    fn multi_page() -> QueryPage {
        QueryPage {
            columns: vec![
                ColumnMeta {
                    name: "id".into(),
                    type_name: "INTEGER".into(),
                },
                ColumnMeta {
                    name: "title".into(),
                    type_name: "TEXT".into(),
                },
            ],
            rows: vec![
                vec![Value::Int(1), Value::Text("a".into())],
                vec![Value::Int(2), Value::Text("b".into())],
                vec![Value::Int(3), Value::Text("c".into())],
            ],
            duration: Duration::from_millis(0),
        }
    }

    #[test]
    fn shift_click_extends_selection_rect_from_anchor() {
        let mut grid = GridDelegate::empty();
        grid.show_page(multi_page());
        grid.note_cell_focus(0, 0, false);
        grid.note_cell_focus(2, 1, true);
        let rect = grid.selection_rect.expect("range");
        assert_eq!(rect.normalized(), (0, 2, 0, 1));
        assert!(grid.cell_in_range(1, 0));
        assert!(!grid.cell_in_range(0, 2));
    }

    #[test]
    fn select_all_cells_covers_full_grid() {
        let mut grid = GridDelegate::empty();
        grid.show_page(multi_page());
        grid.select_all_cells();
        let rect = grid.selection_rect.expect("all");
        assert_eq!(rect.normalized(), (0, 2, 0, 1));
        let copy = grid
            .copy_rect(TableSelection::Cell(0, 0))
            .expect("copy all");
        assert_eq!(copy.normalized(), (0, 2, 0, 1));
    }

    #[test]
    fn copy_after_duplicate_uses_visual_row_not_screen_as_server() {
        // Duplicate under server row 1 → visual [0, 1, dup, 2].
        // Copying screen row 2 must yield server row 1's values (the source),
        // not server row 2 (#79 regression from page-index copy).
        let mut grid = GridDelegate::empty();
        grid.show_page(multi_page());
        let mut raw = BTreeMap::new();
        raw.insert("title".into(), Value::Text("b".into()));
        grid.edit_paint.overlays.push(OverlayPaintRow {
            insert_id: 1,
            marker: RowMarker::Duplicated,
            raw,
            after_server_row: Some(1),
            required_unset: vec!["id".into()],
            generated: Vec::new(),
        });
        assert_eq!(grid.visual_rows().len(), 4);
        let screen_2 = grid.copy_row_cells(2).expect("dup row");
        let server_1 = grid.copy_row_cells(1).expect("source");
        // Title column matches the duplicated source (server row 1 → "b").
        assert_eq!(screen_2[1], server_1[1]);
        assert_eq!(screen_2[1], value_to_copy_cell(&Value::Text("b".into())));
        // Unset PK is Unset (omitted on copy), not NULL / '' / NotLoaded (#99).
        assert!(matches!(screen_2[0], CopyCell::Unset));
        // Screen row 3 is still server row 2 ("c"), not shifted off the page.
        let screen_3 = grid.copy_row_cells(3).expect("last server");
        assert_eq!(screen_3[1], value_to_copy_cell(&Value::Text("c".into())));
        // Select-all / copy_rect clamp to visual count (includes the overlay).
        grid.select_all_cells();
        assert_eq!(grid.selection_rect.expect("all").normalized(), (0, 3, 0, 1));
    }

    fn books_overlay_page() -> QueryPage {
        QueryPage {
            columns: vec![
                ColumnMeta {
                    name: "id".into(),
                    type_name: "INTEGER".into(),
                },
                ColumnMeta {
                    name: "note".into(),
                    type_name: "TEXT".into(),
                },
                ColumnMeta {
                    name: "pages".into(),
                    type_name: "INTEGER".into(),
                },
            ],
            rows: vec![vec![Value::Int(1), Value::Null, Value::Int(310)]],
            duration: Duration::from_millis(0),
        }
    }

    #[test]
    fn copy_overlay_null_and_int_keep_types_in_insert_and_json() {
        // Display-string copy turned NULL→'NULL' and 310→'310' (#91 review).
        let mut grid = GridDelegate::empty();
        grid.show_page(books_overlay_page());
        let mut raw = BTreeMap::new();
        raw.insert("note".into(), Value::Null);
        raw.insert("pages".into(), Value::Int(310));
        grid.edit_paint.overlays.push(OverlayPaintRow {
            insert_id: 1,
            marker: RowMarker::Duplicated,
            raw,
            after_server_row: Some(0),
            required_unset: vec!["id".into()],
            generated: Vec::new(),
        });
        // Visual: [server0, dup] — copy only the duplicate (screen row 1).
        let cells = grid.copy_row_cells(1).expect("dup");
        assert_eq!(cells[1], CopyCell::Null);
        assert_eq!(cells[2], value_to_copy_cell(&Value::Int(310)));
        // Whole overlay row: Unset id is omitted; NULL/Int stay typed (#99).
        let headers: Vec<String> = grid
            .result
            .columns()
            .iter()
            .map(|c| c.name.clone())
            .collect();
        let body = [cells];
        let insert = format_selection(
            &headers,
            &body,
            CopyFormat::Insert,
            Some("public.books"),
            Dialect::Postgres,
        )
        .expect("insert");
        assert!(
            insert.contains("NULL") && !insert.contains("'NULL'"),
            "NULL must be unquoted: {insert}"
        );
        assert!(
            insert.contains("310") && !insert.contains("'310'"),
            "Int must be unquoted: {insert}"
        );
        assert!(
            !insert.contains("\"id\""),
            "unset PK omitted from INSERT: {insert}"
        );
        let json = format_selection(&headers, &body, CopyFormat::Json, None, Dialect::Postgres)
            .expect("json");
        assert!(json.contains("null"), "JSON null: {json}");
        assert!(json.contains("310"), "JSON number: {json}");
        assert!(
            !json.contains("\"NULL\""),
            "must not stringify NULL: {json}"
        );
        assert!(!json.contains("\"id\""), "omit unset PK key: {json}");
    }

    #[test]
    fn copy_overlay_empty_string_distinct_from_null() {
        let mut grid = GridDelegate::empty();
        grid.show_page(books_overlay_page());
        let mut raw = BTreeMap::new();
        raw.insert("note".into(), Value::text(""));
        raw.insert("pages".into(), Value::Int(1));
        grid.edit_paint.overlays.push(OverlayPaintRow {
            insert_id: 1,
            marker: RowMarker::Duplicated,
            raw,
            after_server_row: Some(0),
            required_unset: Vec::new(),
            generated: Vec::new(),
        });
        let cells = grid.copy_row_cells(1).expect("dup");
        assert_ne!(cells[1], CopyCell::Null);
        assert_eq!(cells[1], value_to_copy_cell(&Value::text("")));
        let body = vec![vec![cells[1].clone()]];
        let cols = vec!["note".to_string()];
        let insert = format_selection(
            &cols,
            &body,
            CopyFormat::Insert,
            Some("public.books"),
            Dialect::Postgres,
        )
        .expect("insert");
        assert!(
            insert.contains("''"),
            "empty string must be quoted SQL '': {insert}"
        );
        assert!(
            !insert.contains("NULL"),
            "'' must not become NULL: {insert}"
        );
    }

    #[test]
    fn copy_overlay_unset_cells_omit_from_insert() {
        let mut grid = GridDelegate::empty();
        grid.show_page(books_overlay_page());
        // Blank insert: every column unset.
        grid.edit_paint.overlays.push(OverlayPaintRow {
            insert_id: 1,
            marker: RowMarker::Inserted,
            raw: BTreeMap::new(),
            after_server_row: None,
            required_unset: vec!["id".into()],
            generated: Vec::new(),
        });
        let cells = grid.copy_row_cells(1).expect("insert overlay");
        assert!(cells.iter().all(|c| matches!(c, CopyCell::Unset)));
        let headers: Vec<String> = grid
            .result
            .columns()
            .iter()
            .map(|c| c.name.clone())
            .collect();
        let insert = format_selection(
            &headers,
            &[cells],
            CopyFormat::Insert,
            Some("public.books"),
            Dialect::Postgres,
        )
        .expect("all-unset overlay still copies");
        assert!(
            insert.contains("DEFAULT VALUES"),
            "all unset → DEFAULT VALUES: {insert}"
        );
    }

    #[test]
    fn right_click_inside_range_keeps_selection_rect() {
        let mut grid = GridDelegate::empty();
        grid.show_page(multi_page());
        grid.note_cell_focus(0, 0, false);
        grid.note_cell_focus(2, 1, true);
        let before = grid.selection_rect;
        // Simulate the context-menu path: only select when outside.
        assert!(cell_in_selection(
            1,
            0,
            TableSelection::Cell(2, 1),
            grid.selection_rect
        ));
        // Inside → do not call note_cell_focus(false).
        assert_eq!(grid.selection_rect, before);
        // Outside → collapse to the clicked cell.
        assert!(!cell_in_selection(
            0,
            0,
            TableSelection::Cell(2, 1),
            // Use a range that does not include (0,0) after moving focus only —
            // with the full range (0,0) is inside; pick a cell past the grid.
            Some(SelectionRect::from_corners(
                CellPos::new(1, 0),
                CellPos::new(2, 1)
            ))
        ));
        grid.note_cell_focus(0, 0, false);
        assert_eq!(grid.selection_rect, Some(SelectionRect::cell(0, 0)));
    }

    #[test]
    fn pointer_shift_preferred_over_window_modifiers() {
        let mut grid = GridDelegate::empty();
        grid.note_pointer_shift(true);
        assert_eq!(grid.take_shift_for_select(), Some(true));
        assert_eq!(grid.take_shift_for_select(), None);
    }

    /// RC6 Ctrl+A failure: `set_selected_cell(0, 0)` emits `SelectCell` after
    /// `select_all_cells` returns; without suppress, that collapses the range.
    #[test]
    fn select_all_then_deferred_select_cell_without_suppress_collapses() {
        let mut grid = GridDelegate::empty();
        grid.show_page(multi_page());
        grid.note_cell_focus(1, 0, false);
        grid.select_all_cells();
        assert_eq!(grid.selection_rect.expect("all").normalized(), (0, 2, 0, 1));
        // Deferred SelectCell(0, 0) with shift=false — the RC6 bug path.
        grid.apply_select_cell_event(0, 0, false);
        assert_eq!(
            grid.selection_rect.expect("collapsed"),
            SelectionRect::cell(0, 0)
        );
    }

    #[test]
    fn select_all_then_plain_click_collapses_to_clicked_cell() {
        let mut grid = GridDelegate::empty();
        grid.show_page(multi_page());
        grid.note_cell_focus(1, 0, false);
        // Ctrl/Cmd+A path: select_all_cells only, no suppress flag armed.
        grid.select_all_cells();
        assert_eq!(grid.selection_rect.expect("all").normalized(), (0, 2, 0, 1));
        // The user's next plain click must collapse to the clicked cell.
        grid.apply_select_cell_event(2, 1, false);
        assert_eq!(
            grid.selection_rect.expect("single"),
            SelectionRect::cell(2, 1)
        );
    }

    #[test]
    fn suppress_is_one_shot_after_deferred_select_cell() {
        let mut grid = GridDelegate::empty();
        grid.show_page(multi_page());
        grid.note_cell_focus(0, 0, false);
        grid.note_cell_focus(2, 1, true);
        // Armed right before a set_selected_cell that emits SelectCell.
        grid.suppress_next_select_collapse();
        grid.apply_select_cell_event(2, 1, false);
        assert_eq!(
            grid.selection_rect.expect("kept").normalized(),
            (0, 2, 0, 1)
        );
        // A later plain SelectCell still collapses.
        grid.apply_select_cell_event(1, 0, false);
        assert_eq!(
            grid.selection_rect.expect("single"),
            SelectionRect::cell(1, 0)
        );
    }

    /// RC6 Shift+Down failure: DataTable never sees `shift-down`, so we drive
    /// the move ourselves with a pending Shift flag (or suppress + sync extend).
    #[test]
    fn shift_arrow_keyboard_path_extends_via_pending_shift() {
        let mut grid = GridDelegate::empty();
        grid.show_page(multi_page());
        grid.note_cell_focus(1, 1, false);
        // First Shift+Down: pending shift + SelectCell to row 2.
        let (r, c) = grid.focus_after_shift_step(1, 1, 1, 0).expect("step");
        assert_eq!((r, c), (2, 1));
        grid.note_pointer_shift(true);
        // window_shift=false proves the pending flag is what extends the range.
        grid.apply_select_cell_event(r, c, false);
        assert_eq!(
            grid.selection_rect.expect("range").normalized(),
            (1, 2, 1, 1)
        );
        // Second Shift+Down: grow to rows 1..=2 still (already at last row).
        let (r2, c2) = grid.focus_after_shift_step(2, 1, 1, 0).expect("edge");
        assert_eq!((r2, c2), (2, 1));
        grid.note_pointer_shift(true);
        grid.apply_select_cell_event(r2, c2, false);
        assert_eq!(
            grid.selection_rect.expect("edge").normalized(),
            (1, 2, 1, 1)
        );
    }

    #[test]
    fn shift_arrow_twice_from_row0_grows_two_rows() {
        let mut grid = GridDelegate::empty();
        grid.show_page(multi_page());
        grid.note_cell_focus(0, 0, false);
        for step in 1..=2 {
            let (r, c) = grid
                .focus_after_shift_step(step - 1, 0, 1, 0)
                .expect("step");
            assert_eq!((r, c), (step, 0));
            grid.note_pointer_shift(true);
            grid.suppress_next_select_collapse();
            // Production path: sync extend, then deferred SelectCell is suppressed.
            grid.note_cell_focus(r, c, true);
            let _ = grid.take_shift_for_select();
            grid.apply_select_cell_event(r, c, false);
        }
        assert_eq!(
            grid.selection_rect.expect("grown").normalized(),
            (0, 2, 0, 0)
        );
    }

    #[test]
    fn shift_click_still_extends_after_keyboard_helpers() {
        let mut grid = GridDelegate::empty();
        grid.show_page(multi_page());
        grid.note_cell_focus(0, 0, false);
        grid.note_pointer_shift(true);
        grid.apply_select_cell_event(2, 1, false);
        assert_eq!(
            grid.selection_rect.expect("click range").normalized(),
            (0, 2, 0, 1)
        );
    }

    fn id_name_page() -> QueryPage {
        QueryPage {
            columns: vec![
                ColumnMeta {
                    name: "id".into(),
                    type_name: "INTEGER".into(),
                },
                ColumnMeta {
                    name: "name".into(),
                    type_name: "TEXT".into(),
                },
            ],
            rows: vec![vec![Value::Int(1), Value::Text("alpha".into())]],
            duration: Duration::from_millis(0),
        }
    }

    #[test]
    fn apply_column_widths_updates_delegate_column() {
        let mut grid = GridDelegate::empty();
        grid.show_page(id_name_page());
        grid.apply_column_widths(&[px(140.), px(286.)]);
        assert_eq!(grid.column_widths.get("name"), Some(&286.));
    }

    #[test]
    fn column_widths_survive_reload_with_same_columns() {
        let mut grid = GridDelegate::empty();
        grid.show_page(id_name_page());
        grid.apply_column_widths(&[px(140.), px(286.)]);
        let page = QueryPage {
            columns: id_name_page().columns,
            rows: vec![vec![Value::Int(2), Value::Text("beta".into())]],
            duration: Duration::from_millis(0),
        };
        grid.show_page(page);
        assert_eq!(grid.column_widths.get("name"), Some(&286.));
    }

    #[test]
    fn column_widths_drop_when_columns_change() {
        let mut grid = GridDelegate::empty();
        grid.show_page(id_name_page());
        grid.apply_column_widths(&[px(140.), px(286.)]);
        let other = QueryPage {
            columns: vec![ColumnMeta {
                name: "note".into(),
                type_name: "TEXT".into(),
            }],
            rows: vec![vec![Value::Text("only".into())]],
            duration: Duration::from_millis(0),
        };
        grid.show_page(other);
        assert!(!grid.column_widths.contains_key("name"));
        assert!(!grid.column_widths.contains_key("id"));
    }
}
