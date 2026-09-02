//! Protocol, UTF-16 coordinate, and replay falsifier tests for Java.
//! Each assertion names one semantic law.
//! The fixture is committed so tests do not depend on host javadoc state.

use compiler_languages_java::{
    Constant, DecodeError, TypeMirror, Utf16Span, decode, probe_javadoc_path, utf16_span_to_utf8,
};

const GOLDEN: &[u8] = include_bytes!("fixtures/golden.json");

#[derive(Debug, thiserror::Error)]
enum TestError {
    #[error(transparent)]
    Decode(#[from] DecodeError),
    #[error("the deliberately bogus tool unexpectedly started: {0:?}")]
    UnexpectedTool(std::path::PathBuf),
}

#[test]
fn golden_preserves_overloads_throws_and_doc_flavor() -> Result<(), TestError> {
    let output = decode(GOLDEN)?;
    let cafe = &output.types[0];
    assert_eq!(cafe.simple_name, "Cafe😀");
    assert_eq!(cafe.doc_kind.as_deref(), Some("END_OF_LINE"));
    assert_eq!(output.references[0].owner, "demo.Cafe😀#run(int)");
    assert_eq!(output.references[1].owner, "demo.Cafe😀#run(String)");
    assert_eq!(
        (output.references[0].start, output.references[0].end),
        (12, 16)
    );
    assert_eq!(
        (output.references[1].start, output.references[1].end),
        (20, 24)
    );
    let methods = &cafe.methods;
    assert_eq!(methods[0].name, "run");
    assert_eq!(methods[1].name, "run", "overload order is preserved");
    match &methods[0].thrown[0] {
        TypeMirror::Declared { name, .. } => assert_eq!(name, "java.lang.Exception"),
        other => panic!("expected a declared throws fact, got {other:?}"),
    }
    match &methods[1].thrown[0] {
        TypeMirror::Declared { name, .. } => assert_eq!(name, "java.lang.IllegalStateException"),
        other => panic!("expected a declared throws fact, got {other:?}"),
    }
    Ok(())
}

#[test]
fn golden_decodes_every_structural_type_mirror_kind() -> Result<(), TestError> {
    let output = decode(GOLDEN)?;
    let cafe = &output.types[0];
    match &cafe.superclass {
        Some(TypeMirror::Declared {
            name, annotations, ..
        }) => {
            assert_eq!(name, "java.lang.Object");
            assert!(annotations.is_empty());
        }
        other => panic!("expected a declared superclass, got {other:?}"),
    }
    let brew_all = &cafe.methods[3];
    assert!(brew_all.varargs, "varargs fact retained");
    match &brew_all.params[0].r#type {
        TypeMirror::Declared { args, .. } => match &args[0] {
            TypeMirror::Wildcard {
                extends,
                super_bound,
            } => {
                assert!(super_bound.is_none());
                assert!(matches!(
                    extends.as_deref(),
                    Some(TypeMirror::Declared { name, .. }) if name == "java.lang.Number"
                ));
            }
            other => panic!("expected a wildcard argument, got {other:?}"),
        },
        other => panic!("expected a declared list parameter, got {other:?}"),
    }
    match &brew_all.params[1].r#type {
        TypeMirror::Array { component, .. } => assert!(matches!(
            component.as_ref(),
            TypeMirror::Declared { name, .. } if name == "java.lang.String"
        )),
        other => panic!("expected an array parameter, got {other:?}"),
    }
    match &cafe.methods[4].type_params[0].bounds[0] {
        TypeMirror::Intersection { bounds } => {
            assert_eq!(bounds.len(), 2);
        }
        other => panic!("expected an intersection bound, got {other:?}"),
    }
    assert!(matches!(
        cafe.methods[4].return_type,
        Some(TypeMirror::TypeVariable { ref name, .. }) if name == "T"
    ));
    assert!(matches!(
        cafe.fields[0].r#type,
        TypeMirror::Primitive { ref name, .. } if name == "int"
    ));
    assert!(cafe.constructors[0].return_type.is_none());
    let roast = &output.types[1];
    assert!(matches!(
        roast.fields[0].constant,
        Some(Constant::Int { value: 2 })
    ));
    assert_eq!(
        roast.enum_constants[0].source.as_deref(),
        Some("LIGHT(1),"),
        "enum constant source slice retained"
    );
    Ok(())
}

