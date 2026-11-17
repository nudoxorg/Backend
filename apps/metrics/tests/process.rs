//! Exercises the real HTTP service against a separately running locald.
#![cfg(unix)]
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    os::unix::fs::PermissionsExt,
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
    let deadline = Instant::now() + Duration::from_secs(90);
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
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
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
    let unavailable = get(&addr, "/metrics");
    assert!(unavailable.contains("backend_dependency_snapshot_available 0\n"));
    assert!(!unavailable.contains("store_requests_total{"));
    let owner_started = Instant::now();
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
    wait_for(|| {
        assert!(
            owner.0.try_wait().unwrap().is_none(),
            "locald exited before serving health"
        );
        get(&addr, "/health").starts_with("HTTP/1.1 200")
    });
    eprintln!(
        "owner health RPC admitted after {:?}",
        owner_started.elapsed()
    );
    let before = get(&addr, "/metrics");
    assert!(before.contains("# TYPE http_request_duration_seconds histogram\n"));
    assert!(before.contains("backend_dependency_snapshot_available 1\n"));
    assert!(
        value(
            &before,
            "store_requests_total{backend=\"view_journal\",op=\"recover\",outcome=\"success\"} "
        ) >= 1.0
    );
    for op in ["recover", "append", "replace"] {
        for outcome in ["success", "error"] {
            let labels = format!("backend=\"view_journal\",op=\"{op}\",outcome=\"{outcome}\"");
            let count = value(
                &before,
                &format!("store_request_duration_seconds_count{{{labels}}} "),
            );
            assert_eq!(
                value(&before, &format!("store_requests_total{{{labels}}} ")),
                count
            );
            assert_eq!(
                value(
                    &before,
                    &format!("store_request_duration_seconds_bucket{{{labels},le=\"+Inf\"}} ")
                ),
                count
            );
            let mut prior = 0.0;
            for bound in ["0.001", "0.005", "0.01", "0.05", "0.1", "0.5", "1"] {
                let bucket = value(
                    &before,
                    &format!("store_request_duration_seconds_bucket{{{labels},le=\"{bound}\"}} "),
                );
                assert!(bucket >= prior && bucket <= count);
                prior = bucket;
            }
        }
    }
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
    let unavailable = get(&addr, "/metrics");
    assert!(unavailable.contains("backend_dependency_snapshot_available 0\n"));
    assert!(!unavailable.contains("store_requests_total{"));
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

