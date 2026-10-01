//! In-process HTTP object store for integration tests that must exercise the
//! production S3 route over real TCP and HTTP range requests.
//!
//! This module is available only with the `test-support` feature. It accepts
//! immutable objects at distinct keys, requires a SigV4-shaped signed query,
//! conditional PUT plus the signed checksum header, and serves exact single
//! byte ranges with S3-compatible metadata.

use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use sha2::{Digest, Sha256};

use crate::{MAX_PACK_BYTES, S3Endpoint, S3ObjectRoute, S3PackRoute, S3RouteConfig};

const MAX_OBJECT_BYTES: usize = MAX_PACK_BYTES as usize;
const LOOPBACK_IO_TIMEOUT: Duration = Duration::from_secs(30);

/// Counts real HTTP operations performed by clients against [`LoopbackS3`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LoopbackS3Stats {
    /// Total accepted requests.
    pub requests: usize,
    /// Requests whose headers were parsed, including those that failed later.
    pub started_requests: usize,
    /// Parsed requests whose response could not be written completely.
    pub failed_requests: usize,
    /// Conditional PutObject requests.
    pub puts: usize,
    /// Full GetObject requests.
    pub full_gets: usize,
    /// Range GetObject requests.
    pub range_gets: usize,
    /// Range requests whose headers were parsed.
    pub started_range_gets: usize,
    /// Parsed range requests whose response did not finish writing.
    pub failed_range_gets: usize,
    /// Total response bytes returned by the server.
    pub response_bytes: usize,
    /// Range header on the most recently started request.
    pub last_range: Option<String>,
    /// Response body size for the most recently started request.
    pub last_response_bytes: Option<usize>,
    /// Last loopback handler error, if a request failed.
    pub last_error: Option<String>,
}

/// Small S3-compatible HTTP service for end-to-end route tests.
///
/// The server stores immutable objects by key. Its `route` method constructs
/// the same production [`S3PackRoute`] used by application code and allows
/// its endpoint only for this server's loopback origin.
pub struct LoopbackS3 {
    address: SocketAddr,
    origin: String,
    objects: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
    stats: Arc<Mutex<LoopbackS3Stats>>,
    stopped: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl LoopbackS3 {
    /// Starts a loopback TCP server on an ephemeral port.
    pub fn start() -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let origin = format!("http://{address}");
        let objects = Arc::new(Mutex::new(BTreeMap::new()));
        let server_objects = Arc::clone(&objects);
        let stats = Arc::new(Mutex::new(LoopbackS3Stats::default()));
        let server_stats = Arc::clone(&stats);
        let stopped = Arc::new(AtomicBool::new(false));
        let server_stopped = Arc::clone(&stopped);
        let join = thread::Builder::new()
            .name("backend-store-s3-loopback".to_owned())
            .spawn(move || {
                while !server_stopped.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            // A nonblocking listener can yield accepted sockets
                            // that are also nonblocking on some platforms.
                            if let Err(error) = stream.set_nonblocking(false) {
                                let mut counters = server_stats
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                                counters.failed_requests += 1;
                                counters.last_error = Some(error.to_string());
                                continue;
                            }
                            server_stats
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .last_range = None;
                            if let Err(error) = serve_one(stream, &server_objects, &server_stats) {
                                let mut counters = server_stats
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                                counters.failed_requests += 1;
                                if counters.last_range.is_some() {
                                    counters.failed_range_gets += 1;
                                }
                                counters.last_error = Some(error.to_string());
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(2));
                        }
                        Err(_) => break,
                    }
                }
            })?;
        Ok(Self {
            address,
            origin,
            objects,
            stats,
            stopped,
            join: Some(join),
        })
    }

    /// Returns the HTTP origin suitable for a test-only publisher capability.
    #[must_use]
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// Builds the production pack route, restricted to this loopback server.
    pub fn route(&self, maximum_object_bytes: u64) -> Result<S3PackRoute, crate::RemoteStoreError> {
        let endpoint = S3Endpoint::loopback_http(&self.origin)?;
        let config =
            S3RouteConfig::new_for_pack_route([endpoint], maximum_object_bytes, 3, Duration::ZERO)?;
        Ok(S3PackRoute::new(S3ObjectRoute::new(config)))
    }

    /// Returns a snapshot of actual HTTP request counts.
    #[must_use]
    pub fn stats(&self) -> LoopbackS3Stats {
        self.stats
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Returns a copy of the immutable object currently held by the server.
    #[must_use]
    pub fn stored_object(&self) -> Option<Vec<u8>> {
        self.objects
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .next()
            .cloned()
    }

    /// Returns the number of immutable object keys stored by the server.
    #[must_use]
    pub fn object_count(&self) -> usize {
        self.objects
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }
}

