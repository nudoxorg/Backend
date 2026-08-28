//! Tests for `heart::client::remote::RemoteClient` — the blanket `Serve<S>`
//! impl (S1, deliverable 2).
//!
//! There is no mock HTTP crate here on purpose: a hand-rolled HTTP/1.1
//! response over a raw `TcpListener` is enough to drive `RemoteClient`
//! end-to-end (real DNS-free socket, real `reqwest`, real
//! `heart::surface::decode_frames`), and it is the lightest way to prove the
//! one behaviour a mock could accidentally paper over — that dropping the
//! `Answer` actually severs the TCP connection, not just stops a local loop
//! from looking at it.
//!
//! Covers, at minimum (per the S1 brief):
//! 1. the blanket impl compiles and works for two *different* surfaces —
//!    `add_a_surface_get_its_remote_client_free`;
//! 2. a 500 response yields `Frame::Failed` — `a_5xx_status_yields_a_typed_failed_frame`;
//! 3. dropping the `Answer` cancels the in-flight request —
//!    `dropping_the_answer_aborts_the_in_flight_request`, verified by proving
//!    the *server's* socket, not just the client's local state, observes the
//!    connection go away.

use std::time::Duration;

use heart::client::remote::RemoteClient;
use heart::stream::WireError;
use heart::surface::{Frame, Gen, Located, Residence, Serve, Summary, Surface};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

// ---------------------------------------------------------------------------
// Two distinct surfaces — proving the blanket impl is generic, not special-
// cased for one shape.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Foo {
    id: u32,
}

struct Foos;

impl Surface for Foos {
    const NAME: &'static str = "foos";
    const PATH: &'static str = "/foos";
    type Request = String;
    type Item = Foo;
    type Note = ();
    type Key = u32;
    fn key(item: &Self::Item) -> u32 {
        item.id
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Bar {
    name: String,
}

struct Bars;

impl Surface for Bars {
    const NAME: &'static str = "bars";
    const PATH: &'static str = "/bars";
    type Request = String;
    type Item = Bar;
    type Note = ();
    type Key = String;
    fn key(item: &Self::Item) -> String {
        item.name.clone()
    }
}

fn line<S: Surface>(frame: &Frame<S>) -> String {
    let mut s = serde_json::to_string(frame).expect("frame serializes");
    s.push('\n');
    s
}

// ---------------------------------------------------------------------------
// A minimal hand-rolled HTTP/1.1 server, just enough to drive `RemoteClient`.
// ---------------------------------------------------------------------------

/// Accept one connection, read (and discard) the request up to its header
/// terminator, then return the socket for the caller to write a response on.
/// Real servers would also drain the declared `Content-Length` body; these
/// tests never care what was sent (`RemoteClient` posts a JSON string), so
/// skipping that is fine — the client's tiny POST body fits in the kernel's
/// receive buffer regardless of whether this side ever reads it.
async fn accept_one(listener: &TcpListener) -> TcpStream {
    let (mut stream, _addr) = listener.accept().await.expect("accept");
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let n = stream.read(&mut chunk).await.expect("read request");
        assert!(n > 0, "connection closed before headers completed");
        buf.extend_from_slice(&chunk[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }
    stream
}

/// Same as [`accept_one`], but also returns the request's path (the second
/// whitespace-delimited token of the request line) for path-based routing.
async fn accept_one_with_path(listener: &TcpListener) -> (TcpStream, String) {
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
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let path = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .expect("request line has a path")
        .to_owned();
    (stream, path)
}

/// Write a complete, well-formed HTTP/1.1 response with an exact
/// `Content-Length` and then close — the ordinary "full answer" shape.
async fn respond_complete(stream: &mut TcpStream, status: u16, reason: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: application/x-ndjson\r\n\
         Content-Length: {len}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        len = body.len(),
    );
    stream
        .write_all(response.as_bytes())
        .await
        .expect("write response");
    stream.flush().await.expect("flush response");
    let _ = stream.shutdown().await;
}

// ---------------------------------------------------------------------------
// 0. The capability handshake — the probe a router runs before delegating.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn capabilities_probe_decodes_the_handshake() {
    use heart::surface::{Capabilities, GenerationId, SurfaceId};

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local_addr");

    let advertised = Capabilities {
        protocol: heart::surface::PROTOCOL_VERSION,
        surfaces: vec![SurfaceId::Symbols, SurfaceId::Packages],
        generation: Some(GenerationId(9)),
        semantic: true,
    };
    let body = serde_json::to_string(&advertised).expect("capabilities serializes");

    let server = tokio::spawn(async move {
        let (mut stream, path) = accept_one_with_path(&listener).await;
        assert_eq!(path, "/capabilities", "probe must GET /capabilities");
        respond_complete(&mut stream, 200, "OK", &body).await;
    });

    let client = RemoteClient::connect(&format!("http://{addr}")).expect("client binds");
    let caps = client.capabilities().await.expect("probe decodes");
    assert_eq!(caps, advertised);
    assert!(caps.is_compatible());
    assert!(caps.serves(SurfaceId::Symbols));
    assert!(!caps.serves(SurfaceId::Usages));

    server.await.expect("server task");
}

#[tokio::test]
async fn capabilities_probe_error_is_a_fallback_signal() {
    // A node that answers a non-2xx to the probe is, to a router, simply "no
    // usable remote" — the probe is an `Err`, and the caller falls back to the
    // standalone path. This pins that a 503 does not decode as a degenerate
    // "empty capabilities" that a router might mistake for a working node.
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local_addr");

    let server = tokio::spawn(async move {
        let mut stream = accept_one(&listener).await;
        respond_complete(&mut stream, 503, "Service Unavailable", "not ready").await;
    });

    let client = RemoteClient::connect(&format!("http://{addr}")).expect("client binds");
    assert!(
        client.capabilities().await.is_err(),
        "a 503 probe must surface as an error a router treats as 'serve locally'"
    );

    server.await.expect("server task");
}

// ---------------------------------------------------------------------------
// 1. The blanket impl compiles and works for two different surfaces.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn add_a_surface_get_its_remote_client_free() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local_addr");

