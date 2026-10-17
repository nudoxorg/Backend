//! Live checker falsifier for cross-file method reference module resolution.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use backend_frontend_typescript::legacy::{Checker, CheckerError, CheckerIndex};

static TEMP_ID: AtomicU64 = AtomicU64::new(0);

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
    let source = "import { WorkoutService } from \"./workout.service\";\nexport function sync(service: WorkoutService) { service.setNote(); }\n";
    std::fs::write(&weeks, source.as_bytes()).expect("write weeks.ts");

    // Use the real bounded package authority and its selected checker seam.
    // Missing runtime/SDK remains a typed failure; this test never installs or
    // silently substitutes a floating TypeScript release.
    let report = Checker::default().run_in_package_at(
        backend_semantic::vocabulary::TypeScriptSource::TypeScript,
        source.as_bytes(),
        &dir,
        Path::new("weeks.ts"),
    );
    std::fs::remove_dir_all(&dir).expect("remove fixture directory");
    let report = report?;
    let _bound = CheckerIndex::bind(&report, source)?;
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

    Ok(())
}
