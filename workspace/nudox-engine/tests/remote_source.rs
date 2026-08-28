//! `RemoteSource` — the corpus-level "minimum work when connected" seam.
//!
//! Proves the load-bearing properties: a package the remote carries is replayed
//! from a fetched `IrSnapshot` and tagged `Provenance::Remote{generation}` (so
//! it lowers to a real `Residence::Remote`, not the hardcoded local badge), the
//! local fallback source is NOT run for it (that is the work being saved), and a
//! package the remote does NOT carry falls back to the local producer rather
//! than being dropped.
//!
//! Exercised against an in-process CAS (the same hand-rolled fixture
//! `remote_store.rs` uses) because the index server's IR read-path is not built
//! yet (IR-STORAGE-PLAN §3); the client subsystem is complete and goes live
//! end-to-end the moment that endpoint exists.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use futures::stream::{self, BoxStream, StreamExt as _};
use heart::surface::GenerationId;
use nudox_engine::store::package::{PackageView, Provenance};
use nudox_engine::store::remote::{IrSnapshot, RemoteStore};
use nudox_engine::store::source::remote::{RemoteSource, SnapshotCatalog};
use nudox_engine::store::source::{
    Error, IrSource, LoadEvent, LoadRequest, PackageHint, SourceDescriptor,
};
use nudox_ir::{
    apply::PristineIntroTable,
    change::{EcosystemId, IntroId, PackageLineageId, PackageName},
    entry::{Deprecation, Entry, Node, Symbol, Visibility},
    index::RawRef,
    kind::Kind,
    kinds::Module,
    view::IrView,
};

// ── fixtures (mirrors remote_store.rs) ───────────────────────────────────────

fn lineage(name: &str) -> PackageLineageId {
    PackageLineageId::new(EcosystemId::new("fixture"), PackageName::new(name))
}

fn make_symbol(name: &str) -> Symbol {
    Symbol {
        name: name.to_owned(),
        visibility: Visibility::Public,
        documentation: "docs".to_owned(),
        source: std::path::PathBuf::from("src/lib.rs"),
        span: 0..1,
        aliases: Box::new([]),
        deprecation: None::<Deprecation>,
        doc_links: Box::new([]),
        attrs: Box::new([]),
        cfg: None,
    }
}

fn one_entry_view(name: &str) -> IrView {
    let entry = Entry::new(
        make_symbol(name),
        Node::build(None::<RawRef>, []),
        Kind::Module(Module),
    );
    let mut table = PristineIntroTable::new();
    table.insert_live(IntroId::from_raw([1; 32]), entry, None);
    IrView::with_package(lineage(name), table)
}

/// A minimal in-process CAS: PUT stores by path, GET returns by path.
fn fixture_n(
    objects: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    requests: usize,
) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let join = thread::spawn(move || {
        for _ in 0..requests {
            let (mut stream, _) = listener.accept().unwrap();
            let mut raw = [0; 8192];
            let size = stream.read(&mut raw).unwrap();
            let request = String::from_utf8_lossy(&raw[..size]);
            let first = request.lines().next().unwrap();
            let mut parts = first.split_whitespace();
            let method = parts.next().unwrap();
            let path = parts.next().unwrap().to_owned();
            let body = request
                .split("\r\n\r\n")
                .nth(1)
                .unwrap_or("")
                .as_bytes()
                .to_vec();
            let response_body = if method == "PUT" {
                objects.lock().unwrap().insert(path, body);
                Vec::new()
            } else {
                objects.lock().unwrap().get(&path).cloned().unwrap_or_default()
            };
            let status = if method == "GET" && response_body.is_empty() {
                "404 Not Found"
            } else {
                "200 OK"
            };
            write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                response_body.len()
            )
            .unwrap();
            stream.write_all(&response_body).unwrap();
        }
    });
    (base, join)
}

// ── a stub fallback source that records whether it ran ───────────────────────

struct StubSource {
    calls: Arc<AtomicUsize>,
}

impl IrSource for StubSource {
    fn describe(&self) -> SourceDescriptor {
        SourceDescriptor {
            label: "stub".to_owned(),
            package_count_hint: None,
        }
    }

    fn load(&self, req: LoadRequest) -> BoxStream<'static, Result<LoadEvent, Error>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut events = Vec::new();
        for lineage in req.filter {
            let view = IrView::with_package(lineage.clone(), PristineIntroTable::new());
            events.push(Ok(LoadEvent::Discovered {
                lineage: lineage.clone(),
                hint: PackageHint {
                    display_name: lineage.name.as_str().to_owned(),
                    ecosystem: lineage.ecosystem.as_str().to_owned(),
                    version: None,
                },
            }));
            events.push(Ok(LoadEvent::Ready {
                // Locally produced → TrustedLocal, so the test can tell a
                // fallback-served package from a remote-served one by provenance.
                package: Arc::new(PackageView::build(view, Provenance::TrustedLocal)),
            }));
        }
        stream::iter(events).boxed()
    }
}

