//! Pluggable executor for blocking directory scans.
//!
//! [`ScanExecutor`] is the seam between the widget and whatever
//! runtime the host application is built on. By default the widget
//! uses [`ThreadExecutor`] (one `std::thread::spawn` per expansion),
//! which is correct but slightly wasteful for apps that already have
//! a blocking-task pool — tokio, smol, rayon, etc. Those apps can
//! implement this trait to route scans through their own pool.
//!
//! # Why a trait, not a feature flag
//!
//! Runtimes vary in their "how do I run blocking work" API, and
//! hard-coding any one of them would shut out the others. A trait
//! lets each application plug in exactly what fits — with zero
//! default dependencies on tokio, smol, or similar.
//!
//! # Example — tokio
//!
//! ```ignore
//! use std::sync::Arc;
//! use std::future::Future;
//! use std::pin::Pin;
//! use std::path::Path;
//! use iced_swdir_tree::{ScanExecutor, DirectoryTree};
//!
//! struct TokioExecutor;
//!
//! impl ScanExecutor for TokioExecutor {
//!     fn spawn_blocking(
//!         &self,
//!         job: Box<dyn FnOnce() -> Result<Vec<swdir::DirEntry>, swdir::ScanError> + Send>,
//!     ) -> Pin<Box<dyn Future<Output = Result<Vec<swdir::DirEntry>, swdir::ScanError>> + Send>> {
//!         Box::pin(async move {
//!             tokio::task::spawn_blocking(job)
//!                 .await
//!                 .expect("scan task panicked")
//!         })
//!     }
//! }
//!
//! let tree = DirectoryTree::new("/".into())
//!     .with_executor(Arc::new(TokioExecutor));
//! ```

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

/// The job passed to a [`ScanExecutor`] — always just "call
/// `swdir::scan_dir` on this path".
///
/// We pre-bake the path into the closure rather than exposing a
/// "scan this path" method on the trait, because trait methods
/// returning boxed futures must take a `'static` closure argument,
/// and smuggling the path via capture is the cleanest way to do that.
///
/// Kept as a type alias so the trait signature reads at a glance.
pub type ScanJob =
    Box<dyn FnOnce() -> Result<Vec<swdir::DirEntry>, swdir::ScanError> + Send + 'static>;

/// The future that runs a [`ScanJob`] to completion.
///
/// `Send + 'static` because the widget hands this off to
/// `iced::Task::perform`, which requires both.
pub type ScanFuture =
    Pin<Box<dyn Future<Output = Result<Vec<swdir::DirEntry>, swdir::ScanError>> + Send + 'static>>;

/// A pluggable executor for blocking `scan_dir` calls.
///
/// Applications that already manage a blocking-task pool can
/// implement this to route tree expansions through it instead of
/// spinning up a new `std::thread` per scan.
///
/// Implementors should ensure the returned future resolves once the
/// job has actually run — cancelling or losing the job will leave
/// the widget stuck in "loading" forever (`is_loaded` never flips
/// to `true` for the affected directory).
///
/// The widget holds the executor behind an `Arc`, so an impl that
/// owns any shared state should wrap it in an `Arc` internally or
/// store only `Send + Sync` references.
pub trait ScanExecutor: Send + Sync + 'static {
    /// Run `job` on a blocking-capable worker and return a future
    /// that resolves to its result.
    ///
    /// The future is driven from the iced runtime. It must be
    /// `Send + 'static` — iced spawns tasks across threads.
    fn spawn_blocking(&self, job: ScanJob) -> ScanFuture;
}

/// Default executor — one `std::thread::spawn` per scan.
///
/// This is what the widget uses if you never call
/// [`DirectoryTree::with_executor`](crate::DirectoryTree::with_executor).
/// It is completely runtime-agnostic: no tokio, no smol, no async.
///
/// Thread-spawn overhead is on the order of tens of microseconds —
/// usually negligible next to the `readdir` syscall the thread is
/// about to do — so this is a reasonable default even for apps
/// that could plug in something fancier.
#[derive(Debug, Clone, Copy, Default)]
pub struct ThreadExecutor;

impl ScanExecutor for ThreadExecutor {
    fn spawn_blocking(&self, job: ScanJob) -> ScanFuture {
        Box::pin(async move {
            // `thread::spawn + join` inside an `async move` block
            // is the standard trick for adapting a synchronous
            // blocking primitive to a runtime-agnostic future.
            std::thread::spawn(job).join().unwrap_or_else(|_| {
                // Worker panic (exceedingly rare — effectively
                // OOM-only in swdir's case). Fabricate an I/O
                // error so the widget sees a failed scan rather
                // than a runtime panic propagating. We construct
                // the enum variant directly because `ScanError::io`
                // is pub(crate) in swdir.
                Err(swdir::ScanError::Io {
                    path: PathBuf::new(),
                    source: std::io::Error::other("scan worker thread panicked"),
                })
            })
        })
    }
}

/// Convenience helper: run `path` through `executor` and get a future
/// of the scan result.
///
/// Crate-internal; used by the walker to keep the async plumbing in
/// one place regardless of executor choice.
pub(crate) fn run_scan(executor: &Arc<dyn ScanExecutor>, path: PathBuf) -> ScanFuture {
    let job: ScanJob = Box::new(move || swdir::scan_dir(&path as &Path));
    executor.spawn_blocking(job)
}
