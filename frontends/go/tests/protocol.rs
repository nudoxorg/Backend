//! Offline falsifiers for the closed Go-oracle protocol.
//! Each test targets one schema or diagnostic law.
//! Subprocess falsifiers use only temporary local scripts and the oracle override.
#![allow(
    unsafe_code,
    reason = "the serialized oracle-override tests mutate process environment, which edition 2024 marks unsafe"
)]

use backend_frontend_go::legacy::oracle::DeclKind;
use backend_frontend_go::legacy::{DocOwner, GoImage, GoOracle, OracleError};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const GOLDEN: &str = include_str!("transcripts/golden.json");
static ENVIRONMENT: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();

fn adapter() -> GoOracle {
    GoOracle::default()
}

/// Real Go authority tests require an explicitly admitted compiler. Never
/// discover the ambient `go` executable from a test: this host has had a Go
/// process wedge in an uninterruptible kernel wait. The probe itself is
/// bounded, and a timed-out child is left for the operating system to reap
/// rather than turning test cleanup into another unbounded wait.
///
/// A missing or unusable compiler is a typed [`OracleError::ToolingUnavailable`]
/// terminal, never a silent skip, so the suite cannot pass green without its
/// toolchain.
fn explicit_go_toolchain() -> Result<PathBuf, OracleError> {
    fn unavailable(message: String) -> OracleError {
        OracleError::ToolingUnavailable {
            tool: "COMPILER_GO_COMPILER",
            source: std::io::Error::new(std::io::ErrorKind::NotFound, message),
        }
    }
    let compiler = std::env::var("COMPILER_GO_COMPILER")
        .map_err(|error| unavailable(format!("COMPILER_GO_COMPILER is not set: {error}")))?;
    let mut child = Command::new(&compiler)
        .arg("version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| unavailable(format!("bounded compiler spawn failed: {error}")))?;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if status.success() {
                    return Ok(PathBuf::from(compiler));
                }
                return Err(unavailable(format!("compiler probe returned {status}")));
            }
            Ok(None) if Instant::now() < deadline => thread::yield_now(),
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let reap_deadline = Instant::now() + Duration::from_millis(100);
                while Instant::now() < reap_deadline {
                    match child.try_wait() {
                        Ok(Some(_)) | Err(_) => break,
                        Ok(None) => thread::yield_now(),
                    }
                }
                return Err(unavailable("compiler probe exceeded 2s".to_owned()));
            }
        }
    }
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
        "\"schemaVersion\":4",
        "\"schemaVersion\":4,\"surprise\":true",
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
    let mutated = GOLDEN.replacen("\"schemaVersion\":4", "\"schemaVersion\":2", 1);
    assert!(matches!(
        adapter().decode(mutated.as_bytes()),
        Err(OracleError::Staleness {
            found: 2,
            expected: 4
        })
    ));
}

