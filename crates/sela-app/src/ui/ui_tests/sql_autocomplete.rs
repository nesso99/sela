//! M3-2 SQL autocomplete headless UI tests (AU-* from v0.3-test-plan).
//!
//! Geometry and in-memory catalog cases use [`AutocompleteHost`] (real
//! [`Editor`] + [`CompletionProvider`]). Keybindings and schema refresh go
//! through [`Workspace::new_for_tests`] so Tab / Control+Space / Refresh
//! hit the production handlers (not a host re-implementation).

use std::time::Duration;

use gpui_kit::component::input::{Editor, EditorState, Enter};
use gpui_kit::prelude::*;
use gpui_kit::test::{TestAppContextExt, TestWindowExt};
use gpui_kit::{
    div, point, px, size, App, Context, Entity, Focusable, TestAppContext, Window,
};
use tempfile::TempDir;

use sela_core::{ColumnMeta, Dialect, SchemaEntry, SchemaSnapshot, TableEntry};
use sela_drivers::create_sample_database;

use crate::ui::workspace::sql_autocomplete::{install_test_completions, SchemaCacheHandle};
use crate::ui::workspace::Workspace;
use crate::ui::{AcceptCompletionOrIndent, ShowSqlCompletions};

struct AutocompleteHost {
    editor: Entity<EditorState>,
    cache: SchemaCacheHandle,
    /// Match the production SQL editor: docked at the bottom of the window so
    /// the kit flips the menu above when there is room (AU-12).
    docked_to_bottom: bool,
}

impl AutocompleteHost {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let cache = SchemaCacheHandle::new();
        cache.set_snapshot(
            Dialect::Postgres,
            SchemaSnapshot {
                schemas: vec![SchemaEntry {
                    name: "public".into(),
                    tables: vec![
                        TableEntry {
                            name: "books".into(),
                            columns: vec![
                                ColumnMeta {
                                    name: "id".into(),
                                    type_name: "integer".into(),
                                },
                                ColumnMeta {
                                    name: "title".into(),
                                    type_name: "text".into(),
                                },
                            ],
                        },
                        TableEntry {
                            name: "authors".into(),
                            columns: vec![ColumnMeta {
                                name: "name".into(),
                                type_name: "text".into(),
                            }],
                        },
                    ],
                }],
            },
            Some("public".into()),
        );
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .default_value("")
                .placeholder("SQL")
        });
        install_test_completions(&editor, cache.clone(), cx);
        Self {
            editor,
            cache,
            docked_to_bottom: false,
        }
    }

    fn focus_editor(&self, window: &mut Window, cx: &mut App) {
        self.editor.read(cx).focus_handle(cx).focus(window, cx);
    }

    fn buffer(&self, cx: &App) -> String {
        self.editor.read(cx).value().to_string()
    }

    fn popup_open(&self, cx: &App) -> bool {
        self.editor.read(cx).completion_menu_state().open
    }

    fn popup_labels(&self, cx: &App) -> Vec<String> {
        self.editor
            .read(cx)
            .completion_menu_state()
            .items
            .iter()
            .map(|i| i.label.clone())
            .collect()
    }
}

impl Render for AutocompleteHost {
    fn render(&mut self, _: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("sql-autocomplete-host")
            .size_full()
            .flex()
            .flex_col()
            .when(self.docked_to_bottom, |this| this.justify_end())
            .key_context("SqlEditor")
            .on_action({
                let editor = self.editor.clone();
                move |_: &ShowSqlCompletions, window, cx| {
                    editor.update(cx, |state, cx| {
                        state.show_completions(window, cx);
                    });
                }
            })
            .on_action({
                let editor = self.editor.clone();
                move |_: &AcceptCompletionOrIndent, window, cx| {
                    let open = editor.read(cx).completion_menu_state().open;
                    if open {
                        editor.update(cx, |state, cx| {
                            let _ = state.route_overlay_action(
                                Box::new(Enter {
                                    secondary: false,
                                    shift: false,
                                }),
                                window,
                                cx,
                            );
                        });
                    } else {
                        editor.update(cx, |state, cx| {
                            state.insert("\t", window, cx);
                        });
                    }
                }
            })
            .child(
                div()
                    .id("sql-editor-pane")
                    .w_full()
                    .h(px(if self.docked_to_bottom { 80. } else { 200. }))
                    .child(
                        Editor::new(&self.editor)
                            .size_full()
                            .appearance(true)
                            .bordered(true),
                    ),
            )
    }
}

