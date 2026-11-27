//! Bridge sqlx's tokio runtime to GPUI.
//!
//! GPUI polls futures on its own executor, which is not a tokio runtime.
//! `sqlx` pools panic if they are used outside tokio. [`drive`] ships the
//! future onto a process-wide tokio runtime and hands back a normal future
//! the GPUI executor can poll without blocking the UI thread.
//!
//! Runtime build failures and panicking/failed tasks surface as
//! [`DbError::Internal`] instead of aborting the process.

use std::future::Future;
use std::sync::OnceLock;

use sela_core::DbError;
use tokio::runtime::{Handle, Runtime};

static RUNTIME: OnceLock<Result<Runtime, String>> = OnceLock::new();

fn runtime_handle() -> Result<Handle, DbError> {
    match RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .thread_name("sela-db")
            .build()
            .map_err(|err| err.to_string())
    }) {
        Ok(runtime) => Ok(runtime.handle().clone()),
        Err(message) => Err(DbError::internal(format!(
            "failed to start the Sela database runtime: {message}"
        ))),
    }
}

/// Run `work` on the database runtime.
///
/// The returned future completes when the tokio task completes. Awaiting it
/// from a GPUI task yields instead of blocking the UI thread.
///
/// If the background task panics or is cancelled, the join failure becomes
/// [`DbError::Internal`] so the UI can show an error and keep running.
pub async fn drive<T>(
    work: impl Future<Output = Result<T, DbError>> + Send + 'static,
) -> Result<T, DbError>
where
    T: Send + 'static,
{
    let handle = runtime_handle()?;
    match handle.spawn(work).await {
        Ok(result) => result,
        Err(join_err) => {
            let message = if join_err.is_panic() {
                let cause = join_err.into_panic();
                let detail = panic_payload_message(cause.as_ref());
                format!("database task panicked: {detail}")
            } else if join_err.is_cancelled() {
                "database task was cancelled".into()
            } else {
                format!("database task failed: {join_err}")
            };
            Err(DbError::internal(message))
        }
    }
}

fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "unknown panic payload".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_runs_work_on_the_database_runtime() {
        let value = std::thread::spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("test runtime")
                .block_on(async { drive(async { Ok::<_, DbError>(2 + 2) }).await })
        })
        .join()
        .expect("bridge thread")
        .expect("drive ok");
        assert_eq!(value, 4);
    }

    #[test]
    fn panicking_task_becomes_internal_error() {
        let result = std::thread::spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("test runtime")
                .block_on(async {
                    drive(async {
                        panic!("intentional test panic");
                        #[allow(unreachable_code)]
                        Ok::<(), DbError>(())
                    })
                    .await
                })
        })
        .join()
        .expect("bridge thread must not panic");

        match result {
            Err(DbError::Internal { message, .. }) => {
                assert!(
                    message.contains("intentional test panic") || message.contains("panicked"),
                    "unexpected message: {message}"
                );
            }
            other => panic!("expected Internal error, got {other:?}"),
        }
    }
}

/// Connection-level failures that a second attempt may clear.
fn is_transient(err: &DbError) -> bool {
    use DbError;
    matches!(err, DbError::Connection { .. } | DbError::Timeout { .. })
}

/// Run `op` up to `attempts` times, retrying only transient errors.
pub async fn retry_transient<T, F, Fut>(
    attempts: usize,
    delay: std::time::Duration,
    mut op: F,
) -> Result<T, DbError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, DbError>>,
{
    let mut tried = 0;
    loop {
        tried += 1;
        match op().await {
            Err(err) if tried < attempts && is_transient(&err) => {
                tokio::time::sleep(delay * tried as u32).await;
            }
            other => return other,
        }
    }
}

#[cfg(test)]
mod retry_tests {
    use super::*;

    #[tokio::test]
    async fn retry_transient_retries_connection_errors_only() {
        use std::cell::Cell;
        let calls = Cell::new(0);
        let out = retry_transient(3, std::time::Duration::ZERO, || {
            calls.set(calls.get() + 1);
            let n = calls.get();
            async move {
                if n < 3 {
                    Err(DbError::connection("reset"))
                } else {
                    Ok(n)
                }
            }
        })
        .await;
        assert_eq!(out.unwrap(), 3);

        let calls = Cell::new(0);
        let out: Result<(), _> = retry_transient(3, std::time::Duration::ZERO, || {
            calls.set(calls.get() + 1);
            async {
                Err(DbError::Query {
                    message: "no such table".into(),
                    sqlstate: None,
                    vendor_code: None,
                })
            }
        })
        .await;
        assert!(out.is_err());
        assert_eq!(calls.get(), 1);
    }
}
