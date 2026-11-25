//! EX-06 Workspace wiring: Stop cancels a real export job; generation gate
//! ignores stale completions. Uses `Workspace::new_for_tests` (no fake hosts).

use std::path::PathBuf;
use std::time::Duration;

use gpui_kit::prelude::*;
use gpui_kit::test::{TestAppContextExt, TestWindowExt};
use gpui_kit::{px, size, Entity, TestAppContext};
use tempfile::TempDir;

use sela_core::EXPORT_CANCELLED_STATUS;
use sela_drivers::create_sample_database;

use crate::ui::workspace::Workspace;
use crate::ui::CancelQuery;

struct TransferFixture {
    _dir: TempDir,
    out_dir: PathBuf,
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

async fn open_fixture(cx: &mut TestAppContext) -> TransferFixture {
    cx.executor().allow_parking();
    let dir = TempDir::new().expect("temp dir");
    let db_path = dir.path().join("sample.sqlite");
    seed_sample_db(&db_path);
    let out_dir = dir.path().join("exports");
    std::fs::create_dir_all(&out_dir).unwrap();
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
    // Focus path so CancelQuery dispatches into Workspace.
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        workspace.update(cx, |ws, cx| {
            ws.test_new_query_tab(window, cx);
            ws.test_focus_sql_editor(window, cx);
        });
        window.render_frame(cx);
    })
    .unwrap();
    TransferFixture {
        _dir: dir,
        out_dir,
        handle,
        workspace,
    }
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

fn open_books_data(
    cx: &mut TestAppContext,
    handle: gpui_kit::AnyWindowHandle,
    workspace: &Entity<Workspace>,
) {
    cx.update_window(handle, |_, window, cx| {
        workspace.update(cx, |ws, cx| {
            ws.test_open_table("main", "books", window, cx);
        });
        window.render_frame(cx);
    })
    .unwrap();
}

/// EX-06: real Workspace export + Stop via CancelQuery clears the job with
/// `Export cancelled.` Catches removal of `job.cancel.cancel()`.
///
/// The export parks on a per-Workspace hold until cancel, so Stop cannot lose
/// a race with a tiny table finishing first (Windows CI flake).
#[gpui_kit::test]
async fn ex06_workspace_stop_cancels_running_export(cx: &mut TestAppContext) {
    let fix = open_fixture(cx).await;
    open_books_data(cx, fix.handle, &fix.workspace);

    let dest = fix.out_dir.join("books.csv");
    cx.update_window(fix.handle, |_, window, cx| {
        fix.workspace.update(cx, |ws, cx| {
            ws.test_focus_sql_editor(window, cx);
            ws.test_arm_export_hold();
            ws.test_start_export(dest.clone(), cx);
        });
        window.render_frame(cx);
        assert!(
            fix.workspace.read(cx).test_has_transfer_job(),
            "export job must be installed"
        );
    })
    .unwrap();

    // Background `drive` parks on the hold; wait until it has entered the gate.
    cx.wait_for(fix.handle, Duration::from_secs(8), |_, cx| {
        fix.workspace.read(cx).test_export_hold_reached()
    })
    .await;
    assert!(
        fix.workspace
            .read_with(cx, |ws, _| ws.test_has_transfer_job()),
        "job must still be in flight when hold is reached"
    );

    // Real CancelQuery action (same path as Stop / secondary-.).
    cx.update_window(fix.handle, |_, window, cx| {
        window.dispatch_action(Box::new(CancelQuery), cx);
        window.render_frame(cx);
    })
    .unwrap();

    cx.wait_for(fix.handle, Duration::from_secs(8), |_, cx| {
        let ws = fix.workspace.read(cx);
        !ws.test_has_transfer_job()
            && ws
                .test_active_status()
                .as_deref()
                .is_some_and(|s| s == EXPORT_CANCELLED_STATUS)
    })
    .await;

    fix.workspace.update(cx, |ws, _| {
        assert!(!ws.test_has_transfer_job());
        assert_eq!(
            ws.test_active_status().as_deref(),
            Some(EXPORT_CANCELLED_STATUS)
        );
        ws.test_clear_export_hold();
    });
}

