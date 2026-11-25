# UX specs — Sela

Design specs for the desktop window. Product intent is in [PRD.md](../PRD.md), milestones in [roadmap.md](../roadmap.md), and the APIs in [SPECS.md](../SPECS.md). What is bound in the app today is listed in [keyboard-shortcuts.md](../../user/keyboard-shortcuts.md).

These files specify M1–M2 shipping UI and M3 connectivity/data. M3 is [10](./10-ssh-tunnel.md)–[12](./12-command-palette.md), plus autocomplete in [03](./03-sql-editor.md) and commit cancel / optimistic WHERE in [08](./08-preview-commit.md). QA maps criteria in [v0.3-test-plan.md](../../qa/v0.3-test-plan.md). They are not a new visual system.

| Doc | Covers |
| --- | --- |
| [01-main-window.md](./01-main-window.md) | Sidebar, tabs, grid, editor, status bar, quit and window lifecycle. Empty, running, 1,000-row cap, cancelled, query error, decode-error cells. |
| [02-filter-sort-review.md](./02-filter-sort-review.md) | Review of the planned filter bar and header sort against the current grid, status bar, and count button. Fixes, not a redesign. |
| [03-sql-editor.md](./03-sql-editor.md) | Editor/result split, run and cancel, **M3-2 autocomplete**, error placement, multiple result sets. |
| [04-query-history.md](./04-query-history.md) | Bottom history panel. Row contents, search, per-connection scope, actions, empty and error states, retention, shortcuts. |
| [05-connection-dialog.md](./05-connection-dialog.md) | Postgres, MySQL, and SQLite fields, password display, Test connection, save and connect, TLS, color/label/group/safe mode, import URL. SSH block → [10](./10-ssh-tunnel.md). |
| [06-theme-tokens.md](./06-theme-tokens.md) | Light and dark tokens, contrast, type, spacing, chrome sizes, gpui-kit field mapping. Font size (#39) and the edit and connection marks. |
| [07-grid-editing.md](./07-grid-editing.md) | Cell editors, NULL versus empty, inserted / duplicated / dirty / deleted rows, read-only without a key (M2-1, US-05). |
| [08-preview-commit.md](./08-preview-commit.md) | Pending-changes bar, Preview SQL, commit, discard; **#68 optimistic WHERE + cancel commit** (M2-3 / M3). |
| [09-safe-mode.md](./09-safe-mode.md) | Off, Confirm writes, Read-only, and the no-WHERE warning (M2-4, US-07). |
| [10-ssh-tunnel.md](./10-ssh-tunnel.md) | SSH in the connection dialog: auth, `~/.ssh/config`, known_hosts, errors (M3-1, US-02). |
| [11-export-import.md](./11-export-import.md) | Export CSV/JSON/SQL, CSV import, progress/cancel, safe mode (M3-3, US-12). |
| [12-command-palette.md](./12-command-palette.md) | Command palette + quick-open table, one surface two modes (M3-4). |

Wireframes are low-fidelity grayscale PNGs in [images/](./images/). The matching `.html` files are the source. Numbers in the specs are the ones to implement; the drawings are schematic. The theme swatch ([images/theme-swatches.png](./images/theme-swatches.png)) is the exception: it is in color, because it is the palette in [06-theme-tokens.md](./06-theme-tokens.md).

## Visual direction

Native, dense, keyboard-first. The reference is TablePlus and Zed, not a web dashboard.

- Compact rows, 1px dividers, monospace grid and editor.
- No cards, no large padding, no rounded hero panels, no centered spinner over the grid.
- Chrome heights stay in the 22–44px range. Do not grow the toolbar, tab bar, or status bar.
- Color comes from the GPUI theme. The light and dark values are in [06-theme-tokens.md](./06-theme-tokens.md). Wireframes stay grayscale.
- One primary button per local group (Run, Apply, New). Everything else is a plain or ghost button at the existing `xsmall` / `small` sizes.

## Modifier notation

Shortcuts in these specs are written `secondary-enter`, `secondary-shift-e`, and so on.

`secondary` is the platform command key:

| Platform | Key |
| --- | --- |
| macOS | Command (⌘) |
| Linux and Windows | Control |

`secondary-shift-enter` is Command+Shift+Enter on macOS and Control+Shift+Enter on Linux and Windows. Plain `Enter`, `Tab`, `Escape`, and `Space` are written out and are not prefixed.

`crates/sela-app/src/ui/mod.rs` currently registers each action twice (`cmd-enter` and `ctrl-enter`). On macOS that also binds Control+Enter. New actions should be registered **once**, as the platform key. Before deleting the extra `ctrl-` bindings, confirm in gpui-pre 0.3.12 that the `cmd` token is Command on macOS and Control on Linux and Windows. If the parser accepts a `secondary` token and that token is Control on Linux (not Super), `KeyBinding::new("secondary-enter", …)` is the binding to use. Do not add a third binding.

The user-facing shortcut page should keep saying “⌘ on macOS, Ctrl on Linux and Windows” and should be updated when a milestone lands these chords. This folder is the design target, not a second runtime list.

### Chords that must stay free

| Chord | Why |
| --- | --- |
| `secondary-space` | Spotlight on macOS (Command+Space) — stays free there. On Linux/Windows, `secondary-space` equals `ctrl-space` and is bound only to `ShowSqlCompletions` in `SqlEditor` ([03](./03-sql-editor.md), [12](./12-command-palette.md)). |
| `secondary-q` | Quit. Global, including the SQL editor and dialogs. Bound. See [01-main-window.md](./01-main-window.md#quit-and-window-lifecycle). |
| `secondary-w` | Close tab, already bound. It does not close the window. |
| `secondary-h`, `secondary-m` | Hide and minimize on macOS |
| `secondary-y` | Redo in text fields on Windows and Linux |
| `secondary-c` / `v` / `x` / `a` / `z` | Clipboard and undo inside text fields. (`secondary-v` is not Preview; Preview is `secondary-shift-v`.) |
| Option/Alt + letter | Option types characters on macOS. Do not bind those as global actions. |
| `secondary-,` | Reserved for a future settings window. Unassigned. |
| `secondary-shift-f` | Reserved for format (`sqlformat`). Backlog, not M1-4. Do not bind it yet. |

Assigned, and not available for a second feature: `secondary-p` (quick open tables), `secondary-shift-p` (command palette actions), `secondary-shift-v` (Preview SQL; was `secondary-shift-p` in M2), `secondary-s` (commit), `secondary-shift-z` (discard outside a text field; inside a text field it stays with the field, which is redo on macOS), `secondary-=` and `secondary-+` (larger font), `secondary--` (smaller), `secondary-0` (reset font, not a tab). The tables below name the focus.

`secondary-alt-left` and `secondary-alt-right` are used only when focus is not in a text field, so they do not fight word navigation.

### Same chord, different focus

GPUI key contexts must differ. Do not rely on the action handler to guess.

| Chord | Editor focused | Filter bar focused | History list or its search field | Connection dialog | Grid, tabs, or sidebar focused |
| --- | --- | --- | --- | --- | --- |
| `secondary-enter` | Run statement at caret, or the selection | Do not run. Apply the filter. | Insert the selected SQL. Do not run it. | Save and connect | Do not run. |
| `secondary-shift-enter` | Run every statement in the buffer | Add a filter row | Rerun. Read-only statements run immediately. Anything else confirms first. Cancel is the default button. | No action | No action |
| `secondary-shift-h` | Toggle the history panel | Toggle the history panel | Close the panel | No action. The dialog eats the key. | Toggle the history panel |
| `secondary-f` | Find in the editor | Focus the first column field | Focus the history search | No action | Toggle the filter bar |
| `Enter` | New line in the editor. Accepts autocomplete when the popup is open (Tab also accepts). | Apply the filter | Open the selected row in a new query tab. Do not run it. | Activate the focused button, except Import URL, which parses. A text field does not insert a newline. | Edit the focused cell ([07-grid-editing.md](./07-grid-editing.md)). `Space` stays inspect. |
| `Escape` | Close find, then close autocomplete | Close the open menu, then leave the bar | Clear the search text if it is non-empty, otherwise close the panel | Cancel the dialog | Close inspect, then cancel an open cell editor. Does not discard the pending set. |
| `secondary-s` | No action | No action | No action | No action | Commit pending edits. Accepts an open cell editor first. |
| `secondary-shift-z` | Left to the field (redo on macOS). Does not discard. | No action | No action | No action | Discard pending edits. Confirms. Cancel is the default. |
| `secondary-shift-p` | Open action palette | Open action palette | Open action palette | No action (dialog eats keys) | Open action palette |
| `secondary-p` | Quick open tables | Quick open tables | Quick open tables | No action | Quick open tables |
| `secondary-shift-v` | No action | No action | No action | No action | Preview SQL. |

`secondary-q` is not in this table. It quits from every column, including the SQL editor and the connection dialog. The quit confirm swallows a second `secondary-q`.

## Shortcuts by screen

### Anywhere in the window

| Action | Shortcut | Bound today |
| --- | --- | --- |
| Quit | `secondary-q` | yes. Global, including the SQL editor and dialogs. |
| New query tab | `secondary-t` | yes |
| Close tab | `secondary-w` | yes |
| Switch to tab 1–9 | `secondary-1` … `secondary-9` | yes |
| Previous / next tab | `secondary-alt-left` / `secondary-alt-right` | no |
| Cancel the running query | `secondary-.` | yes |
| Toggle sidebar | `secondary-b` | no |
| Toggle editor collapsed | `secondary-shift-e` | no |
| Larger / smaller / reset font | `secondary-=` or `secondary-+` / `secondary--` / `secondary-0` | no. [06-theme-tokens.md](./06-theme-tokens.md#font-size). |
| Quick open table | `secondary-p` | no. M3-4. [12-command-palette.md](./12-command-palette.md). |
| Command palette (actions) | `secondary-shift-p` | no. M3-4. |
| Show table Data | `secondary-shift-[` | yes |
| Show table Structure | `secondary-shift-]` | yes |

### Table data pane (filter and sort)

| Action | Shortcut |
| --- | --- |
| Toggle filter bar | `secondary-f` |
| Move between column, operator, value, remove | `Tab` / `Shift-Tab` |
| Apply filter | `Enter` |
| Add a filter row | `secondary-shift-enter` |
| Remove the focused filter row | `secondary-backspace` |
| Close menus, then return to the grid | `Escape` |
| Sort | Click a column header. `Shift`-click adds a key. See [02-filter-sort-review.md](./02-filter-sort-review.md). |
| Exact count | Click `Count` in the status bar. No shortcut. |

### SQL editor

| Action | Shortcut | Milestone |
| --- | --- | --- |
| Run statement at caret, or the selection if there is one | `secondary-enter` | M1-4 |
| Run every statement | `secondary-shift-enter` | M1-4 |
| Cancel | `secondary-.` or Stop | done |
| Find | `secondary-f` | M1-4 |
| Find next / previous | `secondary-g` / `secondary-shift-g` | M1-4 |
| Format | `secondary-shift-f` | Reserved. Backlog, not M1-4. |
| Accept autocomplete | `Enter` or `Tab` while the popup is open | M3-2 |
| Manual autocomplete | `ctrl-space` (editor focused) | M3-2 |
| Dismiss autocomplete or find | `Escape` | M1-4 / M3-2 |

### Grid

| Action | Shortcut | Until |
| --- | --- | --- |
| Move the focused cell | Arrow keys, once a cell is focused | M1-7 owns selection |
| Inspect the focused cell | `Space` | Stays inspect after M2. |
| Edit the focused cell | `Enter` or double-click | M2-1. [07-grid-editing.md](./07-grid-editing.md). |
| Insert / duplicate / delete row | `secondary-i` / `secondary-d` / `secondary-backspace` | M2-1. Grid focused, editor closed. |
| Preview / commit / discard | `secondary-shift-v` / `secondary-s` / `secondary-shift-z` | M2-3 / M3. [08-preview-commit.md](./08-preview-commit.md). |
| Cancel in-flight commit | `secondary-.` / Stop while `Committing…` | M3 (#68). |
| Copy | `secondary-c` | M1-7. Today `cell_text` already returns the full value. |

### Query history

| Action | Shortcut | Bound today |
| --- | --- | --- |
| Toggle history | `secondary-shift-h` | no |
| Focus history search | `secondary-f` while the panel is focused | no |
| Move the selection | `Up` / `Down`, including while the search field is focused | no |
| Open in a new query tab | `Enter` | no |
| Insert into the editor | `secondary-enter` | no |
| Copy the SQL | `secondary-c` when the list is focused | no |
| Rerun | `secondary-shift-enter`. Read-only statements run immediately. Anything else confirms first; Cancel is the default button. | no |
| Delete the row | `secondary-backspace` | no |
| Clear history | The header button. No shortcut. | no |

### Connection dialog

| Action | Shortcut | Bound today |
| --- | --- | --- |
| Save and connect | `secondary-enter` | no |
| Cancel | `Escape` | dialog dismiss already closes it |
| Move between fields | `Tab` / `Shift-Tab` | yes, as focus order |

Test connection has no shortcut. It is a plain button on the focus path. Theme choice (Light, Dark, System) is the status menu in [06-theme-tokens.md](./06-theme-tokens.md). It has no shortcut. `secondary-,` stays reserved for a future settings window.

## Implementation notes for engineering

- Bind new shortcuts with a GPUI focus context. `RunQuery` is global today (`KeyBinding::new(..., None)` in `ui/mod.rs`), so `secondary-enter` runs SQL from anywhere in the window. Scope it to the editor.
- Keep one binding per action. See the modifier section above before editing the `cmd-` / `ctrl-` pairs.
- Do not invent components the kit already has. The window uses `gpui-component` `DataTable`, `TabBar`, `Tree`, `Button`, and `Textarea` from gpui-kit 0.10. Specs name those.
- Wireframe PNGs are generated from the HTML files beside them. They are documentation, not fixtures.

## Decisions

Recorded from review. The screen specs repeat the items that change their behavior.

1. Do not persist sidebar width or editor height yet. Later, an optional config field with a serde default and no migration. The history panel height is session-only as well and is persisted in that same later change. See [01-main-window.md](./01-main-window.md) and [04-query-history.md](./04-query-history.md).
2. Keep the error banner (full message) and the editor gutter. The gutter is drawn only when the engine reports a position. See [03-sql-editor.md](./03-sql-editor.md).
3. M1-9 is done (#13). This batch does not edit `roadmap.md` for that. Engineering updates the status rows in the M1-3 pull request.
4. M1-3 filters are AND-only. OR and groups come later. See [02-filter-sort-review.md](./02-filter-sort-review.md).
5. Header sort is for table Data only.
6. `=` with an empty text value matches a real `''`. Use `IS NULL` for nulls. The empty value field shows that hint.
7. Count has no confirmation. It must be cancellable and must show progress in the status bar (`Counting… · 1.2 s`).
8. Autocomplete stays in M3-2, out of M1-4. The popup spec stays marked M3-2.
9. Run-all stops at the first error, names the failing statement, and keeps earlier results.
10. Cap stored result sets at 8 and evict the oldest.
11. No `sqlformat` in M1-4. Format is backlog. `secondary-shift-f` is reserved and is not an M1-4 binding. The shortcut tables mark it reserved.
12. Grid editing, preview, and safe mode are [07](./07-grid-editing.md), [08](./08-preview-commit.md), and [09](./09-safe-mode.md). Preview chord is `secondary-shift-v` as of M3; `secondary-shift-p` is the action palette.
13. Font size is one UI step in [06](./06-theme-tokens.md#font-size). The chrome heights in that file stay fixed.
14. Linux client decorations use gpui-component `TitleBar` at 34px ([01](./01-main-window.md#client-side-title-bar)), the bar #66 builds. The content toolbar does not also draw a close button.
15. **M3 SSH** ([10](./10-ssh-tunnel.md)): hidden for SQLite; first-seen host key Accept & save; changed host key hard error; no ProxyJump; Import URL does not set SSH.
16. **M3 autocomplete** ([03](./03-sql-editor.md)): auto after `.` or 2 identifier chars; `ctrl-space` manual; Tab/Enter accept; catalog refresh on connect / Refresh schema / successful DDL / schema switch.
17. **M3 export/import** ([11](./11-export-import.md)): context menu + palette; import existing table only; safe-mode copy as specified.
18. **M3 palette** ([12](./12-command-palette.md)): one surface; `secondary-p` tables; `secondary-shift-p` actions; `>` switches to actions; dimmed unavailable rows with reason.
19. **#68** ([08](./08-preview-commit.md)): optimistic WHERE always on; cancel copy `Commit cancelled. No changes were saved.`; conflict copy with Refresh.
