//! The GPUI window. Database work is started here and finished on the tokio runtime.

mod decorations;
mod form;
pub(crate) mod grid;
mod theme;
mod workspace;

use std::rc::Rc;

use gpui_kit::prelude::*;
use gpui_kit::{
    actions, px, size, Action, Bounds, Global, KeyBinding, KeyBindingContextPredicate, Menu,
    MenuItem, TitlebarOptions, WeakEntity, WindowBounds, WindowDecorations, WindowOptions,
};

pub use workspace::Workspace;

/// Weak handle so app-level Quit (menu / Cmd+Q with no focus path) reaches the workspace.
struct QuitWorkspace(WeakEntity<Workspace>);

impl Global for QuitWorkspace {}

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
        CopySelection,
        CopyAsTsv,
        CopyAsCsv,
        CopyAsJson,
        CopyAsInsert,
        SelectAllGrid,
        ExtendGridSelectionUp,
        ExtendGridSelectionDown,
        ExtendGridSelectionLeft,
        ExtendGridSelectionRight,
        ResultStripPrev,
        ResultStripNext,
        EditCell,
        CancelCellEditor,
        AcceptCellEditor,
        SetCellNull,
        InsertEditRow,
        DuplicateEditRow,
        DeleteEditRow,
        PreviewChanges,
        CommitChanges,
        DiscardChanges,
        Quit,
        IncreaseFontSize,
        DecreaseFontSize,
        ResetFontSize
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
            // Themes are installed once the window exists (needs appearance for System).
            // Until then stay on the kit dark default so the first frame is never light-flashy.
            cx.bind_keys([
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
            // Quit: ⌘Q on macOS, Ctrl+Q elsewhere. Global so it works in the
            // SQL editor and dialogs.
            bind_secondary(cx, "secondary-q", Box::new(Quit), None);
            // Font size (#39): global, including inside text fields.
            // Spec: secondary-= and secondary-+ (secondary-shift-=).
            bind_secondary(cx, "secondary-=", Box::new(IncreaseFontSize), None);
            bind_secondary(cx, "secondary-+", Box::new(IncreaseFontSize), None);
            bind_secondary(cx, "secondary-shift-=", Box::new(IncreaseFontSize), None);
            bind_secondary(cx, "secondary--", Box::new(DecreaseFontSize), None);
            bind_secondary(cx, "secondary-0", Box::new(ResetFontSize), None);
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
            // Grid copy (US-14). Scoped to DataTable so text fields keep clipboard chords.
            bind_secondary(
                cx,
                "secondary-c",
                Box::new(CopySelection),
                Some("DataTable"),
            );
            // Select all cells in the result grid (Ctrl/Cmd+A).
            cx.bind_keys([
                KeyBinding::new("cmd-a", SelectAllGrid, Some("DataTable")),
                KeyBinding::new("ctrl-a", SelectAllGrid, Some("DataTable")),
                // DataTable only binds plain arrows; Shift+arrow must be explicit
                // or the keystroke never moves the cell / never extends the range.
                KeyBinding::new("shift-up", ExtendGridSelectionUp, Some("DataTable")),
                KeyBinding::new("shift-down", ExtendGridSelectionDown, Some("DataTable")),
                KeyBinding::new("shift-left", ExtendGridSelectionLeft, Some("DataTable")),
                KeyBinding::new("shift-right", ExtendGridSelectionRight, Some("DataTable")),
            ]);
            // Result strip Prev/Next when the strip itself is focused.
            bind_secondary(cx, "left", Box::new(ResultStripPrev), Some("ResultStrip"));
            bind_secondary(cx, "right", Box::new(ResultStripNext), Some("ResultStrip"));
            // M2-1 / M2-3 grid editing (DataTable context; not SqlEditor).
            bind_secondary(cx, "enter", Box::new(EditCell), Some("DataTable"));
            bind_secondary(cx, "escape", Box::new(CancelCellEditor), Some("DataTable"));
            bind_secondary(cx, "escape", Box::new(CancelCellEditor), Some("CellEditor"));
            bind_secondary(cx, "enter", Box::new(AcceptCellEditor), Some("CellEditor"));
            bind_secondary(
                cx,
                "secondary-enter",
                Box::new(AcceptCellEditor),
                Some("CellEditor"),
            );
            bind_secondary(
                cx,
                "secondary-i",
                Box::new(InsertEditRow),
                Some("DataTable"),
            );
            bind_secondary(
                cx,
                "secondary-d",
                Box::new(DuplicateEditRow),
                Some("DataTable"),
            );
            bind_secondary(
                cx,
                "secondary-backspace",
                Box::new(DeleteEditRow),
                Some("DataTable"),
            );
            bind_secondary(
                cx,
                "secondary-shift-p",
                Box::new(PreviewChanges),
                Some("DataTable"),
            );
            bind_secondary(
                cx,
                "secondary-s",
                Box::new(CommitChanges),
                Some("DataTable"),
            );
            bind_secondary(
                cx,
                "secondary-shift-z",
                Box::new(DiscardChanges),
                Some("DataTable"),
            );
            install_menus(cx);
            // Route Quit through the workspace (confirm + clean shutdown). With
            // no open window (macOS after the last close), quit immediately.
            cx.on_action(|_: &Quit, cx| {
                let Some(workspace) = cx
                    .try_global::<QuitWorkspace>()
                    .and_then(|target| target.0.upgrade())
                else {
                    cx.quit();
                    return;
                };
                let Some(handle) = cx.windows().first().copied() else {
                    cx.quit();
                    return;
                };
                let _ = handle.update(cx, |_root, window, cx| {
                    workspace.update(cx, |workspace, cx| {
                        workspace.request_quit(window, cx);
                    });
                });
            });

            let bounds = Bounds::centered(None, size(px(1180.), px(760.)), cx);
            let (_handle, workspace) = gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some("Sela".into()),
                        ..Default::default()
                    }),
                    // Prefer server-side decorations so Windows, macOS, X11, and
                    // KDE keep the native title bar. On Linux, GNOME Wayland (and
                    // any compositor without SSD) refuses Server and GPUI falls
                    // back to client decorations — that is the Linux CSD path
                    // "as needed". Workspace renders TitleBar only when
                    // `window.window_decorations()` is `Decorations::Client`.
                    window_decorations: Some(WindowDecorations::Server),
                    window_min_size: Some(size(px(880.), px(560.))),
                    ..Default::default()
                },
                cx,
                |window, cx| cx.new(|cx| Workspace::new(window, cx)),
            )
            .expect("failed to open the Sela window");
            cx.set_global(QuitWorkspace(workspace.downgrade()));
        });
}

fn install_menus(cx: &mut gpui_kit::App) {
    #[cfg(target_os = "macos")]
    {
        // First top-level menu is the app menu. Quit Sela is last, after a separator.
        cx.set_menus([
            Menu::new("Sela").items([MenuItem::separator(), MenuItem::action("Quit Sela", Quit)])
        ]);
    }
    #[cfg(not(target_os = "macos"))]
    {
        // File ▸ Quit (last item, after a separator). Keymap supplies the Ctrl+Q hint.
        cx.set_menus([
            Menu::new("File").items([MenuItem::separator(), MenuItem::action("Quit", Quit)])
        ]);
        if let Some(menus) = cx.get_menus() {
            gpui_kit::component::GlobalState::global_mut(cx).set_app_menus(menus);
        }
    }
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
