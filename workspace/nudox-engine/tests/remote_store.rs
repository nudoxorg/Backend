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
    change::{EcosystemId, PackageLineageId, PackageName},
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

fn fixture(objects: Arc<Mutex<HashMap<String, Vec<u8>>>>) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let join = thread::spawn(move || {
        for _ in 0..4 {
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
