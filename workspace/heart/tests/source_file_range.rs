//! `NudoxClient::source_file`/`source_file_range` — the client half of HTTP
//! Range support on the source-file endpoint (the server half is
//! `index::server::http::handlers::indexing::get_source_file`, whose pure
//! `parse_byte_range` helper is unit-tested in `index` itself).
//!
//! Same hand-rolled-HTTP-over-`TcpListener` pattern as `heart/tests/remote_client.rs`
//! (no mock HTTP crate, no live backend, no `SERVER_TEST_BACKENDS` needed): a raw
//! socket is enough to prove the client sends the right `Range` header and
//! reads back exactly the sliced bytes a `206` response carries — and, just as
//! importantly, refuses to hand back a `200` whole-file body as if it were the
//! requested slice.

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use heart::client::http::{Error, NudoxClient};

/// Accept one connection and read its request head (everything up to the
/// blank line), returning the socket plus the raw head text so a test can
/// inspect both the request line and any header it cares about (here,
/// `Range`).
async fn accept_one_with_head(listener: &TcpListener) -> (TcpStream, String) {
    let (mut stream, _addr) = listener.accept().await.expect("accept");
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    let head_end = loop {
        let n = stream.read(&mut chunk).await.expect("read request");
        assert!(n > 0, "connection closed before headers completed");
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
    };
    (stream, String::from_utf8_lossy(&buf[..head_end]).into_owned())
}

/// The request-line path (second whitespace-delimited token) out of a
/// captured head.
fn path_of(head: &str) -> &str {
    head.lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .expect("request line has a path")
}

/// The value of a given header (case-insensitive name) out of a captured
/// head, if present.
fn header_of<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim()
            .eq_ignore_ascii_case(name)
            .then(|| value.trim())
    })
}

/// Write a complete HTTP/1.1 response with an exact `Content-Length` and an
/// arbitrary byte body, then close. `extra_headers` is inserted verbatim
/// (each already `\r\n`-terminated) so a caller can add `Content-Range` etc.
async fn respond(stream: &mut TcpStream, status: u16, reason: &str, extra_headers: &str, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: application/octet-stream\r\n\
         {extra_headers}\
         Content-Length: {len}\r\n\
         Connection: close\r\n\
         \r\n",
        len = body.len(),
    );
    stream
        .write_all(head.as_bytes())
        .await
        .expect("write response head");
    stream.write_all(body).await.expect("write response body");
    stream.flush().await.expect("flush response");
    let _ = stream.shutdown().await;
}

const FILE_BODY: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz"; // 36 bytes, size referenced below

#[tokio::test]
async fn source_file_fetches_the_whole_body_with_no_range_header() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    let package = uuid::Uuid::new_v4();

    let server = tokio::spawn(async move {
        let (mut stream, head) = accept_one_with_head(&listener).await;
        assert_eq!(path_of(&head), format!("/packages/{package}/files/src/main.rs"));
        assert!(
            header_of(&head, "range").is_none(),
            "a whole-file GET must not send a Range header"
        );
        respond(&mut stream, 200, "OK", "", FILE_BODY).await;
    });

    let client = NudoxClient::connect(&format!("http://{addr}")).expect("client binds");
    let bytes = client
        .source_file(package, "src/main.rs")
        .await
        .expect("whole-file read succeeds");
    assert_eq!(bytes.as_ref(), FILE_BODY);

    server.await.expect("server task");
}

#[tokio::test]
async fn source_file_range_sends_the_range_header_and_returns_the_slice() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    let package = uuid::Uuid::new_v4();

    // The slice a server honoring `Range: bytes=10-19` would send back: 10
    // bytes starting at offset 10, out of a 36-byte file.
    let slice = &FILE_BODY[10..20];

    let server = tokio::spawn(async move {
        let (mut stream, head) = accept_one_with_head(&listener).await;
        assert_eq!(path_of(&head), format!("/packages/{package}/files/src/main.rs"));
        assert_eq!(
            header_of(&head, "range"),
            Some("bytes=10-19"),
            "client must send the exact byte range it was asked for"
        );
        respond(
            &mut stream,
            206,
            "Partial Content",
            "Content-Range: bytes 10-19/36\r\n",
            slice,
        )
        .await;
    });

    let client = NudoxClient::connect(&format!("http://{addr}")).expect("client binds");
    let bytes = client
        .source_file_range(package, "src/main.rs", 10, 19)
        .await
        .expect("ranged read succeeds on a 206");
    assert_eq!(
        bytes.as_ref(),
        slice,
        "client must return exactly the sliced bytes, not the whole file"
    );

    server.await.expect("server task");
}

#[tokio::test]
async fn source_file_range_rejects_a_200_as_not_the_requested_slice() {
    // A server that ignores an unsupported/unsatisfiable Range header and
    // falls back to 200 (RFC 7233 §3.1's "MAY ignore") must not have that
    // whole-file body silently handed back to a caller that asked for, and
    // is only prepared to use, a slice.
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    let package = uuid::Uuid::new_v4();

    let server = tokio::spawn(async move {
        let mut stream = accept_one_with_head(&listener).await.0;
        respond(&mut stream, 200, "OK", "", FILE_BODY).await;
    });

    let client = NudoxClient::connect(&format!("http://{addr}")).expect("client binds");
    let error = client
        .source_file_range(package, "src/main.rs", 10, 19)
        .await
        .expect_err("a 200 must not be accepted as a satisfied range request");
    match error {
        Error::Status { status, .. } => assert_eq!(status, 200),
        other => panic!("expected Error::Status(200), got {other:?}"),
    }

    server.await.expect("server task");
}

#[tokio::test]
async fn source_file_range_surfaces_416_as_a_typed_status_error() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    let package = uuid::Uuid::new_v4();

    let server = tokio::spawn(async move {
        let mut stream = accept_one_with_head(&listener).await.0;
        respond(
            &mut stream,
            416,
            "Range Not Satisfiable",
            "Content-Range: bytes */36\r\n",
            b"",
        )
        .await;
    });

    let client = NudoxClient::connect(&format!("http://{addr}")).expect("client binds");
    let error = client
        .source_file_range(package, "src/main.rs", 1000, 2000)
        .await
        .expect_err("a 416 must surface as an error");
    match error {
        Error::Status { status, .. } => assert_eq!(status, 416),
        other => panic!("expected Error::Status(416), got {other:?}"),
    }

    server.await.expect("server task");
}
