//! Exercises the public in-process TypeScript syntax-and-binding authority.
//! Falsifies grammar confusion, lossy diagnostics, local-resolution loss, and coordinate drift.
//! Uses no native compiler, fixture scanner, or canonical-IR serializer.

use backend_frontend_typescript::legacy::{
    AuthorityError, OxcDeclarationKind, Utf8Span, Utf16Span, analyze, analyze_with_declaration,
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
fn authored_javascript_package_entry_is_admitted_by_lexical_authority() {
    let source = "export class JavaScriptBeacon {\n  constructor(intensity) { this.intensity = intensity; }\n  illuminate() { return this.intensity; }\n}\n/** ORACLE/JAVASCRIPT: signal level thirty-four. */\nexport function JavaScriptBeaconEntry() { return new JavaScriptBeacon(34).illuminate(); }\n";
    let arena = Allocator::default();
    let module = analyze(TypeScriptSource::TypeScript, source, &arena)
        .unwrap_or_else(|cause| panic!("authored JavaScript fixture rejected: {cause:?}"));
    let declarations = module
        .declarations()
        .filter_map(|declaration| {
            let start = usize::try_from(declaration.name.start).ok()?;
            let end = usize::try_from(declaration.name.end).ok()?;
            source.get(start..end)
        })
        .collect::<Vec<_>>();
    assert!(declarations.contains(&"JavaScriptBeacon"));
    assert!(declarations.contains(&"JavaScriptBeaconEntry"));
}

#[test]
fn declaration_grammar_admits_ambient_unimplemented_members() -> Result<(), AuthorityTestError> {
    // A declaration-file class legitimately carries constructor and
    // overload signatures with no implementation. The value grammar reports
    // implementation-presence errors; the declaration grammar must not.
    let source = "export class Headers {\n  constructor(headers?: string);\n  concat(...values: string[]): Headers;\n  static concat(...values: string[]): Headers;\n}\n";
    let value_arena = Allocator::default();
    match analyze(TypeScriptSource::TypeScript, source, &value_arena) {
        Err(AuthorityError::Binding { diagnostics }) if !diagnostics.is_empty() => {}
        Ok(_)
        | Err(AuthorityError::Syntax { .. } | AuthorityError::Binding { .. })
        | Err(AuthorityError::Checker { .. }) => {
            return Err(AuthorityTestError::UnexpectedAdmission {
                case: "ambient members under the value grammar",
            });
        }
    }

    let declaration_arena = Allocator::default();
    analyze_with_declaration(
        TypeScriptSource::TypeScript,
        source,
        true,
        &declaration_arena,
    )
    .map(|_| ())
    .map_err(AuthorityTestError::Authority)
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
