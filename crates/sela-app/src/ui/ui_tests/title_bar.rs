//! Client-side title bar (#147): no File menu; Quit via Ctrl+Q and close only.

#![cfg(not(target_os = "macos"))]

use gpui_kit::component::{h_flex, TitleBar};
use gpui_kit::prelude::*;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{div, px, size, Context, Render, TestAppContext, Window};

use crate::ui::workspace::Workspace;

/// Minimal host wrapping gpui-component `TitleBar` like production CSD chrome.
struct TitleBarHost;

impl Render for TitleBarHost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("title-bar-host")
            .size_full()
            .flex()
            .flex_col()
            .child(
                TitleBar::new().child(
                    h_flex()
                        .id("sela-csd-title")
                        .items_center()
                        .h_full()
                        .flex_1()
                        .min_w(px(0.))
                        .child(div().id("sela-csd-app-title").child("Sela")),
                ),
            )
    }
}

#[gpui_kit::test]
async fn csd_title_bar_renders_without_file_menu(cx: &mut TestAppContext) {
    let (handle, _) = super::harness::open_window(cx, size(px(800.), px(600.)), |_window, cx| {
        cx.new(|_cx| TitleBarHost)
    });
    let handle = handle.into();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find("file-menu").is_none(),
            "CSD title bar must not render a File menu trigger (#147)"
        );
        assert!(
            window.try_find("sela-menu-bar").is_none(),
            "SSD menu row must not appear alongside the CSD title bar"
        );
        let title_bar = window.find("title-bar");
        assert!(
            title_bar.bounds().size.height >= px(30.),
            "title bar should render at kit height; bounds={:?}",
            title_bar.bounds()
        );
    })
    .expect("CSD title bar assertions");
}

#[gpui_kit::test]
async fn workspace_root_has_no_file_menu_or_ssd_menu_row(cx: &mut TestAppContext) {
    let (handle, workspace) =
        super::harness::open_window(cx, size(px(800.), px(600.)), |window, cx| {
            cx.new(|cx| Workspace::new(window, cx))
        });
    let handle = handle.into();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find("file-menu").is_none(),
            "workspace must not render File menu (#147)"
        );
        assert!(
            window.try_find("sela-menu-bar").is_none(),
            "workspace must not render SSD menu row (#147)"
        );
        let _ = workspace.read(cx);
    })
    .expect("workspace title-bar assertions");
}
