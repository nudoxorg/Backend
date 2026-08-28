use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::thread;

use futures::StreamExt;
use nudox_engine::{
    acquire::ArchiveCache,
    Purl,
    store::source::{
        IrSource, LoadEvent, LoadRequest,
        producer::{ProducerRegistry, ProducerSource},
    },
};

#[tokio::test]
#[ignore = "fetches a small real archive from crates.io"]
async fn fetches_a_purl_archive_into_content_addressed_cache_and_reopens_it() {
    let root = tempfile::tempdir().expect("cache root");
    let purl = Purl::parse("pkg:cargo/numtoa@0.2.4").expect("valid purl");

    let receipt = ArchiveCache::new(root.path())
        .fetch(&purl)
        .await
        .expect("archive fetch");
    let digest = receipt.digest().to_owned();

    let reopened = ArchiveCache::new(PathBuf::from(root.path()))
        .open(&digest)
        .expect("content-addressed archive must reopen");

    assert_eq!(reopened.purl(), &purl);
    assert!(reopened.bytes().starts_with(&[0x1f, 0x8b]));
}

#[tokio::test]
async fn fetches_a_versioned_purl_from_a_local_upstream_fixture() {
    let archive = cargo_archive();
    let digest = sha256_hex(&archive);
    let index =
        format!("{{\"name\":\"fixture\",\"vers\":\"1.0.0\",\"cksum\":\"{digest}\",\"deps\":[]}}\n");
    let (base, join) = serve_fixture(index, archive);
    let root = tempfile::tempdir().expect("cache root");
    let purl = Purl::parse("pkg:cargo/fixture@1.0.0").expect("valid purl");

    let receipt = ArchiveCache::with_upstream(root.path(), &base)
        .fetch(&purl)
        .await
        .expect("local upstream fetch");
    let reopened = ArchiveCache::new(root.path())
        .open(receipt.digest())
        .expect("durable archive");

    assert_eq!(reopened.purl(), &purl);
    assert_eq!(reopened.bytes(), cargo_archive().as_slice());
    join.join().expect("fixture server");
}

#[tokio::test]
#[ignore = "fetches and lowers a small real archive from crates.io"]
async fn reopens_a_purl_archive_from_cache_and_lowers_without_a_checkout() {
    let root = tempfile::tempdir().expect("cache root");
    let purl = Purl::parse("pkg:cargo/numtoa@0.2.4").expect("valid purl");
    let cache = ArchiveCache::new(root.path());
    let receipt = cache.fetch(&purl).await.expect("archive fetch");
    let digest = receipt.digest().to_owned();

    // Simulate a fresh process: only the durable cache root and address survive.
    drop(cache);
    let reopened = ArchiveCache::new(PathBuf::from(root.path()));
    let source = ProducerSource::from_archive_cache(
        &reopened,
        &digest,
        std::sync::Arc::new(ProducerRegistry::with_rust_pilot()),
    )
    .expect("cached archive source");
    let events = source
        .load(LoadRequest::default())
        .collect::<Vec<_>>()
        .await;

    assert!(
        events
            .iter()
            .any(|event| matches!(event, Ok(LoadEvent::Ready { .. })))
    );
}

fn serve_fixture(index: String, archive: Vec<u8>) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("fixture listener");
    let base = format!("http://{}", listener.local_addr().expect("fixture address"));
    let join = thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().expect("fixture request");
            let mut request = [0; 4096];
            let size = stream.read(&mut request).expect("fixture read");
            let request = String::from_utf8_lossy(&request[..size]);
            let path = request
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .expect("request path");
            let (content_type, body) = if path == "/index/fi/xt/fixture" {
                ("application/json", index.as_bytes().to_vec())
            } else if path == "/static/crates/fixture/fixture-1.0.0.crate" {
                ("application/gzip", archive.clone())
            } else {
                ("text/plain", b"not found".to_vec())
            };
            let status = if content_type == "text/plain" {
                "404 Not Found"
            } else {
                "200 OK"
            };
            write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .expect("fixture headers");
            stream.write_all(&body).expect("fixture body");
        }
    });
    (base, join)
}

fn cargo_archive() -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let encoder = flate2::write::GzEncoder::new(&mut bytes, flate2::Compression::default());
        let mut tar = tar::Builder::new(encoder);
        let contents = b"[package]\nname = \"fixture\"\nversion = \"1.0.0\"\n";
        let mut header = tar::Header::new_gnu();
        header
            .set_path("fixture-1.0.0/Cargo.toml")
            .expect("archive path");
        header.set_size(contents.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append(&header, &contents[..]).expect("archive entry");
        let encoder = tar.into_inner().expect("tar finish");
        encoder.finish().expect("gzip finish");
    }
    bytes
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    use std::fmt::Write;
    sha2::Sha256::digest(bytes).iter().fold(String::new(), |mut acc, byte| {
        let _ = write!(acc, "{byte:02x}");
        acc
    })
}
