# SQL editor

The editor is the bottom of the content column. M1-4 replaces the single-statement textarea behavior with statement-at-caret, multi-statement runs, and a gutter. Format (`sqlformat`) is backlog, not M1-4. Autocomplete is M3-2 (US-08) and is specified here so its chords are not taken by M1-4. Shortcut notation is in [README.md](./README.md). The running, cancelled, capped, and error states of the window are in [01-main-window.md](./01-main-window.md).

![Editor split, statement gutter, autocomplete above the caret, and a result-set strip](images/sql-editor.png)

## What is built

| Piece | Today | File |
| --- | --- | --- |
| Widget | `Textarea` / `TextareaState`, 6 rows, monospace | `tab_view.rs`, `content.rs` `sql_editor` |
| Height | Fixed `h(px(176.))`, not draggable, not collapsible | `content.rs` |
| Run | The whole buffer. `normalize_sql` rejects zero statements and rejects more than one. | `query.rs` `run_editor`, `sql.rs` `normalize_sql` |
| Empty buffer | Status `Write a SQL statement, then run it.` | `run_editor` |
| Several statements | Error string `run one statement at a time` (`SqlError::Multiple`) | `normalize_sql` |
| Unclosed quote | Also `SqlError::Multiple`. `split_statements` returns `Multiple` when a quote is still open. | `sql.rs` |
| Cancel | Stop and `secondary-.`. Rows already delivered stay. | `stop_query`, `apply_cancelled` |
| Running label | Button becomes `Running…`. Status is set to `Running…` once. | `content.rs`, `query.rs` |
| Results | One `DataTable` per tab | `TabView.grid` |
| Error position | `DbError` has a message, SQLSTATE, and vendor code. No byte offset. SPECS mentions a future syntax variant with a position. | `error.rs` |

`split_statements` already splits on semicolons outside quotes and comments and keeps the statement text. It does not return byte ranges. M1-4 needs the ranges so the gutter and the caret can agree.

## Split

The editor stays under the grid. The divider is horizontal (the column is split vertically). It replaces the fixed 176px height.

| | |
| --- | --- |
| Default | 176px, the current `sql_editor` height |
| Minimum | 72px: the 28px SQL header plus one text line |
| Maximum | 70% of the main column below the tab bar |
| Double-click | Returns to 176px |
| Collapse | Chevron in the SQL header, or `secondary-shift-e`. Height 28px. Run and Stop stay. The buffer is kept. |
| Drag | Vertical. Each tab stores its own height on `TabView`. |

The filter bar and the error banner take height from the grid, not from the editor. Opening the filter does not jump the caret.

The 28px header remains: the label `SQL`, the collapse chevron, Stop, Run. Load more moves to the grid footer and the status bar ([01-main-window.md](./01-main-window.md)). Exact count leaves this header ([02-filter-sort-review.md](./02-filter-sort-review.md)).

## Run and cancel

`normalize_sql` stays the helper for “this string is exactly one statement”. The editor stops using it as the only way to run.

| Focus and selection | Shortcut | What runs |
| --- | --- | --- |
| Editor, no selection | `secondary-enter` | The statement that contains the caret |
| Editor, non-empty selection | `secondary-enter` | The selected text only, split with `split_statements`. Do not expand to the surrounding statement. |
| Editor | `secondary-shift-enter` | Every statement in the buffer, top to bottom |
| Filter bar | `secondary-enter` or `Enter` | Apply the filter. Do not run SQL. |
| Anywhere else | `secondary-enter` | Nothing |

Statement at the caret: extend `split_statements` to return each statement’s byte range in the buffer. The caret belongs to a statement when it sits inside that range, or in the whitespace and comments between the previous statement and this one. A caret in the leading comments belongs to the first statement.

The gutter draws a 2px muted bar on every line of the statement that `secondary-enter` will run. A selection replaces that highlight with the selection’s own lines.

Several statements run one after another, not as a batch the server parses itself. Stop at the first error. Statements that already finished stay as result sets. `secondary-.` cancels the in-flight statement and does not start the next. That is the same `CancellationToken` path as Stop.

`secondary-enter` is bound on the editor focus context, not on the window. The global binding in `ui/mod.rs` is why Run fires while the user is elsewhere.

A write classified by safe mode confirms or refuses before it runs. The chords on this page do not change. See [09-safe-mode.md](./09-safe-mode.md).

Empty buffer: keep the current status, `Write a SQL statement, then run it.`

Unclosed quote: `split_statements` should return a distinct `SqlError::UnclosedQuote` instead of `Multiple`. The gutter marks the opening quote. The banner says `Unclosed quote`. Do not say `run one statement at a time`.

## Running indicator

No spinner over the grid.

- Run’s label stays `Running…` while `view.busy`. Stop stays enabled.
- Status ticks: `Running · 1.2 s`, about four times a second, from `query_started`.
- The gutter’s statement mark switches to the running statement’s lines for the duration.
- A new SQL run clears the grid immediately, which `start_sql_stream` already does via `result.reset`. Before the first row, the empty message is `Running…` when `GridDelegate.loading` is true.
- Table Previous/Next may keep the old page on screen. That path is not the editor.

