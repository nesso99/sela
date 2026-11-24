# Preview, commit, and discard

M2-3, US-06. The grid overlay and the `ChangeSet` are in [07-grid-editing.md](./07-grid-editing.md). Safe-mode confirms that wrap Commit are in [09-safe-mode.md](./09-safe-mode.md). The transaction shape is the one in [SPECS.md](../SPECS.md): one transaction, deletes, then updates, then inserts. Dialect SQL and the golden tests are M2-2. This file is the window around that SQL.

![Pending bar and the Preview SQL sheet](images/preview-commit.png)

## Pending bar

A 26px bar, shown only when the active table tab has at least one pending change. It sits under the filter bar (or under the toolbar when the filter bar is closed) and above the read-only strip and the error banner. The height comes out of the grid. It is not a card.

| Side | Contents |
| --- | --- |
| Left | `3 pending · 2 updated, 1 inserted`. Omit a count that is zero. Overlay rows count as inserted. |
| Right | `Preview`, `Discard`, `Commit`. Ghost, ghost, primary. `xsmall`. One primary. |

`Commit` is disabled when a required primary-key cell on a new row is still unset ([07-grid-editing.md](./07-grid-editing.md)), when safe mode is Read-only, or while the tab is `busy`. The tooltip says which. `Discard` and `Preview` stay enabled while changes exist. `Discard` is also disabled while `busy`.

The status bar does not repeat the count. On commit failure it replaces the tab status with the failure line below. After a successful commit the bar closes and the status line is the normal row count from the reload.

`secondary-s`, `secondary-shift-p`, and `secondary-shift-z` are no longer free. They stay out of every other feature.

## Preview sheet

`Preview` or `secondary-shift-p` opens a sheet from the bottom of the content column, above the status bar, height 240px. The grid shrinks. It is not a second window. `Escape` closes the sheet and does not discard.

Title: `Preview SQL · {schema.table}`. A `text_xs` line under it: `The database receives bound parameters. Values are listed beside each statement.`

The body is monospace, scrollable, one block per statement, in commit order (deletes, then updates, then inserts). Each block is the statement with placeholders and the bound values under it:

```text
UPDATE "public"."books" SET "title" = $1 WHERE "id" = $2
$1 = 'The Hobbit'
$2 = 3
```

NULL is an unquoted `NULL`. Strings are quoted and escaped for display only. The sheet never concatenates those literals into the SQL that runs. Placeholders follow the dialect (`$1` on Postgres, `?` on MySQL and SQLite). A comment line names the row: `-- update · id 3`.

Untouched cells on an insert are omitted so the database default applies. An explicit NULL is included. A new row with nothing set is still listed (M2-2 picks `DEFAULT VALUES` or the dialect equivalent). Generated columns are omitted.

The footer repeats `Discard` (ghost) and `Commit` (primary). They do the same thing as the bar. Closing the sheet leaves the `ChangeSet` in place.

`secondary-shift-p` is bound when the table Data pane or the pending bar is focused, including when this sheet is open (it leaves the sheet open). It does nothing in the SQL editor, the filter fields, and the connection dialog.

## Commit

`Commit` or `secondary-s` runs the pending statements in one transaction: `BEGIN`, deletes, updates, inserts, `COMMIT`. Identifiers are quoted. Values are bound. The `WHERE` is the key columns only.

`secondary-s` while the cell editor is open accepts the cell first, then commits, except when that accept fails validation: the editor stays open and nothing is sent. `secondary-s` in the SQL editor does nothing. It is not a document save.

When safe mode is Confirm writes, Commit opens the confirm in [09-safe-mode.md](./09-safe-mode.md) instead of sending. When it is Off, grid Commit sends immediately. Grid statements always have a key `WHERE`, so the no-WHERE warning does not apply to them.

While the transaction runs, `Commit` reads `Committing…` and is disabled. The grid stays as the user left it. There is no spinner over the cells.

Success: close the sheet, drop the `ChangeSet`, reload the current page from the server, status `Committed · 3 statements · 40 ms`. A reload failure after a successful commit uses the normal query error banner. The pending set is still dropped, because the transaction committed. Status: `Committed · the page failed to reload.`

`secondary-s` is registered once, with the platform key, on the table-Data and pending-bar contexts. Not on the SQL editor.

## Discard

