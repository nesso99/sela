//! Bridge sqlx's tokio runtime to GPUI.
//!
//! GPUI polls futures on its own executor, which is not a tokio runtime.
//! `sqlx` pools panic if they are used outside tokio. [`drive`] ships the
//! future onto a process-wide tokio runtime and hands back a normal future
//! the GPUI executor can poll without blocking the UI thread.

use std::future::Future;
use std::sync::OnceLock;

use tokio::runtime::{Handle, Runtime};

static RUNTIME: OnceLock<Runtime> = OnceLock::new();

pub fn handle() -> Handle {
    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4)
                .enable_all()
                .thread_name("sela-db")
                .build()
                .expect("failed to start the Sela database runtime")
        })
        .handle()
        .clone()
}

/// Run `work` on the database runtime.
///
/// The returned future completes when the tokio task completes. Awaiting it
/// from a GPUI task yields instead of blocking the UI thread.
pub async fn drive<T>(work: impl Future<Output = T> + Send + 'static) -> T
where
    T: Send + 'static,
{
    match handle().spawn(work).await {
        Ok(value) => value,
        Err(err) => panic!("database task panicked: {err}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_runs_work_on_the_database_runtime() {
        let value =
            std::thread::spawn(|| handle().block_on(async { drive(async { 2 + 2 }).await }))
                .join()
                .expect("bridge thread");
        assert_eq!(value, 4);
    }
}
