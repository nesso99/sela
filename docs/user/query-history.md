# Query history

Statements you run from the SQL editor are stored in `history.sqlite` next to your [connections file](./installation.md#config-location). Open the history panel with the **History** button on the status bar, or **Cmd/Ctrl+Shift+H**.

## What is recorded

- One row per editor statement that finishes (success, error, or cancel).
- Multi-statement runs add one row for each statement that actually started.
- Catalog, Structure, paging, Count, filter SQL, and Load more are not recorded.
- Empty buffers are not recorded.
- Password-setting statements (for example `CREATE ROLE … PASSWORD`, `IDENTIFIED BY`, `SET PASSWORD`) are not recorded. The status bar notes `not saved to history (contains a password)`.

Recording is on by default. Turn it off per connection with **Record query history** in the connection dialog. Existing rows stay until you delete them or use **Clear history**.

## Panel

The panel sits under the SQL editor. Default height is 196px; drag the top edge (double-click resets). Scope is **This connection** (the active tab) or **All**. Filter by outcome and search SQL with FTS.

| Action | Shortcut |
| --- | --- |
| Toggle panel | Cmd/Ctrl+Shift+H |
| Focus search | Cmd/Ctrl+F (while the panel is focused) |
| Move selection | Up / Down (also from the search field) |
| Open in a new tab | Enter / double-click (does not run) |
| Insert into editor | Cmd/Ctrl+Enter |
| Copy statement | Cmd/Ctrl+C (list focused) |
| Rerun | Cmd/Ctrl+Shift+Enter |
| Delete row | Cmd/Ctrl+Backspace |
| Close | Escape (clears search first when non-empty) |

**Open** and **Insert** never auto-run. **Rerun** runs immediately only when the statement passes the same read-only classifier as Load more (`is_safe_to_rerun_for_load_more`); otherwise a confirm dialog shows the SQL (Cancel is the default button).

## Privacy

The file is not encrypted. On Unix its mode is `0600`. It stores SQL text, including ordinary string literals (for example values in a `WHERE` clause). Connection passwords from the OS keychain are not written into history. Statements that set a database role or user password are skipped rather than stored. See [FAQ](./faq.md#does-query-history-store-my-sql).

The store keeps at most about 10 000 rows globally; the oldest rows are removed after each insert.
