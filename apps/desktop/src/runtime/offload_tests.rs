//! W-Open I3: the off-thread cache keeps its promises: one flight per key, a
//! bound, a typed fault for a panic, a notification for the asker alone, and a
//! count a harness can wait on. Every assertion reads what a view painted or
//! what the cache kept, never how it got there.

#![allow(clippy::expect_used, clippy::panic)]

use super::*;
use gpui::{
    AppContext as _, Entity, IntoElement, ParentElement, Render, StyleRefinement, TestAppContext,
    VisualTestContext, Window, div,
};
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize};

fn slots(count: usize) -> NonZeroUsize {
    NonZeroUsize::new(count).expect("a capacity")
}

/// A view that shows what a memo answers for a key (or, asking nothing,
/// what it was told), and counts its renders.
struct Probe {
    memo: Memo<u32, String>,
    asks: Option<u32>,
    shown: Rc<RefCell<String>>,
    renders: Rc<Cell<u32>>,
}

impl Render for Probe {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.renders.set(self.renders.get() + 1);
        let words = match self.asks {
            None => "asks nothing".to_owned(),
            Some(key) => match self.memo.get(&key, cx) {
                Answer::Reading => "reading".to_owned(),
                Answer::Deferred => "waiting for read capacity".to_owned(),
                Answer::Ready(value) => (*value).clone(),
                Answer::Failed(fault) => format!("failed: {fault}"),
            },
        };
        *self.shown.borrow_mut() = words.clone();
        div().child(words)
    }
}

/// The window's root: it holds the probe as a cached region, the way the shell
/// holds its regions, so the probe renders when it is notified and no
/// oftener (an uncached root renders on every draw).
struct Frame {
    child: Entity<Probe>,
}

impl Render for Frame {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().child(self.child.clone().cached(StyleRefinement::default()))
    }
}

/// A window on one probe, and what it painted.
struct Shown {
    cx: &'static mut VisualTestContext,
    words: Rc<RefCell<String>>,
    renders: Rc<Cell<u32>>,
}

impl Shown {
    /// Draws what changed, as the platform would, and returns what the
    /// probe says now.
    fn draw(&mut self) -> String {
        self.cx.run_until_parked();
        self.cx.update(|window, cx| window.draw(cx).clear(cx));
        self.words.borrow().clone()
    }
}

/// Draws every window until nothing more lands: a value that arrives while
/// one window draws is announced by the next draw, whichever it is.
fn settle(windows: &mut [&mut Shown]) {
    for _ in 0..4 {
        for window in windows.iter_mut() {
            window.draw();
        }
    }
}

fn window(cx: &mut TestAppContext, memo: &Memo<u32, String>, asks: Option<u32>) -> Shown {
    let words = Rc::new(RefCell::new(String::new()));
    let renders = Rc::new(Cell::new(0));
    let (seen, counted, memo) = (Rc::clone(&words), Rc::clone(&renders), memo.clone());
    let opened = cx.update(|cx| {
        cx.open_window(gpui::WindowOptions::default(), move |_, cx| {
            let child = cx.new(|_| Probe {
                memo,
                asks,
                shown: seen,
                renders: counted,
            });
            cx.new(|_| Frame { child })
        })
        .expect("window")
    });
    Shown {
        cx: VisualTestContext::from_window(opened.into(), cx).into_mut(),
        words,
        renders,
    }
}

fn upper(counted: &Arc<AtomicU32>) -> impl Fn(&u32) -> String + Send + Sync + use<> {
    let counted = Arc::clone(counted);
    move |key| {
        counted.fetch_add(1, Ordering::SeqCst);
        format!("value {key}")
    }
}

#[gpui::test]
fn the_first_ask_answers_at_once_and_the_value_lands_after(cx: &mut TestAppContext) {
    let calls = Arc::new(AtomicU32::new(0));
    let memo = Memo::new(slots(4), upper(&calls));
    assert!(
        matches!(
            cx.update(|cx| memo.ask(&1, Asker::Everyone, cx)),
            Answer::Reading
        ),
        "the ask does not wait for the work"
    );
    assert_eq!(memo.reading(), 1, "the flight is counted");
    cx.run_until_parked();
    assert_eq!(memo.reading(), 0, "and lands");
    assert_eq!(
        memo.peek(&1).as_deref().map(String::as_str),
        Some("value 1")
    );
    assert!(matches!(
        cx.update(|cx| memo.ask(&1, Asker::Everyone, cx)),
        Answer::Ready(_)
    ));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "asked twice, computed once"
    );
}

