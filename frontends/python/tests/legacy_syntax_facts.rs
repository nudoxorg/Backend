//! Verifies Ruff-backed Python syntax facts through the public extraction boundary.
//! Keeps grammar selection explicit and checks byte-accurate, non-semantic output.
//! Does not pretend syntax facts are resolved Python types or references.

use backend_frontend_python::legacy::{
    Annotation, AnnotationPosition, AnnotationSyntaxKind, ClassForm, DeclarationFact,
    DeclarationKind, ExtractionError, LiteralValue, ModuleFacts, ParameterKind, Span, TypeReason,
    extract,
};
use backend_semantic::vocabulary::PythonVersion;
use ruff_python_parser::UnsupportedSyntaxErrorKind;

const PROFILE: PythonVersion = PythonVersion::Python314;

#[derive(Debug, thiserror::Error)]
enum TestError {
    #[error(transparent)]
    Extract(#[from] ExtractionError),
    #[error("missing declaration `{0}`")]
    MissingDeclaration(&'static str),
    #[error("expected the selected Python profile to reject unsupported syntax")]
    ExpectedProfileRejection,
    #[error("expected a generic annotation")]
    ExpectedGeneric,
    #[error("expected a typed literal annotation")]
    ExpectedLiteral,
}

fn declaration<'facts>(
    facts: &'facts ModuleFacts,
    name: &'static str,
) -> Result<&'facts DeclarationFact, TestError> {
    facts
        .declarations
        .iter()
        .find(|fact| fact.name == name)
        .ok_or(TestError::MissingDeclaration(name))
}

#[test]
fn ruff_preserves_declarations_signatures_docs_and_utf8_byte_spans() -> Result<(), TestError> {
    let source = r###""""module docs"""
from package import Item as Imported

@dataclasses.dataclass
class Café(Protocol):
    """class docs"""
    name: str

    @property
    def title(self, /, value: list[str] | None = None) -> str:
        """method docs"""
        return Imported(value)
"###
    .as_bytes();
    let facts = extract(source, PROFILE)?;
    let class = declaration(&facts, "Café")?;
    let method = declaration(&facts, "title")?;
    assert_eq!(class.kind, DeclarationKind::Class);
    assert_eq!(class.class_form, Some(ClassForm::Dataclass));
    assert!(class.docstring.is_some());
    assert_eq!(method.kind, DeclarationKind::Function);
    assert!(
        method
            .decorators
            .iter()
            .any(|decorator| decorator == "property")
    );
    assert!(!method.is_async);
    assert_eq!(method.parameters[0].kind, ParameterKind::PositionalOnly);
    assert!(facts.annotations.iter().any(|annotation| {
        annotation.owner == "title" && annotation.position == AnnotationPosition::Parameter
    }));
    assert!(
        facts
            .occurrences
            .iter()
            .any(|occurrence| occurrence.target == "Imported")
    );
    assert!(class.span.start < class.span.end);
    assert_ne!(class.span, Span { start: 0, end: 0 });
    Ok(())
}

#[test]
fn unannotated_values_stay_explicitly_unresolved_without_a_type_authority() -> Result<(), TestError>
{
    let facts = extract(b"def inferred(value):\n    return value\n", PROFILE)?;
    let function = declaration(&facts, "inferred")?;
    assert!(matches!(
        function.parameters[0].annotation,
        Annotation::Unknown(TypeReason::Unannotated {
            position: AnnotationPosition::Parameter
        })
    ));
    Ok(())
}

#[test]
fn selected_profile_controls_grammar_acceptance() {
    let source = b"type Alias = int\n";
    assert!(matches!(
        extract(source, PythonVersion::Python311),
        Err(ExtractionError::RejectedSyntax {
            rejection,
            ..
        }) if rejection.profile == PythonVersion::Python311
            && rejection.parsed.unsupported_syntax_errors().iter().any(|error| error.kind == UnsupportedSyntaxErrorKind::TypeAliasStatement)
    ));
    assert!(extract(source, PythonVersion::Python312).is_ok());
}

#[test]
fn selected_profile_retains_every_version_diagnostic() -> Result<(), TestError> {
    let source = b"type Alias = int\nclass Generic[T]: pass\n";
    match extract(source, PythonVersion::Python311) {
        Err(ExtractionError::RejectedSyntax { rejection }) => {
            assert_eq!(rejection.profile, PythonVersion::Python311);
            let errors = rejection.parsed.unsupported_syntax_errors();
            assert_eq!(errors.len(), 2);
            assert!(
                errors
                    .iter()
                    .any(|error| error.kind == UnsupportedSyntaxErrorKind::TypeAliasStatement)
            );
            assert!(
                errors
                    .iter()
                    .any(|error| error.kind == UnsupportedSyntaxErrorKind::TypeParameterList)
            );
        }
        _ => return Err(TestError::ExpectedProfileRejection),
    }
    Ok(())
}

#[test]
fn ruff_annotation_nodes_preserve_nested_generic_literal_and_unsupported_shapes()
-> Result<(), TestError> {
    let source = b"def f(value: dict[str, list[int | None]], mode: Literal['fast', 7, True, None], bad: 1 + 2):\n    pass\n";
    let facts = extract(source, PROFILE)?;
    let function = declaration(&facts, "f")?;

    let Annotation::Generic { base, args } = &function.parameters[0].annotation else {
        return Err(TestError::ExpectedGeneric);
    };
    assert!(matches!(base.as_ref(), Annotation::Name { name, span: Some(_) } if name == "dict"));
    assert!(
        matches!(args.as_slice(), [Annotation::Name { name, span: Some(_) }, Annotation::Generic { .. }] if name == "str")
    );

    let Annotation::Literal(values) = &function.parameters[1].annotation else {
        return Err(TestError::ExpectedLiteral);
    };
    assert_eq!(
        values,
        &[
            LiteralValue::String("fast".to_owned()),
            LiteralValue::Integer("7".to_owned()),
            LiteralValue::Boolean(true),
            LiteralValue::None,
        ]
    );

    assert!(matches!(
        function.parameters[2].annotation,
        Annotation::Unknown(TypeReason::UnsupportedSyntax {
            kind: AnnotationSyntaxKind::BinaryOperator,
            ..
        })
    ));
    Ok(())
}
