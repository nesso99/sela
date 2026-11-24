//! Filter bar UI for table Data tabs (M1-3 / US-04).

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Enter, Input, InputState};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Disableable as _, Sizable as _};
use gpui_kit::prelude::*;
use gpui_kit::{
    div, px, ClipboardItem, Context, Entity, Focusable, IntoElement, SharedString, Window,
};

use sela_core::{
    format_predicate_preview, validate_filter_condition, ColumnKind, FilterCondition, FilterOp,
    SortDirection, SortSpec, MAX_SORT_KEYS,
};

use super::Workspace;

/// One editable filter row (column / op / value inputs).
pub(super) struct FilterRowUi {
    pub column: Entity<InputState>,
    pub value: Entity<InputState>,
    pub op: FilterOp,
    /// Validation message from the last Apply attempt (cleared on edit / success).
    pub error: Option<String>,
}

impl FilterRowUi {
    pub fn new(window: &mut Window, cx: &mut Context<Workspace>, seed: &FilterCondition) -> Self {
        let column = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(&seed.column)
                .placeholder("column")
        });
        let value = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(&seed.value)
                .placeholder("value")
        });
        Self {
            column,
            value,
            op: seed.op,
            error: None,
        }
    }

    pub fn to_condition(&self, cx: &gpui_kit::App) -> FilterCondition {
        FilterCondition {
            column: self.column.read(cx).value().to_string(),
            op: self.op,
            value: self.value.read(cx).value().to_string(),
            column_type: None,
        }
    }
}

fn empty_text_placeholder() -> SharedString {
    SharedString::from("empty matches '' · IS NULL for nulls")
}

fn like_placeholder() -> SharedString {
    SharedString::from("% and _ are wildcards")
}

