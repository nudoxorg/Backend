//! Public process journey for loopback-only, source-discovered registry metadata.
//! It does not fetch an archive or ask any language frontend to parse a package.

use super::*;
use backend_engine::registry::{
    DiscoveryAdvisory, DiscoveryBatch, DiscoveryCompleteness, DiscoveryCursor, DiscoveryFacet,
    DiscoveryFact, DiscoveryMetadata, DiscoveryObservedAt, DiscoverySourceEvent, DiscoveryStanding,
    RegistryEcosystem, RegistryEndpoint, discovery_source_identity,
};
use serde_json::Value;
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, AtomicU8};
use std::sync::{Arc, Mutex};

const PYPI_OLD: &str = r#"{"info":{"name":"fixture","version":"1.0.0","summary":"renderer","description":"README witness","requires_python":">=3.10","requires_dist":["dep>=1"]},"vulnerabilities":[{"id":"GHSA-pypi-fixture","aliases":["CVE-2026-12345"]}],"releases":{"1.0.0":[{"yanked":false}]}}"#;
const PYPI_NEW: &str = r#"{"info":{"name":"fixture","version":"1.0.0","summary":"renderer GHSA-pypi-fixture","description":"README witness","requires_python":">=3.10","requires_dist":["dep>=1"]},"releases":{"1.0.0":[{"yanked":false}]}}"#;
const PYPI_EMPTY: &str = r#"{"info":{"name":"empty-fixture","version":"1.0.0","summary":"scoped empty source list"},"vulnerabilities":[],"releases":{"1.0.0":[{"yanked":false}]}}"#;
const NPM: &str = r#"{"name":"fixture","_rev":"1-a","description":"npm fixture","readme":"README witness","versions":{"1.0.0":{"license":"MIT","engines":{"node":">=18"},"dependencies":{"dep":"^1"}}}}"#;
const OSV_NPM: &str = r#"{"schema_version":"1.3.1","id":"OSV-LOOPBACK-1","modified":"2026-01-02T00:00:00Z","affected":[{"package":{"ecosystem":"npm","name":"fixture"},"ranges":[{"type":"SEMVER","events":[{"introduced":"0"},{"fixed":"2.0.0"}]}]}]}"#;

struct NativeRegistry {
    base: String,
    changed: Arc<AtomicBool>,
    osv_mode: Arc<AtomicU8>,
    stopped: Arc<AtomicBool>,
    requests: Arc<Mutex<Vec<String>>>,
    worker: Option<thread::JoinHandle<()>>,
}

impl NativeRegistry {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("loopback native registry");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let base = format!("http://{}", listener.local_addr().expect("address"));
        let changed = Arc::new(AtomicBool::new(false));
        let osv_mode = Arc::new(AtomicU8::new(0));
        let stopped = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (change, stop, log) = (
            Arc::clone(&changed),
            Arc::clone(&stopped),
            Arc::clone(&requests),
        );
        let advisory_mode = Arc::clone(&osv_mode);
        let worker = thread::spawn(move || {
            while !stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
                        let _ = stream.set_write_timeout(Some(Duration::from_secs(3)));
                        let mut input = Vec::new();
                        let mut buffer = [0_u8; 1024];
                        while !input.windows(4).any(|window| window == b"\r\n\r\n") {
                            match stream.read(&mut buffer) {
                                Ok(0) | Err(_) => break,
                                Ok(count) => input.extend_from_slice(&buffer[..count]),
                            }
                            if input.len() > 16 * 1024 {
                                break;
                            }
                        }
                        let request = String::from_utf8_lossy(&input);
                        let path = request
                            .lines()
                            .next()
                            .and_then(|line| line.split_whitespace().nth(1))
                            .unwrap_or("");
                        log.lock().expect("request log").push(path.to_owned());
                        if path == "/osv.zip" {
                            let mode = advisory_mode.load(Ordering::Acquire);
                            if mode == 2 {
                                let _ = stream.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                                continue;
                            }
                            let body: &[u8] = if mode == 1 || mode == 4 {
                                include_bytes!("fixtures/osv-npm-other.zip")
                            } else {
                                include_bytes!("fixtures/osv-npm-positive.zip")
                            };
                            let freshness = if mode == 3 || mode == 4 {
                                "max-age=0"
                            } else {
                                "max-age=3600"
                            };
                            let header = format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: application/zip\r\nCache-Control: {freshness}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                body.len()
                            );
                            let _ = stream.write_all(header.as_bytes());
                            let _ = stream.write_all(body);
                            continue;
                        }
                        if path == "/osv" {
                            let mode = advisory_mode.load(Ordering::Acquire);
                            if mode == 2 {
                                let _ = stream.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                                continue;
                            }
                            let body = if mode == 1 || mode == 4 {
                                r#"{"vulns":[]}"#
                            } else {
                                OSV_NPM
                            };
                            let freshness = if mode == 3 || mode == 4 {
                                "max-age=0"
                            } else {
                                "max-age=3600"
                            };
                            let header = format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nCache-Control: {freshness}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                body.len()
                            );
                            let _ = stream.write_all(header.as_bytes());
                            let _ = stream.write_all(body.as_bytes());
                            continue;
                        }
                        let body = if path == "/npm" || path == "/npm/" {
                            r#"{"update_seq":1}"#.to_owned()
                        } else if path.starts_with("/npm/_changes?") {
                            if path.contains("since=0") {
                                format!(
                                    r#"{{"results":[{{"seq":1,"id":"fixture","changes":[{{"rev":"1-a"}}],"doc":{NPM}}}],"last_seq":1,"pending":0}}"#
                                )
                            } else {
                                r#"{"results":[],"last_seq":1,"pending":0}"#.to_owned()
                            }
                        } else if path == "/npm/fixture" {
                            NPM.to_owned()
                        } else if path == "/pypi/simple/" {
                            r#"{"meta":{},"projects":[{"name":"fixture"},{"name":"empty-fixture"}]}"#.to_owned()
                        } else if path == "/pypi/pypi/fixture/json" {
                            if change.load(Ordering::Acquire) {
                                PYPI_NEW
                            } else {
                                PYPI_OLD
                            }
                            .to_owned()
                        } else if path == "/pypi/pypi/empty-fixture/json" {
                            PYPI_EMPTY.to_owned()
                        } else if path.starts_with("/npm/feed?") || path.starts_with("/pypi/feed?")
                        {
                            format!(r#"{{"schema":1,"next":"{}","items":[]}}"#, "00".repeat(32))
                        } else {
                            let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                            continue;
                        };
                        let header = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(header.as_bytes());
                        let _ = stream.write_all(body.as_bytes());
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(error) => panic!("native registry accept: {error}"),
                }
            }
        });
        Self {
            base,
            changed,
            osv_mode,
            stopped,
            requests,
            worker: Some(worker),
        }
    }

    fn source(&self, ecosystem: &str) -> String {
        format!("{ecosystem}={}/{ecosystem}", self.base)
    }
    fn advance(&self) {
        self.changed.store(true, Ordering::Release);
    }
    fn requests(&self) -> Vec<String> {
        self.requests.lock().expect("request log").clone()
    }
}

