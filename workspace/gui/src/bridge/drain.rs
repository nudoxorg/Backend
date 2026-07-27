//! `drain` — the only way events enter GPUI.
//!
//! ## The batching rule (read this first)
//!
//! GPUI re-renders on every `cx.notify()`. If a burst of 500 events each
//! triggered an independent notify, the UI would re-render 500 times in one
//! scheduling quantum — burning frame budget on frames that are immediately
//! superseded by the next event.
//!
//! `drain` collapses bursts into a single update+notify per scheduler wake:
//!
//! 1. **Await one event** — the task yields to the executor while the channel
//!    is empty, costing nothing.
//! 2. **Drain everything already queued** — once woken, pull every event that
//!    is immediately available with `try_recv` (no additional awaiting).
//! 3. **Notify exactly once** — a single `cx.notify()` at the end of the
//!    batch, which schedules at most one re-render.
//!
//! This is what turns a 500-event burst into **one** re-render. The
//! implementation is ~25 lines; do not add any complexity to it.
//!
//! ## Task ownership (LD-18)
//!
//! `drain` returns the owning `Task<()>`. The caller (a store) assigns it to a
//! `Task<()>` field. When the store is dropped, the task is dropped, and GPUI
//! cancels a dropped `Task` — so the drain loop is cancelled with the store and
//! there are no orphaned background loops. **Nothing is `.detach()`ed.**
//!
//! ## What this function is not
//!
//! `drain` knows nothing about `Gen`, nothing about events, and nothing about
//! the application domain. It is a pure mechanical bridge: channel events →
//! foreground state mutations, batched. Gen filtering, stale-guard logic, and
//! value construction all live in the `apply` closure supplied by the store
//! (see GUI-PLAN §7.4 for the canonical shape).
//!
//! ## Adapting the plan's code to the real `cx.spawn` signature
//!
//! GUI-PLAN §7.3 shows:
//!
//! ```text
//! cx.spawn(async move |store, cx| { ... })
//! ```
//!
//! The real signature at `gpui/src/app/context.rs:237-245` is:
//!
//! ```text
//! pub fn spawn<AsyncFn, R>(&self, f: AsyncFn) -> Task<R>
//! where
//!     AsyncFn: AsyncFnOnce(WeakEntity<T>, &mut AsyncApp) -> R + 'static,
//! ```
//!
//! So the closure receives `(WeakEntity<S>, &mut AsyncApp)` — matching the
//! plan's `(store, cx)` naming exactly, where `store: WeakEntity<S>` and
//! `cx: &mut AsyncApp`. Inside the closure, store mutations are done via
//! `store.update(cx, |store, cx| { ... })`, which calls
//! `WeakEntity::update` (verified at `gpui/src/app/entity_map.rs:777-787`).
//! That method returns `Result<R>`, so `.is_err()` is the entity-dropped
//! sentinel.

use gpui::{AsyncApp, Context, Task, WeakEntity};

