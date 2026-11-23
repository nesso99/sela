# Filtering and sorting table data

On a **table** tab (not an arbitrary SQL result), Sela can filter and sort rows without writing SQL. Filters and sorts are applied server-side and re-query from page 1.

## Filter bar

- Open or close the filter bar with **Ctrl/⌘+F**, or the **Filter** button in the toolbar.
- Each condition has a **column**, **operator**, and **value** (value is hidden for `is null` / `is not null`).
- Conditions are combined with **AND** only in this release.
- **Apply** (or Enter in a filter field) runs the filtered preview. Any invalid row refuses the whole Apply and leaves the previous page. **Clear** removes all conditions and reloads the unfiltered estimate. **Add** appends another row.
- Operators depend on the column type. Pattern operators (**contains** / **not contains** / **starts with** / **ends with**) are only offered for text-like columns. Click the operator button to cycle legal operators for that column.
- **in** takes a comma-separated list (each item is a bound parameter).
- Pattern operators use `LIKE` / `ILIKE` with `ESCAPE '!'` (so `%`, `_`, and `!` in the value are literals). They do not auto-add wildcards beyond what the operator itself wraps (contains → `%value%`). Empty patterns are rejected; empty text for `=` binds a real empty string.
- Never emit `col = NULL` — use **is null**. On a text column, the characters `NULL` are a normal string.
- Values are bound parameters; identifiers are quoted. Generated SQL is **not** written into the SQL editor (that would flip the tab to a capped SQL result). A muted strip under the grid shows the `WHERE` / `ORDER BY` fragment with placeholders.

Filter state for table tabs is saved in `tabs.json` and restored when you reopen Sela. Restoring a tab does **not** auto-run a query until you reconnect and open the table as usual.

## Column sort

- Plain click on a header: if that column is the only sort key, cycle **asc → desc → none**; otherwise replace the list with that column ascending.
- **Shift-click** adds another key (or flips / removes that key), up to **3** columns. Headers show marks like `title ▲1` / `author ▼2`.
- Sort is an `ORDER BY` on quoted identifiers (server-side). The grid does not sort the visible page client-side.
- SQL result tabs do not get header sort in this release.

## Row counts

Without a filter, table previews still show an engine estimate when available (Postgres / MySQL).

When any filter is active, the estimate is invalid: status shows `unknown rows · N on this page · <ms>`. Use **Count** in the status bar to run `COUNT(*)` with the same `WHERE`. A failed count does not show an error banner over the grid.

## SQL tabs

Filter and header sort apply only to browsed **table** tabs. For free-form SQL, write `WHERE` / `ORDER BY` in the editor and run the statement. **Ctrl/⌘+Enter** runs the editor unless a filter field is focused (then it Applies).
