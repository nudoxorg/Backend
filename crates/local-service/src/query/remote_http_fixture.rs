//! Owned, bounded loopback HTTP fixtures for the remote projection tests.
//! Expected transcripts are assertions, never a listener's lifetime contract.

#![allow(
    clippy::expect_used,
    reason = "Only loopback fixture setup and faults; failures retain the actual transcript"
)]

use std::fmt::Write as _;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const IO_TIMEOUT: Duration = Duration::from_secs(1);
const SERVER_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_BODY_BYTES: usize = 64 * 1024;
const MAX_REQUESTS: usize = 64;

struct RequestTrace {
    line: String,
    body: String,
    response: Option<(String, String)>,
}

struct Control {
    stop: AtomicBool,
    trace: Mutex<Vec<RequestTrace>>,
}

/// One test's listener and request transcript; no connection survives its owner.
pub(super) struct Connections {
    listener: TcpListener,
    control: Arc<Control>,
    deadline: Instant,
}

impl Connections {
    pub(super) fn accept(&self) -> Option<TcpStream> {
        while !self.control.stop.load(Ordering::Acquire) {
            assert!(
                Instant::now() < self.deadline,
                "HTTP fixture lifetime expired"
            );
            match self.listener.accept() {
                Ok((stream, _)) => {
                    stream
                        .set_read_timeout(Some(IO_TIMEOUT))
                        .expect("fixture read deadline");
                    stream
                        .set_write_timeout(Some(IO_TIMEOUT))
                        .expect("fixture write deadline");
                    return Some(stream);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!("fixture accept failed: {error}"),
            }
        }
        None
    }

    pub(super) fn read_request(&self, stream: &mut TcpStream) -> Vec<u8> {
        let deadline = Instant::now() + IO_TIMEOUT;
        let mut request = Vec::new();
        let mut byte = [0_u8; 1];
        while !request.ends_with(b"\r\n\r\n") {
            assert!(
                request.len() < MAX_HEADER_BYTES,
                "fixture HTTP header exceeded bound"
            );
            assert!(
                Instant::now() < deadline,
                "fixture HTTP header timed out: {request:?}"
            );
            stream.read_exact(&mut byte).unwrap_or_else(|error| {
                panic!("fixture HTTP header failed: {error}; retained bytes: {request:?}")
            });
            request.push(byte[0]);
        }
        let header = std::str::from_utf8(&request).expect("fixture HTTP header UTF-8");
        let line = header
            .lines()
            .next()
            .expect("fixture request line")
            .to_owned();
        let length = header
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then_some(value.trim())
            })
            .map(|length| length.parse::<usize>().expect("fixture Content-Length"))
            .unwrap_or(0);
        assert!(length <= MAX_BODY_BYTES, "fixture HTTP body exceeded bound");
        let body_start = request.len();
        request.resize(body_start + length, 0);
        let mut read = body_start;
        while read < request.len() {
            assert!(
                Instant::now() < deadline,
                "fixture HTTP body timed out for {line}"
            );
            match stream.read(&mut request[read..]) {
                Ok(0) => panic!(
                    "fixture HTTP body closed for {line} after {} of {length} bytes",
                    read - body_start
                ),
                Ok(count) => read += count,
                Err(error) => panic!("fixture HTTP body failed for {line}: {error}"),
            }
        }
        let body = String::from_utf8(request[body_start..].to_vec()).expect("fixture JSON UTF-8");
        let mut trace = self.control.trace.lock().expect("fixture transcript lock");
        assert!(
            trace.len() < MAX_REQUESTS,
            "fixture request count exceeded bound"
        );
        trace.push(RequestTrace {
            line,
            body,
            response: None,
        });
        request
    }

    pub(super) fn respond(&self, stream: &mut TcpStream, status: &str, body: &str) {
        self.control
            .trace
            .lock()
            .expect("fixture transcript lock")
            .last_mut()
            .expect("response follows a captured request")
            .response = Some((status.to_owned(), body.to_owned()));
        write!(stream,
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
            .expect("fixture response");
    }
}

/// Explicitly stops accepting after the tested operations, then observes the
/// owned worker's completion and panic. Dropping on assertion failure also
/// closes the listener without leaving a detached accept loop.
pub(super) struct FixtureServer<T> {
    worker: Option<JoinHandle<T>>,
    control: Arc<Control>,
    expected_requests: usize,
}

impl<T> FixtureServer<T> {
    pub(super) fn join(mut self) -> Result<T, String> {
        self.finish()
    }

    fn finish(&mut self) -> Result<T, String> {
        self.control.stop.store(true, Ordering::Release);
        let deadline = Instant::now() + SERVER_TIMEOUT;
        while !self
            .worker
            .as_ref()
            .expect("owned fixture worker")
            .is_finished()
        {
            if Instant::now() >= deadline {
                return Err(self.failure("owned worker failed to stop within its deadline"));
            }
            thread::sleep(Duration::from_millis(2));
        }
        let result = self.worker.take().expect("owned fixture worker").join();
        match result {
            Err(panic) => {
                let reason = panic
                    .downcast_ref::<String>()
                    .map(String::as_str)
                    .or_else(|| panic.downcast_ref::<&str>().copied())
                    .unwrap_or("non-string panic payload");
                Err(self.failure(&format!("fixture worker panicked: {reason}")))
            }
            Ok(value) => {
                let observed = self
                    .control
                    .trace
                    .lock()
                    .expect("fixture transcript lock")
                    .len();
                if observed != self.expected_requests {
                    Err(self.failure(&format!(
                        "expected {} requests, observed {observed}",
                        self.expected_requests
                    )))
                } else {
                    Ok(value)
                }
            }
        }
    }

    fn failure(&self, reason: &str) -> String {
        // A malformed fixture request must not lose the requests completed
        // before its panic. The trace lock is never held across HTTP parsing.
        let trace = self
            .control
            .trace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut captured = String::new();
        for (index, request) in trace.iter().enumerate() {
            let _ = writeln!(
                captured,
                "[{index}] {}\nrequest body: {}\nresponse: {:?}",
                request.line, request.body, request.response
            );
        }
        format!("{reason}; actual request/response transcript:\n{captured}")
    }
}

impl<T> Drop for FixtureServer<T> {
    fn drop(&mut self) {
        if self.worker.is_some() {
            if let Err(error) = self.finish() {
                if thread::panicking() {
                    eprintln!("fixture cleanup during test failure: {error}");
                } else {
                    panic!("fixture cleanup failed: {error}");
                }
            }
        }
    }
}

pub(super) fn spawn<T, F>(expected_requests: usize, handler: F) -> (String, FixtureServer<T>)
where
    T: Send + 'static,
    F: FnOnce(Connections) -> T + Send + 'static,
{
    assert!(expected_requests <= MAX_REQUESTS);
    let listener = TcpListener::bind("127.0.0.1:0").expect("fixture listener");
    listener
        .set_nonblocking(true)
        .expect("fixture accept deadline");
    let address = listener.local_addr().expect("fixture address");
    let control = Arc::new(Control {
        stop: AtomicBool::new(false),
        trace: Mutex::new(Vec::new()),
    });
    let connections = Connections {
        listener,
        control: control.clone(),
        deadline: Instant::now() + SERVER_TIMEOUT,
    };
    let worker = thread::spawn(move || handler(connections));
    (
        format!("http://{address}"),
        FixtureServer {
            worker: Some(worker),
            control,
            expected_requests,
        },
    )
}