fn open_host(cx: &mut TestAppContext) -> (gpui_kit::AnyWindowHandle, Entity<AutocompleteHost>) {
    let (handle, host) = super::harness::open_window(cx, size(px(900.), px(600.)), |window, cx| {
        cx.new(|cx| AutocompleteHost::new(window, cx))
    });
    let handle = handle.into();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        host.update(cx, |host, cx| host.focus_editor(window, cx));
        window.render_frame(cx);
    })
    .expect("focus editor");
    cx.run_until_parked();
    (handle, host)
}

fn settle(handle: gpui_kit::AnyWindowHandle, cx: &mut TestAppContext) {
    cx.run_until_parked();
    let _ = cx.update_window(handle, |_, window, cx| window.render_frame(cx));
    cx.run_until_parked();
}

fn type_into(handle: gpui_kit::AnyWindowHandle, text: &str, cx: &mut TestAppContext) {
    cx.update_window(handle, |_, window, cx| {
        window.input(text, cx);
    })
    .expect("input");
    settle(handle, cx);
}

/// AU-01: popup opens after two identifier characters, not one.
#[gpui_kit::test]
async fn au01_opens_after_two_ident_chars(cx: &mut TestAppContext) {
    let (handle, host) = open_host(cx);
    type_into(handle, "b", cx);
    cx.update_window(handle, |_, _, cx| {
        assert!(
            !host.read(cx).popup_open(cx),
            "one character must not open the popup"
        );
    })
    .unwrap();
    type_into(handle, "o", cx);
    cx.update_window(handle, |_, window, cx| {
        assert!(
            host.read(cx).popup_open(cx),
            "two characters must open the popup"
        );
        let labels = host.read(cx).popup_labels(cx);
        assert!(
            labels.iter().any(|l| l == "books"),
            "expected books in {labels:?}"
        );
        let _ = window.try_find("completion-menu");
    })
    .unwrap();
}

/// AU-02: popup opens immediately after `.` on a real table/alias/schema name.
#[gpui_kit::test]
async fn au02_opens_after_dot(cx: &mut TestAppContext) {
    let (handle, host) = open_host(cx);
    cx.update_window(handle, |_, window, cx| {
        host.update(cx, |host, cx| {
            host.editor.update(cx, |state, cx| {
                state.set_value("books", window, cx);
                state.set_selected_range(5..5, cx);
            });
        });
    })
    .unwrap();
    settle(handle, cx);
    type_into(handle, ".", cx);
    cx.update_window(handle, |_, _, cx| {
        assert!(
            host.read(cx).popup_open(cx),
            "dot after a table name must open the popup"
        );
        let labels = host.read(cx).popup_labels(cx);
        assert!(
            labels.iter().any(|l| l == "title" || l == "id"),
            "expected columns after books.; got {labels:?}"
        );
    })
    .unwrap();
}

/// AU-03: no open inside a comment.
#[gpui_kit::test]
async fn au03_no_open_inside_comment(cx: &mut TestAppContext) {
    let (handle, host) = open_host(cx);
    type_into(handle, "-- bo", cx);
    cx.update_window(handle, |_, _, cx| {
        assert!(
            !host.read(cx).popup_open(cx),
            "comment must not open the popup"
        );
    })
    .unwrap();
}

/// AU-05: ctrl-space opens manually; secondary-space does not.
///
/// On Linux `secondary` resolves to Control, so Control+Space and
/// `secondary-space` are the same physical chord. Simulate macOS so
/// `secondary-space` is ⌘Space (Spotlight) and must not open the popup.
#[gpui_kit::test]
async fn au05_ctrl_space_opens_secondary_space_free(cx: &mut TestAppContext) {
    let (handle, host) = open_host(cx);
    cx.set_platform_info(gpui_kit::PlatformInfo {
        os: gpui_kit::OperatingSystem::Mac,
        target: gpui_kit::PlatformTarget::Native,
    });
    cx.update_window(handle, |_, window, cx| {
        window.press("secondary-space", cx);
    })
    .unwrap();
    settle(handle, cx);
    cx.update_window(handle, |_, window, cx| {
        assert!(
            !host.read(cx).popup_open(cx),
            "secondary-space (⌘Space on macOS) must stay free"
        );
        window.press("ctrl-space", cx);
    })
    .unwrap();
    settle(handle, cx);
    cx.update_window(handle, |_, _, cx| {
        assert!(
            host.read(cx).popup_open(cx),
            "ctrl-space must open the popup"
        );
    })
    .unwrap();
}