/// Generation bump on `start_export` / `start_import` must advance the counter.
/// Removing either `transfer_generation` bump makes this fail.
#[gpui_kit::test]
async fn ex06_workspace_transfer_generation_bumps(cx: &mut TestAppContext) {
    let fix = open_fixture(cx).await;
    open_books_data(cx, fix.handle, &fix.workspace);

    let dest1 = fix.out_dir.join("g1.csv");
    cx.update_window(fix.handle, |_, window, cx| {
        fix.workspace.update(cx, |ws, cx| {
            assert_eq!(ws.test_transfer_generation(), 0);
            ws.test_start_export(dest1.clone(), cx);
            assert_eq!(ws.test_transfer_generation(), 1);
            assert_eq!(ws.test_transfer_job_generation(), Some(1));
            ws.cancel_transfer_if_active(cx);
        });
        window.render_frame(cx);
    })
    .unwrap();

    cx.wait_for(fix.handle, Duration::from_secs(8), |_, cx| {
        !fix.workspace.read(cx).test_has_transfer_job()
    })
    .await;

    let dest2 = fix.out_dir.join("g2.csv");
    let csv = fix.out_dir.join("tiny.csv");
    std::fs::write(&csv, "id,title\n1,A\n").unwrap();

    cx.update_window(fix.handle, |_, window, cx| {
        fix.workspace.update(cx, |ws, cx| {
            ws.test_start_export(dest2.clone(), cx);
            assert_eq!(
                ws.test_transfer_generation(),
                2,
                "second export must bump generation (start_export)"
            );
            assert_eq!(ws.test_transfer_job_generation(), Some(2));
            ws.cancel_transfer_if_active(cx);
        });
        window.render_frame(cx);
    })
    .unwrap();

    cx.wait_for(fix.handle, Duration::from_secs(8), |_, cx| {
        !fix.workspace.read(cx).test_has_transfer_job()
    })
    .await;

    cx.update_window(fix.handle, |_, window, cx| {
        fix.workspace.update(cx, |ws, cx| {
            ws.test_start_import(csv.clone(), "main", "books", cx);
            assert_eq!(
                ws.test_transfer_generation(),
                3,
                "import must bump generation (start_import)"
            );
            assert_eq!(ws.test_transfer_job_generation(), Some(3));
            ws.cancel_transfer_if_active(cx);
        });
        window.render_frame(cx);
    })
    .unwrap();

    cx.wait_for(fix.handle, Duration::from_secs(8), |_, cx| {
        !fix.workspace.read(cx).test_has_transfer_job()
            || fix
                .workspace
                .read(cx)
                .test_active_status()
                .as_deref()
                .is_some_and(|s| {
                    s.contains("cancelled") || s.contains("failed") || s.contains("Imported")
                })
    })
    .await;
}

/// Stale `finish_export` after the generation advances must not clear the newer job
/// or rewrite status as a successful export of the stale row count.
#[gpui_kit::test]
async fn ex06_workspace_stale_export_completion_ignored(cx: &mut TestAppContext) {
    let fix = open_fixture(cx).await;
    open_books_data(cx, fix.handle, &fix.workspace);

    let dest = fix.out_dir.join("stale.csv");
    cx.update_window(fix.handle, |_, window, cx| {
        fix.workspace.update(cx, |ws, cx| {
            ws.test_start_export(dest.clone(), cx);
            let gen = ws.test_transfer_job_generation().expect("job gen");
            assert!(gen >= 1, "start_export must bump generation above 0");
            ws.test_supersede_transfer_generation();
            let newer = ws.test_transfer_job_generation().unwrap();
            assert_eq!(newer, gen + 1);
            ws.test_finish_export(gen, dest.clone(), Ok(999), cx);
            assert!(
                ws.test_has_transfer_job(),
                "stale finish must leave the newer job"
            );
            assert_eq!(ws.test_transfer_job_generation(), Some(newer));
            let status = ws.test_active_status().unwrap_or_default();
            assert!(
                !status.contains("Exported 999"),
                "stale success must not rewrite status: {status}"
            );
            ws.cancel_transfer_if_active(cx);
        });
        window.render_frame(cx);
    })
    .unwrap();

    cx.run_until_parked();
}
