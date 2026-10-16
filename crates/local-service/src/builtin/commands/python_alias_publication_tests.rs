//! Supplied-report lowering through product publication and graph/reference queries.
//! Native inference is exercised separately in the engine's genuine Pyrefly test.

use super::*;
use backend_engine::driver::{
    CompileControl, CompileRequest, CompileScratch, DeclarationScope, ResolvedToolchain,
    SemanticAuthorityInput, ToolchainSelection, compile_ir,
};
use backend_engine::{RowId, SemanticConfidence, SemanticLinkKind, ViewRoot, package_key};
use backend_frontend_python::legacy::checker::DefinitionTarget;
use backend_frontend_python::legacy::{
    CheckerReport, DeclarationKind, Span, SymbolOutcome, SymbolResolution, extract,
};
use backend_semantic::ir::{
    DeclarationPublicationRole, ItemKind, SemanticImageView, encode_full_semantic_image,
    full_semantic_image_len,
};
use backend_semantic::vocabulary::{LanguageProfile, NativeTool, PackageUrl, PythonVersion, Stage};
use std::{
    path::Path,
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};

const PATH: &str = "pkg/client.py";
const SOURCE: &str = concat!(
    "# 🐍 captured source bytes\r\n",
    "from unavailable_publication_fixture import first as ｃｏｐｙ\r\n",
    "before = copy\r\n",
    "from unavailable_publication_fixture import second as copy\r\n",
    "after = copy\r\n",
    "from unavailable_publication_fixture import third as copy, fourth as copy\r\n",
    "final = copy\r\n",
);