`Discard` or `secondary-shift-z` (editor closed) opens a confirm. Same button rules as [Quit and window lifecycle](./01-main-window.md#quit-and-window-lifecycle): Cancel is the primary button, `Enter` and `Escape` cancel, the other button is not primary. Do not use the danger style from Clear history.

| Piece | Value |
| --- | --- |
| Title | `Discard pending changes?` |
| Body | `{N} edits on {schema.table} will be dropped. The database is not changed.` |
| Cancel | Primary. `Enter` and `Escape`. |
| Discard | Not primary. Click, or `Tab` then `Enter`. |

Confirming drops the `ChangeSet`, closes the sheet, and repaints the page from `ResultWindow`. Nothing is sent to the database.

`secondary-shift-z` inside a text field, including the cell editor and the SQL editor, is left to the field. On macOS that is redo. On Windows and Linux, redo stays `secondary-y`. Either way it does not discard.

## Commit failure

Any error rolls the transaction back. The `ChangeSet` stays. Dirty marks, overlay rows, and the pending bar stay. The grid is not reloaded from the server.

The existing error banner shows the `DbError` text, with SQLSTATE when there is one, capped and scrolling as in [01-main-window.md](./01-main-window.md). The pending bar adds one clause, `status.error.text`: `Commit failed. Changes kept.` Status: `Commit failed · statement 2 of 4 · changes kept`. When the driver does not say which statement, omit the statement clause.

A key that matched zero rows is a failure of the whole transaction, not a partial apply. Banner and status use the same failure pattern. The message includes `No row matched {schema.table} for this key.` when the engine reports zero affected rows for an update or a delete. The user fixes the pending cells, or discards, and tries again.

## Closing with pending edits

Pending edits are not typed SQL. The close-tab and quit confirms still use the quit button rules.

| Action | Title | Body |
| --- | --- | --- |
| `secondary-w` on a tab with pending edits | `Close {schema.table}?` | `{N} pending changes will be discarded. The database is not changed.` |
| Quit, pending edits, nothing busy | `Quit Sela?` | `{N} pending changes will be discarded.` |
| Quit, pending edits, and a busy query | `Quit Sela?` | Both sentences: the running-query sentence from [01-main-window.md](./01-main-window.md#quit-and-window-lifecycle), then the pending-changes sentence. |

Buttons are Cancel (primary, `Enter`, `Escape`) and `Close tab` or `Quit` (not primary). Cancel leaves the tab, the `ChangeSet`, and any running query alone.

Switching connections and Disconnect already confirm when a tab has typed SQL or a busy query ([01-main-window.md](./01-main-window.md)). Pending grid edits join that same confirm. Add the pending sentence to the body. Cancel still keeps the session.

A tab with pending edits shows a trailing `•` on the tab label, the same bullet as the dirty gutter, not only a color.

Until this milestone ships, quit still confirms only while a query is running. That shipped rule stays in the main-window spec.

## Keyboard

| Action | Shortcut | Focus |
| --- | --- | --- |
| Preview | `secondary-shift-p` | Table Data, pending bar, or the sheet |
| Commit | `secondary-s` | Table Data or the pending bar. Accepts an open cell editor first. |
| Discard | `secondary-shift-z` | Table Data or the pending bar, and not a text field |
| Close the sheet or the confirm | `Escape` | Sheet, then confirm. Does not discard by itself. |

## Implementation notes for engineering

- One `ChangeSet` per table tab. `secondary-s` sends that tab only.
- Do not reload the page on failure. A reload would paint server values under the overlay and look like a discard.
- Zero affected rows on an update or a delete fails the transaction. Do not commit the remaining statements.
- The preview formatter and the executor share one statement list. The sheet must not format a second, drifted copy.
- `secondary-s` and `secondary-shift-z` are not window-global. A global binding would steal redo and would commit while the user is typing SQL.

## Decisions

1. The pending count lives on the 26px bar, not a second time on the status bar.
2. The sheet shows placeholders plus bound values. Execution stays on the bound path.
3. `WHERE` is the key columns only. Old values of other columns are not part of the predicate. See [Open questions](#open-questions).
4. A failed commit keeps every pending edit and does not reload.
5. Discard, close-tab, and quit-with-edits reuse the quit dialog's button rules. Cancel is primary. The destructive button is not.

## Open questions

1. SPECS calls an optimistic `WHERE` on the old non-key values optional. This spec leaves it out of v0.2. Confirm that a concurrent edit to a column the user did not touch is not a conflict.
