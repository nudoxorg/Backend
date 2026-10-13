//! Owned visual contexts release mounted apps without static allocations.

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

struct WorkerLifetime(Arc<AtomicUsize>);

impl WorkerLifetime {
    fn new(live: &Arc<AtomicUsize>) -> Self {
        live.fetch_add(1, Ordering::SeqCst);
        Self(Arc::clone(live))
    }
}

impl Drop for WorkerLifetime {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

struct Reader(WorkerLifetime);

impl PageReader for Reader {
    fn read(
        &mut self,
        request: &ReadRequest,
        context: &ReadContext<'_>,
    ) -> Result<PageValue, ReadFailure> {
        let _lifetime = &self.0;
        Fixture.read(request, context)
    }
}

struct Client(WorkerLifetime);

impl EngineClient for Client {
    fn execute(&mut self, request: &EngineRequest) -> Result<EngineDto, EngineFault> {
        let _lifetime = &self.0;
        RootOnly.execute(request)
    }
}

fn mount(cx: &mut TestAppContext, live: &Arc<AtomicUsize>) -> Rig {
    let pool =
        ReadPool::start(2, |_| Reader(WorkerLifetime::new(live))).expect("real read workers");
    rig_with_engine(
        cx,
        None,
        1440.0,
        900.0,
        pool,
        Client(WorkerLifetime::new(live)),
    )
}

#[test]
fn repeated_mounted_context_drop_joins_each_rigs_workers_before_the_next_context() {
    let live = Arc::new(AtomicUsize::new(0));
    for iteration in 0..64 {
        let mut cx = TestAppContext::single();
        let app = std::rc::Rc::downgrade(&cx.app);
        let mounted = mount(&mut cx, &live);
        assert_eq!(live.load(Ordering::SeqCst), 3, "mounted worker owners");
        drop(mounted);
        // The window still owns its shell and graph. Dropping the rig's
        // handles alone cannot certify that the worker threads have stopped.
        assert_eq!(live.load(Ordering::SeqCst), 3, "window keeps graph alive");
        // Match the installed gpui macro's teardown boundary exactly.
        cx.executor().forbid_parking();
        let dispatcher = cx.dispatcher.clone();
        drop(cx);
        dispatcher.drain_tasks();
        assert!(app.upgrade().is_none(), "context {iteration} retained app");
        assert_eq!(
            live.load(Ordering::SeqCst),
            0,
            "context {iteration} retained workers"
        );
    }
}

#[test]
fn queued_foreground_work_after_owned_context_unwind_cannot_retain_the_app_or_workers() {
    let live = Arc::new(AtomicUsize::new(0));
    let queued = Arc::new(AtomicUsize::new(0));
    let dispatcher = gpui::TestDispatcher::new(0);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut cx = TestAppContext::build(dispatcher.clone(), None);
        let mounted = mount(&mut cx, &live);
        assert_eq!(live.load(Ordering::SeqCst), 3);
        let root = mounted.graph.root.downgrade();
        let window = mounted._window;
        let lifetime = WorkerLifetime::new(&queued);
        cx.spawn(async move |mut app| {
            let _lifetime = lifetime;
            // The queued continuation can outlive the scope through weak
            // handles, never a static mutable visual-context reference.
            assert!(root.update(&mut app, |_, _| ()).is_err());
            assert!(window.update(&mut app, |_, _, _| ()).is_err());
        })
        .detach();
        panic!("deliberate mounted test unwind");
    }));
    assert_eq!(
        result.expect_err("test unwound").downcast_ref::<&str>(),
        Some(&"deliberate mounted test unwind")
    );
    assert_eq!(
        live.load(Ordering::SeqCst),
        0,
        "queued weak continuation retained workers"
    );
    assert_eq!(
        queued.load(Ordering::SeqCst),
        1,
        "continuation remains queued after unwind"
    );
    // Actually poll the continuation after its app and window are gone. Both
    // accesses must refuse through their ordinary weak-handle boundaries.
    dispatcher.run_until_parked();
    dispatcher.drain_tasks();
    assert_eq!(
        queued.load(Ordering::SeqCst),
        0,
        "queued continuation was not released"
    );
}
