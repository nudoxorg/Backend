//! Exercises the real HTTP service against a separately running locald.
#![cfg(unix)]
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn request(addr: &str, request: &str) -> String {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(4)))
        .unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    let mut result = String::new();
    stream.read_to_string(&mut result).unwrap();
    result
}
fn get(addr: &str, path: &str) -> String {
    request(
        addr,
        &format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n"),
    )
}
fn wait_for(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !predicate() {
        assert!(Instant::now() < deadline, "timed out waiting for process");
        thread::sleep(Duration::from_millis(50));
    }
}
fn value(body: &str, prefix: &str) -> f64 {
    body.lines()
        .find_map(|line| line.strip_prefix(prefix))
        .unwrap_or_else(|| panic!("missing {prefix} in {body}"))
        .trim()
        .parse()
        .unwrap()
}
#[test]
fn real_owner_http_metrics_and_shutdown() {
    let dir = tempfile::Builder::new()
        .prefix("bm-")
        .tempdir_in("/tmp")
        .unwrap();
    let socket = dir.path().join("owner.sock");
    let locald = std::env::var_os("BACKEND_LOCALD_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let exe = std::env::current_exe().unwrap();
            exe.parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("backend-locald")
        });
    assert!(
        locald.is_file(),
        "build backend-locald first: {}",
        locald.display()
    );
    let reserve = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = reserve.local_addr().unwrap().to_string();
    drop(reserve);
    let mut metrics = Process(
        Command::new(env!("CARGO_BIN_EXE_backend-metrics"))
            .args(["--endpoint", socket.to_str().unwrap(), "--bind", &addr])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    wait_for(|| TcpStream::connect(&addr).is_ok());
    assert!(get(&addr, "/health").starts_with("HTTP/1.1 503"));
    let mut owner = Process(
        Command::new(locald)
            .args([
                "--endpoint",
                socket.to_str().unwrap(),
                "--workspace",
                dir.path().join("workspace").to_str().unwrap(),
                "--idle-timeout-ms",
                "0",
                "--registry-offline",
                "--advisory-offline",
                "--forge-offline",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    wait_for(|| get(&addr, "/health").starts_with("HTTP/1.1 200"));
    let before = get(&addr, "/metrics");
    assert!(before.contains("# TYPE http_request_duration_seconds histogram\n"));
    assert!(value(&before, "http_requests_total{status=\"503\"} ") >= 1.0);
    assert!(before.contains("http_requests_total{status=\"404\"} 0\n"));
    assert!(get(&addr, "/secret?token=abc").starts_with("HTTP/1.1 404"));
    assert!(
        request(&addr, "POST /metrics HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .starts_with("HTTP/1.1 405")
    );
    assert!(
        request(
            &addr,
            "GET /metrics HTTP/1.1\r\nHost: localhost\r\nContent-Length: 4\r\n\r\n"
        )
        .starts_with("HTTP/1.1 400")
    );
    assert!(request(&addr, "GET / HTTP/1.1\r\nBad header\r\n\r\n").starts_with("HTTP/1.1 400"));
    assert!(
        request(&addr, &format!("GET / HTTP/1.1\r\nX: {}", "a".repeat(8173)))
            .starts_with("HTTP/1.1 431")
    );
    let after = get(&addr, "/metrics");
    assert!(!after.contains("token=abc"));
    for status in ["404", "405", "400"] {
        assert_eq!(
            value(
                &after,
                &format!("http_requests_total{{status=\"{status}\"}} ")
            ),
            1.0
        );
    }
    assert_eq!(
        value(&after, "http_requests_total{status=\"200\"} ")
            - value(&before, "http_requests_total{status=\"200\"} "),
        1.0,
    );
    let count = value(&after, "http_request_duration_seconds_count ");
    assert_eq!(
        count - value(&before, "http_request_duration_seconds_count "),
        4.0
    );
    assert_eq!(
        value(&after, "http_request_duration_seconds_bucket{le=\"+Inf\"} "),
        count
    );
    let mut previous = 0.0;
    for bound in [
        "0.005", "0.01", "0.025", "0.05", "0.1", "0.25", "0.5", "1", "1.5", "2", "5",
    ] {
        let v = value(
            &after,
            &format!("http_request_duration_seconds_bucket{{le=\"{bound}\"}} "),
        );
        assert!(v >= previous && v <= count);
        previous = v;
    }
    assert!(value(&after, "http_request_duration_seconds_sum ") > 0.0);
    let mut slow = TcpStream::connect(&addr).unwrap();
    slow.set_read_timeout(Some(Duration::from_secs(4))).unwrap();
    slow.write_all(b"GET /metrics HTTP/1.1\r\n").unwrap();
    let waiting = Instant::now();
    let mut closed = [0; 1];
    let n = slow.read(&mut closed).unwrap();
    assert!(waiting.elapsed() >= Duration::from_secs(1));
    if n != 0 {
        let mut reply = vec![closed[0]];
        slow.read_to_end(&mut reply).unwrap();
        assert!(
            reply.starts_with(b"HTTP/1.1 408"),
            "unexpected response to stalled headers: {reply:?}"
        );
    }
    owner.0.kill().unwrap();
    owner.0.wait().unwrap();
    wait_for(|| get(&addr, "/health").starts_with("HTTP/1.1 503"));
    assert!(
        !Command::new(env!("CARGO_BIN_EXE_backend-metrics"))
            .args([
                "--endpoint",
                socket.to_str().unwrap(),
                "--bind",
                "0.0.0.0:0"
            ])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("kill")
            .args(["-TERM", &metrics.0.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    wait_for(|| metrics.0.try_wait().unwrap().is_some());
    assert!(metrics.0.wait().unwrap().success());
}
