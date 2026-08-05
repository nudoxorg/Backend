//! Cooperative cancellation for cage runs.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

/// Shared cancellation flag for in-flight cage work.
///
/// Cheap to clone. The VM cage honors cancel by killing the machine
/// (`VmHandle::kill` — the ephemeral overlay dies with it); the dev
/// passthrough supervisor honors it by writing `cgroup.kill` when a
/// fairness cgroup is owned, and worker slots kill the child process tree
/// the same way.
#[derive(Debug, Clone)]
pub struct CancelToken {
    inner: Arc<AtomicBool>,
}

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

impl CancelToken {
    /// Fresh token (not cancelled).
    pub fn new() -> Self {
        Self {
            inner: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Process-wide token that is never cancelled.
    ///
    /// Do **not** call [`cancel`](Self::cancel) on this handle — it is shared.
    pub fn never() -> Self {
        static NEVER: OnceLock<CancelToken> = OnceLock::new();
        NEVER
            .get_or_init(|| Self {
                // Shared flag that stays false for the process lifetime.
                inner: Arc::new(AtomicBool::new(false)),
            })
            .clone()
    }

    /// Request cancellation. Idempotent.
    pub fn cancel(&self) {
        self.inner.store(true, Ordering::SeqCst);
    }

    /// Whether cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.inner.load(Ordering::SeqCst)
    }
}
