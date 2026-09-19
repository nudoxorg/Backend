//! Offline falsifiers for the closed TypeScript checker protocol.
//! Each test targets one schema, binding, or subprocess law.
//! Subprocess falsifiers use only temporary local scripts through the typed
//! program seam. The end-to-end authority test drives the real vendored
//! checker and reports a typed [`CheckerError`] when `node` or the
//! `typescript` module is unavailable; it never self-skips.

use backend_frontend_typescript::legacy::{
    Checker, CheckerError, CheckerIndex, Declaration, LiteralBase, MappedModifier, Narrowing,
    Origin, Report, TypeTree, source_digest,
};

const GOLDEN: &str = include_str!("transcripts/golden.json");
const GOLDEN_SOURCE: &[u8] = include_bytes!("fixtures/source.ts");
const UNDEFINED_TYPE_SOURCE: &[u8] = include_bytes!("fixtures/l7_r2_old_crash.ts");
const AS_CONST: &str = include_str!("transcripts/as-const.json");
const AS_CONST_SOURCE: &[u8] = include_bytes!("fixtures/as-const.ts");
const SIGNATURE_PARAMETERS: &str = include_str!("transcripts/signature-parameters.json");
const SIGNATURE_PARAMETERS_SOURCE: &[u8] = include_bytes!("fixtures/signature-parameters.ts");
const TYPE_ALIAS: &str = include_str!("transcripts/type-alias.json");
const TYPE_ALIAS_SOURCE: &[u8] = include_bytes!("fixtures/type-alias.ts");

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
    let first_reference = *index.references().next().ok_or(CheckerError::Decode {
        message: "golden reference missing".to_owned(),
        transcript: String::new(),
    })?;
    assert_eq!(
        index.reference_at(first_reference.span),
        Some(&first_reference)
    );
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
fn undefined_conditional_branch_is_an_honest_closed_record() -> Result<(), CheckerError> {
    let _guard = ENVIRONMENT
        .get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .map_err(|_| CheckerError::Decode {
            message: "checker environment mutex poisoned".to_owned(),
            transcript: String::new(),
        })?;
    // The real checker authority must run; a missing `typescript` module is a
    // typed `CheckerError::ModuleUnavailable` terminal, never a silent pass.
    let report = Checker::default().run(
        backend_semantic::vocabulary::TypeScriptSource::TypeScript,
        UNDEFINED_TYPE_SOURCE,
    )?;
    assert!(report.declarations.iter().any(|declaration| {
        matches!(
            declaration.r#type,
            Some(TypeTree::Conditional { ref then_type, ref else_type, .. })
                if matches!(then_type.as_ref(), TypeTree::Other { text } if text == "any")
                    && matches!(else_type.as_ref(), TypeTree::Other { text } if text == "any")
        )
    }));
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
fn as_const_golden_seals_members_readonly() -> Result<(), CheckerError> {
    let report = adapter().decode(AS_CONST.as_bytes())?;
    assert_eq!(report.source_digest, hex_of(AS_CONST_SOURCE));
    let source = core::str::from_utf8(AS_CONST_SOURCE).expect("as-const fixture is UTF-8");
    let index = CheckerIndex::bind(&report, source)?;
    // `export const readonlyValue = { a: 1 } as const;` writes no `readonly`
    // token, yet the `as const` assertion seals every member readonly.
    let sealed = *index.declarations().next().ok_or(CheckerError::Decode {
        message: "as-const declaration missing".to_owned(),
        transcript: String::new(),
    })?;
    let Some(TypeTree::Object { members }) = sealed.r#type else {
        panic!("as-const declaration must be an object literal type");
    };
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].name, "a");
    assert!(members[0].readonly, "as const must seal members readonly");
    Ok(())
}

#[test]
fn as_const_authority_reports_sealed_members_readonly() -> Result<(), CheckerError> {
    let _guard = ENVIRONMENT
        .get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .expect("environment mutex");
    let report = Checker::default().run(
        backend_semantic::vocabulary::TypeScriptSource::TypeScript,
        AS_CONST_SOURCE,
    )?;
    let sealed = report
        .declarations
        .iter()
        .find_map(|declaration| match declaration.r#type.as_ref() {
            Some(tree @ TypeTree::Object { .. }) => Some(tree),
            _ => None,
        })
        .ok_or(CheckerError::Decode {
            message: "as-const object type missing".to_owned(),
            transcript: String::new(),
        })?;
    let TypeTree::Object { members } = sealed else {
        panic!("sealed object type missing");
    };
    assert!(
        members.iter().any(|member| member.name == "a" && member.readonly),
        "the live authority must seal `as const` members readonly: {members:?}"
    );
    Ok(())
}

