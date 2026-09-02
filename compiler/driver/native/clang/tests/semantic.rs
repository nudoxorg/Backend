//! Exercises the direct Clang frontend's semantic fact admission through its observable boundary.
//! The cases prove declaration, type-use, reference, and documentation facts against real sources.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use std::fs;

use super::common::{Fixture, TestError, real_clang, require_linked_library, run_analysis};
use super::{
    ClangDiagnosticSeverity, ClangError, ClangFact, ClangReferenceKind, ClangSourceLanguage,
    ClangTypeUseResolution, EntityFact, MAX_ANALYSIS_SCRATCH_BYTES, SemanticKind,
};
use crate::ClangSourceSpan;

fn run_facts<'source>(
    fixture: &Fixture,
    source: &'source [u8],
    language: ClangSourceLanguage,
) -> Result<super::common::Analysis<'source>, TestError> {
    run_analysis(
        fixture,
        source,
        language,
        None,
        None,
        MAX_ANALYSIS_SCRATCH_BYTES,
        MAX_ANALYSIS_SCRATCH_BYTES,
    )
}

fn assert_borrowed(text: &str, start: usize, end: usize) {
    let pointer = text.as_ptr() as usize;
    assert!(pointer >= start && pointer + text.len() <= end);
}

fn probe_tool() -> Result<(), TestError> {
    require_linked_library()?;
    real_clang().map(|_| ())
}

#[test]
fn doxygen_comment_is_retained_with_lightweight_markers() -> Result<(), TestError> {
    probe_tool()?;
    let source = b"/** docs\\param value input\\return result\\deprecated old */\nint documented(int value);\n";
    let fixture = Fixture::new("c")?;
    let analysis = run_facts(&fixture, source, ClangSourceLanguage::C)?;
    let documentation = analysis
        .facts
        .iter()
        .find_map(|fact| match fact {
            ClangFact::Documentation(documentation) => Some(*documentation),
            _ => None,
        })
        .ok_or_else(|| TestError("documentation fact missing".to_owned()))?;
    assert_eq!(
        documentation.raw,
        "/** docs\\param value input\\return result\\deprecated old */"
    );
    assert_eq!(documentation.params().collect::<Vec<_>>(), vec!["value"]);
    assert!(documentation.has_return);
    assert_eq!(documentation.deprecated, Some("old */"));
    Ok(())
}

#[test]
fn reference_kind_comes_from_resolved_declaration_and_call_context() -> Result<(), TestError> {
    probe_tool()?;
    let source = b"#define MAGIC 7\nstruct S { int f; };\nint g;\nint target(void);\nint read(void) { struct S s; return g + s.f + target() + MAGIC; }\n";
    let fixture = Fixture::new("c")?;
    let analysis = run_facts(&fixture, source, ClangSourceLanguage::C)?;
    let references: Vec<_> = analysis
        .facts
        .iter()
        .filter_map(|fact| match fact {
            ClangFact::Reference(reference) => Some(*reference),
            _ => None,
        })
        .collect();
    assert!(
        references
            .iter()
            .any(|item| item.target == "g" && item.kind == ClangReferenceKind::VariableUse)
    );
    assert!(
        references
            .iter()
            .any(|item| item.target == "f" && item.kind == ClangReferenceKind::FieldAccess)
    );
    assert!(
        references
            .iter()
            .any(|item| item.target == "target" && item.kind == ClangReferenceKind::FunctionCall)
    );
    assert!(references.iter().any(|item| {
        item.target == "MAGIC" && item.kind == ClangReferenceKind::MacroInvocation
    }));
    Ok(())
}

#[test]
fn contiguous_documentation_and_parameter_order_are_exact() -> Result<(), TestError> {
    probe_tool()?;
    let source = b"/** license doc */\n/* plain note */\nint distant(void);\n/// one\n/// two\n/// three\n/**\n * \\param[out] first output\n * \\param second input\n */\nint documented(int first, int second);\n";
    let fixture = Fixture::new("c")?;
    let analysis = run_facts(&fixture, source, ClangSourceLanguage::C)?;
    let docs: Vec<_> = analysis
        .facts
        .iter()
        .filter_map(|fact| match fact {
            ClangFact::Documentation(documentation) => Some(*documentation),
            _ => None,
        })
        .collect();
    assert!(
        !docs.iter().any(|doc| doc.raw.contains("license")),
        "{docs:?}"
    );
    let documented = docs
        .iter()
        .find(|doc| doc.raw.contains("first output"))
        .ok_or_else(|| TestError("parameter documentation missing".to_owned()))?;
    assert_eq!(
        documented.params().collect::<Vec<_>>(),
        vec!["first", "second"]
    );
    assert!(!documented.raw.contains("one"));
    Ok(())
}