#[test]
fn decoder_admits_every_emitted_type_mirror_kind() -> Result<(), TestError> {
    // The remaining mirror kinds the Cafe fixture cannot spell naturally:
    // multi-catch unions, the NONE/NULL pseudo-types, unresolvable errors,
    // and the `other` fallback. This is a decoder-surface law, not a claim
    // about where the doclet emits them.
    let document = br#"{"format":1,"javaVersion":"21","modules":[],"packages":[],"types":[{"qualifiedName":"k.K","simpleName":"K","kind":"CLASS","package":"k","module":null,"enclosing":null,"nesting":"TOP_LEVEL","modifiers":[],"typeParams":[],"superclass":{"kind":"declared","name":"java.lang.Object","args":[],"owner":null,"annotations":[]},"interfaces":[{"kind":"none"}],"permits":[{"kind":"null"}],"recordComponents":[],"annotations":[],"deprecated":false,"fields":[],"enumConstants":[],"constructors":[],"methods":[{"name":"m","modifiers":[],"typeParams":[],"params":[],"return":{"kind":"other","repr":"x"},"thrown":[{"kind":"union","alternatives":[{"kind":"error","name":"A"},{"kind":"error","name":"B"}]}],"varargs":false,"default":false,"receiver":null,"annotationDefault":null,"annotations":[],"deprecated":false,"origin":"EXPLICIT","doc":null,"docKind":null,"position":null}],"nested":[],"doc":null,"docKind":null,"position":null}],"references":[]}"#;
    let output = decode(document)?;
    let methods = &output.types[0].methods;
    assert!(matches!(
        methods[0].return_type,
        Some(TypeMirror::Other { ref repr }) if repr == "x"
    ));
    assert_eq!(output.types[0].interfaces[0], TypeMirror::None);
    assert_eq!(output.types[0].permits[0], TypeMirror::Null);
    match &methods[0].thrown[0] {
        TypeMirror::Union { alternatives } => assert_eq!(alternatives.len(), 2),
        other => panic!("expected a union throws fact, got {other:?}"),
    }
    Ok(())
}

#[test]
fn utf16_offsets_become_different_utf8_byte_spans() -> Result<(), DecodeError> {
    let span = utf16_span_to_utf8("😀é", Utf16Span { start: 2, end: 3 })?;
    assert_eq!(span, 4..6);
    assert!(matches!(
        utf16_span_to_utf8("😀", Utf16Span { start: 1, end: 2 }),
        Err(DecodeError::SurrogateSplit {
            start: 1,
            end: 2,
            ..
        })
    ));
    Ok(())
}

#[test]
fn mutations_report_exact_rejection_classes() -> Result<(), TestError> {
    let stale =
        br#"{"format":2,"javaVersion":"21","modules":[],"packages":[],"types":[],"references":[]}"#;
    // A 2^32+1 schema cell must not fold onto 1 through a u32 cast.
    let u64_stale = br#"{"format":4294967297,"javaVersion":"21","modules":[],"packages":[],"types":[],"references":[]}"#;
    let unknown = br#"{"format":1,"javaVersion":"21","modules":[],"packages":[],"types":[],"references":[],"x":0}"#;
    let kind = br#"{"format":1,"javaVersion":"21","modules":[],"packages":[],"types":[{"kind":"alien"}],"references":[]}"#;
    let truncated = br#"{"format":1,"javaVersion":"21","modules":[],"packages":["#;
    assert!(matches!(
        decode(stale),
        Err(DecodeError::StaleSchema { found: 2, .. })
    ));
    assert!(matches!(
        decode(u64_stale),
        Err(DecodeError::StaleSchema {
            found: 4294967297,
            ..
        })
    ));
    assert!(
        matches!(decode(unknown), Err(DecodeError::UnknownField { ref field, .. }) if field == "x")
    );
    assert!(
        matches!(decode(kind), Err(DecodeError::UnknownKind { ref kind, .. }) if kind == "alien")
    );
    assert!(matches!(
        decode(truncated),
        Err(DecodeError::Truncated { .. })
    ));
    Ok(())
}

#[test]
fn bogus_override_is_typed_unavailable_before_work() -> Result<(), TestError> {
    let error = match probe_javadoc_path("/definitely/not-a-javadoc".into()) {
        Err(error) => error,
        Ok(path) => return Err(TestError::UnexpectedTool(path)),
    };
    assert_eq!(error.language, "java");
    assert_eq!(error.tool.to_string_lossy(), "/definitely/not-a-javadoc");
    Ok(())
}
