# Keyboard shortcuts

Bindings are registered in `crates/sela-app/src/ui/mod.rs`. On macOS use **⌘**; on Linux/Windows use **Ctrl** for the same actions.

Newer shortcuts use GPUI’s `secondary-` modifier (⌘ on macOS, Ctrl elsewhere) as a single binding. Older shortcuts still register both `cmd-` and `ctrl-`.

The design target for shortcuts that are not built yet (editor split, find, history) is [docs/design/ux/README.md](../design/ux/README.md). That folder writes the platform key as `secondary`. `secondary-shift-f` is reserved for format and is backlog, not M1-4. This page stays the list of what the app binds today.

| Action | Shortcut |
| --- | --- |
| Run query | Cmd/Ctrl+Enter |
| Cancel query | Cmd/Ctrl+. |
| New query tab | Cmd/Ctrl+T |
| Close tab | Cmd/Ctrl+W |
| Switch to tab 1–9 | Cmd/Ctrl+1 … Cmd/Ctrl+9 |
| Show table Data | Cmd/Ctrl+Shift+[ |
| Show table Structure | Cmd/Ctrl+Shift+] |
| Toggle filter bar (table tabs) | Cmd/Ctrl+F |
| Add filter row (filter bar focused) | Cmd/Ctrl+Shift+Enter |
| Remove filter row (empty field / row focus) | Cmd/Ctrl+Backspace |
| Leave filter bar | Escape |
| Quit | Cmd+Q |

Run applies to the active query tab (or Applies the filter bar when a filter field is focused). Cancel stops the running query for that tab (server-side cancel on Postgres/MySQL; SQLite drops the stream). Closing a tab cancels its in-flight work. Filter (Cmd/Ctrl+F) only affects table Data tabs.

In the filter bar: **Enter** Applies; **Cmd/Ctrl+Shift+Enter** inserts a row after the focused row; **Cmd/Ctrl+Backspace** removes the focused row when focus is on the row itself or an empty column/value field (clears the sole row in place). When a column or value field is non-empty, the same chord deletes to the start of the line instead (macOS ⌘⌫). **Escape** closes an open value menu first, otherwise moves focus to the grid and hides the bar when the draft is empty and nothing is applied.

The chords above are the ones registered in `ui/mod.rs`. Query history, the connection dialog, and the theme menu are specified and not bound yet. They are in the same design index: [docs/design/ux/README.md](../design/ux/README.md). `secondary` in that folder means ⌘ on macOS and Ctrl on Linux and Windows.