Cancel and the 1,000-row cap keep the behavior in the main-window spec.

## Autocomplete

Milestone M3-2 (US-08). Maps to QA [v0.3-test-plan.md](../../qa/v0.3-test-plan.md) **AU-01…AU-16** (v0.3 UX updates AU-01 trigger length, AU-05 manual chord, AU-13 Tab accepts — see [Answers to QA](#answers-to-qa-110-autocomplete)).

**Trigger.**

| Event | Behavior |
| --- | --- |
| After `.` | Open immediately (`alias.` / `table.` / `schema.`). |
| After **2** identifier characters | Open (not after the first character). |
| `ctrl-space` (Control+Space, not `secondary`) | Manual open / refresh of suggestions in the editor focus context. |
| Inside a comment or string | Do not open or move the selection. |
| IME composition active | Popup stays closed. Enter commits the composition and does **not** accept a completion. |

`secondary-space` stays free (Spotlight). Binding `ctrl-space` only while the SQL editor is focused is intentional for v0.3; users who need the OS input-source switch on macOS use the OS alternate chord.

**Catalog.** Same cache as the sidebar. Load on connect. Refresh on: Refresh schema (sidebar / palette), after a successful DDL statement run from the editor, and when switching database/schema. While the catalog is loading, the popup shows a single muted row `Loading schema…` (not selectable). If the catalog is empty after load, `No schema loaded.` 

**Contents.** Dialect keywords, schema names, table names, column names, and aliases from the current statement. After `alias.` or `table.`, that relation’s columns first.

**Ranking.**

1. Case-insensitive prefix on the typed fragment.
2. Columns of tables named in the current statement, above other columns.
3. Other substring matches.
4. Keywords, below identifiers, unless the fragment itself is a keyword prefix.

**Rows.** Monospace name; muted label on the right: type name, `table`, `schema`, or `keyword`. Max 8 rows × 22px (≤ 176px) then scroll; width up to 360px.

**Placement.** Prefer above the caret; flip below only when there is no room above.

**Keys.**

| Key | Behavior |
| --- | --- |
| Up / Down | Move selection |
| Enter | Accept and insert |
| Tab | Accept and insert (same as Enter) |
| Escape | Dismiss; leave the text |
| Click outside | Dismiss |
| Arrows when closed | Do not open the popup |

### Acceptance criteria (→ QA IDs)

1. **AU-01** (updated) — Popup opens after **two** identifier characters, not one.
2. **AU-02** — Opens immediately after `.`.
3. **AU-03** — No open inside comment/string.
4. **AU-04** — IME: closed during composition; Enter commits composition only.
5. **AU-05** (updated) — `secondary-space` stays free; **`ctrl-space` manually opens** autocomplete in the editor.
6. **AU-06…AU-12** — Catalog + keywords; alias columns first; ranking; quoting; large catalog; row chrome; placement above caret.
7. **AU-13** (updated) — Up/Down navigate; **Tab and Enter accept**; Escape dismisses; click outside closes.
8. **AU-14** — Arrows do nothing to a closed popup.
9. **AU-15** — Optional tree-sitter; skip if not shipped.
10. **AU-16** — Completions follow catalog refresh (connect, Refresh schema, successful DDL, switch database/schema); loading row `Loading schema…`.

### Answers to QA (#110) (autocomplete)

| QA question | Answer |
| --- | --- |
| 7. Autocomplete cache refresh | Load on connect; refresh on Refresh schema, successful DDL from the editor, and database/schema switch. Loading row while in flight. |

Delta vs the M1-era popup notes that QA marked “aligned”: trigger length 2; `ctrl-space` manual; Tab accepts. QA should retarget AU-01, AU-05, AU-13 in the follow-up plan PR.

## Errors

The banner and the gutter are both kept. They are not substitutes.

1. **Banner and status.** Always. `error_banner` above the grid shows the full `DbError` message, capped around 60px, plus the status line from the main-window spec (`Query failed · SQLSTATE 42601`). On a multi-statement run, the status names the statement: `Query failed · statement 2 · SQLSTATE 42601`.
2. **Gutter and underline.** Only when the engine reports a position. A 2px `danger` bar on that line, and a 1px `danger` underline on the reported span. This is separate from the muted bar that marks the statement `secondary-enter` will run. If the engine reports a line and no column, underline the whole line. If it reports no position, draw nothing in the gutter and do not guess a line.

`DbError` has no position field today. Parse a position only from what the engine actually returns (Postgres `LINE n` and a caret line, or a future position field). A parse that fails means there is no position.

A local failure from `split_statements` (empty buffer, unclosed quote) is not an engine error. The banner still shows it. The unclosed-quote gutter is allowed because `split_statements` knows the opening quote’s offset. That is not a guessed engine position.

## Multiple result sets

One grid per tab. Do not stack grids.

A 22px strip between the grid and the splitter: `Result 2 of 3 · statement 2`, with Prev and Next. The strip is a tab stop (`ResultStrip` key context). With the strip focused, Left and Right move Prev and Next. Do not bind Alt+arrows for it; window managers and word navigation already use those keys. Tab switching stays on `secondary-alt-left` / `secondary-alt-right` when focus is outside a text field.

Each finished statement keeps its own `ResultWindow`. Store at most 8. When a 9th finishes, evict the oldest and set the status note `Showing the latest 8 results`. A statement that returns no row set (`Executed`) is still a result: the grid shows the existing `show_message` text (`3 rows affected · 4 ms`).

Run-all stops at the first error. Do not start the next statement. Results that already finished stay switchable. The strip selects the failed statement and names it (`Statement 2 failed`). The banner has the full message. The gutter marks that statement only when the engine reported a position. Status: `Query failed · statement 2 · SQLSTATE 42601`.

Memory stays bounded: 8 windows, each already capped by `ResultWindow` and the SQL row cap. Do not materialize every result without the cap.

## Find

`secondary-f` in the editor opens a one-line find bar in the SQL header, not the filter bar. Type, Enter jumps to the next match, `secondary-g` / `secondary-shift-g` are next and previous, Escape closes find. No replace in M1-4. Matches use the editor’s current selection. This exists so filter and find do not share one global chord.

## Format (backlog)

`sqlformat` is not part of M1-4. Do not add a format command or a format binding in that milestone. `secondary-shift-f` is reserved for a later format action and must not be assigned to anything else. When format is built, it formats the selection if there is one, otherwise the buffer, and it does not run the statement.

## Highlight

M1-4. Stay inside the theme: comments in `muted_foreground`, strings in `foreground`, keywords slightly brighter than identifiers. No multi-color syntax theme. If gpui-component 0.10 has a code editor, use it in place of `Textarea`. If it does not, keep the monospace textarea for M1-4 and draw the gutter and the underline as an overlay. Autocomplete does not require the code editor; it is a popup anchored to the caret in M3-2.

## Keyboard

| Action | Shortcut | Milestone |
| --- | --- | --- |
| Run caret statement or selection | `secondary-enter` | M1-4 |
| Run all statements | `secondary-shift-enter` | M1-4 |
| Cancel | `secondary-.` or Stop | done |
| Find | `secondary-f` | M1-4 |
| Find next / previous | `secondary-g` / `secondary-shift-g` | M1-4 |
| Format | `secondary-shift-f` | Reserved. Backlog, not M1-4. Do not bind it yet. |
| Collapse editor | `secondary-shift-e` | M1-4 |
| Accept completion | `Enter` or `Tab` while the popup is open | M3-2 |
| Manual trigger | `ctrl-space` (editor focused) | M3-2 |
| Dismiss popup or find | `Escape` | M1-4 / M3-2 |

`secondary-shift-enter` adds a filter row when the filter bar is focused. The two bindings use different focus contexts.

## Implementation notes for engineering

- Change `split_statements` to return ranges (start and end byte offsets into the original string) and add `SqlError::UnclosedQuote`. Keep `normalize_sql` as “exactly one statement” for callers that still want that. Update its tests in `sql.rs`.
- Store the editor height and the collapsed flag on `TabView`, in memory only. Default height 176. Do not persist them.
- Replace the `RunQuery` window binding with an editor-context binding. `Workspace::run_editor` reads the caret and the selection from the editor state instead of always taking `value()`.
- Run statements sequentially on the existing per-tab request id. A new run bumps the request id so a late batch from statement 1 cannot paint into statement 2. Cancel cancels the token and does not schedule the rest.
- Keep a `Vec` of at most 8 result snapshots on the tab. Showing one calls the same grid update `apply_outcome` uses. Do not allocate a second `DataTable`.
- Gutter and underline read an optional position. Parse Postgres `LINE n` only in one function with tests taken from real driver messages. If that parse fails, draw no gutter and no underline. The banner still shows the full message.
- IME: while a composition is active, the autocomplete popup stays closed and Enter is not treated as “accept completion”.
- Bind `ctrl-space` only on the SQL editor focus context. Accept on both Enter and Tab while the popup is open.
- Do not enable `TableState::sortable(true)` on SQL results as a stand-in for ORDER BY.

## Decisions

1. **Autocomplete is M3-2.** Auto-trigger after `.` or 2 identifier chars; manual `ctrl-space`; Tab and Enter accept; Esc dismisses.
2. **Catalog refresh:** connect, Refresh schema, successful DDL from the editor, switch database/schema. Loading row: `Loading schema…`.
3. **Run-all stops at the first error.** Name that statement in the strip and the status (`Query failed · statement 2`). Keep every result that already finished. Do not start the rest.
4. **Store at most 8 result sets.** Evict the oldest when a new one arrives.
5. **No `sqlformat` in M1-4.** Format is backlog. `secondary-shift-f` is reserved for that later work and is not in the M1-4 binding set.

## Open questions

None for autocomplete product behavior. AU-15 tree-sitter remains optional per roadmap.
