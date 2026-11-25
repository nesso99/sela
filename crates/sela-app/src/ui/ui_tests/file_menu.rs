//! File menu layout (#101): SSD menu row trigger is content-sized / left-aligned
//! and the dropdown opens under File (not at the window top-left).

#![cfg(not(target_os = "macos"))]

use gpui_kit::component::{h_flex, ActiveTheme, TitleBar};
use gpui_kit::prelude::*;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{div, px, size, App, Context, Entity, TestAppContext, Window};

use crate::ui::workspace::Workspace;

/// Host that draws the production SSD menu row or the CSD title-bar File row.
struct FileMenuHost {
    csd: bool,
}

impl FileMenuHost {
    fn new(csd: bool) -> Self {
        Self { csd }
    }
}

impl Render for FileMenuHost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Sidebar title sits under the menu row in the real window; include it so
        // a top-left menu would visibly overlap this "Sela" label as in QA.
        let body = h_flex().id("sela-body").w_full().flex_1().child(
            div()
                .id("sidebar-title")
                .w(px(292.))
                .h(px(40.))
                .child("Sela"),
        );

        if self.csd {
            div()
                .id("file-menu-host")
                .size_full()
                .flex()
                .flex_col()
                .child(
                    // Same leading File + title row as Workspace::client_title_bar,
                    // wrapped in TitleBar so CSD chrome matches production.
                    TitleBar::new().child(
                        h_flex()
                            .id("sela-csd-title")
                            .items_center()
                            .gap_2()
                            .h_full()
                            .flex_1()
                            .min_w(px(0.))
                            .child(Workspace::file_menu_button())
                            .child(div().id("sela-csd-app-title").child("Sela")),
                    ),
                )
                .child(body)
        } else {
            div()
                .id("file-menu-host")
                .size_full()
                .flex()
                .flex_col()
                .bg(cx.theme().background)
                .child(Workspace::menu_bar_row(cx))
                .child(body)
        }
    }
}

async fn open_host(
    cx: &mut TestAppContext,
    csd: bool,
) -> (gpui_kit::AnyWindowHandle, Entity<FileMenuHost>) {
    // Wide enough that a full-row File trigger would centre around x≈640 (QA).
    let (handle, host) =
        super::harness::open_window(cx, size(px(1280.), px(720.)), |_window, cx| {
            cx.new(|_cx| FileMenuHost::new(csd))
        });
    let handle = handle.into();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
    })
    .expect("render file-menu host");
    (handle, host)
}

/// Button already registers `.test_support()`, so `file-menu` is findable.
/// Assert content-sized / left-aligned trigger and menu origin under it.
fn assert_file_trigger_and_menu(window: &mut Window, cx: &mut App) {
    let trigger = window.find("file-menu");
    let trigger_bounds = trigger.bounds();
    let window_width = px(1280.);

    // Regression for the block-row bug: ghost Button stretched to full width
    // (~1280) so "File" looked centred near x≈640 and TopLeft put the menu at 0.
    assert!(
        trigger_bounds.size.width < window_width * 0.25,
        "File trigger should be content-sized, not full row width; width={:?}",
        trigger_bounds.size.width
    );
    assert!(
        trigger_bounds.origin.x < px(80.),
        "File trigger should be left-aligned; origin.x={:?}",
        trigger_bounds.origin.x
    );

    window.click("file-menu", cx);
    let menu = window.find("popup-menu");
    let menu_origin = menu.bounds().origin;

    assert!(
        (menu_origin.x - trigger_bounds.origin.x).abs() < px(8.)
            || (menu_origin.x >= trigger_bounds.origin.x
                && menu_origin.x <= trigger_bounds.origin.x + trigger_bounds.size.width),
        "menu origin.x should sit at/under the File trigger; menu={:?} trigger={:?}",
        menu_origin,
        trigger_bounds
    );
    assert!(
        menu_origin.y >= trigger_bounds.origin.y,
        "menu should open at or below the File trigger; menu.y={:?} trigger={:?}",
        menu_origin.y,
        trigger_bounds
    );
}

#[gpui_kit::test]
async fn ssd_file_menu_trigger_is_content_sized_and_menu_opens_under_it(cx: &mut TestAppContext) {
    let (handle, _) = open_host(cx, false).await;
    cx.update_window(handle, |_, window, cx| {
        assert_file_trigger_and_menu(window, cx);
    })
    .expect("SSD file menu assertions");
}

#[gpui_kit::test]
async fn csd_file_menu_trigger_is_content_sized_and_menu_opens_under_it(cx: &mut TestAppContext) {
    let (handle, _) = open_host(cx, true).await;
    cx.update_window(handle, |_, window, cx| {
        assert_file_trigger_and_menu(window, cx);
    })
    .expect("CSD file menu assertions");
}