impl Workspace {
    pub(super) fn toggle_filter_bar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab_id) = self.tabs.active().map(|tab| tab.id) else {
            return;
        };
        let is_table = self
            .views
            .get(&tab_id)
            .is_some_and(|view| matches!(view.source, super::GridSource::Table { .. }));
        if !is_table {
            return;
        }
        let open = self
            .views
            .get(&tab_id)
            .map(|view| view.filter_bar_open)
            .unwrap_or(false);
        if let Some(view) = self.view_mut(tab_id) {
            view.filter_bar_open = !open;
            if view.filter_bar_open && view.filter_rows.is_empty() {
                let seed = FilterCondition::new("", FilterOp::Eq, "");
                view.filter_rows.push(FilterRowUi::new(window, cx, &seed));
            }
        }
        self.refresh_filter_placeholders(window, cx);
        self.persist_tabs(cx);
        cx.notify();
    }

    pub(super) fn add_filter_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab_id) = self.tabs.active().map(|tab| tab.id) else {
            return;
        };
        if let Some(view) = self.view_mut(tab_id) {
            view.filter_bar_open = true;
            let seed = FilterCondition::new("", FilterOp::Eq, "");
            view.filter_rows.push(FilterRowUi::new(window, cx, &seed));
        }
        self.refresh_filter_placeholders(window, cx);
        cx.notify();
    }

    pub(super) fn remove_filter_row(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(tab_id) = self.tabs.active().map(|tab| tab.id) else {
            return;
        };
        if let Some(view) = self.view_mut(tab_id) {
            if index < view.filter_rows.len() {
                view.filter_rows.remove(index);
            }
            if view.filter_rows.is_empty() {
                // Leave one empty draft row; do not reload until Apply/Clear.
            }
        }
        cx.notify();
    }

    pub(super) fn cycle_filter_op(
        &mut self,
        window: &mut Window,
        index: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(tab_id) = self.tabs.active().map(|tab| tab.id) else {
            return;
        };
        let kind = {
            let Some(view) = self.views.get(&tab_id) else {
                return;
            };
            let Some(row) = view.filter_rows.get(index) else {
                return;
            };
            let col_name = row.column.read(cx).value().to_string();
            view.grid
                .read(cx)
                .delegate()
                .result
                .columns()
                .iter()
                .find(|c| c.name == col_name)
                .map(|c| ColumnKind::from_type_name(&c.type_name))
                .unwrap_or(ColumnKind::Other)
        };
        if let Some(view) = self.view_mut(tab_id) {
            if let Some(row) = view.filter_rows.get_mut(index) {
                row.op = row.op.next_for_kind(kind);
                row.error = None;
            }
        }
        self.refresh_filter_placeholders(window, cx);
        cx.notify();
    }

    /// Update value-field placeholders for `=` / `≠` on text and for LIKE.
    pub(super) fn refresh_filter_placeholders(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab_id) = self.tabs.active().map(|tab| tab.id) else {
            return;
        };
        let Some(view) = self.views.get(&tab_id) else {
            return;
        };
        let cols = view.grid.read(cx).delegate().result.columns().to_vec();
        let updates: Vec<_> = view
            .filter_rows
            .iter()
            .map(|row| {
                let col_name = row.column.read(cx).value().to_string();
                let kind = cols
                    .iter()
                    .find(|c| c.name == col_name)
                    .map(|c| ColumnKind::from_type_name(&c.type_name))
                    .unwrap_or(ColumnKind::Other);
                let placeholder = if matches!(row.op, FilterOp::Eq | FilterOp::Ne)
                    && matches!(kind, ColumnKind::Text | ColumnKind::Other)
                {
                    empty_text_placeholder()
                } else if matches!(row.op, FilterOp::Like | FilterOp::NotLike) {
                    like_placeholder()
                } else {
                    SharedString::from("value")
                };
                (row.value.clone(), placeholder)
            })
            .collect();
        for (entity, placeholder) in updates {
            entity.update(cx, |state, cx| {
                state.set_placeholder(placeholder, window, cx);
            });
        }
    }

    pub(super) fn apply_filters(&mut self, cx: &mut Context<Self>) {
        let Some(tab_id) = self.tabs.active().map(|tab| tab.id) else {
            return;
        };
        let (conditions, errors) = {
            let Some(view) = self.views.get(&tab_id) else {
                return;
            };
            let cols = view.grid.read(cx).delegate().result.columns().to_vec();
            let mut conditions = Vec::new();
            let mut errors = Vec::new();
            for row in &view.filter_rows {
                let mut cond = row.to_condition(cx);
                if cond.column.trim().is_empty() && cond.value.is_empty() {
                    // Skip blank draft rows.
                    errors.push(None);
                    continue;
                }
                if let Some(meta) = cols.iter().find(|c| c.name == cond.column) {
                    cond.column_type = Some(meta.type_name.clone());
                } else if !cond.column.trim().is_empty() {
                    // Unknown column name — still allow Other operators after validation.
                    cond.column_type = Some(String::new());
                }
                match validate_filter_condition(&cond) {
                    Ok(()) => {
                        errors.push(None);
                        conditions.push(cond);
                    }
                    Err(msg) => {
                        errors.push(Some(msg));
                    }
                }
            }
            (conditions, errors)
        };
        let any_invalid = errors.iter().any(|e| e.is_some());
        if let Some(view) = self.view_mut(tab_id) {
            for (row, err) in view.filter_rows.iter_mut().zip(errors) {
                row.error = err;
            }
            if any_invalid {
                // Keep previous page; do not send a partial filter.
                cx.notify();
                return;
            }
            view.filters = conditions;
            view.page_offset = 0;
            view.filter_bar_open = true;
        }
        self.persist_tabs(cx);
        self.load_source_for(tab_id, cx);
    }

    pub(super) fn clear_filters(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab_id) = self.tabs.active().map(|tab| tab.id) else {
            return;
        };
        if let Some(view) = self.view_mut(tab_id) {
            view.filters.clear();
            view.filter_rows.clear();
            let seed = FilterCondition::new("", FilterOp::Eq, "");
            view.filter_rows.push(FilterRowUi::new(window, cx, &seed));
            view.page_offset = 0;
        }
        self.refresh_filter_placeholders(window, cx);
        self.persist_tabs(cx);
        self.load_source_for(tab_id, cx);
    }

    /// True when a filter bar input on the active table tab has focus.
    pub(super) fn filter_field_focused(&self, window: &Window, cx: &gpui_kit::App) -> bool {
        let Some(view) = self.active_view() else {
            return false;
        };
        if !view.filter_bar_open {
            return false;
        }
        view.filter_rows.iter().any(|row| {
            row.column.read(cx).focus_handle(cx).is_focused(window)
                || row.value.read(cx).focus_handle(cx).is_focused(window)
        })
    }

    pub(super) fn filter_bar(&self, cx: &Context<Self>, connected: bool) -> impl IntoElement {
        let this = cx.entity();
        let view = self.active_view();
        let open = view.map(|v| v.filter_bar_open).unwrap_or(false);
        let is_table = view
            .map(|v| matches!(v.source, super::GridSource::Table { .. }))
            .unwrap_or(false);
        let busy = view.map(|v| v.busy).unwrap_or(false);
        if !is_table || !open {
            return div().into_any_element();
        }

        let rows = view.map(|v| v.filter_rows.len()).unwrap_or(0);

        v_flex()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().muted)
            .px_3()
            .py_2()
            .gap_2()
            .on_action({
                let this = this.clone();
                move |_: &Enter, _, cx| {
                    // Plain Enter in a filter field applies (single-line Input
                    // propagates Enter after emitting PressEnter).
                    this.update(cx, |workspace, cx| workspace.apply_filters(cx));
                }
            })
            .child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child("Filter"),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child({
                                let this = this.clone();
                                Button::new("filter-add")
                                    .xsmall()
                                    .label("Add")
                                    .disabled(!connected || busy)
                                    .on_click(move |_, window, cx| {
                                        this.update(cx, |workspace, cx| {
                                            workspace.add_filter_row(window, cx)
                                        });
                                    })
                            })
                            .child({
                                let this = this.clone();
                                Button::new("filter-clear")
                                    .xsmall()
                                    .label("Clear")
                                    .disabled(!connected || busy)
                                    .on_click(move |_, window, cx| {
                                        this.update(cx, |workspace, cx| {
                                            workspace.clear_filters(window, cx)
                                        });
                                    })
                            })
                            .child({
                                let this = this.clone();
                                Button::new("filter-apply")
                                    .xsmall()
                                    .primary()
                                    .label("Apply")
                                    .disabled(!connected || busy)
                                    .tooltip("Apply filters (Enter)")
                                    .on_click(move |_, _, cx| {
                                        this.update(cx, |workspace, cx| {
                                            workspace.apply_filters(cx)
                                        });
                                    })
                            }),
                    ),
            )
            .children((0..rows).map(|index| {
                let this_op = this.clone();
                let this_rm = this.clone();
                let row = self.active_view().and_then(|v| v.filter_rows.get(index));
                let Some(row) = row else {
                    return div().into_any_element();
                };
                let op_label = row.op.label().to_string();
                let needs_value = row.op.needs_value();
                let err = row.error.clone();
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .w(px(160.))
                            .child(Input::new(&row.column).appearance(true)),
                    )
                    .child({
                        let like_tip = matches!(row.op, FilterOp::Like | FilterOp::NotLike);
                        Button::new(("filter-op", index))
                            .xsmall()
                            .label(op_label)
                            .disabled(!connected || busy)
                            .tooltip(if like_tip {
                                "LIKE: % and _ are wildcards; use ! to escape (ESCAPE '!')"
                            } else {
                                "Click to cycle operator for this column type"
                            })
                            .on_click(move |_, window, cx| {
                                this_op.update(cx, |workspace, cx| {
                                    workspace.cycle_filter_op(window, index, cx)
                                });
                            })
                    })
                    .child(
                        div()
                            .flex_1()
                            .when(needs_value, |el| {
                                el.child(Input::new(&row.value).appearance(true))
                            })
                            .when(!needs_value, |el| {
                                el.child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child("(no value)"),
                                )
                            }),
                    )
                    .when_some(err, |row, msg| {
                        row.child(div().text_xs().text_color(cx.theme().danger).child(msg))
                    })
                    .child(
                        Button::new(("filter-rm", index))
                            .xsmall()
                            .label("×")
                            .disabled(!connected || busy)
                            .on_click(move |_, _, cx| {
                                this_rm.update(cx, |workspace, cx| {
                                    workspace.remove_filter_row(index, cx)
                                });
                            }),
                    )
                    .into_any_element()
            }))
            .into_any_element()
    }

    /// Read-only WHERE / ORDER BY strip under the grid (does not touch TabView.sql).
    pub(super) fn predicate_strip(&self, cx: &Context<Self>) -> impl IntoElement {
        let view = self.active_view();
        let Some(view) = view else {
            return div().into_any_element();
        };
        let super::GridSource::Table { schema, table } = &view.source else {
            return div().into_any_element();
        };
        if view.filters.is_empty() && view.sorts.is_empty() {
            return div().into_any_element();
        }
        let dialect = self
            .session
            .as_ref()
            .map(|s| s.dialect())
            .unwrap_or(sela_core::Dialect::Sqlite);
        let preview = format_predicate_preview(dialect, schema, table, &view.filters, &view.sorts);
        if preview.is_empty() {
            return div().into_any_element();
        }
        let truncated = if preview.chars().count() > 120 {
            let t: String = preview.chars().take(117).collect();
            format!("{t}…")
        } else {
            preview.clone()
        };
        let full = preview.clone();
        let full_for_tip = preview;
        div()
            .id("predicate-strip")
            .h(px(20.))
            .px_3()
            .flex()
            .items_center()
            .border_t_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().muted)
            .text_xs()
            .font_family(cx.theme().mono_font_family.clone())
            .text_color(cx.theme().muted_foreground)
            .truncate()
            .cursor_pointer()
            .tooltip(move |window, cx| Tooltip::new(full_for_tip.clone()).build(window, cx))
            .on_click(move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(full.clone()));
            })
            .child(truncated)
            .into_any_element()
    }

    pub(super) fn apply_column_sort(
        &mut self,
        tab_id: sela_core::TabId,
        col_ix: usize,
        shift: bool,
        cx: &mut Context<Self>,
    ) {
        let column_name = self.views.get(&tab_id).and_then(|view| {
            view.grid
                .read(cx)
                .delegate()
                .result
                .columns()
                .get(col_ix)
                .map(|c| c.name.clone())
        });
        let Some(column_name) = column_name else {
            return;
        };
        let mut status_note: Option<String> = None;
        if let Some(view) = self.view_mut(tab_id) {
            if shift {
                // Multi-sort: append / flip / remove this key only (max MAX_SORT_KEYS).
                if let Some(pos) = view.sorts.iter().position(|s| s.column == column_name) {
                    match view.sorts[pos].direction {
                        SortDirection::Asc => {
                            view.sorts[pos].direction = SortDirection::Desc;
                        }
                        SortDirection::Desc => {
                            view.sorts.remove(pos);
                        }
                    }
                } else if view.sorts.len() >= MAX_SORT_KEYS {
                    status_note = Some("At most 3 sort columns".into());
                } else {
                    view.sorts.push(SortSpec::asc(column_name));
                }
            } else {
                // Plain click: cycle when sole key; otherwise replace list with ASC.
                if view.sorts.len() == 1 && view.sorts[0].column == column_name {
                    match view.sorts[0].direction {
                        SortDirection::Asc => {
                            view.sorts[0].direction = SortDirection::Desc;
                        }
                        SortDirection::Desc => {
                            view.sorts.clear();
                        }
                    }
                } else {
                    view.sorts = vec![SortSpec::asc(column_name)];
                }
            }
            view.page_offset = 0;
            if let Some(note) = status_note {
                view.status = note;
                self.persist_tabs(cx);
                cx.notify();
                return;
            }
        }
        self.persist_tabs(cx);
        self.load_source_for(tab_id, cx);
    }
}
