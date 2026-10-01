//! Concurrent loopback server used only by the explicit retrieval benchmark.
//!
//! The historical `LoopbackServer` remains unchanged: its fault modes and
//! close-after-each-response behavior continue to back the existing protocol
//! and benchmark oracles. This server instead shares immutable stored bytes,
//! precomputes their checksum once on PUT, and dispatches accepted sockets to
//! a fixed worker pool so a client batch can run concurrently end to end.

use std::{
    io::{self, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use sha2::{Digest, Sha256};

const SERVER_POLL_INTERVAL: Duration = Duration::from_millis(1);
const SOCKET_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct ConcurrentLoopbackStats {
    pub(super) requests: usize,
    pub(super) put_requests: usize,
    pub(super) get_requests: usize,
    pub(super) accepted_tcp_connections: usize,
    pub(super) request_body_bytes: usize,
    pub(super) response_body_bytes: usize,
    pub(super) peak_active_connections: usize,
}

#[derive(Clone)]
struct ImmutableObject {
    bytes: Arc<[u8]>,
    checksum_base64: Arc<str>,
}

struct ServerState {
    object: Mutex<Option<ImmutableObject>>,
    stats: Mutex<ConcurrentLoopbackStats>,
}

/// In-memory S3-shaped endpoint with a bounded set of request workers.
pub(super) struct ConcurrentLoopbackServer {
    address: SocketAddr,
    origin: String,
    stopped: Arc<AtomicBool>,
    accept_join: Option<JoinHandle<()>>,
    worker_joins: Vec<JoinHandle<()>>,
    state: Arc<ServerState>,
}

impl ConcurrentLoopbackServer {
    pub(super) fn start(worker_count: usize) -> io::Result<Self> {
        if worker_count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "loopback server requires at least one worker",
            ));
        }
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let origin = format!("http://{address}");
        let state = Arc::new(ServerState {
            object: Mutex::new(None),
            stats: Mutex::new(ConcurrentLoopbackStats::default()),
        });
        let stopped = Arc::new(AtomicBool::new(false));
        let active_connections = Arc::new(AtomicUsize::new(0));
        let peak_active_connections = Arc::new(AtomicUsize::new(0));

        let (sender, receiver) = mpsc::sync_channel::<TcpStream>(worker_count);
        let receiver = Arc::new(Mutex::new(receiver));
        let mut worker_joins = Vec::with_capacity(worker_count);
        for worker_index in 0..worker_count {
            let receiver = Arc::clone(&receiver);
            let state = Arc::clone(&state);
            let active_connections = Arc::clone(&active_connections);
            let peak_active_connections = Arc::clone(&peak_active_connections);
            worker_joins.push(
                thread::Builder::new()
                    .name(format!("backend-store-s3-bench-{worker_index}"))
                    .spawn(move || {
                        loop {
                            let stream = receiver
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .recv();
                            let Ok(stream) = stream else {
                                break;
                            };
                            let active = active_connections.fetch_add(1, Ordering::AcqRel) + 1;
                            peak_active_connections.fetch_max(active, Ordering::AcqRel);
                            let peak = peak_active_connections.load(Ordering::Acquire);
                            state
                                .stats
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .peak_active_connections = peak;
                            let _ = serve_connection(stream, &state);
                            active_connections.fetch_sub(1, Ordering::AcqRel);
                        }
                    })?,
            );
        }

        let accept_stopped = Arc::clone(&stopped);
        let accept_state = Arc::clone(&state);
        let accept_join = thread::Builder::new()
            .name("backend-store-s3-bench-accept".to_owned())
            .spawn(move || {
                while !accept_stopped.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let mut stats = accept_state
                                .stats
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            stats.accepted_tcp_connections =
                                stats.accepted_tcp_connections.saturating_add(1);
                            drop(stats);
                            if sender.send(stream).is_err() {
                                break;
                            }
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            thread::sleep(SERVER_POLL_INTERVAL);
                        }
                        Err(_) => break,
                    }
                }
                drop(sender);
            })?;

        Ok(Self {
            address,
            origin,
            stopped,
            accept_join: Some(accept_join),
            worker_joins,
            state,
        })
    }

    pub(super) fn origin(&self) -> &str {
        &self.origin
    }

    pub(super) fn stats(&self) -> ConcurrentLoopbackStats {
        *self
            .state
            .stats
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Drop for ConcurrentLoopbackServer {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        // Wake the nonblocking accept loop so shutdown does not wait for its
        // next poll interval. A closed wake-up socket is ignored by a worker.
        let _ = TcpStream::connect_timeout(&self.address, Duration::from_millis(100));
        if let Some(join) = self.accept_join.take() {
            let _ = join.join();
        }
        for join in self.worker_joins.drain(..) {
            let _ = join.join();
        }
    }
}

