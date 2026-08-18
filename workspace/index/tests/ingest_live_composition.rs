//! Live HTTP → follower → Dolt catalog → durable watermark composition.

mod common;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

use common::migrated_writer;
use index::ingest::{DriveOutcome, FileWatermarkStore, HomebrewIngestor, WatermarkStore};
use index::store::Catalog;

#[test]
fn live_http_ingest_persists_watermark_across_store_reopen() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture");
    let url = format!(
        "http://{}/formula.json",
        listener.local_addr().expect("address")
    );
    let server = thread::spawn(move || {
        let body = br#"[{"name":"zlib","versions":{"stable":"1.0"},"urls":{"stable":{"url":"https://github.com/madler/zlib/releases/download/v1.0/zlib.tar.gz","checksum":"abc"}}}]"#;
        let (mut stream, _) = listener.accept().expect("accept fixture request");
        let mut request = [0; 2048];
        let _ = stream.read(&mut request).expect("read request");
        let response = format!(
            "HTTP/1.1 200 OK\r\nETag: \"live-v1\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream
            .write_all(response.as_bytes())
            .expect("write headers");
        stream.write_all(body).expect("write body");
    });

    let watermark_dir = tempfile::tempdir().expect("watermark dir");
    let writer = migrated_writer();
    let mut ingestor =
        HomebrewIngestor::new(&writer, watermark_dir.path(), url).expect("open ingestor");
    let outcome = ingestor.drive_once(1_000).expect("drive live fixture");
    assert!(matches!(
        outcome,
        DriveOutcome::Committed { applied: 3, .. }
    ));
    assert!(
        writer
            .get_package(index::ingest::enumerate::cpp_stem_id(
                "github.com/madler/zlib"
            ))
            .expect("read catalog")
            .is_some()
    );
    drop(ingestor);

    let reopened = FileWatermarkStore::open(watermark_dir.path()).expect("reopen watermark store");
    let watermark = reopened
        .feed_watermark("homebrew")
        .expect("read watermark")
        .expect("watermark persisted");
    assert_eq!(watermark.last_ref.as_deref(), Some("\"live-v1\""));
    server.join().expect("fixture server");
}
