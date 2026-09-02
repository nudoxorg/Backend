//! Verifies quoted-annotation resolution through Ruff's expression authority.
//! A value Ruff parses is projected like a written annotation; a value Ruff
//! rejects stays an unsupported-syntax fact carrying the literal's span; and
//! the recursion guard reports truncation instead of guessing.

use compiler_languages_python::{
    Annotation, AnnotationSyntaxKind, DeclarationKind, TypeReason, extract,
};
use compiler_vocabulary::PythonVersion;

const PROFILE: PythonVersion = PythonVersion::Python314;

#[derive(Debug, thiserror::Error)]
enum TestError {
    #[error(transparent)]
    Extract(#[from] compiler_languages_python::ExtractionError),
    #[error("expected a resolved quoted annotation")]
    ExpectedResolved,
    #[error("expected an unsupported quoted annotation")]
    ExpectedUnsupported,
    #[error("expected the depth guard to truncate the quoted annotation")]
    ExpectedTruncation,
    #[error("expected an exact declaration name span")]
    ExpectedNameSpan,
}

fn field_annotation(source: &[u8]) -> Result<compiler_languages_python::Annotation, TestError> {
    let facts = extract(source, PROFILE)?;
    let declaration = facts
        .declarations
        .iter()
        .find(|declaration| declaration.kind == DeclarationKind::Field)
        .ok_or(TestError::ExpectedResolved)?;
    let owner = &declaration.name;
    let annotation = facts
        .annotations
        .iter()
        .find(|candidate| candidate.owner == *owner)
        .ok_or(TestError::ExpectedResolved)?;
    Ok(annotation.annotation.clone())
}

/// A quoted name that Ruff resolves closes the recursion honestly.
#[test]
fn quoted_name_resolves_to_a_name_annotation() -> Result<(), TestError> {
    let annotation = field_annotation(b"class Node:\n    next: \"Node\"\n")?;
    match annotation {
        Annotation::Name(name) if name == "Node" => Ok(()),
        _ => Err(TestError::ExpectedResolved),
    }
}

/// A quoted generic application resolves structurally like a written one.
#[test]
fn quoted_generic_resolves_to_a_generic_annotation() -> Result<(), TestError> {
    let annotation = field_annotation(b"class Box:\n    items: \"list[int]\"\n")?;
    match annotation {
        Annotation::Generic { base, args } => {
            if matches!(base.as_ref(), Annotation::Name(name) if name == "list")
                && matches!(args.as_slice(), [Annotation::Name(arg)] if arg == "int")
            {
                Ok(())
            } else {
                Err(TestError::ExpectedResolved)
            }
        }
        _ => Err(TestError::ExpectedResolved),
    }
}

/// A quoted value Ruff cannot parse stays an unsupported-syntax fact whose
/// span addresses the exact literal bytes.
#[test]
fn unparseable_quoted_annotation_stays_unsupported_with_its_span() -> Result<(), TestError> {
    let source = b"class Broken:\n    field: \"not a type(\"\n";
    let annotation = field_annotation(source)?;
    let Annotation::Unknown(TypeReason::UnsupportedSyntax {
        kind: AnnotationSyntaxKind::StringLiteral,
        span,
    }) = annotation
    else {
        return Err(TestError::ExpectedUnsupported);
    };
    let start = usize::try_from(span.start).map_err(|_| TestError::ExpectedUnsupported)?;
    let end = usize::try_from(span.end).map_err(|_| TestError::ExpectedUnsupported)?;
    if source.get(start..end) != Some(&b"\"not a type(\""[..]) {
        return Err(TestError::ExpectedUnsupported);
    }
    Ok(())
}

/// Quoted annotations that quote further annotations resolve recursively
/// until the depth guard reports the truncation.
#[test]
fn quoted_annotation_recursion_reports_truncation() -> Result<(), TestError> {
    // Each layer is a double-quoted string whose value is the previous
    // layer's source text, with its quotes and backslashes escaped.
    let mut nested = String::from("Node");
    for _ in 0..9 {
        let escaped = nested.replace('\\', "\\\\").replace('"', "\\\"");
        nested = format!("\"{escaped}\"");
    }
    let source = format!("class Node:\n    next: {nested}\n");
    let facts = extract(source.as_bytes(), PROFILE)?;
    let declaration = facts
        .declarations
        .iter()
        .find(|declaration| declaration.kind == DeclarationKind::Field)
        .ok_or(TestError::ExpectedTruncation)?;
    let owner = &declaration.name;
    let annotation = facts
        .annotations
        .iter()
        .find(|candidate| candidate.owner == *owner)
        .ok_or(TestError::ExpectedTruncation)?;
    if matches!(
        annotation.annotation,
        Annotation::Unknown(TypeReason::TruncatedAtDepthLimit)
    ) {
        Ok(())
    } else {
        Err(TestError::ExpectedTruncation)
    }
}

/// Every declaration and parameter carries an exact name span whose bytes
/// are the declared identifier.
#[test]
fn declarations_carry_exact_name_spans() -> Result<(), TestError> {
    let source = b"def scale(step: int) -> str:\n    return \"\"\n";
    let facts = extract(source, PROFILE)?;
    let function = facts
        .declarations
        .iter()
        .find(|declaration| declaration.name == "scale")
        .ok_or(TestError::ExpectedNameSpan)?;
    let slice = |span: compiler_languages_python::Span| {
        let start = usize::try_from(span.start).ok()?;
        let end = usize::try_from(span.end).ok()?;
        source.get(start..end)
    };
    if slice(function.name_span) != Some(&b"scale"[..]) {
        return Err(TestError::ExpectedNameSpan);
    }
    let parameter = &function.parameters[0];
    if slice(parameter.name_span) != Some(&b"step"[..])
        || slice(
            parameter
                .annotation_span
                .ok_or(TestError::ExpectedNameSpan)?,
        ) != Some(&b"int"[..])
    {
        return Err(TestError::ExpectedNameSpan);
    }
    Ok(())
}
