//! Opt-in startup marker for `scripts/bench-nfr.sh` (M4-2).
//!
//! With `SELA_BENCH_STARTUP=1` the first workspace frame prints one stderr line
//! carrying the wall-clock time in nanoseconds since the Unix epoch. The script
//! subtracts its own spawn timestamp, so the figure covers process exec, GPU
//! and window setup, and the first paint. Without the variable this is a no-op.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static FIRST_FRAME_SEEN: AtomicBool = AtomicBool::new(false);

/// Line printed on the first frame; parsed by `scripts/bench-nfr.sh`.
pub(crate) fn marker_line(epoch_ns: u128) -> String {
    format!("sela-bench first_frame_epoch_ns={epoch_ns}")
}

/// Call from every `Workspace::render`; only the first call may print.
pub(crate) fn mark_first_frame() {
    if FIRST_FRAME_SEEN.load(Ordering::Relaxed) {
        return;
    }
    FIRST_FRAME_SEEN.store(true, Ordering::Relaxed);
    if std::env::var_os("SELA_BENCH_STARTUP").is_none() {
        return;
    }
    if let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) {
        eprintln!("{}", marker_line(now.as_nanos()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_line_format_is_stable() {
        assert_eq!(
            marker_line(1_700_000_000_123_456_789),
            "sela-bench first_frame_epoch_ns=1700000000123456789"
        );
    }
}
