# FAQ

## Does query history store my SQL?

Yes. Statements you run are stored in `history.sqlite` next to the connections file, including ordinary string literals inside the SQL. The file is not encrypted. On Unix its mode is `0600`. Connection passwords stay in the OS keychain and are not copied into history. Statements that set a database user or role password (for example `PASSWORD '…'`, `IDENTIFIED BY`, or `SET PASSWORD`) are not recorded; the status bar says so. Clear history in the history panel removes the stored statements for the current scope, or all of them when the scope is All.

## Where are my passwords stored?

In the OS keychain when available — never in `connections.json` or `settings.json`. See [Connecting](./connecting.md).

## How do I copy cells from the grid?

Select a cell or Shift-click a range, then Cmd/Ctrl+C for TSV, or use the grid context menu for CSV, JSON, or INSERT. Unloaded rows and decode-error cells for INSERT are refused with a status message. See [Copying](./copying.md).

## How do I switch light and dark themes?

Use the theme menu on the status bar (Light, Dark, or System). The choice is saved in `settings.json`. See [Themes](./themes.md).

## Why does Linux ask for my password again after restart?

No unlocked Secret Service was available when you saved the connection. Sela kept the password in memory only. Unlock gnome-keyring / KWallet (or another Secret Service) and save the connection again.

## Can I cancel a long query?

Yes: **Stop** or Cmd/Ctrl+.. Postgres and MySQL cancel on the server via a second connection; SQLite only stops consuming the local stream. See [ADR 0003](../adr/0003-cancel-second-connection.md).

## Why did Load more say the result is capped?

Arbitrary SQL streams up to 1 000 rows. Load more re-runs only when the statement is classified as safe read-only. Otherwise re-run the query yourself. See [ADR 0004](../adr/0004-safe-load-more.md).

## Do restored tabs re-run SQL?

No. Query-tab SQL is restored into the editor and never auto-run. Table tabs reload only after the connection is available.

## Is Structure editable?

No. Structure is read-only (columns, indexes, foreign keys, DDL). Editing DDL from the UI is post-v1.

## What license is Sela under?

[GPL-3.0-or-later](../../LICENSE).
