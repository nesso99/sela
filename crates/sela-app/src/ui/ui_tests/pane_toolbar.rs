//! Data / Structure toggle must not move when the Data-only controls hide (#158).

use std::path::PathBuf;
use std::time::Duration;

use gpui_kit::prelude::*;
use gpui_kit::test::{TestAppContextExt, TestWindowExt};
use gpui_kit::{px, size, Entity, TestAppContext};
use tempfile::TempDir;

use sela_drivers::create_sample_database;

use crate::ui::workspace::Workspace;

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

#[gpui_kit::test]
async fn pane_toggle_stays_put_between_data_and_structure(cx: &mut TestAppContext) {
    let fix = open_workspace_fixture(cx).await;
    cx.update_window(fix.handle, |_, window, cx| {
        fix.workspace
            .update(cx, |ws, cx| ws.test_open_table("main", "books", window, cx));
        window.render_frame(cx);
    })
    .expect("open books");
    cx.update_window(fix.handle, |_, window, cx| {
        let span = |window: &mut gpui_kit::Window| {
            let controls = window.find("filter-toggle").bounds();
            let toggle = window.find("pane-structure").bounds();
            // Bar cluster = left edge of the Data-only controls .. right edge of the toggle.
            (
                controls.origin.x,
                toggle.origin.x + toggle.size.width,
                toggle.origin.x,
            )
        };
        let (data_left, data_right, data_toggle_x) = span(window);
        window.click("pane-structure", cx);
        window.render_frame(cx);
        assert!(
            window.try_find("pane-structure-refresh").is_some(),
            "Structure shows Refresh"
        );
        let (left, right, toggle_x) = span(window);
        assert_eq!(left, data_left, "bar start must not move");
        assert_eq!(right, data_right, "bar width must be equal in both panes");
        assert_eq!(toggle_x, data_toggle_x, "toggle x must not move");
    })
    .expect("toggle");
}