/// AU-06: dialect keywords + catalog tables appear.
#[gpui_kit::test]
async fn au06_keywords_and_catalog_tables(cx: &mut TestAppContext) {
    let (handle, host) = open_host(cx);
    type_into(handle, "bo", cx);
    cx.update_window(handle, |_, _, cx| {
        let labels = host.read(cx).popup_labels(cx);
        assert!(labels.iter().any(|l| l == "books"), "{labels:?}");
    })
    .unwrap();
    cx.update_window(handle, |_, window, cx| {
        host.update(cx, |host, cx| {
            host.editor.update(cx, |state, cx| {
                state.set_value("SEL", window, cx);
                state.set_selected_range(3..3, cx);
                state.show_completions(window, cx);
            });
        });
    })
    .unwrap();
    settle(handle, cx);
    cx.update_window(handle, |_, _, cx| {
        let labels = host.read(cx).popup_labels(cx);
        assert!(labels.iter().any(|l| l == "SELECT"), "{labels:?}");
    })
    .unwrap();
}

/// AU-09: accepting a mixed-case name inserts dialect quotes.
#[gpui_kit::test]
async fn au09_quoted_insert(cx: &mut TestAppContext) {
    let (handle, host) = open_host(cx);
    cx.update_window(handle, |_, window, cx| {
        host.update(cx, |host, cx| {
            host.cache.set_snapshot(
                Dialect::Postgres,
                SchemaSnapshot {
                    schemas: vec![SchemaEntry {
                        name: "public".into(),
                        tables: vec![TableEntry {
                            name: "Odd Name".into(),
                            columns: vec![],
                        }],
                    }],
                },
                Some("public".into()),
            );
            host.editor.update(cx, |state, cx| {
                state.set_value("Od", window, cx);
                state.set_selected_range(2..2, cx);
                state.show_completions(window, cx);
            });
        });
    })
    .unwrap();
    settle(handle, cx);
    cx.update_window(handle, |_, window, cx| {
        assert!(host.read(cx).popup_open(cx));
        window.press("enter", cx);
    })
    .unwrap();
    settle(handle, cx);
    cx.update_window(handle, |_, _, cx| {
        let buf = host.read(cx).buffer(cx);
        assert!(
            buf.contains("\"Odd Name\""),
            "expected quoted insert in {buf:?}"
        );
    })
    .unwrap();
}

/// AU-10 / AU-11: popup element exists with bounded height when many items.
#[gpui_kit::test]
async fn au10_au11_popup_bounds(cx: &mut TestAppContext) {
    let (handle, host) = open_host(cx);
    let mut tables = Vec::new();
    for i in 0..40 {
        tables.push(TableEntry {
            name: format!("table_{i:02}"),
            columns: vec![],
        });
    }
    cx.update_window(handle, |_, window, cx| {
        host.update(cx, |host, cx| {
            host.cache.set_snapshot(
                Dialect::Postgres,
                SchemaSnapshot {
                    schemas: vec![SchemaEntry {
                        name: "public".into(),
                        tables,
                    }],
                },
                Some("public".into()),
            );
            host.editor.update(cx, |state, cx| {
                state.set_value("ta", window, cx);
                state.set_selected_range(2..2, cx);
                state.show_completions(window, cx);
            });
        });
    })
    .unwrap();
    settle(handle, cx);
    cx.update_window(handle, |_, window, cx| {
        assert!(host.read(cx).popup_open(cx));
        let menu = window.find("completion-menu");
        let h = menu.bounds().size.height;
        assert!(
            h <= px(260.),
            "popup height should stay near 8 rows; got {h:?}"
        );
        assert!(
            menu.bounds().size.width <= px(360.) + px(24.),
            "popup width should fit ≤360px labels"
        );
    })
    .unwrap();
}

