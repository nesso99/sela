//! Filter bar UI for table Data tabs (M1-3 / US-04).

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Disableable as _, Sizable as _};
use gpui_kit::prelude::*;
use gpui_kit::{div, px, Context, Entity, IntoElement, Window};

use sela_core::{build_table_select, display_bound_sql, FilterCondition, FilterOp, SortSpec};

use super::Workspace;

/// One editable filter row (column / op / value inputs).
pub(super) struct FilterRowUi {
    pub column: Entity<InputState>,
    pub value: Entity<InputState>,
    pub op: FilterOp,
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
                view.filters.clear();
            }
        }
        cx.notify();
    }

    pub(super) fn cycle_filter_op(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(tab_id) = self.tabs.active().map(|tab| tab.id) else {
            return;
        };
        if let Some(view) = self.view_mut(tab_id) {
            if let Some(row) = view.filter_rows.get_mut(index) {
                row.op = row.op.next();
            }
        }
        cx.notify();
    }

    pub(super) fn apply_filters(&mut self, cx: &mut Context<Self>) {
        let Some(tab_id) = self.tabs.active().map(|tab| tab.id) else {
            return;
        };
        let conditions = {
            let Some(view) = self.views.get(&tab_id) else {
                return;
            };
            let cols = view.grid.read(cx).delegate().result.columns().to_vec();
            view.filter_rows
                .iter()
                .map(|row| {
                    let mut cond = row.to_condition(cx);
                    if let Some(meta) = cols.iter().find(|c| c.name == cond.column) {
                        cond.column_type = Some(meta.type_name.clone());
                    }
                    cond
                })
                .filter(|c| !c.column.trim().is_empty())
                .collect::<Vec<_>>()
        };
        if let Some(view) = self.view_mut(tab_id) {
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
        self.persist_tabs(cx);
        self.load_source_for(tab_id, cx);
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

        let dialect = self
            .session
            .as_ref()
            .map(|s| s.dialect())
            .unwrap_or(sela_core::Dialect::Sqlite);
        let (schema, table) = match view.map(|v| v.source.clone()) {
            Some(super::GridSource::Table { schema, table }) => (schema, table),
            _ => (String::new(), String::new()),
        };
        let filters = view.map(|v| v.filters.clone()).unwrap_or_default();
        let sorts = view.map(|v| v.sorts.clone()).unwrap_or_default();
        let sql_preview = build_table_select(dialect, &schema, &table, &filters, &sorts)
            .map(|b| display_bound_sql(&b))
            .unwrap_or_else(|err| err.to_string());

        let rows = view.map(|v| v.filter_rows.len()).unwrap_or(0);

        v_flex()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().muted)
            .px_3()
            .py_2()
            .gap_2()
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
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .w(px(160.))
                            .child(Input::new(&row.column).appearance(true)),
                    )
                    .child({
                        Button::new(("filter-op", index))
                            .xsmall()
                            .label(op_label)
                            .disabled(!connected || busy)
                            .tooltip("Click to cycle operator")
                            .on_click(move |_, _, cx| {
                                this_op.update(cx, |workspace, cx| {
                                    workspace.cycle_filter_op(index, cx)
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
            .child(
                div()
                    .text_xs()
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("SQL: {sql_preview}")),
            )
            .into_any_element()
    }

    pub(super) fn apply_column_sort(
        &mut self,
        tab_id: sela_core::TabId,
        col_ix: usize,
        sort: gpui_kit::component::table::ColumnSort,
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
        if let Some(view) = self.view_mut(tab_id) {
            view.sorts = match sort {
                gpui_kit::component::table::ColumnSort::Default => Vec::new(),
                gpui_kit::component::table::ColumnSort::Ascending => {
                    vec![SortSpec::asc(column_name)]
                }
                gpui_kit::component::table::ColumnSort::Descending => {
                    vec![SortSpec::desc(column_name)]
                }
            };
            view.page_offset = 0;
        }
        self.persist_tabs(cx);
        self.load_source_for(tab_id, cx);
    }
}