#[test]
fn newer_schema_is_rejected_by_the_single_schema_owner() {
    let mutated = GOLDEN.replacen("\"schemaVersion\":4", "\"schemaVersion\":5", 1);
    assert!(matches!(
        adapter().decode(mutated.as_bytes()),
        Err(OracleError::Staleness {
            found: 5,
            expected: 4
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
        // SAFETY: the mutex above remains held until this environment cleanup completes.
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
        // SAFETY: this test still exclusively holds the shared environment mutex.
        unsafe { std::env::remove_var("NUDOX_GO_ORACLE_BIN") };
        assert!(matches!(
            error,
            OracleError::ToolingUnavailable {
                tool: "NUDOX_GO_ORACLE_BIN or go run",
                ..
            }
        ));
    }

    #[test]
    fn unset_compiler_is_a_typed_unavailable_terminal() {
        let _guard = super::ENVIRONMENT
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let saved = std::env::var_os("COMPILER_GO_COMPILER");
        // SAFETY: the process-global test environment is serialized by the mutex.
        unsafe { std::env::remove_var("COMPILER_GO_COMPILER") };
        let error = super::explicit_go_toolchain()
            .expect_err("an unset compiler must be typed unavailable");
        // SAFETY: this test still exclusively holds the shared environment mutex.
        if let Some(value) = saved {
            unsafe { std::env::set_var("COMPILER_GO_COMPILER", value) };
        }
        assert!(matches!(
            error,
            OracleError::ToolingUnavailable {
                tool: "COMPILER_GO_COMPILER",
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
    let _compiler = explicit_go_toolchain()?;
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
    assert_eq!(
        demo.decls.iter().any(|decl| decl.name == "LinuxOnly"),
        !active_darwin
    );
    let excluded_file = if active_darwin {
        "linux.go"
    } else {
        "darwin.go"
    };
    assert!(
        demo.build_constraints
            .iter()
            .any(|constraint| constraint.file.ends_with(excluded_file))
    );
    Ok(())
}

/// The v4 widened Uses walk, over a fixture exercising every class: one
/// type use, one method call, one method value, one field read, one field
/// write, one import use, one foreign call, and one satisfaction edge —
/// one row each, with the closed kind, the NAME-TOKEN extent, and a
/// resolvable target identity. Requires an explicit Go toolchain.
#[test]
fn widened_references_record_every_named_object_use() -> Result<(), OracleError> {
    use backend_frontend_go::legacy::oracle::{DeclKind, Reference};
    let _guard = ENVIRONMENT
        .get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap();
    let _compiler = explicit_go_toolchain()?;
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/refs");
    let source =
        fs::read(fixture.join("refs.go")).map_err(|error| OracleError::ToolingUnavailable {
            tool: "refs fixture",
            source: error,
        })?;
    let output = adapter().run(&fixture)?;
    let package = output
        .packages
        .iter()
        .find(|package| package.import_path == "example.com/refs")
        .ok_or_else(|| OracleError::Decode {
            message: "refs package missing".to_owned(),
            transcript: String::new(),
        })?;
    let extent = |row: &Reference| -> String {
        source
            .get(row.start..row.end)
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
            .unwrap_or_default()
    };
    let find = |predicate: &dyn Fn(&Reference) -> bool| -> Option<&Reference> {
        package.references.iter().find(|row| predicate(row))
    };
    fn expect_row<'a>(
        found: Option<&'a Reference>,
        what: &'static str,
    ) -> Result<&'a Reference, OracleError> {
        found.ok_or_else(|| OracleError::Decode {
            message: format!("{what} row missing"),
            transcript: String::new(),
        })
    }

    // One type use: `Greeter` inside `Use`'s variable declaration.
    let type_use = expect_row(
        find(&|row| row.owner == "Use" && row.target == "Greeter" && row.kind == "typeref"),
        "Greeter type use",
    )?;
    assert_eq!(type_use.class, "type");
    assert_eq!(type_use.target_pkg, "");
    assert_eq!(extent(&type_use), "Greeter");

    // One method call: `l.SetName("go")`.
    let method_call = expect_row(
        find(&|row| row.owner == "Use" && row.target == "SetName" && row.kind.is_empty()),
        "SetName method call",
    )?;
    assert_eq!(method_call.class, "method");
    assert_eq!(method_call.recv, "Lang");
    assert_eq!(extent(&method_call), "SetName");

    // One method value: `fn := l.Greet` reads the method without calling it.
    let method_value = expect_row(
        find(&|row| row.owner == "Use" && row.target == "Greet" && row.kind == "read"),
        "Greet method value",
    )?;
    assert_eq!(method_value.class, "method");
    assert_eq!(method_value.recv, "Lang");
    assert_eq!(extent(&method_value), "Greet");

    // One unexported field read and one exported field write: both carry
    // the read kind (the Uses table records no lvalue distinction), the
    // field class, and the receiver spelling.
    let field_read = expect_row(
        find(&|row| row.owner == "Use" && row.target == "n" && row.class == "field"),
        "unexported field read",
    )?;
    assert_eq!(field_read.kind, "read");
    assert_eq!(field_read.recv, "Lang");
    assert_eq!(extent(&field_read), "n");
    let field_write = expect_row(
        find(&|row| row.owner == "SetName" && row.target == "Name" && row.class == "field"),
        "field write",
    )?;
    assert_eq!(field_write.kind, "read");
    assert_eq!(field_write.recv, "Lang");
    assert_eq!(field_write.owner_recv, "Lang");
    assert_eq!(extent(&field_write), "Name");

    // One import use and one foreign call, both keyed to the fmt package.
    let import_use = expect_row(
        find(&|row| row.owner == "Use" && row.kind == "import"),
        "fmt import use",
    )?;
    assert_eq!(import_use.class, "pkg");
    assert_eq!(import_use.target, "fmt");
    assert_eq!(import_use.target_pkg, "fmt");
    assert_eq!(extent(&import_use), "fmt");
    let foreign_call = expect_row(
        find(&|row| row.owner == "Use" && row.target == "Println" && row.kind.is_empty()),
        "Println foreign call",
    )?;
    // The v3 call-row spelling: an empty class and an empty kind.
    assert!(foreign_call.class.is_empty());
    assert_eq!(foreign_call.target_pkg, "fmt");
    assert_eq!(extent(&foreign_call), "Println");

    // One satisfaction edge: Lang satisfies Greeter structurally, and the
    // declaration carries its exact NAME-TOKEN extent.
    let lang = package
        .decls
        .iter()
        .find(|decl| decl.name == "Lang" && decl.kind == DeclKind::Type)
        .ok_or_else(|| OracleError::Decode {
            message: "Lang declaration missing".to_owned(),
            transcript: String::new(),
        })?;
    let implemented = lang
        .implements
        .iter()
        .find(|implemented| implemented.name == "Greeter")
        .ok_or_else(|| OracleError::Decode {
            message: "Lang→Greeter satisfaction edge missing".to_owned(),
            transcript: String::new(),
        })?;
    assert_eq!(implemented.pkg, "example.com/refs");
    let name_span = lang
        .name_span
        .as_ref()
        .map(|span| (span.start as usize, span.end as usize))
        .ok_or_else(|| OracleError::Decode {
            message: "Lang NAME-TOKEN extent missing".to_owned(),
            transcript: String::new(),
        })?;
    assert_eq!(&source[name_span.0..name_span.1], b"Lang");

    // The binary image carries the same facts in version-6 rows: the bound
    // flag marks the digest-bound source's declarations, the NAME-TOKEN
    // extent survives, and every reference row keeps its closed kind,
    // class, and receiver spelling.
    let bytes = adapter().authority_image(&fixture.join("refs.go"), &fixture)?;
    let image = GoImage::open(&bytes).map_err(image_fault)?;
    let mut typed_rows = 0_usize;
    for index in 0..image.reference_count() {
        let row = image.reference(index).map_err(image_fault)?;
        if row.use_kind != backend_frontend_go::legacy::ReferenceUseKind::Call
            || row.target_class != backend_frontend_go::legacy::ReferenceTargetClass::Func
        {
            typed_rows += 1;
        }
        if row.target == b"Greet"
            && row.use_kind == backend_frontend_go::legacy::ReferenceUseKind::Read
        {
            assert_eq!(
                row.target_class,
                backend_frontend_go::legacy::ReferenceTargetClass::Method,
                "the method value keeps its method class in the image"
            );
            assert_eq!(row.recv_type, b"Lang");
        }
        if row.target == b"Println" {
            assert_eq!(
                row.use_kind,
                backend_frontend_go::legacy::ReferenceUseKind::Call
            );
            assert_eq!(
                row.target_class,
                backend_frontend_go::legacy::ReferenceTargetClass::Func
            );
            assert_eq!(row.recv_type, b"");
        }
    }
    assert!(
        typed_rows >= 8,
        "the image must carry the widened typed rows, found {typed_rows}"
    );
    let lang_index = (0..image.declaration_count())
        .find(|&index| image.declaration(index).map_err(image_fault).unwrap().name == b"Lang")
        .expect("Lang declaration row");
    let lang_row = image.declaration(lang_index).map_err(image_fault)?;
    assert!(lang_row.bound, "Lang declares in the bound source");
    let lang_name = lang_row.name_span.ok_or_else(|| OracleError::Decode {
        message: "Lang image name extent missing".to_owned(),
        transcript: String::new(),
    })?;
    assert_eq!(&source[lang_name.0 as usize..lang_name.1 as usize], b"Lang");
    Ok(())
}

/// One decoded (name, package) fact pair shared by the method-set and
/// satisfaction differentials.
type NamePackage = (Vec<u8>, Vec<u8>);

/// One decoded transcript method-set spelling.
type JsonMethodSet = (String, String);

/// Transcript signature facts: parameter/result names and method-set
/// spellings, in encounter order.
type SignatureFacts = (Vec<String>, Vec<JsonMethodSet>);

/// Every func-type parameter/result name and every interface method-set
/// entry in the decoded transcript, in encounter order: the exact fact set
/// the v5 signature-parameter and method-set planes must carry.
fn json_signature_facts(output: &backend_frontend_go::legacy::oracle::Output) -> SignatureFacts {
    let mut names = Vec::new();
    let mut method_set = Vec::new();
    fn walk(
        t: &backend_frontend_go::legacy::oracle::Type,
        names: &mut Vec<String>,
        method_set: &mut Vec<JsonMethodSet>,
    ) {
        use backend_frontend_go::legacy::oracle::TypeKind;
        if t.kind == TypeKind::Func {
            for parameter in t.params.iter() {
                names.push(parameter.name.clone());
            }
            for result in t.results.iter() {
                names.push(result.name.clone());
            }
        }
        if t.kind == TypeKind::Interface {
            for method in t.all_methods.iter() {
                method_set.push((method.name.clone(), method.pkg.clone()));
            }
        }
        for argument in t.type_args.iter() {
            walk(argument, names, method_set);
        }
        if let Some(elem) = t.elem.as_deref() {
            walk(elem, names, method_set);
        }
        if let Some(key) = t.key.as_deref() {
            walk(key, names, method_set);
        }
        if let Some(value) = t.value.as_deref() {
            walk(value, names, method_set);
        }
        for parameter in t.params.iter() {
            if let Some(parameter_type) = parameter.r#type.as_ref() {
                walk(parameter_type, names, method_set);
            }
        }
        for result in t.results.iter() {
            if let Some(result_type) = result.r#type.as_ref() {
                walk(result_type, names, method_set);
            }
        }
        for field in t.fields.iter() {
            if let Some(field_type) = field.r#type.as_ref() {
                walk(field_type, names, method_set);
            }
        }
        for method in t.explicit_methods.iter() {
            if let Some(signature) = method.signature.as_ref() {
                walk(signature, names, method_set);
            }
        }
        for method in t.all_methods.iter() {
            if let Some(signature) = method.signature.as_ref() {
                walk(signature, names, method_set);
            }
        }
        for embedded in t.embeddeds.iter() {
            walk(embedded, names, method_set);
        }
        for term in t.terms.iter() {
            if let Some(term_type) = term.r#type.as_ref() {
                walk(term_type, names, method_set);
            }
        }
        for component in t.types.iter() {
            walk(component, names, method_set);
        }
    }
    for package in output.packages.iter() {
        for declaration in package.decls.iter() {
            for root in [
                declaration.underlying.as_ref(),
                declaration.target.as_ref(),
                declaration.signature.as_ref(),
                declaration.r#type.as_ref(),
            ]
            .into_iter()
            .flatten()
            {
                walk(root, &mut names, &mut method_set);
            }
            for constraint in declaration
                .type_params
                .iter()
                .filter_map(|p| p.constraint.as_ref())
            {
                walk(constraint, &mut names, &mut method_set);
            }
            for method in declaration
                .methods
                .iter()
                .chain(declaration.promoted_methods.iter())
            {
                if let Some(signature) = method.signature.as_ref() {
                    walk(signature, &mut names, &mut method_set);
                }
            }
            for implemented in declaration.implements.iter() {
                walk(implemented, &mut names, &mut method_set);
            }
        }
    }
    (names, method_set)
}

/// The binary authority image round-trips the decoded oracle Output:
/// module metadata, package rows with files and declaration runs, every
/// constant value, const group, and iota flag, the interface method sets,
/// the satisfaction edges, and the package doc comment. Skipped when no Go
/// toolchain is available.
#[test]
fn authority_image_round_trips_the_full_output() -> Result<(), OracleError> {
    use backend_frontend_go::legacy::oracle::TypeKind;
    use backend_frontend_go::legacy::{DeclarationKind, MethodSetRow, SignatureParameterRow};
    let _guard = ENVIRONMENT
        .get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap();
    let _compiler = explicit_go_toolchain()?;
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/module");
    let source = fixture.join("demo.go");
    // Both decodes come from the same oracle binary over the same module:
    // the JSON transcript is the fact source, the image its binary twin.
    let output = adapter().run(&fixture)?;
    let bytes = adapter().authority_image(&source, &fixture)?;
    let image = GoImage::open(&bytes).map_err(|cause| OracleError::Decode {
        message: cause.to_string(),
        transcript: String::new(),
    })?;
    let expected: [u8; 32] =
        Sha256::digest(fs::read(&source).expect("fixture source bytes")).into();
    assert_eq!(image.source_digest(), expected);

    // Module metadata: the go.mod facts in one row.
    let module = image
        .module()
        .map_err(image_fault)?
        .expect("fixture module metadata row");
    let json_module = output.module.as_ref().expect("fixture module metadata");
    assert_eq!(module.path, json_module.path.as_bytes());
    assert_eq!(module.directory, json_module.dir.as_bytes());
    assert_eq!(module.go_version, json_module.go_version.as_bytes());
    assert_eq!(module.version, json_module.version.as_bytes());

    // Package rows: identity and files. The image's declarations follow the
    // transcript's package order, so one shared cursor walks them.
    assert_eq!(image.package_count(), output.packages.len());
    let mut declaration_cursor = 0_usize;
    for (index, json_package) in output.packages.iter().enumerate() {
        let row = image.package(index).map_err(image_fault)?;
        assert_eq!(row.import_path, json_package.import_path.as_bytes());
        assert_eq!(row.name, json_package.name.as_bytes());
        let files: Vec<&[u8]> = backend_frontend_go::legacy::split_nul(row.files, row.file_count)
            .unwrap_or_else(|| panic!("package {index} files must split"))
            .to_vec();
        let expected_files: Vec<&[u8]> = json_package
            .files
            .iter()
            .map(|file| file.as_bytes())
            .collect();
        assert_eq!(files, expected_files, "package {index} files");
        // Every declaration fact: kind, constant value, group, iota.
        for json_decl in json_package.decls.iter() {
            let declaration = image.declaration(declaration_cursor).map_err(image_fault)?;
            declaration_cursor += 1;
            assert_eq!(declaration.name, json_decl.name.as_bytes());
            assert_eq!(
                declaration.package,
                json_package.import_path.as_bytes(),
                "declaration {} package binding",
                json_decl.name
            );
            let expected_kind = match json_decl.kind {
                backend_frontend_go::legacy::oracle::DeclKind::Type => DeclarationKind::Type,
                backend_frontend_go::legacy::oracle::DeclKind::Alias => DeclarationKind::Alias,
                backend_frontend_go::legacy::oracle::DeclKind::Func => DeclarationKind::Function,
                backend_frontend_go::legacy::oracle::DeclKind::Const => DeclarationKind::Constant,
                backend_frontend_go::legacy::oracle::DeclKind::Var => DeclarationKind::Static,
            };
            assert_eq!(declaration.kind, expected_kind);
            assert_eq!(declaration.exported, json_decl.exported);
            assert_eq!(declaration.value, json_decl.value.as_bytes());
            assert_eq!(declaration.const_group, json_decl.const_group);
            assert_eq!(declaration.iota, json_decl.group_has_iota);
        }
    }
    assert_eq!(
        image.declaration_count(),
        declaration_cursor,
        "the declaration plane must hold exactly the transcript's declarations"
    );

    // A method call is owned by its receiver declaration, while its resolved
    // span remains attached to the method row for containment.
    let inner_index = (0..image.declaration_count())
        .find(|&index| image.declaration(index).unwrap().name == b"Inner")
        .expect("Inner declaration") as u32;
    let references = image
        .references()
        .collect::<Result<Vec<_>, _>>()
        .map_err(image_fault)?;
    for row in &references {
        let owner = image.declaration(row.owner as usize).map_err(image_fault)?;
        assert_ne!(
            owner.name, b"init",
            "implicit init must not own a reference row"
        );
    }
    let method_reference = references
        .into_iter()
        .find(|row| row.target == b"helper" && row.receiver == b"Inner")
        .expect("Inner method reference to helper");
    assert_eq!(method_reference.owner, inner_index);
    assert!(!method_reference.owner_is_declaration);
    let method = image
        .method(method_reference.owner_row as usize)
        .map_err(image_fault)?;
    assert_eq!(method.owner, inner_index);
    assert_eq!(method.name, b"CallsHelper");

    // Signature-parameter plane: one row per func parameter/result with the
    // exact source names (empty for unnamed).
    let (json_names, json_method_sets) = json_signature_facts(&output);
    assert_eq!(
        image.signature_parameter_count(),
        json_names.len(),
        "signature-parameter plane must hold one row per func parameter/result"
    );
    let image_names: Vec<&[u8]> = image
        .signature_parameters()
        .collect::<Result<Vec<_>, _>>()
        .map_err(image_fault)?
        .iter()
        .map(|row: &SignatureParameterRow| row.name)
        .collect();
    assert_eq!(
        image_names,
        json_names.iter().map(String::as_bytes).collect::<Vec<_>>()
    );

    // Method-set plane: the complete post-embedding method sets.
    let image_sets: Vec<NamePackage> = image
        .method_sets()
        .collect::<Result<Vec<_>, _>>()
        .map_err(image_fault)?
        .iter()
        .map(|row: &MethodSetRow| (row.name.to_vec(), row.package.to_vec()))
        .collect();
    let expected_sets: Vec<NamePackage> = json_method_sets
        .iter()
        .map(|(name, pkg)| (name.as_bytes().to_vec(), pkg.as_bytes().to_vec()))
        .collect();
    assert_eq!(image_sets, expected_sets);
    // Reader (interface{ Read() }) carries its own explicit method.
    assert!(image_sets.contains(&(b"Read".to_vec(), b"example.com/demo".to_vec())));

    // Satisfaction edges mirror the decoded Implements facts exactly.
    let mut json_edges: Vec<NamePackage> = Vec::new();
    for package in output.packages.iter() {
        for declaration in package.decls.iter() {
            for implemented in declaration.implements.iter() {
                if implemented.kind == TypeKind::Named || implemented.kind == TypeKind::Alias {
                    json_edges.push((
                        implemented.name.as_bytes().to_vec(),
                        implemented.pkg.as_bytes().to_vec(),
                    ));
                }
            }
        }
    }
    json_edges.sort();
    let mut image_edges: Vec<NamePackage> = image
        .satisfactions()
        .collect::<Result<Vec<_>, _>>()
        .map_err(image_fault)?
        .iter()
        .map(|row| (row.target.to_vec(), row.target_package.to_vec()))
        .collect();
    image_edges.sort();
    assert_eq!(image_edges, json_edges);
    assert_eq!(
        image_edges
            .iter()
            .filter(|(target, _)| target == b"Reader")
            .count(),
        2,
        "Inner and Outer satisfy demo.Reader"
    );
    assert_eq!(
        image_edges
            .iter()
            .filter(|(target, _)| target == b"Service")
            .count(),
        2,
        "Inner and Outer satisfy sub.Service"
    );

    // The package doc comment rides a Package-owned documentation row.
    let mut package_docs = 0_usize;
    for index in 0..image.doc_count() {
        let row = image.doc(index).map_err(image_fault)?;
        if row.owner_kind == DocOwner::Package {
            package_docs += 1;
            assert!(row.text.starts_with(b"Package demo "));
        }
    }
    assert_eq!(package_docs, 1, "doc.go's package comment missing");
    Ok(())
}

fn image_fault(cause: backend_frontend_go::legacy::ImageError) -> OracleError {
    OracleError::Decode {
        message: cause.to_string(),
        transcript: String::new(),
    }
}

/// M1 falsifier on a param-bearing module: named and unnamed parameters, a
/// variadic final parameter, generics, and an interface embedding another
/// package's interface — whose post-embedding method set is not locally
/// re-derivable because embedded foreign-package references carry no body.
/// Skipped when no Go toolchain is available.
#[test]
fn authority_image_carries_parameter_names_and_embedded_method_sets() -> Result<(), OracleError> {
    let _guard = ENVIRONMENT
        .get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap();
    let _compiler = explicit_go_toolchain()?;
    let root = std::env::temp_dir().join(format!("nudox-go-image-v5-{}", std::process::id()));
    fs::create_dir_all(root.join("api")).expect("temp module directory");
    fs::write(
        root.join("go.mod"),
        "module example.com/params\n\ngo 1.23\n",
    )
    .expect("temp go.mod");
    fs::write(
        root.join("api/api.go"),
        "package api\n\ntype Base interface{ Log() string }\n",
    )
    .expect("temp api source");
    fs::write(
        root.join("main.go"),
        r#"package main

import "example.com/params/api"

type Combo interface {
	api.Base
	Extra(i int) error
}

func Greet(name string, greeting string) (string, error) {
	return greeting + " " + name, nil
}

func Sum(vals ...int) int {
	total := 0
	for _, v := range vals {
		total += v
	}
	return total
}

func Unnamed(int, string) {}

func Pick[K comparable, V any](m map[K]V, k K) (V, bool) {
	v, ok := m[k]
	return v, ok
}

func main() {
	s, _ := Greet("x", "hi")
	_ = s
	_ = Sum(1)
	_ = Unnamed
	_, _ = Pick(map[string]int{"a": 1}, "a")
	var _ Combo
}
"#,
    )
    .expect("temp main source");

    let output = adapter().run(&root);
    let bytes = adapter().authority_image(&root.join("main.go"), &root);
    fs::remove_dir_all(&root).expect("temp module cleanup");
    let output = output?;
    let bytes = bytes?;
    let image = GoImage::open(&bytes).map_err(image_fault)?;

    // Module row binds the temp module's path.
    assert_eq!(
        image
            .module()
            .map_err(image_fault)?
            .expect("temp module metadata row")
            .path,
        b"example.com/params".as_slice()
    );
    // Two package rows in strict import-path order.
    assert_eq!(image.package_count(), 2);
    assert_eq!(
        image.package(0).map_err(image_fault)?.import_path,
        b"example.com/params".as_slice()
    );
    assert_eq!(
        image.package(1).map_err(image_fault)?.import_path,
        b"example.com/params/api".as_slice()
    );

    // Greet's func row carries [name, greeting] then two unnamed results;
    // Sum's final variadic parameter is named vals; Unnamed's parameters
    // are unnamed.
    let func_row = |image: &GoImage, name: &[u8]| -> Option<u32> {
        for index in 0..image.declaration_count() {
            let declaration = image.declaration(index).ok()?;
            if declaration.name == name
                && declaration.kind == backend_frontend_go::legacy::DeclarationKind::Function
            {
                return declaration.type_root;
            }
        }
        None
    };
    // The signature-parameter run of one func row starts at the cumulative
    // child count of the func rows that precede it in type-row order, per
    // the reader's tiling law.
    fn row_params<'image>(
        image: &GoImage<'image>,
        root: u32,
    ) -> Option<Vec<backend_frontend_go::legacy::SignatureParameterRow<'image>>> {
        let row = image.type_row(root as usize).ok()?;
        let mut start = 0_usize;
        for index in 0..root as usize {
            let earlier = image.type_row(index).ok()?;
            if earlier.kind == backend_frontend_go::legacy::TypeRowKind::Func {
                start += earlier.children.1 as usize;
            }
        }
        let count = row.children.1 as usize;
        (0..count)
            .map(|ordinal| image.signature_parameter(start + ordinal).ok())
            .collect()
    }
    let greet = func_row(&image, b"Greet").expect("Greet declaration");
    let greet_params = row_params(&image, greet).expect("Greet signature parameters");
    assert_eq!(
        greet_params
            .iter()
            .map(|row| row.name.to_vec())
            .collect::<Vec<_>>(),
        vec![
            b"name".to_vec(),
            b"greeting".to_vec(),
            Vec::new(),
            Vec::new()
        ]
    );
    // The named parameters carry their exact source positions: the demo
    // file's name and a byte offset inside it.
    for row in &greet_params[..2] {
        assert!(row.file.ends_with(b"main.go"), "param file {:?}", row.file);
        assert!(row.offset != backend_frontend_go::legacy::NONE && row.offset > 0);
    }
    let sum = func_row(&image, b"Sum").expect("Sum declaration");
    let sum_params = row_params(&image, sum).expect("Sum signature parameters");
    assert_eq!(
        sum_params
            .iter()
            .map(|row| row.name.to_vec())
            .collect::<Vec<_>>(),
        vec![b"vals".to_vec(), Vec::new()]
    );
    let unnamed = func_row(&image, b"Unnamed").expect("Unnamed declaration");
    let unnamed_params = row_params(&image, unnamed).expect("Unnamed signature parameters");
    assert_eq!(
        unnamed_params
            .iter()
            .map(|row| row.name.to_vec())
            .collect::<Vec<_>>(),
        vec![Vec::<u8>::new(), Vec::new()]
    );

    // Combo's method set is the complete post-embedding set: the inherited
    // foreign-package Log beside the locally declared Extra, in name order.
    // api.Base's own row (one inherited Log of its own) precedes it, since
    // the main package declares before the api package.
    let combo_sets: Vec<NamePackage> = image
        .method_sets()
        .collect::<Result<Vec<_>, _>>()
        .map_err(image_fault)?
        .iter()
        .map(|row| (row.name.to_vec(), row.package.to_vec()))
        .collect();
    assert_eq!(
        combo_sets,
        [
            (b"Extra".to_vec(), b"example.com/params".to_vec()),
            (b"Log".to_vec(), b"example.com/params/api".to_vec()),
            (b"Log".to_vec(), b"example.com/params/api".to_vec()),
        ]
    );
    // The api.Base interface itself carries exactly its own method.
    let json_api = output
        .packages
        .iter()
        .find(|package| package.import_path == "example.com/params/api")
        .expect("api package in transcript");
    assert_eq!(json_api.decls.len(), 1);
    Ok(())
}

