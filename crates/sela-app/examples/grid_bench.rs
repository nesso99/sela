//! Results-grid benchmark: 100k rows × 50 columns through gpui-component DataTable
//! backed by [`sela_core::ResultWindow`] (bounded pages, lazy Json/`Arc<str>` Text,
//! truncated + cached cell display).
//!
//! ```text
//! # Software Vulkan (lavapipe) under Xvfb — numbers are a lower bound vs a real GPU:
//! export VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.json
//! xvfb-run -a cargo run -p sela-app --release --example grid_bench
//!
//! # Wide Text/Json cells (1 KiB + ~10 KiB columns), shared Arc payloads:
//! xvfb-run -a cargo run -p sela-app --release --example grid_bench -- --wide
//!
//! # Same shape, but unique Arc<str> per wide cell (realistic RSS):
//! xvfb-run -a cargo run -p sela-app --release --example grid_bench -- --wide-unique
//! ```

use std::env;
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{NaiveDate, NaiveTime};
use gpui_kit::component::table::{Column, DataTable, TableDelegate, TableState};
use gpui_kit::component::{v_flex, ActiveTheme, Theme, ThemeMode};
use gpui_kit::prelude::*;
use gpui_kit::{
    div, px, size, App, Bounds, Context, Entity, IntoElement, Render, SharedString,
    TitlebarOptions, Window, WindowBounds, WindowOptions,
};
use rust_decimal::Decimal;
use sela_core::{
    CellView, ColumnMeta, ResultWindow, RowState, Value, PAGE_SIZE, ROW_WINDOW_MAX_PAGES,
};
use uuid::Uuid;

const ROWS: usize = 100_000;
const COLS: usize = 50;
const VERTICAL_FLING_SECS: f64 = 3.0;
const HORIZONTAL_FLING_SECS: f64 = 2.5;
/// Jump far enough each frame to cover all 50 columns several times in the fling window.
const ROW_STEP: usize = 400;
const COL_STEP: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DatasetKind {
    Narrow,
    /// Wide columns share one Arc per payload type (optimistic RSS).
    Wide,
    /// Wide columns allocate a distinct Arc per cell (realistic RSS).
    WideUnique,
}

impl DatasetKind {
    fn from_args() -> Self {
        let args: Vec<String> = env::args().collect();
        if args.iter().any(|a| a == "--wide-unique") {
            Self::WideUnique
        } else if args.iter().any(|a| a == "--wide" || a == "-w") {
            Self::Wide
        } else {
            Self::Narrow
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Narrow => "narrow",
            Self::Wide => "wide",
            Self::WideUnique => "wide-unique",
        }
    }

    fn is_wide(self) -> bool {
        matches!(self, Self::Wide | Self::WideUnique)
    }
}

#[derive(Default)]
struct Counters {
    render_td: AtomicU64,
    display_calls: AtomicU64,
    display_bytes: AtomicU64,
    visible_rows_callbacks: AtomicU64,
    visible_cols_callbacks: AtomicU64,
    pages_inserted: AtomicU64,
    last_visible_rows: Mutex<(usize, usize)>,
    last_visible_cols: Mutex<(usize, usize)>,
    max_visible_rows: AtomicU64,
    max_visible_cols: AtomicU64,
}

impl Counters {
    fn note_visible_rows(&self, range: std::ops::Range<usize>) {
        self.visible_rows_callbacks.fetch_add(1, Ordering::Relaxed);
        let len = range.end.saturating_sub(range.start) as u64;
        self.max_visible_rows.fetch_max(len, Ordering::Relaxed);
        *self.last_visible_rows.lock().unwrap() = (range.start, range.end);
    }

    fn note_visible_cols(&self, range: std::ops::Range<usize>) {
        self.visible_cols_callbacks.fetch_add(1, Ordering::Relaxed);
        let len = range.end.saturating_sub(range.start) as u64;
        self.max_visible_cols.fetch_max(len, Ordering::Relaxed);
        *self.last_visible_cols.lock().unwrap() = (range.start, range.end);
    }
}