#[test]
fn utf8_spans_are_bytes_and_occurrences_keep_exact_owner_offsets() -> Result<(), TestError> {
    probe_tool()?;
    let source = "// 漢字\nint target(void);\nint first(void) { return target(); }\nint second(void) { return target(); }\n".as_bytes();
    let fixture = Fixture::new("c")?;
    let analysis = run_facts(&fixture, source, ClangSourceLanguage::C)?;
    let occurrences: Vec<_> = analysis
        .facts
        .iter()
        .filter_map(|fact| match fact {
            ClangFact::Reference(reference) if reference.target == "target" => Some(*reference),
            _ => None,
        })
        .collect();
    assert_eq!(occurrences.len(), 2);
    assert_eq!(
        occurrences[0].use_span,
        ClangSourceSpan { start: 53, end: 59 }
    );
    assert_eq!(
        occurrences[1].use_span,
        ClangSourceSpan { start: 91, end: 97 }
    );
    assert_ne!(occurrences[0].owner, occurrences[1].owner);
    for occurrence in occurrences {
        assert_eq!(
            &source[occurrence.use_span.start as usize..occurrence.use_span.end as usize],
            b"target"
        );
    }
    Ok(())
}

#[test]
fn function_call_occurrence_is_oracle_and_owner_relative() -> Result<(), TestError> {
    probe_tool()?;
    let source = b"int target(void);\nint caller(void) { return target(); }\n";
    let fixture = Fixture::new("c")?;
    let analysis = run_facts(&fixture, source, ClangSourceLanguage::C)?;
    let occurrence = analysis
        .facts
        .iter()
        .find_map(|fact| match fact {
            ClangFact::Reference(reference)
                if reference.target == "target"
                    && reference.kind == ClangReferenceKind::FunctionCall =>
            {
                Some(*reference)
            }
            _ => None,
        })
        .ok_or_else(|| TestError("function-call occurrence missing".to_owned()))?;
    assert_eq!(
        occurrence.confidence(),
        super::super::protocol::Confidence::Oracle
    );
    let relative = occurrence
        .relative_span()
        .ok_or_else(|| TestError("function-call occurrence was not inside its owner".to_owned()))?;
    assert!(relative.start < relative.end);
    assert!(relative.end <= occurrence.owner.end - occurrence.owner.start);
    Ok(())
}

#[test]
fn clang_emits_native_borrowed_entities_types_and_identity_references() -> Result<(), TestError> {
    probe_tool()?;
    let source =
        b"struct Widget { int field; };\nint helper(struct Widget *value) { return value->field; }\n";
    let fixture = Fixture::new("c")?;
    let analysis = run_facts(&fixture, source, ClangSourceLanguage::C)?;
    let (facts, report) = (&analysis.facts, &analysis.report);
    if report.entities < 3 || report.type_uses < 2 || report.references < 1 {
        return Err(TestError(format!("short native report: {report:?}")));
    }
    assert!(report.library_version_bytes > 0);
    assert!(report.journal_used > 0);
    assert!(report.caller_scratch_high_water > 0);
    let source_start = source.as_ptr() as usize;
    let source_end = source_start + source.len();
    let mut saw_record = false;
    let mut saw_field = false;
    let mut saw_function = false;
    let mut saw_builtin = false;
    let mut saw_declaration_type = false;
    let mut saw_reference = false;
    for fact in facts {
        match fact {
            ClangFact::Entity(item) => {
                assert_borrowed(item.name, source_start, source_end);
                assert_eq!(
                    &source[item.span.start as usize..item.span.end as usize],
                    item.name.as_bytes()
                );
                assert!(item.owner.start <= item.span.start && item.owner.end >= item.span.end);
                match item.kind {
                    SemanticKind::Record => saw_record = item.name == "Widget",
                    SemanticKind::Field => saw_field = item.name == "field",
                    SemanticKind::Function => saw_function = item.name == "helper",
                    SemanticKind::Parameter | SemanticKind::Constant | SemanticKind::Static => {}
                    _ => {}
                }
            }
            ClangFact::TypeUse(item) => {
                assert_borrowed(item.name, source_start, source_end);
                match item.resolution {
                    ClangTypeUseResolution::Builtin => saw_builtin |= item.name == "int",
                    ClangTypeUseResolution::Declaration { target, .. } => {
                        assert_eq!(
                            &source[target.start as usize..target.end as usize],
                            b"Widget"
                        );
                        saw_declaration_type |= item.name == "Widget";
                    }
                }
            }
            ClangFact::Reference(item) => {
                assert_borrowed(item.target, source_start, source_end);
                assert_ne!(item.use_span, item.resolved_span);
                assert_eq!(
                    &source[item.use_span.start as usize..item.use_span.end as usize],
                    item.target.as_bytes()
                );
                saw_reference |= item.kind == ClangReferenceKind::FieldAccess
                    && item.target == "field"
                    && &source[item.resolved_span.start as usize..item.resolved_span.end as usize]
                        == b"field";
            }
            ClangFact::Diagnostic(_) => {
                return Err(TestError("valid C source emitted a diagnostic".to_owned()));
            }
            ClangFact::Documentation(_) => {}
        }
    }
    assert!(
        saw_record
            && saw_field
            && saw_function
            && saw_builtin
            && saw_declaration_type
            && saw_reference
    );
    Ok(())
}

