//! Double-click column border auto-fit (#118).

use std::path::PathBuf;
use std::time::Duration;

use gpui_kit::component::table::TableDelegate;
use gpui_kit::component::{ActiveTheme, Size};
use gpui_kit::prelude::*;
use gpui_kit::test::{TestAppContextExt, TestWindowExt};
use gpui_kit::{
    px, size, App, Entity, Font, Pixels, SharedString, TestAppContext, TextRun, Window,
};
use tempfile::TempDir;

use sela_drivers::create_sample_database;

use crate::ui::grid::{COLUMN_AUTO_FIT_MAX_WIDTH, COLUMN_MIN_WIDTH};
use crate::ui::workspace::Workspace;

fn seed_sample_db_with(path: &std::path::Path, extra_sql: Option<&str>) {
    let path = path.to_path_buf();
    let extra = extra_sql.map(str::to_owned);
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("seed runtime")
            .block_on(create_sample_database(&path))
            .expect("sample sqlite");
        if let Some(sql) = extra {
            let status = std::process::Command::new("sqlite3")
                .arg(&path)
                .arg(&sql)
                .status()
                .expect("spawn sqlite3");
            assert!(status.success(), "sqlite3 extra seed failed: {status}");
        }
    })
    .join()
    .expect("seed thread");
}

async fn open_workspace(
    cx: &mut TestAppContext,
) -> (TempDir, gpui_kit::AnyWindowHandle, Entity<Workspace>) {
    open_workspace_seeded(cx, None).await
}

async fn open_workspace_seeded(
    cx: &mut TestAppContext,
    extra_sql: Option<&str>,
) -> (TempDir, gpui_kit::AnyWindowHandle, Entity<Workspace>) {
    cx.executor().allow_parking();
    let dir = TempDir::new().expect("temp dir");
    let db_path = dir.path().join("sample.sqlite");
    seed_sample_db_with(&db_path, extra_sql);
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

fn table_body_font_size(window: &Window) -> Pixels {
    px(f32::from(window.rem_size()) * 0.875)
}

fn measure_text_width(
    text: &str,
    font_family: &SharedString,
    font_size: Pixels,
    window: &mut Window,
    cx: &App,
) -> Pixels {
    if text.is_empty() {
        return px(0.);
    }
    let font = Font {
        family: font_family.clone(),
        ..Default::default()
    };
    window
        .text_system()
        .shape_line(
            text.into(),
            font_size,
            &[TextRun {
                len: text.len(),
                font,
                color: cx.theme().foreground,
                background_color: None,
                underline: None,
                strikethrough: None,
            }],
            None,
        )
        .width
}

/// Mirror `TableState::fit_col` device-pixel rounding of content + horizontal padding.
fn fitted_width_from_content(
    content: Pixels,
    pad_left: Pixels,
    pad_right: Pixels,
    scale_factor: f32,
) -> f32 {
    let device_pixels = |length: Pixels| (f32::from(length) * scale_factor).ceil();
    let mut content_pixels = device_pixels(content);
    if px(content_pixels / scale_factor) < content {
        content_pixels += 1.;
    }
    (content_pixels + device_pixels(pad_left) + device_pixels(pad_right)) / scale_factor
}

/// Workspace results use `Size::XSmall` at the default font size (`content.rs`).
fn expected_table_cell_padding() -> gpui_kit::Edges<Pixels> {
    Size::XSmall.table_cell_padding()
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

        let font_size = table_body_font_size(window);
        let mono = cx.theme().mono_font_family.clone();
        let sans = cx.theme().font_family.clone();
        let rem = window.rem_size();
        // `.px_2()` on body cells: 0.5rem each side.
        let cell_inset = rem * 1.0;
        let sort_icon = rem * 0.75 + px(4.);
        let pad = expected_table_cell_padding();
        let scale = window.scale_factor();

        // Widest sample title (seed-sqlite.sql).
        let widest_title = "If on a winter's night a traveler";
        let cell_content =
            measure_text_width(widest_title, &mono, font_size, window, cx) + cell_inset;
        let header_content =
            measure_text_width("title", &sans, font_size, window, cx) + sort_icon;
        let expected_content = cell_content.max(header_content);
        let expected_w =
            fitted_width_from_content(expected_content, pad.left, pad.right, scale)
                .clamp(COLUMN_MIN_WIDTH, COLUMN_AUTO_FIT_MAX_WIDTH);
        // Regression baseline: missing px_2 leaves the column ~1rem short.
        let without_px2 = fitted_width_from_content(
            measure_text_width(widest_title, &mono, font_size, window, cx)
                .max(measure_text_width("title", &sans, font_size, window, cx) + sort_icon),
            pad.left,
            pad.right,
            scale,
        )
        .clamp(COLUMN_MIN_WIDTH, COLUMN_AUTO_FIT_MAX_WIDTH);

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
                (COLUMN_MIN_WIDTH..=COLUMN_AUTO_FIT_MAX_WIDTH + 1.0).contains(&title_w),
                "fit must respect min/max, got {title_w}"
            );
            let slack = 1.0 / scale.max(1.0);
            assert!(
                (title_w - expected_w).abs() <= slack,
                "title fit must include cell px_2 inset + table padding + sort icon on header \
                 (got {title_w}, expected {expected_w}, cell_content={}, header_content={})",
                f32::from(cell_content),
                f32::from(header_content),
            );
            assert!(
                title_w > without_px2 + f32::from(rem) * 0.5,
                "fitted width must reflect px_2 (got {title_w}, without_px2={without_px2})"
            );
        });
    })
    .expect("data grid autofit");
}

