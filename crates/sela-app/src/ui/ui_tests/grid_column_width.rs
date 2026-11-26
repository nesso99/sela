//! Column resize widths must survive sort, selection, and grid refresh (#137).

use std::time::Duration;

use gpui_kit::component::table::{DataTable, TableEvent, TableState};
use gpui_kit::prelude::*;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    div, px, size, App, AppContext, Context, Entity, Render, Subscription, TestAppContext, Window,
};

use sela_core::{ColumnMeta, QueryPage, Value};

use crate::ui::grid::GridDelegate;

const RESIZED_NAME_COL: f32 = 286.;

fn edit_pk_page() -> QueryPage {
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
        rows: vec![vec![Value::Int(1), Value::Text("row".into())]],
        duration: Duration::from_millis(0),
    }
}

struct ColumnWidthHost {
    grid: Entity<TableState<GridDelegate>>,
    _event_sub: Subscription,
}

impl ColumnWidthHost {
    fn new(window: &mut Window, cx: &mut App) -> Self {
        let grid = cx.new(|cx| {
            let mut state = TableState::new(GridDelegate::empty(), window, cx)
                .sortable(true)
                .col_movable(false)
                .cell_selectable(true)
                .row_selectable(true)
                .col_selectable(true)
                .row_header(true);
            state.delegate_mut().show_page(edit_pk_page());
            state.refresh(cx);
            state
        });
        let grid_entity = grid.clone();
        let event_sub = cx.subscribe(&grid, move |_, event: &TableEvent, cx| {
            if let TableEvent::ColumnWidthsChanged(widths) = event {
                grid_entity.update(cx, |state, _| {
                    state.delegate_mut().apply_column_widths(widths);
                });
            }
        });
        Self {
            grid,
            _event_sub: event_sub,
        }
    }

    fn record_resized_widths(&self, cx: &mut App) {
        self.grid.update(cx, |state, _| {
            let w0 = state.col_width(0).expect("id column");
            state
                .delegate_mut()
                .apply_column_widths(&[w0, px(RESIZED_NAME_COL)]);
        });
    }
}

impl Render for ColumnWidthHost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().child(DataTable::new(&self.grid).stripe(true).bordered(true))
    }
}

#[gpui_kit::test]
fn column_width_survives_refresh_after_resize(cx: &mut TestAppContext) {
    let (handle, host) = super::harness::open_window(cx, size(px(900.), px(500.)), |window, cx| {
        cx.new(|cx| ColumnWidthHost::new(window, cx))
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        host.update(cx, |host, cx| {
            host.record_resized_widths(cx);
            host.grid.update(cx, |state, cx| {
                state.refresh(cx);
                assert!(
                    (f32::from(state.col_width(1).expect("name width")) - RESIZED_NAME_COL).abs()
                        < 0.5,
                    "refresh after resize must keep user width"
                );

                // Sort path: pending sort + reload page (apply_outcome / sort).
                state.delegate_mut().sortable = true;
                state.delegate_mut().sorts = vec![sela_core::SortSpec {
                    column: "name".into(),
                    direction: sela_core::SortDirection::Asc,
                }];
                state.delegate_mut().show_page(QueryPage {
                    columns: edit_pk_page().columns,
                    rows: vec![vec![Value::Int(2), Value::Text("sorted".into())]],
                    duration: Duration::from_millis(0),
                });
                state.refresh(cx);
                assert!(
                    (f32::from(state.col_width(1).expect("name after sort")) - RESIZED_NAME_COL)
                        .abs()
                        < 0.5,
                    "reload after sort must keep user width"
                );

                // Selection path: cell focus + refresh (tabs.rs SelectCell).
                state.delegate_mut().apply_select_cell_event(0, 1, false);
                state.refresh(cx);
                assert!(
                    (f32::from(state.col_width(1).expect("name after select")) - RESIZED_NAME_COL)
                        .abs()
                        < 0.5,
                    "refresh after header/cell selection must keep user width"
                );
            });
        });
    })
    .expect("column width UI test");
}