#[test]
fn native_reference_identity_wins_over_same_spelling_shadowing() -> Result<(), TestError> {
    probe_tool()?;
    let source = b"struct Left { int value; };\nstruct Right { int value; };\nint read(Left left, Right *right) { return left.value + right->value; }\n";
    let fixture = Fixture::new("cc")?;
    let analysis = run_facts(&fixture, source, ClangSourceLanguage::Cxx)?;
    let facts = &analysis.facts;
    let fields: Vec<_> = facts
        .iter()
        .filter_map(|fact| match fact {
            ClangFact::Entity(item) if item.kind == SemanticKind::Field && item.name == "value" => {
                Some(*item)
            }
            _ => None,
        })
        .collect();
    let references: Vec<_> = facts
        .iter()
        .filter_map(|fact| match fact {
            ClangFact::Reference(item) if item.kind == ClangReferenceKind::FieldAccess => {
                Some(*item)
            }
            _ => None,
        })
        .collect();
    assert_eq!(fields.len(), 2);
    assert_eq!(references.len(), 2);
    assert_eq!(references[0].resolved_span, fields[0].span);
    assert_eq!(references[1].resolved_span, fields[1].span);
    assert_ne!(references[0].resolved_span, references[1].resolved_span);
    assert_ne!(fields[0].identity, fields[1].identity);
    for reference in references {
        assert_eq!(reference.target, "value");
        let matching_field = fields
            .iter()
            .find(|field| field.span == reference.resolved_span)
            .ok_or_else(|| TestError("resolved span matched no shadowed field".to_owned()))?;
        assert_eq!(reference.identity, matching_field.identity);
        assert_eq!(
            &source[reference.use_span.start as usize..reference.use_span.end as usize],
            b"value"
        );
    }
    Ok(())
}

#[test]
fn clang_uses_real_source_and_ignores_comment_string_bait() -> Result<(), TestError> {
    probe_tool()?;
    let source = b"// struct FakeComment { int bait; };\nconst char *text = \"struct FakeString { int bait; };\";\nstruct Real { int field; };\n";
    let fixture = Fixture::new("c")?;
    let analysis = run_facts(&fixture, source, ClangSourceLanguage::C)?;
    let names: Vec<&str> = analysis
        .facts
        .iter()
        .filter_map(|fact| match fact {
            ClangFact::Entity(item) => Some(item.name),
            _ => None,
        })
        .collect();
    assert!(names.contains(&"Real"));
    assert!(!names.contains(&"FakeComment") && !names.contains(&"FakeString"));
    Ok(())
}

