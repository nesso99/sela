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

/// Type into the palette search via real window input (PAL-02 / PAL-05).
/// Headless `window.input` updates InputState but may skip InputEvent::Change,
/// so we sync the filter afterward (same path as the production Change handler).
fn type_into_palette(window: &mut Window, workspace: &Entity<Workspace>, text: &str, cx: &mut App) {
    window.click("palette-search", cx);
    window.input(text, cx);
    workspace.update(cx, |ws, cx| {
        ws.test_sync_palette_query(window, cx);
    });
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

fn settle_palette_ui(handle: gpui_kit::AnyWindowHandle, cx: &mut TestAppContext) {
    cx.run_until_parked();
    let _ = cx.update_window(handle, |_, window, cx| window.render_frame(cx));
    cx.run_until_parked();
}

/// #133: opening palette or quick open dismisses the SQL completion popup.
#[gpui_kit::test]
async fn pal33_open_palette_dismisses_sql_completion(cx: &mut TestAppContext) {
    let fix = open_fixture(cx).await;
    cx.update_window(fix.handle, |_, window, cx| {
        fix.workspace.update(cx, |ws, cx| {
            ws.test_set_sql("autho", window, cx);
            ws.test_focus_sql_editor(window, cx);
            ws.test_show_sql_completions(window, cx);
        });
    })
    .unwrap();
    settle_palette_ui(fix.handle, cx);
    cx.update_window(fix.handle, |_, window, cx| {
        assert!(
            fix.workspace.read(cx).test_completion_popup_open(cx),
            "completion should be open before palette"
        );
        press_secondary(window, "secondary-shift-p", cx);
        window.render_frame(cx);
        assert!(
            fix.workspace.read(cx).palette_is_open(),
            "command palette should open"
        );
        assert!(
            !fix.workspace.read(cx).test_completion_popup_open(cx),
            "command palette must dismiss SQL completion overlay"
        );
    })
    .unwrap();
    cx.update_window(fix.handle, |_, window, cx| {
        window.press("escape", cx);
    })
    .unwrap();
    settle_palette_ui(fix.handle, cx);
    cx.update_window(fix.handle, |_, window, cx| {
        fix.workspace.update(cx, |ws, cx| {
            ws.test_set_sql("autho", window, cx);
            ws.test_focus_sql_editor(window, cx);
            ws.test_show_sql_completions(window, cx);
        });
    })
    .unwrap();
    settle_palette_ui(fix.handle, cx);
    cx.update_window(fix.handle, |_, window, cx| {
        assert!(fix.workspace.read(cx).test_completion_popup_open(cx));
        press_secondary(window, "secondary-p", cx);
        window.render_frame(cx);
        assert!(fix.workspace.read(cx).palette_is_open());
        assert!(
            !fix.workspace.read(cx).test_completion_popup_open(cx),
            "quick open must dismiss SQL completion overlay"
        );
    })
    .unwrap();
}

/// PAL-07: click outside closes (+ focus restore); click on search keeps open; toggle.
#[gpui_kit::test]
async fn pal07_click_outside_and_toggle(cx: &mut TestAppContext) {
    let fix = open_fixture(cx).await;
    cx.update_window(fix.handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            fix.workspace.read(cx).test_editor_focused(window, cx),
            "SQL editor focused before open"
        );
        press_secondary(window, "secondary-p", cx);
        window.render_frame(cx);
        assert!(fix.workspace.read(cx).palette_is_open());
        // Click on the search field must keep the palette open.
        window.click("palette-search", cx);
        window.render_frame(cx);
        assert!(
            fix.workspace.read(cx).palette_is_open(),
            "click on search must not close (panel occlude)"
        );
        // Click outside the panel (top-left of the root backdrop).
        window.click_at("command-palette-root", point(px(8.), px(8.)), cx);
        window.render_frame(cx);
        assert!(
            !fix.workspace.read(cx).palette_is_open(),
            "click outside must close"
        );
        assert!(
            fix.workspace.read(cx).test_editor_focused(window, cx),
            "click outside must restore focus to the SQL editor"
        );
        // Backdrop over New connection: closes palette, does not open the dialog.
        press_secondary(window, "secondary-p", cx);
        window.render_frame(cx);
        assert!(fix.workspace.read(cx).palette_is_open());
        let btn = window
            .try_find("new-connection")
            .expect("new-connection control");
        let root = window
            .try_find("command-palette-root")
            .expect("command-palette-root");
        let btn_b = btn.bounds();
        let root_b = root.bounds();
        let over_new = point(
            btn_b.origin.x + btn_b.size.width / 2. - root_b.origin.x,
            btn_b.origin.y + btn_b.size.height / 2. - root_b.origin.y,
        );
        window.click_at("command-palette-root", over_new, cx);
        window.render_frame(cx);
        assert!(
            !fix.workspace.read(cx).palette_is_open(),
            "backdrop click over New must close palette"
        );
        assert!(
            !fix.workspace.read(cx).test_connection_dialog_open(),
            "backdrop click must not open New connection dialog"
        );
        assert!(
            fix.workspace.read(cx).test_editor_focused(window, cx),
            "backdrop click must restore editor focus"
        );
        assert!(window.try_find("dialog").is_none());
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
        assert!(fix.workspace.read(cx).palette_is_open());
        type_into_palette(window, &fix.workspace, "books", cx);
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
        assert!(fix.workspace.read(cx).palette_is_open());
        type_into_palette(window, &fix.workspace, "books", cx);
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

/// PAL-03 + PAL-05: action mode runs a real action via keyboard typing.
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
        assert!(fix.workspace.read(cx).palette_is_open());
        type_into_palette(window, &fix.workspace, "Toggle theme", cx);
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

/// PAL-08: secondary-q → Quit; secondary-space stays Spotlight-free (ux/12).
///
/// From [ux/12-command-palette.md](docs/design/ux/12-command-palette.md):
/// "`secondary-space` stays Spotlight-free. `secondary-q` still quits
/// (PAL-08). `ctrl-space` is the autocomplete manual trigger in the editor;
/// the palette does not bind it."
///
/// On macOS `secondary` is ⌘, so secondary-space must be completely unbound.
/// On Linux/Windows `secondary` is Ctrl, so secondary-space is the same chord
/// as ctrl-space — the only allowed binding is `ShowSqlCompletions` scoped to
/// `SqlEditor` (not a global / palette steal).
#[gpui_kit::test]
async fn pal08_reserved_chords(cx: &mut TestAppContext) {
    let fix = open_fixture(cx).await;
    cx.update_window(fix.handle, |_, window, cx| {
        window.render_frame(cx);
        let os = cx.platform_info().os;
        let quit_chord = Keystroke::parse_with_os("secondary-q", os).expect("parse secondary-q");
        let quit_matches = cx.all_bindings_for_input(&[quit_chord]);
        assert!(
            quit_matches
                .iter()
                .any(|b| b.action().name().ends_with("Quit") || b.action().name() == "Quit"),
            "secondary-q must resolve to Quit; got {:?}",
            quit_matches
                .iter()
                .map(|b| b.action().name())
                .collect::<Vec<_>>()
        );
        // Also confirm Quit's own binding list includes secondary-q (not merely *-q).
        let quit_bindings = window.bindings_for_action(&Quit);
        assert!(
            quit_bindings.iter().any(|b| {
                b.keystrokes().iter().any(|k| {
                    k.unparse(os) == "secondary-q"
                        || k.unparse(os) == "cmd-q"
                        || k.unparse(os) == "ctrl-q"
                })
            }),
            "Quit must be bound to secondary-q; got {:?}",
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
        let space_matches = cx.all_bindings_for_input(&[space]);
        match os {
            gpui_kit::OperatingSystem::Mac => {
                assert!(
                    space_matches.is_empty(),
                    "secondary-space stays Spotlight-free on macOS; found {:?}",
                    space_matches
                        .iter()
                        .map(|b| b.action().name())
                        .collect::<Vec<_>>()
                );
            }
            _ => {
                // secondary == Control: only SqlEditor-scoped autocomplete may bind.
                for b in &space_matches {
                    let name = b.action().name();
                    assert!(
                        name.ends_with("ShowSqlCompletions"),
                        "outside SqlEditor, secondary-space must stay free; got {name}"
                    );
                    let pred = b.predicate().map(|p| p.to_string()).unwrap_or_default();
                    assert_eq!(
                        pred, "SqlEditor",
                        "ShowSqlCompletions must be exactly SqlEditor-scoped; predicate={pred:?}"
                    );
                }
            }
        }

        // ctrl-space remains editor autocomplete; palette does not bind it.
        // On every OS (including macOS), every ctrl-space binding is ShowSqlCompletions.
        let ctrl_space = Keystroke::parse_with_os("ctrl-space", os).expect("parse ctrl-space");
        let ctrl_matches = cx.all_bindings_for_input(&[ctrl_space]);
        assert!(
            !ctrl_matches.is_empty(),
            "ctrl-space must bind ShowSqlCompletions"
        );
        for b in &ctrl_matches {
            assert!(
                b.action().name().ends_with("ShowSqlCompletions"),
                "every ctrl-space binding must be ShowSqlCompletions; got {}",
                b.action().name()
            );
            let pred = b.predicate().map(|p| p.to_string()).unwrap_or_default();
            assert_eq!(
                pred, "SqlEditor",
                "ctrl-space autocomplete must be exactly SqlEditor-scoped; predicate={pred:?}"
            );
        }

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

/// Preview: positive control on the grid, then blocked while a text input in
/// the grid scope (filter) has focus (`text_input_focused` / handle_preview_changes).
#[gpui_kit::test]
async fn preview_skipped_when_input_focused(cx: &mut TestAppContext) {
    let fix = open_fixture(cx).await;
    cx.update_window(fix.handle, |_, window, cx| {
        window.render_frame(cx);
        press_secondary(window, "secondary-p", cx);
        window.render_frame(cx);
        type_into_palette(window, &fix.workspace, "books", cx);
        window.render_frame(cx);
        window.press("enter", cx);
        window.render_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
    // Positive: DataTable focused + pending edit → secondary-shift-v opens Preview.
    cx.update_window(fix.handle, |_, window, cx| {
        window.render_frame(cx);
        fix.workspace
            .update(cx, |ws, cx| ws.test_inject_pending_edit(cx));
        window.render_frame(cx);
        assert!(!fix.workspace.read(cx).test_preview_open());
        // col 1 = title (editable); sample books has at least one row.
        fix.workspace.update(cx, |ws, cx| {
            ws.test_focus_grid(0, 1, window, cx);
        });
        window.render_frame(cx);
        press_secondary(window, "secondary-shift-v", cx);
        window.render_frame(cx);
        assert!(
            fix.workspace.read(cx).test_preview_open(),
            "Preview must open when the grid has focus and pending edits"
        );
        // Close Preview for the negative control.
        press_secondary(window, "secondary-shift-v", cx);
        window.render_frame(cx);
        assert!(!fix.workspace.read(cx).test_preview_open());
    })
    .unwrap();
    // Negative: filter text input in the grid scope → Preview stays closed.
    // Chord is NoAction under FilterBar (paste); also call the same
    // handle_preview_changes path the Workspace on_action uses so the
    // text_input_focused guard is covered (removing it makes this fail).
    cx.update_window(fix.handle, |_, window, cx| {
        window.render_frame(cx);
        fix.workspace.update(cx, |ws, cx| {
            ws.test_focus_filter_bar(window, cx);
        });
        window.render_frame(cx);
        assert!(
            fix.workspace.read(cx).test_text_input_focused(window, cx),
            "filter field must be focused"
        );
        press_secondary(window, "secondary-shift-v", cx);
        window.render_frame(cx);
        assert!(
            !fix.workspace.read(cx).test_preview_open(),
            "chord must not open Preview while a filter field has focus"
        );
        fix.workspace.update(cx, |ws, cx| {
            ws.test_handle_preview_changes(window, cx);
        });
        window.render_frame(cx);
        assert!(
            !fix.workspace.read(cx).test_preview_open(),
            "text_input_focused guard must block Preview with a filter field focused"
        );
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

/// PAL-12 with SSH enabled on the active connection: after #122,
/// `cycle_safe_mode_from_palette` → `proceed_connect_id` → `connect_auth_for`
/// → `ConnectOutcome`. Bastion is a closed local port so reconnect fails;
/// Safe label + persist still update first, and pending edits are kept.
#[gpui_kit::test]
async fn pal12_safe_mode_cycle_ssh_connect_outcome(cx: &mut TestAppContext) {
    let fix = open_fixture(cx).await;
    let conn_id = cx
        .update_window(fix.handle, |_, window, cx| {
            window.render_frame(cx);
            // Keep the live SQLite session so Toggle safe mode is enabled, then
            // retarget the saved row as Postgres+SSH for the reconnect path.
            let id = fix
                .workspace
                .update(cx, |ws, _| ws.test_retarget_active_as_postgres_ssh())
                .expect("active connection");
            fix.workspace
                .update(cx, |ws, cx| ws.test_inject_pending_edit(cx));
            assert!(fix.workspace.read(cx).test_pending_dirty());
            assert!(fix.workspace.read(cx).test_is_connected());
            fix.workspace
                .update(cx, |ws, cx| ws.test_focus_sql_editor(window, cx));
            window.render_frame(cx);
            press_secondary(window, "secondary-shift-p", cx);
            window.render_frame(cx);
            assert!(fix.workspace.read(cx).palette_is_open());
            fix.workspace.update(cx, |ws, cx| {
                ws.test_set_palette_query("Toggle safe mode", window, cx);
            });
            window.render_frame(cx);
            window.press("enter", cx);
            window.render_frame(cx);
            assert_eq!(
                fix.workspace.read(cx).test_connection_safe_mode(&id),
                Some(SafeMode::Confirm)
            );
            assert_eq!(
                fix.workspace
                    .read(cx)
                    .test_safe_toolbar_segment()
                    .as_deref(),
                Some("Safe: Confirm")
            );
            assert_eq!(
                fix.workspace.read(cx).test_persisted_safe_mode(&id),
                Some(SafeMode::Confirm)
            );
            assert!(
                fix.workspace.read(cx).test_pending_dirty(),
                "pending edits must survive SSH reconnect attempt"
            );
            id
        })
        .unwrap();
    // SSH reconnect attempt fails (closed port) via ConnectOutcome/SSH path.
    cx.wait_for(fix.handle, Duration::from_secs(8), |_, cx| {
        fix.workspace.read(cx).test_view_error().is_some()
            && !fix.workspace.read(cx).test_is_connected()
    })
    .await;
    let err = cx
        .update(|cx| fix.workspace.read(cx).test_view_error())
        .expect("SSH reconnect should surface an error");
    assert!(
        err.starts_with("SSH:") || err.contains("SSH:"),
        "expected SSH: prefix on reconnect failure, got: {err}"
    );
    // Mode stayed Confirm through the failed SSH reconnect; pending kept.
    cx.update(|cx| {
        assert_eq!(
            fix.workspace.read(cx).test_persisted_safe_mode(&conn_id),
            Some(SafeMode::Confirm)
        );
        assert!(fix.workspace.read(cx).test_pending_dirty());
    });
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
            partial: false,
            expression: false,
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
