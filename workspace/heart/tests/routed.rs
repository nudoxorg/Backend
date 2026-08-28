//! `heart::surface::Routed` — the capability-gated router.
//!
//! The load-bearing property is not "the right rows come back" (a `Federated`
//! merge does that too) but "the local engine does **no work** when the router
//! delegates to the remote". So these tests use a `Serve` impl that records
//! whether its `serve` was ever called, and assert on that flag directly — the
//! observable difference between "minimum work when connected" and the
//! fan-out-and-merge it replaces.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use heart::surface::{
    Answer, Capabilities, Gen, GenerationId, PROTOCOL_VERSION, Residence, Routed, Serve, Summary,
    Surface, SurfaceId, answer_channel,
};
use serde::{Deserialize, Serialize};

// ── A test surface named like a real one, so `SurfaceId::from_name` matches ──

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Row {
    id: u32,
}

struct Sym;
impl Surface for Sym {
    const NAME: &'static str = "symbols"; // matches SurfaceId::Symbols
    const PATH: &'static str = "/search";
    type Request = String;
    type Item = Row;
    type Note = ();
    type Key = u32;
    fn key(item: &Self::Item) -> u32 {
        item.id
    }
}

/// A surface whose name no `SurfaceId` knows — used to prove an un-enumerable
/// surface is never delegated, even to a fully-capable remote.
struct Widget;
impl Surface for Widget {
    const NAME: &'static str = "widgets";
    const PATH: &'static str = "/widgets";
    type Request = String;
    type Item = Row;
    type Note = ();
    type Key = u32;
    fn key(item: &Self::Item) -> u32 {
        item.id
    }
}

// ── A `Serve` that records whether it was invoked, and emits one tagged row ──

struct Recorder<S: Surface> {
    called: Arc<AtomicBool>,
    id: u32,
    residence: Residence,
    _surface: std::marker::PhantomData<S>,
}

impl<S> Serve<S> for Recorder<S>
where
    S: Surface<Request = String, Item = Row>,
{
    fn serve(&self, _request: String, generation: Gen) -> Answer<S> {
        // The whole point of the test: flip the flag the instant serve runs.
        self.called.store(true, Ordering::SeqCst);
        let (tx, answer) = answer_channel::<S>(8, generation);
        let id = self.id;
        let residence = self.residence;
        std::thread::spawn(move || {
            let _: Result<(), _> = tx.item(Row { id }, residence);
            let _: Result<(), _> = tx.end(Summary::complete(1));
        });
        answer
    }
}

fn recorder<S>(id: u32, residence: Residence) -> (Arc<dyn Serve<S>>, Arc<AtomicBool>)
where
    S: Surface<Request = String, Item = Row>,
{
    let called = Arc::new(AtomicBool::new(false));
    let serve = Arc::new(Recorder {
        called: Arc::clone(&called),
        id,
        residence,
        _surface: std::marker::PhantomData,
    });
    (serve, called)
}

async fn one_row<S>(answer: Answer<S>) -> (Row, Residence)
where
    S: Surface<Item = Row>,
{
    let mut seen = None;
    while let Some(frame) = answer.recv().await {
        if let heart::surface::Frame::Item(located) = frame {
            seen = Some((located.value, located.residence));
        }
    }
    seen.expect("exactly one item was emitted")
}

fn caps(protocol: u32, surfaces: Vec<SurfaceId>, semantic: bool) -> Capabilities {
    Capabilities {
        protocol,
        surfaces,
        generation: Some(GenerationId(3)),
        semantic,
    }
}

const REMOTE_ID: u32 = 2;
const LOCAL_ID: u32 = 1;
fn remote_res() -> Residence {
    Residence::Remote {
        generation: GenerationId(3),
    }
}

// ── 1. Delegation does no local work ─────────────────────────────────────────

#[tokio::test]
async fn delegates_to_remote_and_never_touches_local() {
    let (local, local_called) = recorder::<Sym>(LOCAL_ID, Residence::Local);
    let (remote, remote_called) = recorder::<Sym>(REMOTE_ID, remote_res());

    let router = Routed::new(
        local,
        remote,
        &caps(PROTOCOL_VERSION, vec![SurfaceId::Symbols, SurfaceId::Packages], true),
    );
    assert!(router.delegates(), "a compatible remote serving symbols is delegated to");

    let (row, residence) = one_row(router.serve("q".to_owned(), Gen(1))).await;

    assert_eq!(row.id, REMOTE_ID, "the answer came from the remote");
    assert_eq!(residence, remote_res(), "and carries the remote's residence");
    assert!(remote_called.load(Ordering::SeqCst), "the remote was served");
    assert!(
        !local_called.load(Ordering::SeqCst),
        "THE contract: the local engine must not run when delegating — that is the wasted work Routed exists to cut"
    );
}

