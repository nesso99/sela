# Safe mode

Per-connection write protection for the SQL editor (M2-4). Design: [09-safe-mode.md](../design/ux/09-safe-mode.md).

## Levels

Set **Safe mode** in the connection dialog. The toolbar engine line always shows the level: `Safe: Off`, `Safe: Confirm`, or `Safe: Read-only`. Missing `safe_mode` in an older `connections.json` loads as Off. Saving Off omits the field. There is no schema migration.

| Level | Editor |
| --- | --- |
| Off | Writes run immediately. |
| Confirm writes | A write asks first (`Run this write?`). Cancel is the default; Enter and Escape cancel. |
| Read-only | A write is refused (`Read-only connection. The statement was not run.`). Reads run. |

At every level, `UPDATE` or `DELETE` without a `WHERE` clause asks (`This statement has no WHERE`). `WHERE 1=1` counts as a WHERE and does not trigger that warning. Unparseable SQL is treated as a write: Off and Confirm show a parse dialog with **Run anyway**; Read-only refuses it.

Run-all confirms once for the batch when any statement writes. Cancel runs nothing in the batch, including earlier reads.

History **Rerun** of a write follows the same rules. Reads keep the existing history confirm path.

## Grid Commit

`decide_grid_commit_gate` in `sela-core` applies the same three levels to grid Commit (Confirm asks; Read-only refuses; Off sends). Wiring Commit into the UI lands with the grid editing / Preview work (M2-3 app).

## Classifier limits

Classification uses `sqlparser` with the connection dialect. A function call inside a `SELECT` still counts as a read. Do not rely on safe mode to block side effects from functions.