/// Minimal synthetic v5 image for the hostile mutation battery: two
/// packages, two declarations, one func type row with a named parameter,
/// and one interface type row with a two-method set.
mod mutation_battery {
    use backend_frontend_go::legacy::{GoImage, HeaderError, ImageError, TypeRowKind};
    use sha2::{Digest, Sha256};

    const DOMAIN: &[u8] = b"nudox.go.authority.image.sha256.v5\x00";
    const HEADER: usize = 136;
    // Frozen body order: declarations, types, methods, type parameters,
    // members, docs, references, constraints, satisfactions, module,
    // packages, signature parameters, method sets, children, atoms. Every
    // plane between types and module is empty, and this fixture carries no
    // module row (header count 0), so packages open where satisfactions end.
    const DECLS_AT: usize = HEADER;
    const TYPES_AT: usize = DECLS_AT + 2 * 56;
    const METHODS_AT: usize = TYPES_AT + 2 * 52;
    const MODULE_AT: usize = METHODS_AT;
    const PACKAGES_AT: usize = MODULE_AT;
    const SIGPARAMS_AT: usize = PACKAGES_AT + 2 * 28;
    const METHOD_SETS_AT: usize = SIGPARAMS_AT + 28;
    const CHILDREN_AT: usize = METHOD_SETS_AT + 2 * 24;
    const ATOMS_AT: usize = CHILDREN_AT + 8;
    const BODY: usize = ATOMS_AT + ATOMS.len() - HEADER;