/// AU-12: with ≥150px free above the caret, the popup must sit above it.
///
/// Placement uses [`sela_core::prefer_above_popup_origin`]. The host docks the
/// editor at the bottom (production layout) and seeds enough rows that the
/// menu cannot fit below, so the on-screen geometry matches the prefer-above
/// rule.
#[gpui_kit::test]
async fn au12_popup_prefers_above_when_room(cx: &mut TestAppContext) {
    let (handle, host) = open_host(cx);
    let mut tables = Vec::new();
    for i in 0..20 {
        tables.push(TableEntry {
            name: format!("books_{i:02}"),
            columns: vec![],
        });
    }
    cx.update_window(handle, |_, window, cx| {
        host.update(cx, |host, cx| {
            host.docked_to_bottom = true;
            host.cache.set_snapshot(
                Dialect::Postgres,
                SchemaSnapshot {
                    schemas: vec![SchemaEntry {
                        name: "public".into(),
                        tables,
                    }],
                },
                Some("public".into()),
            );
            host.editor.update(cx, |state, cx| {
                state.set_value("bo", window, cx);
                state.set_selected_range(2..2, cx);
                state.show_completions(window, cx);
            });
            cx.notify();
        });
    })
    .unwrap();
    settle(handle, cx);
    cx.update_window(handle, |_, window, cx| {
        assert!(host.read(cx).popup_open(cx));
        let menu = window.find("completion-menu").bounds();
        let caret_top = host.read(cx).editor.read(cx).cursor_layout().map_or(
            px(0.),
            |(caret, _)| caret.origin.y + host.read(cx).editor.read(cx).scroll_offset().y,
        );
        assert!(
            caret_top >= px(150.),
            "test setup needs ≥150px free above the caret; caret_top={caret_top:?}"
        );
        // Geometry must match the prefer-above rule (not merely "on screen").
        let expected_y = sela_core::prefer_above_popup_origin(
            f32::from(caret_top),
            f32::from(caret_top) + 20.0,
            f32::from(menu.size.height),
            f32::from(window.bounds().size.height),
            4.0,
        );
        assert!(
            (f32::from(menu.origin.y) - expected_y).abs() < 8.0,
            "popup y {:?} must match prefer-above origin {expected_y} (caret_top={caret_top:?}, h={:?})",
            menu.origin.y,
            menu.size.height
        );
        assert!(
            menu.bottom() <= caret_top + px(2.),
            "popup {menu:?} must sit above caret top {caret_top:?}"
        );
        assert!(menu.origin.y >= px(0.));
    })
    .unwrap();
}

/// AU-13: Enter accepts; Escape dismisses; Tab accepts when open / indents when closed;
/// click outside dismisses.
#[gpui_kit::test]
async fn au13_keys_accept_dismiss_tab(cx: &mut TestAppContext) {
    let (handle, host) = open_host(cx);
    type_into(handle, "bo", cx);
    cx.update_window(handle, |_, window, cx| {
        assert!(host.read(cx).popup_open(cx));
        let before = host.read(cx).buffer(cx);
        window.press("escape", cx);
        window.render_frame(cx);
        assert!(!host.read(cx).popup_open(cx));
        assert_eq!(host.read(cx).buffer(cx), before, "Escape leaves the text");
    })
    .unwrap();

    cx.update_window(handle, |_, window, cx| {
        host.update(cx, |host, cx| {
            host.editor.update(cx, |state, cx| {
                state.set_value("bo", window, cx);
                state.set_selected_range(2..2, cx);
                state.show_completions(window, cx);
            });
        });
    })
    .unwrap();
    settle(handle, cx);
    cx.update_window(handle, |_, window, cx| {
        window.dispatch_action(Box::new(AcceptCompletionOrIndent), cx);
    })
    .unwrap();
    settle(handle, cx);
    cx.update_window(handle, |_, window, cx| {
        let after_tab = host.read(cx).buffer(cx);
        assert!(
            after_tab.contains("books"),
            "Tab should accept; buffer={after_tab:?}"
        );
        host.update(cx, |host, cx| {
            host.editor.update(cx, |state, cx| {
                state.set_value("", window, cx);
                state.set_selected_range(0..0, cx);
            });
        });
    })
    .unwrap();
    settle(handle, cx);
    cx.update_window(handle, |_, window, cx| {
        assert!(!host.read(cx).popup_open(cx));
        window.dispatch_action(Box::new(AcceptCompletionOrIndent), cx);
    })
    .unwrap();
    settle(handle, cx);
    cx.update_window(handle, |_, _, cx| {
        assert_eq!(host.read(cx).buffer(cx), "\t");
    })
    .unwrap();

    // Click outside closes (kit `on_mouse_down_out` on the completion popover).
    cx.update_window(handle, |_, window, cx| {
        host.update(cx, |host, cx| {
            host.editor.update(cx, |state, cx| {
                state.set_value("bo", window, cx);
                state.set_selected_range(2..2, cx);
                state.show_completions(window, cx);
            });
        });
    })
    .unwrap();
    settle(handle, cx);
    cx.update_window(handle, |_, window, cx| {
        assert!(host.read(cx).popup_open(cx));
        let outside = point(px(600.), px(150.));
        window.drag(outside, outside, cx);
        window.render_frame(cx);
    })
    .unwrap();
    settle(handle, cx);
    cx.update_window(handle, |_, _, cx| {
        assert!(
            !host.read(cx).popup_open(cx),
            "click/drag outside must dismiss the popup"
        );
    })
    .unwrap();
}

