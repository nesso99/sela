//! Shared window helpers for headless UI tests.

use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    base::Root, point, px, App, Bounds, ElementId, Entity, Pixels, Point, Render, ScrollDelta,
    Size, TestAppContext, Window, WindowBounds, WindowHandle, WindowOptions,
};

use crate::ui::install_sela_keybindings;

/// Open a kit Root window with deterministic bounds and install Sela keymaps.
pub(super) fn open_window<V: Render>(
    cx: &mut TestAppContext,
    size: Size<Pixels>,
    build: impl FnOnce(&mut Window, &mut App) -> Entity<V>,
) -> (WindowHandle<Root>, Entity<V>) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        install_sela_keybindings(cx);
        cx.set_reduce_motion(true);
        let bounds = Bounds {
            origin: Point::default(),
            size,
        };
        let (window, content) = gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            cx,
            build,
        )
        .expect("open UI test window");
        (window.downcast::<Root>().expect("Base Root"), content)
    })
}

/// Scroll the connection dialog body until `id` is visible, then click it.
///
/// The form is taller than the dialog viewport (TLS fields + footer), so
/// Cancel / Save / password sit below the fold until scrolled.
pub(super) fn click_in_dialog(window: &mut Window, id: impl Into<ElementId>, cx: &mut App) {
    let id = id.into();
    for _ in 0..24 {
        window.render_frame(cx);
        if window
            .within("dialog")
            .try_find(id.clone())
            .is_some_and(|snap| snap.visible())
        {
            window.within("dialog").click(id, cx);
            return;
        }
        // Negative Y scrolls content upward (reveal lower controls).
        window.within("dialog").scroll(
            "surface",
            ScrollDelta::Pixels(point(px(0.), px(-160.))),
            cx,
        );
    }
    panic!("ElementId {id:?} never became visible inside dialog after scrolling");
}