    /// Atoms: 0 "alpha" | 5 "beta" | 9 "A" | 10 "B" | 11 "p" | 12 "M1" | 14 "M2".
    const ATOMS: &[u8] = b"alphabetaABpM1M2";

    const NONE: u32 = u32::MAX;

    fn cells(values: &[u32]) -> Vec<u8> {
        values
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect()
    }

    /// Declaration row: kind and exported flags share the first word.
    fn decl_row(kind: u32, exported: bool, name: [u32; 2], pkg: [u32; 2], root: u32) -> Vec<u8> {
        let mut row = cells(&[
            kind | u32::from(exported) << 8,
            name[0],
            name[1],
            pkg[0],
            pkg[1],
            root,
            NONE,
            NONE,
            0,
            0,
            0,
            0,
            0,
            0,
        ]);
        row[48..56].copy_from_slice(&0_i64.to_le_bytes());
        assert_eq!(row.len(), 56);
        row
    }

    /// Type row: kind in the first byte, no name or package, length zero.
    fn type_row(kind: u32, child_start: u32, child_count: u32) -> Vec<u8> {
        let row = cells(&[kind, 0, 0, 0, 0, 0, 0, child_start, child_count, 0, 0, 0, 0]);
        assert_eq!(row.len(), 52);
        row
    }