/// AU-14: arrows do not open a closed popup.
#[gpui_kit::test]
async fn au14_arrows_do_not_open(cx: &mut TestAppContext) {
    let (handle, host) = open_host(cx);
    cx.update_window(handle, |_, window, cx| {
        assert!(!host.read(cx).popup_open(cx));
        window.press("up", cx);
        window.press("down", cx);
        window.press("left", cx);
        window.press("right", cx);
        window.render_frame(cx);
        assert!(!host.read(cx).popup_open(cx));
    })
    .unwrap();
}

/// AU-16: loading row while cache is Loading; Enter must not insert that text.
#[gpui_kit::test]
async fn au16_loading_row_not_inserted(cx: &mut TestAppContext) {
    let (handle, host) = open_host(cx);
    cx.update_window(handle, |_, window, cx| {
        host.update(cx, |host, _cx| {
            host.cache.set_loading(Dialect::Postgres);
        });
        host.update(cx, |host, cx| {
            host.editor.update(cx, |state, cx| {
                state.show_completions(window, cx);
            });
        });
    })
    .unwrap();
    settle(handle, cx);
    cx.update_window(handle, |_, window, cx| {
        assert!(host.read(cx).popup_open(cx));
        let labels = host.read(cx).popup_labels(cx);
        assert_eq!(labels, vec!["Loading schema…".to_string()]);
        window.press("enter", cx);
    })
    .unwrap();
    settle(handle, cx);
    cx.update_window(handle, |_, _, cx| {
        assert_eq!(
            host.read(cx).buffer(cx),
            "",
            "Loading schema… must not be inserted"
        );
    })
    .unwrap();
}

/// AU-04: IME composition cannot be injected in TestAppContext for this
/// milestone. Manual-at-RC (M-AU-03). Kit `show_completions` refuses while
/// composition is active.
#[gpui_kit::test]
async fn au04_ime_documented_manual_at_rc(_cx: &mut TestAppContext) {}

// --- Workspace::new_for_tests paths (production handlers + catalog refresh) ---

struct WorkspaceFix {
    _dir: TempDir,
    handle: gpui_kit::AnyWindowHandle,
    workspace: Entity<Workspace>,
}

fn seed_sample_db(path: &std::path::Path) {
    let path = path.to_path_buf();
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("seed runtime")
            .block_on(create_sample_database(&path))
    })
    .join()
    .expect("seed thread")
    .expect("sample sqlite");
}

async fn open_workspace(cx: &mut TestAppContext) -> WorkspaceFix {
    cx.executor().allow_parking();
    let dir = TempDir::new().expect("temp dir");
    let db_path = dir.path().join("sample.sqlite");
    seed_sample_db(&db_path);
    let dir_path = dir.path().to_path_buf();
    let (handle, workspace) =
        super::harness::open_window(cx, size(px(1180.), px(760.)), |window, cx| {
            cx.new(|cx| Workspace::new_for_tests(&dir_path, window, cx))
        });
    let handle = handle.into();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let id = workspace.update(cx, |ws, cx| {
            ws.test_seed_sqlite_connection("Sample", &db_path, cx)
        });
        workspace.update(cx, |ws, cx| ws.test_connect(id, window, cx));
        window.render_frame(cx);
    })
    .unwrap();
    cx.wait_for(handle, Duration::from_secs(8), |_, cx| {
        workspace.read(cx).test_is_connected()
            && workspace.read(cx).test_schema_cache_is_ready()
            && workspace.read(cx).test_schema_cache_ready_table_count() > 0
    })
    .await;
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        workspace.update(cx, |ws, cx| {
            ws.test_new_query_tab(window, cx);
            ws.test_focus_sql_editor(window, cx);
        });
        window.render_frame(cx);
    })
    .unwrap();
    WorkspaceFix {
        _dir: dir,
        handle,
        workspace,
    }
}

