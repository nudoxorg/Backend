//! The single-writer store actor (09c §1.1).
//!
//! `qdrant_edge::EdgeShard` is a synchronous, in-process engine that is not
//! proven `Sync` and whose native panics would otherwise kill the whole GUI
//! process. Both risks are contained the same way: the shard lives on **one
//! dedicated OS thread**, owned by a command loop, and every caller talks to
//! it through an async [`StoreHandle`] over an mpsc channel with oneshot
//! replies.
//!
//! - **Writes** are serialized by construction (single-writer actor).
//! - **Reads (searches) also go through the actor** — serialized reads are
//!   an accepted v1 simplification (the fan-out layer still runs *shards*
//!   in parallel, one actor each; per-shard read concurrency can come later
//!   without changing this API).
//! - **Panic containment:** each command is executed under
//!   [`std::panic::catch_unwind`]. A panic poisons the actor: the shard is
//!   deliberately leaked (its `Drop` flushes and could panic again over
//!   corrupted state), the channel is closed, and every subsequent or
//!   in-flight call observes [`StoreError::Closed`]. The process survives.
//! - **Graceful close:** dropping the last handle ends the loop; the shard
//!   is flushed and dropped (Edge flushes again on `Drop`).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::vector::core::StoreError;
use qdrant_edge::{
    CountRequest, EdgeShard, ScoredPoint, SearchRequest as EdgeSearchRequest, UpdateOperation,
};
use tokio::sync::{mpsc, oneshot};

use super::backend_error;

struct ActorCompletion {
    done: AtomicBool,
    notify: tokio::sync::Notify,
    error: Mutex<Option<String>>,
}

impl ActorCompletion {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            done: AtomicBool::new(false),
            notify: tokio::sync::Notify::new(),
            error: Mutex::new(None),
        })
    }

    fn finish(&self, error: Option<String>) {
        if let Some(error) = error {
            *self.error.lock().expect("actor completion mutex poisoned") = Some(error);
        }
        self.done.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }

    async fn wait(&self) -> Result<(), StoreError> {
        loop {
            let notified = self.notify.notified();
            if self.done.load(Ordering::Acquire) {
                break;
            }
            notified.await;
        }
        self
            .error
            .lock()
            .expect("actor completion mutex poisoned")
            .take()
            .map_or(Ok(()), |error| Err(backend_error(error)))
    }
}

/// Depth of the command queue; enough to absorb an ingest burst without
/// unbounded memory.
const COMMAND_QUEUE_DEPTH: usize = 256;

type Reply<T> = oneshot::Sender<Result<T, StoreError>>;

/// A command executed on the actor thread against the owned [`EdgeShard`].
enum Command {
    /// Any WAL-logged mutation: upsert, delete, payload-index creation.
    Update {
        op: UpdateOperation,
        reply: Reply<()>,
    },
    /// Nearest-neighbour search (reads are serialized through the actor;
    /// see module docs).
    Search {
        request: EdgeSearchRequest,
        reply: Reply<Vec<ScoredPoint>>,
    },
    Count {
        request: CountRequest,
        reply: Reply<usize>,
    },
    Flush {
        reply: Reply<()>,
    },
    /// One optimizer pass; `true` means work was done and another pass may
    /// find more.
    Optimize {
        reply: Reply<bool>,
    },
    /// Test hook: panic on the actor thread to exercise poisoning.
    #[doc(hidden)]
    Panic,
}

/// Async handle to a store actor. Cloneable; all clones talk to the same
/// serialized shard. When the last clone drops, the actor flushes and exits.
#[derive(Clone)]
pub struct StoreHandle {
    tx: mpsc::Sender<Command>,
    completion: Arc<ActorCompletion>,
}

impl StoreHandle {
    /// Spawn a dedicated actor thread and open the shard **on that thread**
    /// (`EdgeShard` is not `Send`; it must be born where it lives).
    ///
    /// `open` runs exactly once on the new thread; its error is relayed to
    /// the caller.
    pub async fn spawn<F>(name: &str, open: F) -> Result<Self, StoreError>
    where
        F: FnOnce() -> Result<EdgeShard, StoreError> + Send + 'static,
    {
        let (tx, rx) = mpsc::channel(COMMAND_QUEUE_DEPTH);
        let (ready_tx, ready_rx) = oneshot::channel();
        let completion = ActorCompletion::new();
        let actor_completion = Arc::clone(&completion);

        thread::Builder::new()
            .name(format!("vector-local-store:{name}"))
            .spawn(move || run(open, rx, ready_tx, actor_completion))
            .map_err(backend_error)?;

        // The thread reports Ok(()) once the shard is open, or the open error.
        ready_rx.await.map_err(|_| StoreError::Closed)??;
        Ok(Self { tx, completion })
    }