    fn package_row(import_path: [u32; 2], name: [u32; 2]) -> Vec<u8> {
        let row = cells(&[import_path[0], import_path[1], name[0], name[1], 0, 0, 0]);
        assert_eq!(row.len(), 28);
        row
    }

    /// Signature-parameter row: func-row owner, ordinal, name cell, absent
    /// position (empty file cell, NONE offset).
    fn signature_parameter_row(name: [u32; 2]) -> Vec<u8> {
        let row = cells(&[0, 0, name[0], name[1], 0, 0, NONE]);
        assert_eq!(row.len(), 28);
        row
    }

    fn method_set_row(name: [u32; 2]) -> Vec<u8> {
        let row = cells(&[1, name[0], name[1], NONE, 0, 0]);
        assert_eq!(row.len(), 24);
        row
    }

    /// Builds the valid fixture image.
    fn build() -> Vec<u8> {
        let mut image = vec![0_u8; HEADER + BODY];
        image[..4].copy_from_slice(b"NGAI");
        image[4..6].copy_from_slice(&5_u16.to_le_bytes());
        image[6..8].copy_from_slice(&(HEADER as u16).to_le_bytes());
        image[8..12].copy_from_slice(&2_u32.to_le_bytes()); // declarations
        image[12..16].copy_from_slice(&(ATOMS.len() as u32).to_le_bytes());
        image[16..20].copy_from_slice(&(BODY as u32).to_le_bytes());
        image[84..88].copy_from_slice(&2_u32.to_le_bytes()); // types
        image[116..120].copy_from_slice(&0_u32.to_le_bytes()); // no module
        image[120..124].copy_from_slice(&2_u32.to_le_bytes()); // packages
        image[124..128].copy_from_slice(&1_u32.to_le_bytes()); // signature parameters
        image[128..132].copy_from_slice(&2_u32.to_le_bytes()); // method sets

        // Bytes 132..136 stay zero: the reserved envelope tail.
        image[PACKAGES_AT..PACKAGES_AT + 28].copy_from_slice(&package_row([0, 5], [9, 1]));
        image[PACKAGES_AT + 28..PACKAGES_AT + 56].copy_from_slice(&package_row([5, 4], [10, 1]));

        let declarations = [
            decl_row(3, true, [9, 1], [0, 5], 0),
            decl_row(4, true, [10, 1], [5, 4], NONE),
        ];
        image[DECLS_AT..DECLS_AT + 112].copy_from_slice(declarations.concat().as_slice());

        // Type row 0: func with one child (its parameter); type row 1:
        // interface owning the two-row method set. Its empty child run
        // starts where the func row's run ends, per the tiling law.
        let types = [type_row(9, 0, 1), type_row(11, 1, 0)];
        image[TYPES_AT..TYPES_AT + 104].copy_from_slice(types.concat().as_slice());

        // Signature parameter: owner func row 0, ordinal 0, name "p",
        // position fully absent.
        image[SIGPARAMS_AT..SIGPARAMS_AT + 28].copy_from_slice(&signature_parameter_row([11, 1]));

        // Method set: owner interface row 1, names M1 then M2.
        image[METHOD_SETS_AT..METHOD_SETS_AT + 24].copy_from_slice(&method_set_row([12, 2]));
        image[METHOD_SETS_AT + 24..METHOD_SETS_AT + 48].copy_from_slice(&method_set_row([14, 2]));

        // The func row's one child targets the interface row.
        image[CHILDREN_AT..CHILDREN_AT + 8].copy_from_slice(&cells(&[1, 0]));

        image[ATOMS_AT..].copy_from_slice(ATOMS);
        reseal(&mut image);
        image
    }

