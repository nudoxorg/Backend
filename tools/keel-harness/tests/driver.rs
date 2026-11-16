//! Adversarial tests run the built harness as an external process.
#![cfg(unix)]
use serde_json::{Value, json};
use std::fs;
use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Sandbox(PathBuf);
impl Sandbox {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "keel-driver-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).expect("private integration scratch");
        fs::create_dir(root.join("bins")).expect("bin directory");
        symlink("/bin/sh", root.join("bins/probe")).expect("real shell process artifact");
        Self(root)
    }
    fn invoke(&self, manifest: &Value, extra: &[&str]) -> Output {
        let path = self.0.join("manifest.json");
        fs::write(
            &path,
            serde_json::to_vec(manifest).expect("manifest encoding"),
        )
        .expect("write manifest");
        Command::new(env!("CARGO_BIN_EXE_backend-keel-harness"))
            .arg("--manifest")
            .arg(path)
            .arg("--bin-dir")
            .arg(self.0.join("bins"))
            .arg("--evidence")
            .arg(self.0.join("evidence"))
            .args(extra)
            .output()
            .expect("real harness process")
    }
    fn report(&self) -> Value {
        serde_json::from_slice(
            &fs::read(self.0.join("evidence/report.json")).expect("report retained"),
        )
        .expect("valid evidence")
    }
}
impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn process_case(id: &str, script: &str) -> Value {
    json!({"id":id,"language":"protocol","daemon":false,"steps":[{"id":"observe","kind":"process","binary":"probe","args":["-c",script],"timeout_seconds":1,"expect":{"exit":0,"json":[{"pointer":"/answer","equals":"independent-oracle"}]}}]})
}
fn manifest(cases: Vec<Value>) -> Value {
    json!({"schema":1,"cases":cases})
}

