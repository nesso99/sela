# Results grid benchmark (M0-6)

Decision record for whether Sela should keep gpui-component’s `DataTable` or replace it with a custom `uniform_list` + column-virtualized grid.

## Recommendation

**Keep DataTable.**

At 100 000 × 50, DataTable already virtualizes **both** rows and columns. Paint calls `Value::display()` only for cells in the visible window (≈23 × ≤11 on a 1280×800 window). Scroll frame times on this VM are capped by software Vulkan / Xvfb (~21 fps), not by cell formatting — even when trailing 1 KiB Text / ~10 KiB Json columns are scrolled into view.

Do **not** rewrite the grid for virtualization. Prefer these follow-ups (not in this PR):

1. **Keep paging** (`PAGE_SIZE = 100`). Materializing 100k wide Json cells costs ~11 GiB RSS; that is a dataset problem, not a table widget problem.
2. **Truncate display** for Text/Json in the grid (e.g. first N chars) so `display()` / text shaping stay bounded when a huge cell is visible.
3. **Avoid cloning** the `Value` in `render_td` before `display()` (borrow instead); for Json, consider caching the compact string.
4. Revisit a custom grid only if a real GPU build still misses frame budget after (2)–(3), or if we need features DataTable cannot grow into.

## Method

### Harness

| Piece | How to run |
| --- | --- |
| Grid UI bench | `cargo run -p sela-app --release --example grid_bench` (+ `--wide`) |
| `Value::display` microbench | `cargo bench -p sela-core --bench value_display` |

The example mirrors production `crates/sela-app/src/ui/grid.rs`: `TableDelegate::render_td` clones the cell, calls `display().into_owned()`, and paints a truncated mono cell. It counts `render_td` / `display` invocations and records `visible_rows_changed` / `visible_columns_changed`.

Protocol:

1. Open an empty `DataTable`, wait one frame.
2. Load a synthetic `QueryPage` (100 000 rows × 50 columns), measure **time to first frame** and RSS delta.
3. **Vertical fling** (~3 s): `scroll_to_row` jumps of 400 rows; record inter-frame times → p50/p95/p99 and effective fps.
4. **Horizontal fling** (~2.5 s): `scroll_to_col` jumps of 3 columns across all 50; same frame stats; paint counters reset so horizontal display bytes isolate the column sweep.

### Datasets

- **narrow**: cycling Int / Decimal / short Text / Timestamp / Uuid / Bool / Float / small Json / Date / Time (with occasional Null).
- **wide**: same, but column 48 = Text(1 KiB) and column 49 = Json (~10 KiB multi-line array) on every row.

### Machine (this run)

| | |
| --- | --- |
| Host | Cloud Agent VM (`Linux 6.12`, x86_64) |
| CPU | Intel Xeon (hypervisor), **4** cores @ 2400 MHz |
| RAM | **15 GiB** |
| Display | **Xvfb** 1280×800×24 |
| GPU | **Mesa lavapipe** software Vulkan (`VK_ICD_FILENAMES=…/lvp_icd.json`) |
| Rust | `rustc 1.100.0`, `--release` |

Software rendering is a **lower bound** on interactive fps. A Metal / real Vulkan GPU should be faster; absolute frame times here should not be used as a ship gate.

## Raw numbers

### Grid — narrow (100 000 × 50)

| Metric | Value |
| --- | --- |
| Generate dataset | 400 ms |
| Baseline RSS | 104.5 MiB |
| RSS after load | 729.2 MiB (**+625 MiB**) |
| Time to first frame | **43.9 ms** |
| `render_td` on that frame | 253 (≈ visible rows × visible cols) |
| Max visible rows / cols | **23 / 11** |
| Columns virtualized? | **yes** (`max_cols` ≪ 50) |
| Vertical fling | 63 frames · p50 **48.0 ms** · p95 48.2 ms · p99 48.3 ms · **21.1 fps** |
| Horizontal fling | 52 frames · p50 **48.0 ms** · p95 48.2 ms · p99 48.3 ms · **20.8 fps** |
| Horizontal paint | 13 340 `render_td` · 178 425 display bytes |

### Grid — wide (100 000 × 50, last two cols 1 KiB / ~10 KiB)

| Metric | Value |
| --- | --- |
| Generate dataset | 11.9 s |
| Baseline RSS | 104.8 MiB |
| RSS after load | 11 679 MiB (**+11.6 GiB**) |
| Time to first frame | **52.7 ms** |
| `render_td` on that frame | 253 |
| Max visible rows / cols | **23 / 11** |
| Columns virtualized? | **yes** |
| Vertical fling | 62 frames · p50 **48.0 ms** · p95/p99 48.2 ms · **20.8 fps** |
| Horizontal fling | 52 frames · p50 **48.0 ms** · p95/p99 48.2 ms · **20.8 fps** |
| Horizontal paint | 13 271 `render_td` · **2 348 298** display bytes |

Notes:

- `display_calls == render_td` for the whole run: the grid formats only cells it paints, not the full 5 M cells.
- Wide horizontal display bytes jump ~13× vs narrow because trailing wide columns enter the virtualized column window; frame times do **not** move on lavapipe (still vsync-ish ~48 ms).
- Wide RSS is dominated by retaining 100k owned Json trees in the result `Vec`, not by DataTable’s element tree.

### `Value::display()` microbench

`cargo bench -p sela-core --bench value_display` (same machine, release):

| Case | ns / call | Notes |
| --- | --- | --- |
| Narrow mix (12 variants) | ~81 | Allocating formats for numbers/temporals |
| Text 1 KiB | ~8 | Borrowed |
| Text 10 KiB | ~8 | Borrowed |
| Json ~10 KiB (serialize) | ~23 500 | Owns a new `String` every call |
| Grid path Text 1 KiB (`clone` + `display` + `into_owned`) | ~47 | Matches `grid.rs` |
| Grid path Json ~10 KiB (same) | ~112 700 | Clone tree + serialize |
| Timestamp | ~256 | Owned format string |

So Json (and any owned format) is the expensive cell type; Text is cheap to display but still expensive to **store** at 100k× width.

## Why not a custom grid?

DataTable’s implementation already uses `uniform_list` vertically and a horizontal `virtual_list` for scrollable columns (gpui-component 0.10 / GPUI Kit 0.10). A from-scratch grid would reimplement that stack for little scroll-fps gain on this workload. The measurable risks at 100k×50 are:

- **Memory of the result set** (especially wide Json), solved by paging / not holding the full set;
- **Per-paint Json formatting** when a wide column is visible, solved by truncation / caching in `render_td`.

Those are small, localized changes to `GridDelegate` / session paging — not a new widget.