// ── 2. Standalone: no remote → local ─────────────────────────────────────────

#[tokio::test]
async fn local_only_serves_local() {
    let (local, local_called) = recorder::<Sym>(LOCAL_ID, Residence::Local);
    let router = Routed::local_only(local);
    assert!(!router.delegates());

    let (row, _) = one_row(router.serve("q".to_owned(), Gen(1))).await;
    assert_eq!(row.id, LOCAL_ID);
    assert!(local_called.load(Ordering::SeqCst));
}

// ── 3. Incompatible protocol → local (never drop the query) ───────────────────

#[tokio::test]
async fn incompatible_protocol_falls_back_to_local() {
    let (local, local_called) = recorder::<Sym>(LOCAL_ID, Residence::Local);
    let (remote, remote_called) = recorder::<Sym>(REMOTE_ID, remote_res());

    // The remote serves symbols, but speaks a protocol this client does not.
    let router = Routed::new(
        local,
        remote,
        &caps(PROTOCOL_VERSION + 1, vec![SurfaceId::Symbols], true),
    );
    assert!(!router.delegates(), "an unknown protocol is not delegated to");

    let (row, _) = one_row(router.serve("q".to_owned(), Gen(1))).await;
    assert_eq!(row.id, LOCAL_ID, "fell back to local rather than dropping the query");
    assert!(local_called.load(Ordering::SeqCst));
    assert!(!remote_called.load(Ordering::SeqCst), "the incompatible remote was not served");
}

// ── 4. Surface not advertised → local ─────────────────────────────────────────

#[tokio::test]
async fn a_surface_the_remote_does_not_advertise_stays_local() {
    let (local, local_called) = recorder::<Sym>(LOCAL_ID, Residence::Local);
    let (remote, remote_called) = recorder::<Sym>(REMOTE_ID, remote_res());

    // Compatible protocol, but this remote serves only packages — not symbols.
    let router = Routed::new(local, remote, &caps(PROTOCOL_VERSION, vec![SurfaceId::Packages], true));
    assert!(!router.delegates());

    let (row, _) = one_row(router.serve("q".to_owned(), Gen(1))).await;
    assert_eq!(row.id, LOCAL_ID);
    assert!(local_called.load(Ordering::SeqCst));
    assert!(!remote_called.load(Ordering::SeqCst));
}

// ── 5. An un-enumerable surface is never delegated ────────────────────────────

#[tokio::test]
async fn an_unknown_surface_name_is_never_delegated() {
    let (local, local_called) = recorder::<Widget>(LOCAL_ID, Residence::Local);
    let (remote, remote_called) = recorder::<Widget>(REMOTE_ID, remote_res());

    // The remote advertises every surface it can, but `widgets` is not a
    // `SurfaceId` — so there is no way to know the remote serves it, and the
    // router must keep it local.
    let router = Routed::new(
        local,
        remote,
        &caps(
            PROTOCOL_VERSION,
            vec![SurfaceId::Symbols, SurfaceId::Packages, SurfaceId::Usages],
            true,
        ),
    );
    assert!(!router.delegates(), "a surface no SurfaceId knows cannot be delegated");

    let (row, _) = one_row(router.serve("q".to_owned(), Gen(1))).await;
    assert_eq!(row.id, LOCAL_ID);
    assert!(local_called.load(Ordering::SeqCst));
    assert!(!remote_called.load(Ordering::SeqCst));
}

// ── 6. It is a `Serve<S>` — transparency ─────────────────────────────────────

#[tokio::test]
async fn a_router_is_indistinguishable_from_any_other_serve() {
    let (local, _) = recorder::<Sym>(LOCAL_ID, Residence::Local);
    let (remote, _) = recorder::<Sym>(REMOTE_ID, remote_res());
    let router: Arc<dyn Serve<Sym>> = Arc::new(Routed::new(
        local,
        remote,
        &caps(PROTOCOL_VERSION, vec![SurfaceId::Symbols], true),
    ));

    // A caller holding `Arc<dyn Serve<Sym>>` cannot tell it is a Routed.
    let (row, _) = one_row(router.serve("q".to_owned(), Gen(1))).await;
    assert_eq!(row.id, REMOTE_ID);
}