#[test]
fn external_driver_rejects_vacuous_and_escaping_definitions_before_spawning() {
    for mutation in ["unknown-field", "traversal", "exit-only", "duplicate-id"] {
        let world = Sandbox::new();
        let mut case = process_case("probe", "echo '{\"answer\":\"independent-oracle\"}'");
        match mutation {
            "unknown-field" => {
                case["steps"][0]["typo"] = json!(true);
            }
            "traversal" => {
                case["files"] = json!({"../escape":"forbidden"});
            }
            "exit-only" => {
                case["steps"][0]["expect"]["json"] = json!([]);
            }
            "duplicate-id" => {
                let duplicate = case["steps"][0].clone();
                case["steps"].as_array_mut().expect("steps").push(duplicate);
            }
            _ => unreachable!(),
        }
        let result = world.invoke(&manifest(vec![case]), &[]);
        assert_eq!(
            result.status.code(),
            Some(2),
            "{mutation}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(
            !world.0.join("evidence").exists(),
            "invalid manifest must not produce pass evidence"
        );
        assert!(!world.0.join("escape").exists());
    }
}
#[test]
fn failed_oracle_blocks_its_history_without_hiding_an_independent_pass() {
    let world = Sandbox::new();
    let mut bad = process_case("bad", "echo '{\"answer\":\"wrong\"}'");
    bad["steps"]
        .as_array_mut()
        .expect("steps")
        .push(json!({"id":"must-not-run","kind":"write","files":{"sentinel":"bad dependent ran"}}));
    let good = process_case("good", "echo '{\"answer\":\"independent-oracle\"}'");
    let output = world.invoke(&manifest(vec![bad, good]), &[]);
    assert_eq!(output.status.code(), Some(1));
    let report = world.report();
    assert_eq!(report["status"], "fail");
    assert_eq!(report["cases"][0]["steps"][1]["status"], "blocked");
    assert_eq!(report["cases"][1]["status"], "pass");
    assert!(!world.0.join("evidence/bad/project/sentinel").exists());
    assert_eq!(
        fs::read_to_string(world.0.join("evidence/good/000-observe.stdout")).expect("raw stdout"),
        "{\"answer\":\"independent-oracle\"}\n"
    );
}
#[test]
fn ordering_cannot_drop_unknown_or_duplicate_a_case() {
    for ids in [json!(["a"]), json!(["a", "a"]), json!(["a", "unknown"])] {
        let world = Sandbox::new();
        let order = world.0.join("order.json");
        fs::write(&order, json!({"schema":1,"cases":ids}).to_string()).expect("order");
        let a = process_case("a", "echo '{\"answer\":\"independent-oracle\"}'");
        let b = process_case("b", "echo '{\"answer\":\"independent-oracle\"}'");
        let result = world.invoke(
            &manifest(vec![a, b]),
            &["--order", order.to_str().expect("path")],
        );
        assert_eq!(result.status.code(), Some(2));
        assert!(!world.0.join("evidence").exists());
    }
    let world = Sandbox::new();
    let order = world.0.join("order.json");
    fs::write(&order, json!({"schema":1,"cases":["b","a"]}).to_string()).expect("order");
    let result = world.invoke(
        &manifest(vec![
            process_case("a", "echo '{\"answer\":\"independent-oracle\"}'"),
            process_case("b", "echo '{\"answer\":\"independent-oracle\"}'"),
        ]),
        &["--order", order.to_str().expect("path")],
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(world.report()["cases"][0]["id"], "b");
}
fn alive(pid: &str) -> bool {
    let Some(pid) = pid
        .parse::<i32>()
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    else {
        return false;
    };
    #[cfg(target_os = "linux")]
    if let Ok(stat) = fs::read_to_string(format!("/proc/{}/stat", pid.as_raw_nonzero())) {
        if stat
            .rsplit_once(')')
            .is_some_and(|(_, tail)| tail.trim_start().starts_with('Z'))
        {
            return false;
        }
    }
    rustix::process::test_kill_process(pid).is_ok()
}

#[test]
fn timeout_retires_descendants_and_retains_a_failure_instead_of_a_pass() {
    let world = Sandbox::new();
    let script = "sleep 20 & echo $! > descendant.pid; wait";
    let start = Instant::now();
    let result = world.invoke(&manifest(vec![process_case("hung", script)]), &[]);
    assert_eq!(result.status.code(), Some(1));
    assert!(start.elapsed().as_secs() < 8, "custody deadline escaped");
    let pid = fs::read_to_string(world.0.join("evidence/hung/project/descendant.pid"))
        .expect("child identity");
    // Reparented killed grandchildren may be zombies briefly; process output
    // and failed step prove the timeout, while no sleeping descendant may live.
    let deadline = Instant::now() + std::time::Duration::from_secs(2);
    while alive(pid.trim()) && Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        !alive(pid.trim()),
        "descendant survived process-group timeout: {}",
        pid.trim()
    );
    assert_eq!(world.report()["cases"][0]["steps"][0]["status"], "fail");
    let stderr = fs::read_to_string(world.0.join("evidence/hung/000-observe.stderr"))
        .expect("timeout evidence");
    assert!(stderr.contains("timed out"));
}
#[test]
fn parent_exit_does_not_wait_for_a_descendant_holding_stdout() {
    let world = Sandbox::new();
    let result = world.invoke(
        &manifest(vec![process_case(
            "pipe-holder",
            "sleep 20 & echo '{\"answer\":\"independent-oracle\"}'; exit 0",
        )]),
        &[],
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn copied_identity_bindings_preserve_json_and_symbol_bytes_across_real_processes() {
    let world = Sandbox::new();
    let identity = json!({"kind":"selected-symbol-id","bytes":(0u8..32).collect::<Vec<_>>()});
    let emit = format!("printf '%s' '{}'", json!({"identity":identity}));
    let case = json!({"id":"typed-binding","language":"protocol","daemon":false,"steps":[
        {"id":"capture","kind":"process","binary":"probe","args":["-c",emit],"expect":{"exit":0,"json":[{"pointer":"/identity","equals":identity}]},"bind":{"copied":"/identity","key":{"pointer":"/identity","transform":"selected-symbol-key"}}},
        {"id":"consume","kind":"process","binary":"probe","args":["-c","IFS= read -r value; printf '%s' \"$value\""],"input":{"identity":"${copied}","key":"${key}"},"expect":{"exit":0,"json":[{"pointer":"/identity","equals":"${copied}"},{"pointer":"/key","equals":"symbol:000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"}]}}
    ]});
    let result = world.invoke(&manifest(vec![case]), &[]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(world.report()["cases"][0]["steps"][1]["status"], "pass");
}

#[test]
fn inventory_binds_reviewed_expectations_and_never_writes_execution_evidence() {
    let world = Sandbox::new();
    let mut spec = manifest(vec![process_case(
        "inventory",
        "echo '{\"answer\":\"independent-oracle\"}'",
    )]);
    let first = world.invoke(&spec, &["--inventory"]);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first: Value = serde_json::from_slice(&first.stdout).expect("typed inventory");
    assert_eq!(first["kind"], "keel-contract-inventory");
    assert_eq!(first["cases"][0]["world"], "process");
    spec["cases"][0]["steps"][0]["expect"]["json"][0]["equals"] =
        json!("different-reviewed-contract");
    let second = world.invoke(&spec, &["--inventory"]);
    assert!(second.status.success());
    let second: Value = serde_json::from_slice(&second.stdout).expect("inventory");
    assert_ne!(first["manifest_sha256"], second["manifest_sha256"]);
    assert_ne!(
        first["cases"][0]["definition_sha256"],
        second["cases"][0]["definition_sha256"]
    );
    assert!(!world.0.join("evidence").exists());
}

#[test]
fn failed_foundation_blocks_dependent_world_and_selection_includes_its_prerequisite() {
    let world = Sandbox::new();
    let bad = process_case("foundation", "echo '{\"answer\":\"wrong\"}'");
    let child = json!({"id":"dependent","language":"protocol","daemon":false,"requires":["foundation"],"steps":[{"id":"must-not-materialize","kind":"write","files":{"sentinel":"unexpected"}}]});
    let good = process_case("independent", "echo '{\"answer\":\"independent-oracle\"}'");
    let result = world.invoke(&manifest(vec![child.clone(), good, bad.clone()]), &[]);
    assert_eq!(result.status.code(), Some(1));
    let report = world.report();
    let cases = report["cases"].as_array().expect("case receipts");
    assert_eq!(
        cases
            .iter()
            .find(|v| v["id"] == "dependent")
            .expect("dependent")["status"],
        "blocked"
    );
    assert_eq!(
        cases
            .iter()
            .find(|v| v["id"] == "independent")
            .expect("independent")["status"],
        "pass"
    );
    assert!(
        !world.0.join("evidence/dependent/project").exists(),
        "blocked world must not be created"
    );
    let selected = Sandbox::new();
    let result = selected.invoke(&manifest(vec![child, bad]), &["--case", "dependent"]);
    assert_eq!(result.status.code(), Some(1));
    let report = selected.report();
    assert_eq!(report["cases"].as_array().expect("closure").len(), 2);
    assert_eq!(report["cases"][0]["id"], "foundation");
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires delegated KEEL_CGROUP_ROOT and pinned KEEL_TEST_SETSID; explicit containment lane"]
fn cgroup_contains_a_descendant_that_creates_a_new_session() {
    let root = std::env::var_os("KEEL_CGROUP_ROOT").expect("explicit delegated cgroup root");
    let setsid = std::env::var("KEEL_TEST_SETSID").expect("explicit setsid authority");
    assert!(std::path::Path::new(&setsid).is_absolute());
    let world = Sandbox::new();
    let script = format!(
        "'{}' /bin/sh -c 'echo $$ > detached.pid; sleep 20' & wait",
        setsid.replace('\'', "'\\''")
    );
    let spec = manifest(vec![process_case("detached", &script)]);
    let path = world.0.join("manifest.json");
    fs::write(&path, spec.to_string()).expect("manifest");
    let result = Command::new(env!("CARGO_BIN_EXE_backend-keel-harness"))
        .arg("--manifest")
        .arg(path)
        .arg("--bin-dir")
        .arg(world.0.join("bins"))
        .arg("--evidence")
        .arg(world.0.join("evidence"))
        .env("KEEL_CGROUP_ROOT", &root)
        .env("KEEL_REQUIRE_CGROUP", "1")
        .output()
        .expect("contained harness process");
    assert_eq!(result.status.code(), Some(1));
    let pid = fs::read_to_string(world.0.join("evidence/detached/project/detached.pid"))
        .expect("detached descendant actually started");
    assert!(
        !alive(pid.trim()),
        "new-session descendant escaped cgroup cleanup"
    );
    let report = world.report();
    assert_eq!(report["containment_policy"]["required"], true);
    assert_eq!(report["cases"][0]["steps"][0]["status"], "fail");
    let entries = fs::read_dir(root)
        .expect("delegated group readable")
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with("keel-"))
        .count();
    assert_eq!(entries, 0, "owned scopes removed after timeout");
}
