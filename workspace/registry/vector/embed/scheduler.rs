//! `EmbedScheduler` — priority queue + batch coalescing over one embedder
//! (09c §4.3 threading model).
//!
//! ```text
//! callers ── EmbedHandle::embed(key, text, role, priority, cancel)
//!     │            (bounded mpsc = backpressure)
//!     ▼
//! worker task: coalesce ~75 ms ──► pop highest-priority same-role batch ≤ 32
//!     ▼
//! Embedder::embed_batch  (the runtime spawn_blocks into ORT internally)
//!     ▼
//! per-job oneshot replies
//! ```
//!
//! Rules (09c §4.3): interactive query > open-file symbols > background cold
//! index; batches coalesce during bulk commits; cancelled job groups are
//! dropped without touching the model.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use heart::ContentHash;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;
use crate::vector::core::{EmbedError, EmbedRole, Embedder, Embedding, EmbeddingModel};

use super::MAX_BATCH;

/// Job priority, highest first when draining. Derived `Ord`: variants are
/// declared low→high so `Interactive > OpenFile > Background`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Priority {
	/// Cold-index / delta re-embed work.
	Background,
	/// Symbols of the file the user has open.
	OpenFile,
	/// A user query waiting on screen.
	Interactive,
}

impl Priority {
	const ALL_DESC: [Priority; 3] = [Priority::Interactive, Priority::OpenFile, Priority::Background];
}

/// Cooperative cancellation for a group of jobs (e.g. one EmbedStage run).
/// Cancelled jobs are dropped before inference; their callers observe
/// [`SchedulerError::Cancelled`].
#[derive(Debug, Clone, Default)]
pub struct CancelGroup(Arc<AtomicBool>);

impl CancelGroup {
	pub fn new() -> Self { Self::default() }

	pub fn cancel(&self) { self.0.store(true, Ordering::Release); }

	pub fn is_cancelled(&self) -> bool { self.0.load(Ordering::Acquire) }
}

/// Scheduler tuning (09c §4.3 / §8.3).
#[derive(Debug, Clone)]
pub struct SchedulerConfig {
	/// How long to keep gathering same-priority jobs before running a batch.
	pub coalesce_window: Duration,
	/// Hard batch cap — never above [`MAX_BATCH`] (I16).
	pub max_batch: usize,
	/// Bound of the submission channel; senders block when full (backpressure).
	pub queue_bound: usize,
}

impl Default for SchedulerConfig {
	fn default() -> Self {
		Self {
			coalesce_window: Duration::from_millis(75),
			max_batch: MAX_BATCH,
			queue_bound: 1024,
		}
	}
}

/// Why an embed request did not produce a vector.
#[derive(Debug, thiserror::Error)]
pub enum SchedulerError {
	#[error("embed job cancelled")]
	Cancelled,
	#[error("embed scheduler shut down")]
	Closed,
	#[error(transparent)]
	Embed(#[from] EmbedError),
}

struct Job<M: EmbeddingModel> {
	key: ContentHash,
	text: String,
	role: EmbedRole,
	priority: Priority,
	cancel: CancelGroup,
	reply: oneshot::Sender<Result<Embedding<M>, EmbedError>>,
}

/// Cloneable submission handle. Dropping every handle shuts the worker down
/// after it drains its queue.
pub struct EmbedHandle<M: EmbeddingModel> {
	tx: mpsc::Sender<Job<M>>,
}

impl<M: EmbeddingModel> Clone for EmbedHandle<M> {
	fn clone(&self) -> Self { Self { tx: self.tx.clone() } }
}

impl<M: EmbeddingModel + Send + 'static> EmbedHandle<M> {
	/// Submit one text and await its embedding. `key` is carried for tracing
	/// only — dedup/cutoff lives in `EmbedStage`, not here.
	pub async fn embed(
		&self,
		key: ContentHash,
		text: String,
		role: EmbedRole,
		priority: Priority,
		cancel: CancelGroup,
	) -> Result<Embedding<M>, SchedulerError> {
		let (reply, rx) = oneshot::channel();
		let job = Job { key, text, role, priority, cancel: cancel.clone(), reply };
		self.tx.send(job).await.map_err(|_| SchedulerError::Closed)?;
		match rx.await {
			Ok(Ok(embedding)) => Ok(embedding),
			Ok(Err(error)) => Err(SchedulerError::Embed(error)),
			// The worker dropped the reply: either the job group was
			// cancelled, or the scheduler shut down mid-flight.
			Err(_) if cancel.is_cancelled() => Err(SchedulerError::Cancelled),
			Err(_) => Err(SchedulerError::Closed),
		}
	}
}

/// The scheduler: spawn once per embedder, keep the handle.
pub struct EmbedScheduler;

