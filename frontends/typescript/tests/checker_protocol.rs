//! Offline falsifiers for the closed TypeScript checker protocol.
//! Each test targets one schema, binding, or subprocess law.
//! Subprocess falsifiers use only temporary local scripts through the typed
//! program seam. The end-to-end authority test drives the real vendored
//! checker and reports a typed [`CheckerError`] when `node` or the
//! `typescript` module is unavailable; it never self-skips.

use backend_compile::TypeScriptSource;
use backend_frontend_typescript::{
    Checker, CheckerError, CheckerIndex, Declaration, LiteralBase, MappedModifier, Narrowing,
    Origin, Report, TemplatePart, TypeTree, source_digest,
};

const GOLDEN: &str = include_str!("transcripts/golden.json");
const GOLDEN_SOURCE: &[u8] = include_bytes!("fixtures/source.ts");
const CONDITIONAL: &str = include_str!("transcripts/conditional.json");
const MAPPED: &str = include_str!("transcripts/mapped.json");
const TEMPLATE_LITERAL: &str = include_str!("transcripts/template-literal.json");
const AS_CONST: &str = include_str!("transcripts/as-const.json");
const SIGNATURE_PARAMETERS: &str = include_str!("transcripts/signature-parameters.json");

static ENVIRONMENT: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();

fn adapter() -> Checker {
    Checker::default()
}

fn golden_source() -> &'static str {
    core::str::from_utf8(GOLDEN_SOURCE).expect("golden fixture source is UTF-8")
}

#[test]
fn golden_transcript_decodes_declarations_and_references() -> Result<(), CheckerError> {
    let report = adapter().decode(GOLDEN.as_bytes())?;
    assert_eq!(report.schema_version, 1);
    let source = golden_source();
    let index = CheckerIndex::bind(&report, source)?;
    let declarations: Vec<_> = index.declarations().collect();
    // `export const n: number = 1;` opens the fixture: the annotation is a
    // declared `number` and the checker computes the same type.
    let first = *declarations.first().ok_or(CheckerError::Decode {
        message: "golden declaration missing".to_owned(),
        transcript: String::new(),
    })?;
    assert_eq!(
        &source[first.name.start as usize..first.name.end as usize],
        "n"
    );
    assert_eq!(first.origin, Origin::Declared);
    assert_eq!(
        first.r#type,
        Some(&TypeTree::Primitive {
            name: "number".to_owned(),
        })
    );
    // The inferred const computes its literal type.
    let inferred = declarations
        .iter()
        .copied()
        .find(|declaration| {
            &source[declaration.name.start as usize..declaration.name.end as usize] == "inferred"
        })
        .ok_or(CheckerError::Decode {
            message: "inferred declaration missing".to_owned(),
            transcript: String::new(),
        })?;
    assert_eq!(inferred.origin, Origin::Computed);
    assert_eq!(
        inferred.r#type,
        Some(&TypeTree::Literal {
            base: LiteralBase::Number,
            text: "7".to_owned(),
        })
    );
    // The two overload call sites pick distinct overload members.
    let overload_calls: Vec<_> = index
        .references()
        .filter(|reference| reference.overload_index.is_some())
        .map(|reference| {
            (
                &source[reference.span.start as usize..reference.span.end as usize],
                reference.overload_index,
            )
        })
        .collect();
    assert!(overload_calls.contains(&("g", Some(0))));
    assert!(overload_calls.contains(&("g", Some(1))));
    // The recorded assignment narrowing binds its declaration name span,
    // its exact assignment site, and the literal type of the value.
    let narrowing = *index.narrowings().next().ok_or(CheckerError::Decode {
        message: "golden narrowing missing".to_owned(),
        transcript: String::new(),
    })?;
    assert_eq!(
        &source[narrowing.name.start as usize..narrowing.name.end as usize],
        "widened"
    );
    assert_eq!(
        &source[narrowing.site.start as usize..narrowing.site.end as usize],
        "widened = \"text\""
    );
    assert_eq!(
        narrowing.r#type,
        Some(&TypeTree::Literal {
            base: LiteralBase::String,
            text: "\"text\"".to_owned(),
        })
    );
    Ok(())
}