impl Drop for LoopbackS3 {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        let _ = TcpStream::connect_timeout(&self.address, Duration::from_millis(100));
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

struct Request {
    writer: TcpStream,
    reader: BufReader<TcpStream>,
    method: String,
    target: String,
    signed_query: bool,
    headers: BTreeMap<String, String>,
    body_len: usize,
}

fn read_request(stream: TcpStream) -> std::io::Result<Request> {
    stream.set_read_timeout(Some(LOOPBACK_IO_TIMEOUT))?;
    stream.set_write_timeout(Some(LOOPBACK_IO_TIMEOUT))?;
    let writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    let mut first = String::new();
    reader.read_line(&mut first)?;
    let mut request_line = first.split_whitespace();
    let method = request_line.next().unwrap_or_default().to_owned();
    let raw_target = request_line.next().unwrap_or_default();
    let (target, signed_query) = match raw_target.split_once('?') {
        Some((path, query)) if !path.contains('#') && !query.contains('#') => {
            (path.to_owned(), has_signed_query(query, &method))
        }
        Some(_) => return Err(std::io::Error::other("invalid loopback request target")),
        None => (raw_target.to_owned(), false),
    };
    if !target.starts_with('/') || target.contains('#') {
        return Err(std::io::Error::other("invalid loopback object key"));
    }
    let mut headers = BTreeMap::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        if line == "\r\n" || line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.trim_end().split_once(':') {
            headers.insert(name.to_ascii_lowercase(), value.trim().to_owned());
        }
    }
    let body_len = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    if body_len > MAX_OBJECT_BYTES {
        return Err(std::io::Error::other("loopback object exceeds pack limit"));
    }
    Ok(Request {
        writer,
        reader,
        method,
        target,
        signed_query,
        headers,
        body_len,
    })
}

fn has_signed_query(query: &str, method: &str) -> bool {
    let mut values = BTreeMap::new();
    for field in query.split('&') {
        let Some((raw_name, raw_value)) = field.split_once('=') else {
            return false;
        };
        let Some(name) = super::percent_decode_query_component(raw_name) else {
            return false;
        };
        let Some(value) = super::percent_decode_query_component(raw_value) else {
            return false;
        };
        if name.is_empty() || value.is_empty() || values.insert(name, value).is_some() {
            return false;
        }
    }
    let Some(algorithm) = values.get("X-Amz-Algorithm") else {
        return false;
    };
    let Some(credential) = values.get("X-Amz-Credential") else {
        return false;
    };
    let Some(date) = values.get("X-Amz-Date") else {
        return false;
    };
    let Some(expires) = values
        .get("X-Amz-Expires")
        .and_then(|value| value.parse::<u32>().ok())
    else {
        return false;
    };
    let Some(signed_headers) = values.get("X-Amz-SignedHeaders") else {
        return false;
    };
    let Some(signature) = values.get("X-Amz-Signature") else {
        return false;
    };
    if algorithm != "AWS4-HMAC-SHA256"
        || !credential.contains('/')
        || date.len() != 16
        || !date.ends_with('Z')
        || !(1..=604_800).contains(&expires)
        || signature.len() != 64
        || !signature.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return false;
    }
    let signed_headers = signed_headers
        .split(';')
        .collect::<std::collections::BTreeSet<_>>();
    signed_headers.contains("host")
        && match method {
            "PUT" => {
                signed_headers.contains("if-none-match")
                    && signed_headers.contains("x-amz-checksum-sha256")
            }
            "GET" => true,
            _ => false,
        }
}