fn serve_connection(mut stream: TcpStream, state: &ServerState) -> io::Result<()> {
    stream.set_read_timeout(Some(SOCKET_TIMEOUT))?;
    stream.set_write_timeout(Some(SOCKET_TIMEOUT))?;
    loop {
        let mut request = match super::super::read_request(stream) {
            Ok(request) => request,
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(error) => return Err(error),
        };
        if request.method.is_empty() {
            return Ok(());
        }
        if request
            .headers
            .get("expect")
            .is_some_and(|value| value.eq_ignore_ascii_case("100-continue"))
        {
            request.stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n")?;
            request.stream.flush()?;
        }
        request.read_all_body()?;

        let method = request.method.clone();
        let request_body_bytes = request.body.len();
        let (response, next_stream) = match request.method.as_str() {
            "PUT" => {
                let requested_checksum = request
                    .headers
                    .get("x-amz-checksum-sha256")
                    .cloned()
                    .unwrap_or_default();
                let digest = Sha256::digest(&request.body);
                let checksum_matches = BASE64
                    .decode(&requested_checksum)
                    .is_ok_and(|requested| requested.as_slice() == digest.as_slice());
                let checksum_base64 = BASE64.encode(digest.as_slice());
                let mut stored = state
                    .object
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let response = if request.headers.get("if-none-match").map(String::as_str)
                    != Some("*")
                    || !checksum_matches
                {
                    super::super::HttpResponse::new(400, "Bad Request", Vec::new())
                } else if stored.is_some() {
                    super::super::HttpResponse::new(412, "Precondition Failed", Vec::new())
                } else {
                    *stored = Some(ImmutableObject {
                        bytes: Arc::from(std::mem::take(&mut request.body)),
                        checksum_base64: Arc::from(checksum_base64.clone()),
                    });
                    super::super::HttpResponse::new(200, "OK", Vec::new())
                        .header("x-amz-checksum-sha256", &checksum_base64)
                };
                let reader = request.reader.into_inner();
                (response, reader)
            }
            "GET" => {
                let stored = state
                    .object
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
                let response = match stored {
                    None => super::super::HttpResponse::new(404, "Not Found", Vec::new()),
                    Some(stored) => match request.headers.get("range") {
                        Some(range) => {
                            let (start, end) =
                                super::super::parse_request_range(range).map_err(|()| {
                                    io::Error::new(io::ErrorKind::InvalidData, "bad byte range")
                                })?;
                            if start > end || end >= stored.bytes.len() {
                                super::super::HttpResponse::new(
                                    416,
                                    "Range Not Satisfiable",
                                    Vec::new(),
                                )
                            } else {
                                let content_range =
                                    format!("bytes {start}-{end}/{}", stored.bytes.len());
                                super::super::HttpResponse::new(
                                    206,
                                    "Partial Content",
                                    stored.bytes[start..=end].to_vec(),
                                )
                                .header("Content-Range", &content_range)
                                .header("ETag", "\"opaque-and-not-an-integrity-hash\"")
                                .header("x-amz-checksum-sha256", &stored.checksum_base64)
                            }
                        }
                        None => super::super::HttpResponse::new(
                            200,
                            "OK",
                            stored.bytes.as_ref().to_vec(),
                        )
                        .header("ETag", "\"opaque-and-not-an-integrity-hash\"")
                        .header("x-amz-checksum-sha256", &stored.checksum_base64),
                    },
                };
                let reader = request.reader.into_inner();
                (response, reader)
            }
            _ => {
                let reader = request.reader.into_inner();
                (
                    super::super::HttpResponse::new(405, "Method Not Allowed", Vec::new()),
                    reader,
                )
            }
        };
        let response_body_bytes = response.body.len();
        let keep_alive = request
            .headers
            .get("connection")
            .is_none_or(|value| !value.eq_ignore_ascii_case("close"));
        let response_result = write_response(&request.stream, response, keep_alive);
        {
            let mut stats = state
                .stats
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            stats.requests = stats.requests.saturating_add(1);
            if method == "PUT" {
                stats.put_requests = stats.put_requests.saturating_add(1);
            } else if method == "GET" {
                stats.get_requests = stats.get_requests.saturating_add(1);
            }
            stats.request_body_bytes = stats.request_body_bytes.saturating_add(request_body_bytes);
            stats.response_body_bytes = stats
                .response_body_bytes
                .saturating_add(response_body_bytes);
        }
        response_result?;
        drop(request.stream);
        if !keep_alive {
            return Ok(());
        }
        stream = next_stream;
    }
}

fn write_response(
    stream: &TcpStream,
    response: super::super::HttpResponse,
    keep_alive: bool,
) -> io::Result<()> {
    let mut writer = stream;
    write!(
        writer,
        "HTTP/1.1 {} {}\r\n",
        response.status, response.reason
    )?;
    write!(writer, "Content-Length: {}\r\n", response.body.len())?;
    if keep_alive {
        writer.write_all(b"Connection: keep-alive\r\n")?;
    } else {
        writer.write_all(b"Connection: close\r\n")?;
    }
    for (name, value) in response.headers {
        write!(writer, "{name}: {value}\r\n")?;
    }
    writer.write_all(b"\r\n")?;
    writer.write_all(&response.body)?;
    writer.flush()
}