/// Shared wide payloads so pages do not each allocate unique 1 KiB / 10 KiB strings.
struct WidePayloads {
    text_1k: Arc<str>,
    json_10k: Arc<str>,
}

impl WidePayloads {
    fn new() -> Self {
        let text_1k: Arc<str> = Arc::from("W".repeat(1024));
        let mut items = Vec::with_capacity(180);
        for i in 0..180 {
            items.push(serde_json::json!({
                "id": i,
                "label": format!("row-payload-{i}"),
                "blob": "abcdefghijklmnopqrstuvwxyz0123456789",
            }));
        }
        let json_10k: Arc<str> =
            Arc::from(serde_json::to_string(&serde_json::Value::Array(items)).unwrap());
        Self { text_1k, json_10k }
    }
}

struct BenchDelegate {
    result: ResultWindow,
    kind: DatasetKind,
    wide: Arc<WidePayloads>,
    counters: Arc<Counters>,
}

impl BenchDelegate {
    fn empty(kind: DatasetKind, wide: Arc<WidePayloads>, counters: Arc<Counters>) -> Self {
        Self {
            result: ResultWindow::with_capacity(ROW_WINDOW_MAX_PAGES, PAGE_SIZE as usize),
            kind,
            wide,
            counters,
        }
    }

    fn begin(&mut self, columns: Vec<ColumnMeta>) {
        self.result.reset(columns, ROWS);
    }

    fn fill_missing(&mut self, pages: &[usize]) {
        // Cap synchronous fills per callback. DataTable can report a huge
        // visible_rows range on first layout; generating every page here was the
        // narrow first-frame regression (263 ms) in M1-2a.
        const MAX_PAGES_PER_TICK: usize = 2;
        for &page in pages.iter().take(MAX_PAGES_PER_TICK) {
            if self.result.loaded_page_indices().contains(&page) {
                continue;
            }
            let rows = generate_page(page, PAGE_SIZE as usize, COLS, self.kind, &self.wide);
            self.result.insert_page(page, rows);
            self.counters.pages_inserted.fetch_add(1, Ordering::Relaxed);
        }
    }
}

impl TableDelegate for BenchDelegate {
    fn columns_count(&self, _: &App) -> usize {
        self.result.columns().len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.result.total_rows()
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        let meta = &self.result.columns()[col_ix];
        let header = format!("{}   {}", meta.name, meta.type_name);
        let width = (header.chars().count() as f32 * 7.5).clamp(140.0, 340.0);
        Column::new(format!("{col_ix}:{}", meta.name), header)
            .width(px(width))
            .resizable(false)
            .movable(false)
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        self.counters.render_td.fetch_add(1, Ordering::Relaxed);
        let view = self.result.cell(row_ix, col_ix);
        let (null, loading, text) = match view {
            CellView::Loading => (false, true, SharedString::from("…")),
            CellView::Ready { null, display } => {
                self.counters.display_calls.fetch_add(1, Ordering::Relaxed);
                self.counters
                    .display_bytes
                    .fetch_add(display.len() as u64, Ordering::Relaxed);
                (null, false, SharedString::from(display))
            }
        };
        let color = if null || loading {
            cx.theme().muted_foreground
        } else {
            cx.theme().foreground
        };
        div()
            .h_full()
            .w_full()
            .px_2()
            .flex()
            .items_center()
            .truncate()
            .text_color(color)
            .font_family(cx.theme().mono_font_family.clone())
            .when(null || loading, |cell| cell.italic())
            .child(text)
    }

    fn visible_rows_changed(
        &mut self,
        visible_range: std::ops::Range<usize>,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) {
        self.counters.note_visible_rows(visible_range.clone());
        let needed = self.result.touch(visible_range);
        self.fill_missing(&needed);
    }

