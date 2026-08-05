//! Request coalescing (a.k.a. single-flight): dedupe concurrent computations for
//! the same key so a stampede of *N* cache misses triggers **one** compute, not
//! *N*. This is the classic thundering-herd fix — when a hot key expires (or a
//! deploy cold-starts the process) every in-flight request would otherwise race
//! to recompute the same expensive value (an embedder call, a GitHub fetch, a
//! Postgres round-trip), pile onto the backend, and amplify the incident.
//!
//! Unlike moka's `try_get_with`, this primitive never shares the compute's
//! `Err` across callers (moka surfaces errors as `Arc<E>`, which forces
//! `E: Send + Sync` and loses the concrete type). Here the leader owns its
//! `Result` outright and waiters simply re-check the backing store, so non-`Clone`
//! error types — like the embedder's — flow through unchanged. See the
//! [`crate::stampede`] cache for the read-side loop that drives this gate.

use std::{hash::Hash, sync::Arc};

use dashmap::{DashMap, mapref::entry::Entry};
use tokio::sync::Notify;

/// A set of in-flight keys, each guarding one leader computation. Cheap to
/// clone-share behind an [`Arc`]; holds only the keys currently being computed.
pub struct SingleFlight<K> {
    inflight: DashMap<K, Arc<Notify>>,
}

impl<K> Default for SingleFlight<K>
where
    K: Eq + Hash + Clone,
{
    fn default() -> Self {
        Self::new()
    }
}

/// The outcome of trying to claim a key: either this caller is the *leader* and
/// must produce the value, or it is a *waiter* parked on the leader's [`Notify`].
pub enum Ticket<'a, K>
where
    K: Eq + Hash + Clone,
{
    /// This caller won the race: it must compute the value. Dropping the held
    /// [`Leader`] guard clears the key and wakes every waiter.
    Leader(Leader<'a, K>),
    /// Another caller is already computing; await this handle, then re-check the
    /// backing store. A lost-wakeup-safe await pattern lives in [`Leader`]'s docs.
    Waiter(Arc<Notify>),
}

impl<K> SingleFlight<K>
where
    K: Eq + Hash + Clone,
{
    /// Create an empty coalescer.
    pub fn new() -> Self {
        Self {
            inflight: DashMap::new(),
        }
    }

    /// Claim `key`. The first caller for an absent key becomes the
    /// [`Ticket::Leader`]; concurrent callers become [`Ticket::Waiter`]s sharing
    /// the leader's notify handle.
    pub fn enter(&self, key: K) -> Ticket<'_, K> {
        match self.inflight.entry(key.clone()) {
            Entry::Occupied(e) => Ticket::Waiter(e.get().clone()),
            Entry::Vacant(v) => {
                let notify = Arc::new(Notify::new());
                v.insert(notify.clone());
                Ticket::Leader(Leader {
                    sf: self,
                    key,
                    notify,
                })
            }
        }
    }

    /// Whether `key` currently has a leader in flight. Used by waiters to detect
    /// a leader that finished (or died) between claiming and awaiting.
    pub fn is_inflight(&self, key: &K) -> bool {
        self.inflight.contains_key(key)
    }
}

/// RAII guard for the leader of a key. On drop — whether the compute succeeded,
/// failed, or panicked — the key is released and all waiters are woken, so a
/// failed leader never wedges the herd.
///
/// Waiters must guard against a lost wakeup (leader finishing between the
/// [`Ticket::Waiter`] handoff and the `notified()` await). The safe pattern,
/// used by [`crate::stampede`]:
///
/// ```ignore
/// let notified = notify.notified();
/// tokio::pin!(notified);
/// notified.as_mut().enable();          // register *before* re-checking
/// if store.get(&key).await.is_some() { return hit; }
/// if !sf.is_inflight(&key) { continue; } // leader already gone → re-race
/// notified.await;                        // caught even if fired post-enable
/// ```
pub struct Leader<'a, K>
where
    K: Eq + Hash + Clone,
{
    sf: &'a SingleFlight<K>,
    key: K,
    notify: Arc<Notify>,
}

impl<K> Drop for Leader<'_, K>
where
    K: Eq + Hash + Clone,
{
    fn drop(&mut self) {
        self.sf.inflight.remove(&self.key);
        self.notify.notify_waiters();
    }
}
