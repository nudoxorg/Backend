//! Supplied-report lowering through product publication and graph/reference queries.
//! Native inference is exercised separately in the engine's genuine Pyrefly test.

use super::*;
use backend_engine::driver::{
    CompileControl, CompileRequest, CompileScratch, DeclarationScope, ResolvedToolchain,
    SemanticAuthorityInput, ToolchainSelection, compile_ir,
};
use backend_engine::{RowId, SemanticConfidence, ViewRoot, package_key};
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
    "before = [copy]\r\n",
    "from unavailable_publication_fixture import second as copy\r\n",
    "after = [copy]\r\n",
    "from unavailable_publication_fixture import third as copy, fourth as copy\r\n",
    "final = [copy]\r\n",
);

struct SuppliedImage {
    bytes: Vec<u8>,
    targets: Vec<DeclarationIdentity>,
}

fn supplied_image(
    source: &str,
    uses: &[(&str, usize)],
    relation: LinkKind,
) -> Result<SuppliedImage, String> {
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
            let span = Span {
                start: start as u32,
                end: start as u32 + 4,
            };
            assert!(syntax.occurrences.iter().any(|occurrence| {
                occurrence.span == span
                    && occurrence.kind == backend_frontend_python::legacy::OccurrenceKind::NameRead
                    && occurrence.owner == syntax.identity
            }));
            assert_eq!(
                syntax.annotations.iter().any(|annotation| {
                    annotation.span.start <= span.start && annotation.span.end >= span.end
                }),
                relation == LinkKind::TypeReference,
                "bare-name alias assignments and runtime list reads have different existing classification"
            );
            Ok(SymbolResolution {
                target: "copy".into(),
                span,
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
    let targets = owned_use_targets(&compiled.ir, &syntax, &report, uses, &sites, relation)?;
    let mut bytes =
        vec![0; full_semantic_image_len(&compiled.ir).map_err(|error| error.to_string())?];
    encode_full_semantic_image(&compiled.ir, &mut bytes).map_err(|error| error.to_string())?;
    Ok(SuppliedImage { bytes, targets })
}

fn owned_use_targets(
    ir: &backend_semantic::ir::Ir,
    syntax: &backend_frontend_python::legacy::ModuleFacts,
    report: &CheckerReport,
    uses: &[(&str, usize)],
    sites: &[Span],
    relation: LinkKind,
) -> Result<Vec<DeclarationIdentity>, String> {
    // Retain exact owned-IR targets before encoding. Canonical image order is
    // identity order, not Ruff/source order, so indexing reopened aliases by
    // the report's source-site index would silently choose a different row.
    let mut targets = Vec::with_capacity(report.symbols.len());
    for (symbol, (_, site)) in report.symbols.iter().zip(uses) {
        let occurrences = ir
            .link_occurrences()
            .filter_map(|(_, occurrence)| {
                occurrence
                    .source
                    .filter(|source| {
                        source.start() == symbol.span.start && source.end() == symbol.span.end
                    })
                    .map(|_| occurrence)
            })
            .collect::<Vec<_>>();
        assert_eq!(occurrences.len(), 1, "one exact owned use");
        let link = ir.link(occurrences[0].link).ok_or("owned use link")?;
        assert_eq!(link.kind, relation);
        assert_eq!(
            ir.entity(link.from).ok_or("owned use owner")?.kind,
            ItemKind::Module,
            "the captured top-level use is owned by its module"
        );
        assert_eq!(
            occurrences[0].confidence,
            backend_semantic::ir::Confidence::Compiler
        );
        let LinkTarget::Local(target) = link.target else {
            return Err("supplied exact definition lost its local target".into());
        };
        let alias = ir.entity(target).ok_or("owned alias target")?;
        assert_eq!(alias.kind, ItemKind::Alias);
        assert_eq!(
            alias.authority.publication,
            DeclarationPublicationRole::PrimaryOwned
        );
        let declaration = syntax
            .declarations
            .iter()
            .find(|declaration| declaration.name_span == sites[*site])
            .ok_or("supplied target declaration")?;
        let captured = alias.source.ok_or("owned alias source extent")?;
        assert_eq!(
            (captured.start(), captured.end()),
            (declaration.span.start, declaration.span.end)
        );
        targets.push(alias.version.identity());
    }
    Ok(targets)
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
        assert!(
            aliases.iter().any(|alias| view
                .row_ref(RowId::Symbol(view_build::semantic_symbol(
                    package,
                    alias.version.identity()
                )))
                .is_some_and(|row| row.label.ends_with("::ｃｏｐｙ"))),
            "normalization must not replace captured source spelling"
        );
    }
    Ok(aliases)
}

fn assert_public_alias_targets(
    source: &str,
    uses: &[(&str, usize)],
    count: usize,
    relation: LinkKind,
) -> Result<(), String> {
    let supplied = supplied_image(source, uses, relation)?;
    let bytes = supplied.bytes;
    let image = SemanticImageView::reopen(&bytes).map_err(|error| error.to_string())?;
    let view = activated_view(&bytes)?;
    let package = package_key("fixture");
    let aliases = assert_owned_alias_rows(&image, &view, source, count)?;
    assert!(
        supplied.targets.iter().all(|identity| aliases
            .iter()
            .any(|alias| alias.version.identity() == *identity)),
        "every exact owned target survives canonical image admission"
    );
    let module = image
        .canonical_entities()
        .find(|entity| entity.kind == ItemKind::Module)
        .ok_or("module use owner")?;
    let module_symbol = view_build::semantic_symbol(package, module.version.identity());
    assert!(view.row_ref(RowId::Symbol(module_symbol)).is_some());
    assert_eq!(
        semantic_entity_for_symbol(&image, package, module_symbol)
            .map_err(|error| error.to_string())?,
        Some(module.id),
        "the actual use owner remains admitted by publication lookup"
    );
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
    let expected = supplied
        .targets
        .iter()
        .map(|identity| RowId::Symbol(view_build::semantic_symbol(package, *identity)))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        expected.len(),
        uses.len(),
        "distinct supplied binding sites stay distinct"
    );
    assert_eq!(
        edges
            .iter()
            .filter(|edge| edge.relation == semantic_link_kind(relation))
            .map(|edge| edge.to)
            .collect::<BTreeSet<_>>(),
        expected
    );
    for ((statement, _), identity) in uses.iter().zip(&supplied.targets) {
        let target = view_build::semantic_symbol(package, *identity);
        let facts =
            project_reference_facts_from_bytes(&[&bytes], &view, package, target, &paths, &[])
                .map_err(|error| error.to_string())?;
        assert!(
            !facts.is_empty(),
            "published later alias must have native-source references"
        );
        let start = source.find(statement).ok_or("use")? + statement.len() - 4;
        assert!(facts.iter().any(|fact| fact.site == module_symbol
            && fact.relation == semantic_link_kind(relation)
            && fact.evidence.confidence == SemanticConfidence::Compiler
            && matches!(&fact.target, backend_engine::SemanticLinkTarget::Local { declaration }
                if *declaration == semantic_declaration_identity(*identity))
            && fact.evidence.source.as_ref().is_some_and(
                |span| span.start as usize == start && span.end as usize == start + 4
            )));
    }
    Ok(())
}

#[test]
fn authored_python_aliases_remain_public_query_graph_and_reference_targets() -> Result<(), String> {
    let single = "from unavailable_publication_fixture import first as copy\nbefore = [copy]\n";
    assert_public_alias_targets(single, &[("before = [copy", 0)], 1, LinkKind::Reads)?;
    // Preserve the original bare-name cases: the established frontend marks
    // these potential alias assignments as TypeReference, not runtime Reads.
    let bare_single = single.replace("[copy]", "copy");
    assert_public_alias_targets(
        &bare_single,
        &[("before = copy", 0)],
        1,
        LinkKind::TypeReference,
    )?;
    let bare_source = SOURCE.replace("[copy]", "copy");
    assert_public_alias_targets(
        &bare_source,
        &[
            ("before = copy", 0),
            ("after = copy", 1),
            ("final = copy", 3),
        ],
        4,
        LinkKind::TypeReference,
    )?;
    assert_public_alias_targets(
        SOURCE,
        &[
            ("before = [copy", 0),
            ("after = [copy", 1),
            ("final = [copy", 3),
        ],
        4,
        LinkKind::Reads,
    )
}
