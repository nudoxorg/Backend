//! Offline falsifiers for the closed Go-oracle protocol.
//! Each test targets one schema or diagnostic law.
//! Subprocess falsifiers use only temporary local scripts and the oracle override.

use compiler_languages_go::oracle::DeclKind;
use compiler_languages_go::{GoOracle, OracleError};

const GOLDEN: &str = include_str!("transcripts/golden.json");
static ENVIRONMENT: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();

fn adapter() -> GoOracle {
    GoOracle::default()
}

#[test]
fn golden_transcript_decodes_decl_and_promotion() -> Result<(), OracleError> {
    let output = adapter().decode(GOLDEN.as_bytes())?;
    let package = output.packages.first().ok_or_else(|| OracleError::Decode {
        message: "golden package missing".to_owned(),
        transcript: GOLDEN.to_owned(),
    })?;
    let decl = package.decls.first().ok_or_else(|| OracleError::Decode {
        message: "golden declaration missing".to_owned(),
        transcript: GOLDEN.to_owned(),
    })?;
    assert_eq!(decl.name, "CJK名前");
    assert_eq!(decl.kind, DeclKind::Var);
    assert_eq!(decl.pos.as_ref().map(|pos| pos.offset), Some(120));
    assert_eq!(decl.span.as_ref().map(|span| span.start), Some(116));
    let outer = package
        .decls
        .iter()
        .find(|decl| decl.name == "Outer")
        .ok_or_else(|| OracleError::Decode {
            message: "golden Outer declaration missing".to_owned(),
            transcript: GOLDEN.to_owned(),
        })?;
    let promoted = outer
        .promoted_methods
        .first()
        .ok_or_else(|| OracleError::Decode {
            message: "promoted method missing".to_owned(),
            transcript: GOLDEN.to_owned(),
        })?;
    assert_eq!(promoted.origin, "example.com/demo.Inner");
    assert_eq!(outer.const_group, 0);
    Ok(())
}

#[test]
fn unknown_field_is_a_decode_fault() {
    let mutated = GOLDEN.replacen(
        "\"schemaVersion\":3",
        "\"schemaVersion\":3,\"surprise\":true",
        1,
    );
    let error = adapter()
        .decode(mutated.as_bytes())
        .expect_err("surprise cell must fail");
    assert!(
        matches!(error, OracleError::Decode { ref message, .. } if message.contains("surprise"))
    );
}

#[test]
fn unknown_decl_kind_is_retained_in_decode_fault() {
    let mutated = GOLDEN.replacen("\"kind\":\"type\"", "\"kind\":\"renamed\"", 1);
    let error = adapter()
        .decode(mutated.as_bytes())
        .expect_err("renamed kind must fail");
    assert!(
        matches!(error, OracleError::Decode { ref message, transcript } if message.contains("unknown variant") && transcript.contains("renamed"))
    );
}

#[test]
fn stale_schema_names_both_versions() {
    let mutated = GOLDEN.replacen("\"schemaVersion\":3", "\"schemaVersion\":2", 1);
    assert!(matches!(
        adapter().decode(mutated.as_bytes()),
        Err(OracleError::Staleness {
            found: 2,
            expected: 3
        })
    ));
}

#[test]
fn newer_schema_is_rejected_by_the_single_schema_owner() {
    let mutated = GOLDEN.replacen("\"schemaVersion\":3", "\"schemaVersion\":4", 1);
    assert!(matches!(
        adapter().decode(mutated.as_bytes()),
        Err(OracleError::Staleness {
            found: 4,
            expected: 3
        })
    ));
}

#[test]
fn truncated_record_is_a_decode_fault() {
    let truncated = &GOLDEN[..GOLDEN.len().saturating_sub(3)];
    let error = adapter()
        .decode(truncated.as_bytes())
        .expect_err("truncated cell must fail");
    assert!(matches!(error, OracleError::Decode { ref message, .. } if message.contains("EOF")));
}

#[test]
fn unknown_channel_direction_is_a_decode_fault() {
    let mutated = GOLDEN.replacen(
        "\"underlying\":{\"kind\":\"struct\"}",
        "\"underlying\":{\"kind\":\"struct\",\"dir\":\"sideways\"}",
        1,
    );
    let error = adapter()
        .decode(mutated.as_bytes())
        .expect_err("sideways cell must fail");
    assert!(
        matches!(error, OracleError::Decode { ref message, .. } if message.contains("unknown variant") && message.contains("sideways"))
    );
}

#[cfg(unix)]
mod bounded_child {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SCRIPT_ID: AtomicUsize = AtomicUsize::new(0);