impl EmbedScheduler {
	/// Spawn the worker task on the current tokio runtime and return the
	/// submission handle.
	pub fn spawn<E>(embedder: Arc<E>, config: SchedulerConfig) -> EmbedHandle<E::Model>
	where
		E: Embedder + Send + Sync + 'static,
		E::Model: Send + 'static,
	{
		let max_batch = config.max_batch.min(MAX_BATCH);
		let (tx, rx) = mpsc::channel(config.queue_bound);
		tokio::spawn(worker(embedder, rx, config.coalesce_window, max_batch));
		EmbedHandle { tx }
	}
}

async fn worker<E>(
	embedder: Arc<E>,
	mut rx: mpsc::Receiver<Job<E::Model>>,
	coalesce_window: Duration,
	max_batch: usize,
) where
	E: Embedder + Send + Sync + 'static,
	E::Model: EmbeddingModel + Send + 'static,
{
	// One queue per priority, FIFO within a priority.
	let mut queues: [VecDeque<Job<E::Model>>; 3] = Default::default();

	while let Some(job) = rx.recv().await {
		enqueue(&mut queues, job);
		coalesce(&mut rx, &mut queues, coalesce_window).await;
		drain(&embedder, &mut rx, &mut queues, max_batch).await;
	}
	// Channel closed: run whatever is left.
	drain(&embedder, &mut rx, &mut queues, max_batch).await;
}

fn enqueue<M: EmbeddingModel>(queues: &mut [VecDeque<Job<M>>; 3], job: Job<M>) {
	queues[job.priority as usize].push_back(job);
}

/// Keep admitting jobs until the coalescing window elapses (or the channel
/// closes), so bulk submissions form full batches instead of singletons.
async fn coalesce<M: EmbeddingModel>(
	rx: &mut mpsc::Receiver<Job<M>>,
	queues: &mut [VecDeque<Job<M>>; 3],
	window: Duration,
) {
	let deadline = Instant::now() + window;
	loop {
		match tokio::time::timeout_at(deadline, rx.recv()).await {
			Ok(Some(job)) => enqueue(queues, job),
			Ok(None) | Err(_) => return,
		}
	}
}

/// Run every queued job, highest priority first, in same-role batches capped
/// at `max_batch`. Between batches, newly arrived jobs are admitted without
/// waiting, so an interactive query can overtake a long background drain.
async fn drain<E>(
	embedder: &Arc<E>,
	rx: &mut mpsc::Receiver<Job<E::Model>>,
	queues: &mut [VecDeque<Job<E::Model>>; 3],
	max_batch: usize,
) where
	E: Embedder + Send + Sync + 'static,
	E::Model: EmbeddingModel + Send + 'static,
{
	while let Some(batch) = pop_batch(queues, max_batch) {
		run_batch(embedder.as_ref(), batch).await;
		while let Ok(job) = rx.try_recv() {
			enqueue(queues, job);
		}
	}
}

/// Take up to `max_batch` non-cancelled jobs of one role from the front of
/// the highest non-empty priority queue. Cancelled jobs are discarded here
/// (dropping the reply sender), before any model work.
fn pop_batch<M: EmbeddingModel>(queues: &mut [VecDeque<Job<M>>; 3], max_batch: usize) -> Option<Vec<Job<M>>> {
	for priority in Priority::ALL_DESC {
		let queue = &mut queues[priority as usize];
		queue.retain(|job| !job.cancel.is_cancelled());

		let Some(front) = queue.front() else { continue };
		let role = front.role;

		let mut batch = Vec::new();
		let mut deferred = VecDeque::new();
		while let Some(job) = queue.pop_front() {
			if batch.len() < max_batch && job.role == role {
				batch.push(job);
			} else {
				deferred.push_back(job);
			}
		}
		*queue = deferred;
		return Some(batch);
	}
	None
}

async fn run_batch<E>(embedder: &E, batch: Vec<Job<E::Model>>)
where
	E: Embedder + Send + Sync,
	E::Model: EmbeddingModel + Send + 'static,
{
	debug_assert!(!batch.is_empty());
	let role = batch[0].role;
	let texts: Vec<&str> = batch.iter().map(|job| job.text.as_str()).collect();
	tracing::debug!(batch = texts.len(), ?role, first_key = %batch[0].key, "embed batch");

	match embedder.embed_batch(&texts, role).await {
		Ok(embeddings) => {
			if embeddings.len() != batch.len() {
				let msg = format!(
					"embedder returned {} vectors for {} texts",
					embeddings.len(),
					batch.len()
				);
				reply_all_err(batch, &msg);
				return;
			}
			for (job, embedding) in batch.into_iter().zip(embeddings) {
				let _ = job.reply.send(Ok(embedding));
			}
		}
		Err(error) => {
			let msg = format!("embed batch failed: {error}");
			tracing::warn!("{msg}");
			reply_all_err(batch, &msg);
		}
	}
}

fn reply_all_err<M: EmbeddingModel>(batch: Vec<Job<M>>, msg: &str) {
	for job in batch {
		let _ = job.reply.send(Err(super::backend_error(msg)));
	}
}
