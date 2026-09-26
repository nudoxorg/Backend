//! Live checker falsifier for cross-file method reference module resolution.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use backend_frontend_typescript::legacy::{Checker, CheckerError};

static TEMP_ID: AtomicU64 = AtomicU64::new(0);

fn main_cjs() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/legacy/checker/main.cjs")
}

fn ensure_typescript() -> PathBuf {
    let prefix = PathBuf::from("/tmp/nudox-typescript");
    let package_json = prefix.join("node_modules/typescript/package.json");
    if !package_json.is_file() {
        let status = Command::new("npm")
            .args(["install", "typescript@5", "--prefix"])
            .arg(&prefix)
            .status()
            .expect("npm install typescript");
        assert!(status.success(), "npm install typescript@5 failed");
    }
    prefix.join("node_modules")
}

fn temp_dir() -> PathBuf {
    let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("nudox-ts-cross-file-{}-{}", std::process::id(), id))
}

#[test]
fn cross_file_method_reference_uses_the_spelled_import() -> Result<(), CheckerError> {
    let dir = temp_dir();
    std::fs::create_dir_all(&dir).expect("create temp directory");
    std::fs::write(
        dir.join("workout.service.ts"),
        "export class WorkoutService { setNote() { return 1; } }\n",
    )
    .expect("write workout.service.ts");
    let weeks = dir.join("weeks.ts");
    std::fs::write(
        &weeks,
        "import { WorkoutService } from \"./workout.service\";\nexport function sync(service: WorkoutService) { service.setNote(); }\n",
    )
    .expect("write weeks.ts");

    let output = Command::new("node")
        .arg(main_cjs())
        .arg(&weeks)
        .env("NODE_PATH", ensure_typescript())
        .output()
        .expect("run checker driver");

    assert!(
        output.status.success(),
        "checker driver failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let report = Checker::default().decode(&output.stdout)?;
    let setnote = report
        .references
        .iter()
        .filter(|reference| reference.name.as_deref() == Some("setNote"))
        .collect::<Vec<_>>();
    assert_eq!(
        setnote.len(),
        1,
        "expected exactly one setNote reference, got {:?}",
        setnote
    );
    assert_eq!(setnote[0].module.as_deref(), Some("./workout.service"));

    for reference in report.references.iter() {
        if let Some(module) = reference.module.as_deref() {
            assert!(
                !module.starts_with('/'),
                "module must not be an absolute path: {module}"
            );
        }
    }

    std::fs::remove_dir_all(&dir).ok();
    Ok(())
}
