# Filtering and sorting table data

On a **table** tab (not an arbitrary SQL result), Sela can filter and sort rows without writing SQL. Filters and sorts are applied server-side and re-query from page 1.

## Filter bar

- Open or close the filter bar with **Ctrl/⌘+F**, or the **Filter** button in the toolbar. When a filter is applied and the bar is closed, the button stays selected.
- Each condition has a **column**, **operator**, and **value** (value is hidden for `is null` / `is not null`).
- Conditions are combined with **AND** only in this release (no OR / groups).
- **Apply** (or Enter / Ctrl/⌘+Enter in a filter field) runs the filtered preview. Any invalid row refuses the whole Apply and leaves the previous page. **Clear** removes all conditions and reloads the unfiltered estimate. **Add** appends another row.
- Operators depend on the column type (`ColumnKind`). Text columns offer `=`, `≠`, `LIKE`, `NOT LIKE`, `in`, `is null`, `is not null`. Click the operator button to cycle legal operators for that column.
- **in** takes a comma-separated list (each item is a bound parameter).
- **LIKE** / **NOT LIKE** use `ESCAPE '!'`. `%` and `_` are wildcards; use `!` to escape them (and `!!` for a literal `!`). Empty patterns are rejected. Empty text for `=` binds a real empty string (`''`); the value field placeholder says `empty matches '' · IS NULL for nulls`.
- Never emit `col = NULL` — use **is null**. On a text column, the characters `NULL` are a normal string.
- Values are bound parameters; identifiers are quoted. Generated SQL is **not** written into the SQL editor (that would flip the tab to a capped SQL result). A muted strip under the grid shows the `WHERE` / `ORDER BY` fragment with placeholders; hover shows the full line, click copies it.

Filter state for table tabs is saved in `tabs.json` and restored when you reopen Sela. Restoring a tab does **not** auto-run a query until you reconnect and open the table as usual.

## Column sort

- Plain click on a header: if that column is the only sort key, cycle **asc → desc → none**; otherwise replace the list with that column ascending.
- **Shift-click** adds another key (or flips / removes that key), up to **3** columns. Headers show marks like `title ▲1` / `author ▼2`.
- Sort is an `ORDER BY` on quoted identifiers (server-side). The grid does not sort the visible page client-side.
- SQL result tabs do not get header sort in this release.

## Row counts

Without a filter, table previews still show an engine estimate when available (Postgres / MySQL).

When any filter is active, the estimate is invalid: status shows `unknown rows · N on this page · <ms>`, the page label no longer shows a stale total, and **Count** in the status bar runs `COUNT(*)` with the same `WHERE`. While counting, status ticks `Counting… · 1.2 s`; Stop / Ctrl/⌘+. cancels with `Count cancelled` (grid unchanged, no error banner). A failed count shows `Count failed · …` without a banner.

## SQL tabs

Filter and header sort apply only to browsed **table** tabs. For free-form SQL, write `WHERE` / `ORDER BY` in the editor and run the statement. **Ctrl/⌘+Enter** runs the editor unless a filter field is focused (then it Applies).
