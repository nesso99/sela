//! Double-click column border auto-fit (#118).

use std::path::PathBuf;
use std::time::Duration;

use gpui_kit::prelude::*;
use gpui_kit::test::{TestAppContextExt, TestWindowExt};
use gpui_kit::{px, size, Entity, TestAppContext};
use tempfile::TempDir;

use sela_drivers::create_sample_database;

use crate::ui::grid::{COLUMN_AUTO_FIT_MAX_WIDTH, COLUMN_MIN_WIDTH};
use crate::ui::workspace::Workspace;

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

async fn open_workspace(
    cx: &mut TestAppContext,
) -> (TempDir, gpui_kit::AnyWindowHandle, Entity<Workspace>) {
    cx.executor().allow_parking();
    let dir = TempDir::new().expect("temp dir");
    let db_path = dir.path().join("sample.sqlite");
    seed_sample_db(&db_path);
    let dir_path: PathBuf = dir.path().to_path_buf();
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
    .expect("connect");
    cx.wait_for(handle, Duration::from_secs(8), |_, cx| {
        workspace.read(cx).test_is_connected() && workspace.read(cx).test_catalog_table_count() > 0
    })
    .await;
    (dir, handle, workspace)
}

#[gpui_kit::test]
async fn double_click_autofit_data_grid_matches_widest_visible_cell(cx: &mut TestAppContext) {
    let (_dir, handle, workspace) = open_workspace(cx).await;
    cx.update_window(handle, |_, window, cx| {
        workspace.update(cx, |ws, cx| ws.test_open_table("main", "books", window, cx));
        window.render_frame(cx);
    })
    .expect("open books");

    cx.wait_for(handle, Duration::from_secs(8), |_, cx| {
        workspace
            .read(cx)
            .test_active_view_page_len()
            .is_some_and(|n| n > 0)
    })
    .await;

    cx.update_window(handle, |_, window, cx| {
        let grid = workspace
            .read(cx)
            .test_active_grid()
            .expect("data grid");
        let title_ix = grid
            .read(cx)
            .delegate()
            .result
            .columns()
            .iter()
            .position(|c| c.name == "title")
            .expect("title column");
        let id_ix = grid
            .read(cx)
            .delegate()
            .result
            .columns()
            .iter()
            .position(|c| c.name == "id")
            .expect("id column");
        window.render_frame(cx);
        grid.update(cx, |state, cx| {
            state.refresh(cx);
            let before_id = f32::from(state.col_width(id_ix).expect("id width"));
            assert!(
                state.fit_col(title_ix, window, cx),
                "double-click fit must resize title"
            );
            let title_w = f32::from(state.col_width(title_ix).expect("title width"));
            let id_w = f32::from(state.col_width(id_ix).expect("id width"));
            assert!(
                title_w > before_id && title_w > id_w,
                "title auto-fit must be wider than id (before_id={before_id}, title={title_w}, id={id_w})"
            );
            assert!(
                title_w >= COLUMN_MIN_WIDTH && title_w <= COLUMN_AUTO_FIT_MAX_WIDTH + 1.0,
                "fit must respect min/max, got {title_w}"
            );
        });
    })
    .expect("data grid autofit");
}

#[gpui_kit::test]
async fn double_click_autofit_caps_very_long_values(cx: &mut TestAppContext) {
    let long_body = "W".repeat(400);
    let sql = format!("SELECT 'x' AS narrow, '{long_body}' AS huge");
    let (_dir, handle, workspace) = open_workspace(cx).await;
    cx.update_window(handle, |_, window, cx| {
        workspace.update(cx, |ws, cx| {
            ws.test_new_query_tab(window, cx);
            ws.test_set_sql(&sql, window, cx);
            ws.test_run_editor(window, cx);
        });
        window.render_frame(cx);
    })
    .expect("run long sql");

    cx.wait_for(handle, Duration::from_secs(8), |_, cx| {
        workspace.read(cx).test_active_view_busy() == Some(false)
            && workspace
                .read(cx)
                .test_active_view_page_len()
                .is_some_and(|n| n > 0)
    })
    .await;

    cx.update_window(handle, |_, window, cx| {
        let grid = workspace
            .read(cx)
            .test_active_grid()
            .expect("sql result grid");
        let huge_ix = grid
            .read(cx)
            .delegate()
            .result
            .columns()
            .iter()
            .position(|c| c.name == "huge")
            .expect("huge column");
        let narrow_ix = grid
            .read(cx)
            .delegate()
            .result
            .columns()
            .iter()
            .position(|c| c.name == "narrow")
            .expect("narrow column");
        window.render_frame(cx);
        grid.update(cx, |state, cx| {
            state.refresh(cx);
            assert!(state.fit_col(huge_ix, window, cx), "fit_col on huge column");
            let huge_w = f32::from(state.col_width(huge_ix).expect("huge width"));
            assert!(
                huge_w <= COLUMN_AUTO_FIT_MAX_WIDTH + 0.5,
                "long value must cap at max width, got {huge_w}"
            );

            // Narrow column still auto-fits below the cap.
            assert!(state.fit_col(narrow_ix, window, cx));
            let narrow_w = f32::from(state.col_width(narrow_ix).expect("narrow width"));
            assert!(
                narrow_w < huge_w,
                "narrow column must stay narrower than capped huge column"
            );
            assert!(narrow_w >= COLUMN_MIN_WIDTH);
        });
    })
    .expect("cap assertion");
}
