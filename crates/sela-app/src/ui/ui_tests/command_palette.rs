//! M3-4 command palette / quick open — drives the real Workspace + keybindings.

use std::path::PathBuf;
use std::time::Duration;

use gpui_kit::prelude::*;
use gpui_kit::test::{TestAppContextExt, TestWindowExt};
use gpui_kit::{point, px, size, App, Entity, Keystroke, TestAppContext, Window};
use tempfile::TempDir;

use sela_core::SafeMode;
use sela_drivers::create_sample_database;

use crate::ui::workspace::palette::{PaletteMode, PALETTE_MAX_HEIGHT, PALETTE_WIDTH};
use crate::ui::workspace::Workspace;
use crate::ui::{Quit, ToggleCommandPalette, ToggleQuickOpen};

struct PaletteFixture {
    _dir: TempDir,
    db_path: PathBuf,
    handle: gpui_kit::AnyWindowHandle,
    workspace: Entity<Workspace>,
}

/// Seed sample SQLite off the GPUI test executor (sqlx/tokio cannot park there).
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

async fn open_fixture(cx: &mut TestAppContext) -> PaletteFixture {
    // Real SQLite connect uses sela-drivers::drive (background tokio). Allow
    // parking so the test scheduler accepts cross-thread wakeups.
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
    wait_connected(cx, handle, &workspace).await;
    // Key chords need a focus path into Workspace's on_action listeners.
    focus_for_keys(cx, handle, &workspace);
    PaletteFixture {
        _dir: dir,
        db_path,
        handle,
        workspace,
    }
}

/// Give the window a focused SQL editor so global keymap actions dispatch.
fn focus_for_keys(
    cx: &mut TestAppContext,
    handle: gpui_kit::AnyWindowHandle,
    workspace: &Entity<Workspace>,
) {
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        workspace.update(cx, |ws, cx| {
            ws.test_new_query_tab(window, cx);
            ws.test_focus_sql_editor(window, cx);
        });
        window.render_frame(cx);
    })
    .unwrap();
}

async fn wait_connected(
    cx: &mut TestAppContext,
    handle: gpui_kit::AnyWindowHandle,
    workspace: &Entity<Workspace>,
) {
    cx.wait_for(handle, Duration::from_secs(8), |_, cx| {
        workspace.read(cx).test_is_connected() && workspace.read(cx).test_catalog_table_count() > 0
    })
    .await;
}

fn press_secondary(window: &mut Window, chord: &str, cx: &mut App) {
    window.press(chord, cx);
}

/// PAL-01: secondary-p opens; Escape closes; focus restored; size constants.
#[gpui_kit::test]
async fn pal01_open_escape_focus_restore(cx: &mut TestAppContext) {
    let fix = open_fixture(cx).await;
    assert_eq!(PALETTE_WIDTH, 520.);
    assert_eq!(PALETTE_MAX_HEIGHT, 420.);
    cx.update_window(fix.handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            fix.workspace.read(cx).test_editor_focused(window, cx),
            "SQL editor should be focused before opening palette"
        );
        press_secondary(window, "secondary-p", cx);
        window.render_frame(cx);
        assert!(fix.workspace.read(cx).palette_is_open());
        assert_eq!(
            fix.workspace.read(cx).palette_mode(),
            Some(PaletteMode::Tables)
        );
        assert!(window.try_find("command-palette").is_some());
        assert!(window.try_find("palette-search").is_some());
        window.press("escape", cx);
        window.render_frame(cx);
        assert!(!fix.workspace.read(cx).palette_is_open());
        assert!(
            fix.workspace.read(cx).test_editor_focused(window, cx),
            "Escape must restore focus to the previous SQL editor"
        );
    })
    .unwrap();
}