    /// Re-seals the checksum after a mutation so the fault hits the plane
    /// law instead of the digest.
    fn reseal(image: &mut [u8]) {
        let mut digest = Sha256::new();
        digest.update(DOMAIN);
        digest.update(&image[..52]);
        digest.update(&image[84..HEADER]);
        digest.update(&image[HEADER..]);
        image[52..84].copy_from_slice(digest.finalize().as_slice());
    }

    fn open(image: &[u8]) -> ImageError {
        GoImage::open(image).expect_err("mutated image must be rejected")
    }

    #[test]
    fn fixture_is_valid() {
        GoImage::open(&build()).expect("fixture must validate before mutation");
    }

    #[test]
    fn version_mutation_is_rejected_with_the_found_version() {
        let mut image = build();
        image[4..6].copy_from_slice(&4_u16.to_le_bytes());
        assert_eq!(
            open(&image),
            ImageError::Header(HeaderError::Version { found: 4 })
        );
    }

    #[test]
    fn module_count_mutation_is_rejected_exactly() {
        let mut image = build();
        image[116..120].copy_from_slice(&2_u32.to_le_bytes());
        reseal(&mut image);
        assert_eq!(
            open(&image),
            ImageError::Header(HeaderError::ModuleCount { found: 2 })
        );
    }

