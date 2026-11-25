//! After SQL on a table tab, Save/reconnect and plain tab switch must reload (#98).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui_kit::component::button::Button;
use gpui_kit::component::WindowExt as _;
use gpui_kit::prelude::*;
use gpui_kit::test::{TestAppContextExt, TestWindowExt};
use gpui_kit::{
    div, px, size, App, AppContext, Context, Entity, SharedString, TestAppContext, Window,
};

use sela_core::{ColumnMeta, QueryPage, RelationKind, Value};

use crate::ui::form::{ConnectionForm, FormEvent};
use crate::ui::workspace::tab_view::TabView;
use crate::ui::workspace::tabs::table_tab_needs_load_on_activate;
use crate::ui::workspace::GridSource;

/// Host that mirrors Save → reconnect activate and plain tab switch for #98.
struct ReconnectHost {
    #[allow(dead_code)]
    form: Entity<ConnectionForm>,
    /// Active table tab view (source + grid + page_len).
    view: TabView,
    load_requested: Arc<Mutex<u32>>,
    _subscription: gpui_kit::Subscription,
}

impl ReconnectHost {
    fn seed_sql_result_on_table_tab(window: &mut Window, cx: &mut Context<Self>) -> TabView {
        let mut view = TabView::new(window, cx, "SELECT 1;");
        view.source = GridSource::Sql("SELECT 1;".into());
        view.page_len = 1;
        view.busy = false;
        let page = QueryPage {
            columns: vec![ColumnMeta {
                name: "?column?".into(),
                type_name: "integer".into(),
            }],
            rows: vec![vec![Value::Int(1)]],
            duration: Duration::from_millis(0),
        };
        view.grid.update(cx, |state, cx| {
            state.delegate_mut().show_page(page);
            state.refresh(cx);
        });
        view
    }

    /// Same path as `Workspace::on_tab_activated` for a `TabKind::Table`.
    fn activate_table_tab(&mut self, cx: &mut Context<Self>) {
        let has_session = true;
        let needs_load = table_tab_needs_load_on_activate(
            &self.view.source,
            self.view.page_len,
            self.view.busy,
            has_session,
        );
        self.view.source = GridSource::Table {
            schema: "public".into(),
            table: "edit_pk".into(),
            kind: RelationKind::Table,
        };
        if needs_load {
            *self.load_requested.lock().expect("flag") += 1;
            // Simulate load_source_for clearing the stale SQL grid.
            self.view.page_len = 0;
            self.view.grid.update(cx, |state, cx| {
                state.delegate_mut().show_page(QueryPage {
                    columns: vec![
                        ColumnMeta {
                            name: "id".into(),
                            type_name: "integer".into(),
                        },
                        ColumnMeta {
                            name: "name".into(),
                            type_name: "text".into(),
                        },
                    ],
                    rows: vec![],
                    duration: Duration::from_millis(0),
                });
                state.refresh(cx);
            });
        }
        cx.notify();
    }

    /// Build host state. Call [`Self::show_dialog`] after `open_window` returns.
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let load_requested = Arc::new(Mutex::new(0u32));
        let form = cx.new(|cx| ConnectionForm::new(None, String::new(), None, false, window, cx));
        form.update(cx, |form, cx| {
            form.test_seed_minimal_postgres(window, cx);
        });
        let view = Self::seed_sql_result_on_table_tab(window, cx);
        let subscription = cx.subscribe_in(&form, window, move |this, _, event, window, cx| {
            match event {
                FormEvent::Submit(_draft) => {
                    // Never log draft.password.
                    window.close_dialog(cx);
                    // Save reconnects then activates the tab (same as production).
                    this.activate_table_tab(cx);
                }
                FormEvent::Cancel => {
                    window.close_dialog(cx);
                    cx.notify();
                }
            }
        });
        Self {
            form,
            view,
            load_requested,
            _subscription: subscription,
        }
    }

    fn show_dialog(&self, window: &mut Window, cx: &mut App) {
        let title: SharedString = "Edit connection".into();
        let form_for_dialog = self.form.clone();
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title(title.clone())
                .width(px(560.))
                .on_ok(|_, _, _| false)
                .child(form_for_dialog.clone())
        });
    }

    fn grid_column_names(&self, cx: &gpui_kit::App) -> Vec<String> {
        self.view
            .grid
            .read(cx)
            .delegate()
            .result
            .columns()
            .iter()
            .map(|c| c.name.clone())
            .collect()
    }

    fn loads(&self) -> u32 {
        *self.load_requested.lock().expect("flag")
    }

    fn reseed_sql_on_table(&mut self, cx: &mut Context<Self>) {
        self.view.source = GridSource::Sql("SELECT 1;".into());
        self.view.page_len = 1;
        self.view.busy = false;
        self.view.grid.update(cx, |state, cx| {
            state.delegate_mut().show_page(QueryPage {
                columns: vec![ColumnMeta {
                    name: "?column?".into(),
                    type_name: "integer".into(),
                }],
                rows: vec![vec![Value::Int(1)]],
                duration: Duration::from_millis(0),
            });
            state.refresh(cx);
        });
        *self.load_requested.lock().expect("flag") = 0;
        cx.notify();
    }
}