/// PAL-07: click outside closes; click on search keeps open; second chord toggles.
#[gpui_kit::test]
async fn pal07_click_outside_and_toggle(cx: &mut TestAppContext) {
    let fix = open_fixture(cx).await;
    cx.update_window(fix.handle, |_, window, cx| {
        window.render_frame(cx);
        press_secondary(window, "secondary-p", cx);
        window.render_frame(cx);
        assert!(fix.workspace.read(cx).palette_is_open());
        // Click on the search field must keep the palette open.
        window.click("palette-search", cx);
        window.render_frame(cx);
        assert!(
            fix.workspace.read(cx).palette_is_open(),
            "click on search must not close (occlude / mouse_down_out)"
        );
        // Click outside the panel (top-left of the root backdrop).
        window.click_at("command-palette-root", point(px(8.), px(8.)), cx);
        window.render_frame(cx);
        assert!(
            !fix.workspace.read(cx).palette_is_open(),
            "click outside must close"
        );
        // Second secondary-p toggles closed.
        press_secondary(window, "secondary-p", cx);
        window.render_frame(cx);
        assert!(fix.workspace.read(cx).palette_is_open());
        press_secondary(window, "secondary-p", cx);
        window.render_frame(cx);
        assert!(!fix.workspace.read(cx).palette_is_open());
        // Cross-mode switch keeps surface open.
        press_secondary(window, "secondary-p", cx);
        window.render_frame(cx);
        press_secondary(window, "secondary-shift-p", cx);
        window.render_frame(cx);
        assert!(fix.workspace.read(cx).palette_is_open());
        assert_eq!(
            fix.workspace.read(cx).palette_mode(),
            Some(PaletteMode::Actions)
        );
    })
    .unwrap();
}