/// Spawn a foreground task that drains `rx` into `store`, batching bursts
/// into a single update+notify per executor wake.
///
/// # Parameters
///
/// - `cx` — the store's `Context`, used to capture a `WeakEntity<S>` and to
///   spawn the foreground task.
/// - `rx` — a bounded `flume` receiver. Closing the sender signals that the
///   stream is complete; the drain loop exits cleanly on channel closure.
/// - `apply` — called for every event, inside a single `store.update(cx, …)`
///   call that covers the whole batch. The closure is responsible for the
///   stale-gen guard and all state mutations.
///
/// # Returns
///
/// The owning `Task<()>`. Assign this to a store field; dropping the store
/// drops the task and cancels the drain loop (LD-18).
///
/// # Batching guarantee
///
/// A burst of N events that arrive before the executor schedules a new wake
/// produces exactly **one** `apply` call per event and exactly **one**
/// `cx.notify()` for the entire burst — not N notifies.
pub fn drain<S: 'static, T: Send + 'static>(
    cx: &mut Context<S>,
    rx: flume::Receiver<T>,
    mut apply: impl FnMut(&mut S, T, &mut Context<S>) + 'static,
) -> Task<()> {
    cx.spawn(async move |store: WeakEntity<S>, cx: &mut AsyncApp| {
        loop {
            // ── Step 1: await one event ────────────────────────────────────
            // The task yields here while the channel is empty; the executor
            // runs other work. This costs nothing per idle second.
            let Ok(first) = rx.recv_async().await else {
                // Channel closed (sender dropped or all senders gone).
                // This is the normal completion signal for finite streams.
                break;
            };

            // ── Steps 2+3: drain the batch and notify once ────────────────
            // We have at least one event. Enter a single store update that
            // consumes the first event and then greedily drains everything
            // else that is already queued — turning a burst into one render.
            let update_result = store.update(cx, |store, cx| {
                apply(store, first, cx);

                // `try_recv` is non-blocking: it takes whatever is already
                // in the buffer without awaiting. A burst of N events that
                // arrived between wakes costs one loop iteration here, not
                // N scheduler round-trips.
                while let Ok(more) = rx.try_recv() {
                    apply(store, more, cx);
                }

                // One notify covers the entire batch. GPUI will coalesce
                // multiple notifies within the same effect cycle anyway, but
                // emitting exactly one here documents the intent clearly.
                cx.notify();
            });

            if update_result.is_err() {
                // The store entity was dropped (e.g. a tab was closed while
                // events were in flight). Stop the loop cleanly — no panic,
                // no error log needed, this is normal operation.
                break;
            }
        }
    })
}

#[cfg(test)]
mod tests {
    // The drain function requires a GPUI executor to test the async behaviour.
    // Plain #[test]s cover the channel mechanics; gpui::test tests would cover
    // the notify-count assertion. The plain tests below verify the flume
    // channel semantics that drain relies on (non-blocking try_recv drains a
    // burst), which is the property we need to audit most.

    use super::*;

    /// Verify that `try_recv` after a burst drains all queued items without
    /// blocking — this is the core property that makes the batching rule work.
    #[test]
    fn try_recv_drains_burst() {
        let (tx, rx) = flume::bounded::<u32>(256);

        // Send a burst of 100 events.
        for i in 0..100u32 {
            tx.send(i).unwrap();
        }
        drop(tx); // signal completion

        // Simulate the drain logic: take the first via recv (would be async
        // in the real loop), then drain the rest with try_recv.
        let first = rx.recv().unwrap();
        assert_eq!(first, 0);

        let mut rest = Vec::new();
        while let Ok(v) = rx.try_recv() {
            rest.push(v);
        }
        assert_eq!(rest.len(), 99, "all remaining events drained by try_recv");
        assert_eq!(rest.last(), Some(&99u32));
    }

    /// Verify that channel closure (sender dropped) is the completion signal.
    #[test]
    fn closed_channel_breaks_loop() {
        let (tx, rx) = flume::bounded::<u32>(8);
        drop(tx); // close immediately
        // recv should return Err immediately on a closed + empty channel.
        assert!(rx.recv().is_err(), "closed channel returns Err");
        assert!(rx.try_recv().is_err());
    }

    /// Verify that a 100-event burst through the drain logic produces
    /// exactly one logical "batch" (one contiguous application of all events
    /// before the channel would block again).
    #[test]
    fn burst_forms_one_batch() {
        let (tx, rx) = flume::bounded::<u32>(256);
        let mut batch_count = 0usize;
        let mut event_count = 0usize;

        // Seed the channel synchronously so all 100 events are available
        // before the first recv.
        for i in 0..100u32 {
            tx.send(i).unwrap();
        }
        drop(tx);

        // Replicate the drain loop structure synchronously.
        loop {
            let Ok(first) = rx.recv() else { break };
            batch_count += 1;
            event_count += 1;
            while let Ok(_more) = rx.try_recv() {
                event_count += 1;
            }
            // One "notify" per batch_count increment.
        }

        assert_eq!(event_count, 100, "all 100 events processed");
        assert_eq!(
            batch_count, 1,
            "a fully pre-loaded burst forms exactly ONE batch (one notify)"
        );
    }
}
