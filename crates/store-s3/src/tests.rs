use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use backend_store::{RelationAdmissionRegistry, TypedObject, UntrustedObjectId};
use backend_version::{ObjectKey, Schema};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use sha2::{Digest, Sha256};

use super::{
    AllowedRange, ObjectEnvelope, PackReadCapability, PackUploadCapability, ReadCapability,
    RemoteStoreError, S3Endpoint, S3ObjectRoute, S3PackBuilder, S3PackRoute, S3RouteConfig,
    UploadCapability, WorkFence,
};

mod retrieval_bench;

struct TestBytesSchema;

impl Schema for TestBytesSchema {
    const DOMAIN: u8 = 0x7d;
    const TYPE: u16 = 19;
    type Value = [u8];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

fn object() -> TypedObject {
    // Keep this baseline object simple; a separate roundtrip exercises a key
    // whose canonical preimage differs from the payload.
    let value = b"s3-route-test-payload".as_slice();
    let key = ObjectKey::<TestBytesSchema>::from_value(value);
    TypedObject::from_value(&key, value)
}

fn object_with_distinct_key_and_value() -> TypedObject {
    let key = ObjectKey::<TestBytesSchema>::from_value(b"stable-logical-key");
    TypedObject::from_value(&key, b"independent-object-payload".as_slice())
}

fn pack_object(index: usize, payload_bytes: usize) -> TypedObject {
    let key_bytes = format!("pack-key-{index:06}").into_bytes();
    let payload = vec![(index % 251) as u8; payload_bytes];
    let key = ObjectKey::<TestBytesSchema>::from_value(key_bytes.as_slice());
    TypedObject::from_value(&key, payload.as_slice())
}

fn build_test_pack(
    count: usize,
    payload_bytes: usize,
    max_pack_bytes: u64,
    reverse: bool,
) -> (super::ImmutableS3Pack, Vec<backend_store::ObjectId>) {
    let mut builder = S3PackBuilder::new(max_pack_bytes).expect("valid pack ceiling");
    let indexes: Box<dyn Iterator<Item = usize>> = if reverse {
        Box::new((0..count).rev())
    } else {
        Box::new(0..count)
    };
    let mut ids = Vec::with_capacity(count);
    for index in indexes {
        let object = pack_object(index, payload_bytes);
        ids.push(object.id());
        builder.push(&object).expect("stage typed object");
    }
    ids.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
    let pack = builder.finish().expect("finish deterministic pack");
    (pack, ids)
}

fn fence() -> WorkFence {
    WorkFence {
        namespace_id: [0x19; 16],
        work_id: [0x31; 16],
        attempt: 7,
        fence: [0x54; 32],
        closure_root: [0x82; 32],
    }
}

#[derive(Clone, Copy)]
enum ServerMode {
    Store,
    DisconnectMidPut,
    CommitThenDropPutResponse,
    DropFirstGet,
    RedirectGet,
    BadRange,
    TamperRange,
    BadChecksum,
    BadPutChecksum,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct LoopbackStats {
    requests: usize,
    put_requests: usize,
    get_requests: usize,
    request_body_bytes: usize,
    response_body_bytes: usize,
}

struct LoopbackServer {
    origin: String,
    signature_url: String,
    object: Arc<Mutex<Option<Vec<u8>>>>,
    stats: Arc<Mutex<LoopbackStats>>,
    join: Option<thread::JoinHandle<()>>,
}

const MAX_TEST_BODY_BYTES: usize = 64 * 1024 * 1024;
const MAX_TEST_BODY_IDLE: Duration = Duration::from_secs(60);
const MAX_TEST_BODY_DURATION: Duration = Duration::from_secs(120);

impl LoopbackServer {
    fn start(mode: ServerMode, requests: usize, initial: Option<Vec<u8>>) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind loopback S3 test server");
        listener
            .set_nonblocking(true)
            .expect("make loopback listener nonblocking");
        let address = listener
            .local_addr()
            .expect("read loopback listener address");
        let origin = format!("http://{address}");
        let signature_url = format!(
            "{origin}/bucket/object?X-Amz-Signature=secret-token&X-Amz-SignedHeaders=host%3Bif-none-match%3Bx-amz-checksum-sha256%3Brange"
        );
        let object = Arc::new(Mutex::new(initial));
        let server_object = Arc::clone(&object);
        let stats = Arc::new(Mutex::new(LoopbackStats::default()));
        let server_stats = Arc::clone(&stats);
        let join = thread::spawn(move || {
            for request_number in 0..requests {
                let deadline = Instant::now() + Duration::from_secs(5);
                let (stream, _) = loop {
                    match listener.accept() {
                        Ok(connection) => break connection,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            if Instant::now() >= deadline {
                                return;
                            }
                            thread::sleep(Duration::from_millis(10));
                        }
                        Err(_) => return,
                    }
                };
                if stream.set_nonblocking(false).is_err() {
                    return;
                }
                let mut request = match read_request(stream) {
                    Ok(request) => request,
                    Err(_) => return,
                };
                if request
                    .headers
                    .get("expect")
                    .is_some_and(|value| value.eq_ignore_ascii_case("100-continue"))
                {
                    if request
                        .stream
                        .write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
                        .and_then(|()| request.stream.flush())
                        .is_err()
                    {
                        return;
                    }
                }
                {
                    let mut counters = server_stats.lock().expect("test stats mutex");
                    counters.requests += 1;
                    if request.method == "PUT" {
                        counters.put_requests += 1;
                    } else if request.method == "GET" {
                        counters.get_requests += 1;
                    }
                }
                let first_put = request.method == "PUT" && request_number == 0;
                if first_put && matches!(mode, ServerMode::DisconnectMidPut) {
                    let partial_read = if request.chunked {
                        request.read_chunked_prefix()
                    } else {
                        request.read_body((request.body_len / 2).max(1))
                    };
                    if partial_read.is_err() {
                        return;
                    }
                    server_stats
                        .lock()
                        .expect("test stats mutex")
                        .request_body_bytes += request.body.len();
                    continue;
                }
                if request.read_all_body().is_err() {
                    return;
                }
                server_stats
                    .lock()
                    .expect("test stats mutex")
                    .request_body_bytes += request.body.len();
                if request.method == "GET"
                    && request_number == 0
                    && matches!(mode, ServerMode::DropFirstGet)
                {
                    continue;
                }
                let response = match request.method.as_str() {
                    "PUT" => {
                        let mut stored = server_object.lock().expect("test object mutex");
                        let checksum_matches = request
                            .headers
                            .get("x-amz-checksum-sha256")
                            .and_then(|checksum| BASE64.decode(checksum).ok())
                            .is_some_and(|checksum| {
                                checksum.as_slice() == Sha256::digest(&request.body).as_slice()
                            });
                        if request.headers.get("if-none-match").map(String::as_str) != Some("*")
                            || !checksum_matches
                        {
                            HttpResponse::new(400, "Bad Request", Vec::new())
                        } else if stored.is_some() {
                            HttpResponse::new(412, "Precondition Failed", Vec::new())
                        } else {
                            *stored = Some(request.body);
                            if first_put && matches!(mode, ServerMode::CommitThenDropPutResponse) {
                                continue;
                            }
                            if matches!(mode, ServerMode::BadPutChecksum) {
                                HttpResponse::new(200, "OK", Vec::new())
                                    .header("x-amz-checksum-sha256", &BASE64.encode([0_u8; 32]))
                            } else {
                                HttpResponse::new(200, "OK", Vec::new())
                            }
                        }
                    }
                    "GET" if matches!(mode, ServerMode::RedirectGet) => {
                        HttpResponse::new(302, "Found", Vec::new()).header(
                            "Location",
                            "http://127.0.0.1:9/exfiltrate?token=not-followed",
                        )
                    }
                    "GET" => {
                        let stored = server_object.lock().expect("test object mutex").clone();
                        match stored {
                            None => HttpResponse::new(404, "Not Found", Vec::new()),
                            Some(bytes) => match request.headers.get("range") {
                                Some(range) => {
                                    let Ok((start, end)) = parse_request_range(range) else {
                                        return;
                                    };
                                    if end >= bytes.len() || start > end {
                                        HttpResponse::new(416, "Range Not Satisfiable", Vec::new())
                                    } else {
                                        let mut range_body = bytes[start..=end].to_vec();
                                        if matches!(mode, ServerMode::TamperRange) && start > 0 {
                                            if let Some(first) = range_body.first_mut() {
                                                *first ^= 0x80;
                                            }
                                        }
                                        let content_range = if matches!(mode, ServerMode::BadRange)
                                        {
                                            format!("bytes {}-{}/{}", start + 1, end, bytes.len())
                                        } else {
                                            format!("bytes {start}-{end}/{}", bytes.len())
                                        };
                                        let checksum = if matches!(mode, ServerMode::BadChecksum) {
                                            BASE64.encode([0_u8; 32])
                                        } else {
                                            BASE64.encode(Sha256::digest(&bytes))
                                        };
                                        HttpResponse::new(206, "Partial Content", range_body)
                                            .header("Content-Range", &content_range)
                                            .header("ETag", "\"opaque-and-not-an-integrity-hash\"")
                                            .header("x-amz-checksum-sha256", &checksum)
                                    }
                                }
                                None => {
                                    let checksum = BASE64.encode(Sha256::digest(&bytes));
                                    HttpResponse::new(200, "OK", bytes)
                                        .header("ETag", "\"opaque-and-not-an-integrity-hash\"")
                                        .header("x-amz-checksum-sha256", &checksum)
                                }
                            },
                        }
                    }
                    _ => HttpResponse::new(405, "Method Not Allowed", Vec::new()),
                };
                let response_body_len = response.body.len();
                if write_response(request.stream, response).is_err() {
                    return;
                }
                server_stats
                    .lock()
                    .expect("test stats mutex")
                    .response_body_bytes += response_body_len;
            }
        });
        Self {
            origin,
            signature_url,
            object,
            stats,
            join: Some(join),
        }
    }