impl Drop for NativeRegistry {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.join().expect("registry worker");
        }
    }
}

fn start_owner(
    workspace: &Path,
    authority: &Path,
    registry: &NativeRegistry,
    offline: bool,
) -> Daemon {
    let endpoint = backend_runtime::derive_endpoint(workspace);
    let mut command = ProcessCommand::new(env!("CARGO_BIN_EXE_backend-journey-locald"));
    command
        .arg("--endpoint")
        .arg(&endpoint)
        .arg("--workspace")
        .arg(workspace)
        .args(["--profile", "builtin", "--authority-secret-file"])
        .arg(authority)
        .args(["--registry-discovery-source", &registry.source("npm")])
        .args(["--registry-discovery-source", &registry.source("pypi")])
        .args(["--registry-source", &registry.source("npm")])
        .args(["--registry-source", &registry.source("pypi")])
        .args([
            "--advisory-offline",
            "--forge-offline",
            "--max-frame",
            "1048576",
            "--timeout-ms",
            "120000",
            "--idle-timeout-ms",
            "0",
        ])
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    if offline {
        command.args(["--registry-discovery-offline", "--registry-offline"]);
    }
    let child = command.spawn().expect("spawn isolated locald");
    let mut daemon = Daemon {
        child: Some(child),
        endpoint,
    };
    daemon.wait_ready();
    daemon
}

fn start_osv_owner(
    workspace: &Path,
    authority: &Path,
    registry: &NativeRegistry,
    snapshot: bool,
) -> Daemon {
    let endpoint = backend_runtime::derive_endpoint(workspace);
    let mut command = ProcessCommand::new(env!("CARGO_BIN_EXE_backend-journey-locald"));
    command
        .arg("--endpoint")
        .arg(&endpoint)
        .arg("--workspace")
        .arg(workspace)
        .args(["--profile", "builtin", "--authority-secret-file"])
        .arg(authority)
        .args(["--registry-discovery-source", &registry.source("npm")])
        .args(["--registry-source", &registry.source("npm")])
        .args([
            "--advisory-osv",
            &format!(
                "{}/{}",
                registry.base,
                if snapshot { "osv.zip" } else { "osv" }
            ),
        ])
        .args([
            "--advisory-osv-scope",
            "npm",
            "--forge-offline",
            "--max-frame",
            "1048576",
            "--timeout-ms",
            "120000",
            "--idle-timeout-ms",
            "0",
        ])
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    let child = command.spawn().expect("spawn configured locald");
    let mut daemon = Daemon {
        child: Some(child),
        endpoint,
    };
    daemon.wait_ready();
    daemon
}