    fn visible_columns_changed(
        &mut self,
        visible_range: std::ops::Range<usize>,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) {
        self.counters.note_visible_cols(visible_range);
    }

    fn cell_text(&self, row_ix: usize, col_ix: usize, _: &App) -> String {
        match self.result.row(row_ix) {
            RowState::Ready(cells) => cells
                .get(col_ix)
                .map(|cell| cell.display().into_owned())
                .unwrap_or_else(|| "NULL".into()),
            RowState::Loading => String::new(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    WaitingFirstPaint,
    LoadAndMeasureFirstFrame,
    VerticalFling,
    HorizontalFling,
    Done,
}

struct FrameStats {
    samples: Vec<Duration>,
}

impl FrameStats {
    fn new() -> Self {
        Self {
            samples: Vec::with_capacity(512),
        }
    }

    fn push(&mut self, d: Duration) {
        if d > Duration::from_millis(0) {
            self.samples.push(d);
        }
    }

    fn percentile_ms(&self, p: f64) -> f64 {
        if self.samples.is_empty() {
            return 0.0;
        }
        let mut sorted: Vec<Duration> = self.samples.clone();
        sorted.sort_unstable();
        let idx = ((p / 100.0) * (sorted.len() as f64 - 1.0)).round() as usize;
        sorted[idx.min(sorted.len() - 1)].as_secs_f64() * 1000.0
    }

    fn effective_fps(&self) -> f64 {
        if self.samples.is_empty() {
            return 0.0;
        }
        let total: Duration = self.samples.iter().sum();
        if total.is_zero() {
            return 0.0;
        }
        self.samples.len() as f64 / total.as_secs_f64()
    }
}

struct BenchApp {
    grid: Entity<TableState<BenchDelegate>>,
    counters: Arc<Counters>,
    kind: DatasetKind,
    columns: Vec<ColumnMeta>,
    phase: Phase,
    baseline_rss_kb: u64,
    after_load_rss_kb: u64,
    setup_ms: f64,
    load_to_first_frame_ms: f64,
    first_frame_render_td: u64,
    vertical: FrameStats,
    horizontal: FrameStats,
    pre_horizontal_render_td: u64,
    pre_horizontal_display_calls: u64,
    pre_horizontal_display_bytes: u64,
    fling_started: Option<Instant>,
    last_frame: Option<Instant>,
    scroll_row: usize,
    scroll_col: usize,
    load_started: Option<Instant>,
    status: SharedString,
    quit_scheduled: bool,
}

impl BenchApp {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let kind = DatasetKind::from_args();
        let counters = Arc::new(Counters::default());
        let wide = Arc::new(WidePayloads::new());
        let grid = cx.new(|cx| {
            TableState::new(
                BenchDelegate::empty(kind, wide.clone(), counters.clone()),
                window,
                cx,
            )
            .sortable(false)
            .col_movable(false)
            .col_resizable(false)
            .row_selectable(false)
            .col_selectable(false)
            .cell_selectable(false)
        });

        let baseline_rss_kb = current_rss_kb();
        eprintln!(
            "grid_bench: preparing {} × {} ({}) with ResultWindow (max_pages={}) …",
            ROWS,
            COLS,
            kind.label(),
            ROW_WINDOW_MAX_PAGES
        );
        let setup_start = Instant::now();
        let columns = make_columns(COLS, kind);
        // Warm one page so generation cost is visible without retaining 100k rows.
        let _warm = generate_page(0, PAGE_SIZE as usize, COLS, kind, &wide);
        let setup_ms = setup_start.elapsed().as_secs_f64() * 1000.0;
        eprintln!(
            "grid_bench: setup in {setup_ms:.1} ms; baseline RSS {} MiB",
            baseline_rss_kb as f64 / 1024.0
        );

        let mut app = Self {
            grid,
            counters,
            kind,
            columns,
            phase: Phase::WaitingFirstPaint,
            baseline_rss_kb,
            after_load_rss_kb: 0,
            setup_ms,
            load_to_first_frame_ms: 0.0,
            first_frame_render_td: 0,
            vertical: FrameStats::new(),
            horizontal: FrameStats::new(),
            pre_horizontal_render_td: 0,
            pre_horizontal_display_calls: 0,
            pre_horizontal_display_bytes: 0,
            fling_started: None,
            last_frame: None,
            scroll_row: 0,
            scroll_col: 0,
            load_started: None,
            status: "warming up…".into(),
            quit_scheduled: false,
        };
        app.schedule_tick(window, cx);
        app
    }

    fn schedule_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.on_next_frame(window, |this, window, cx| {
            this.on_frame(window, cx);
        });
    }

    fn on_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let now = Instant::now();
        if let Some(prev) = self.last_frame {
            let dt = now.saturating_duration_since(prev);
            match self.phase {
                Phase::VerticalFling => self.vertical.push(dt),
                Phase::HorizontalFling => self.horizontal.push(dt),
                _ => {}
            }
        }
        self.last_frame = Some(now);

        match self.phase {
            Phase::WaitingFirstPaint => {
                self.phase = Phase::LoadAndMeasureFirstFrame;
                self.status = "loading ResultWindow into DataTable…".into();
                let before_td = self.counters.render_td.load(Ordering::Relaxed);
                self.load_started = Some(Instant::now());
                let columns = self.columns.clone();
                self.grid.update(cx, |state, cx| {
                    let delegate = state.delegate_mut();
                    delegate.begin(columns);
                    // Seed the first viewport pages before paint.
                    let needed = delegate.result.touch(0..32);
                    delegate.fill_missing(&needed);
                    state.refresh(cx);
                    cx.notify();
                });
                self.after_load_rss_kb = current_rss_kb();
                self.first_frame_render_td = before_td;
                cx.notify();
            }
            Phase::LoadAndMeasureFirstFrame => {
                let started = self.load_started.expect("load started");
                self.load_to_first_frame_ms = started.elapsed().as_secs_f64() * 1000.0;
                let td_now = self.counters.render_td.load(Ordering::Relaxed);
                self.first_frame_render_td = td_now.saturating_sub(self.first_frame_render_td);
                self.after_load_rss_kb = current_rss_kb();
                let pages = self.grid.read(cx).delegate().result.loaded_page_count();
                eprintln!(
                    "grid_bench: first frame after load in {:.1} ms (render_td={}, pages={}) RSS {:.1} MiB",
                    self.load_to_first_frame_ms,
                    self.first_frame_render_td,
                    pages,
                    self.after_load_rss_kb as f64 / 1024.0
                );
                self.phase = Phase::VerticalFling;
                self.fling_started = Some(Instant::now());
                self.last_frame = None;
                self.scroll_row = 0;
                self.status = "vertical fling…".into();
                self.advance_vertical(cx);
                cx.notify();
            }
            Phase::VerticalFling => {
                let started = self.fling_started.expect("fling started");
                if started.elapsed().as_secs_f64() >= VERTICAL_FLING_SECS {
                    self.phase = Phase::HorizontalFling;
                    self.fling_started = Some(Instant::now());
                    self.last_frame = None;
                    self.scroll_col = 0;
                    self.status = "horizontal fling…".into();
                    self.grid.update(cx, |state, cx| {
                        state.scroll_to_row(0, cx);
                        state.scroll_to_col(0, cx);
                        let needed = state.delegate_mut().result.touch(0..32);
                        state.delegate_mut().fill_missing(&needed);
                    });
                    self.pre_horizontal_render_td = self.counters.render_td.load(Ordering::Relaxed);
                    self.pre_horizontal_display_calls =
                        self.counters.display_calls.load(Ordering::Relaxed);
                    self.pre_horizontal_display_bytes =
                        self.counters.display_bytes.load(Ordering::Relaxed);
                    self.counters.render_td.store(0, Ordering::Relaxed);
                    self.counters.display_calls.store(0, Ordering::Relaxed);
                    self.counters.display_bytes.store(0, Ordering::Relaxed);
                    eprintln!(
                        "grid_bench: vertical fling done ({} frames, p50={:.1} ms, pages_inserted={})",
                        self.vertical.samples.len(),
                        self.vertical.percentile_ms(50.0),
                        self.counters.pages_inserted.load(Ordering::Relaxed)
                    );
                } else {
                    self.advance_vertical(cx);
                }
            }
            Phase::HorizontalFling => {
                let started = self.fling_started.expect("fling started");
                if started.elapsed().as_secs_f64() >= HORIZONTAL_FLING_SECS {
                    self.phase = Phase::Done;
                    self.status = "done".into();
                    self.finish(cx);
                    return;
                }
                self.advance_horizontal(cx);
            }
            Phase::Done => return,
        }

        self.schedule_tick(window, cx);
        cx.notify();
    }

