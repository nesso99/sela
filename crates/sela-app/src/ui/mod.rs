//! The GPUI window. Database work is started here and finished on the tokio runtime.

mod form;
mod grid;
mod workspace;

use std::rc::Rc;

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::prelude::*;
use gpui_kit::{
    actions, px, size, Action, Bounds, KeyBinding, KeyBindingContextPredicate, TitlebarOptions,
    WindowBounds, WindowOptions,
};

pub use workspace::Workspace;

actions!(
    sela,
    [
        RunQuery,
        CancelQuery,
        NewQueryTab,
        CloseTab,
        ShowTableData,
        ShowTableStructure,
        ToggleFilterBar,
        InsertFilterRow,
        RemoveFilterRow,
        EscapeFilterBar,
        RunAllSql,
        FindInSql,
        FindNext,
        FindPrevious,
        ToggleEditor,
        ToggleHistory,
        FocusHistorySearch,
        HistorySelectUp,
        HistorySelectDown,
        HistoryOpen,
        HistoryInsert,
        HistoryCopy,
        HistoryRerun,
        HistoryDelete,
        EscapeHistory,
        Quit
    ]
);

/// Activate tab N where N is 1..=9 (Cmd/Ctrl+N).
#[derive(Clone, PartialEq, Eq, Action)]
#[action(namespace = sela, no_json)]
pub struct ActivateTab(pub usize);