#[test]
fn structured_parameter_golden_preserves_names_and_flags() -> Result<(), CheckerError> {
    let report = adapter().decode(SIGNATURE_PARAMETERS.as_bytes())?;
    assert_eq!(report.source_digest, hex_of(SIGNATURE_PARAMETERS_SOURCE));
    let Some(TypeTree::Function { parameters, .. }) = report.declarations[0].r#type.as_ref()
    else {
        return Err(CheckerError::Decode {
            message: "signature missing".to_owned(),
            transcript: String::new(),
        });
    };
    assert_eq!(parameters[0].name.as_deref(), Some("value"));
    assert!(!parameters[0].optional && !parameters[0].rest);
    assert_eq!(parameters[1].name.as_deref(), Some("optional"));
    assert!(parameters[1].optional && !parameters[1].rest);
    assert_eq!(parameters[2].name.as_deref(), Some("rest"));
    assert!(parameters[2].rest && !parameters[2].optional);
    // Each structured cell still carries the bare checker type.
    assert_eq!(
        parameters[0].r#type,
        TypeTree::Primitive {
            name: "string".to_owned()
        }
    );
    Ok(())
}

#[test]
fn legacy_parameter_cells_still_decode_as_bare_types() -> Result<(), CheckerError> {
    // golden.json predates the structured parameter lane: every signature
    // parameter is a bare type tree and must keep decoding with name `None`
    // and both flags `false`, so persisted reports never change meaning.
    let report = adapter().decode(GOLDEN.as_bytes())?;
    let function = report
        .declarations
        .iter()
        .find_map(|declaration| match declaration.r#type.as_ref() {
            Some(tree @ TypeTree::Function { parameters, .. }) if !parameters.is_empty() => {
                Some(tree)
            }
            _ => None,
        })
        .ok_or(CheckerError::Decode {
            message: "golden signature missing".to_owned(),
            transcript: String::new(),
        })?;
    let TypeTree::Function { parameters, .. } = function else {
        panic!("golden signature missing");
    };
    assert_eq!(parameters[0].name, None);
    assert!(!parameters[0].optional);
    assert!(!parameters[0].rest);
    assert_eq!(
        parameters[0].r#type,
        TypeTree::Primitive {
            name: "number".to_owned()
        }
    );
    Ok(())
}

#[test]
fn signature_authority_preserves_parameter_names_and_flags() -> Result<(), CheckerError> {
    let _guard = ENVIRONMENT
        .get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .expect("environment mutex");
    let report = Checker::default().run(
        backend_semantic::vocabulary::TypeScriptSource::TypeScript,
        SIGNATURE_PARAMETERS_SOURCE,
    )?;
    let signature = report
        .declarations
        .iter()
        .find_map(|declaration| match declaration.r#type.as_ref() {
            Some(tree @ TypeTree::Function { .. }) => Some(tree),
            _ => None,
        })
        .ok_or(CheckerError::Decode {
            message: "live signature missing".to_owned(),
            transcript: String::new(),
        })?;
    let TypeTree::Function { parameters, .. } = signature else {
        panic!("live signature missing");
    };
    assert_eq!(parameters[0].name.as_deref(), Some("value"));
    assert_eq!(parameters[1].name.as_deref(), Some("optional"));
    assert!(parameters[1].optional);
    assert_eq!(parameters[2].name.as_deref(), Some("rest"));
    assert!(parameters[2].rest);
    Ok(())
}

#[test]
fn type_alias_golden_decodes_declared_cells() -> Result<(), CheckerError> {
    let report = adapter().decode(TYPE_ALIAS.as_bytes())?;
    assert_eq!(report.source_digest, hex_of(TYPE_ALIAS_SOURCE));
    let source = core::str::from_utf8(TYPE_ALIAS_SOURCE).expect("type-alias fixture is UTF-8");
    let index = CheckerIndex::bind(&report, source)?;
    let named = index.declarations().find(|declaration| {
        &source[declaration.name.start as usize..declaration.name.end as usize] == "Identifier"
    });
    let identifier = named.ok_or(CheckerError::Decode {
        message: "type-alias declaration missing".to_owned(),
        transcript: String::new(),
    })?;
    assert_eq!(identifier.origin, Origin::Declared);
    assert_eq!(
        identifier.r#type,
        Some(&TypeTree::Primitive {
            name: "string".to_owned()
        })
    );
    let pair = index
        .declarations()
        .find(|declaration| {
            &source[declaration.name.start as usize..declaration.name.end as usize] == "Pair"
        })
        .ok_or(CheckerError::Decode {
            message: "object-alias declaration missing".to_owned(),
            transcript: String::new(),
        })?;
    assert_eq!(pair.origin, Origin::Declared);
    let Some(TypeTree::Object { members }) = pair.r#type else {
        panic!("object alias must carry its literal members");
    };
    assert_eq!(
        members.iter().map(|member| member.name.as_str()).collect::<Vec<_>>(),
        vec!["first", "second"]
    );
    Ok(())
}