    let server = tokio::spawn(async move {
        for _ in 0..2 {
            let (mut stream, path) = accept_one_with_path(&listener).await;
            match path.as_str() {
                "/foos" => {
                    let body = format!(
                        "{}{}",
                        line(&Frame::<Foos>::Item(Located::new(
                            Foo { id: 1 },
                            Residence::Remote {
                                generation: heart::surface::GenerationId(1),
                            },
                        ))),
                        line(&Frame::<Foos>::End(Summary::complete(1))),
                    );
                    respond_complete(&mut stream, 200, "OK", &body).await;
                }
                "/bars" => {
                    let body = format!(
                        "{}{}",
                        line(&Frame::<Bars>::Item(Located::new(
                            Bar {
                                name: "b".to_owned(),
                            },
                            Residence::Local,
                        ))),
                        line(&Frame::<Bars>::End(Summary::complete(1))),
                    );
                    respond_complete(&mut stream, 200, "OK", &body).await;
                }
                other => panic!("unexpected path {other}"),
            }
        }
    });

    let client = RemoteClient::connect(&format!("http://{addr}")).expect("connect");

    let foos: heart::surface::Answer<Foos> =
        Serve::<Foos>::serve(&client, "q".to_owned(), Gen(1));
    let mut foo_frames = Vec::new();
    while let Some(frame) = foos.recv().await {
        foo_frames.push(frame);
    }
    assert_eq!(foo_frames.len(), 2);
    assert!(matches!(&foo_frames[0], Frame::Item(l) if l.value.id == 1));
    assert!(matches!(&foo_frames[1], Frame::End(_)));

    let bars: heart::surface::Answer<Bars> =
        Serve::<Bars>::serve(&client, "q".to_owned(), Gen(1));
    let mut bar_frames = Vec::new();
    while let Some(frame) = bars.recv().await {
        bar_frames.push(frame);
    }
    assert_eq!(bar_frames.len(), 2);
    assert!(matches!(&bar_frames[0], Frame::Item(l) if l.value.name == "b"));
    assert!(matches!(&bar_frames[1], Frame::End(_)));