#[gpui::test]
fn a_value_lands_and_only_the_view_that_asked_redraws(cx: &mut TestAppContext) {
    let calls = Arc::new(AtomicU32::new(0));
    let memo = Memo::new(slots(4), upper(&calls));
    let mut asking = window(cx, &memo, Some(1));
    let mut idle = window(cx, &memo, None);
    settle(&mut [&mut asking, &mut idle]);
    assert_eq!(asking.draw(), "value 1", "the asker's frame has the value");
    assert_eq!(idle.draw(), "asks nothing");
    assert_eq!(
        asking.renders.get(),
        2,
        "the asker drew twice: once reading, once with the value"
    );
    assert_eq!(
        idle.renders.get(),
        1,
        "a view that asked nothing never redrew"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[gpui::test]
fn every_ask_before_it_lands_joins_one_flight(cx: &mut TestAppContext) {
    let calls = Arc::new(AtomicU32::new(0));
    let memo = Memo::new(slots(4), upper(&calls));
    let mut first = window(cx, &memo, Some(7));
    let mut second = window(cx, &memo, Some(7));
    settle(&mut [&mut first, &mut second]);
    assert_eq!(
        (first.draw(), second.draw()),
        ("value 7".to_owned(), "value 7".to_owned()),
        "both were told"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the work ran once for two views"
    );
    assert_eq!(memo.reading(), 0);
}

#[gpui::test]
fn it_keeps_only_what_it_can_hold_and_forgets_the_least_recently_asked(cx: &mut TestAppContext) {
    let memo = Memo::new(slots(2), upper(&Arc::new(AtomicU32::new(0))));
    let land = |cx: &mut TestAppContext, key: u32| {
        cx.update(|cx| memo.ask(&key, Asker::Everyone, cx));
        cx.run_until_parked();
    };
    land(cx, 1);
    land(cx, 2);
    // Ask for 1 again (it is now the more recent), then a third key: 2 goes.
    assert!(matches!(
        cx.update(|cx| memo.ask(&1, Asker::Everyone, cx)),
        Answer::Ready(_)
    ));
    land(cx, 3);
    assert_eq!(memo.len(), 2, "the cache is bounded");
    assert_eq!(
        memo.peek(&1).as_deref().map(String::as_str),
        Some("value 1"),
        "the recently asked stays"
    );
    assert_eq!(
        memo.peek(&3).as_deref().map(String::as_str),
        Some("value 3"),
        "the newest stays"
    );
    assert!(memo.peek(&2).is_none(), "the least recently asked went");
}

#[gpui::test]
fn a_panic_becomes_a_typed_fault_and_the_next_key_still_works(cx: &mut TestAppContext) {
    let memo: Memo<u32, String> = Memo::new(slots(4), |key| {
        assert!(*key != 13, "unlucky {key}");
        format!("value {key}")
    });
    let mut unlucky = window(cx, &memo, Some(13));
    let mut lucky = window(cx, &memo, Some(2));
    settle(&mut [&mut unlucky, &mut lucky]);
    let failed = unlucky.draw();
    assert!(
        failed.starts_with("failed: the work panicked: ") && failed.contains("unlucky 13"),
        "the page says why: {failed}"
    );
    assert_eq!(
        lucky.draw(),
        "value 2",
        "a panic did not take the worker down: the next key lands"
    );
    assert_eq!(memo.reading(), 0, "nothing is left in flight");
    // A failure is kept (no busy retry each frame) until it is forgotten.
    assert!(matches!(
        cx.update(|cx| memo.ask(&13, Asker::Everyone, cx)),
        Answer::Failed(Fault::Panicked(_))
    ));
    memo.forget(&13);
    assert!(
        matches!(
            cx.update(|cx| memo.ask(&13, Asker::Everyone, cx)),
            Answer::Reading
        ),
        "forgotten, it is asked again"
    );
}

#[gpui::test]
fn async_work_can_yield_and_poll_panics_still_become_typed_faults(cx: &mut TestAppContext) {
    let (release, wait) = async_channel::bounded::<()>(1);
    let entered = Arc::new(AtomicBool::new(false));
    let work_entered = Arc::clone(&entered);
    let memo = Memo::new_cancellable_async(slots(1), move |_key, cancellation| {
        let wait = wait.clone();
        let entered = Arc::clone(&work_entered);
        async move {
            entered.store(true, Ordering::SeqCst);
            wait.recv().await.expect("test releases async work");
            assert!(!cancellation.is_cancelled());
            panic!("async work after yielding")
        }
    });

    assert!(matches!(
        cx.update(|cx| memo.ask(&7, Asker::Everyone, cx)),
        Answer::Reading
    ));
    cx.run_until_parked();
    assert!(
        entered.load(Ordering::SeqCst),
        "the background future was polled"
    );
    assert_eq!(memo.reading(), 1, "yielded work keeps its bounded slot");
    assert_eq!(memo.len(), 1, "yielded work remains tracked");

    release.try_send(()).expect("release async work");
    cx.run_until_parked();
    assert!(matches!(
        cx.update(|cx| memo.ask(&7, Asker::Everyone, cx)),
        Answer::Failed(Fault::Panicked(message)) if message.as_ref() == "async work after yielding"
    ));
    assert_eq!(memo.reading(), 0, "a panic after a yield closes the flight");
}

#[gpui::test]
fn the_flight_is_counted_until_the_value_is_announced(cx: &mut TestAppContext) {
    let memo = Memo::new(slots(4), upper(&Arc::new(AtomicU32::new(0))));
    cx.update(|cx| memo.ask(&6, Asker::Everyone, cx));
    assert_eq!(memo.reading(), 1);
    assert!(in_flight() >= 1, "a harness sees the flight");
    cx.run_until_parked();
    assert_eq!(memo.reading(), 0, "and sees it end");
}

#[gpui::test]
fn everyone_is_the_fallback_that_redraws_every_window(cx: &mut TestAppContext) {
    let memo = Memo::new(slots(4), upper(&Arc::new(AtomicU32::new(0))));
    let mut first = window(cx, &memo, None);
    let mut second = window(cx, &memo, None);
    settle(&mut [&mut first, &mut second]);
    let before = (first.renders.get(), second.renders.get());
    assert_eq!(before, (1, 1), "two idle windows, drawn once");
    cx.update(|cx| memo.ask(&9, Asker::Everyone, cx));
    settle(&mut [&mut first, &mut second]);
    assert!(
        first.renders.get() > before.0 && second.renders.get() > before.1,
        "a caller with no view redraws all: {:?}",
        (first.renders.get(), second.renders.get())
    );
}

#[gpui::test]
fn a_seeded_value_is_there_at_once_with_no_work(cx: &mut TestAppContext) {
    let calls = Arc::new(AtomicU32::new(0));
    let memo = Memo::new(slots(4), upper(&calls));
    memo.seed(3, "value from the snapshot".to_owned());
    let mut asking = window(cx, &memo, Some(3));
    assert_eq!(
        asking.draw(),
        "value from the snapshot",
        "the first frame has it"
    );
    assert_eq!(asking.renders.get(), 1, "and never redrew for it");
    assert_eq!(calls.load(Ordering::SeqCst), 0, "no work ran");
}

/// The teardown case: a value that lands while the test's app is being torn
/// down (the flight is still running when the body returns) must leave no
/// entity handle behind. gpui's leak detector panics the test at exit
/// ("Exited with leaked handles") if one survives.
#[gpui::test]
fn a_value_that_lands_during_teardown_leaks_no_entity_handle(cx: &mut TestAppContext) {
    let memo = Memo::new(slots(4), upper(&Arc::new(AtomicU32::new(0))));
    let asking = window(cx, &memo, Some(1));
    let idle = window(cx, &memo, None);
    assert_eq!(
        memo.reading(),
        1,
        "the asker's first render started the flight, and it has not landed"
    );
    // A caller with no view (`Asker::Everyone`) asks for another key the same way.
    cx.update(|cx| memo.ask(&2, Asker::Everyone, cx));
    assert_eq!(
        memo.reading(),
        2,
        "two flights are running when the body returns"
    );
    // The windows and the memo go out of scope here, with both flights running.
    let _ = (&asking, &idle);
}

#[gpui::test]
fn unique_route_keys_wait_for_capacity_without_untracked_flights(cx: &mut TestAppContext) {
    let capacity = slots(3);
    let started = Arc::new(AtomicU32::new(0));
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(AtomicBool::new(false));
    let work_started = Arc::clone(&started);
    let work_active = Arc::clone(&active);
    let work_peak = Arc::clone(&peak);
    let work_release = Arc::clone(&release);
    let memo = Memo::new(capacity, move |key| {
        work_started.fetch_add(1, Ordering::SeqCst);
        let now = work_active.fetch_add(1, Ordering::SeqCst) + 1;
        work_peak.fetch_max(now, Ordering::SeqCst);
        assert!(
            work_release.load(Ordering::SeqCst),
            "the queued worker starts after the test releases it"
        );
        work_active.fetch_sub(1, Ordering::SeqCst);
        format!("value {key}")
    });

    let mut reading = 0;
    let mut deferred = 0;
    for key in 0..1_000 {
        let answer = cx.update(|cx| memo.ask(&key, Asker::Everyone, cx));
        match answer {
            Answer::Reading => reading += 1,
            Answer::Deferred => deferred += 1,
            Answer::Ready(_) | Answer::Failed(_) => panic!("unique key was unexpectedly cached"),
        }
        assert!(memo.len() <= capacity.get(), "retained entries are bounded");
        assert!(
            memo.reading() <= capacity.get(),
            "tracked flights are bounded"
        );
    }
    assert_eq!(reading, capacity.get());
    assert_eq!(deferred, 1_000 - capacity.get());
    assert_eq!(memo.len(), capacity.get());
    assert_eq!(memo.reading(), capacity.get());
    assert!(peak.load(Ordering::SeqCst) <= capacity.get());

    let mut retry = window(cx, &memo, Some(10_000));
    // The deterministic executor polls background work on this test thread.
    // Inspect the first painted frame before pumping it, rather than blocking
    // a worker on a release that this same thread still needs to publish.
    assert_eq!(retry.words.borrow().as_str(), "waiting for read capacity");
    release.store(true, Ordering::SeqCst);
    settle(&mut [&mut retry]);
    assert_eq!(
        retry.draw(),
        "value 10000",
        "the blocked visible route retries"
    );
    assert!(retry.renders.get() >= 2, "the retry requester was notified");
    assert_eq!(
        started.load(Ordering::SeqCst),
        4,
        "only three initial and one retry work item ran"
    );
    assert!(
        peak.load(Ordering::SeqCst) <= capacity.get(),
        "actual worker concurrency remains within capacity"
    );
    assert!(memo.len() <= capacity.get());
    assert!(memo.reading() <= capacity.get());
}

#[gpui::test]
fn a_forgotten_inflight_key_cannot_resurrect_or_strand_a_waiter(cx: &mut TestAppContext) {
    let cancelled = Arc::new(AtomicBool::new(false));
    let work_cancelled = Arc::clone(&cancelled);
    let memo = Memo::new_cancellable(slots(1), move |key, cancellation| {
        if *key == 1 {
            assert!(
                cancellation.is_cancelled(),
                "the forgotten queued flight receives cancellation"
            );
            work_cancelled.store(true, Ordering::SeqCst);
        } else {
            assert!(
                !cancellation.is_cancelled(),
                "the waiting route is still wanted"
            );
        }
        format!("value {key}")
    });
    assert!(matches!(
        cx.update(|cx| memo.ask(&1, Asker::Everyone, cx)),
        Answer::Reading
    ));
    let mut retry = window(cx, &memo, Some(2));
    assert_eq!(retry.words.borrow().as_str(), "waiting for read capacity");
    memo.forget(&1);
    assert_eq!(
        memo.len(),
        1,
        "a cancelled worker keeps its bounded slot until exit"
    );
    assert_eq!(
        memo.reading(),
        1,
        "forgetting does not create untracked work capacity"
    );
    assert!(matches!(
        cx.update(|cx| memo.ask(&3, Asker::Everyone, cx)),
        Answer::Deferred
    ));
    settle(&mut [&mut retry]);

    assert_eq!(
        retry.draw(),
        "value 2",
        "the waiting key was retried after stale work landed"
    );
    assert!(
        memo.peek(&1).is_none(),
        "the stale completion did not reinsert its key"
    );
    assert_eq!(
        memo.peek(&2).as_deref().map(String::as_str),
        Some("value 2")
    );
    assert_eq!(memo.reading(), 0, "all tracked work settled");
    assert!(
        cancelled.load(Ordering::SeqCst),
        "the worker observed cooperative cancellation"
    );
}