pub fn run() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(|cx| {
            gpui_kit::init(cx);
            Theme::change(ThemeMode::Dark, None, cx);
            cx.bind_keys([
                KeyBinding::new("cmd-q", Quit, None),
                KeyBinding::new("cmd-.", CancelQuery, None),
                KeyBinding::new("ctrl-.", CancelQuery, None),
                KeyBinding::new("cmd-t", NewQueryTab, None),
                KeyBinding::new("ctrl-t", NewQueryTab, None),
                KeyBinding::new("cmd-w", CloseTab, None),
                KeyBinding::new("ctrl-w", CloseTab, None),
                KeyBinding::new("cmd-1", ActivateTab(1), None),
                KeyBinding::new("ctrl-1", ActivateTab(1), None),
                KeyBinding::new("cmd-2", ActivateTab(2), None),
                KeyBinding::new("ctrl-2", ActivateTab(2), None),
                KeyBinding::new("cmd-3", ActivateTab(3), None),
                KeyBinding::new("ctrl-3", ActivateTab(3), None),
                KeyBinding::new("cmd-4", ActivateTab(4), None),
                KeyBinding::new("ctrl-4", ActivateTab(4), None),
                KeyBinding::new("cmd-5", ActivateTab(5), None),
                KeyBinding::new("ctrl-5", ActivateTab(5), None),
                KeyBinding::new("cmd-6", ActivateTab(6), None),
                KeyBinding::new("ctrl-6", ActivateTab(6), None),
                KeyBinding::new("cmd-7", ActivateTab(7), None),
                KeyBinding::new("ctrl-7", ActivateTab(7), None),
                KeyBinding::new("cmd-8", ActivateTab(8), None),
                KeyBinding::new("ctrl-8", ActivateTab(8), None),
                KeyBinding::new("cmd-9", ActivateTab(9), None),
                KeyBinding::new("ctrl-9", ActivateTab(9), None),
                // Data ↔ Structure for the open table tab (US-11 / M1-6).
                KeyBinding::new("cmd-shift-[", ShowTableData, None),
                KeyBinding::new("ctrl-shift-[", ShowTableData, None),
                KeyBinding::new("cmd-shift-]", ShowTableStructure, None),
                KeyBinding::new("ctrl-shift-]", ShowTableStructure, None),
            ]);
            // Platform secondary modifier (⌘ on macOS, Ctrl elsewhere) — M1-3 filter bar.
            bind_secondary(cx, "secondary-f", Box::new(ToggleFilterBar), None);
            bind_secondary(cx, "secondary-enter", Box::new(RunQuery), Some("SqlEditor"));
            bind_secondary(
                cx,
                "secondary-shift-enter",
                Box::new(RunAllSql),
                Some("SqlEditor"),
            );
            bind_secondary(cx, "secondary-f", Box::new(FindInSql), Some("SqlEditor"));
            bind_secondary(cx, "secondary-g", Box::new(FindNext), Some("SqlEditor"));
            bind_secondary(
                cx,
                "secondary-shift-g",
                Box::new(FindPrevious),
                Some("SqlEditor"),
            );
            bind_secondary(
                cx,
                "secondary-shift-e",
                Box::new(ToggleEditor),
                Some("SqlEditor"),
            );
            // Filter-bar-only chords (UX §10). `FilterBar > Input` overrides the
            // Input widget's cmd/ctrl-backspace so the row is removed instead.
            bind_secondary(
                cx,
                "secondary-shift-enter",
                Box::new(InsertFilterRow),
                Some("FilterBar"),
            );
            bind_secondary(
                cx,
                "secondary-backspace",
                Box::new(RemoveFilterRow),
                Some("FilterBar"),
            );
            bind_secondary(
                cx,
                "secondary-backspace",
                Box::new(RemoveFilterRow),
                Some("FilterBar > Input"),
            );
            bind_secondary(cx, "escape", Box::new(EscapeFilterBar), Some("FilterBar"));
            // Query history panel (M1-5). `secondary-h` stays free (Hide on macOS).
            bind_secondary(cx, "secondary-shift-h", Box::new(ToggleHistory), None);
            bind_secondary(
                cx,
                "secondary-f",
                Box::new(FocusHistorySearch),
                Some("HistoryPanel"),
            );
            bind_secondary(cx, "up", Box::new(HistorySelectUp), Some("HistoryPanel"));
            bind_secondary(
                cx,
                "down",
                Box::new(HistorySelectDown),
                Some("HistoryPanel"),
            );
            bind_secondary(
                cx,
                "up",
                Box::new(HistorySelectUp),
                Some("HistoryPanel > Input"),
            );
            bind_secondary(
                cx,
                "down",
                Box::new(HistorySelectDown),
                Some("HistoryPanel > Input"),
            );
            bind_secondary(cx, "enter", Box::new(HistoryOpen), Some("HistoryPanel"));
            bind_secondary(
                cx,
                "secondary-enter",
                Box::new(HistoryInsert),
                Some("HistoryPanel"),
            );
            bind_secondary(
                cx,
                "secondary-c",
                Box::new(HistoryCopy),
                Some("HistoryPanel"),
            );
            bind_secondary(
                cx,
                "secondary-shift-enter",
                Box::new(HistoryRerun),
                Some("HistoryPanel"),
            );
            bind_secondary(
                cx,
                "secondary-backspace",
                Box::new(HistoryDelete),
                Some("HistoryPanel"),
            );
            bind_secondary(cx, "escape", Box::new(EscapeHistory), Some("HistoryPanel"));
            bind_secondary(
                cx,
                "escape",
                Box::new(EscapeHistory),
                Some("HistoryPanel > Input"),
            );
            cx.on_action(|_: &Quit, cx| cx.quit());

            let bounds = Bounds::centered(None, size(px(1180.), px(760.)), cx);
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some("Sela".into()),
                        ..Default::default()
                    }),
                    window_min_size: Some(size(px(880.), px(560.))),
                    ..Default::default()
                },
                cx,
                |window, cx| cx.new(|cx| Workspace::new(window, cx)),
            )
            .expect("failed to open the Sela window");
        });
}

fn bind_secondary(
    cx: &mut gpui_kit::App,
    keystrokes: &str,
    action: Box<dyn Action>,
    context: Option<&str>,
) {
    let predicate =
        context.and_then(|ctx| KeyBindingContextPredicate::parse(ctx).ok().map(Rc::new));
    if let Ok(binding) = KeyBinding::load(keystrokes, action, predicate, false, None, cx) {
        cx.bind_keys([binding]);
    }
}