#[test]
fn type_alias_authority_emits_declared_cells() -> Result<(), CheckerError> {
    let _guard = ENVIRONMENT
        .get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .expect("environment mutex");
    let report = Checker::default().run(
        backend_semantic::vocabulary::TypeScriptSource::TypeScript,
        TYPE_ALIAS_SOURCE,
    )?;
    let alias = report
        .declarations
        .iter()
        .find(|declaration| declaration.origin == Origin::Declared)
        .ok_or(CheckerError::Decode {
            message: "declared type-alias cell missing".to_owned(),
            transcript: String::new(),
        })?;
    assert_eq!(
        alias.r#type,
        Some(TypeTree::Primitive {
            name: "string".to_owned()
        })
    );
    Ok(())
}

#[test]
fn mapped_report_decodes_modifiers_and_the_optional_as_remap() -> Result<(), CheckerError> {
    let report = adapter().decode(
        br#"{
        "schemaVersion": 1,
        "sourceDigest": "00",
        "diagnostics": [],
        "declarations": [{
            "nameStart": 0,
            "nameEnd": 1,
            "origin": "computed",
            "type": {
                "kind": "mapped",
                "parameter": "K",
                "constraint": { "kind": "primitive", "name": "string" },
                "nameAs": { "kind": "primitive", "name": "number" },
                "value": { "kind": "primitive", "name": "boolean" },
                "readonly": "add",
                "optional": "remove"
            }
        }],
        "references": [],
        "narrowings": []
    }"#,
    )?;
    let declaration = report.declarations.first().ok_or(CheckerError::Decode {
        message: "mapped declaration missing".to_owned(),
        transcript: String::new(),
    })?;
    let Some(TypeTree::Mapped {
        name_as,
        readonly,
        optional,
        ..
    }) = declaration.r#type.as_ref()
    else {
        panic!("mapped checker type missing");
    };
    assert!(name_as.is_some());
    assert_eq!(*readonly, MappedModifier::Add);
    assert_eq!(*optional, MappedModifier::Remove);
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
        declaration_file: false,
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
        declaration_file: false,
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
        declaration_file: false,
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
        declaration_file: false,
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
            .run_with_program(
                &path,
                backend_semantic::vocabulary::TypeScriptSource::TypeScript,
                b"export const n = 1;",
            )
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
                backend_semantic::vocabulary::TypeScriptSource::TypeScript,
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

    #[test]
    fn explicit_node_binds_the_selected_module_root_without_ambient_lookup() {
        let source = b"export const n = 1;";
        let module_root = std::env::temp_dir().join(format!(
            "nudox-ts-modules-{}-{}",
            std::process::id(),
            SCRIPT_ID.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir_all(&module_root).expect("test module root");
        let digest = hex_of(source);
        let node = script(&format!(
            "test \"$NODE_PATH\" = \"{}\" || exit 7\nprintf '%s' '{{\"schemaVersion\":1,\"sourceDigest\":\"{digest}\",\"diagnostics\":[],\"declarations\":[],\"references\":[],\"narrowings\":[]}}'",
            module_root.display(),
        ));
        let report = Checker::default()
            .with_node(node, module_root.clone())
            .expect("absolute Node authority is admissible")
            .run(
                backend_semantic::vocabulary::TypeScriptSource::TypeScript,
                source,
            )
            .expect("selected module root reaches the explicit child");
        assert_eq!(report.source_digest, digest);
        std::fs::remove_dir(&module_root).expect("remove test module root");
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
    let report = checker.run(
        backend_semantic::vocabulary::TypeScriptSource::TypeScript,
        GOLDEN_SOURCE,
    )?;
    assert_eq!(report.source_digest, hex_of(GOLDEN_SOURCE));
    assert!(!report.declarations.is_empty());
    Ok(())
}
