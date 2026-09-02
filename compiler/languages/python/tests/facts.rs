//! Extraction laws for the Python frontend.
//! Each test names one source-preservation or omission law.
//! Integration tests consume only the public extraction entry point.

use compiler_languages_python::{extract as extract_with_profile, *};
use compiler_vocabulary::PythonVersion;

const PROFILE: PythonVersion = PythonVersion::Python314;

#[derive(Debug, thiserror::Error)]
enum TestError {
    #[error(transparent)]
    Extract(#[from] ExtractionError),
    #[error("missing declaration `{0}")]
    MissingDeclaration(&'static str),
}

fn extract(source: &[u8]) -> Result<ModuleFacts, ExtractionError> {
    extract_with_profile(source, PROFILE)
}

#[test]
fn extracts_typed_core_facts() -> Result<(), TestError> {
    let facts = extract(
        b"\"module\"\n\ndef f(a: int, /, b=2, *args, c: str = 'x', **kwargs):\n    return f(a)\n",
    )?;
    assert_eq!(
        facts.docstring.as_ref().map(|d| d.raw.as_str()),
        Some("\"module\"")
    );
    let function = facts
        .declarations
        .iter()
        .find(|fact| fact.name == "f")
        .ok_or(TestError::MissingDeclaration("f"))?;
    assert_eq!(function.parameters[0].kind, ParameterKind::PositionalOnly);
    assert_eq!(function.parameters[1].default_source.as_deref(), Some("2"));
    assert_eq!(function.parameters[2].kind, ParameterKind::VarArgs);
    assert_eq!(function.parameters[3].kind, ParameterKind::KeywordOnly);
    assert_eq!(function.parameters[4].kind, ParameterKind::KwArgs);
    assert!(facts.occurrences.iter().any(|o| o.target == "f"));
    Ok(())
}

#[test]
fn dynamic_calls_are_omitted() -> Result<(), TestError> {
    let facts = extract(b"def f():\n    getattr(self, 'f')()\n")?;
    assert!(facts.occurrences.is_empty());
    Ok(())
}

#[test]
fn declarations_keep_class_forms_aliases_decorators_and_docs() -> Result<(), TestError> {
    let source = b"\"module doc\"\nimport x as y\nfrom pkg import item as local\n\n@dataclass\nclass Data: \n    value: int = 1\n\n@staticmethod\ndef helper():\n    \"helper doc\"\n\nclass Proto(Protocol):\n    pass\nclass Typed(TypedDict):\n    pass\nclass Choice(enum.Enum):\n    A = 1\n";
    let facts = extract(source)?;
    let by_name = |name: &str| facts.declarations.iter().find(|d| d.name == name);
    assert_eq!(
        by_name("__main__").map(|d| d.kind),
        Some(DeclarationKind::Module)
    );
    assert_eq!(by_name("y").map(|d| d.kind), Some(DeclarationKind::Alias));
    assert_eq!(
        by_name("local").map(|d| d.kind),
        Some(DeclarationKind::Alias)
    );
    assert_eq!(
        by_name("Data").and_then(|d| d.class_form),
        Some(ClassForm::Dataclass)
    );
    assert_eq!(
        by_name("Proto").and_then(|d| d.class_form),
        Some(ClassForm::Protocol)
    );
    assert_eq!(
        by_name("Typed").and_then(|d| d.class_form),
        Some(ClassForm::TypedDict)
    );
    assert_eq!(
        by_name("Choice").and_then(|d| d.class_form),
        Some(ClassForm::Enum)
    );
    assert_eq!(
        by_name("value").map(|d| d.kind),
        Some(DeclarationKind::Field)
    );
    assert_eq!(
        by_name("helper").map(|d| d.decorators.as_slice()),
        Some(["staticmethod".to_owned()].as_slice())
    );
    assert_eq!(
        by_name("helper")
            .and_then(|d| d.docstring.as_ref())
            .map(|d| d.raw.as_str()),
        Some("\"helper doc\"")
    );
    Ok(())
}

#[test]
fn written_param_spec_and_type_var_tuple_are_preserved() -> Result<(), TestError> {
    let facts = extract(b"def f(p: ParamSpec, t: TypeVarTuple):\n    pass\n")?;
    let function = facts
        .declarations
        .iter()
        .find(|fact| fact.name == "f")
        .ok_or(TestError::MissingDeclaration("f"))?;
    assert_eq!(
        function.parameters[0].annotation,
        Annotation::Name("ParamSpec".to_owned())
    );
    assert_eq!(
        function.parameters[1].annotation,
        Annotation::Name("TypeVarTuple".to_owned())
    );
    Ok(())
}

#[test]
fn spans_are_utf8_bytes_not_utf16_units() -> Result<(), TestError> {
    let facts = extract("# CJK 注释\nvalue = \"😀\"\ndef héllo():\n    pass\n".as_bytes())?;
    let hello = facts
        .declarations
        .iter()
        .find(|fact| fact.name == "héllo")
        .ok_or(TestError::MissingDeclaration("héllo"))?;
    assert_eq!(hello.span, Span { start: 28, end: 41 });
    assert_ne!(hello.span.end - hello.span.start, 12);
    Ok(())
}

#[test]
fn module_assignments_are_constants_with_source_and_annotation() -> Result<(), TestError> {
    let facts = extract(b"X = 5\nY: int = 6\n")?;
    let x = facts.declarations.iter().find(|d| d.name == "X");
    let y = facts.declarations.iter().find(|d| d.name == "Y");
    assert_eq!(x.map(|d| d.kind), Some(DeclarationKind::Constant));
    assert_eq!(x.and_then(|d| d.value_source.as_deref()), Some("5"));
    assert_eq!(y.map(|d| d.kind), Some(DeclarationKind::Constant));
    assert_eq!(y.and_then(|d| d.value_source.as_deref()), Some("6"));
    assert_eq!(
        facts
            .annotations
            .iter()
            .find(|a| a.owner == "Y")
            .map(|a| &a.annotation),
        Some(&Annotation::Name("int".to_owned()))
    );
    Ok(())
}

#[test]
fn class_decorator_matching_is_exact_and_calls_belong_to_module() -> Result<(), TestError> {
    let json = extract(b"@dataclass_json\nclass Json: pass\n")?;
    assert_eq!(
        json.declarations
            .iter()
            .find(|d| d.name == "Json")
            .and_then(|d| d.class_form),
        Some(ClassForm::Plain)
    );
    let dotted = extract(b"@dataclasses.dataclass\nclass Data: pass\n")?;
    assert_eq!(
        dotted
            .declarations
            .iter()
            .find(|d| d.name == "Data")
            .and_then(|d| d.class_form),
        Some(ClassForm::Dataclass)
    );
    let called = extract(b"@dataclass()\nclass Called: pass\n")?;
    assert_eq!(
        called
            .declarations
            .iter()
            .find(|d| d.name == "Called")
            .and_then(|d| d.class_form),
        Some(ClassForm::Dataclass)
    );
    let owned = extract(b"@decorate()\ndef handler(): pass\ndef decorate(): pass\n")?;
    assert_eq!(
        owned
            .occurrences
            .iter()
            .find(|o| o.target == "decorate")
            .map(|o| o.owner.as_str()),
        Some("__main__")
    );
    Ok(())
}

#[test]
fn nested_main_named_function_does_not_enable_module_imports() -> Result<(), TestError> {
    let facts = extract(b"def __main__():\n    import sys\n")?;
    assert!(
        facts
            .declarations
            .iter()
            .all(|d| d.kind != DeclarationKind::Alias)
    );
    Ok(())
}

#[test]
fn unannotated_parameter_has_unavailable_authority() -> Result<(), TestError> {
    let facts = extract(b"def f(value):\n    pass\n")?;
    let f = facts
        .declarations
        .iter()
        .find(|d| d.name == "f")
        .ok_or(TestError::MissingDeclaration("f"))?;
    assert!(matches!(
        f.parameters[0].annotation,
        Annotation::Unknown(TypeReason::Unannotated {
            position: AnnotationPosition::Parameter
        })
    ));
    Ok(())
}

#[test]
fn invalid_utf8_retains_operands() {
    assert!(
        matches!(extract(b"x\xff"), Err(ExtractionError::InvalidUtf8 { bytes, .. }) if bytes == vec![0xff])
    );
}

#[test]
fn truncation_faults_retain_variant_operands() {
    for source in [
        b"x = 'unterminated".as_slice(),
        b"'''unterminated".as_slice(),
        b"@decorator(\n".as_slice(),
    ] {
        assert!(
            matches!(extract(source), Err(ExtractionError::RejectedSyntax { rejection }) if !rejection.parsed.errors().is_empty())
        );
    }
}
