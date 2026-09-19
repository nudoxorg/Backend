//! Executable floor for source behavior carried by the deleted language oracles.

use backend_compile::{DeclarationKind, SourceAnalysis, SyntaxFrontend};
use std::error::Error;
use std::path::Path;

type FrontendFactory = fn() -> Result<SyntaxFrontend, backend_compile::SyntaxError>;

struct OracleFixture {
    path: &'static str,
    source: &'static [u8],
    frontend: FrontendFactory,
    declarations: &'static [(&'static str, DeclarationKind)],
}

const FIXTURES: &[OracleFixture] = &[
    OracleFixture {
        path: "parity.rs",
        source: include_bytes!("../fixtures/parity/rust.rs"),
        frontend: backend_frontend_rust::syntax_frontend,
        declarations: &[
            ("Service", DeclarationKind::Trait),
            ("Worker", DeclarationKind::Struct),
            ("Event", DeclarationKind::Enum),
            ("execute", DeclarationKind::Function),
        ],
    },
    OracleFixture {
        path: "parity.py",
        source: include_bytes!("../fixtures/parity/python.py"),
        frontend: backend_frontend_python::syntax_frontend,
        declarations: &[
            ("Service", DeclarationKind::Class),
            ("Worker", DeclarationKind::Class),
            ("execute", DeclarationKind::Function),
        ],
    },
    OracleFixture {
        path: "parity.ts",
        source: include_bytes!("../fixtures/parity/typescript.ts"),
        frontend: backend_frontend_typescript::syntax_frontend,
        declarations: &[
            ("Service", DeclarationKind::Interface),
            ("Worker", DeclarationKind::Class),
            ("execute", DeclarationKind::Function),
        ],
    },
    OracleFixture {
        path: "parity.go",
        source: include_bytes!("../fixtures/parity/go.go"),
        frontend: backend_frontend_go::syntax_frontend,
        declarations: &[
            ("Service", DeclarationKind::Type),
            ("Worker", DeclarationKind::Type),
            ("Execute", DeclarationKind::Function),
        ],
    },
    OracleFixture {
        path: "parity.java",
        source: include_bytes!("../fixtures/parity/java.java"),
        frontend: backend_frontend_java::syntax_frontend,
        declarations: &[
            ("Service", DeclarationKind::Interface),
            ("Worker", DeclarationKind::Class),
        ],
    },
    OracleFixture {
        path: "parity.cs",
        source: include_bytes!("../fixtures/parity/csharp.cs"),
        frontend: backend_frontend_csharp::syntax_frontend,
        declarations: &[
            ("IService", DeclarationKind::Interface),
            ("Worker", DeclarationKind::Class),
        ],
    },
    OracleFixture {
        path: "parity.cpp",
        source: include_bytes!("../fixtures/parity/clang.cpp"),
        frontend: backend_frontend_clang::syntax_frontend,
        declarations: &[
            ("Service", DeclarationKind::Struct),
            ("Worker", DeclarationKind::Struct),
        ],
    },
];

#[test]
fn all_language_fallbacks_retain_the_normalized_declaration_floor() -> Result<(), Box<dyn Error>> {
    for fixture in FIXTURES {
        let frontend = (fixture.frontend)()?;
        let analysis = frontend.analyze(Path::new(fixture.path), fixture.source)?;
        assert_required_declarations(fixture, &analysis)?;
    }
    Ok(())
}

fn assert_required_declarations(
    fixture: &OracleFixture,
    analysis: &SourceAnalysis,
) -> Result<(), Box<dyn Error>> {
    for &(name, kind) in fixture.declarations {
        let declaration = analysis
            .declarations()
            .iter()
            .find(|declaration| declaration.name() == name)
            .ok_or_else(|| {
                std::io::Error::other(format!(
                    "{} omitted {name}: {:?}",
                    fixture.path,
                    analysis.declarations()
                ))
            })?;
        assert_eq!(
            declaration.kind(),
            kind,
            "{} classified {name}",
            fixture.path
        );
        assert_eq!(declaration.location().path(), fixture.path);
        assert!(
            !declaration.signature().is_empty(),
            "{} erased {name}'s signature",
            fixture.path
        );
        assert!(
            declaration.source_excerpt().text().is_some(),
            "{} erased {name}'s source excerpt",
            fixture.path
        );
    }
    Ok(())
}

#[test]
fn syntax_fallback_never_claims_cross_file_semantic_facts() -> Result<(), Box<dyn Error>> {
    for fixture in FIXTURES {
        let frontend = (fixture.frontend)()?;
        let analysis = frontend.analyze(Path::new(fixture.path), fixture.source)?;
        assert!(
            analysis.declarations().iter().all(|declaration| {
                !declaration.signature().contains("resolved-target=")
                    && !declaration.signature().contains("inferred-type=")
            }),
            "syntax fallback fabricated semantic authority for {}",
            fixture.path
        );
    }
    Ok(())
}