#[test]
fn expected_version_family_is_enforced_before_analysis() -> Result<(), TestError> {
    probe_tool()?;
    let source = b"struct VersionChecked {};\n";
    let fixture = Fixture::new("c")?;
    let wrong_version = b"0.0.0 intentionally wrong\n";
    let mut identity = vec![0_u8; MAX_ANALYSIS_SCRATCH_BYTES];
    let mut facts = vec![0_u8; MAX_ANALYSIS_SCRATCH_BYTES];
    let mut emitted = Vec::new();
    let error = super::analyze(
        super::AnalysisInput {
            include_root: fixture.root(),
            source_name: fixture.source_name(),
            source_language: ClangSourceLanguage::C,
            source,
        },
        Some(wrong_version),
        None,
        None,
        super::AnalysisScratch {
            identity: &mut identity,
            facts: &mut facts,
        },
        |fact| emitted.push(fact),
    );
    assert!(
        matches!(
            error,
            Err(ClangError::ToolVersionMismatch { expected_bytes, .. }) if expected_bytes == wrong_version.len()
        ),
        "unexpected seal terminal: {error:?}"
    );
    assert!(emitted.is_empty());

    let real_tool = real_clang()?;
    let analysis = run_analysis(
        &fixture,
        source,
        ClangSourceLanguage::C,
        Some(&real_tool.version),
        None,
        MAX_ANALYSIS_SCRATCH_BYTES,
        MAX_ANALYSIS_SCRATCH_BYTES,
    )?;
    assert_eq!(analysis.report.entities, 1);
    Ok(())
}

#[test]
fn nested_cpp_owners_are_exact_enclosing_ranges() -> Result<(), TestError> {
    probe_tool()?;
    let source = b"struct Outer { struct Inner { int inner_field; }; int outer_field; };\nint use_outer(Outer *value) { return value->outer_field; }\n";
    let fixture = Fixture::new("cc")?;
    let analysis = run_facts(&fixture, source, ClangSourceLanguage::Cxx)?;
    let facts = &analysis.facts;
    let find = |name: &str| -> Option<EntityFact> {
        facts.iter().find_map(|fact| match fact {
            ClangFact::Entity(item) if item.name == name => Some(*item),
            _ => None,
        })
    };
    let (Some(outer), Some(inner), Some(inner_field), Some(outer_field)) = (
        find("Outer"),
        find("Inner"),
        find("inner_field"),
        find("outer_field"),
    ) else {
        return Err(TestError("nested declaration facts missing".to_owned()));
    };
    assert_eq!(
        &source[outer.owner.start as usize..outer.owner.end as usize],
        b"struct Outer { struct Inner { int inner_field; }; int outer_field; }"
    );
    assert_eq!(
        &source[inner.owner.start as usize..inner.owner.end as usize],
        b"struct Inner { int inner_field; }"
    );
    assert!(outer.owner.start < inner.owner.start && inner.owner.end <= outer.owner.end);
    assert_eq!(inner_field.owner, inner.owner);
    assert_eq!(outer_field.owner, outer.owner);
    Ok(())
}

#[test]
fn malformed_source_preserves_first_native_diagnostic_without_partial_facts()
-> Result<(), TestError> {
    probe_tool()?;
    let source = b"int broken( {\n";
    let fixture = Fixture::new("c")?;
    let mut identity = vec![0_u8; MAX_ANALYSIS_SCRATCH_BYTES];
    let mut facts = vec![0_u8; MAX_ANALYSIS_SCRATCH_BYTES];
    let mut emitted = Vec::new();
    let error = super::analyze(
        super::AnalysisInput {
            include_root: fixture.root(),
            source_name: fixture.source_name(),
            source_language: ClangSourceLanguage::C,
            source,
        },
        None,
        None,
        None,
        super::AnalysisScratch {
            identity: &mut identity,
            facts: &mut facts,
        },
        |fact| emitted.push(fact),
    )
    .expect_err("malformed source must be rejected");
    let diagnostic = match error {
        ClangError::ParseRejected {
            diagnostic: Some(diagnostic),
            ..
        } => diagnostic,
        other => {
            return Err(TestError(format!("unexpected malformed error: {other:?}")));
        }
    };
    assert_eq!(diagnostic.severity, ClangDiagnosticSeverity::Error);
    assert_eq!(diagnostic.line, 1);
    assert!(diagnostic.column > 0);
    assert!(emitted.is_empty());
    Ok(())
}

#[test]
fn diagnostics_keep_main_file_severity_and_reject_header_ownership() -> Result<(), TestError> {
    probe_tool()?;
    let source = b"#warning main warning\n#include \"header.h\"\nstruct [[deprecated(\"old\")]] Old {};\nOld value;\n";
    let fixture = Fixture::new("cc")?;
    fs::write(
        fixture.root().join("header.h"),
        b"#warning header warning\n",
    )?;
    let analysis = run_facts(&fixture, source, ClangSourceLanguage::Cxx)?;
    let diagnostics: Vec<_> = analysis
        .facts
        .iter()
        .filter_map(|fact| match fact {
            ClangFact::Diagnostic(item) => Some(*item),
            _ => None,
        })
        .collect();
    let severities: Vec<_> = diagnostics.iter().map(|item| item.severity).collect();
    assert_eq!(
        severities,
        vec![
            ClangDiagnosticSeverity::Warning,
            ClangDiagnosticSeverity::Note,
            ClangDiagnosticSeverity::Warning,
            ClangDiagnosticSeverity::Note
        ]
    );
    assert!(diagnostics.iter().all(|item| item.line <= 4));
    assert_eq!(analysis.report.diagnostics, diagnostics.len() as u32);
    Ok(())
}

