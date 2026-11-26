//! Running SQL must leave exactly the viewed tab active in the tab bar.

use std::path::PathBuf;
use std::time::Duration;

use gpui_kit::prelude::*;
use gpui_kit::test::{TestAppContextExt, TestWindowExt};
use gpui_kit::{px, size, Entity, TestAppContext};
use tempfile::TempDir;

use sela_drivers::create_sample_database;

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
async fn run_sql_from_table_tab_activates_new_query_tab(cx: &mut TestAppContext) {
    let (_dir, handle, workspace) = open_workspace(cx).await;
    cx.update_window(handle, |_, window, cx| {
        workspace.update(cx, |ws, cx| ws.test_open_table("main", "books", window, cx));
        window.render_frame(cx);
    })
    .unwrap();
    cx.wait_for(handle, Duration::from_secs(8), |_, cx| {
        workspace
            .read(cx)
            .test_active_view_page_len()
            .is_some_and(|n| n > 0)
    })
    .await;
    cx.update_window(handle, |_, window, cx| {
        workspace.update(cx, |ws, cx| {
            ws.test_set_sql("SELECT 1 AS one", window, cx);
            ws.test_run_editor(window, cx);
        });
        window.render_frame(cx);
    })
    .unwrap();
    cx.wait_for(handle, Duration::from_secs(8), |_, cx| {
        workspace.read(cx).test_active_view_busy() == Some(false)
    })
    .await;
    cx.update_window(handle, |_, _, cx| {
        let ws = workspace.read(cx);
        let titles = ws.test_tab_titles();
        let active = ws.test_active_tab_index().expect("active tab");
        let label = ws.test_active_source_label().unwrap();
        assert_eq!(label, "SQL result");
        assert_eq!(titles.len(), 2, "SQL runs in a new query tab: {titles:?}");
        assert_eq!(titles[0], "main.books");
        assert_eq!(
            active, 1,
            "the query tab must be the active one: {titles:?}"
        );
        assert_eq!(titles[active], "SELECT 1 AS one");
    })
    .unwrap();
}