    fn advance_vertical(&mut self, cx: &mut Context<Self>) {
        self.scroll_row = (self.scroll_row + ROW_STEP).min(ROWS.saturating_sub(1));
        let row = self.scroll_row;
        self.grid.update(cx, |state, cx| {
            state.scroll_to_row(row, cx);
            // Ensure pages around the jump target are resident (mirrors production touch).
            let start = row.saturating_sub(16);
            let end = (row + 48).min(ROWS);
            let needed = state.delegate_mut().result.touch(start..end);
            state.delegate_mut().fill_missing(&needed);
        });
        if self.scroll_row >= ROWS.saturating_sub(1) {
            self.scroll_row = 0;
        }
    }

    fn advance_horizontal(&mut self, cx: &mut Context<Self>) {
        self.scroll_col = (self.scroll_col + COL_STEP).min(COLS.saturating_sub(1));
        let col = self.scroll_col;
        self.grid.update(cx, |state, cx| {
            state.scroll_to_col(col, cx);
        });
        if self.scroll_col >= COLS.saturating_sub(1) {
            self.scroll_col = 0;
        }
    }

    fn finish(&mut self, cx: &mut Context<Self>) {
        if self.quit_scheduled {
            return;
        }
        self.quit_scheduled = true;

        let loaded_pages = self.grid.read(cx).delegate().result.loaded_page_count();
        let end_rss_kb = current_rss_kb();
        let visible = self.grid.read(cx).visible_range();
        let vis_rows = visible.rows().clone();
        let vis_cols = visible.cols().clone();
        let (cb_rows_lo, cb_rows_hi) = *self.counters.last_visible_rows.lock().unwrap();
        let (cb_cols_lo, cb_cols_hi) = *self.counters.last_visible_cols.lock().unwrap();

        let horiz_td = self.counters.render_td.load(Ordering::Relaxed);
        let horiz_disp = self.counters.display_calls.load(Ordering::Relaxed);
        let horiz_bytes = self.counters.display_bytes.load(Ordering::Relaxed);
        let report = BenchReport {
            kind: self.kind.label().to_string(),
            rows: ROWS,
            cols: COLS,
            setup_ms: self.setup_ms,
            baseline_rss_mib: self.baseline_rss_kb as f64 / 1024.0,
            after_load_rss_mib: self.after_load_rss_kb as f64 / 1024.0,
            end_rss_mib: end_rss_kb as f64 / 1024.0,
            load_to_first_frame_ms: self.load_to_first_frame_ms,
            first_frame_render_td: self.first_frame_render_td,
            loaded_pages_at_end: loaded_pages,
            pages_inserted: self.counters.pages_inserted.load(Ordering::Relaxed),
            max_pages: ROW_WINDOW_MAX_PAGES,
            render_td_total: self.pre_horizontal_render_td + horiz_td,
            display_calls: self.pre_horizontal_display_calls + horiz_disp,
            display_bytes: self.pre_horizontal_display_bytes + horiz_bytes,
            horizontal_render_td: horiz_td,
            horizontal_display_calls: horiz_disp,
            horizontal_display_bytes: horiz_bytes,
            max_visible_rows: self.counters.max_visible_rows.load(Ordering::Relaxed),
            max_visible_cols: self.counters.max_visible_cols.load(Ordering::Relaxed),
            visible_rows_callbacks: self.counters.visible_rows_callbacks.load(Ordering::Relaxed),
            visible_cols_callbacks: self.counters.visible_cols_callbacks.load(Ordering::Relaxed),
            final_visible_rows: format!("{}..{}", vis_rows.start, vis_rows.end),
            final_visible_cols: format!("{}..{}", vis_cols.start, vis_cols.end),
            callback_visible_rows: format!("{cb_rows_lo}..{cb_rows_hi}"),
            callback_visible_cols: format!("{cb_cols_lo}..{cb_cols_hi}"),
            columns_virtualized: self.counters.max_visible_cols.load(Ordering::Relaxed)
                < COLS as u64
                && self.counters.max_visible_cols.load(Ordering::Relaxed) > 0,
            vertical_frames: self.vertical.samples.len(),
            vertical_p50_ms: self.vertical.percentile_ms(50.0),
            vertical_p95_ms: self.vertical.percentile_ms(95.0),
            vertical_p99_ms: self.vertical.percentile_ms(99.0),
            vertical_fps: self.vertical.effective_fps(),
            horizontal_frames: self.horizontal.samples.len(),
            horizontal_p50_ms: self.horizontal.percentile_ms(50.0),
            horizontal_p95_ms: self.horizontal.percentile_ms(95.0),
            horizontal_p99_ms: self.horizontal.percentile_ms(99.0),
            horizontal_fps: self.horizontal.effective_fps(),
        };

        print_report(&report);
        let _ = cx;
        process::exit(0);
    }
}