fn supplied_image(source: &str, uses: &[(&str, usize)]) -> Result<Vec<u8>, String> {
    let syntax = extract(source.as_bytes(), PythonVersion::Python314)
        .map_err(|error| format!("{error:?}"))?;
    let sites = syntax
        .declarations
        .iter()
        .filter(|declaration| {
            declaration.kind == DeclarationKind::Alias && declaration.name == "copy"
        })
        .map(|declaration| declaration.name_span)
        .collect::<Vec<_>>();
    let symbols = uses
        .iter()
        .map(|(statement, site)| {
            let start = source.find(statement).ok_or("missing fixture use")? + statement.len() - 4;
            Ok(SymbolResolution {
                target: "copy".into(),
                span: Span {
                    start: start as u32,
                    end: start as u32 + 4,
                },
                outcome: SymbolOutcome::Definition {
                    target: DefinitionTarget {
                        relative_path: PATH.into(),
                        package: "fixture".into(),
                        name_span: *sites.get(*site).ok_or("missing fixture alias")?,
                        qualified_name: "fixture.client.copy".into(),
                        source_coordinate: "supplied-local-target".into(),
                        name: "copy".into(),
                        kind: DeclarationKind::Alias,
                        same_module: true,
                    },
                    callee: None,
                },
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let report = CheckerReport {
        symbols: symbols.into_boxed_slice(),
        ..CheckerReport::default()
    };
    let coordinate = PackageUrl::parse("pkg:pypi/fixture@1.0.0".to_owned())
        .map_err(|error| format!("{error:?}"))?;
    // The supplied-authority path is in-process. This fixture identity is not
    // a claim that /bin/true is CPython or that native inference was run here.
    let toolchain = ResolvedToolchain::from_version(
        NativeTool::Python,
        Path::new("/bin/true"),
        b"supplied-python-alias-test",
    )
    .map_err(|error| format!("{error:?}"))?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0; 4096];
    let work = tempfile::tempdir().map_err(|error| error.to_string())?;
    let compiled = compile_ir(
        CompileRequest {
            profile: LanguageProfile::Python(PythonVersion::Python314),
            stage: Stage::LowerIr,
            source: source.as_bytes(),
            declaration_scope: DeclarationScope::for_package(&coordinate, PATH)
                .map_err(|error| format!("{error:?}"))?,
            toolchain: ToolchainSelection::ResolvedNative(toolchain),
            authority: SemanticAuthorityInput::Python { report: &report },
            control: CompileControl {
                cancelled: &cancelled,
                deadline: Instant::now() + Duration::from_secs(30),
            },
        },
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: work.path(),
        },
    )
    .map_err(|error| format!("{error:?}"))?;
    let mut bytes =
        vec![0; full_semantic_image_len(&compiled.ir).map_err(|error| error.to_string())?];
    encode_full_semantic_image(&compiled.ir, &mut bytes).map_err(|error| error.to_string())?;
    Ok(bytes)
}

fn activated_view(bytes: &[u8]) -> Result<ViewRoot, String> {
    let image = SemanticImageView::reopen(bytes).map_err(|error| error.to_string())?;
    let (initial, _) = crate::builtin::initial_view().map_err(|error| error.to_string())?;
    let project = crate::builtin::IndexedProject {
        package: package_key("fixture"),
        label: "fixture".into(),
        files: Arc::from([]),
    };
    // Deliberately no structural presentation pairing: Python's legacy
    // same-line pairing is not exact captured binding identity. This test must
    // obtain its graph endpoints from the authored semantic rows alone.
    let rows = view_build::image_publication_rows_for_test(
        &image,
        &project,
        LanguageProfile::Python(PythonVersion::Python314),
        initial.basis(),
        PATH,
    )
    .map_err(|error| error.to_string())?;
    let capability =
        crate::builtin::test_builtin_view_capability().map_err(|error| error.to_string())?;
    ViewRoot::new_checked(
        initial.recipe(),
        initial.basis(),
        initial.frontier(),
        rows,
        vec![backend_engine::ViewCoverage::Complete],
        capability,
    )
    .map_err(|error| format!("{error:?}"))
}

fn assert_owned_alias_rows(
    image: &SemanticImageView<'_>,
    view: &ViewRoot,
    source: &str,
    count: usize,
) -> Result<Vec<backend_semantic::ir::SemanticEntity>, String> {
    let package = package_key("fixture");
    let aliases = image
        .canonical_entities()
        .filter(|entity| entity.kind == ItemKind::Alias)
        .collect::<Vec<_>>();
    assert_eq!(aliases.len(), count);
    let ids = aliases
        .iter()
        .map(|entity| {
            RowId::Symbol(view_build::semantic_symbol(
                package,
                entity.version.identity(),
            ))
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        ids.len(),
        count,
        "every authored alias has its own product identity"
    );
    for alias in &aliases {
        assert_eq!(
            alias.authority.publication,
            DeclarationPublicationRole::PrimaryOwned
        );
        let symbol = view_build::semantic_symbol(package, alias.version.identity());
        let row = view
            .row_ref(RowId::Symbol(symbol))
            .ok_or("authored alias absent from product view")?;
        assert!(
            row.source.captured().is_none(),
            "do not invent Python structural exact-site pairing"
        );
        assert_eq!(
            semantic_entity_for_symbol(image, package, symbol).map_err(|error| error.to_string())?,
            Some(alias.id)
        );
    }
    if source.contains("ｃｏｐｙ") {
        let first = view
            .row_ref(RowId::Symbol(view_build::semantic_symbol(
                package,
                aliases[0].version.identity(),
            )))
            .ok_or("normalized import row")?;
        assert!(
            first.label.ends_with("::ｃｏｐｙ"),
            "normalization must not replace captured source spelling"
        );
    }
    Ok(aliases)
}

fn assert_public_alias_targets(
    source: &str,
    uses: &[(&str, usize)],
    count: usize,
) -> Result<(), String> {
    let bytes = supplied_image(source, uses)?;
    let image = SemanticImageView::reopen(&bytes).map_err(|error| error.to_string())?;
    let view = activated_view(&bytes)?;
    let package = package_key("fixture");
    let aliases = assert_owned_alias_rows(&image, &view, source, count)?;
    let module = image
        .canonical_entities()
        .find(|entity| entity.kind == ItemKind::Module)
        .ok_or("module use owner")?;
    let module_symbol = view_build::semantic_symbol(package, module.version.identity());
    let paths = BTreeSet::from([PATH.to_owned()]);
    let edges = project_semantic_graph_relations_from_bytes(
        &[&bytes],
        &view,
        package,
        module_symbol,
        RowId::Symbol(module_symbol),
        false,
        &paths,
    )
    .map_err(|error| error.to_string())?;
    let expected = uses
        .iter()
        .map(|(_, index)| {
            RowId::Symbol(view_build::semantic_symbol(
                package,
                aliases[*index].version.identity(),
            ))
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        edges
            .iter()
            .filter(|edge| edge.relation == SemanticLinkKind::Reads)
            .map(|edge| edge.to)
            .collect::<BTreeSet<_>>(),
        expected
    );
    for (statement, index) in uses {
        let alias = &aliases[*index];
        let target = view_build::semantic_symbol(package, alias.version.identity());
        let facts =
            project_reference_facts_from_bytes(&[&bytes], &view, package, target, &paths, &[])
                .map_err(|error| error.to_string())?;
        assert!(
            !facts.is_empty(),
            "published later alias must have native-source references"
        );
        let start = source.find(statement).ok_or("use")? + statement.len() - 4;
        assert!(facts.iter().any(|fact| fact.site == module_symbol
            && fact.relation == SemanticLinkKind::Reads
            && fact.evidence.confidence == SemanticConfidence::Compiler
            && matches!(&fact.target, backend_engine::SemanticLinkTarget::Local { declaration }
                if *declaration == semantic_declaration_identity(alias.version.identity()))
            && fact.evidence.source.as_ref().is_some_and(
                |span| span.start as usize == start && span.end as usize == start + 4
            )));
    }
    Ok(())
}

#[test]
fn authored_python_aliases_remain_public_query_graph_and_reference_targets() -> Result<(), String> {
    let single = "from unavailable_publication_fixture import first as copy\nbefore = copy\n";
    assert_public_alias_targets(single, &[("before = copy", 0)], 1)?;
    assert_public_alias_targets(
        SOURCE,
        &[
            ("before = copy", 0),
            ("after = copy", 1),
            ("final = copy", 3),
        ],
        4,
    )
}