/// Production ShowSqlCompletions / AcceptCompletionOrIndent (not AutocompleteHost).
#[gpui_kit::test]
async fn workspace_ctrl_space_and_tab_accept(cx: &mut TestAppContext) {
    let fix = open_workspace(cx).await;
    cx.set_platform_info(gpui_kit::PlatformInfo {
        os: gpui_kit::OperatingSystem::Mac,
        target: gpui_kit::PlatformTarget::Native,
    });
    cx.update_window(fix.handle, |_, window, cx| {
        fix.workspace.update(cx, |ws, cx| {
            ws.test_set_sql("bo", window, cx);
            ws.test_focus_sql_editor(window, cx);
        });
        window.press("secondary-space", cx);
    })
    .unwrap();
    settle(fix.handle, cx);
    cx.update_window(fix.handle, |_, window, cx| {
        assert!(
            !fix.workspace.read(cx).test_completion_popup_open(cx),
            "secondary-space must stay free"
        );
        window.press("ctrl-space", cx);
    })
    .unwrap();
    settle(fix.handle, cx);
    cx.update_window(fix.handle, |_, window, cx| {
        assert!(
            fix.workspace.read(cx).test_completion_popup_open(cx),
            "ctrl-space must open via Workspace handler"
        );
        let labels = fix.workspace.read(cx).test_completion_labels(cx);
        assert!(
            labels.iter().any(|l| l.eq_ignore_ascii_case("books")),
            "expected books; got {labels:?}"
        );
        window.dispatch_action(Box::new(AcceptCompletionOrIndent), cx);
    })
    .unwrap();
    settle(fix.handle, cx);
    cx.update_window(fix.handle, |_, _, cx| {
        let buf = fix.workspace.read(cx).test_sql_buffer(cx);
        assert!(
            buf.to_ascii_lowercase().contains("books"),
            "Tab accept via Workspace; buffer={buf:?}"
        );
    })
    .unwrap();
}

/// AU-16: Refresh schema reloads the autocomplete cache (Workspace path).
#[gpui_kit::test]
async fn workspace_refresh_reloads_schema_cache(cx: &mut TestAppContext) {
    let fix = open_workspace(cx).await;
    let before = cx.update(|cx| fix.workspace.read(cx).test_schema_cache_generation());
    cx.update_window(fix.handle, |_, _, cx| {
        fix.workspace.update(cx, |ws, cx| ws.test_refresh_catalog(cx));
    })
    .unwrap();
    cx.wait_for(fix.handle, Duration::from_secs(8), |_, cx| {
        let ws = fix.workspace.read(cx);
        ws.test_schema_cache_is_ready() && ws.test_schema_cache_generation() != before
    })
    .await;
    cx.update(|cx| {
        let ws = fix.workspace.read(cx);
        assert!(ws.test_schema_cache_ready_table_count() > 0);
        assert_ne!(ws.test_schema_cache_generation(), before);
    });
}

/// AU-16: successful DDL triggers the catalog-refresh predicate → cache reload.
#[gpui_kit::test]
async fn workspace_ddl_triggers_schema_cache_reload(cx: &mut TestAppContext) {
    let fix = open_workspace(cx).await;
    let before = cx.update(|cx| fix.workspace.read(cx).test_schema_cache_generation());
    // Drive the same Workspace entry as query.rs after a successful run.
    cx.update_window(fix.handle, |_, _, cx| {
        fix.workspace.update(cx, |ws, cx| {
            ws.test_maybe_refresh_schema_after_sql(
                "CREATE TABLE au_ddl_probe (id INTEGER PRIMARY KEY)",
                cx,
            );
        });
    })
    .unwrap();
    cx.wait_for(fix.handle, Duration::from_secs(8), |_, cx| {
        let ws = fix.workspace.read(cx);
        ws.test_schema_cache_is_ready() && ws.test_schema_cache_generation() != before
    })
    .await;
    // SELECT must not refresh.
    let mid = cx.update(|cx| fix.workspace.read(cx).test_schema_cache_generation());
    cx.update_window(fix.handle, |_, _, cx| {
        fix.workspace.update(cx, |ws, cx| {
            ws.test_maybe_refresh_schema_after_sql("SELECT 1", cx);
        });
    })
    .unwrap();
    settle(fix.handle, cx);
    cx.update(|cx| {
        assert_eq!(
            fix.workspace.read(cx).test_schema_cache_generation(),
            mid,
            "SELECT must not reload the schema cache"
        );
    });
}
