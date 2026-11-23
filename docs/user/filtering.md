# Filtering and sorting table data

On a **table** tab (not an arbitrary SQL result), Sela can filter and sort rows without writing SQL. Filters and sorts are applied server-side and re-query from page 1.

## Filter bar

- Open or close the filter bar with **Ctrl/⌘+F**, or the **Filter** button in the toolbar.
- Each condition has a **column**, **operator**, and **value** (value is hidden for `is null` / `is not null`).
- Conditions are combined with **AND**.
- **Apply** runs the filtered preview. **Clear** removes all conditions. **Add** appends another row.
- Click the operator button to cycle: `=`, `≠`, `<`, `≤`, `>`, `≥`, contains, not contains, starts with, ends with, is null, is not null, in.
- **in** takes a comma-separated list (each item is a bound parameter).
- **contains** / **starts with** / **ends with** use `LIKE` with escaped `%`, `_`, and `\`. PostgreSQL uses `ILIKE` (case-insensitive). MySQL and SQLite use their default case-insensitive `LIKE`.
- The bar shows the generated SQL with placeholders; values are never string-interpolated into the statement.

Filter state for table tabs is saved in `tabs.json` and restored when you reopen Sela. Restoring a tab does **not** auto-run a query until you reconnect and open the table as usual.

## Column sort

Click a column header to cycle **descending → ascending → none**. Sort is an `ORDER BY` on the quoted identifier (server-side). Only one column is sorted at a time in this release.

## Row counts

Without a filter, table previews still show an engine estimate when available (Postgres / MySQL), with an **Exact count** button.

When any filter is active, the estimate is invalid: the status shows an unknown count. Use **Exact count** to run `COUNT(*)` with the same `WHERE`.

## SQL tabs

Filter and header sort apply only to browsed **table** tabs. For free-form SQL, write `WHERE` / `ORDER BY` in the editor and run the statement.