#[test]
fn dependency_snapshot_trickle_is_bounded_and_omitted() {
    use std::os::unix::net::UnixListener;
    let dir = tempfile::Builder::new()
        .prefix("bm-")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket = dir.path().join("owner.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
    let peer = thread::spawn(move || {
        let (mut connection, _) = listener.accept().unwrap();
        let mut request = [0; 9];
        connection.read_exact(&mut request).unwrap();
        assert_eq!(&request, b"\0\0\0\x05LDM1\x01");
        // A malicious peer drips a declared response without ever completing it.
        for byte in [0, 0, 0, 8, b'n', b'u', b'l', b'l'] {
            if connection.write_all(&[byte]).is_err() {
                break;
            }
            thread::sleep(Duration::from_millis(250));
        }
    });
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
    let start = Instant::now();
    let body = get(&addr, "/metrics");
    assert!(start.elapsed() < Duration::from_secs(2));
    assert!(body.starts_with("HTTP/1.1 200"));
    assert!(body.contains("backend_dependency_snapshot_available 0\n"));
    assert!(!body.contains("store_requests_total{"));
    assert!(
        Command::new("kill")
            .args(["-TERM", &metrics.0.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    wait_for(|| metrics.0.try_wait().unwrap().is_some());
    assert!(metrics.0.wait().unwrap().success());
    peer.join().unwrap();
}

#[test]
fn real_view_journal_append_success_and_path_failure() {
    let dir = tempfile::Builder::new()
        .prefix("bm-")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket = dir.path().join("owner.sock");
    let workspace = dir.path().join("workspace");
    let project = dir.path().join("project");
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(
        project.join("Cargo.toml"),
        "[package]\nname='metrics_fixture'\nversion='0.1.0'\nedition='2021'\n",
    )
    .unwrap();
    std::fs::write(project.join("src/lib.rs"), "pub fn value() -> u32 { 1 }\n").unwrap();
    let bin_dir = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let locald = std::env::var_os("BACKEND_LOCALD_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| bin_dir.join("backend-locald"));
    let cli = std::env::var_os("BACKEND_CLI_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| bin_dir.join("backend-cli"));
    assert!(
        locald.is_file() && cli.is_file(),
        "build backend-locald and backend-cli first"
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
    let mut owner = Process(
        Command::new(locald)
            .args([
                "--endpoint",
                socket.to_str().unwrap(),
                "--workspace",
                workspace.to_str().unwrap(),
                "--idle-timeout-ms",
                "0",
                "--registry-offline",
                "--advisory-offline",
                "--forge-offline",
                "--registry-discovery-offline",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    wait_for(|| {
        assert!(
            owner.0.try_wait().unwrap().is_none(),
            "locald exited during startup"
        );
        get(&addr, "/health").starts_with("HTTP/1.1 200")
    });
    let counter = |outcome: &str| {
        format!(
            "store_requests_total{{backend=\"view_journal\",op=\"append\",outcome=\"{outcome}\"}} "
        )
    };
    let initial = value(&get(&addr, "/metrics"), &counter("success"));
    let start_add = || {
        Command::new(&cli)
            .args([
                "--endpoint",
                socket.to_str().unwrap(),
                "--workspace",
                workspace.to_str().unwrap(),
                "--project",
                project.to_str().unwrap(),
                "add",
                ".",
                "--wait",
                "--wait-timeout",
                "90",
            ])
            .current_dir(&project)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    };
    let mut first = Process(start_add());
    wait_for(|| first.0.try_wait().unwrap().is_some());
    let after = get(&addr, "/metrics");
    assert!(
        value(&after, &counter("success")) > initial,
        "real request did not append to journal"
    );
    let error_before = value(&after, &counter("error"));
    let journal = workspace.join("view.journal");
    let saved_journal = workspace.join("view.journal.saved");
    std::fs::rename(&journal, &saved_journal).unwrap();
    let obstruction = std::os::unix::net::UnixListener::bind(&journal).unwrap();
    assert_eq!(std::fs::metadata(&journal).unwrap().len(), 0);
    std::fs::write(project.join("src/lib.rs"), "pub fn value() -> u32 { 2 }\n").unwrap();
    let mut second = Process(start_add());
    let mut after_failure = String::new();
    wait_for(|| {
        let scrape = get(&addr, "/metrics");
        if !scrape.contains("backend_dependency_snapshot_available 1\n") {
            return false;
        }
        if value(&scrape, &counter("error")) <= error_before {
            return false;
        }
        after_failure = scrape;
        true
    });
    let errors = value(&after_failure, &counter("error"));
    let histogram = "store_request_duration_seconds_bucket{backend=\"view_journal\",op=\"append\",outcome=\"error\",le=\"+Inf\"} ";
    assert_eq!(value(&after_failure, histogram), errors);
    assert_eq!(
        value(&after_failure, histogram) - value(&after, histogram),
        errors - error_before
    );
    drop(obstruction);
    std::fs::remove_file(&journal).unwrap();
    std::fs::rename(&saved_journal, &journal).unwrap();
    let _ = second.0.kill();
    let _ = second.0.wait();
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

#[test]
fn real_sparse_boundary_compaction_survives_cold_restart() {
    let dir = tempfile::Builder::new()
        .prefix("bm-")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket = dir.path().join("owner.sock");
    let workspace = dir.path().join("workspace");
    let project = dir.path().join("project");
    std::fs::create_dir(&project).unwrap();
    std::fs::write(
        project.join("package.json"),
        "{\"name\":\"metrics-compaction\",\"version\":\"1.0.0\"}\n",
    )
    .unwrap();
    std::fs::write(
        project.join("index.js"),
        "export function value() { return 1; }\n",
    )
    .unwrap();
    let bin_dir = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let locald = std::env::var_os("BACKEND_LOCALD_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| bin_dir.join("backend-locald"));
    let cli = std::env::var_os("BACKEND_CLI_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| bin_dir.join("backend-cli"));
    assert!(
        locald.is_file() && cli.is_file(),
        "build backend-locald and backend-cli first"
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
    let start_owner = || {
        Process(
            Command::new(&locald)
                .args([
                    "--endpoint",
                    socket.to_str().unwrap(),
                    "--workspace",
                    workspace.to_str().unwrap(),
                    "--idle-timeout-ms",
                    "0",
                    "--registry-offline",
                    "--advisory-offline",
                    "--forge-offline",
                    "--registry-discovery-offline",
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        )
    };
    let wait_owner = |owner: &mut Process| {
        wait_for(|| {
            assert!(
                owner.0.try_wait().unwrap().is_none(),
                "locald exited during startup"
            );
            get(&addr, "/health").starts_with("HTTP/1.1 200")
        })
    };
    let command = |args: &[&str]| {
        let mut child = Process(
            Command::new(&cli)
                .args([
                    "--endpoint",
                    socket.to_str().unwrap(),
                    "--workspace",
                    workspace.to_str().unwrap(),
                    "--project",
                    project.to_str().unwrap(),
                ])
                .args(args)
                .current_dir(&project)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        wait_for(|| child.0.try_wait().unwrap().is_some());
        let status = child.0.wait().unwrap();
        let mut stdout = Vec::new();
        child
            .0
            .stdout
            .take()
            .unwrap()
            .read_to_end(&mut stdout)
            .unwrap();
        let mut stderr = String::new();
        child
            .0
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();
        assert!(status.success(), "CLI failed: {stderr}");
        stdout
    };
    let mut owner = start_owner();
    wait_owner(&mut owner);
    command(&["add", ".", "--wait", "--wait-timeout", "90"]);
    let before = get(&addr, "/metrics");
    let counter =
        "store_requests_total{backend=\"view_journal\",op=\"replace\",outcome=\"success\"} ";
    let bucket = "store_request_duration_seconds_bucket{backend=\"view_journal\",op=\"replace\",outcome=\"success\",le=\"+Inf\"} ";
    let journal = workspace.join("view.journal");
    let old_length = std::fs::metadata(&journal).unwrap().len();
    assert!(old_length < 128 * 1024 * 1024);
    // Deliberate sparse boundary fixture: this is not a claim about natural workload size.
    std::fs::OpenOptions::new()
        .write(true)
        .open(&journal)
        .unwrap()
        .set_len(128 * 1024 * 1024)
        .unwrap();
    std::fs::write(
        project.join("index.js"),
        "export function value() { return 2; }\n",
    )
    .unwrap();
    command(&["add", ".", "--wait", "--wait-timeout", "90"]);
    let after = get(&addr, "/metrics");
    let delta = value(&after, counter) - value(&before, counter);
    assert!(delta >= 1.0);
    assert_eq!(value(&after, bucket) - value(&before, bucket), delta);
    assert!(std::fs::metadata(&journal).unwrap().len() < 128 * 1024 * 1024);
    let selected = command(&["packages"]);
    assert!(String::from_utf8_lossy(&selected).contains("metrics-compaction"));
    assert!(
        Command::new("kill")
            .args(["-TERM", &owner.0.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    wait_for(|| owner.0.try_wait().unwrap().is_some());
    assert!(owner.0.wait().unwrap().success());
    let mut restarted = start_owner();
    wait_owner(&mut restarted);
    assert_eq!(
        command(&["packages"]),
        selected,
        "cold restart changed selected package state"
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