    pub async fn update(&self, op: UpdateOperation) -> Result<(), StoreError> {
        self.request(|reply| Command::Update { op, reply }).await
    }

    pub async fn search(&self, request: EdgeSearchRequest) -> Result<Vec<ScoredPoint>, StoreError> {
        self.request(|reply| Command::Search { request, reply })
            .await
    }

    pub async fn count(&self, request: CountRequest) -> Result<usize, StoreError> {
        self.request(|reply| Command::Count { request, reply })
            .await
    }

    pub async fn flush(&self) -> Result<(), StoreError> {
        self.request(|reply| Command::Flush { reply }).await
    }

    /// Flush and wait until the actor has dropped the Edge shard. Edge flushes
    /// again from `Drop`, so callers that remove the shard directory must not
    /// proceed after the explicit flush reply alone.
    pub async fn close(self) -> Result<(), StoreError> {
        self.flush().await?;
        let completion = Arc::clone(&self.completion);
        drop(self);
        completion.wait().await
    }

    /// One optimizer pass; returns whether anything was optimized.
    pub async fn optimize(&self) -> Result<bool, StoreError> {
        self.request(|reply| Command::Optimize { reply }).await
    }

    /// Test hook: induce a panic on the actor thread. After this resolves,
    /// every call on this handle returns [`StoreError::Closed`].
    #[doc(hidden)]
    pub async fn induce_panic(&self) {
        // The actor never replies to Panic; delivery is all we need.
        let _ = self.tx.send(Command::Panic).await;
        // Wait until the poisoned actor has closed the channel.
        self.tx.closed().await;
    }

    async fn request<T>(&self, build: impl FnOnce(Reply<T>) -> Command) -> Result<T, StoreError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(build(reply))
            .await
            .map_err(|_| StoreError::Closed)?;
        rx.await.map_err(|_| StoreError::Closed)?
    }
}

/// The actor loop. Runs on its own OS thread; owns the shard for its whole
/// life.
fn run<F>(
    open: F,
    mut rx: mpsc::Receiver<Command>,
    ready_tx: oneshot::Sender<Result<(), StoreError>>,
    completion: Arc<ActorCompletion>,
) where
    F: FnOnce() -> Result<EdgeShard, StoreError>,
{
    let shard = match open() {
        Ok(shard) => {
            let _ = ready_tx.send(Ok(()));
            shard
        }
        Err(err) => {
            let _ = ready_tx.send(Err(err));
            completion.finish(None);
            return;
        }
    };

    while let Some(command) = rx.blocking_recv() {
        let outcome = catch_unwind(AssertUnwindSafe(|| handle(&shard, command)));
        if outcome.is_err() {
            tracing::error!(
                shard = %shard.path().display(),
                "edge shard panicked; poisoning store actor (subsequent calls return Closed)"
            );
            // Poison: do NOT drop the shard (its Drop flushes and may panic
            // again over the state that just panicked). Leak it; the handle
            // side observes Closed via the dropped channel. Queued commands
            // are dropped with their reply senders → Closed for callers.
            rx.close();
            std::mem::forget(shard);
            completion.finish(Some("edge shard command panicked".to_owned()));
            return;
        }
    }

    // Graceful close: all handles dropped. Flush + drop; both are wrapped
    // because Edge's flush path panics (rather than returns) on IO failure.
    let drop_error = catch_unwind(AssertUnwindSafe(|| drop(shard))).err();
    if drop_error.is_some() {
        tracing::error!(
            "edge shard flush-on-close panicked; shard directory may need WAL recovery on next open"
        );
    }
    completion.finish(drop_error.map(|_| "edge shard flush-on-close panicked".to_owned()));
}

fn handle(shard: &EdgeShard, command: Command) {
    match command {
        Command::Update { op, reply } => {
            let _ = reply.send(shard.update(op).map_err(backend_error));
        }
        Command::Search { request, reply } => {
            let _ = reply.send(shard.search(request).map_err(backend_error));
        }
        Command::Count { request, reply } => {
            let _ = reply.send(shard.count(request).map_err(backend_error));
        }
        Command::Flush { reply } => {
            // EdgeShard::flush is infallible-by-signature (panics on IO
            // failure; caught at the loop boundary as poisoning).
            shard.flush();
            let _ = reply.send(Ok(()));
        }
        Command::Optimize { reply } => {
            let _ = reply.send(shard.optimize().map_err(backend_error));
        }
        Command::Panic => panic!("vector-local: induced store-actor panic (test hook)"),
    }
}
