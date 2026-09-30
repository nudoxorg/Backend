//! One off-thread cache for what a view needs and cannot compute in a frame
//! (W-Open I3).
//!
//! Five places had each written this by hand: a world, a release fixture, a
//! page's workspace lines, a package's source facts, a declaration's reach.
//! Each had its own eviction (none, none, FIFO, LRU), its own error handling
//! (none contained a panic), and each redrew every window when a value landed.
//! [`Memo`] is the one copy:
//!
//! - **Single-flight.** The first ask for a key starts its work on the
//!   background executor and answers [`Answer::Reading`]; every ask until it
//!   lands joins the one flight.
//! - **Bounded.** At most `capacity` keys are retained or in flight. If every
//!   slot is occupied by work, a new ask waits for capacity and its requester
//!   is notified to retry; the memo never starts untracked work.
//! - **Panic-safe.** The work runs under `catch_unwind`; a panic is one
//!   typed [`Fault`] on that key, not a dead worker and a page that never
//!   arrives (the rule `ReadPool` already keeps for its readers).
//! - **Targeted.** The value's landing notifies the views that asked
//!   ([`Asker::View`]), never every window. [`Asker::Everyone`] is the one
//!   fallback, for a caller whose signature does not know its view yet.
//! - **Visible.** [`in_flight`] counts every flight of every memo in the
//!   process, so a harness can wait for none before it captures.
//!
//! A value is shared as an `Arc`; the work is a pure `Fn(&K, &Cancellation) -> V`.

use gpui::{App, Context, EntityId};
use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::hash::Hash;
use std::num::NonZeroUsize;
use std::panic::AssertUnwindSafe;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

struct InFlight;

impl Drop for InFlight {
    fn drop(&mut self) {
        // A detached UI completion may be dropped during app shutdown. Keep
        // the process-wide harness count balanced on every exit path.
        IN_FLIGHT.fetch_sub(1, Ordering::Relaxed);
    }
}

/// How many values, in every memo in the process, are being computed or have
/// been computed but not yet announced to the views that asked. (A harness
/// waits for none before it captures; the product never asks.)
#[cfg(any(test, feature = "visual-harness"))]
#[must_use]
pub(crate) fn in_flight() -> usize {
    IN_FLIGHT.load(Ordering::Relaxed)
}

/// Who redraws when a value lands.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Asker {
    /// The one view that asked: only it is notified.
    #[allow(
        dead_code,
        reason = "built by `Memo::get` and `hand_view_for`, which the shell callers move to (MIGRATE.md, R-Open3); delete this allow with that move"
    )]
    View(EntityId),
    /// A caller that cannot name its view (a `&mut App` signature): every
    /// window redraws once. Migrate the caller to [`Memo::get`].
    Everyone,
}

/// Why a value is not there.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Fault {
    /// The work panicked, and said so.
    Panicked(Arc<str>),
}

impl fmt::Display for Fault {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Panicked(what) => write!(formatter, "the work panicked: {what}"),
        }
    }
}

/// What an ask gets.
#[derive(Debug)]
pub(crate) enum Answer<V> {
    /// The work is running; the asker is notified when it lands.
    Reading,
    /// The bounded memo is full of work. This key is not tracked yet; the
    /// requester will be notified when a slot opens and should ask again.
    Deferred,
    /// The value.
    Ready(Arc<V>),
    /// The work panicked. It stays this way until [`Memo::forget`].
    Failed(Fault),
}

/// A logical clock: which ask touched an entry last.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
struct Tick(u64);