    fn route(&self) -> S3ObjectRoute {
        self.route_with_max(2 * 1024 * 1024)
    }

    fn route_with_max(&self, max_object_bytes: u64) -> S3ObjectRoute {
        let endpoint = S3Endpoint::loopback_http(&self.origin).expect("valid loopback origin");
        let config = S3RouteConfig::new([endpoint], max_object_bytes, 3, Duration::ZERO)
            .expect("valid route config");
        S3ObjectRoute::new(config)
    }

    fn stats(&self) -> LoopbackStats {
        *self.stats.lock().expect("test stats mutex")
    }

    fn finish(mut self) -> Arc<Mutex<Option<Vec<u8>>>> {
        if let Some(join) = self.join.take() {
            join.join().expect("loopback server thread");
        }
        Arc::clone(&self.object)
    }
}

impl Drop for LoopbackServer {
    fn drop(&mut self) {
        // Each test configures an exact request count. If a route rejects a
        // capability before making a request, the server thread is detached;
        // the listener then drops with its thread when the test process exits.
        // The explicit `finish` method is used whenever requests are expected.
    }
}

struct HttpRequest {
    stream: TcpStream,
    reader: BufReader<TcpStream>,
    method: String,
    headers: BTreeMap<String, String>,
    body_len: usize,
    chunked: bool,
    body_deadline: Instant,
    body: Vec<u8>,
}