fn seed_legacy_unscoped(workspace: &Path, registry: &NativeRegistry) -> Vec<u8> {
    let endpoint =
        RegistryEndpoint::new(RegistryEcosystem::Pypi, format!("{}/pypi", registry.base))
            .expect("endpoint");
    let source = discovery_source_identity(&endpoint);
    let observed_at = DiscoveryObservedAt::from_unix_millis(10);
    let fact = DiscoveryFact {
        source,
        coordinate: backend_engine::registry::PackageCoordinate::parse(
            "pkg:pypi/legacy-unknown@1.0.0",
        )
        .expect("legacy coordinate"),
        standing: DiscoveryStanding::Published,
        observed_at,
        source_event: DiscoverySourceEvent::Unordered,
        source_event_time: None,
        proof: [9; 32],
        metadata: DiscoveryMetadata {
            advisories: DiscoveryFacet::Known(vec![DiscoveryAdvisory {
                id: "GHSA-legacy-wrong".to_owned(),
                aliases: DiscoveryFacet::Unknown,
                summary: DiscoveryFacet::Unknown,
                severity: DiscoveryFacet::Unknown,
                fixed_in: DiscoveryFacet::Unknown,
            }]),
            ..DiscoveryMetadata::default()
        },
    };
    let batch = DiscoveryBatch {
        source,
        expected_base_sequence: 0,
        previous_cursor: DiscoveryCursor::default(),
        next_cursor: DiscoveryCursor::default(),
        source_high_watermark: DiscoveryCursor::default(),
        caught_up: false,
        observed_at,
        completeness: DiscoveryCompleteness::Windowed,
        facts: vec![fact],
        package_retractions: Vec::new(),
    };
    let payload = serde_json::to_vec(&json!({"version":2,"batch":batch})).expect("legacy envelope");
    assert!(!String::from_utf8_lossy(&payload).contains("advisory_release_scope"));
    let mut frame = b"DISCOV01".to_vec();
    frame.extend_from_slice(&2_u16.to_be_bytes());
    frame.extend_from_slice(
        &u32::try_from(payload.len())
            .expect("frame length")
            .to_be_bytes(),
    );
    frame.extend_from_slice(&payload);
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.registry.discovery.transaction.v2\0");
    hasher.update(&payload);
    frame.extend_from_slice(hasher.finalize().as_bytes());
    let root = workspace.join("registry-discovery");
    std::fs::create_dir_all(&root).expect("legacy journal directory");
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
        .expect("private directory");
    std::fs::write(root.join("catalog.journal"), &frame).expect("legacy frame");
    frame
}

fn matching_discovery<'a>(page: &'a Value, proof: &[u8; 32]) -> Option<&'a Value> {
    let proof = serde_json::to_value(proof).expect("proof JSON");
    page.get("records")?
        .as_array()?
        .iter()
        .filter_map(|row| row.get("discovery"))
        .find(|candidate| candidate.get("proof") == Some(&proof))
}

fn assert_mixed_historical(page: &Value, proof: &[u8; 32], source: &Value, time: u64) {
    let expected_proof = serde_json::to_value(proof).expect("proof JSON");
    let group = page["records"]
        .as_array()
        .expect("search records")
        .iter()
        .filter_map(|row| row.get("package_group"))
        .find(|group| {
            group["releases"].as_array().is_some_and(|releases| {
                releases.iter().any(|release| {
                    release["value"]["proof"] == expected_proof
                        && release["value"]["source"] == *source
                })
            })
        })
        .unwrap_or_else(|| panic!("mixed search omitted historical source proof: {page}"));
    assert_eq!(
        group["release_match_scope"], "release-matches",
        "the old observation itself supplied renderer and the advisory: {group}"
    );
    let candidate = group["releases"]
        .as_array()
        .expect("releases")
        .iter()
        .find(|release| release["value"]["proof"] == expected_proof)
        .expect("historical release");
    assert_eq!(candidate["value"]["freshness"]["state"], "historical");
    assert_eq!(candidate["value"]["freshness"]["observed_at_millis"], time);
    assert_eq!(
        candidate["value"]["metadata"]["advisories"]["state"],
        "known"
    );
}

fn assert_current_search(page: &Value, proof: &[u8; 32], source: &Value, time: u64) {
    let expected_proof = serde_json::to_value(proof).expect("proof JSON");
    let candidate = page["records"]
        .as_array()
        .expect("search records")
        .iter()
        .filter_map(|row| row.get("package_group"))
        .flat_map(|group| group["releases"].as_array().into_iter().flatten())
        .map(|release| &release["value"])
        .find(|candidate| candidate["proof"] == expected_proof && candidate["source"] == *source)
        .unwrap_or_else(|| panic!("current description search omitted its source proof: {page}"));
    assert_eq!(candidate["freshness"]["observed_at_millis"], time);
    assert_eq!(candidate["metadata"]["advisories"]["state"], "unknown");
}

