//! The GPUI window. Database work is started here and finished on the tokio runtime.

mod decorations;
mod file_picker;
mod form;
pub(crate) mod grid;
pub(crate) mod long_value_popover;
mod palette_list;
mod theme;
mod workspace;

#[cfg(test)]
mod ui_tests;

use std::rc::Rc;

use gpui_kit::prelude::*;
use gpui_kit::{
    actions, px, size, Action, Bounds, Global, KeyBinding, KeyBindingContextPredicate, Menu,
    MenuItem, NoAction, TitlebarOptions, WeakEntity, WindowBounds, WindowDecorations,
    WindowOptions,
};

pub use workspace::Workspace;

/// Key context on the result grid (`DataTable`). Space → inspect.
pub(crate) const DATA_TABLE_KEY_CONTEXT: &str = "DataTable";
/// Key context wrapping the in-cell editor Input. Space → character, not inspect.
pub(crate) const CELL_EDITOR_KEY_CONTEXT: &str = "CellEditor";

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
        InspectCell,
        CancelCellEditor,
        AcceptCellEditor,
        SetCellNull,
        InsertEditRow,
        DuplicateEditRow,
        DeleteEditRow,
        PreviewChanges,
        CommitChanges,
        DiscardChanges,
        ExportData,
        ImportCsv,
        ToggleQuickOpen,
        ToggleCommandPalette,
        EscapePalette,
        PaletteSelectUp,
        PaletteSelectDown,
        PaletteConfirm,
        Quit,
        IncreaseFontSize,
        DecreaseFontSize,
        ResetFontSize,
        /// Manual SQL autocomplete (ctrl-space in the editor; AU-05).
        ShowSqlCompletions,
        /// Tab: accept completion when open, else indent (AU-13).
        AcceptCompletionOrIndent
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
            install_sela_keybindings(cx);
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
                    // Wayland app_id / X11 class: must match packaging/linux/sela.desktop
                    // so the shell shows the installed icon.
                    app_id: Some("sela".into()),
                    ..Default::default()
                },
                cx,
                |window, cx| cx.new(|cx| Workspace::new(window, cx)),
            )
            .expect("failed to open the Sela window");
            cx.set_global(QuitWorkspace(workspace.downgrade()));
        });
}

/// Keymap used by the app window and by headless UI tests.
pub(crate) fn install_sela_keybindings(cx: &mut gpui_kit::App) {
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
    // M3-2 autocomplete: Control+Space (not secondary) in the SQL editor (AU-05).
    // secondary-space stays free for Spotlight / OS.
    cx.bind_keys([KeyBinding::new(
        "ctrl-space",
        ShowSqlCompletions,
        Some("SqlEditor"),
    )]);
    // Tab accepts when the completion popup is open; otherwise indents (AU-13).
    // Bound on the editor input context so it wins over kit IndentInline.
    cx.bind_keys([KeyBinding::new(
        "tab",
        AcceptCompletionOrIndent,
        Some("Input && mode == editor"),
    )]);
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
    // Space inspects; Enter edits (01-main-window.md / 07-grid-editing.md).
    // CellEditor NoAction lets Space reach the Input as a character on
    // Linux, where key bindings win over character input (#78).
    cx.bind_keys([
        KeyBinding::new("space", InspectCell, Some(DATA_TABLE_KEY_CONTEXT)),
        KeyBinding::new("space", NoAction, Some(CELL_EDITOR_KEY_CONTEXT)),
    ]);
    bind_secondary(cx, "escape", Box::new(CancelCellEditor), Some("DataTable"));
    bind_secondary(cx, "escape", Box::new(CancelCellEditor), Some("CellEditor"));
    bind_secondary(cx, "enter", Box::new(AcceptCellEditor), Some("CellEditor"));
    bind_secondary(
        cx,
        "secondary-enter",
        Box::new(AcceptCellEditor),
        Some("CellEditor"),
    );
    // Long-value popover: Enter inserts a newline; Ctrl/Cmd+Enter accepts.
    bind_secondary(
        cx,
        "escape",
        Box::new(CancelCellEditor),
        Some(long_value_popover::LONG_VALUE_KEY_CONTEXT),
    );
    bind_secondary(
        cx,
        "secondary-enter",
        Box::new(AcceptCellEditor),
        Some(long_value_popover::LONG_VALUE_KEY_CONTEXT),
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
        "secondary-shift-v",
        Box::new(PreviewChanges),
        Some("DataTable"),
    );
    // Leave paste (Ctrl/Cmd+Shift+V on some platforms) to text inputs.
    bind_secondary(
        cx,
        "secondary-shift-v",
        Box::new(NoAction),
        Some(CELL_EDITOR_KEY_CONTEXT),
    );
    bind_secondary(cx, "secondary-shift-v", Box::new(NoAction), Some("Input"));
    bind_secondary(
        cx,
        "secondary-shift-v",
        Box::new(NoAction),
        Some("SqlEditor"),
    );
    bind_secondary(
        cx,
        "secondary-shift-v",
        Box::new(NoAction),
        Some("FilterBar"),
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
    // M3-4 command palette / quick open (global; dialogs eat keys via focus).
    bind_secondary(cx, "secondary-p", Box::new(ToggleQuickOpen), None);
    bind_secondary(
        cx,
        "secondary-shift-p",
        Box::new(ToggleCommandPalette),
        None,
    );
    bind_secondary(
        cx,
        "escape",
        Box::new(EscapePalette),
        Some(workspace::palette::PALETTE_KEY_CONTEXT),
    );
    bind_secondary(
        cx,
        "up",
        Box::new(PaletteSelectUp),
        Some(workspace::palette::PALETTE_KEY_CONTEXT),
    );
    bind_secondary(
        cx,
        "down",
        Box::new(PaletteSelectDown),
        Some(workspace::palette::PALETTE_KEY_CONTEXT),
    );
    bind_secondary(
        cx,
        "enter",
        Box::new(PaletteConfirm),
        Some(workspace::palette::PALETTE_KEY_CONTEXT),
    );
    bind_secondary(
        cx,
        "up",
        Box::new(PaletteSelectUp),
        Some("CommandPalette > Input"),
    );
    bind_secondary(
        cx,
        "down",
        Box::new(PaletteSelectDown),
        Some("CommandPalette > Input"),
    );
    bind_secondary(
        cx,
        "enter",
        Box::new(PaletteConfirm),
        Some("CommandPalette > Input"),
    );
    bind_secondary(
        cx,
        "escape",
        Box::new(EscapePalette),
        Some("CommandPalette > Input"),
    );
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
        // File ▸ Quit is not drawn in-window (#147); keep the GPUI menu
        // registration so the Quit action and Ctrl+Q hint stay wired.
        cx.set_menus([
            Menu::new("File").items([MenuItem::separator(), MenuItem::action("Quit", Quit)])
        ]);
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

#[cfg(test)]
mod tests {
    use super::{CELL_EDITOR_KEY_CONTEXT, DATA_TABLE_KEY_CONTEXT};

    #[test]
    fn space_inspect_contexts_match_grid_and_editor() {
        // DataTable Space → InspectCell; CellEditor Space → NoAction so the
        // Input receives the character (Linux binding-before-input, #78).
        assert_eq!(DATA_TABLE_KEY_CONTEXT, "DataTable");
        assert_eq!(CELL_EDITOR_KEY_CONTEXT, "CellEditor");
    }
}
