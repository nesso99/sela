# UX specs — batch 2

This file is the index for the second UX batch. It is separate from [README.md](./README.md) on purpose: that index lands in [PR #18](https://github.com/nesso99/sela/pull/18) and is not on `main` yet. When #18 merges, fold this page into `README.md` (the doc table, the shortcut groups, and the focus-context rows below) and delete this file.

Product intent is in [PRD.md](../PRD.md), milestones in [roadmap.md](../roadmap.md), and the APIs in [SPECS.md](../SPECS.md). What the app binds today is [keyboard-shortcuts.md](../../user/keyboard-shortcuts.md).

These files specify M1-5 (query history), the connection dialog including the M1-8 TLS section, and M1-7 theme tokens. They describe the window that already ships and the changes those milestones should make. They are not a new visual system. Wireframes stay grayscale. Color lives only in [06-theme-tokens.md](./06-theme-tokens.md) and its swatch.

| Doc | Covers |
| --- | --- |
| [04-query-history.md](./04-query-history.md) | Bottom history panel. Row contents, search, per-connection scope, actions, empty and error states, retention, shortcuts. |
| [05-connection-dialog.md](./05-connection-dialog.md) | Postgres, MySQL, and SQLite fields, password display, Test connection, save and connect, TLS modes and certificate files. |
| [06-theme-tokens.md](./06-theme-tokens.md) | Light and dark tokens, contrast, type, spacing, chrome sizes, gpui-kit field mapping. |

Wireframes are low-fidelity grayscale PNGs in [images/](./images/). The matching `.html` files are the source. Numbers in the specs are the ones to implement; the drawings are schematic. The theme swatch is the exception: it is in color, because it is the palette.

## Modifier notation

Shortcuts in these specs are written `secondary-enter`, `secondary-shift-h`, and so on.

`secondary` is the platform command key:

| Platform | Key |
| --- | --- |
| macOS | Command (⌘) |
| Linux and Windows | Control |

`secondary-shift-h` is Command+Shift+H on macOS and Control+Shift+H on Linux and Windows. Plain `Enter`, `Tab`, `Escape`, and `Space` are written out and are not prefixed.

The shared rules for binding once, and the chords that must stay free, live in the batch 1 README. This batch adds no exception. In particular `secondary-h` stays free (Hide on macOS) and `secondary-y` stays free (Ctrl+Y is redo in text fields on Windows and Linux).

### Same chord, different focus

GPUI key contexts must differ. Do not rely on the action handler to guess. Rows already specified for the editor and the filter bar stay as they are.

| Chord | History list or its search field | Connection dialog | Elsewhere |
| --- | --- | --- | --- |
| `secondary-shift-h` | Close the panel | No action. The dialog eats the key. | Toggle the history panel |
| `secondary-enter` | Insert the selected SQL into the editor. Do not run it. | Save and connect | Editor: run the statement (batch 1) |
| `secondary-shift-enter` | Rerun the selected row | No action | Editor: run every statement (batch 1) |
| `secondary-f` | Focus the history search | No action | Editor find, or toggle the filter bar (batch 1) |
| `Enter` | Open the selected row in a new query tab. Do not run it. | Activate the focused button. In a text field, it does not insert a newline. | Editor: newline (batch 1) |
| `Escape` | Clear the search text if it is non-empty, otherwise close the panel | Cancel the dialog | Existing dismiss order (batch 1) |

## Shortcuts added in this batch

### Query history

| Action | Shortcut | Bound today |
| --- | --- | --- |
| Toggle history | `secondary-shift-h` | no |
| Focus history search | `secondary-f` while the panel is focused | no |
| Move the selection | `Up` / `Down`, including while the search field is focused | no |
| Open in a new query tab | `Enter` | no |
| Insert into the editor | `secondary-enter` | no |
| Copy the SQL | `secondary-c` when the list is focused | no |
| Rerun | `secondary-shift-enter` | no |
| Delete the row | `secondary-backspace` | no |

### Connection dialog

| Action | Shortcut | Bound today |
| --- | --- | --- |
| Save and connect | `secondary-enter` | no |
| Cancel | `Escape` | dialog dismiss already closes it |
| Move between fields | `Tab` / `Shift-Tab` | yes, as focus order |

Test connection has no shortcut. It is a plain button on the focus path.
