use super::*;

#[derive(Default)]
pub(super) struct LoopbackFixtureCounters {
    pub(super) requests: AtomicU64,
    feed_requests: AtomicU64,
    archive_requests: AtomicU64,
    response_bytes: AtomicU64,
    feed_response_bytes: AtomicU64,
    archive_response_bytes: AtomicU64,
}

pub(super) struct LoopbackFixtureReport {
    pub(super) requests: u64,
    pub(super) feed_requests: u64,
    pub(super) archive_requests: u64,
    pub(super) response_bytes: u64,
    pub(super) feed_response_bytes: u64,
    pub(super) archive_response_bytes: u64,
}

fn read_http_request(stream: &mut TcpStream) -> Result<String, String> {
    let mut bytes = Vec::with_capacity(1024);
    let mut chunk = [0_u8; 1024];
    while bytes.len() < 8 * 1024 {
        let read = stream
            .read(&mut chunk)
            .map_err(|error| format!("loopback fixture request read failed: {error}"))?;
        if read == 0 {
            return Err("loopback fixture client closed before request headers".to_owned());
        }
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            let line = bytes
                .split(|byte| *byte == b'\n')
                .next()
                .ok_or_else(|| "loopback fixture request line was empty".to_owned())?;
            return String::from_utf8(line.trim_ascii().to_vec())
                .map_err(|_| "loopback fixture request line was not UTF-8".to_owned());
        }
    }
    Err("loopback fixture request headers exceeded 8 KiB".to_owned())
}

fn serve_loopback_registry(
    listener: TcpListener,
    feed: Vec<u8>,
    archive: Vec<u8>,
    counters: Arc<LoopbackFixtureCounters>,
) -> Result<LoopbackFixtureReport, String> {
    listener
        .set_nonblocking(true)
        .map_err(|error| format!("loopback fixture nonblocking setup failed: {error}"))?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while counters.requests.load(Ordering::Acquire) < 2 && Instant::now() < deadline {
        match listener.accept() {
            Ok((mut stream, _)) => {
                stream
                    .set_nonblocking(false)
                    .map_err(|error| format!("loopback fixture blocking setup failed: {error}"))?;
                stream
                    .set_read_timeout(Some(Duration::from_secs(1)))
                    .map_err(|error| format!("loopback fixture read timeout failed: {error}"))?;
                stream
                    .set_write_timeout(Some(Duration::from_secs(1)))
                    .map_err(|error| format!("loopback fixture write timeout failed: {error}"))?;
                let request = read_http_request(&mut stream)?;
                let path = request
                    .split_ascii_whitespace()
                    .nth(1)
                    .ok_or_else(|| "loopback fixture request path was missing".to_owned())?;
                let (kind, body) = if path.starts_with("/feed?") {
                    ("feed", feed.as_slice())
                } else if path == "/archive" {
                    ("archive", archive.as_slice())
                } else {
                    return Err(format!(
                        "loopback fixture received unexpected path {path:?}"
                    ));
                };
                counters.requests.fetch_add(1, Ordering::AcqRel);
                match kind {
                    "feed" => {
                        counters.feed_requests.fetch_add(1, Ordering::AcqRel);
                        counters
                            .feed_response_bytes
                            .fetch_add(body.len() as u64, Ordering::AcqRel);
                    }
                    "archive" => {
                        counters.archive_requests.fetch_add(1, Ordering::AcqRel);
                        counters
                            .archive_response_bytes
                            .fetch_add(body.len() as u64, Ordering::AcqRel);
                    }
                    _ => unreachable!("fixture response kind is closed above"),
                }
                counters
                    .response_bytes
                    .fetch_add(body.len() as u64, Ordering::AcqRel);
                let headers = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream
                    .write_all(headers.as_bytes())
                    .and_then(|_| stream.write_all(body))
                    .map_err(|error| format!("loopback fixture response write failed: {error}"))?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(1));
            }
            Err(error) => return Err(format!("loopback fixture accept failed: {error}")),
        }
    }
    let report = LoopbackFixtureReport {
        requests: counters.requests.load(Ordering::Acquire),
        feed_requests: counters.feed_requests.load(Ordering::Acquire),
        archive_requests: counters.archive_requests.load(Ordering::Acquire),
        response_bytes: counters.response_bytes.load(Ordering::Acquire),
        feed_response_bytes: counters.feed_response_bytes.load(Ordering::Acquire),
        archive_response_bytes: counters.archive_response_bytes.load(Ordering::Acquire),
    };
    if report.requests != 2 || report.feed_requests != 1 || report.archive_requests != 1 {
        return Err(format!(
            "loopback fixture deadline expired after {} requests (feed={}, archive={})",
            report.requests, report.feed_requests, report.archive_requests
        ));
    }
    Ok(report)
}

pub(super) fn start_loopback_registry(
    feed: Vec<u8>,
    archive: Vec<u8>,
) -> BenchResult<(
    String,
    Arc<LoopbackFixtureCounters>,
    thread::JoinHandle<Result<LoopbackFixtureReport, String>>,
)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let counters = Arc::new(LoopbackFixtureCounters::default());
    let thread_counters = Arc::clone(&counters);
    let handle =
        thread::spawn(move || serve_loopback_registry(listener, feed, archive, thread_counters));
    Ok((format!("http://{address}"), counters, handle))
}