impl Render for BenchApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .child(
                div()
                    .px_3()
                    .py_2()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "Sela grid_bench — {} — {}",
                        self.kind.label(),
                        self.status
                    )),
            )
            .child(
                div()
                    .id("bench-grid")
                    .flex_1()
                    .w_full()
                    .child(DataTable::new(&self.grid).stripe(true).bordered(true)),
            )
    }
}

#[derive(Debug)]
struct BenchReport {
    kind: String,
    rows: usize,
    cols: usize,
    setup_ms: f64,
    baseline_rss_mib: f64,
    after_load_rss_mib: f64,
    /// RSS after flings with a full resident window (`loaded_at_end` pages).
    end_rss_mib: f64,
    load_to_first_frame_ms: f64,
    first_frame_render_td: u64,
    loaded_pages_at_end: usize,
    pages_inserted: u64,
    max_pages: usize,
    render_td_total: u64,
    display_calls: u64,
    display_bytes: u64,
    horizontal_render_td: u64,
    horizontal_display_calls: u64,
    horizontal_display_bytes: u64,
    max_visible_rows: u64,
    max_visible_cols: u64,
    visible_rows_callbacks: u64,
    visible_cols_callbacks: u64,
    final_visible_rows: String,
    final_visible_cols: String,
    callback_visible_rows: String,
    callback_visible_cols: String,
    columns_virtualized: bool,
    vertical_frames: usize,
    vertical_p50_ms: f64,
    vertical_p95_ms: f64,
    vertical_p99_ms: f64,
    vertical_fps: f64,
    horizontal_frames: usize,
    horizontal_p50_ms: f64,
    horizontal_p95_ms: f64,
    horizontal_p99_ms: f64,
    horizontal_fps: f64,
}

