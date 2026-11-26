//! Column resize widths must survive sort, selection, and grid refresh (#137).
//!
//! Exercises the real `Workspace` table subscription in `tabs.rs` (not a duplicate
//! handler).

use std::path::PathBuf;
use std::time::Duration;

use gpui_kit::component::table::{ColumnSort, TableDelegate, TableEvent, TableState};
use gpui_kit::prelude::*;
use gpui_kit::test::{TestAppContextExt, TestWindowExt};
use gpui_kit::{px, size, Entity, Pixels, TestAppContext};
use tempfile::TempDir;

use sela_drivers::create_sample_database;

use crate::ui::grid::GridDelegate;
use crate::ui::workspace::Workspace;

const TITLE_COL_WIDTH: f32 = 286.;

struct WorkspaceFixture {
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

async fn open_workspace_fixture(cx: &mut TestAppContext) -> WorkspaceFixture {
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
    WorkspaceFixture {
        _dir: dir,
        handle,
        workspace,
    }
}

/// Emit `ColumnWidthsChanged` on the workspace grid so `tabs.rs` must persist widths.
fn emit_title_resize_via_table_event(
    grid: &Entity<TableState<GridDelegate>>,
    title_ix: usize,
    cx: &mut gpui_kit::App,
) {
    grid.update(cx, |state, cx| {
        let mut widths: Vec<Pixels> = (0..state.delegate().columns_count(cx))
            .map(|ix| state.col_width(ix).expect("initial col width"))
            .collect();
        widths[title_ix] = px(TITLE_COL_WIDTH);
        cx.emit(TableEvent::ColumnWidthsChanged(widths));
    });
}

#[gpui_kit::test]
async fn workspace_column_width_survives_resize_sort_and_refresh(cx: &mut TestAppContext) {
    let fix = open_workspace_fixture(cx).await;
    cx.update_window(fix.handle, |_, window, cx| {
        fix.workspace.update(cx, |ws, cx| {
            ws.test_open_table("main", "books", window, cx);
        });
        window.render_frame(cx);
    })
    .expect("open books");

    cx.wait_for(fix.handle, Duration::from_secs(8), |_, cx| {
        fix.workspace
            .read(cx)
            .test_active_view_page_len()
            .is_some_and(|n| n > 0)
    })
    .await;

    cx.update_window(fix.handle, |_, window, cx| {
        let grid = fix
            .workspace
            .read(cx)
            .test_active_grid()
            .expect("grid on table tab");
        let title_ix = grid
            .read(cx)
            .delegate()
            .result
            .columns()
            .iter()
            .position(|c| c.name == "title")
            .expect("title before resize");
        emit_title_resize_via_table_event(&grid, title_ix, cx);
        window.render_frame(cx);
    })
    .expect("emit ColumnWidthsChanged");

    cx.update_window(fix.handle, |_, window, cx| {
        let grid = fix
            .workspace
            .read(cx)
            .test_active_grid()
            .expect("grid on table tab");
        let title_ix = grid
            .read(cx)
            .delegate()
            .result
            .columns()
            .iter()
            .position(|c| c.name == "title")
            .expect("title before refresh");
        assert!(
            grid.read(cx)
                .delegate()
                .test_cached_column_width("title")
                .is_some_and(|w| (w - TITLE_COL_WIDTH).abs() < 0.5),
            "tabs.rs ColumnWidthsChanged handler must cache title width before refresh"
        );
        window.render_frame(cx);
        grid.update(cx, |state, cx| {
            state.refresh(cx);
            let w = f32::from(state.col_width(title_ix).expect("title width after resize"));
            assert!(
                (w - TITLE_COL_WIDTH).abs() < 0.5,
                "refresh after resize must keep user width, got {w}"
            );
        });

        // Sort path: delegate records pending sort; workspace observer runs apply_column_sort.
        grid.update(cx, |state, cx| {
            state
                .delegate_mut()
                .perform_sort(title_ix, ColumnSort::Ascending, window, cx);
        });
        window.render_frame(cx);
    })
    .expect("resize assert and sort request");

    cx.wait_for(fix.handle, Duration::from_secs(8), |_, cx| {
        !fix.workspace
            .read(cx)
            .test_active_view_busy()
            .unwrap_or(true)
    })
    .await;

    cx.update_window(fix.handle, |_, window, cx| {
        let grid = fix
            .workspace
            .read(cx)
            .test_active_grid()
            .expect("grid after sort");
        let title_ix = grid
            .read(cx)
            .delegate()
            .result
            .columns()
            .iter()
            .position(|c| c.name == "title")
            .expect("title after sort");
        window.render_frame(cx);
        grid.update(cx, |state, cx| {
            state.refresh(cx);
            let w = f32::from(state.col_width(title_ix).expect("title width after sort"));
            assert!(
                (w - TITLE_COL_WIDTH).abs() < 0.5,
                "reload after sort must keep user width, got {w}"
            );
        });

        // Selection path: tabs.rs SelectCell handler calls refresh.
        fix.workspace
            .update(cx, |ws, cx| ws.test_focus_grid(0, title_ix, window, cx));
        window.render_frame(cx);
        grid.update(cx, |state, _| {
            let w = f32::from(state.col_width(title_ix).expect("title width after select"));
            assert!(
                (w - TITLE_COL_WIDTH).abs() < 0.5,
                "refresh after selection must keep user width, got {w}"
            );
        });
    })
    .expect("assertions after sort and select");
}
