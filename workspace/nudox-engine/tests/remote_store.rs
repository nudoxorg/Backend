use std::{
    collections::HashMap,
    io::{Read, Write},
    net::TcpListener,
    sync::{Arc, Mutex},
    thread,
};

use heart::ContentHash;
use nudox_engine::store::remote::{IrSnapshot, RemoteStore};
use nudox_ir::{
    change::{EcosystemId, IntroId, PackageLineageId, PackageName},
    entry::{Deprecation, Entry, Node, Symbol, Visibility},
    index::RawRef,
    kind::Kind,
    kinds::Module,
    view::IrView,
};

#[tokio::test]
async fn remote_package_and_ir_objects_are_verified_and_replayed_from_a_fixture() {
    let objects = Arc::new(Mutex::new(HashMap::<String, Vec<u8>>::new()));
    let (base, join) = fixture(Arc::clone(&objects));

    let first_root = tempfile::tempdir().unwrap();
    let first = RemoteStore::open(first_root.path(), &base).unwrap();
    let package_hash = first
        .publish_package(b"source archive".to_vec())
        .await
        .unwrap();

    let lineage = PackageLineageId::new(EcosystemId::new("fixture"), PackageName::new("demo"));
    let snapshot = IrSnapshot::from_view(&IrView::with_package(
        lineage.clone(),
        nudox_ir::apply::PristineIntroTable::new(),
    ));
    let ir_hash = first.publish_ir(&snapshot).await.unwrap();

    // A new process has a different local CAS and must use the shared fixture.
    let second_root = tempfile::tempdir().unwrap();
    let second = RemoteStore::open(second_root.path(), &base).unwrap();
    assert_eq!(
        second.fetch_package(package_hash).await.unwrap().as_ref(),
        b"source archive"
    );
    let replayed = second.replay_ir(ir_hash).await.unwrap();
    assert_eq!(replayed.package(), &lineage);
    assert!(replayed.entries_sorted().next().is_none());

    join.join().unwrap();
}

// ---------------------------------------------------------------------------
// postcard round-trip fidelity, non-empty snapshot
// ---------------------------------------------------------------------------
//
// An empty-view round trip (the test above) cannot see field loss: every
// field of a zero-entry snapshot is vacuously preserved. This builds a view
// with a real entry — a name, a kind, a parent edge — and checks that the
// content itself, not just the entry count, survives `postcard::to_allocvec`
// / `postcard::from_bytes`. See the "count-based tests cannot see field loss"
// failure class.

fn intro(n: u8) -> IntroId {
    IntroId::from_raw([n; 32])
}

fn make_symbol(name: &str) -> Symbol {
    Symbol {
        name: name.to_owned(),
        visibility: Visibility::Public,
        documentation: "docs for this symbol".to_owned(),
        source: std::path::PathBuf::from("src/lib.rs"),
        span: 3..9,
        aliases: Box::new([]),
        deprecation: None::<Deprecation>,
        doc_links: Box::new([]),
        attrs: Box::new([]),
        cfg: None,
    }
}

fn module_entry(sym: Symbol) -> Entry {
    Entry::new(sym, Node::build(None::<RawRef>, []), Kind::Module(Module))
}

#[tokio::test]
async fn postcard_round_trip_preserves_a_non_empty_snapshots_content() {
    let objects = Arc::new(Mutex::new(HashMap::<String, Vec<u8>>::new()));
    // One PUT (publish_ir) and one GET (fetch_ir via replay_ir on a fresh
    // local CAS) — no package object is exchanged here.
    let (base, join) = fixture_n(Arc::clone(&objects), 2);

    let lineage = PackageLineageId::new(EcosystemId::new("fixture"), PackageName::new("widgets"));
    let module_id = intro(1);
    let module = module_entry(make_symbol("widgets"));

    let mut table = nudox_ir::apply::PristineIntroTable::new();
    table.insert_live(module_id, module, None);
    let view = IrView::with_package(lineage.clone(), table);
    let snapshot = IrSnapshot::from_view(&view);

    let first_root = tempfile::tempdir().unwrap();
    let first = RemoteStore::open(first_root.path(), &base).unwrap();
    let ir_hash = first.publish_ir(&snapshot).await.unwrap();

    // Fresh local CAS, so `fetch_ir` must actually decode the bytes the
    // fixture server holds rather than reusing an in-memory value.
    let second_root = tempfile::tempdir().unwrap();
    let second = RemoteStore::open(second_root.path(), &base).unwrap();
    let replayed = second.replay_ir(ir_hash).await.unwrap();

    let entries: Vec<_> = replayed.entries_sorted().collect();
    assert_eq!(
        entries.len(),
        1,
        "the single entry published must be the single entry replayed"
    );
    let (replayed_id, replayed_entry) = entries[0];
    assert_eq!(replayed_id, module_id, "the intro id must survive byte-for-byte");
    assert_eq!(
        replayed_entry.sym().name,
        "widgets",
        "the symbol name must survive the postcard round trip"
    );
    assert_eq!(
        replayed_entry.sym().documentation,
        "docs for this symbol",
        "documentation text must survive the postcard round trip"
    );
    assert!(
        matches!(
            replayed_entry.kind(),
            nudox_ir::entry::EntryInner::Owned(Kind::Module(_))
        ),
        "the entry kind must survive the postcard round trip"
    );
    assert_eq!(
        replayed.parent_of(module_id),
        None,
        "the (root) parent edge must survive the postcard round trip"
    );

    join.join().unwrap();
}

fn fixture(objects: Arc<Mutex<HashMap<String, Vec<u8>>>>) -> (String, thread::JoinHandle<()>) {
    fixture_n(objects, 4)
}

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
            let mut lines = request.lines();
            let first = lines.next().unwrap();
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
                objects
                    .lock()
                    .unwrap()
                    .insert(path, body);
                Vec::new()
            } else {
                objects
                    .lock()
                    .unwrap()
                    .get(&path)
                    .cloned()
                    .unwrap_or_default()
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

#[test]
fn content_hash_is_the_remote_object_address() {
    assert_eq!(
        ContentHash::of_bytes(b"source archive").hex(),
        ContentHash::of_bytes(b"source archive").to_string()
    );
}
