# Copying grid data

Select cells in the results grid, then copy them to the clipboard. This covers US-14 (clipboard copy of a selection). Full-table export is a later milestone.

## Selecting

- Click a cell to focus it.
- Shift-click another cell to grow a rectangular range from the first focus.
- Click a row or column header when the table offers that selection (same as the DataTable selection model).

## Copy formats

With the grid focused (`DataTable` key context):

| Action | Shortcut / control |
| --- | --- |
| Copy as TSV (default) | Cmd/Ctrl+C |
| Copy as TSV / CSV / JSON / INSERT | Right-click context menu |

- **TSV** — tab-separated values with a header row. Default for Cmd/Ctrl+C so spreadsheets paste cleanly.
- **CSV** — comma-separated values; fields with commas, quotes, or newlines are quoted (RFC 4180-style).
- **JSON** — a pretty-printed array of objects keyed by column name. Displayed `NULL` becomes JSON `null`.
- **INSERT** — one `INSERT INTO … VALUES (…);` statement. On a table Data tab the target is `schema.table` with dialect quoting. On a SQL result tab the placeholder table name is `table`.

NULL cells copy as the text `NULL` in TSV/CSV/INSERT (and as JSON `null`). Loading placeholders copy as empty strings.

Copy uses the current page of rows in the result window. It does not pull rows that have not been loaded yet.