fn print_report(r: &BenchReport) {
    println!();
    println!("=== Sela grid_bench results ===");
    println!("dataset: {} ({} × {})", r.kind, r.rows, r.cols);
    println!(
        "model: ResultWindow max_pages={} page_size={}",
        r.max_pages, PAGE_SIZE
    );
    println!("setup_ms: {:.1}", r.setup_ms);
    println!(
        "rss_mib: baseline={:.1} after_load={:.1} delta={:.1} end={:.1} end_delta={:.1}",
        r.baseline_rss_mib,
        r.after_load_rss_mib,
        r.after_load_rss_mib - r.baseline_rss_mib,
        r.end_rss_mib,
        r.end_rss_mib - r.baseline_rss_mib
    );
    println!(
        "time_to_first_frame_ms: {:.1} (render_td on that frame: {})",
        r.load_to_first_frame_ms, r.first_frame_render_td
    );
    println!(
        "window: pages_inserted={} loaded_at_end={}",
        r.pages_inserted, r.loaded_pages_at_end
    );
    println!(
        "paint_calls: render_td={} display={} display_bytes={}",
        r.render_td_total, r.display_calls, r.display_bytes
    );
    println!(
        "horizontal_paint: render_td={} display={} display_bytes={}",
        r.horizontal_render_td, r.horizontal_display_calls, r.horizontal_display_bytes
    );
    println!(
        "visible: max_rows={} max_cols={} rows_cb={} cols_cb={}",
        r.max_visible_rows, r.max_visible_cols, r.visible_rows_callbacks, r.visible_cols_callbacks
    );
    println!(
        "visible_ranges: rows={} cols={} (callbacks rows={} cols={})",
        r.final_visible_rows,
        r.final_visible_cols,
        r.callback_visible_rows,
        r.callback_visible_cols
    );
    println!("columns_virtualized: {}", r.columns_virtualized);
    println!(
        "vertical_fling: frames={} p50={:.2}ms p95={:.2}ms p99={:.2}ms fps={:.1}",
        r.vertical_frames, r.vertical_p50_ms, r.vertical_p95_ms, r.vertical_p99_ms, r.vertical_fps
    );
    println!(
        "horizontal_fling: frames={} p50={:.2}ms p95={:.2}ms p99={:.2}ms fps={:.1}",
        r.horizontal_frames,
        r.horizontal_p50_ms,
        r.horizontal_p95_ms,
        r.horizontal_p99_ms,
        r.horizontal_fps
    );
    println!("=== end ===");
}

