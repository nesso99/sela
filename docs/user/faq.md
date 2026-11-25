# FAQ

## Does query history store my SQL?

Yes. Statements you run are stored in `history.sqlite` next to the connections file, including ordinary string literals inside the SQL. The file is not encrypted. On Unix its mode is `0600`. Connection passwords stay in the OS keychain and are not copied into history. Statements that set a database user or role password (for example `PASSWORD '…'`, `IDENTIFIED BY`, MySQL `IDENTIFIED WITH <plugin> BY`, or `SET PASSWORD`) are not recorded; the status bar says so. Clear history in the history panel removes the stored statements for the current scope, or all of them when the scope is All.

## Where are my passwords stored?

In the OS keychain when available — never in `connections.json` or `settings.json`. See [Connecting](./connecting.md).

## How do I copy cells from the grid?

Select a cell, Shift-click / Shift+arrow a range, or Cmd/Ctrl+A for all cells, then Cmd/Ctrl+C for TSV, or use the grid context menu for CSV, JSON, or INSERT. Unloaded rows and decode-error cells for INSERT are refused with a status message. See [Copying](./copying.md).

## How do I switch light and dark themes?

Use the theme menu on the status bar (Light, Dark, or System). The choice is saved in `settings.json`. See [Themes](./themes.md).

## Why does Linux ask for my password again after restart?

No unlocked Secret Service was available when you saved the connection. Sela kept the password in memory only for that session and warned in the connection dialog and status bar (*The OS keychain is unavailable, so the password is kept for this session only*). Unlock gnome-keyring / KWallet (or another Secret Service) and save the connection again so the password is stored in the keychain (service `app.sela.database`). A working Secret Service is required for the password to survive quit; the in-session memory fallback never writes to disk.

## Why can’t I keep tabs after Disconnect?

Disconnect closes that connection’s tabs on purpose so a later connect cannot restore or show tables from the wrong database. If a tab has typed SQL or a query is still running, Sela asks first (*Close N tabs for …?*); Cancel keeps the session and tabs as they were. Save any SQL you need before confirming, or leave the connection connected.

## Can I cancel a long query?

Yes: **Stop** or Cmd/Ctrl+.. Postgres and MySQL cancel on the server via a second connection; SQLite only stops consuming the local stream. See [ADR 0003](../adr/0003-cancel-second-connection.md).

## What is Safe mode?

A per-connection setting (Off / Confirm writes / Read-only) that gates writes from the SQL editor and warns on `UPDATE` / `DELETE` without `WHERE`. See [Safe mode](./safe-mode.md).

## Will there be SSH / export / a command palette?

Yes — specified for v0.3 (M3). See [UX specs](../design/ux/README.md) and the [v0.3 test plan](../qa/v0.3-test-plan.md). Not all of those are in the binary yet.

## Can I edit cells in the grid?

Yes on a table Data tab that has a primary key: Enter or double-click a cell, then Commit from the pending bar (or Cmd/Ctrl+S). Tables without a primary key, views, and SQL result tabs stay read-only. Preview SQL and Discard are on the pending bar. Safe mode gates editor writes and grid Commit — see [Safe mode](./safe-mode.md), [grid editing](../design/ux/07-grid-editing.md), and [preview / commit](../design/ux/08-preview-commit.md).

## How do I quit?

Cmd/Ctrl+Q, or **Quit Sela** (macOS app menu) / File ▸ **Quit** (Linux/Windows). If a query or Count is still running, or if any tab has pending grid edits, Sela asks before quitting. On Linux and Windows, closing the last window also quits; on macOS the app stays open until you Quit.

## Why did Load more say the result is capped?

Arbitrary SQL streams up to 1 000 rows. Load more re-runs only when the statement is classified as safe read-only. Otherwise re-run the query yourself. See [ADR 0004](../adr/0004-safe-load-more.md).

## Do restored tabs re-run SQL?

No. Query-tab SQL is restored into the editor and never auto-run. Table tabs reload only after the connection is available.

## Is Structure editable?

No. Structure is read-only (columns, indexes, foreign keys, DDL). Editing DDL from the UI is post-v1.

## What license is Sela under?

[GPL-3.0-or-later](../../LICENSE).