impl Render for ReconnectHost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.entity();
        let cols = self.grid_column_names(cx).join(",");
        let source_label = self.view.source_label();
        div()
            .size_full()
            .id("reconnect-host")
            .flex()
            .flex_col()
            .gap_2()
            .child(format!("source={source_label}; cols={cols}"))
            .child(
                Button::new("switch-away-and-back")
                    .label("Switch away and back")
                    .on_click({
                        let this = this.clone();
                        move |_, _, cx| {
                            this.update(cx, |host, cx| {
                                // Leave the table tab and return — #98 also
                                // reloads on plain tab switch, not only Save.
                                host.activate_table_tab(cx);
                            });
                        }
                    }),
            )
            .child(
                gpui_kit::component::table::DataTable::new(&self.view.grid)
                    .stripe(true)
                    .into_any_element(),
            )
    }
}

fn assert_table_reloaded(host: &ReconnectHost, cx: &gpui_kit::App, loads_before: u32) {
    assert!(
        matches!(host.view.source, GridSource::Table { .. }),
        "tab chrome returns to the table"
    );
    assert!(
        host.loads() > loads_before,
        "table tab must reload when it last showed SQL (#98); loads={}",
        host.loads()
    );
    assert_ne!(
        host.grid_column_names(cx),
        vec!["?column?".to_string()],
        "stale SQL grid must not remain under the table title"
    );
}

async fn open_reconnect_host(
    cx: &mut TestAppContext,
) -> (gpui_kit::AnyWindowHandle, Entity<ReconnectHost>) {
    let (handle, host) =
        super::harness::open_window(cx, size(px(900.), px(1000.)), |window, cx| {
            cx.new(|cx| ReconnectHost::new(window, cx))
        });
    let handle = handle.into();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        host.update(cx, |host, cx| {
            host.show_dialog(window, cx);
        });
        window.render_frame(cx);
    })
    .unwrap();
    cx.wait_for(handle, Duration::from_secs(2), |window, _| {
        window.try_find("dialog").is_some()
    })
    .await;
    (handle, host)
}

/// #98: after Save/reconnect, a table tab that last ran SQL reloads table data.
#[gpui_kit::test]
async fn save_reconnect_refreshes_table_tab_after_sql(cx: &mut TestAppContext) {
    let (handle, host) = open_reconnect_host(cx).await;
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            host.read(cx).grid_column_names(cx),
            vec!["?column?".to_string()]
        );
        assert_eq!(host.read(cx).loads(), 0);
        window.within("dialog").click("name-input", cx);
        super::harness::click_in_dialog(window, "save-connection", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("dialog").is_none(), "Save closes dialog");
        assert_table_reloaded(host.read(cx), cx, 0);
    })
    .unwrap();
}

/// #98: plain tab switch away and back also reloads (not only Save/reconnect).
#[gpui_kit::test]
async fn tab_switch_refreshes_table_tab_after_sql(cx: &mut TestAppContext) {
    let (handle, host) = open_reconnect_host(cx).await;
    // Close the edit dialog without saving so we exercise tab switch alone.
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        super::harness::click_in_dialog(window, "cancel", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("dialog").is_none());
        host.update(cx, |host, cx| {
            host.reseed_sql_on_table(cx);
        });
        window.render_frame(cx);
        assert_eq!(
            host.read(cx).grid_column_names(cx),
            vec!["?column?".to_string()]
        );
        window.click("switch-away-and-back", cx);
        window.render_frame(cx);
        assert_table_reloaded(host.read(cx), cx, 0);
    })
    .unwrap();
}