#[test]
fn feature_transcripts_decode_exact_type_trees() -> Result<(), CheckerError> {
    let conditional = adapter().decode(CONDITIONAL.as_bytes())?;
    assert!(matches!(
        conditional.declarations[0].r#type,
        Some(TypeTree::Conditional { ref check, ref extends, ref true_type, ref false_type })
            if matches!(check.as_ref(), TypeTree::TypeParameter { name } if name == "T")
                && matches!(extends.as_ref(), TypeTree::Primitive { name } if name == "string")
                && matches!(true_type.as_ref(), TypeTree::Literal { base: LiteralBase::String, text } if text == "\"s\"")
                && matches!(false_type.as_ref(), TypeTree::Literal { base: LiteralBase::String, text } if text == "\"n\"")
    ));

    let mapped = adapter().decode(MAPPED.as_bytes())?;
    assert!(matches!(
        mapped.declarations[0].r#type,
        Some(TypeTree::Mapped { modifier: MappedModifier::Readonly, ref keys, ref template })
            if matches!(keys.as_ref(), TypeTree::Union { members } if members.len() == 2)
                && matches!(template.as_ref(), TypeTree::TypeParameter { name } if name == "T")
    ));

    let template = adapter().decode(TEMPLATE_LITERAL.as_bytes())?;
    assert!(matches!(
        template.declarations[0].r#type,
        Some(TypeTree::TemplateLiteral { ref parts })
            if parts == &[
                TemplatePart::Text { text: "id-".to_owned() },
                TemplatePart::Placeholder { r#type: Box::new(TypeTree::Primitive { name: "number".to_owned() }) },
                TemplatePart::Text { text: String::new() },
            ]
    ));

    let as_const = adapter().decode(AS_CONST.as_bytes())?;
    assert!(matches!(
        as_const.declarations[0].r#type,
        Some(TypeTree::Object { ref members })
            if members.len() == 1 && members[0].readonly
    ));
    Ok(())
}

#[test]
fn signature_transcript_preserves_parameter_names_and_flags() -> Result<(), CheckerError> {
    let report = adapter().decode(SIGNATURE_PARAMETERS.as_bytes())?;
    let Some(TypeTree::Function { parameters, .. }) = report.declarations[0].r#type.as_ref() else {
        return Err(CheckerError::Decode {
            message: "signature missing".to_owned(),
            transcript: String::new(),
        });
    };
    assert_eq!(parameters[0].name.as_deref(), Some("value"));
    assert_eq!(parameters[1].name.as_deref(), Some("optional"));
    assert!(parameters[1].optional);
    assert_eq!(parameters[2].name.as_deref(), Some("rest"));
    assert!(parameters[2].rest);
    Ok(())
}

#[test]
fn golden_transcript_binds_this_type_and_foreign_base() -> Result<(), CheckerError> {
    let report = adapter().decode(GOLDEN.as_bytes())?;
    let source = golden_source();
    let index = CheckerIndex::bind(&report, source)?;
    // The `self(): this` method computes a `this` result.
    let this_result = index.declarations().any(|declaration| {
        matches!(
            declaration.r#type,
            Some(TypeTree::Function { result, .. }) if matches!(result.as_ref(), TypeTree::This)
        )
    });
    assert!(this_result, "golden must retain a this-type result");
    // The `Map` base resolves to the foreign `typescript` module.
    let foreign = index
        .declarations()
        .find_map(|declaration| match declaration.r#type {
            Some(TypeTree::Reference {
                module: Some(module),
                ..
            }) => Some(module.to_owned()),
            _ => None,
        })
        .ok_or(CheckerError::Decode {
            message: "foreign module reference missing".to_owned(),
            transcript: String::new(),
        })?;
    assert_eq!(foreign, "typescript");
    Ok(())
}

#[test]
fn unknown_field_is_a_decode_fault() {
    let mutated = GOLDEN.replacen(
        "\"schemaVersion\":1",
        "\"schemaVersion\":1,\"surprise\":true",
        1,
    );
    let error = adapter()
        .decode(mutated.as_bytes())
        .expect_err("surprise cell must fail");
    assert!(
        matches!(error, CheckerError::Decode { ref message, .. } if message.contains("surprise"))
    );
}

