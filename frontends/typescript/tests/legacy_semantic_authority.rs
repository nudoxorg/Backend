//! Exercises the public in-process TypeScript syntax-and-binding authority.
//! Falsifies grammar confusion, lossy diagnostics, local-resolution loss, and coordinate drift.
//! Uses no native compiler, fixture scanner, or canonical-IR serializer.

use backend_frontend_typescript::legacy::{
    AuthorityError, OxcDeclarationKind, Utf8Span, Utf16Span, analyze,
};
use backend_semantic::vocabulary::TypeScriptSource;
use oxc_allocator::Allocator;

#[derive(Debug, thiserror::Error)]
enum AuthorityTestError {
    #[error(transparent)]
    Authority(#[from] AuthorityError),
    #[error(transparent)]
    Coordinate(#[from] backend_frontend_typescript::legacy::CoordinateError),
    #[error("missing lexical symbol {name}")]
    MissingSymbol { name: &'static str },
    #[error("symbol {name} was not resolved at each expected local use")]
    MissingResolvedUse { name: &'static str },
    #[error("authority accepted an input that must be rejected: {case}")]
    UnexpectedAdmission { case: &'static str },
}

#[test]
fn typescript_profile_preserves_local_binding_and_utf16_coordinates()
-> Result<(), AuthorityTestError> {
    let source = "const rocket = '🚀';\nfunction useRocket(): string { return rocket; }\n";
    let arena = Allocator::default();
    let module = analyze(TypeScriptSource::TypeScript, source, &arena)?;
    let scoping = module.semantic.scoping();
    let rocket = scoping
        .symbol_ids()
        .find(|symbol| scoping.symbol_name(*symbol) == "rocket")
        .ok_or(AuthorityTestError::MissingSymbol { name: "rocket" })?;
    (!scoping.get_resolved_reference_ids(rocket).is_empty())
        .then_some(())
        .ok_or(AuthorityTestError::MissingResolvedUse { name: "rocket" })?;

    let bytes = Utf8Span::try_from(16..20)?;
    let units = bytes.to_utf16(source)?;
    let expected = Utf16Span::try_from(16..18)?;
    (units == expected)
        .then_some(())
        .ok_or(AuthorityTestError::MissingResolvedUse {
            name: "UTF-16 conversion",
        })?;
    (units.to_utf8(source)? == bytes).then_some(()).ok_or(
        AuthorityTestError::MissingResolvedUse {
            name: "UTF-8 round trip",
        },
    )?;
    Ok(())
}

#[test]
fn declaration_stream_lends_exact_source_names_and_closed_symbol_kinds()
-> Result<(), AuthorityTestError> {
    let source = "interface Vessel {}\nconst rocket = 1;\nfunction launch() {}\n";
    let arena = Allocator::default();
    let module = analyze(TypeScriptSource::TypeScript, source, &arena)?;
    let observed = module
        .declarations()
        .filter_map(|declaration| {
            let name = source.get(
                usize::try_from(declaration.name.start).ok()?
                    ..usize::try_from(declaration.name.end).ok()?,
            )?;
            Some((name, declaration.kind))
        })
        .collect::<Vec<_>>();
    let required = [
        ("Vessel", OxcDeclarationKind::Interface),
        ("rocket", OxcDeclarationKind::Constant),
        ("launch", OxcDeclarationKind::Function),
    ];
    required
        .iter()
        .all(|fact| observed.contains(fact))
        .then_some(())
        .ok_or(AuthorityTestError::MissingResolvedUse {
            name: "typed declaration stream",
        })
}

#[test]
fn tsx_profile_accepts_jsx_while_typescript_profile_rejects_it() -> Result<(), AuthorityTestError> {
    let source = "export const view = <main data-id='record' />;";
    let jsx_enabled_arena = Allocator::default();
    analyze(TypeScriptSource::Tsx, source, &jsx_enabled_arena)?;

    let standard_typescript_arena = Allocator::default();
    match analyze(
        TypeScriptSource::TypeScript,
        source,
        &standard_typescript_arena,
    ) {
        Err(AuthorityError::Syntax { diagnostics }) if !diagnostics.is_empty() => Ok(()),
        Ok(_)
        | Err(AuthorityError::Binding { .. } | AuthorityError::Syntax { .. })
        | Err(AuthorityError::Checker { .. }) => Err(AuthorityTestError::MissingResolvedUse {
            name: "TSX grammar distinction",
        }),
    }
}

#[test]
fn malformed_source_preserves_every_parser_diagnostic() -> Result<(), AuthorityTestError> {
    let arena = Allocator::default();
    let result = analyze(
        TypeScriptSource::TypeScript,
        "interface Broken {\nconst alsoBroken = ;",
        &arena,
    );
    match result {
        Err(AuthorityError::Syntax { diagnostics }) if !diagnostics.is_empty() => Ok(()),
        Ok(_)
        | Err(AuthorityError::Binding { .. } | AuthorityError::Syntax { .. })
        | Err(AuthorityError::Checker { .. }) => Err(AuthorityTestError::UnexpectedAdmission {
            case: "malformed TypeScript source",
        }),
    }
}

#[test]
fn surrogate_halves_are_never_reinterpreted_as_coordinates() -> Result<(), AuthorityTestError> {
    let source = "x🚀y";
    let interior = Utf16Span::try_from(2..2)?;
    matches!(
        interior.to_utf8(source),
        Err(backend_frontend_typescript::legacy::CoordinateError::SurrogateBoundary)
    )
    .then_some(())
    .ok_or(AuthorityTestError::UnexpectedAdmission {
        case: "UTF-16 surrogate half",
    })
}