#[gpui_kit::test]
async fn double_click_autofit_sortable_header_adds_sort_icon(cx: &mut TestAppContext) {
    // Long header, short cells — header (+ sort icon) must drive the fit.
    let (_dir, handle, workspace) = open_workspace_seeded(
        cx,
        Some(
            "CREATE TABLE autofit_hdr (very_long_sortable_header_name_abcdef INTEGER);\
             INSERT INTO autofit_hdr VALUES (1), (2);",
        ),
    )
    .await;

    cx.update_window(handle, |_, window, cx| {
        workspace.update(cx, |ws, cx| {
            ws.test_open_table("main", "autofit_hdr", window, cx);
        });
        window.render_frame(cx);
    })
    .expect("open autofit_hdr");

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
        let col_ix = grid
            .read(cx)
            .delegate()
            .result
            .columns()
            .iter()
            .position(|c| c.name == "very_long_sortable_header_name_abcdef")
            .expect("long header column");
        window.render_frame(cx);

        let font_size = table_body_font_size(window);
        let mono = cx.theme().mono_font_family.clone();
        let sans = cx.theme().font_family.clone();
        let rem = window.rem_size();
        let cell_inset = rem * 1.0;
        let sort_icon = rem * 0.75 + px(4.);
        // `Column::name` includes type suffix (e.g. "…   INTEGER"), matching fit_column.
        let header_label = grid.read(cx).delegate().column(col_ix, cx).name.to_string();
        let header_text = measure_text_width(&header_label, &sans, font_size, window, cx);
        let header_with_icon = header_text + sort_icon;
        let cell_with_inset =
            measure_text_width("1", &mono, font_size, window, cx) + cell_inset;
        assert!(
            f32::from(header_with_icon) > f32::from(cell_with_inset),
            "test setup: header+icon must beat cell+px_2 (header={header_label:?})"
        );

        let pad = expected_table_cell_padding();
        let scale = window.scale_factor();
        let expected_add = fitted_width_from_content(header_with_icon, pad.left, pad.right, scale)
            .clamp(COLUMN_MIN_WIDTH, COLUMN_AUTO_FIT_MAX_WIDTH);
        let expected_max_only = fitted_width_from_content(
            header_text.max(sort_icon),
            pad.left,
            pad.right,
            scale,
        )
        .clamp(COLUMN_MIN_WIDTH, COLUMN_AUTO_FIT_MAX_WIDTH);

        grid.update(cx, |state, cx| {
            state.refresh(cx);
            assert!(
                state.delegate().sortable,
                "data grid columns must be sortable so the header shows a sort icon"
            );
            assert!(state.fit_col(col_ix, window, cx), "fit_col on header column");
            let got = f32::from(state.col_width(col_ix).expect("width"));
            let slack = 1.0 / scale.max(1.0);
            assert!(
                (got - expected_add).abs() <= slack,
                "sortable header fit must ADD sort icon width (got {got}, expected {expected_add}, header={header_label:?})"
            );
            assert!(
                got > expected_max_only + f32::from(sort_icon) * 0.5,
                "max(header, icon) under-fits: got {got}, max_only={expected_max_only}, icon={}",
                f32::from(sort_icon)
            );
        });
    })
    .expect("header icon autofit");
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