impl Tick {
    const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

enum State<V> {
    Reading,
    Ready(Arc<V>),
    Failed(Fault),
}

struct Entry<V> {
    state: State<V>,
    used: Tick,
    /// Cache age begins when work completes, not when a slow read starts.
    completed_at: Option<Instant>,
    /// Forgotten reads remain in the bounded table until their worker exits.
    /// Cooperative work can stop between blocking reads or bounded batches.
    cancellation: Cancellation,
    /// A forgotten result is discarded when the worker returns.
    forgotten: bool,
    /// Who to tell when it lands (only while it is being read).
    askers: Vec<Asker>,
}

struct Inner<K, V> {
    entries: HashMap<K, Entry<V>>,
    /// Requesters whose key could not be admitted while every slot was in
    /// flight. The current visible route re-asks when notified.
    retry_askers: Vec<Asker>,
    clock: Tick,
}

/// The work a memo does for one key: pure, and safe to run on any thread.
type Work<K, V> = dyn Fn(&K, &Cancellation) -> V + Send + Sync;

/// Cooperative cancellation for off-thread reads.
#[derive(Clone, Debug, Default)]
pub(crate) struct Cancellation(Arc<std::sync::atomic::AtomicBool>);

impl Cancellation {
    fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

struct Shared<K, V> {
    inner: RefCell<Inner<K, V>>,
    work: Arc<Work<K, V>>,
    capacity: NonZeroUsize,
}

/// A bounded, single-flight, panic-safe cache filled off the UI thread.
/// Cheap to clone; every clone is the same cache.
pub(crate) struct Memo<K, V> {
    shared: Rc<Shared<K, V>>,
}

impl<K, V> Clone for Memo<K, V> {
    fn clone(&self) -> Self {
        Self {
            shared: Rc::clone(&self.shared),
        }
    }
}

impl<K, V> Memo<K, V>
where
    K: Clone + Eq + Hash + Send + 'static,
    V: Send + Sync + 'static,
{
    /// A memo that keeps `capacity` values, each computed by `work`.
    pub(crate) fn new(
        capacity: NonZeroUsize,
        work: impl Fn(&K) -> V + Send + Sync + 'static,
    ) -> Self {
        Self::new_cancellable(capacity, move |key, _| work(key))
    }

    /// As [`Memo::new`], with a token cooperative work can check between
    /// blocking reads or bounded batches.
    pub(crate) fn new_cancellable(
        capacity: NonZeroUsize,
        work: impl Fn(&K, &Cancellation) -> V + Send + Sync + 'static,
    ) -> Self {
        Self {
            shared: Rc::new(Shared {
                inner: RefCell::new(Inner {
                    entries: HashMap::new(),
                    retry_askers: Vec::new(),
                    clock: Tick::default(),
                }),
                work: Arc::new(work),
                capacity,
            }),
        }
    }

    /// The value for `key`, asked for by the view `cx` belongs to.
    #[allow(
        dead_code,
        reason = "the shell callers move to it (MIGRATE.md, R-Open3); delete this allow with that move"
    )]
    pub(crate) fn get<T: 'static>(&self, key: &K, cx: &mut Context<T>) -> Answer<V> {
        self.ask(key, Asker::View(cx.entity_id()), cx)
    }

    /// Gets a cached value unless it completed more than `lifetime` ago.
    /// In-flight work is never duplicated; its freshness window starts when
    /// the result lands.
    pub(crate) fn get_expiring<T: 'static>(
        &self,
        key: &K,
        lifetime: Duration,
        cx: &mut Context<T>,
    ) -> Answer<V> {
        {
            let mut inner = self.shared.inner.borrow_mut();
            let expired = inner.entries.get(key).is_some_and(|entry| {
                !matches!(&entry.state, State::Reading)
                    && entry.completed_at.is_some_and(|completed| {
                        Instant::now().saturating_duration_since(completed) >= lifetime
                    })
            });
            if expired {
                inner.entries.remove(key);
            }
        }
        self.get(key, cx)
    }

    /// Keeps values relevant to the current view identity. Completed stale
    /// values are dropped immediately; stale flights keep their capacity
    /// slot, receive cooperative cancellation, and discard their result when
    /// they finish.
    pub(crate) fn retain_keys(&self, mut keep: impl FnMut(&K) -> bool) {
        let mut inner = self.shared.inner.borrow_mut();
        inner.entries.retain(|key, entry| {
            if keep(key) {
                return true;
            }
            if matches!(entry.state, State::Reading) {
                entry.forgotten = true;
                entry.cancellation.cancel();
                true
            } else {
                false
            }
        });
    }

    /// The value for `key`, asked for by `asker`.
    pub(crate) fn ask(&self, key: &K, asker: Asker, cx: &mut App) -> Answer<V> {
        let cancellation = {
            let mut inner = self.shared.inner.borrow_mut();
            inner.clock = inner.clock.next();
            let now = inner.clock;
            if let Some(entry) = inner.entries.get_mut(key) {
                entry.used = now;
                return match &entry.state {
                    State::Ready(value) => Answer::Ready(Arc::clone(value)),
                    State::Failed(fault) => Answer::Failed(fault.clone()),
                    State::Reading => {
                        remember_asker(&mut entry.askers, asker, self.shared.capacity);
                        Answer::Reading
                    }
                };
            }
            if !inner.make_room(self.shared.capacity) {
                inner.defer(asker, self.shared.capacity);
                return Answer::Deferred;
            }
            let cancellation = Cancellation::default();
            inner.entries.insert(
                key.clone(),
                Entry {
                    state: State::Reading,
                    used: now,
                    completed_at: None,
                    cancellation: cancellation.clone(),
                    forgotten: false,
                    askers: vec![asker],
                },
            );
            cancellation
        };
        self.start(key.clone(), cancellation, cx);
        Answer::Reading
    }

    /// The value for `key` if it is there now; asks nothing, touches nothing.
    #[cfg(test)]
    pub(crate) fn peek(&self, key: &K) -> Option<Arc<V>> {
        match &self.shared.inner.borrow().entries.get(key)?.state {
            State::Ready(value) => Some(Arc::clone(value)),
            State::Reading | State::Failed(_) => None,
        }
    }

    /// Forgets `key` (a failed one is asked again the next time). A running
    /// read keeps its slot until it exits, receives cancellation, and cannot
    /// resurrect the forgotten value.
    #[cfg(test)]
    pub(crate) fn forget(&self, key: &K) {
        let mut inner = self.shared.inner.borrow_mut();
        match inner.entries.get_mut(key) {
            Some(entry) if matches!(entry.state, State::Reading) => {
                entry.forgotten = true;
                entry.cancellation.cancel();
            }
            Some(_) => {
            inner.entries.remove(key);
        }
            None => {}
        }
    }

    /// Puts a value in as if it had been computed (a test's stand-in for the work).
    #[cfg(test)]
    pub(crate) fn seed(&self, key: K, value: V) {
        let mut inner = self.shared.inner.borrow_mut();
        inner.clock = inner.clock.next();
        let now = inner.clock;
        if !inner.entries.contains_key(&key) {
            if !inner.make_room(self.shared.capacity) {
                return;
            }
        }
        inner.entries.insert(
            key,
            Entry {
                state: State::Ready(Arc::new(value)),
                used: now,
                completed_at: Some(Instant::now()),
                cancellation: Cancellation::default(),
                forgotten: false,
                askers: Vec::new(),
            },
        );
    }

    /// How many values are being computed by this memo.
    #[cfg(any(test, feature = "visual-harness"))]
    pub(crate) fn reading(&self) -> usize {
        self.shared
            .inner
            .borrow()
            .entries
            .values()
            .filter(|entry| matches!(entry.state, State::Reading))
            .count()
    }

    /// How many keys are kept.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.shared.inner.borrow().entries.len()
    }

    fn start(&self, key: K, cancellation: Cancellation, cx: &mut App) {
        IN_FLIGHT.fetch_add(1, Ordering::Relaxed);
        let in_flight = InFlight;
        let work = Arc::clone(&self.shared.work);
        let owned = key.clone();
        let work_cancellation = cancellation.clone();
        let task = cx.background_executor().spawn(async move {
            std::panic::catch_unwind(AssertUnwindSafe(|| work(&owned, &work_cancellation)))
                .map_err(|panic| Fault::Panicked(describe(panic.as_ref())))
        });
        let shared = Rc::downgrade(&self.shared);
        cx.spawn(async move |cx| {
            let _in_flight = in_flight;
            let outcome = task.await;
            cx.update(|cx| land(&shared, &key, outcome, cx));
            // Dropping this completion task also balances IN_FLIGHT.
        })
        .detach();
    }
}