fn make_columns(cols: usize, kind: DatasetKind) -> Vec<ColumnMeta> {
    let type_names = [
        "int8",
        "numeric",
        "text",
        "timestamptz",
        "uuid",
        "bool",
        "float8",
        "jsonb",
        "date",
        "text",
    ];
    (0..cols)
        .map(|i| {
            let (name, type_name) = if kind.is_wide() && i == cols - 2 {
                ("wide_text_1k".into(), "text".into())
            } else if kind.is_wide() && i == cols - 1 {
                ("wide_json_10k".into(), "jsonb".into())
            } else {
                (format!("c{i}"), type_names[i % type_names.len()].into())
            };
            ColumnMeta { name, type_name }
        })
        .collect()
}

fn unique_text_1k(row: usize) -> Arc<str> {
    // Distinct content per row so Arc cannot coalesce; keep length ≈ 1 KiB.
    let prefix = format!("W{row:08}-");
    let fill = 1024usize.saturating_sub(prefix.len());
    Arc::from(format!("{prefix}{}", "x".repeat(fill)))
}

fn unique_json_10k(row: usize, template: &str) -> Arc<str> {
    // Prefix with a unique key so each cell is a distinct allocation (~10 KiB).
    Arc::from(format!("{{\"row\":{row},\"payload\":{template}}}"))
}