impl HttpRequest {
    fn read_body(&mut self, bytes: usize) -> std::io::Result<()> {
        let mut remaining = bytes;
        let mut buffer = [0_u8; 32 * 1024];
        let mut last_progress = Instant::now();
        while remaining > 0 {
            let count = remaining.min(buffer.len());
            match self.reader.read(&mut buffer[..count]) {
                Ok(0) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "client closed request body",
                    ));
                }
                Ok(read) => {
                    if self.body.len().saturating_add(read) > MAX_TEST_BODY_BYTES {
                        return Err(std::io::Error::other("test request body too large"));
                    }
                    self.body.extend_from_slice(&buffer[..read]);
                    remaining -= read;
                    last_progress = Instant::now();
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) && Instant::now() < self.body_deadline
                        && last_progress.elapsed() < MAX_TEST_BODY_IDLE =>
                {
                    continue;
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    fn read_all_body(&mut self) -> std::io::Result<()> {
        if !self.chunked {
            if self.body_len > MAX_TEST_BODY_BYTES {
                return Err(std::io::Error::other("test request body too large"));
            }
            return self.read_body(self.body_len);
        }
        loop {
            let mut line = String::new();
            self.reader.read_line(&mut line)?;
            let size = line.trim().split(';').next().unwrap_or_default();
            let size = usize::from_str_radix(size, 16)
                .map_err(|_| std::io::Error::other("invalid HTTP chunk length"))?;
            if size == 0 {
                loop {
                    line.clear();
                    self.reader.read_line(&mut line)?;
                    if line == "\r\n" || line.is_empty() {
                        return Ok(());
                    }
                }
            }
            if self.body.len().saturating_add(size) > MAX_TEST_BODY_BYTES {
                return Err(std::io::Error::other("test request body too large"));
            }
            self.read_body(size)?;
            let mut crlf = [0_u8; 2];
            self.read_control_exact(&mut crlf)?;
            if crlf != *b"\r\n" {
                return Err(std::io::Error::other("invalid HTTP chunk terminator"));
            }
        }
    }

    fn read_control_exact(&mut self, output: &mut [u8]) -> std::io::Result<()> {
        let mut offset = 0;
        let mut last_progress = Instant::now();
        while offset < output.len() {
            match self.reader.read(&mut output[offset..]) {
                Ok(0) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "client closed chunked request body",
                    ));
                }
                Ok(read) => {
                    offset += read;
                    last_progress = Instant::now();
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) && Instant::now() < self.body_deadline
                        && last_progress.elapsed() < MAX_TEST_BODY_IDLE =>
                {
                    continue;
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    fn read_chunked_prefix(&mut self) -> std::io::Result<()> {
        let mut line = String::new();
        self.reader.read_line(&mut line)?;
        let size = line.trim().split(';').next().unwrap_or_default();
        let size = usize::from_str_radix(size, 16)
            .map_err(|_| std::io::Error::other("invalid HTTP chunk length"))?;
        self.read_body((size / 2).max(1).min(size))
    }
}

fn read_request(stream: TcpStream) -> std::io::Result<HttpRequest> {
    // Large local pack PUTs can be contending with other loopback tests. Keep
    // each socket wait bounded but retry transient read timeouts while body
    // bytes continue to arrive.
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    stream.set_write_timeout(Some(Duration::from_secs(30)))?;
    let writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    let mut first_line = String::new();
    reader.read_line(&mut first_line)?;
    let method = first_line
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_owned();
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
        .and_then(|length| length.parse().ok())
        .unwrap_or(0);
    let chunked = headers.get("transfer-encoding").is_some_and(|encoding| {
        encoding
            .split(',')
            .any(|part| part.trim().eq_ignore_ascii_case("chunked"))
    });
    Ok(HttpRequest {
        stream: writer,
        reader,
        method,
        headers,
        body_len,
        chunked,
        body_deadline: Instant::now() + MAX_TEST_BODY_DURATION,
        body: Vec::with_capacity(body_len),
    })
}

struct HttpResponse {
    status: u16,
    reason: &'static str,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

impl HttpResponse {
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
}

fn write_response(stream: TcpStream, response: HttpResponse) -> std::io::Result<()> {
    let mut writer = stream;
    write!(
        writer,
        "HTTP/1.1 {} {}\r\n",
        response.status, response.reason
    )?;
    write!(writer, "Content-Length: {}\r\n", response.body.len())?;
    write!(writer, "Connection: close\r\n")?;
    for (name, value) in response.headers {
        write!(writer, "{name}: {value}\r\n")?;
    }
    writer.write_all(b"\r\n")?;
    writer.write_all(&response.body)?;
    writer.flush()
}

fn parse_request_range(raw: &str) -> Result<(usize, usize), ()> {
    let Some(raw) = raw.strip_prefix("bytes=") else {
        return Err(());
    };
    let Some((start, end)) = raw.split_once('-') else {
        return Err(());
    };
    let Ok(start) = start.parse() else {
        return Err(());
    };
    let Ok(end) = end.parse() else {
        return Err(());
    };
    Ok((start, end))
}

fn expires_later() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("test clock after epoch")
        .as_secs()
        + 3_600
}

fn stored_bytes(envelope: &ObjectEnvelope) -> Vec<u8> {
    let mut file = envelope.file.open().expect("open object spool");
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).expect("read object spool");
    bytes
}

fn upload_capability(
    server: &LoopbackServer,
    envelope: &ObjectEnvelope,
    fence: WorkFence,
) -> UploadCapability {
    UploadCapability::new(
        server.signature_url.clone(),
        envelope.id(),
        envelope.len(),
        *envelope.sha256(),
        expires_later(),
        fence,
        true,
        true,
    )
}

fn read_capability(
    server: &LoopbackServer,
    envelope: &ObjectEnvelope,
    fence: WorkFence,
    exact_range: Option<AllowedRange>,
) -> ReadCapability {
    ReadCapability::new(
        server.signature_url.clone(),
        envelope.id(),
        envelope.len(),
        expires_later(),
        fence,
        exact_range,
        exact_range.is_some(),
    )
}

#[test]
fn interrupted_put_retries_and_a_fresh_route_cold_reads_the_same_object_id() {
    let object = object();
    let envelope = ObjectEnvelope::from_typed(&object, 2 * 1024 * 1024).expect("encode envelope");
    let expected_bytes = stored_bytes(&envelope);
    let server = LoopbackServer::start(ServerMode::DisconnectMidPut, 3, None);
    let grant = upload_capability(&server, &envelope, fence());
    let config_route = server.route();
    let receipt = config_route
        .put_object(
            &object,
            &grant,
            fence(),
            None,
            &RelationAdmissionRegistry::default(),
        )
        .expect("retry interrupted PutObject");
    assert_eq!(receipt.object_id(), envelope.id());
    assert!(!receipt.reused_after_412());
    drop(config_route);

    // Reconstructing the route proves that remote cold reads do not depend on
    // writer state or an in-memory upload cache, as after a process restart.
    let cold_route = server.route();
    let full_read = read_capability(&server, &envelope, fence(), None);
    let checked = cold_route
        .get_full_object(&full_read, fence(), &RelationAdmissionRegistry::default())
        .expect("read checked object from cold storage");
    assert_eq!(checked.id(), envelope.id());
    let mut got = Vec::new();
    checked
        .open_envelope()
        .expect("open cold envelope")
        .read_to_end(&mut got)
        .expect("read cold envelope");
    assert_eq!(got, expected_bytes);
    let saved = server.finish();
    let stored = saved.lock().expect("test object mutex").clone();
    assert_eq!(stored, Some(expected_bytes));
}

#[test]
fn conditional_retry_after_lost_ack_accepts_only_a_full_exact_readback() {
    let object = object();
    let envelope = ObjectEnvelope::from_typed(&object, 2 * 1024 * 1024).expect("encode envelope");
    let expected_bytes = stored_bytes(&envelope);
    let server = LoopbackServer::start(ServerMode::CommitThenDropPutResponse, 3, None);
    let grant = upload_capability(&server, &envelope, fence());
    let read = read_capability(&server, &envelope, fence(), None);
    let receipt = server
        .route()
        .put_object(
            &object,
            &grant,
            fence(),
            Some(&read),
            &RelationAdmissionRegistry::default(),
        )
        .expect("verify already-stored identical bytes");
    assert!(receipt.reused_after_412());
    let saved = server.finish();
    let stored = saved.lock().expect("test object mutex").clone();
    assert_eq!(stored, Some(expected_bytes));
}

#[test]
fn independently_keyed_object_roundtrips_through_s3_and_cold_read() {
    let object = object_with_distinct_key_and_value();
    let envelope = ObjectEnvelope::from_typed(&object, 2 * 1024 * 1024).expect("encode envelope");
    let expected_bytes = stored_bytes(&envelope);
    let server = LoopbackServer::start(ServerMode::Store, 2, None);
    let write_route = server.route();
    let upload = upload_capability(&server, &envelope, fence());
    let receipt = write_route
        .put_object(
            &object,
            &upload,
            fence(),
            None,
            &RelationAdmissionRegistry::default(),
        )
        .expect("store independently keyed object");
    assert_eq!(receipt.object_id(), object.id());
    drop(write_route);

    let read_route = server.route();
    let read = read_capability(&server, &envelope, fence(), None);
    let checked = read_route
        .get_full_object(&read, fence(), &RelationAdmissionRegistry::default())
        .expect("admit independently keyed cold object");
    assert_eq!(checked.id(), object.id());
    let mut got = Vec::new();
    checked
        .open_envelope()
        .expect("open cold object envelope")
        .read_to_end(&mut got)
        .expect("read cold object envelope");
    assert_eq!(got, expected_bytes);
    let saved = server.finish();
    assert_eq!(
        saved.lock().expect("test object mutex").as_ref(),
        Some(&expected_bytes)
    );
}

#[test]
fn range_get_requires_exact_206_metadata_and_returns_unverified_bytes() {
    let object = object();
    let envelope = ObjectEnvelope::from_typed(&object, 2 * 1024 * 1024).expect("encode envelope");
    let bytes = stored_bytes(&envelope);
    let range = AllowedRange {
        start: 11,
        end_inclusive: 37,
    };
    let server = LoopbackServer::start(ServerMode::Store, 1, Some(bytes.clone()));
    let grant = read_capability(&server, &envelope, fence(), Some(range));
    let fetched = server
        .route()
        .get_range(&grant, range, fence())
        .expect("read S3 byte range");
    assert_eq!(fetched.object_id(), envelope.id().as_bytes());
    assert_eq!(fetched.total_bytes(), envelope.len());
    assert_eq!(fetched.range(), range);
    assert_eq!(fetched.bytes(), &bytes[11..=37]);
    server.finish();

    let bad_server = LoopbackServer::start(ServerMode::BadRange, 1, Some(bytes));
    let bad_grant = read_capability(&bad_server, &envelope, fence(), Some(range));
    assert!(matches!(
        bad_server.route().get_range(&bad_grant, range, fence()),
        Err(RemoteStoreError::Protocol)
    ));
    bad_server.finish();
}

#[test]
fn range_get_retries_a_dropped_connection_without_relaxing_range_admission() {
    let object = object();
    let envelope = ObjectEnvelope::from_typed(&object, 2 * 1024 * 1024).expect("encode envelope");
    let bytes = stored_bytes(&envelope);
    let range = AllowedRange {
        start: 11,
        end_inclusive: 37,
    };
    let server = LoopbackServer::start(ServerMode::DropFirstGet, 2, Some(bytes.clone()));
    let grant = read_capability(&server, &envelope, fence(), Some(range));
    let fetched = server
        .route()
        .get_range(&grant, range, fence())
        .expect("retry a dropped idempotent range request");
    assert_eq!(fetched.bytes(), &bytes[11..=37]);
    assert_eq!(server.stats().get_requests, 2);
    server.finish();
}

#[test]
fn hostile_redirects_stale_fences_and_mismatched_claims_are_rejected() {
    let object = object();
    let envelope = ObjectEnvelope::from_typed(&object, 2 * 1024 * 1024).expect("encode envelope");
    let server = LoopbackServer::start(ServerMode::RedirectGet, 1, None);
    let route = server.route();
    let read = read_capability(&server, &envelope, fence(), None);
    assert!(matches!(
        route.get_full_object(&read, fence(), &RelationAdmissionRegistry::default()),
        Err(RemoteStoreError::Protocol)
    ));
    server.finish();

    let server = LoopbackServer::start(ServerMode::Store, 0, None);
    let route = server.route();
    let stale = WorkFence {
        attempt: fence().attempt + 1,
        ..fence()
    };
    let grant = upload_capability(&server, &envelope, stale);
    assert!(matches!(
        route.put_object(
            &object,
            &grant,
            fence(),
            None,
            &RelationAdmissionRegistry::default(),
        ),
        Err(RemoteStoreError::StaleFence)
    ));
    let stale_persisted_fence = WorkFence {
        fence: [0x55; 32],
        ..fence()
    };
    let stale_persisted_fence_grant = upload_capability(&server, &envelope, stale_persisted_fence);
    assert!(matches!(
        route.put_object(
            &object,
            &stale_persisted_fence_grant,
            fence(),
            None,
            &RelationAdmissionRegistry::default(),
        ),
        Err(RemoteStoreError::StaleFence)
    ));

    let unsigned_headers_url = server.signature_url.replace(
        "host%3Bif-none-match%3Bx-amz-checksum-sha256%3Brange",
        "host",
    );
    let unsigned_headers = UploadCapability::new(
        unsigned_headers_url,
        envelope.id(),
        envelope.len(),
        *envelope.sha256(),
        expires_later(),
        fence(),
        true,
        true,
    );
    assert!(matches!(
        route.put_object(
            &object,
            &unsigned_headers,
            fence(),
            None,
            &RelationAdmissionRegistry::default(),
        ),
        Err(RemoteStoreError::Capability)
    ));

    let zero_fence = WorkFence {
        fence: [0; 32],
        ..fence()
    };
    let zero_fence_grant = upload_capability(&server, &envelope, zero_fence);
    assert!(matches!(
        route.put_object(
            &object,
            &zero_fence_grant,
            zero_fence,
            None,
            &RelationAdmissionRegistry::default(),
        ),
        Err(RemoteStoreError::Capability)
    ));
    let zero_fence_read = ReadCapability::new(
        server.signature_url.clone(),
        envelope.id(),
        envelope.len(),
        expires_later(),
        zero_fence,
        None,
        false,
    );
    assert!(matches!(
        route.get_full_object(
            &zero_fence_read,
            zero_fence,
            &RelationAdmissionRegistry::default(),
        ),
        Err(RemoteStoreError::Capability)
    ));

    let bad_checksum = UploadCapability::new(
        server.signature_url.clone(),
        envelope.id(),
        envelope.len(),
        [0x55; 32],
        expires_later(),
        fence(),
        true,
        true,
    );
    assert!(matches!(
        route.put_object(
            &object,
            &bad_checksum,
            fence(),
            None,
            &RelationAdmissionRegistry::default(),
        ),
        Err(RemoteStoreError::Identity)
    ));

    let expired = ReadCapability::new(
        server.signature_url.clone(),
        envelope.id(),
        envelope.len(),
        1,
        fence(),
        None,
        false,
    );
    assert!(matches!(
        route.get_full_object(&expired, fence(), &RelationAdmissionRegistry::default()),
        Err(RemoteStoreError::Capability)
    ));

    let other = TypedObject::from_value(
        &ObjectKey::<TestBytesSchema>::from_value(b"different-key"),
        b"different-object".as_slice(),
    );
    server.finish();
    let wrong_id_server =
        LoopbackServer::start(ServerMode::Store, 2, Some(stored_bytes(&envelope)));
    let wrong_size_grant = ReadCapability::new(
        wrong_id_server.signature_url.clone(),
        envelope.id(),
        envelope.len() + 1,
        expires_later(),
        fence(),
        None,
        false,
    );
    assert!(matches!(
        wrong_id_server.route().get_full_object(
            &wrong_size_grant,
            fence(),
            &RelationAdmissionRegistry::default(),
        ),
        Err(RemoteStoreError::Identity)
    ));
    let wrong_id_grant = ReadCapability::new(
        wrong_id_server.signature_url.clone(),
        other.id(),
        envelope.len(),
        expires_later(),
        fence(),
        None,
        false,
    );
    assert!(matches!(
        wrong_id_server.route().get_full_object(
            &wrong_id_grant,
            fence(),
            &RelationAdmissionRegistry::default(),
        ),
        Err(RemoteStoreError::Identity)
    ));
    wrong_id_server.finish();
}

fn pack_upload_capability(
    server: &LoopbackServer,
    pack: &super::ImmutableS3Pack,
    work_fence: WorkFence,
) -> PackUploadCapability {
    PackUploadCapability::new(
        server.signature_url.replace(
            "host%3Bif-none-match%3Bx-amz-checksum-sha256%3Brange",
            "host%3Bif-none-match%3Bx-amz-checksum-sha256",
        ),
        pack.pack_id(),
        pack.layout_id(),
        pack.len(),
        pack.manifest_bytes(),
        *pack.sha256(),
        pack.manifest_sha256(),
        expires_later(),
        work_fence,
        true,
        true,
    )
}

fn pack_read_capability(
    server: &LoopbackServer,
    pack: &super::ImmutableS3Pack,
    work_fence: WorkFence,
    full_read_allowed: bool,
) -> PackReadCapability {
    PackReadCapability::new(
        server.signature_url.replace(
            "host%3Bif-none-match%3Bx-amz-checksum-sha256%3Brange",
            "host",
        ),
        pack.pack_id(),
        pack.layout_id(),
        pack.len(),
        pack.manifest_bytes(),
        *pack.sha256(),
        pack.manifest_sha256(),
        expires_later(),
        work_fence,
        full_read_allowed,
    )
}

#[test]
fn hundreds_of_small_objects_share_one_put_and_cold_ranges_verify_before_admission() {
    // 1,024 independently keyed 8 KiB envelopes make an approximately 9 MiB
    // pack: near the recommended target, but well below the single-PUT ceiling.
    let (pack, ids) = build_test_pack(1_024, 8 * 1024, 16 * 1024 * 1024, false);
    assert_eq!(pack.object_count(), 1_024);
    assert!(pack.len() >= super::RECOMMENDED_PACK_BYTES);
    assert!(pack.len() <= 16 * 1024 * 1024);
    assert!(pack.manifest_bytes() as usize <= super::MAX_PACK_MANIFEST_BYTES);
    assert!(pack.retained_metadata_bytes() < 4 * 1024 * 1024);

    let requested_ids = [ids[4], ids[519], ids[4]];
    let mut requested_pages = BTreeMap::new();
    for id in requested_ids {
        let page = pack
            .manifest()
            .page_for_object(id)
            .expect("root selects one page");
        requested_pages.insert(page.offset(), u64::from(page.bytes()));
    }
    let request_count = 2 + requested_pages.len() + requested_ids.len();
    let server = LoopbackServer::start(ServerMode::Store, request_count, None);
    let route = S3PackRoute::new(server.route_with_max(16 * 1024 * 1024));
    let upload = pack_upload_capability(&server, &pack, fence());
    let receipt = route
        .put_pack(&pack, &upload, fence(), None)
        .unwrap_or_else(|error| {
            panic!(
                "one conditional whole-pack PUT: {error:?}; stats={:?}",
                server.stats()
            )
        });
    assert_eq!(receipt.pack_id(), pack.pack_id());
    assert_eq!(receipt.layout_id(), pack.layout_id());
    assert_eq!(receipt.object_count(), 1_024);
    assert_eq!(receipt.pack_bytes(), pack.len());
    assert_eq!(receipt.fence(), fence());

    // A new route reads only the root directory. Each object lookup then reads
    // one bounded authenticated page and the complete envelope. ETag is
    // deliberately opaque and carries no integrity meaning.
    let cold_route = S3PackRoute::new(server.route_with_max(16 * 1024 * 1024));
    let read = pack_read_capability(&server, &pack, fence(), true);
    let reopened = cold_route
        .open_pack(&read, fence())
        .unwrap_or_else(|error| {
            panic!(
                "reopen canonical pack manifest from S3: {error:?}; stats={:?}",
                server.stats()
            )
        });
    assert_eq!(reopened.manifest().pack_id(), pack.pack_id());
    assert_eq!(reopened.manifest().layout_id(), pack.layout_id());
    assert_eq!(reopened.manifest().object_count(), 1_024);

    let checked_objects = cold_route
        .get_objects(
            &reopened,
            &requested_ids,
            fence(),
            &RelationAdmissionRegistry::default(),
        )
        .expect("resolve pages once, then admit bounded full envelopes");
    assert_eq!(checked_objects.len(), requested_ids.len());
    for (checked, id) in checked_objects.iter().zip(requested_ids) {
        assert_eq!(checked.id(), id);
        assert_eq!(checked.pack_id(), pack.pack_id());
        assert_eq!(checked.layout_id(), pack.layout_id());
        assert_eq!(checked.fence(), fence());
    }
    let stats_handle = Arc::clone(&server.stats);
    let saved = server.finish();
    let stored = saved
        .lock()
        .expect("test object mutex")
        .clone()
        .expect("stored pack");
    assert_eq!(stored.len() as u64, pack.len());
    let stats = *stats_handle.lock().expect("test stats mutex");
    assert_eq!(stats.put_requests, 1);
    assert_eq!(stats.get_requests, request_count - 1);
    assert_eq!(stats.requests, request_count);
    assert_eq!(stats.request_body_bytes as u64, pack.len());
    let downloaded = u64::from(pack.manifest_bytes())
        + requested_pages.values().sum::<u64>()
        + requested_ids
            .iter()
            .map(|id| {
                pack.manifest()
                    .extents()
                    .iter()
                    .find(|extent| extent.object_id_bytes() == id.as_bytes())
                    .expect("local manifest has full extents")
                    .length()
            })
            .sum::<u64>();
    assert_eq!(stats.response_body_bytes as u64, downloaded);
}

#[test]
fn lost_pack_put_ack_requires_full_readback_and_deterministic_repack_keeps_object_ids() {
    let (pack, ids) = build_test_pack(192, 384, 4 * 1024 * 1024, false);
    let (repacked, reversed_ids) = build_test_pack(192, 384, 4 * 1024 * 1024, true);
    assert_eq!(ids, reversed_ids);
    assert_eq!(pack.layout_id(), repacked.layout_id());
    assert_eq!(pack.pack_id(), repacked.pack_id());

    let server = LoopbackServer::start(ServerMode::CommitThenDropPutResponse, 3, None);
    let route = S3PackRoute::new(server.route_with_max(4 * 1024 * 1024));
    let upload = pack_upload_capability(&server, &pack, fence());
    let read = pack_read_capability(&server, &pack, fence(), true);
    let receipt = route
        .put_pack(&pack, &upload, fence(), Some(&read))
        .unwrap_or_else(|error| {
            panic!(
                "retry conditional PUT and compare full cold bytes: {error:?}; stats={:?}",
                server.stats()
            )
        });
    assert!(receipt.reused_after_412());
    assert_eq!(receipt.pack_id(), pack.pack_id());
    let stats_handle = Arc::clone(&server.stats);
    let saved = server.finish();
    let stored = saved
        .lock()
        .expect("test object mutex")
        .clone()
        .expect("stored pack");
    assert_eq!(Sha256::digest(&stored).as_slice(), pack.sha256());
    let stats = *stats_handle.lock().expect("test stats mutex");
    assert_eq!(stats.put_requests, 2);
    assert_eq!(stats.get_requests, 1);
    assert_eq!(stats.requests, 3);
}

#[test]
fn coalesced_cold_open_prefetches_only_a_bounded_verified_directory_prefix() {
    let (pack, ids) = build_test_pack(320, 128, 4 * 1024 * 1024, false);
    let upper_bound = backend_store::artifact_pack::artifact_pack_directory_prefix_upper_bound(
        pack.manifest_bytes(),
    )
    .expect("canonical root has a bounded directory prefix");
    assert!(upper_bound <= super::MAX_COALESCED_DIRECTORY_PREFIX_BYTES);
    let prefix_bytes = u64::try_from(upper_bound)
        .expect("prefix fits")
        .min(pack.len());
    let server = LoopbackServer::start(ServerMode::Store, 4, None);
    let route = S3PackRoute::new(server.route_with_max(4 * 1024 * 1024))
        .with_coalesced_directory_prefetch(super::MAX_COALESCED_DIRECTORY_PREFIX_BYTES)
        .expect("enable explicit RTT-oriented policy");
    let upload = pack_upload_capability(&server, &pack, fence());
    route
        .put_pack(&pack, &upload, fence(), None)
        .expect("conditionally store immutable pack");
    let read = pack_read_capability(&server, &pack, fence(), true);
    let reopened = route
        .open_pack(&read, fence())
        .expect("coalesced prefix contains root and every page");
    assert_eq!(reopened.prefetched_prefix_bytes(), prefix_bytes);
    assert!(matches!(
        S3PackRoute::new(server.route_with_max(4 * 1024 * 1024))
            .with_coalesced_directory_prefetch(super::MAX_COALESCED_DIRECTORY_PREFIX_BYTES + 1),
        Err(RemoteStoreError::Bounds)
    ));
    let selected = [ids[4], ids[219]];
    let objects = route
        .get_objects(
            &reopened,
            &selected,
            fence(),
            &RelationAdmissionRegistry::default(),
        )
        .expect("cached pages still admit complete object envelopes");
    assert_eq!(objects[0].id(), selected[0]);
    assert_eq!(objects[1].id(), selected[1]);
    let stats_handle = Arc::clone(&server.stats);
    let saved = server.finish();
    assert!(saved.lock().expect("test object mutex").is_some());
    let stats = *stats_handle.lock().expect("test stats mutex");
    assert_eq!(stats.requests, 4);
    assert_eq!(stats.put_requests, 1);
    assert_eq!(stats.get_requests, 3); // one coalesced prefix, two full envelopes
    let envelope_bytes = pack
        .manifest()
        .extents()
        .iter()
        .filter(|extent| {
            selected
                .iter()
                .any(|id| id.as_bytes() == extent.object_id_bytes())
        })
        .map(|extent| extent.length())
        .sum::<u64>();
    assert_eq!(
        stats.response_body_bytes as u64,
        prefix_bytes + envelope_bytes
    );
}

#[test]
fn cold_selected_member_claim_is_admitted_only_after_full_pack_proof() {
    let (pack, ids) = build_test_pack(2, 384, 2 * 1024 * 1024, false);
    let server = LoopbackServer::start(
        ServerMode::Store,
        4, // PUT, root GET, page GET, admitted envelope GET
        None,
    );
    let route = S3PackRoute::new(server.route_with_max(2 * 1024 * 1024));
    let upload = pack_upload_capability(&server, &pack, fence());
    route
        .put_pack(&pack, &upload, fence(), None)
        .expect("store immutable pack");

    let read = pack_read_capability(&server, &pack, fence(), true);
    let reopened = route
        .open_pack(&read, fence())
        .expect("cold open root directory");
    let admitted = route
        .get_object_claim(
            &reopened,
            UntrustedObjectId::from_bytes(*ids[0].as_bytes()),
            fence(),
            &RelationAdmissionRegistry::default(),
        )
        .expect("prove membership and admit complete typed envelope");
    assert_eq!(admitted.id(), ids[0]);
    assert_eq!(admitted.fence(), fence());
    let envelope = admitted.verified_envelope();
    assert_eq!(envelope.id(), ids[0]);
    assert_eq!(
        envelope.schema(),
        backend_version::SchemaIdentity::new(
            TestBytesSchema::DOMAIN,
            TestBytesSchema::TYPE,
            TestBytesSchema::VERSION,
        )
    );
    assert_eq!(envelope.payload_len(), 384);

    // The route can locate the candidate page from raw bytes, but a claim
    // absent from the authenticated page is rejected before any object GET.
    assert!(matches!(
        route.get_object_claim(
            &reopened,
            UntrustedObjectId::from_bytes([0xff; 32]),
            fence(),
            &RelationAdmissionRegistry::default(),
        ),
        Err(RemoteStoreError::Identity)
    ));
    let stats_handle = Arc::clone(&server.stats);
    server.finish();
    let stats = *stats_handle.lock().expect("test stats mutex");
    assert_eq!(stats.requests, 4);
    assert_eq!(stats.put_requests, 1);
    assert_eq!(stats.get_requests, 3);
}

#[test]
fn cold_pack_rejects_wrong_ranges_checksums_and_tampered_object_pages() {
    let (pack, ids) = build_test_pack(96, 512, 2 * 1024 * 1024, false);

    let bad_range =
        LoopbackServer::start(ServerMode::BadRange, 1, Some(stored_bytes_for_pack(&pack)));
    let route = S3PackRoute::new(bad_range.route());
    let read = pack_read_capability(&bad_range, &pack, fence(), false);
    assert!(matches!(
        route.open_pack(&read, fence()),
        Err(RemoteStoreError::Protocol)
    ));
    bad_range.finish();

    let bad_checksum = LoopbackServer::start(
        ServerMode::BadChecksum,
        1,
        Some(stored_bytes_for_pack(&pack)),
    );
    let route = S3PackRoute::new(bad_checksum.route());
    let read = pack_read_capability(&bad_checksum, &pack, fence(), false);
    assert!(matches!(
        route.open_pack(&read, fence()),
        Err(RemoteStoreError::Identity)
    ));
    bad_checksum.finish();

    let tampered = LoopbackServer::start(
        ServerMode::TamperRange,
        2,
        Some(stored_bytes_for_pack(&pack)),
    );
    let route = S3PackRoute::new(tampered.route());
    let read = pack_read_capability(&tampered, &pack, fence(), false);
    let reopened = route
        .open_pack(&read, fence())
        .expect("read valid manifest");
    assert!(matches!(
        route.get_object(
            &reopened,
            ids[23],
            fence(),
            &RelationAdmissionRegistry::default(),
        ),
        Err(RemoteStoreError::Identity)
    ));
    tampered.finish();
}

#[test]
fn pack_put_rejects_a_bad_server_checksum_and_keeps_etag_opaque() {
    let (pack, _) = build_test_pack(24, 256, 2 * 1024 * 1024, false);
    let bad_ack = LoopbackServer::start(ServerMode::BadPutChecksum, 1, None);
    let route = S3PackRoute::new(bad_ack.route());
    let upload = pack_upload_capability(&bad_ack, &pack, fence());
    assert!(matches!(
        route.put_pack(&pack, &upload, fence(), None),
        Err(RemoteStoreError::Identity)
    ));
    let saved = bad_ack.finish();
    assert!(saved.lock().expect("test object mutex").is_some());
    // The route does not interpret ETag as a content hash. Pack GET tests
    // above accept the intentionally opaque ETag only after SHA-256 and the
    // page proof establish the bytes.
}

#[test]
fn streamed_pack_uses_local_storage_neutral_layout_and_checks_claims() {
    let payload = b"streamed semantic payload".as_slice();
    let key = ObjectKey::<TestBytesSchema>::from_value(b"stable-key".as_slice());
    let object = TypedObject::from_value(&key, payload);
    let claim = backend_store::ArtifactObjectClaim::new(
        object.schema(),
        *object.key(),
        *object.version(),
        u64::try_from(payload.len()).expect("payload fits"),
    )
    .with_object_id(UntrustedObjectId::from_bytes(*object.id().as_bytes()));

    let mut builder = S3PackBuilder::new(2 * 1024 * 1024).expect("create streamed pack");
    let mut source = std::io::Cursor::new(payload);
    assert_eq!(
        builder
            .push_streamed(claim, &mut source)
            .expect("stream checked payload"),
        object.id()
    );
    let pack = builder.finish().expect("finish S3 pack");

    let (_temp, common_file) = super::TempEnvelope::create().expect("create common pack file");
    let mut common =
        backend_store::artifact_pack::ArtifactPackWriter::new(common_file, 1, 2 * 1024 * 1024)
            .expect("create local shared-layout writer");
    common
        .push_with(object.id(), |output| {
            backend_store::write_object_envelope(&object, output, 4096)
        })
        .expect("write same canonical envelope locally");
    let (common_file, common_manifest) = common.finish().expect("finish local pack");
    drop(common_file);
    assert_eq!(pack.pack_id(), common_manifest.pack_id());
    assert_eq!(pack.layout_id(), common_manifest.layout_id());
    assert_eq!(pack.manifest_bytes(), common_manifest.manifest_bytes());

    let wrong_claim = claim;
    let wrong_payload = b"payload with another identity".as_slice();
    let mut wrong_builder = S3PackBuilder::new(2 * 1024 * 1024).expect("create retry builder");
    let mut wrong_source = std::io::Cursor::new(wrong_payload);
    assert!(matches!(
        wrong_builder.push_streamed(wrong_claim, &mut wrong_source),
        Err(RemoteStoreError::Identity)
    ));

    let empty_payload = b"".as_slice();
    let empty_key = ObjectKey::<TestBytesSchema>::from_value(empty_payload);
    let empty_object = TypedObject::from_value(&empty_key, empty_payload);
    let empty_claim = backend_store::ArtifactObjectClaim::new(
        empty_object.schema(),
        *empty_object.key(),
        *empty_object.version(),
        0,
    )
    .with_object_id(UntrustedObjectId::from_bytes(*empty_object.id().as_bytes()));
    let mut empty_builder = S3PackBuilder::new(2 * 1024 * 1024).expect("create empty pack");
    let mut empty_source = std::io::Cursor::new(empty_payload);
    assert_eq!(
        empty_builder
            .push_streamed(empty_claim, &mut empty_source)
            .expect("admit zero-byte payload"),
        empty_object.id()
    );
    assert_eq!(
        empty_builder
            .finish()
            .expect("finish zero-byte object pack")
            .object_count(),
        1
    );
}

fn stored_bytes_for_pack(pack: &super::ImmutableS3Pack) -> Vec<u8> {
    let mut file = pack.open_pack().expect("open pack spool");
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).expect("read pack spool");
    bytes
}