    #[test]
    fn package_count_mutation_breaks_the_body_geometry() {
        let mut image = build();
        image[120..124].copy_from_slice(&1_u32.to_le_bytes());
        reseal(&mut image);
        assert!(matches!(
            open(&image),
            ImageError::Header(HeaderError::BodyLength { .. })
        ));
    }

    #[test]
    fn reserved_tail_mutation_is_rejected() {
        let mut image = build();
        image[132] = 1;
        reseal(&mut image);
        assert_eq!(open(&image), ImageError::Header(HeaderError::Reserved));
    }

    #[test]
    fn package_file_count_mutation_is_rejected_exactly() {
        let mut image = build();
        let cell = PACKAGES_AT + 24;
        image[cell..cell + 4].copy_from_slice(&1_u32.to_le_bytes());
        reseal(&mut image);
        assert_eq!(
            open(&image),
            ImageError::PackageFiles {
                index: 0,
                count: 1,
                blob_bytes: 0,
            }
        );
    }

    #[test]
    fn package_order_mutation_is_rejected_exactly() {
        let mut image = build();
        // Package 1's import path becomes equal to package 0's, so only the
        // order law can fire.
        image[PACKAGES_AT + 28..PACKAGES_AT + 36].copy_from_slice(&cells(&[0, 5]));
        reseal(&mut image);
        assert_eq!(open(&image), ImageError::PackageSort { index: 1 });
    }

    #[test]
    fn foreign_package_declaration_is_rejected_exactly() {
        let mut image = build();
        // Declaration 1's package cell stops naming any package row.
        let cell = DECLS_AT + 56 + 12;
        image[cell..cell + 4].copy_from_slice(&9_u32.to_le_bytes());
        reseal(&mut image);
        assert_eq!(
            open(&image),
            ImageError::DeclarationPackage {
                index: 1,
                package_count: 2,
            }
        );
    }

    #[test]
    fn signature_parameter_owner_mutation_names_the_expected_row() {
        let mut image = build();
        image[SIGPARAMS_AT..SIGPARAMS_AT + 4].copy_from_slice(&1_u32.to_le_bytes());
        reseal(&mut image);
        assert_eq!(
            open(&image),
            ImageError::SignatureParameterOwnerRow {
                index: 0,
                owner: 1,
                expected: 0,
            }
        );
    }

    #[test]
    fn signature_parameter_ordinal_mutation_is_rejected_exactly() {
        let mut image = build();
        image[SIGPARAMS_AT + 4..SIGPARAMS_AT + 8].copy_from_slice(&1_u32.to_le_bytes());
        reseal(&mut image);
        assert_eq!(
            open(&image),
            ImageError::SignatureParameterOrdinal {
                index: 0,
                ordinal: 1,
                expected: 0,
            }
        );
    }

    #[test]
    fn half_present_position_is_rejected_exactly() {
        let mut image = build();
        // The fixture row's position is fully absent (empty file, NONE);
        // materializing only the offset breaks the presence law.
        image[SIGPARAMS_AT + 24..SIGPARAMS_AT + 28].copy_from_slice(&0_u32.to_le_bytes());
        reseal(&mut image);
        assert_eq!(
            open(&image),
            ImageError::SignatureParameterPosition { index: 0 }
        );
    }

    #[test]
    fn method_set_owner_kind_is_rejected_exactly() {
        let mut image = build();
        // Owner 0 is the func type row; only interface rows carry sets.
        image[METHOD_SETS_AT..METHOD_SETS_AT + 4].copy_from_slice(&0_u32.to_le_bytes());
        reseal(&mut image);
        assert_eq!(
            open(&image),
            ImageError::MethodSetOwnerKind {
                index: 0,
                kind: TypeRowKind::Func,
            }
        );
    }

    #[test]
    fn method_set_order_mutation_is_rejected_exactly() {
        let mut image = build();
        image[METHOD_SETS_AT..METHOD_SETS_AT + 48].rotate_left(24);
        reseal(&mut image);
        assert_eq!(open(&image), ImageError::MethodSetSort { index: 1 });
    }