    server.await.expect("server task");
}

// ---------------------------------------------------------------------------
// 2. A non-2xx status before streaming begins is a typed `Frame::Failed`.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_5xx_status_yields_a_typed_failed_frame() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local_addr");

    let server = tokio::spawn(async move {
        let mut stream = accept_one(&listener).await;
        respond_complete(&mut stream, 500, "Internal Server Error", "qdrant down").await;
    });

    let client = RemoteClient::connect(&format!("http://{addr}")).expect("connect");
    let answer: heart::surface::Answer<Foos> =
        Serve::<Foos>::serve(&client, "q".to_owned(), Gen(1));

    let mut frames = Vec::new();
    while let Some(frame) = answer.recv().await {
        frames.push(frame);
    }

    assert_eq!(frames.len(), 1, "a pre-stream failure is the only frame");
    match &frames[0] {
        Frame::Failed(WireError::Backend(detail)) => {
            assert!(
                detail.contains("500"),
                "expected the status in the failure detail, got {detail}"
            );
        }
        other => panic!("expected a Backend-classified Failed frame, got {other:?}"),
    }

    server.await.expect("server task");
}

// ---------------------------------------------------------------------------
// 3. Dropping the `Answer` aborts the in-flight request — proven at the
//    server's socket, not just by client-side silence.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn dropping_the_answer_aborts_the_in_flight_request() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local_addr");

    // The server declares far more body than it will ever send, so the
    // connection stays legitimately "open, more to come" from the client's
    // point of view after the first frame — exactly the state a real
    // still-streaming search would be in. It then tries to keep writing
    // after the test drops the `Answer`; if the client genuinely aborted the
    // request, those writes start failing (the client's kernel resets the
    // now-orphaned socket). A server that never observes a failed write
    // would mean the "cancellation" only stopped a local loop while leaving
    // the TCP connection (and the server's work) running — precisely the
    // bug this test exists to catch.
    let (result_tx, result_rx) = tokio::sync::oneshot::channel();

    let server = tokio::spawn(async move {
        let mut stream = accept_one(&listener).await;

        let first = line(&Frame::<Foos>::Item(Located::new(
            Foo { id: 1 },
            Residence::Remote {
                generation: heart::surface::GenerationId(1),
            },
        )));
        let header = "HTTP/1.1 200 OK\r\n\
             Content-Type: application/x-ndjson\r\n\
             Content-Length: 1000000\r\n\
             \r\n";
        stream
            .write_all(header.as_bytes())
            .await
            .expect("write header");
        stream
            .write_all(first.as_bytes())
            .await
            .expect("write first frame");
        stream.flush().await.expect("flush");

        // Give the client time to receive the first frame and drop `Answer`.
        tokio::time::sleep(Duration::from_millis(150)).await;

        let mut observed_failure = false;
        for _ in 0..20 {
            if stream.write_all(b"x").await.is_err() {
                observed_failure = true;
                break;
            }
            let _ = stream.flush().await;
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let _ = result_tx.send(observed_failure);
    });

    let client = RemoteClient::connect(&format!("http://{addr}")).expect("connect");
    let answer: heart::surface::Answer<Foos> =
        Serve::<Foos>::serve(&client, "q".to_owned(), Gen(1));

    let first = tokio::time::timeout(Duration::from_secs(5), answer.recv())
        .await
        .expect("first frame must arrive")
        .expect("a frame");
    assert!(matches!(&first, Frame::Item(l) if l.value.id == 1));

    // The request is still "in flight" — no `End`/`Failed` has arrived, and
    // the server is deliberately holding the connection open. Dropping here
    // must abort it.
    drop(answer);

    let observed_failure = tokio::time::timeout(Duration::from_secs(5), result_rx)
        .await
        .expect("server must finish its write-retry loop")
        .expect("server task result");
    assert!(
        observed_failure,
        "the server never observed a failed write after the Answer was dropped — \
         the in-flight request was not actually aborted"
    );

    server.await.expect("server task");
}