fn generate_page(
    page: usize,
    page_size: usize,
    cols: usize,
    kind: DatasetKind,
    wide: &WidePayloads,
) -> Vec<Vec<Value>> {
    let start = page * page_size;
    let end = (start + page_size).min(ROWS);
    let mut out = Vec::with_capacity(end.saturating_sub(start));
    for r in start..end {
        let mut row = Vec::with_capacity(cols);
        for c in 0..cols {
            if kind.is_wide() && c == cols - 2 {
                let text = match kind {
                    DatasetKind::WideUnique => unique_text_1k(r),
                    _ => wide.text_1k.clone(),
                };
                row.push(Value::Text(text));
                continue;
            }
            if kind.is_wide() && c == cols - 1 {
                let json = match kind {
                    DatasetKind::WideUnique => unique_json_10k(r, wide.json_10k.as_ref()),
                    _ => wide.json_10k.clone(),
                };
                row.push(Value::Json(json));
                continue;
            }
            row.push(cell_for(r, c));
        }
        out.push(row);
    }
    out
}

fn cell_for(row: usize, col: usize) -> Value {
    let mix = (row as u64)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(col as u64);
    match col % 10 {
        0 => {
            if mix % 17 == 0 {
                Value::Null
            } else {
                Value::Int((mix as i64) % 1_000_000 - 500_000)
            }
        }
        1 => Value::Decimal(Decimal::new((mix % 10_000_000) as i64, 3)),
        2 => Value::text(format!("r{row}-c{col}-{:x}", mix & 0xffff)),
        3 => {
            let day = (mix % 28) as u32 + 1;
            let hour = (mix % 24) as u32;
            let min = (mix % 60) as u32;
            Value::Timestamp(
                NaiveDate::from_ymd_opt(2020 + (mix % 5) as i32, ((mix % 12) as u32) + 1, day)
                    .unwrap_or_else(|| NaiveDate::from_ymd_opt(2024, 1, 1).unwrap())
                    .and_hms_opt(hour, min, 0)
                    .unwrap(),
            )
        }
        4 => {
            let mut bytes = [0u8; 16];
            bytes[..8].copy_from_slice(&mix.to_le_bytes());
            bytes[8..].copy_from_slice(&(row as u64).to_le_bytes());
            Value::Uuid(Uuid::from_bytes(bytes))
        }
        5 => Value::Bool(mix % 2 == 0),
        6 => Value::Float((mix % 10_000) as f64 / 100.0),
        7 => Value::json(&serde_json::json!({"row": row, "col": col, "v": mix % 1000})),
        8 => Value::Date(
            NaiveDate::from_ymd_opt(2018, 3, 1 + (mix % 28) as u32)
                .unwrap_or_else(|| NaiveDate::from_ymd_opt(2018, 3, 1).unwrap()),
        ),
        _ => Value::Time(
            NaiveTime::from_hms_opt((mix % 24) as u32, (mix % 60) as u32, (mix % 60) as u32)
                .unwrap_or_else(|| NaiveTime::from_hms_opt(0, 0, 0).unwrap()),
        ),
    }
}

fn current_rss_kb() -> u64 {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return 0;
    };
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let kb: u64 = rest
                .split_whitespace()
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            return kb;
        }
    }
    0
}

fn main() {
    if env::var_os("VK_ICD_FILENAMES").is_none() {
        let lvp = "/usr/share/vulkan/icd.d/lvp_icd.json";
        if std::path::Path::new(lvp).exists() {
            // SAFETY: single-threaded before GPUI starts; setting process env for the ICD.
            unsafe { env::set_var("VK_ICD_FILENAMES", lvp) };
        }
    }

    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(|cx| {
            gpui_kit::init(cx);
            Theme::change(ThemeMode::Dark, None, cx);

            let bounds = Bounds::centered(None, size(px(1280.), px(800.)), cx);
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some("Sela grid_bench".into()),
                        ..Default::default()
                    }),
                    window_min_size: Some(size(px(800.), px(500.))),
                    ..Default::default()
                },
                cx,
                |window, cx| cx.new(|cx| BenchApp::new(window, cx)),
            )
            .expect("failed to open grid_bench window");
        });
}
