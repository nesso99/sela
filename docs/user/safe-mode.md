# Safe mode

Per-connection write protection for the SQL editor and grid (M2-4). Design: [09-safe-mode.md](../design/ux/09-safe-mode.md).

## Levels

Set **Safe mode** in the connection dialog. The helper under the select is one line per level (`Writes are refused. Reads run as usual.` for Read-only). The toolbar engine line always shows the level: `Safe: Off`, `Safe: Confirm`, or `Safe: Read-only`. Missing `safe_mode` in an older `connections.json` loads as Off. Saving Off omits the field. There is no schema migration.

A new connection starts at Off. Choosing the **Red** color swatch before you touch Safe mode sets Confirm writes; leaving Red sets Off again. After you change Safe mode yourself, color no longer changes it. Editing an existing connection never changes Safe mode when the color changes.

Saving the connection always reconnects, so a level change (including Read-only server guards) applies on that reconnect.

| Level | SQL editor | Grid | Server |
| --- | --- | --- | --- |
| Off | Writes run immediately (history Rerun still asks “Rerun this statement?”). | Commit sends immediately. | Normal session. |
| Confirm writes | A write asks first (`Run this write?`). Cancel is the default; Enter and Escape cancel. | Commit asks (`Commit N changes?`). | Normal session. |
| Read-only | A write is refused (`Read-only connection. The statement was not run.`). Reads run. | Data pane strip `Read-only · safe mode on this connection.`; Commit disabled; Discard still works. Pending edits are kept. | Postgres `default_transaction_read_only=on`; MySQL `SET SESSION TRANSACTION READ ONLY`; SQLite file opened read-only. |

At every level, editor `UPDATE` or `DELETE` without a `WHERE` clause asks (`This statement has no WHERE`). `WHERE 1=1` counts as a WHERE and does not trigger that warning. Grid Commit always has a key WHERE, so that warning never applies to it. Unparseable SQL is treated as a write: Off and Confirm show a parse dialog with **Run anyway**; Read-only refuses it.

Run-all confirms once for the batch when any statement writes. Cancel runs nothing in the batch, including earlier reads. When the no-WHERE or parse-failed dialog opens for a batch that has more than one write, the body adds `This batch also runs N other write(s).`

History **Rerun** of a write follows safe mode (including Read-only refusal and Confirm / no-WHERE / parse dialogs). When safe mode would allow the write immediately (Off, no extra warning), Rerun still uses the classic “Rerun this statement?” confirm. Reads keep that classic confirm path when the load-more classifier says they need it.

## Classifier limits

Classification uses `sqlparser` with the connection dialect. Writes include DML/DDL, `EXPLAIN ANALYZE` (including `EXPLAIN (ANALYZE …)`), `SELECT … INTO`, `SELECT … FOR UPDATE` / `FOR SHARE`, transactions, `SET`, `PRAGMA` assignments, and similar. A function call inside a plain `SELECT` still counts as a read — do not rely on safe mode alone to block side effects from functions; Read-only’s server guard is the backstop for those.