    fn script(body: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "nudox-go-test-{}-{}",
            std::process::id(),
            SCRIPT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).expect("test temp directory");
        let path = root.join("oracle.sh");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("test script");
        let mut permissions = std::fs::metadata(&path)
            .expect("script metadata")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&path, permissions).expect("script permissions");
        (root, path)
    }

    fn with_script(body: &str, oracle: GoOracle) -> Result<OracleError, String> {
        let (root, path) = script(body);
        let _guard = super::ENVIRONMENT
            .get_or_init(|| Mutex::new(()))
            .lock()
            .map_err(|_| "lock")?;
        // SAFETY: every test that mutates this process-global variable holds
        // the same mutex, so no concurrent child observes a torn configuration.
        unsafe { std::env::set_var("NUDOX_GO_ORACLE_BIN", &path) };
        let result = oracle.run(&root).expect_err("script must fail");
        unsafe { std::env::remove_var("NUDOX_GO_ORACLE_BIN") };
        std::fs::remove_dir_all(root).map_err(|_| "cleanup")?;
        Ok(result)
    }

    #[test]
    fn output_limit_reports_exact_operands_and_reaps_child() {
        let error = with_script(
            "printf '%0100d' 0",
            GoOracle {
                output_limit: 8,
                timeout: std::time::Duration::from_secs(2),
            },
        )
        .unwrap();
        assert!(matches!(
            error,
            OracleError::OutputLimit {
                phase: "collect",
                stream: "stdout",
                observed: 100,
                limit: 8
            }
        ));
    }

    #[test]
    fn deadline_kills_process_group_and_returns_promptly() {
        let started = std::time::Instant::now();
        let error = with_script(
            "(printf x; sleep 10) & sleep 10",
            GoOracle {
                output_limit: 4096,
                timeout: std::time::Duration::from_secs(1),
            },
        )
        .unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(4));
        assert!(matches!(
            error,
            OracleError::Timeout {
                phase: "collect",
                milliseconds: 1000
            }
        ));
    }

    #[test]
    fn missing_oracle_is_typed_unavailable_without_fallback() {
        let _guard = super::ENVIRONMENT
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        // SAFETY: the process-global test environment is serialized by the mutex.
        unsafe { std::env::set_var("NUDOX_GO_ORACLE_BIN", "/no/such/nudox-go-oracle") };
        let error = GoOracle::default()
            .run(std::path::Path::new("missing"))
            .expect_err("missing oracle must not fall back");
        unsafe { std::env::remove_var("NUDOX_GO_ORACLE_BIN") };
        assert!(matches!(
            error,
            OracleError::ToolingUnavailable {
                tool: "NUDOX_GO_ORACLE_BIN or go run",
                ..
            }
        ));
    }
}

#[test]
fn end_to_end_fixture_preserves_package_and_tagged_declarations() -> Result<(), OracleError> {
    let _guard = ENVIRONMENT
        .get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap();
    let compiler = std::env::var("COMPILER_GO_COMPILER").unwrap_or_else(|_| "go".to_owned());
    if std::process::Command::new(&compiler)
        .arg("version")
        .output()
        .is_err()
    {
        eprintln!("skipping Go oracle e2e: {compiler:?} is unavailable; set COMPILER_GO_COMPILER");
        return Ok(());
    }
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/module");
    let output = adapter().run(&fixture)?;
    let package_names: Vec<&str> = output
        .packages
        .iter()
        .map(|package| package.name.as_str())
        .collect();
    assert!(package_names.contains(&"demo"));
    assert!(package_names.contains(&"sub"));
    let demo = output
        .packages
        .iter()
        .find(|package| package.name == "demo")
        .ok_or_else(|| OracleError::Decode {
            message: "demo package missing".to_owned(),
            transcript: String::new(),
        })?;
    let cjk = demo
        .decls
        .iter()
        .find(|decl| decl.name == "CJK名前")
        .ok_or_else(|| OracleError::Decode {
            message: "CJK名前 declaration missing".to_owned(),
            transcript: String::new(),
        })?;
    assert_eq!(cjk.pos.as_ref().map(|pos| pos.offset), Some(120));
    assert_eq!(
        cjk.span.as_ref().map(|span| (span.start, span.end)),
        Some((116, 139))
    );
    let first = demo
        .decls
        .iter()
        .find(|decl| decl.name == "First")
        .ok_or_else(|| OracleError::Decode {
            message: "First declaration missing".to_owned(),
            transcript: String::new(),
        })?;
    let second = demo
        .decls
        .iter()
        .find(|decl| decl.name == "Second")
        .ok_or_else(|| OracleError::Decode {
            message: "Second declaration missing".to_owned(),
            transcript: String::new(),
        })?;
    assert_eq!((first.value.as_str(), first.group_has_iota), ("0", true));
    assert_eq!((second.value.as_str(), second.group_has_iota), ("1", true));
    let outer = demo
        .decls
        .iter()
        .find(|decl| decl.name == "Outer")
        .ok_or_else(|| OracleError::Decode {
            message: "Outer declaration missing".to_owned(),
            transcript: String::new(),
        })?;
    assert_eq!(outer.promoted_methods[0].origin, "example.com/demo.Inner");
    let active_darwin = cfg!(target_os = "macos") || cfg!(target_os = "ios");
    assert_eq!(
        demo.decls.iter().any(|decl| decl.name == "DarwinOnly"),
        active_darwin
    );
    assert!(
        demo.build_constraints
            .iter()
            .any(|constraint| constraint.file.ends_with("linux.go"))
    );
    Ok(())
}