#[test]
fn stale_schema_names_both_versions() {
    let mutated = GOLDEN.replacen("\"schemaVersion\":1", "\"schemaVersion\":0", 1);
    assert!(matches!(
        adapter().decode(mutated.as_bytes()),
        Err(CheckerError::Staleness {
            found: 0,
            expected: 1
        })
    ));
}

#[test]
fn newer_schema_is_rejected_by_the_single_schema_owner() {
    let mutated = GOLDEN.replacen("\"schemaVersion\":1", "\"schemaVersion\":2", 1);
    assert!(matches!(
        adapter().decode(mutated.as_bytes()),
        Err(CheckerError::Staleness {
            found: 2,
            expected: 1
        })
    ));
}

#[test]
fn truncated_record_is_a_decode_fault() {
    let truncated = &GOLDEN[..GOLDEN.len().saturating_sub(3)];
    let error = adapter()
        .decode(truncated.as_bytes())
        .expect_err("truncated cell must fail");
    assert!(matches!(error, CheckerError::Decode { ref message, .. } if message.contains("EOF")));
}

#[test]
fn report_bound_to_different_source_is_typed_rejection() -> Result<(), CheckerError> {
    let report = adapter().decode(GOLDEN.as_bytes())?;
    let other = format!("// different bytes\n{}", golden_source());
    let error = CheckerIndex::bind(&report, &other).expect_err("foreign source must fail");
    let CheckerError::SourceBinding { expected, observed } = error else {
        return Err(CheckerError::Decode {
            message: "wrong fault".to_owned(),
            transcript: String::new(),
        });
    };
    assert_eq!(expected, source_digest(other.as_bytes()));
    assert_eq!(observed, source_digest(GOLDEN_SOURCE));
    Ok(())
}

#[test]
fn malformed_digest_is_a_decode_fault() {
    let mut report = adapter().decode(GOLDEN.as_bytes()).expect("golden decodes");
    report.source_digest = "not-hex".to_owned();
    let error = CheckerIndex::bind(&report, golden_source()).expect_err("malformed digest");
    assert!(
        matches!(error, CheckerError::Decode { ref message, .. } if message.contains("digest"))
    );
}

#[test]
fn surrogate_splitting_span_is_a_typed_binding_fault() {
    let source = "x\u{1F680}";
    let clean = Report {
        schema_version: 1,
        source_digest: hex_of(source.as_bytes()),
        diagnostics: Box::default(),
        declarations: Box::default(),
        references: Box::default(),
        narrowings: Box::default(),
    };
    // The rocket occupies bytes 1..5 and code units 1..3; a span that ends
    // inside the surrogate pair cannot bind to a byte boundary.
    let faulted = Report {
        schema_version: 1,
        source_digest: hex_of(source.as_bytes()),
        diagnostics: Box::default(),
        declarations: Box::new([Declaration {
            name_start: 1,
            name_end: 2,
            origin: Origin::Computed,
            overload_index: None,
            r#type: None,
        }]),
        references: Box::default(),
        narrowings: Box::default(),
    };
    let error = CheckerIndex::bind(&faulted, source).expect_err("surrogate span must fail");
    assert!(
        matches!(error, CheckerError::SpanBinding { start: 1, end: 2 }),
        "observed {error:?}"
    );
    // The report without declarations binds cleanly.
    CheckerIndex::bind(&clean, source).expect("clean report binds");
}