fn wait_for_discovery(endpoint: &Path, workspace: &Path, purl: &str) -> Value {
    let deadline = Instant::now() + INDEX_DEADLINE;
    loop {
        let page = cli_json(endpoint, workspace, &["index-search", "fixture"]);
        if page.to_string().contains(purl) {
            return page;
        }
        assert!(
            Instant::now() < deadline,
            "discovery did not reach {purl}: {page}"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

fn wait_for_package(
    endpoint: &Path,
    workspace: &Path,
    old: &[u8; 32],
    new: Option<&[u8; 32]>,
) -> Value {
    let deadline = Instant::now() + INDEX_DEADLINE;
    loop {
        let page = cli_json(endpoint, workspace, &["package", "pkg:pypi/fixture@1.0.0"]);
        if matching_discovery(&page, old).is_some()
            && new.is_none_or(|proof| matching_discovery(&page, proof).is_some())
        {
            return page;
        }
        assert!(
            Instant::now() < deadline,
            "package evidence did not arrive: {page}"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

fn assert_unconfigured_advisory(page: &Value) {
    let decision = page["records"]
        .as_array()
        .expect("advisory records")
        .iter()
        .find(|row| row["title"] == "security decision")
        .expect("security decision row");
    let tags = decision["tags"].as_array().expect("decision tags");
    assert!(tags.iter().any(|tag| tag == "coverage: unknown"), "{page}");
    assert!(tags.iter().any(|tag| tag == "freshness: unknown"), "{page}");
    assert!(
        tags.iter().any(|tag| tag
            .as_str()
            .is_some_and(|tag| tag.starts_with("decision: warn"))),
        "{page}"
    );
}

// JSON remains partial; only the scoped ZIP may establish complete coverage.
#[test]
fn configured_osv_partial_feed_survives_public_discovery_and_reopen() {
    configured_osv_discovery_lifecycle(false);
}

#[test]
fn configured_osv_scoped_snapshot_survives_public_discovery_and_reopen() {
    configured_osv_discovery_lifecycle(true);
}

fn configured_osv_discovery_lifecycle(snapshot: bool) {
    const PURL: &str = "pkg:npm/fixture@1.0.0";
    let root = fresh_root("configured-osv-discovery");
    let workspace = root.join("workspace");
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).expect("private root");
    std::fs::set_permissions(&workspace, std::fs::Permissions::from_mode(0o700))
        .expect("private workspace");
    let secret = authority(&root);
    let registry = NativeRegistry::start();
    registry.osv_mode.store(1, Ordering::Release);
    let mut owner = start_osv_owner(&workspace, &secret, &registry, snapshot);
    let endpoint = owner.endpoint.clone();
    let initial_refresh = cli_json(&endpoint, &workspace, &["advisory-refresh"]);
    assert!(
        initial_refresh["records"]
            .as_array()
            .expect("feed records")
            .iter()
            .any(|row| {
                row["title"] == "osv"
                    && row["tags"].as_array().is_some_and(|tags| {
                        tags.iter().any(|tag| {
                            tag == if snapshot {
                                "a complete source"
                            } else {
                                "a partial source"
                            }
                        })
                    })
            }),
        "{initial_refresh}"
    );
    let _ = wait_for_discovery(&endpoint, &workspace, PURL);
    let mut failures = Vec::new();
    let expected_coverage = if snapshot { "complete" } else { "partial" };
    let expected_id = if snapshot {
        "OSV-ZIP-POSITIVE"
    } else {
        "OSV-LOOPBACK-1"
    };
    for (stage, coverage, freshness, positive) in [
        ("fresh-empty", expected_coverage, "fresh", false),
        ("stale-empty", expected_coverage, "stale", false),
        ("unavailable-empty", "unavailable", "stale", false),
        ("fresh-positive", expected_coverage, "fresh", true),
        ("reopened-positive", expected_coverage, "fresh", true),
        ("stale-positive", expected_coverage, "stale", true),
        ("unavailable-positive", "unavailable", "stale", true),
        (
            "reopened-unavailable-positive",
            "unavailable",
            "stale",
            true,
        ),
    ] {
        match stage {
            "reopened-positive" | "reopened-unavailable-positive" => {
                let feed_path = if snapshot { "/osv.zip" } else { "/osv" };
                let previous_count = registry
                    .requests()
                    .iter()
                    .filter(|path| path.as_str() == feed_path)
                    .count();
                owner.crash();
                owner = start_osv_owner(&workspace, &secret, &registry, snapshot);
                assert_eq!(
                    registry
                        .requests()
                        .iter()
                        .filter(|path| path.as_str() == feed_path)
                        .count(),
                    previous_count,
                    "reopen must use persisted authority without feed refresh"
                );
            }
            "stale-empty" => {
                registry.osv_mode.store(4, Ordering::Release);
                let refreshed = cli_json(&endpoint, &workspace, &["advisory-refresh"]);
                eprintln!("{stage} refresh: {refreshed}");
            }
            "stale-positive" => {
                registry.osv_mode.store(3, Ordering::Release);
                let refreshed = cli_json(&endpoint, &workspace, &["advisory-refresh"]);
                eprintln!("{stage} refresh: {refreshed}");
            }
            "unavailable-empty" | "unavailable-positive" => {
                registry.osv_mode.store(2, Ordering::Release);
                let _ = cli_json(&endpoint, &workspace, &["advisory-refresh"]);
            }
            "fresh-positive" => {
                registry.osv_mode.store(0, Ordering::Release);
                let refreshed = mcp(
                    &endpoint,
                    &workspace,
                    &secret,
                    &[(
                        30,
                        json!({"name":"backend.advisory_refresh","arguments":{}}),
                    )],
                );
                assert_ne!(
                    refreshed[&30]["result"]["isError"], true,
                    "MCP feed refresh: {refreshed:?}"
                );
            }
            _ => {}
        }
        let package = cli_json(&endpoint, &workspace, &["package", PURL]);
        assert!(
            package.to_string().contains("discovered · unacquired"),
            "{stage}: {package}"
        );
        let cli_advisory = cli_json(&endpoint, &workspace, &["advisory", PURL]);
        let replies = mcp(
            &endpoint,
            &workspace,
            &secret,
            &[
                (
                    31,
                    json!({"name":"backend.advisory","arguments":{"package":PURL,"detail":"full"}}),
                ),
                (
                    32,
                    json!({"name":"backend.package","arguments":{"package":PURL,"detail":"full"}}),
                ),
            ],
        );
        let mcp_advisory = mcp_structured(&replies, 31, "configured advisory");
        let mcp_package = mcp_structured(&replies, 32, "discovered package");
        assert!(
            mcp_package.to_string().contains("discovered · unacquired"),
            "{stage}: {mcp_package}"
        );
        for (surface, page) in [("CLI", &package), ("MCP", mcp_package)] {
            let candidates = page["package_discovery"]["observation"]["candidates"]
                .as_array()
                .expect("source observations remain separate from advisory authority");
            assert!(!candidates.is_empty(), "{stage} {surface}: {page}");
            for source in candidates {
                assert_eq!(source["coordinate"], PURL, "{stage} {surface}: {source}");
                assert_eq!(source["source"].as_array().map(Vec::len), Some(32));
                assert_eq!(source["proof"].as_array().map(Vec::len), Some(32));
                assert!(source["freshness"]["observed_at_millis"].as_u64().is_some());
                assert!(source["metadata"].is_object());
            }
            let authority = &page["package_discovery"]["advisory"];
            let source_objects = authority["advisories"].as_array();
            let has_positive = source_objects
                .is_some_and(|rows| rows.iter().any(|row| row["canonical_id"] == expected_id));
            if authority["coverage"] != coverage
                || authority["freshness"] != freshness
                || !authority["registry_standing_observed"].is_null()
                || has_positive != positive
            {
                failures.push(format!("{stage} {surface} package authority: {page}"));
            }
        }
        for (surface, page) in [("CLI", &cli_advisory), ("MCP", mcp_advisory)] {
            let records = page["records"].as_array().expect("advisory records");
            let decision = records
                .iter()
                .find(|row| row["title"] == "security decision")
                .expect("decision row");
            let tags = decision["tags"].as_array().expect("decision tags");
            let has = |expected: &str| tags.iter().any(|tag| tag.as_str() == Some(expected));
            let has_positive = records.iter().any(|row| row["title"] == expected_id);
            let decision_prefix = if positive {
                "decision: deny"
            } else {
                "decision: warn"
            };
            if !has(&format!("coverage: {coverage}"))
                || !has(&format!("freshness: {freshness}"))
                || !has("yanked: unknown")
                || !has("unlisted: unknown")
                || has_positive != positive
                || !tags.iter().any(|tag| {
                    tag.as_str()
                        .is_some_and(|tag| tag.starts_with(decision_prefix))
                })
            {
                failures.push(format!("{stage} {surface}: {page}"));
            }
        }
    }
    let feed_path = if snapshot { "/osv.zip" } else { "/osv" };
    assert!(registry.requests().iter().any(|path| path == feed_path));
    assert!(
        registry
            .requests()
            .iter()
            .all(|path| !path.contains("archive") && !path.contains("download"))
    );
    assert!(
        failures.is_empty(),
        "configured advisory lifecycle failures: {}",
        failures.join("; ")
    );
}

#[test]
fn unversioned_registry_locator_points_to_discovery_on_cli_and_mcp() {
    let root = fresh_root("unversioned-discovery");
    let workspace = root.join("workspace");
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).expect("private root");
    std::fs::set_permissions(&workspace, std::fs::Permissions::from_mode(0o700))
        .expect("private workspace");
    let secret = authority(&root);
    let registry = NativeRegistry::start();
    let daemon = start_owner(&workspace, &secret, &registry, false);
    let endpoint = &daemon.endpoint;
    for command in ["package", "package-versions"] {
        let output = cli(endpoint, &workspace, "json", &[command, "pkg:npm/fixture"]);
        assert!(!output.status.success());
        let reply: Value = serde_json::from_slice(&output.stdout).expect("CLI fault");
        assert_eq!(reply["operand"], "pkg:npm/fixture");
        assert!(
            reply["detail"]
                .as_str()
                .expect("detail")
                .contains("exact version")
        );
        assert_eq!(reply["call"]["name"], "backend.index_search");
        assert_eq!(reply["call"]["arguments"]["query"], "fixture");
    }
    let replies = mcp(
        endpoint,
        &workspace,
        &secret,
        &[
            (
                10,
                json!({"name":"backend.package","arguments":{"package":"pkg:npm/fixture"}}),
            ),
            (
                11,
                json!({"name":"backend.package_versions","arguments":{"package":"pkg:npm/fixture"}}),
            ),
        ],
    );
    for id in [10, 11] {
        let reply = &replies[&id]["result"];
        assert_eq!(reply["isError"], true);
        assert_eq!(reply["structuredContent"]["operand"], "pkg:npm/fixture");
        assert_eq!(
            reply["structuredContent"]["call"]["name"],
            "backend.index_search"
        );
        assert_eq!(
            reply["structuredContent"]["call"]["arguments"]["query"],
            "fixture"
        );
    }
    let found = wait_for_discovery(endpoint, &workspace, "pkg:npm/fixture@1.0.0");
    assert!(found.to_string().contains("pkg:npm/fixture@1.0.0"));
    let exact = cli_json(endpoint, &workspace, &["package", "pkg:npm/fixture@1.0.0"]);
    assert!(exact.to_string().contains("pkg:npm/fixture@1.0.0"));
}

#[test]
fn loopback_npm_pypi_public_metadata_refresh_and_offline_reopen() {
    let root = fresh_root("native-metadata");
    let workspace = root.join("workspace");
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).expect("private root");
    std::fs::set_permissions(&workspace, std::fs::Permissions::from_mode(0o700))
        .expect("private workspace");
    let secret = authority(&root);
    let registry = NativeRegistry::start();
    let old_frame = seed_legacy_unscoped(&workspace, &registry);
    let old_proof = *blake3::hash(PYPI_OLD.as_bytes()).as_bytes();
    let new_proof = *blake3::hash(PYPI_NEW.as_bytes()).as_bytes();
    let mut daemon = start_owner(&workspace, &secret, &registry, false);
    let endpoint = daemon.endpoint.clone();
    for purl in ["pkg:npm/fixture@1.0.0", "pkg:pypi/fixture@1.0.0"] {
        let _ = wait_for_discovery(&endpoint, &workspace, purl);
        let package = cli_json(&endpoint, &workspace, &["package", purl]);
        assert!(
            package.to_string().contains("discovered · unacquired"),
            "{package}"
        );
        assert!(
            !package.to_string().contains("README witness"),
            "discovery must not invent README acquisition: {package}"
        );
        assert!(
            !package.to_string().contains(">=18") && !package.to_string().contains(">=3.10"),
            "discovery must not invent engine acquisition: {package}"
        );
        let deps = cli(&endpoint, &workspace, "json", &["dependencies", purl]);
        let dependency_text = fault(&deps).to_lowercase();
        assert!(
            dependency_text.contains("unavailable") || dependency_text.contains("acquir"),
            "{dependency_text}"
        );
        let versions = cli(&endpoint, &workspace, "json", &["package-versions", purl]);
        let version_text = fault(&versions).to_lowercase();
        assert!(
            version_text.contains("discover") || version_text.contains("acquir"),
            "{version_text}"
        );
        let advisory = cli_json(&endpoint, &workspace, &["advisory", purl]);
        assert_unconfigured_advisory(&advisory);
    }
    let legacy = cli_json(
        &endpoint,
        &workspace,
        &["index-search", "pkg:pypi/legacy-unknown@1.0.0"],
    );
    assert!(
        !legacy.to_string().contains("GHSA-legacy-wrong"),
        "legacy unscoped list must not identify this release: {legacy}"
    );
    let legacy_unknown = legacy["records"]
        .as_array()
        .expect("legacy search records")
        .iter()
        .filter_map(|row| row.get("package_group"))
        .flat_map(|group| group["releases"].as_array().into_iter().flatten())
        .any(|release| {
            release["value"]["coordinate"] == "pkg:pypi/legacy-unknown@1.0.0"
                && release["value"]["metadata"]["advisories"]["state"] == "unknown"
        });
    assert!(
        legacy_unknown,
        "legacy release must retain unknown advisory state: {legacy}"
    );
    let legacy_query = cli_json(
        &endpoint,
        &workspace,
        &["index-search", "GHSA-legacy-wrong"],
    );
    assert!(
        !legacy_query.to_string().contains("legacy-unknown"),
        "{legacy_query}"
    );
    let empty = cli_json(
        &endpoint,
        &workspace,
        &["package", "pkg:pypi/empty-fixture@1.0.0"],
    );
    let scoped_empty = empty["records"]
        .as_array()
        .expect("empty package records")
        .iter()
        .filter_map(|row| row.get("discovery"))
        .any(|candidate| {
            candidate["metadata"]["advisories"]["state"] == "known"
                && candidate["metadata"]["advisories"]["value"]
                    .as_array()
                    .is_some_and(Vec::is_empty)
        });
    assert!(
        scoped_empty,
        "scoped empty source list must remain visible: {empty}"
    );
    let empty_authority = cli_json(
        &endpoint,
        &workspace,
        &["advisory", "pkg:pypi/empty-fixture@1.0.0"],
    );
    assert_unconfigured_advisory(&empty_authority);
    let first = wait_for_package(&endpoint, &workspace, &old_proof, None);
    let old = matching_discovery(&first, &old_proof).expect("old point observation");
    let old_source = old["source"].clone();
    let point_endpoint = RegistryEndpoint::new(
        RegistryEcosystem::Pypi,
        format!("{}/pypi/pypi/fixture/json", registry.base),
    )
    .expect("point metadata endpoint");
    let expected_point_source =
        serde_json::to_value(discovery_source_identity(&point_endpoint).id())
            .expect("point source JSON");
    assert_eq!(
        old_source, expected_point_source,
        "point proof must stay bound to its actual endpoint identity"
    );
    let old_time = old["freshness"]["observed_at_millis"]
        .as_u64()
        .expect("old time");
    daemon.crash();
    registry.advance();
    let mut daemon = start_owner(&workspace, &secret, &registry, false);
    let refreshed = wait_for_package(&endpoint, &workspace, &old_proof, Some(&new_proof));
    let previous = matching_discovery(&refreshed, &old_proof).expect("historical positive");
    let current = matching_discovery(&refreshed, &new_proof).expect("current unknown");
    assert_eq!(previous["source"], old_source);
    assert_eq!(previous["freshness"]["state"], "historical");
    assert_eq!(previous["freshness"]["observed_at_millis"], old_time);
    assert_eq!(current["source"], old_source);
    assert!(
        current["freshness"]["observed_at_millis"]
            .as_u64()
            .expect("current time")
            > old_time
    );
    let new_time = current["freshness"]["observed_at_millis"]
        .as_u64()
        .expect("current time");
    assert_eq!(current["metadata"]["advisories"]["state"], "unknown");
    let current_search = cli_json(&endpoint, &workspace, &["index-search", "renderer"]);
    assert_current_search(&current_search, &new_proof, &old_source, new_time);
    let mixed = cli_json(
        &endpoint,
        &workspace,
        &["index-search", "renderer GHSA-pypi-fixture"],
    );
    assert_mixed_historical(&mixed, &old_proof, &old_source, old_time);
    let replies = mcp(
        &endpoint,
        &workspace,
        &secret,
        &[
            (
                10,
                json!({"name":"backend.package","arguments":{"package":"pkg:pypi/fixture@1.0.0","detail":"full"}}),
            ),
            (
                11,
                json!({"name":"backend.index_search","arguments":{"query":"renderer GHSA-pypi-fixture","limit":10,"detail":"full"}}),
            ),
            (
                12,
                json!({"name":"backend.advisory","arguments":{"package":"pkg:pypi/fixture@1.0.0","detail":"full"}}),
            ),
            (
                13,
                json!({"name":"backend.dependencies","arguments":{"package":"pkg:pypi/fixture@1.0.0","detail":"full"}}),
            ),
            (
                14,
                json!({"name":"backend.package_versions","arguments":{"package":"pkg:pypi/fixture@1.0.0","detail":"full"}}),
            ),
            (
                15,
                json!({"name":"backend.package","arguments":{"package":"pkg:pypi/empty-fixture@1.0.0","detail":"full"}}),
            ),
            (
                16,
                json!({"name":"backend.advisory","arguments":{"package":"pkg:pypi/empty-fixture@1.0.0","detail":"full"}}),
            ),
        ],
    );
    let mcp_package = mcp_structured(&replies, 10, "package");
    let mcp_old = matching_discovery(mcp_package, &old_proof).expect("MCP historical positive");
    let mcp_new = matching_discovery(mcp_package, &new_proof).expect("MCP current unknown");
    assert_eq!(mcp_old["source"], old_source);
    assert_eq!(mcp_old["freshness"]["state"], "historical");
    assert_eq!(mcp_old["freshness"]["observed_at_millis"], old_time);
    assert_eq!(mcp_new["source"], old_source);
    assert_eq!(mcp_new["freshness"]["observed_at_millis"], new_time);
    assert_eq!(mcp_new["metadata"]["advisories"]["state"], "unknown");
    let mcp_mixed = mcp_structured(&replies, 11, "index-search");
    assert_mixed_historical(mcp_mixed, &old_proof, &old_source, old_time);
    assert_unconfigured_advisory(mcp_structured(&replies, 12, "advisory"));
    assert_unconfigured_advisory(mcp_structured(&replies, 16, "empty-source advisory"));
    for id in [13, 14] {
        let result = &replies[&id]["result"];
        let text = result.to_string().to_lowercase();
        assert!(
            text.contains("unavailable") || text.contains("acquir") || text.contains("discover"),
            "{result}"
        );
    }
    let mcp_empty = mcp_structured(&replies, 15, "scoped empty source");
    assert!(
        mcp_empty["records"]
            .as_array()
            .expect("records")
            .iter()
            .filter_map(|row| row.get("discovery"))
            .any(
                |candidate| candidate["metadata"]["advisories"]["state"] == "known"
                    && candidate["metadata"]["advisories"]["value"]
                        .as_array()
                        .is_some_and(Vec::is_empty)
            ),
        "{mcp_empty}"
    );
    daemon.crash();
    let requests_before = registry.requests();
    let mut offline = start_owner(&workspace, &secret, &registry, true);
    let reopened = wait_for_package(&endpoint, &workspace, &old_proof, Some(&new_proof));
    let old_after = matching_discovery(&reopened, &old_proof).expect("reopened historical");
    let new_after = matching_discovery(&reopened, &new_proof).expect("reopened current");
    assert_eq!(old_after["source"], old_source);
    assert_eq!(old_after["freshness"]["observed_at_millis"], old_time);
    assert_eq!(old_after["freshness"]["state"], "historical");
    assert_eq!(new_after["metadata"]["advisories"]["state"], "unknown");
    let offline_current = cli_json(&endpoint, &workspace, &["index-search", "renderer"]);
    assert_current_search(&offline_current, &new_proof, &old_source, new_time);
    let offline_mixed = cli_json(
        &endpoint,
        &workspace,
        &["index-search", "renderer GHSA-pypi-fixture"],
    );
    assert_mixed_historical(&offline_mixed, &old_proof, &old_source, old_time);
    let offline_advisory = cli_json(
        &endpoint,
        &workspace,
        &["advisory", "pkg:pypi/empty-fixture@1.0.0"],
    );
    assert_unconfigured_advisory(&offline_advisory);
    let offline_replies = mcp(
        &endpoint,
        &workspace,
        &secret,
        &[
            (
                20,
                json!({"name":"backend.package","arguments":{"package":"pkg:pypi/fixture@1.0.0","detail":"full"}}),
            ),
            (
                21,
                json!({"name":"backend.index_search","arguments":{"query":"renderer GHSA-pypi-fixture","limit":10,"detail":"full"}}),
            ),
            (
                22,
                json!({"name":"backend.advisory","arguments":{"package":"pkg:pypi/empty-fixture@1.0.0","detail":"full"}}),
            ),
        ],
    );
    let offline_package = mcp_structured(&offline_replies, 20, "offline package");
    let offline_mcp_old =
        matching_discovery(offline_package, &old_proof).expect("offline MCP historical");
    let offline_mcp_new =
        matching_discovery(offline_package, &new_proof).expect("offline MCP current");
    assert_eq!(offline_mcp_old["source"], old_source);
    assert_eq!(offline_mcp_old["freshness"]["state"], "historical");
    assert_eq!(offline_mcp_old["freshness"]["observed_at_millis"], old_time);
    assert_eq!(offline_mcp_new["source"], old_source);
    assert_eq!(offline_mcp_new["freshness"]["observed_at_millis"], new_time);
    assert_eq!(
        offline_mcp_new["metadata"]["advisories"]["state"],
        "unknown"
    );
    assert_mixed_historical(
        mcp_structured(&offline_replies, 21, "offline search"),
        &old_proof,
        &old_source,
        old_time,
    );
    assert_unconfigured_advisory(mcp_structured(&offline_replies, 22, "offline advisory"));
    assert_eq!(
        registry.requests(),
        requests_before,
        "offline restart contacted registry"
    );
    let journal =
        std::fs::read(workspace.join("registry-discovery/catalog.journal")).expect("journal");
    assert!(
        journal.starts_with(&old_frame),
        "replay changed original legacy envelope bytes"
    );
    assert!(!journal.is_empty());
    assert!(
        registry
            .requests()
            .iter()
            .all(|path| !path.contains("archive") && !path.contains("download")),
        "metadata journey fetched archive"
    );
    offline.crash();
}