fn serve_one(
    stream: TcpStream,
    objects: &Mutex<BTreeMap<String, Vec<u8>>>,
    stats: &Mutex<LoopbackS3Stats>,
) -> std::io::Result<()> {
    let mut request = read_request(stream)?;
    {
        let mut counters = stats
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        counters.started_requests += 1;
        counters.last_range = request.headers.get("range").cloned();
        if counters.last_range.is_some() {
            counters.started_range_gets += 1;
        }
        counters.last_response_bytes = None;
        counters.last_error = None;
    }
    if request
        .headers
        .get("expect")
        .is_some_and(|value| value.eq_ignore_ascii_case("100-continue"))
    {
        request.writer.write_all(b"HTTP/1.1 100 Continue\r\n\r\n")?;
        request.writer.flush()?;
    }
    let mut response = match request.method.as_str() {
        "PUT" => {
            let mut body = vec![0; request.body_len];
            request.reader.read_exact(&mut body)?;
            let checksum = BASE64.encode(Sha256::digest(&body));
            if !request.signed_query {
                Response::new(403, "Forbidden", Vec::new())
            } else if request.headers.get("if-none-match").map(String::as_str) != Some("*")
                || request.headers.get("x-amz-checksum-sha256") != Some(&checksum)
            {
                Response::new(400, "Bad Request", Vec::new())
            } else {
                let mut stored = objects
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if stored.contains_key(&request.target) {
                    Response::new(412, "Precondition Failed", Vec::new())
                } else {
                    stored.insert(request.target.clone(), body);
                    Response::new(200, "OK", Vec::new()).header("x-amz-checksum-sha256", &checksum)
                }
            }
        }
        "GET" => {
            if !request.signed_query {
                Response::new(403, "Forbidden", Vec::new())
            } else {
                let stored = objects
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get(&request.target)
                    .cloned();
                match stored {
                    None => Response::new(404, "Not Found", Vec::new()),
                    Some(bytes) => {
                        let checksum = BASE64.encode(Sha256::digest(&bytes));
                        if let Some(range) = request.headers.get("range") {
                            match parse_range(range, bytes.len()) {
                                Some((start, end)) => Response::new(
                                    206,
                                    "Partial Content",
                                    bytes[start..=end].to_vec(),
                                )
                                .header(
                                    "Content-Range",
                                    &format!("bytes {start}-{end}/{}", bytes.len()),
                                )
                                .header("ETag", "\"opaque-loopback-etag\"")
                                .header("x-amz-checksum-sha256", &checksum),
                                None => Response::new(416, "Range Not Satisfiable", Vec::new()),
                            }
                        } else {
                            Response::new(200, "OK", bytes)
                                .header("ETag", "\"opaque-loopback-etag\"")
                                .header("x-amz-checksum-sha256", &checksum)
                        }
                    }
                }
            }
        }
        _ => Response::new(405, "Method Not Allowed", Vec::new()),
    };
    let is_put = request.method == "PUT";
    let is_range = request.headers.contains_key("range");
    let response_bytes = response.body.len();
    stats
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .last_response_bytes = Some(response_bytes);
    response.write_to(request.writer)?;
    let mut counters = stats
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    counters.requests += 1;
    if is_put {
        counters.puts += 1;
    } else if is_range {
        counters.range_gets += 1;
    } else {
        counters.full_gets += 1;
    }
    counters.response_bytes = counters.response_bytes.saturating_add(response_bytes);
    Ok(())
}

fn parse_range(value: &str, length: usize) -> Option<(usize, usize)> {
    let range = value.strip_prefix("bytes=")?;
    let (start, end) = range.split_once('-')?;
    let start = start.parse::<usize>().ok()?;
    let end = end.parse::<usize>().ok()?;
    (start <= end && end < length).then_some((start, end))
}

struct Response {
    status: u16,
    reason: &'static str,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

impl Response {
    fn new(status: u16, reason: &'static str, body: Vec<u8>) -> Self {
        Self {
            status,
            reason,
            headers: BTreeMap::new(),
            body,
        }
    }

    fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.insert(name.to_owned(), value.to_owned());
        self
    }

    fn write_to(&mut self, mut stream: TcpStream) -> std::io::Result<()> {
        write!(stream, "HTTP/1.1 {} {}\r\n", self.status, self.reason)?;
        write!(
            stream,
            "Content-Length: {}\r\nConnection: close\r\n",
            self.body.len()
        )?;
        for (name, value) in &self.headers {
            write!(stream, "{name}: {value}\r\n")?;
        }
        stream.write_all(b"\r\n")?;
        stream.write_all(&self.body)?;
        stream.flush()
    }
}