impl<K: Clone + Eq + Hash, V> Inner<K, V> {
    /// Forgets least recently asked completed values until there is room.
    /// An in-flight entry is never evicted to admit untracked work.
    fn make_room(&mut self, capacity: NonZeroUsize) -> bool {
        while self.entries.len() >= capacity.get() {
            let Some(oldest) = self
                .entries
                .iter()
                .filter(|(_, entry)| !matches!(entry.state, State::Reading))
                .min_by_key(|(_, entry)| entry.used)
                .map(|(key, _)| key.clone())
            else {
                return false;
            };
            self.entries.remove(&oldest);
        }
        true
    }

    /// Remembers a bounded retry signal. When too many different views are
    /// blocked, one `Everyone` signal replaces them: waking all windows once
    /// lets each current route retry without an unbounded waiter list.
    fn defer(&mut self, asker: Asker, capacity: NonZeroUsize) {
        remember_asker(&mut self.retry_askers, asker, capacity);
    }
}

impl<K, V> Drop for Shared<K, V> {
    fn drop(&mut self) {
        for entry in self.inner.get_mut().entries.values() {
            if matches!(entry.state, State::Reading) {
                entry.cancellation.cancel();
            }
        }
    }
}

fn remember_asker(askers: &mut Vec<Asker>, asker: Asker, capacity: NonZeroUsize) {
    if askers.contains(&Asker::Everyone) {
        return;
    }
    if asker == Asker::Everyone {
        askers.clear();
        askers.push(Asker::Everyone);
        return;
    }
    if askers.contains(&asker) {
        return;
    }
    if askers.len() >= capacity.get() {
        askers.clear();
        askers.push(Asker::Everyone);
    } else {
        askers.push(asker);
    }
}

