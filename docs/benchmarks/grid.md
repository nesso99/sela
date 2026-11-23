# Results grid benchmark

Decision record for the results grid and the result **model** behind it.

## Recommendation

**Keep DataTable; fix the result model (M1-2a).**

At 100 000 × 50, DataTable already virtualizes **both** rows and columns. Paint uses truncated, cached cell display strings for the visible window (≈23 × ≤11 on a 1280×800 window). Scroll frame times on this VM are capped by software Vulkan / Xvfb (~21 fps), not by cell formatting.

Do **not** rewrite the grid for virtualization. The measurable risk at 100k×50 was **memory of the result set** (especially wide Json), not the table widget.

### Follow-ups

1. **M1-2a (this PR):** `ResultWindow` with a page cap, lazy Json/`Arc<str>` Text, truncated + cached cell display, decode-error markers.
2. **M1-2b:** real query cancel + batched streaming off `ResultWindow::touch` / `pending_pages`, dropping per-page `COUNT(*)`.
3. Revisit a custom grid only if a real GPU build still misses frame budget after the model work, or if we need features DataTable cannot grow into.

## Method

### Harness

| Piece | How to run |
| --- | --- |
| Grid UI bench | `cargo run -p sela-app --release --example grid_bench` (+ `--wide`) |
| `Value::display` microbench | `cargo bench -p sela-core --bench value_display` |

The example mirrors production `crates/sela-app/src/ui/grid.rs`: `TableDelegate::render_td` asks `ResultWindow::cell` (truncated + cached), and copy uses full `Value::display()`. Pages are generated on demand as the viewport moves; at most `ROW_WINDOW_MAX_PAGES` (16) pages stay resident.

Protocol:

1. Open an empty `DataTable`, wait one frame.
2. Reset a `ResultWindow` for 100 000 × 50, seed the first viewport pages, measure **time to first frame** and RSS delta.
3. **Vertical fling** (~3 s): `scroll_to_row` jumps of 400 rows; record inter-frame times → p50/p95/p99 and effective fps.
4. **Horizontal fling** (~2.5 s): `scroll_to_col` jumps of 3 columns across all 50; same frame stats; paint counters reset so horizontal display bytes isolate the column sweep.

### Datasets

- **narrow**: cycling Int / Decimal / short Text / Timestamp / Uuid / Bool / Float / small Json / Date / Time (with occasional Null).
- **wide**: same, but column 48 = Text(1 KiB) and column 49 = Json (~10 KiB) on every row (shared `Arc<str>` payloads across pages).

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

## Before / after (M0-6 → M1-2a)

| Metric | Narrow before | Narrow after | Wide before | Wide after |
| --- | --- | --- | --- | --- |
| RSS delta after load | **+625 MiB** | **+30.1 MiB** | **+11.6 GiB** | **+33.6 MiB** |
| Time to first frame | 43.9 ms | 263.5 ms | 52.7 ms | 66.6 ms |
| Vertical fling p50 / p95 | 48.0 / 48.2 ms | 48.0 / 48.1 ms | 48.0 / 48.2 ms | 48.0 / 48.2 ms |
| Horizontal fling p50 / p95 | 48.0 / 48.2 ms | 48.0 / 48.0 ms | 48.0 / 48.2 ms | 48.0 / 48.0 ms |
| Horizontal display bytes | 178 425 | 178 425 | **2 348 298** | **231 516** |

Wide RSS is now dominated by the bounded window (≤16 × 100 rows), not by retaining 100k owned Json trees. Horizontal display bytes drop on wide because cells truncate to ≤200 chars (newlines → ⏎) with a per-cell cache.

## Raw numbers (M1-2a)

### Grid — narrow (100 000 × 50)

| Metric | Value |
| --- | --- |
| Setup (columns + warm page) | 0.5 ms |
| Baseline RSS | 117.3 MiB |
| RSS after load | 147.4 MiB (**+30.1 MiB**) |
| Time to first frame | **263.5 ms** |
| `render_td` on that frame | 253 (≈ visible rows × visible cols) |
| Pages inserted / loaded at end | 128 / 16 (`ROW_WINDOW_MAX_PAGES`) |
| Max visible rows / cols | **23 / 11** |
| Columns virtualized? | **yes** (`max_cols` ≪ 50) |
| Vertical fling | 62 frames · p50 **48.0 ms** · p95 48.1 ms · p99 48.3 ms · **20.8 fps** |
| Horizontal fling | 52 frames · p50 **48.0 ms** · p95 48.0 ms · p99 48.1 ms · **20.8 fps** |
| Horizontal paint | 13 340 `render_td` · 178 425 display bytes |

### Grid — wide (100 000 × 50, last two cols 1 KiB / ~10 KiB)

| Metric | Value |
| --- | --- |
| Setup (columns + warm page) | 0.5 ms |
| Baseline RSS | 106.1 MiB |
| RSS after load | 139.8 MiB (**+33.6 MiB**) |
| Time to first frame | **66.6 ms** |
| `render_td` on that frame | 253 |
| Pages inserted / loaded at end | 128 / 16 |
| Max visible rows / cols | **23 / 11** |
| Columns virtualized? | **yes** |
| Vertical fling | 62 frames · p50 **48.0 ms** · p95/p99 ~48–50 ms · **20.7 fps** |
| Horizontal fling | 52 frames · p50 **48.0 ms** · p95/p99 ~48 ms · **20.8 fps** |
| Horizontal paint | 13 271 `render_td` · **231 516** display bytes |

Notes:

- The logical result is still 100k rows; only a sliding window of pages is resident.
- Wide horizontal display bytes stay bounded by truncation even when trailing wide columns enter the virtualized column window.
- Frame times do **not** move on lavapipe (still vsync-ish ~48 ms).

### `Value::display()` microbench

`cargo bench -p sela-core --bench value_display` (same machine, release):

Json display is now a borrow of the raw `Arc<str>` (no per-paint serialize). Truncation / caching live in `ResultWindow`, not in `display()`.

## Why not a custom grid?

DataTable’s implementation already uses `uniform_list` vertically and a horizontal `virtual_list` for scrollable columns (gpui-component 0.10 / GPUI Kit 0.10). A from-scratch grid would reimplement that stack for little scroll-fps gain on this workload. The result-model changes above address the memory and per-paint formatting risks without a new widget.