/// PAL-02: fuzzy filter + Enter opens/focuses Data tab (no duplicate).
#[gpui_kit::test]
async fn pal02_quick_open_focuses_existing_tab(cx: &mut TestAppContext) {
    let fix = open_fixture(cx).await;
    cx.update_window(fix.handle, |_, window, cx| {
        window.render_frame(cx);
        press_secondary(window, "secondary-p", cx);
        window.render_frame(cx);
        assert!(
            fix.workspace.read(cx).palette_is_open(),
            "palette must be open before setting query"
        );
        fix.workspace.update(cx, |ws, cx| {
            ws.test_set_palette_query("books", window, cx);
        });
        window.render_frame(cx);
        window.render_frame(cx);
        window.press("enter", cx);
        window.render_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(fix.handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(!fix.workspace.read(cx).palette_is_open());
        let tabs_after_first = fix.workspace.read(cx).test_tab_count();
        assert!(tabs_after_first >= 1);
        let opened = fix.workspace.read(cx).test_active_table();
        assert!(
            opened.as_ref().is_some_and(|(_, t)| t == "books"),
            "expected books tab, got {opened:?}"
        );
        // Open the same table again — must focus, not duplicate.
        // Refocus the SQL editor so Workspace stays on the key-dispatch path
        // (DataTable focus alone can miss global palette chords in headless).
        fix.workspace.update(cx, |ws, cx| {
            ws.test_focus_sql_editor(window, cx);
        });
        window.render_frame(cx);
        press_secondary(window, "secondary-p", cx);
        window.render_frame(cx);
        assert!(
            fix.workspace.read(cx).palette_is_open(),
            "palette must be open before setting query"
        );
        fix.workspace.update(cx, |ws, cx| {
            ws.test_set_palette_query("books", window, cx);
        });
        window.render_frame(cx);
        window.press("enter", cx);
        window.render_frame(cx);
        assert_eq!(
            fix.workspace.read(cx).test_tab_count(),
            tabs_after_first,
            "opening an already-open table must not duplicate"
        );
    })
    .unwrap();
}

/// PAL-03 + PAL-05: action mode runs a real action via keyboard.
#[gpui_kit::test]
async fn pal03_pal05_action_runs_toggle_theme(cx: &mut TestAppContext) {
    let fix = open_fixture(cx).await;
    let before = cx
        .update_window(fix.handle, |_, _, cx| {
            fix.workspace.read(cx).test_theme_preference()
        })
        .unwrap();
    cx.update_window(fix.handle, |_, window, cx| {
        window.render_frame(cx);
        press_secondary(window, "secondary-shift-p", cx);
        window.render_frame(cx);
        assert_eq!(
            fix.workspace.read(cx).palette_mode(),
            Some(PaletteMode::Actions)
        );
        assert!(
            fix.workspace.read(cx).palette_is_open(),
            "palette must be open before setting query"
        );
        fix.workspace.update(cx, |ws, cx| {
            ws.test_set_palette_query("Toggle theme", window, cx);
        });
        window.render_frame(cx);
        window.press("down", cx);
        window.press("up", cx);
        window.press("enter", cx);
        window.render_frame(cx);
        assert!(!fix.workspace.read(cx).palette_is_open());
        let after = fix.workspace.read(cx).test_theme_preference();
        assert_ne!(before, after, "Toggle theme must change preference");
    })
    .unwrap();
}

/// PAL-06: dimmed Connect when already connected; Enter is a no-op.
#[gpui_kit::test]
async fn pal06_dimmed_connect_noop(cx: &mut TestAppContext) {
    let fix = open_fixture(cx).await;
    cx.update_window(fix.handle, |_, window, cx| {
        window.render_frame(cx);
        press_secondary(window, "secondary-shift-p", cx);
        window.render_frame(cx);
        assert!(
            fix.workspace.read(cx).palette_is_open(),
            "palette must be open before setting query"
        );
        fix.workspace.update(cx, |ws, cx| {
            ws.test_set_palette_query("Connect", window, cx);
        });
        window.render_frame(cx);
        let before_tabs = fix.workspace.read(cx).test_tab_count();
        window.press("enter", cx);
        window.render_frame(cx);
        assert!(
            fix.workspace.read(cx).palette_is_open(),
            "Enter on dimmed Connect must not close / run"
        );
        assert_eq!(fix.workspace.read(cx).test_tab_count(), before_tabs);
        window.press("escape", cx);
    })
    .unwrap();
}

/// PAL-08: secondary-q → Quit; nothing binds secondary-space.
#[gpui_kit::test]
async fn pal08_reserved_chords(cx: &mut TestAppContext) {
    let fix = open_fixture(cx).await;
    cx.update_window(fix.handle, |_, window, cx| {
        window.render_frame(cx);
        let os = cx.platform_info().os;
        let quit_bindings = window.bindings_for_action(&Quit);
        assert!(
            quit_bindings.iter().any(|b| {
                b.keystrokes()
                    .iter()
                    .any(|k| k.unparse(os).ends_with("-q") || k.unparse(os) == "q")
            }),
            "secondary-q must map to Quit; got {:?}",
            quit_bindings
                .iter()
                .map(|b| {
                    b.keystrokes()
                        .iter()
                        .map(|k| k.unparse(os))
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        );
        let space = Keystroke::parse_with_os("secondary-space", os).expect("parse secondary-space");
        let matches = cx.all_bindings_for_input(&[space]);
        assert!(
            matches.is_empty(),
            "secondary-space must stay unbound; found {:?}",
            matches
                .iter()
                .map(|b| b.action().name())
                .collect::<Vec<_>>()
        );
        let _ = ToggleQuickOpen;
        let _ = ToggleCommandPalette;
    })
    .unwrap();
}

/// PAL-09: disconnected / empty catalog shows muted empty row.
#[gpui_kit::test]
async fn pal09_empty_when_disconnected(cx: &mut TestAppContext) {
    let dir = TempDir::new().expect("temp");
    let dir_path = dir.path().to_path_buf();
    let (handle, workspace) =
        super::harness::open_window(cx, size(px(900.), px(700.)), |window, cx| {
            cx.new(|cx| Workspace::new_for_tests(&dir_path, window, cx))
        });
    let handle = handle.into();
    focus_for_keys(cx, handle, &workspace);
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(!workspace.read(cx).test_is_connected());
        press_secondary(window, "secondary-p", cx);
        window.render_frame(cx);
        assert!(workspace.read(cx).palette_is_open());
        assert!(window.try_find("command-palette").is_some());
        window.press("enter", cx);
        window.render_frame(cx);
        assert!(
            workspace.read(cx).palette_is_open(),
            "empty hint is not runnable"
        );
        // focus_for_keys opens one query tab; empty hint must not open another.
        assert_eq!(workspace.read(cx).test_tab_count(), 1);
    })
    .unwrap();
}

/// PAL-11: `>` switches table → action mode.
#[gpui_kit::test]
async fn pal11_gt_switches_to_actions(cx: &mut TestAppContext) {
    let fix = open_fixture(cx).await;
    cx.update_window(fix.handle, |_, window, cx| {
        window.render_frame(cx);
        press_secondary(window, "secondary-p", cx);
        window.render_frame(cx);
        assert!(
            fix.workspace.read(cx).palette_is_open(),
            "palette must be open before setting query"
        );
        fix.workspace.update(cx, |ws, cx| {
            ws.test_set_palette_query(">", window, cx);
        });
        window.render_frame(cx);
        window.render_frame(cx);
        assert_eq!(
            fix.workspace.read(cx).palette_mode(),
            Some(PaletteMode::Actions)
        );
    })
    .unwrap();
}

/// Preview chord does not fire while a text input (palette search) has focus.
#[gpui_kit::test]
async fn preview_skipped_when_input_focused(cx: &mut TestAppContext) {
    let fix = open_fixture(cx).await;
    cx.update_window(fix.handle, |_, window, cx| {
        window.render_frame(cx);
        press_secondary(window, "secondary-p", cx);
        window.render_frame(cx);
        assert!(
            fix.workspace.read(cx).palette_is_open(),
            "palette must be open before setting query"
        );
        fix.workspace.update(cx, |ws, cx| {
            ws.test_set_palette_query("books", window, cx);
        });
        window.render_frame(cx);
        window.press("enter", cx);
        window.render_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(fix.handle, |_, window, cx| {
        window.render_frame(cx);
        fix.workspace
            .update(cx, |ws, cx| ws.test_inject_pending_edit(cx));
        window.render_frame(cx);
        assert!(!fix.workspace.read(cx).test_preview_open());
        // Focus a text input, then fire Preview.
        fix.workspace.update(cx, |ws, cx| {
            ws.test_focus_sql_editor(window, cx);
        });
        window.render_frame(cx);
        press_secondary(window, "secondary-shift-p", cx);
        window.render_frame(cx);
        assert!(
            fix.workspace.read(cx).palette_is_open(),
            "action palette must open so search has focus"
        );
        // Search is focused on open; Preview must no-op while Input has focus.
        press_secondary(window, "secondary-shift-v", cx);
        window.render_frame(cx);
        assert!(
            !fix.workspace.read(cx).test_preview_open(),
            "Preview must not run while palette search has focus"
        );
        assert!(fix.workspace.read(cx).palette_is_open());
    })
    .unwrap();
}

/// PAL-07: open connection dialog swallows palette chords.
#[gpui_kit::test]
async fn pal07_dialog_eats_palette_chords(cx: &mut TestAppContext) {
    let fix = open_fixture(cx).await;
    cx.update_window(fix.handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("new-connection", cx);
        window.render_frame(cx);
    })
    .unwrap();
    cx.wait_for(fix.handle, Duration::from_secs(2), |window, _| {
        window.try_find("dialog").is_some()
    })
    .await;
    cx.update_window(fix.handle, |_, window, cx| {
        window.render_frame(cx);
        press_secondary(window, "secondary-p", cx);
        window.render_frame(cx);
        assert!(
            !fix.workspace.read(cx).palette_is_open(),
            "connection dialog must eat secondary-p"
        );
        press_secondary(window, "secondary-shift-p", cx);
        window.render_frame(cx);
        assert!(!fix.workspace.read(cx).palette_is_open());
    })
    .unwrap();
}

/// PAL-12: Toggle safe mode cycles, persists, keeps pending, reconnects.
#[gpui_kit::test]
async fn pal12_toggle_safe_mode_cycle(cx: &mut TestAppContext) {
    let fix = open_fixture(cx).await;
    let conn_id = cx
        .update_window(fix.handle, |_, window, cx| {
            window.render_frame(cx);
            press_secondary(window, "secondary-p", cx);
            window.render_frame(cx);
            assert!(
                fix.workspace.read(cx).palette_is_open(),
                "palette must be open before setting query"
            );
            fix.workspace.update(cx, |ws, cx| {
                ws.test_set_palette_query("books", window, cx);
            });
            window.render_frame(cx);
            window.press("enter", cx);
            window.render_frame(cx);
            fix.workspace.read(cx).test_active_id().unwrap().to_string()
        })
        .unwrap();
    cx.run_until_parked();
    cx.update_window(fix.handle, |_, window, cx| {
        window.render_frame(cx);
        fix.workspace
            .update(cx, |ws, cx| ws.test_inject_pending_edit(cx));
        assert!(fix.workspace.read(cx).test_pending_dirty());
        assert_eq!(
            fix.workspace.read(cx).test_active_safe_mode(),
            SafeMode::Off
        );
        // Off → Confirm
        fix.workspace.update(cx, |ws, cx| {
            ws.test_focus_sql_editor(window, cx);
        });
        window.render_frame(cx);
        press_secondary(window, "secondary-shift-p", cx);
        window.render_frame(cx);
        assert!(
            fix.workspace.read(cx).palette_is_open(),
            "palette must be open before setting query"
        );
        fix.workspace.update(cx, |ws, cx| {
            ws.test_set_palette_query("Toggle safe mode", window, cx);
        });
        window.render_frame(cx);
        window.press("enter", cx);
        window.render_frame(cx);
        // Label / in-memory value updates before reconnect finishes.
        assert_eq!(
            fix.workspace.read(cx).test_connection_safe_mode(&conn_id),
            Some(SafeMode::Confirm)
        );
        assert_eq!(
            fix.workspace.read(cx).test_active_safe_mode(),
            SafeMode::Confirm
        );
        assert_eq!(
            fix.workspace
                .read(cx)
                .test_safe_toolbar_segment()
                .as_deref(),
            Some("Safe: Confirm")
        );
        assert!(
            fix.workspace.read(cx).test_pending_dirty(),
            "pending edits must be kept across safe-mode toggle"
        );
        assert_eq!(
            fix.workspace.read(cx).test_persisted_safe_mode(&conn_id),
            Some(SafeMode::Confirm)
        );
    })
    .unwrap();
    wait_connected(cx, fix.handle, &fix.workspace).await;
    cx.update_window(fix.handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(fix.workspace.read(cx).test_pending_dirty());
        // Confirm → Read-only
        fix.workspace.update(cx, |ws, cx| {
            ws.test_focus_sql_editor(window, cx);
        });
        window.render_frame(cx);
        press_secondary(window, "secondary-shift-p", cx);
        window.render_frame(cx);
        assert!(
            fix.workspace.read(cx).palette_is_open(),
            "palette must be open before setting query"
        );
        fix.workspace.update(cx, |ws, cx| {
            ws.test_set_palette_query("Toggle safe mode", window, cx);
        });
        window.render_frame(cx);
        window.press("enter", cx);
        window.render_frame(cx);
        assert_eq!(
            fix.workspace.read(cx).test_active_safe_mode(),
            SafeMode::ReadOnly
        );
        assert_eq!(
            fix.workspace
                .read(cx)
                .test_safe_toolbar_segment()
                .as_deref(),
            Some("Safe: Read-only")
        );
    })
    .unwrap();
    wait_connected(cx, fix.handle, &fix.workspace).await;
    cx.update_window(fix.handle, |_, window, cx| {
        window.render_frame(cx);
        // Read-only → Off
        fix.workspace.update(cx, |ws, cx| {
            ws.test_focus_sql_editor(window, cx);
        });
        window.render_frame(cx);
        press_secondary(window, "secondary-shift-p", cx);
        window.render_frame(cx);
        assert!(
            fix.workspace.read(cx).palette_is_open(),
            "palette must be open before setting query"
        );
        fix.workspace.update(cx, |ws, cx| {
            ws.test_set_palette_query("Toggle safe mode", window, cx);
        });
        window.render_frame(cx);
        window.press("enter", cx);
        window.render_frame(cx);
        assert_eq!(
            fix.workspace.read(cx).test_active_safe_mode(),
            SafeMode::Off
        );
        assert!(fix.workspace.read(cx).test_pending_dirty());
        let _ = &fix.db_path;
    })
    .unwrap();
}

/// Cancelled-commit pending-bar copy (carry-over from #112).
#[gpui_kit::test]
fn cancelled_commit_pending_bar_copy(_cx: &mut TestAppContext) {
    use sela_core::{ColumnStructure, Dialect, IndexInfo, RelationKind, TableStructure, Value};

    use crate::ui::workspace::edit_session::{format_commit_failure, EditSession};
    use crate::ui::workspace::edit_ui::pending_bar_left_text;

    let structure = TableStructure {
        schema: "main".into(),
        table: "books".into(),
        columns: vec![
            ColumnStructure {
                name: "id".into(),
                type_name: "INTEGER".into(),
                nullable: false,
                default: None,
                comment: None,
                generated: false,
            },
            ColumnStructure {
                name: "title".into(),
                type_name: "TEXT".into(),
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
    };
    let mut edit = EditSession::from_structure(&structure, Dialect::Sqlite, RelationKind::Table);
    let pk = vec![Value::Int(1)];
    edit.changeset
        .as_mut()
        .unwrap()
        .set_cell(&pk, "title", Value::text("a"), Value::text("b"))
        .unwrap();
    let gen = edit.begin_commit();
    let err = sela_core::DbError::cancelled("apply cancelled");
    let (_banner, status) = format_commit_failure(&err, 1);
    assert_eq!(status, "Commit cancelled. No changes were saved.");
    assert!(edit.note_commit_failure(gen, false, true));
    let left = pending_bar_left_text(&edit, &edit.pending_counts());
    assert!(left.contains("Commit cancelled. No changes were saved."));
    assert!(!left.contains("Commit failed"));
}
