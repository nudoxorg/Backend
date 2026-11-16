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