#[test]
fn narrowing_binds_both_spans_and_tolerates_an_absent_type() -> Result<(), CheckerError> {
    let source = "let u: string | number = 0;\nu = 1;";
    // Declaration name `u` at 28..29; assignment site 28..34.
    let report = Report {
        schema_version: 1,
        source_digest: hex_of(source.as_bytes()),
        diagnostics: Box::default(),
        declarations: Box::default(),
        references: Box::default(),
        narrowings: Box::new([Narrowing {
            name_start: 28,
            name_end: 29,
            start: 28,
            end: 34,
            r#type: None,
        }]),
    };
    let index = CheckerIndex::bind(&report, source)?;
    let narrowing = *index.narrowings().next().ok_or(CheckerError::Decode {
        message: "narrowing missing".to_owned(),
        transcript: String::new(),
    })?;
    assert_eq!(
        &source[narrowing.name.start as usize..narrowing.name.end as usize],
        "u"
    );
    assert_eq!(
        &source[narrowing.site.start as usize..narrowing.site.end as usize],
        "u = 1;"
    );
    assert_eq!(narrowing.r#type, None);
    Ok(())
}

#[test]
fn narrowing_span_outside_the_source_is_a_typed_binding_fault() {
    let source = "let u = 0;";
    let report = Report {
        schema_version: 1,
        source_digest: hex_of(source.as_bytes()),
        diagnostics: Box::default(),
        declarations: Box::default(),
        references: Box::default(),
        narrowings: Box::new([Narrowing {
            name_start: 4,
            name_end: 5,
            start: 40,
            end: 99,
            r#type: None,
        }]),
    };
    let error = CheckerIndex::bind(&report, source).expect_err("narrowing span must fail");
    assert!(matches!(
        error,
        CheckerError::SpanBinding { start: 40, end: 99 }
    ));
}

fn hex_of(bytes: &[u8]) -> String {
    source_digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(unix)]
mod bounded_child {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SCRIPT_ID: AtomicUsize = AtomicUsize::new(0);

    fn script(body: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "nudox-ts-checker-test-{}-{}",
            std::process::id(),
            SCRIPT_ID.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir_all(&root).expect("test temp directory");
        let path = root.join("checker.sh");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("test script");
        let mut permissions = std::fs::metadata(&path)
            .expect("script metadata")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&path, permissions).expect("script permissions");
        path
    }

    fn with_script(body: &str, checker: Checker) -> CheckerError {
        let path = script(body);
        let _guard = ENVIRONMENT
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .expect("environment mutex");
        checker
            .run_with_program(&path, TypeScriptSource::TypeScript, b"export const n = 1;")
            .expect_err("script must fail")
    }

    #[test]
    fn output_limit_reports_exact_operands_and_reaps_child() {
        let error = with_script(
            "printf '%0100d' 0",
            Checker {
                output_limit: 8,
                timeout: std::time::Duration::from_secs(2),
            },
        );
        assert!(matches!(
            error,
            CheckerError::OutputLimit {
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
            Checker {
                output_limit: 4096,
                timeout: std::time::Duration::from_secs(1),
            },
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(4));
        assert!(matches!(
            error,
            CheckerError::Timeout {
                phase: "collect",
                milliseconds: 1000
            }
        ));
    }

    #[test]
    fn missing_checker_is_a_typed_spawn_fault() {
        let missing = std::env::temp_dir().join("no/such/nudox-ts-checker");
        let error = Checker::default()
            .run_with_program(
                &missing,
                TypeScriptSource::TypeScript,
                b"export const n = 1;",
            )
            .expect_err("missing checker must not fall back");
        assert!(matches!(
            error,
            CheckerError::Spawn { ref program, .. } if program.contains("no/such")
        ));
    }

    #[test]
    fn module_missing_exit_maps_to_typed_module_unavailability() {
        let error = with_script(
            "cat >/dev/null; echo 'checker-driver: missing typescript' >&2; exit 3",
            Checker::default(),
        );
        assert!(
            matches!(error, CheckerError::ModuleUnavailable { ref stderr } if stderr.contains("missing typescript"))
        );
    }

    #[test]
    fn checker_failure_retains_stderr_tail() {
        let error = with_script(
            "cat >/dev/null; echo 'semantic failure' >&2; exit 1",
            Checker::default(),
        );
        assert!(
            matches!(error, CheckerError::Exit { ref stderr, .. } if stderr.contains("semantic failure"))
        );
    }
}

#[test]
fn end_to_end_fixture_preserves_overload_and_computed_facts() -> Result<(), CheckerError> {
    let _guard = ENVIRONMENT
        .get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .expect("environment mutex");
    let checker = Checker {
        timeout: std::time::Duration::from_secs(30),
        ..Checker::default()
    };
    // The real checker authority must run. A missing `node` is
    // `CheckerError::Spawn`; an unresolvable vendored `typescript` module is
    // `CheckerError::ModuleUnavailable`. Both are typed terminals, so the
    // suite cannot pass green without its toolchain.
    let report = checker.run(TypeScriptSource::TypeScript, GOLDEN_SOURCE)?;
    assert_eq!(report.source_digest, hex_of(GOLDEN_SOURCE));
    assert!(!report.declarations.is_empty());
    Ok(())
}