fn land<K, V>(shared: &Weak<Shared<K, V>>, key: &K, outcome: Result<V, Fault>, cx: &mut App)
where
    K: Eq + Hash,
    V: Send + Sync + 'static,
{
    let Some(shared) = shared.upgrade() else {
        return;
    };
    let (askers, retry_askers) = {
        let mut inner = shared.inner.borrow_mut();
        let forgotten = inner.entries.get(key).is_some_and(|entry| entry.forgotten);
        let askers = if forgotten {
            inner
                .entries
                .remove(key)
                .map(|mut entry| std::mem::take(&mut entry.askers))
        } else if let Some(entry) = inner.entries.get_mut(key) {
        entry.state = match outcome {
            Ok(value) => State::Ready(Arc::new(value)),
            Err(fault) => State::Failed(fault),
        };
        entry.completed_at = Some(Instant::now());
            Some(std::mem::take(&mut entry.askers))
        } else {
            None
    };
        let askers = askers.unwrap_or_default();
        // A completed or forgotten flight opens one slot. Retrying askers
        // compete through `ask`, which evicts only retained completed values
        // and still enforces the same strict capacity.
        let retry = std::mem::take(&mut inner.retry_askers);
        (askers, retry)
    };
    let mut told = Vec::with_capacity(askers.len().saturating_add(retry_askers.len()));
    for asker in askers.into_iter().chain(retry_askers) {
        if told.contains(&asker) {
            continue;
        }
        told.push(asker);
        match asker {
            Asker::View(view) => cx.notify(view),
            Asker::Everyone => cx.refresh_windows(),
        }
    }
}

/// What a caught panic said, or that it said nothing.
pub(crate) fn describe(panic: &(dyn Any + Send)) -> Arc<str> {
    let words = panic
        .downcast_ref::<&str>()
        .map(|words| (*words).to_owned())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "with no message".to_owned());
    Arc::from(words)
}

#[cfg(test)]
#[path = "offload_tests.rs"]
mod tests;