async fn drain(mut stream: BoxStream<'static, Result<LoadEvent, Error>>) -> Vec<LoadEvent> {
    let mut out = Vec::new();
    while let Some(event) = stream.next().await {
        out.push(event.expect("source events are Ok in these tests"));
    }
    out
}

// ── tests ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn covered_lineage_is_replayed_from_remote_and_never_touches_the_fallback() {
    let objects = Arc::new(Mutex::new(HashMap::<String, Vec<u8>>::new()));
    // publish_ir (1 PUT) + RemoteSource's replay_ir on a fresh CAS (1 GET).
    let (base, join) = fixture_n(Arc::clone(&objects), 2);

    let widgets = lineage("widgets");
    let publisher_root = tempfile::tempdir().unwrap();
    let publisher = RemoteStore::open(publisher_root.path(), &base).unwrap();
    let ir_hash = publisher
        .publish_ir(&IrSnapshot::from_view(&one_entry_view("widgets")))
        .await
        .unwrap();

    let catalog = SnapshotCatalog::new(HashMap::from([(widgets.clone(), ir_hash)]));

    // A fresh local CAS, so the source must actually fetch from the fixture.
    let source_root = tempfile::tempdir().unwrap();
    let store = RemoteStore::open(source_root.path(), &base).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let source = RemoteSource::new(
        store,
        Arc::new(catalog),
        GenerationId(7),
        StubSource {
            calls: Arc::clone(&calls),
        },
    );

    let events = drain(source.load(LoadRequest {
        filter: vec![widgets.clone()],
    }))
    .await;

    // Discovered then Ready.
    assert!(
        matches!(&events[0], LoadEvent::Discovered { lineage, .. } if *lineage == widgets),
        "first event is Discovered for the requested lineage"
    );
    let ready = events
        .iter()
        .find_map(|e| match e {
            LoadEvent::Ready { package } => Some(package),
            _ => None,
        })
        .expect("a Ready event");

    assert_eq!(
        ready.provenance(),
        Provenance::Remote {
            generation: GenerationId(7)
        },
        "a remote-replayed package must carry Remote provenance at the handshake generation"
    );
    assert_eq!(ready.lineage(), &widgets);
    assert_eq!(
        ready.view().entries_sorted().count(),
        1,
        "the fetched snapshot's entry survived replay"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "THE win: the local fallback source must NOT run for a package the remote serves"
    );

    join.join().unwrap();
}

#[tokio::test]
async fn uncovered_lineage_falls_back_to_the_local_producer() {
    // The catalog is empty, so no remote fetch happens; the RemoteStore base is
    // never dialed (RemoteStore::open does not connect), so no fixture is needed.
    let store = RemoteStore::open(tempfile::tempdir().unwrap().path(), "http://127.0.0.1:1").unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let source = RemoteSource::new(
        store,
        Arc::new(SnapshotCatalog::default()),
        GenerationId(1),
        StubSource {
            calls: Arc::clone(&calls),
        },
    );

    let orphan = lineage("orphan");
    let events = drain(source.load(LoadRequest {
        filter: vec![orphan.clone()],
    }))
    .await;

    let ready = events
        .iter()
        .find_map(|e| match e {
            LoadEvent::Ready { package } => Some(package),
            _ => None,
        })
        .expect("a Ready event from the fallback");
    assert_eq!(
        ready.provenance(),
        Provenance::TrustedLocal,
        "a package the remote does not carry is produced locally, not dropped"
    );
    assert_eq!(ready.lineage(), &orphan);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the fallback ran exactly once for the uncovered package"
    );
}

#[tokio::test]
async fn whole_corpus_load_is_delegated_to_the_local_source() {
    // An empty filter ("load everything") is not something a remote source
    // mirrors — it belongs to the local producer.
    let store = RemoteStore::open(tempfile::tempdir().unwrap().path(), "http://127.0.0.1:1").unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let source = RemoteSource::new(
        store,
        Arc::new(SnapshotCatalog::default()),
        GenerationId(1),
        StubSource {
            calls: Arc::clone(&calls),
        },
    );

    let _ = drain(source.load(LoadRequest::default())).await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "an empty-filter load is handed wholly to the local source"
    );
}