#[test]
fn package_context_probe_admits_bounded_root_include() -> Result<(), TestError> {
    probe_tool()?;
    let source = b"#include \"package_config.h\"\nstruct Local { int value; };\nint read(void) { return PACKAGE_VALUE; }\n";
    let fixture = Fixture::new("c")?;
    fs::write(
        fixture.root().join("package_config.h"),
        b"#define PACKAGE_VALUE 7\n",
    )?;
    let analysis = run_facts(&fixture, source, ClangSourceLanguage::C)?;
    assert!(analysis.report.entities >= 2);
    assert!(analysis.facts.iter().any(|fact| matches!(
        fact,
        ClangFact::Entity(item) if item.name == "Local"
    )));
    assert!(!analysis.facts.iter().any(|fact| matches!(
        fact,
        ClangFact::Reference(item) if item.target == "PACKAGE_VALUE"
    )));
    Ok(())
}

#[test]
fn resolved_member_targets_use_any_call_expression_flavor() -> Result<(), TestError> {
    probe_tool()?;
    // Destructor references use the same MethodCall role as ordinary and static member calls;
    // libclang 21 reports each through a CallExpr cursor in this fixture.
    let source = b"struct S { int m(int value); static int s(int value); ~S(); }; int f(S *v) { return v->m(1) + S::s(2) + (v->~S(), 0); }\n";
    let fixture = Fixture::new("cc")?;
    let analysis = run_facts(&fixture, source, ClangSourceLanguage::Cxx)?;
    let calls: Vec<_> = analysis
        .facts
        .iter()
        .filter_map(|fact| match fact {
            ClangFact::Reference(reference) => Some(*reference),
            _ => None,
        })
        .collect();
    assert!(
        calls
            .iter()
            .any(|item| item.target == "m" && item.kind == ClangReferenceKind::MethodCall)
    );
    assert!(
        calls
            .iter()
            .any(|item| item.target == "s" && item.kind == ClangReferenceKind::MethodCall)
    );
    assert!(
        calls
            .iter()
            .any(|item| item.target == "~" && item.kind == ClangReferenceKind::MethodCall)
    );
    Ok(())
}

#[test]
fn documentation_is_emitted_once_for_a_function_not_its_parameters() -> Result<(), TestError> {
    probe_tool()?;
    let source = b"/// documented\nint documented(int first, int second);\n";
    let fixture = Fixture::new("c")?;
    let analysis = run_facts(&fixture, source, ClangSourceLanguage::C)?;
    let docs = analysis
        .facts
        .iter()
        .filter(|fact| matches!(fact, ClangFact::Documentation(_)))
        .count();
    assert_eq!(docs, 1);
    Ok(())
}

#[test]
fn external_header_type_is_not_silently_dropped() -> Result<(), TestError> {
    probe_tool()?;
    let source = b"#include \"package_types.h\"\nstruct Local { struct HeaderType value; };\n";
    let fixture = Fixture::new("c")?;
    fs::write(
        fixture.root().join("package_types.h"),
        b"struct HeaderType { int field; };\n",
    )?;
    let mut identity = vec![0_u8; MAX_ANALYSIS_SCRATCH_BYTES];
    let mut facts = vec![0_u8; MAX_ANALYSIS_SCRATCH_BYTES];
    let mut emitted = Vec::new();
    let error = super::analyze(
        super::AnalysisInput {
            include_root: fixture.root(),
            source_name: fixture.source_name(),
            source_language: ClangSourceLanguage::C,
            source,
        },
        None,
        None,
        None,
        super::AnalysisScratch {
            identity: &mut identity,
            facts: &mut facts,
        },
        |fact| emitted.push(fact),
    );
    assert!(matches!(
        error,
        Err(ClangError::LibclangExternalIdentityUnavailable { .. })
    ));
    assert!(emitted.is_empty());
    Ok(())
}
