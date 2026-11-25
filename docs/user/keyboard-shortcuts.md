# Keyboard shortcuts

Bindings are registered in `crates/sela-app/src/ui/mod.rs`. On macOS use **⌘**; on Linux/Windows use **Ctrl** for the same actions.

Newer shortcuts use GPUI’s `secondary-` modifier (⌘ on macOS, Ctrl elsewhere) as a single binding. Older shortcuts still register both `cmd-` and `ctrl-`.

The design target for shortcuts that are not built yet is [docs/design/ux/README.md](../design/ux/README.md). That folder writes the platform key as `secondary`. `secondary-shift-f` is reserved for format and is backlog. M3 targets: `secondary-p` quick open, `secondary-shift-p` command palette (Preview SQL moves to `secondary-shift-v`), editor `ctrl-space` for autocomplete. This page stays the list of what the app binds today.

Font size (Cmd/Ctrl+= / − / 0) and grid edit / preview / commit chords below are bound.

| Action | Shortcut |
| --- | --- |
| Run statement at caret or selection | Cmd/Ctrl+Enter (SQL editor focused) |
| Run all statements | Cmd/Ctrl+Shift+Enter (SQL editor focused) |
| Find in SQL | Cmd/Ctrl+F (SQL editor focused; table tabs use Filter) |
| Find next / previous | Cmd/Ctrl+G / Cmd/Ctrl+Shift+G (SQL editor focused) |
| Collapse SQL editor | Cmd/Ctrl+Shift+E (SQL editor focused) |
| Cancel query | Cmd/Ctrl+. |
| New query tab | Cmd/Ctrl+T |
| Close tab | Cmd/Ctrl+W |
| Switch to tab 1–9 | Cmd/Ctrl+1 … Cmd/Ctrl+9 |
| Show table Data | Cmd/Ctrl+Shift+[ |
| Show table Structure | Cmd/Ctrl+Shift+] |
| Toggle filter bar (table tabs, focus outside SQL editor) | Cmd/Ctrl+F |
| Add filter row (filter bar focused) | Cmd/Ctrl+Shift+Enter |
| Remove filter row (empty field / row focus) | Cmd/Ctrl+Backspace |
| Leave filter bar | Escape |
| Toggle query history | Cmd/Ctrl+Shift+H |
| Focus history search | Cmd/Ctrl+F (history panel focused) |
| History selection | Up / Down (history panel, including search) |
| Open history row in new tab | Enter (history panel) |
| Insert history SQL into editor | Cmd/Ctrl+Enter (history panel) |
| Copy history SQL | Cmd/Ctrl+C (history list focused) |
| Rerun history row | Cmd/Ctrl+Shift+Enter (history panel) |
| Delete history row | Cmd/Ctrl+Backspace (history list) |
| Close history / clear search | Escape (history panel) |
| Select all cells | Cmd/Ctrl+A (grid focused) |
| Extend selection | Shift+Arrow (grid focused) |
| Copy selection as TSV | Cmd/Ctrl+C (grid focused) |
| Edit focused cell | Enter or double-click (grid focused; table with PK) |
| Inspect focused cell | Space (grid focused; any cell, including read-only). Inside the cell editor, Space types a space. |
| Cancel open cell editor / close inspect | Escape (does not discard the pending set) |
| Insert / Duplicate / Delete row | Cmd/Ctrl+I / Cmd/Ctrl+D / Cmd/Ctrl+Backspace (grid focused) |
| Preview pending SQL | Cmd/Ctrl+Shift+P (table Data; M3 design target moves Preview to Cmd/Ctrl+Shift+V so Shift+P can open the action palette) |
| Commit pending changes | Cmd/Ctrl+S (table Data; accepts open editor first) |
| Discard pending changes | Cmd/Ctrl+Shift+Z (table Data, editor closed; confirms) |
| Previous / next SQL result | Left / Right (result strip focused) |
| Larger / smaller / reset font size | Cmd/Ctrl+= or Cmd/Ctrl+Shift+= (also +) / Cmd/Ctrl+- / Cmd/Ctrl+0 (global) |
| Quit | Cmd/Ctrl+Q (File ▸ Quit / Quit Sela; confirms while a query is running or pending edits exist) |

Run (Cmd/Ctrl+Enter) runs the statement at the caret, the selected text split into statements, or the whole buffer when using Run all. It only fires when the SQL editor has focus; filter fields still use Enter / Cmd/Ctrl+Enter to apply. Cancel stops the running query for that tab (server-side cancel on Postgres/MySQL; SQLite drops the stream). Closing a tab cancels its in-flight work. Global Cmd/Ctrl+F toggles the filter bar on table tabs; with the SQL editor focused, the same chord opens find-in-SQL; with the history panel focused, it focuses the history search field.

In the filter bar: **Enter** Applies; **Cmd/Ctrl+Shift+Enter** inserts a row after the focused row; **Cmd/Ctrl+Backspace** removes the focused row when focus is on the row itself or an empty column/value field (clears the sole row in place). When a column or value field is non-empty, the same chord deletes to the start of the line instead (macOS ⌘⌫). **Escape** closes an open value menu first, otherwise moves focus to the grid and hides the bar when the draft is empty and nothing is applied.

Query history details: [query-history.md](./query-history.md).

With the grid focused, **Cmd/Ctrl+C** copies the current cell or range as TSV. Right-click the grid for CSV, JSON, or INSERT. See [Copying](./copying.md). With the history list focused, the same chord copies the selected history SQL.

The SQL result strip (between the grid and the editor after a multi-statement run) is a tab stop. With the strip focused, **Left** / **Right** move Prev / Next. Theme (Light / Dark / System) and font size are status-bar menus; font size also has Cmd/Ctrl+= / − / 0. See [Themes](./themes.md).

The chords above are the ones registered in `ui/mod.rs`. `secondary` in [docs/design/ux/README.md](../design/ux/README.md) means ⌘ on macOS and Ctrl on Linux and Windows. Quit is `secondary-q` (global, including the SQL editor and dialogs): [Quit and window lifecycle](../design/ux/01-main-window.md#quit-and-window-lifecycle).