    #[test]
    fn signature_parameter_atom_escape_is_rejected_exactly() {
        let mut image = build();
        image[SIGPARAMS_AT + 8..SIGPARAMS_AT + 12].copy_from_slice(&16_u32.to_le_bytes());
        reseal(&mut image);
        assert!(matches!(
            open(&image),
            ImageError::AtomRange {
                plane: "signature parameter",
                ..
            }
        ));
    }

    #[test]
    fn unresealed_body_mutation_fails_the_checksum() {
        let mut image = build();
        image[SIGPARAMS_AT + 8] ^= 0xff;
        assert_eq!(open(&image), ImageError::Digest);
    }
}

/// Reference rows are ordered by source file first, then by call start.  This
/// fixture keeps the owner checks real while making the cross-file reset
/// explicit: the second file's first call starts before the first file's last
/// call.
mod reference_order {
    use backend_frontend_go::legacy::{GoImage, ImageError};
    use sha2::{Digest, Sha256};

    const DOMAIN: &[u8] = b"nudox.go.authority.image.sha256.v5\0";
    const HEADER: usize = 136;
    const DECLS: usize = HEADER;
    const REFS: usize = DECLS + 2 * 56;
    const PACKAGES: usize = REFS + 3 * 48;
    const ATOMS: usize = PACKAGES + 28;
    const ATOM_BYTES: &[u8] = b"example.com/demo\0demo\0first.go\0second.go\0one\0two\0three\0";

    fn cell(value: u32) -> [u8; 4] {
        value.to_le_bytes()
    }

    fn atom(bytes: &[u8], needle: &[u8]) -> [u32; 2] {
        let start = bytes
            .windows(needle.len())
            .position(|window| window == needle)
            .unwrap();
        [start as u32, needle.len() as u32]
    }

    fn declaration(name: [u32; 2], package: [u32; 2], file: [u32; 2], span: (u32, u32)) -> Vec<u8> {
        let mut row = Vec::with_capacity(56);
        for value in [
            3 | (1 << 8),
            name[0],
            name[1],
            package[0],
            package[1],
            u32::MAX,
            span.0,
            span.1,
            file[0],
            file[1],
            0,
            0,
            0,
            0,
        ] {
            row.extend_from_slice(&cell(value));
        }
        row
    }

    fn reference(owner: usize, file: [u32; 2], start: u32, target: [u32; 2]) -> Vec<u8> {
        let mut row = Vec::with_capacity(48);
        for value in [
            owner as u32,
            target[0],
            target[1],
            0,
            0,
            start,
            start + 1,
            file[0],
            file[1],
            0,
            0,
            0,
        ] {
            row.extend_from_slice(&cell(value));
        }
        row
    }

    fn build(order: &[(usize, usize, u32)]) -> Vec<u8> {
        let first = atom(ATOM_BYTES, b"first.go");
        let second = atom(ATOM_BYTES, b"second.go");
        let target = atom(ATOM_BYTES, b"one");
        let package = atom(ATOM_BYTES, b"example.com/demo");
        let package_name = atom(ATOM_BYTES, b"demo");
        let body = 2 * 56 + 3 * 48 + 28 + ATOM_BYTES.len();
        let mut image = vec![0; HEADER + body];
        image[..4].copy_from_slice(b"NGAI");
        image[4..6].copy_from_slice(&5_u16.to_le_bytes());
        image[6..8].copy_from_slice(&(HEADER as u16).to_le_bytes());
        image[8..12].copy_from_slice(&2_u32.to_le_bytes());
        image[12..16].copy_from_slice(&(ATOM_BYTES.len() as u32).to_le_bytes());
        image[16..20].copy_from_slice(&(body as u32).to_le_bytes());
        image[84..88].copy_from_slice(&0_u32.to_le_bytes());
        image[88..92].copy_from_slice(&3_u32.to_le_bytes());
        image[120..124].copy_from_slice(&1_u32.to_le_bytes());

        let declarations = [
            declaration(atom(ATOM_BYTES, b"two"), package, first, (0, 100)),
            declaration(atom(ATOM_BYTES, b"three"), package, second, (0, 100)),
        ];
        image[DECLS..DECLS + 112].copy_from_slice(&declarations.concat());
        let rows = order
            .iter()
            .map(|&(owner, file, start)| {
                reference(owner, if file == 0 { first } else { second }, start, target)
            })
            .collect::<Vec<_>>()
            .concat();
        image[REFS..REFS + 144].copy_from_slice(&rows);
        let mut package_row = Vec::new();
        for value in [
            package[0],
            package[1],
            package_name[0],
            package_name[1],
            0,
            0,
            0,
        ] {
            package_row.extend_from_slice(&cell(value));
        }
        image[PACKAGES..PACKAGES + 28].copy_from_slice(&package_row);
        image[ATOMS..].copy_from_slice(ATOM_BYTES);
        reseal(&mut image);
        image
    }

    fn reseal(image: &mut [u8]) {
        let mut digest = Sha256::new();
        digest.update(DOMAIN);
        digest.update(&image[..52]);
        digest.update(&image[84..HEADER]);
        digest.update(&image[HEADER..]);
        image[52..84].copy_from_slice(digest.finalize().as_slice());
    }

    #[test]
    fn ascending_cross_file_order_opens_with_offset_reset() {
        let image = build(&[(0, 0, 10), (0, 0, 20), (1, 1, 5)]);
        GoImage::open(&image).expect("canonical cross-file references must open");
    }

    #[test]
    fn swapping_files_rejects_reference_index_two() {
        let image = build(&[(0, 0, 10), (1, 1, 5), (0, 0, 20)]);
        assert!(matches!(
            GoImage::open(&image),
            Err(ImageError::ReferenceSort { index: 2 })
        ));
    }

    #[test]
    fn swapping_starts_within_one_file_rejects_reference_index_one() {
        let image = build(&[(0, 0, 20), (0, 0, 10), (1, 1, 5)]);
        assert!(matches!(
            GoImage::open(&image),
            Err(ImageError::ReferenceSort { index: 1 })
        ));
    }
}
