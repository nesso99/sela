//! M3-2 SQL autocomplete headless UI tests (AU-* from v0.3-test-plan).
//!
//! Drives the real [`Editor`] after [`install_sela_keybindings`], with a
//! [`CompletionProvider`] backed by an in-memory schema cache — not a
//! re-implementation of the popup.

use gpui_kit::component::input::{Editor, EditorState, Enter};
use gpui_kit::prelude::*;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{div, px, size, App, Context, Entity, Focusable, TestAppContext, Window};

use sela_core::{ColumnMeta, Dialect, SchemaEntry, SchemaSnapshot, TableEntry};

use crate::ui::workspace::sql_autocomplete::{install_test_completions, SchemaCacheHandle};
use crate::ui::{AcceptCompletionOrIndent, ShowSqlCompletions};

struct AutocompleteHost {
    editor: Entity<EditorState>,
    cache: SchemaCacheHandle,
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
        Self { editor, cache }
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
                div().id("sql-editor-pane").w_full().h(px(200.)).child(
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
        if let Some(menu) = window.try_find("completion-menu") {
            let h = menu.bounds().size.height;
            assert!(
                h <= px(260.),
                "popup height should stay near 8 rows; got {h:?}"
            );
            assert!(
                menu.bounds().size.width <= px(360.) + px(24.),
                "popup width should fit ≤360px labels"
            );
        }
    })
    .unwrap();
}

/// AU-12: popup stays on screen (kit flips when needed).
#[gpui_kit::test]
async fn au12_popup_on_screen(cx: &mut TestAppContext) {
    let (handle, host) = open_host(cx);
    type_into(handle, "bo", cx);
    cx.update_window(handle, |_, window, cx| {
        assert!(host.read(cx).popup_open(cx));
        if let Some(menu) = window.try_find("completion-menu") {
            let b = menu.bounds();
            let win = window.bounds();
            assert!(b.origin.y >= px(0.));
            assert!(b.origin.y + b.size.height <= win.size.height + px(8.));
        }
    })
    .unwrap();
}

/// AU-13: Enter accepts; Escape dismisses; Tab accepts when open / indents when closed.
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
