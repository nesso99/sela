//! Microbench for `Value::display()` cost on narrow vs wide cells.
//!
//! Run (release):
//! ```text
//! cargo bench -p sela-core --bench value_display
//! ```

use std::hint::black_box;
use std::time::Instant;

use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use rust_decimal::Decimal;
use sela_core::Value;
use uuid::Uuid;

fn sample_narrow() -> Vec<Value> {
    let ts = NaiveDate::from_ymd_opt(2024, 6, 15)
        .unwrap()
        .and_hms_opt(12, 30, 45)
        .unwrap();
    vec![
        Value::Null,
        Value::Bool(true),
        Value::Int(-42_001),
        Value::UInt(99_999),
        Value::Float(std::f64::consts::PI),
        Value::Decimal(Decimal::new(123456789, 4)),
        Value::text("bookmark-title-42"),
        Value::Uuid(Uuid::nil()),
        Value::Date(NaiveDate::from_ymd_opt(2024, 1, 2).unwrap()),
        Value::Time(NaiveTime::from_hms_opt(3, 4, 5).unwrap()),
        Value::Timestamp(ts),
        Value::json(&serde_json::json!({"k": 1, "ok": true})),
    ]
}

fn sample_wide_text_1k() -> Value {
    Value::text("x".repeat(1024))
}

fn sample_wide_text_10k() -> Value {
    Value::text("y".repeat(10 * 1024))
}

fn sample_wide_json_multiline() -> Value {
    // ~10 KB of pretty-printed JSON (multi-line), stored as raw Arc<str>.
    let mut items = Vec::with_capacity(200);
    for i in 0..200 {
        items.push(serde_json::json!({
            "id": i,
            "name": format!("item-{i}"),
            "note": "lorem ipsum dolor sit amet, consectetur adipiscing elit",
            "tags": ["a", "b", "c", "d"],
        }));
    }
    Value::json(&serde_json::Value::Array(items))
}

fn time_display(label: &str, values: &[Value], iters: u32) {
    // Warmup.
    for _ in 0..1_000 {
        for v in values {
            black_box(v.display());
        }
    }

    let start = Instant::now();
    for _ in 0..iters {
        for v in values {
            black_box(v.display());
        }
    }
    let elapsed = start.elapsed();
    let calls = iters as u128 * values.len() as u128;
    let ns_per = elapsed.as_nanos() / calls.max(1);
    println!(
        "{label}: {calls} calls in {elapsed:?} ({ns_per} ns/call, n={})",
        values.len()
    );
}

fn time_single(label: &str, value: &Value, iters: u32) {
    for _ in 0..1_000 {
        black_box(value.display());
    }
    let start = Instant::now();
    for _ in 0..iters {
        black_box(value.display());
    }
    let elapsed = start.elapsed();
    let ns_per = elapsed.as_nanos() / iters as u128;
    let preview = value.display();
    let preview_len = preview.len();
    println!("{label}: {iters} calls in {elapsed:?} ({ns_per} ns/call, display_len={preview_len})");
}

fn main() {
    println!("Value::display() microbench (release recommended)\n");

    let narrow = sample_narrow();
    time_display("narrow mix", &narrow, 200_000);

    let text_1k = sample_wide_text_1k();
    time_single("Text 1 KiB (borrowed)", &text_1k, 500_000);

    let text_10k = sample_wide_text_10k();
    time_single("Text 10 KiB (borrowed)", &text_10k, 200_000);

    let json_wide = sample_wide_json_multiline();
    time_single("Json ~10 KiB (borrowed raw)", &json_wide, 200_000);

    // Simulate the grid path: clone Value then display().into_owned().
    let start = Instant::now();
    let iters = 50_000u32;
    for _ in 0..iters {
        let cell = text_1k.clone();
        black_box(cell.display().into_owned());
    }
    let elapsed = start.elapsed();
    println!(
        "grid path Text 1 KiB (clone + display + into_owned): {iters} in {elapsed:?} ({} ns/call)",
        elapsed.as_nanos() / iters as u128
    );

    let start = Instant::now();
    let iters = 2_000u32;
    for _ in 0..iters {
        let cell = json_wide.clone();
        black_box(cell.display().into_owned());
    }
    let elapsed = start.elapsed();
    println!(
        "grid path Json ~10 KiB (clone + display + into_owned): {iters} in {elapsed:?} ({} ns/call)",
        elapsed.as_nanos() / iters as u128
    );

    // Timestamp formatting allocates every time (Owned).
    let ts = Value::Timestamp(
        NaiveDateTime::parse_from_str("2024-06-15T12:30:45", "%Y-%m-%dT%H:%M:%S").unwrap(),
    );
    time_single("Timestamp (owned format)", &ts, 500_000);
}
