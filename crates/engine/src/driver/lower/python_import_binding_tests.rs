//! Exact import provenance is not a name-only publication/type authority.

use std::{
    collections::HashSet,
    error::Error,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

use backend_frontend_python::legacy::checker::{
    DefinitionTarget, NativePythonProjectAuthority, PythonProjectControl, PythonProjectSource,
};
use backend_frontend_python::legacy::{
    CheckerReport, DeclarationKind, ModuleFacts, OccurrenceKind, Span, SymbolOutcome,
    SymbolResolution, extract,
};
use backend_semantic::{
    ir::{
        DeclarationPublicationRole as Role, Ir, LinkTarget, OccurrenceConfidence, SemanticReader,
        SemanticTypeTag, SourceIdentity,
    },
    vocabulary::{CompileRecipeFact, LanguageProfile, NativeTool, PythonVersion, Stage},
};
use backend_version::{ContentId, SourceFactDomain, ToolchainDomain};

use super::{Emitter, FactSet, collect_with_checker, collect_with_project_checker};

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

// These deliberately unavailable imports exercise the native import-site
// fallback, not a fabricated imported function/type. Calls must remain refused.
const SOURCE: &str = concat!(
    "# 🐍 exact UTF-8/CRLF sites\r\n",
    "from unavailable_alias_provenance import first as copy\r\n",
    "before = copy\r\n",
    "from unavailable_alias_provenance import second as copy\r\n",
    "after = copy\r\n",
    "from unavailable_alias_provenance import third as copy, fourth as copy\r\n",
    "final = copy\r\n",
    "called = copy()\r\n",
    "def local(copy):\r\n    return copy\r\n",
    "class Scope:\r\n    copy = 1\r\n    inside = copy\r\n",
    "from unavailable_alias_provenance import unicode_first as K\r\n",
    "unicode_before = K\r\n",
    "from unavailable_alias_provenance import unicode_second as K\r\n",
    "unicode_after = K\r\n",
    "mixed = 1\r\n",
    "from unavailable_alias_provenance import replacement as mixed\r\n",
    "mixed_read = mixed\r\n",
);

fn syntax(source: &str) -> TestResult<ModuleFacts> {
    extract(source.as_bytes(), PythonVersion::Python314)
        .map_err(|cause| format!("{cause:?}").into())
}

fn image(source: &str, facts: &FactSet<'_>) -> TestResult<Ir> {
    let identity = ContentId::<SourceFactDomain>::from_canonical_bytes(source.as_bytes());
    let profile = LanguageProfile::Python(PythonVersion::Python314);
    let recipe = CompileRecipeFact::derive(
        profile,
        Stage::LowerIr,
        NativeTool::Python,
        identity,
        ContentId::<ToolchainDomain>::from_canonical_bytes(b"python-import-provenance-test"),
    );
    facts
        .build_ir(
            profile,
            SourceIdentity {
                identity,
                byte_len: source.len() as u32,
            },
            recipe,
            crate::driver::types::DeclarationScope::fixture(),
        )
        .map_err(|cause| format!("{cause:?}").into())
}

fn copy_sites(module: &ModuleFacts) -> Vec<Span> {
    module
        .declarations
        .iter()
        .filter(|declaration| {
            declaration.kind == DeclarationKind::Alias && declaration.name == "copy"
        })
        .map(|declaration| declaration.name_span)
        .collect()
}

#[test]
fn import_provenance_identity_is_exact_owned_and_independent_of_emission_order() -> TestResult {
    let mut module = syntax(SOURCE)?;
    assert_eq!(copy_sites(&module).len(), 4);
    let mut identities = None;
    for reverse in [false, true] {
        if reverse {
            module.declarations.reverse();
        }
        let mut facts = FactSet::new();
        collect_with_checker(&module, SOURCE.as_bytes(), &mut facts, None)
            .map_err(|cause| format!("{cause:?}"))?;
        let aliases = facts
            .names
            .iter()
            .take(facts.len())
            .enumerate()
            .filter(|(ordinal, name)| {
                **name == b"copy"
                    && facts.kinds[*ordinal] == backend_semantic::ir::EntityKind::Alias
            })
            .map(|(ordinal, _)| ordinal)
            .collect::<Vec<_>>();
        assert_eq!(
            aliases.len(),
            4,
            "all written sites reach canonical admission"
        );
        for ordinal in &aliases {
            assert_eq!(facts.type_records[*ordinal].tag, SemanticTypeTag::Unknown);
            assert_eq!(facts.publication_roles[*ordinal], Role::PrimaryOwned);
        }
        let ir = image(SOURCE, &facts)?;
        let observed = aliases
            .iter()
            .map(|ordinal| {
                let row = ir
                    .entity(backend_semantic::ir::EntityId::new(*ordinal as u32))
                    .ok_or("missing canonical alias")?;
                assert_eq!(row.authority.publication, Role::PrimaryOwned);
                assert!(row.source.is_some());
                Ok(row.version.identity())
            })
            .collect::<TestResult<HashSet<_>>>()?;
        assert_eq!(
            observed.len(),
            4,
            "same-line sites have distinct identities"
        );
        if let Some(expected) = identities.replace(observed.clone()) {
            assert_eq!(
                expected, observed,
                "identity cannot depend on emission ordinal"
            );
        }
    }
    Ok(())
}

fn assert_exact_targets(module: &ModuleFacts, report: &CheckerReport, source: &str) -> TestResult {
    let mut facts = FactSet::new();
    let mut emitter = Emitter::new(source.as_bytes(), module, &mut facts, Some(report));
    emitter.project_authority = true;
    emitter.project_definitions = report
        .symbols
        .iter()
        .filter_map(|symbol| match &symbol.outcome {
            SymbolOutcome::Definition { target, .. } => {
                Some(((symbol.span.start, symbol.span.end), target))
            }
            _ => None,
        })
        .collect();
    emitter
        .emit_classes()
        .map_err(|cause| format!("{cause:?}"))?;
    emitter
        .emit_non_class_declarations()
        .map_err(|cause| format!("{cause:?}"))?;
    let rows = emitter.pushed.clone();
    let mut expected_links = Vec::new();
    for symbol in &report.symbols {
        let SymbolOutcome::Definition { target, .. } = &symbol.outcome else {
            continue;
        };
        if !target.same_module || target.kind != DeclarationKind::Alias {
            continue;
        }
        let declaration = module
            .declarations
            .iter()
            .position(|declaration| declaration.name_span == target.name_span)
            .ok_or("missing exact native site")?;
        let expected = emitter.ordinals[declaration].ok_or("exact native site was collapsed")?;
        let occurrence = module
            .occurrences
            .iter()
            .find(|occurrence| occurrence.span == symbol.span)
            .ok_or("native occurrence absent from captured syntax")?;
        let resolved = emitter
            .occurrence_target(&rows, occurrence)
            .map_err(|cause| format!("{cause:?}"))?
            .ok_or("native target lost")?;
        assert!(matches!(resolved,
            (backend_semantic::ir::OccurrenceTarget::Local(entity), OccurrenceConfidence::Oracle)
                if entity.raw == expected));
        expected_links.push((symbol.span, expected));
    }
    assert!(
        expected_links.len() >= 3,
        "earlier, later and same-line final native sites must be checked"
    );
    drop(emitter);
    // Also exercise the complete lowerer and owned image, beyond target selection.
    let mut complete = FactSet::new();
    collect_with_project_checker(module, source.as_bytes(), &mut complete, report)
        .map_err(|cause| format!("{cause:?}"))?;
    let ir = image(source, &complete)?;
    for (span, expected) in expected_links {
        let links = ir
            .link_occurrences()
            .filter_map(|(_, occurrence)| {
                occurrence
                    .source
                    .filter(|source| source.start() == span.start && source.end() == span.end)
                    .map(|_| occurrence)
            })
            .collect::<Vec<_>>();
        assert_eq!(
            links.len(),
            1,
            "one persisted occurrence at the exact captured use"
        );
        assert_eq!(
            links[0].confidence,
            backend_semantic::ir::Confidence::Compiler
        );
        let link = ir.link(links[0].link).ok_or("persisted link")?;
        assert_eq!(
            link.target,
            LinkTarget::Local(backend_semantic::ir::EntityId::new(expected))
        );
        let target = ir
            .entity(backend_semantic::ir::EntityId::new(expected))
            .ok_or("persisted alias")?;
        assert_eq!(target.authority.publication, Role::PrimaryOwned);
    }
    Ok(())
}

#[test]
fn supplied_exact_definitions_reach_each_written_alias_without_name_fallback() -> TestResult {
    let module = syntax(SOURCE)?;
    let sites = copy_sites(&module);
    let mut symbols = Vec::new();
    for (statement, site) in [
        ("before = copy", sites[0]),
        ("after = copy", sites[1]),
        ("final = copy", sites[3]),
    ] {
        let start = SOURCE.find(statement).ok_or("fixture use")? + statement.len() - 4;
        symbols.push(SymbolResolution {
            target: "copy".into(),
            span: Span {
                start: start as u32,
                end: start as u32 + 4,
            },
            outcome: SymbolOutcome::Definition {
                target: DefinitionTarget {
                    relative_path: "pkg/client.py".into(),
                    package: "pkg".into(),
                    name_span: site,
                    qualified_name: "pkg.client.copy".into(),
                    source_coordinate: "unused-local-coordinate".into(),
                    name: "copy".into(),
                    kind: DeclarationKind::Alias,
                    same_module: true,
                },
                callee: None,
            },
        });
    }
    assert_exact_targets(
        &module,
        &CheckerReport {
            symbols: symbols.into_boxed_slice(),
            ..CheckerReport::default()
        },
        SOURCE,
    )
}

#[test]
fn import_identity_preserves_unicode_source_spelling_when_ruff_normalizes_names() -> TestResult {
    let source = "from unavailable_alias_provenance import first as K, second as K\r\n";
    let module = syntax(source)?;
    let aliases = module
        .declarations
        .iter()
        .filter(|declaration| declaration.kind == DeclarationKind::Alias)
        .collect::<Vec<_>>();
    assert_eq!(aliases.len(), 2);
    assert!(
        aliases.iter().all(|declaration| declaration.name == "K"),
        "Ruff's semantic identifiers are NFKC normalized"
    );
    let mut facts = FactSet::new();
    collect_with_checker(&module, source.as_bytes(), &mut facts, None)
        .map_err(|cause| format!("{cause:?}"))?;
    assert_eq!(&facts.names[..2], &["K".as_bytes(), b"K".as_slice()]);
    assert_eq!(&facts.publication_roles[..2], &[Role::PrimaryOwned; 2]);
    for (ordinal, declaration) in aliases.iter().enumerate() {
        let discriminator = facts.identity_discriminators[ordinal].ok_or("captured import site")?;
        assert_eq!(
            &discriminator[8..12],
            &declaration.name_span.start.to_le_bytes()
        );
        assert_eq!(
            &discriminator[12..16],
            &declaration.name_span.end.to_le_bytes()
        );
    }
    let ir = image(source, &facts)?;
    assert_eq!(
        ir.canonical_entities()
            .filter(|entity| entity.kind == backend_semantic::ir::ItemKind::Alias)
            .count(),
        2
    );
    Ok(())
}

#[test]
fn import_identity_refuses_a_same_spelling_name_outside_its_captured_owner() -> TestResult {
    let mut module = syntax(SOURCE)?;
    let sites = copy_sites(&module);
    let alias = module
        .declarations
        .iter_mut()
        .find(|declaration| declaration.name_span == sites[0])
        .ok_or("first alias")?;
    // The spelling still matches, but another statement cannot lend its site.
    alias.name_span = sites[3];
    let mut facts = FactSet::new();
    let outcome = collect_with_checker(&module, SOURCE.as_bytes(), &mut facts, None);
    assert!(
        matches!(outcome, Err(super::PythonCollectError::Span { start, end })
        if (start, end) == (sites[3].start, sites[3].end))
    );
    Ok(())
}

fn assert_normalized_native_aliases(checked: &CheckerReport) -> TestResult {
    for (statement, binding) in [
        ("unicode_before = K", "as K"),
        ("unicode_after = K", "as K"),
    ] {
        let start = SOURCE.find(statement).ok_or("normalized-name use")? + statement.len() - 1;
        let binding_start = SOURCE.find(binding).ok_or("normalized-name binding")? + 3;
        let symbol = checked
            .symbols
            .iter()
            .find(|symbol| symbol.span.start as usize == start)
            .ok_or("native normalized-name use")?;
        assert!(
            matches!(&symbol.outcome, SymbolOutcome::Definition { target, callee: None }
            if target.same_module && target.kind == DeclarationKind::Alias && target.name.as_ref() == "K"
                && target.name_span.start as usize == binding_start)
        );
    }
    let start = SOURCE.find("mixed_read = mixed").ok_or("mixed use")? + "mixed_read = ".len();
    let binding = SOURCE.find("as mixed").ok_or("mixed import")? + 3;
    assert!(
        matches!(checked.symbols.iter().find(|symbol| symbol.span.start as usize == start)
        .map(|symbol| &symbol.outcome), Some(SymbolOutcome::Definition { target, callee: None })
            if target.same_module && target.kind == DeclarationKind::Alias && target.name_span.start as usize == binding),
        "native exact later import bypasses the ambiguous ordinary/import name fallback"
    );
    Ok(())
}

#[test]
fn native_alias_definitions_preserve_earlier_later_sites_and_refuse_callable_authority()
-> TestResult {
    let root = tempfile::tempdir()?;
    let authority = NativePythonProjectAuthority::admit()?;
    let sources = [
        PythonProjectSource {
            relative_path: "pkg/__init__.py",
            source: "",
        },
        PythonProjectSource {
            relative_path: "pkg/client.py",
            source: SOURCE,
        },
    ];
    let cancelled = AtomicBool::new(false);
    let control = || PythonProjectControl {
        cancelled: &cancelled,
        deadline: Instant::now() + Duration::from_secs(30),
    };
    let report = authority.analyze_project(
        root.path(),
        "pkg",
        &sources,
        PythonVersion::Python314,
        control(),
    )?;
    let checked = report
        .module("pkg/client.py")
        .ok_or("native module report")?;
    let module = syntax(SOURCE)?;
    let sites = copy_sites(&module);
    for (statement, expected) in [
        ("before = copy", sites[0]),
        ("after = copy", sites[1]),
        ("final = copy", sites[3]),
    ] {
        let start = SOURCE.find(statement).ok_or("fixture use")? + statement.len() - 4;
        let symbol = checked
            .symbols
            .iter()
            .find(|symbol| symbol.span.start as usize == start)
            .ok_or("native use")?;
        assert!(
            matches!(&symbol.outcome, SymbolOutcome::Definition { target, callee: None }
            if target.same_module && target.kind == DeclarationKind::Alias && target.name_span == expected)
        );
    }
    let call = module
        .occurrences
        .iter()
        .find(|occurrence| {
            occurrence.kind == OccurrenceKind::FunctionCall && occurrence.target == "copy"
        })
        .ok_or("fixture call")?;
    assert!(matches!(
        checked.symbol_at(call.span).map(|symbol| &symbol.outcome),
        Some(SymbolOutcome::Unresolved)
    ));
    let local = SOURCE.find("return copy").ok_or("local read")? + 7;
    assert!(
        matches!(
            checked
                .symbols
                .iter()
                .find(|symbol| symbol.span.start as usize == local)
                .map(|symbol| &symbol.outcome),
            Some(SymbolOutcome::Unresolved)
        ),
        "an unrepresented parameter is not a module import target"
    );
    let class = SOURCE.find("inside = copy").ok_or("class read")? + 9;
    let field = SOURCE.find("    copy = 1").ok_or("class field")? + 4;
    assert!(
        matches!(
            checked.symbols.iter().find(|symbol| symbol.span.start as usize == class)
                .map(|symbol| &symbol.outcome),
            Some(SymbolOutcome::Definition { target, .. })
                if target.kind == DeclarationKind::Field && target.name_span.start as usize == field
        ),
        "the actual class binding keeps its own native target"
    );
    assert_normalized_native_aliases(checked)?;
    assert_exact_targets(&module, checked, SOURCE)?;
    report.witness().validate_current(control())?;
    Ok(())
}

fn assert_mixed_fallback(
    emitter: &Emitter<'_, '_>,
    module: &ModuleFacts,
    source: &str,
) -> TestResult {
    let start = source.find("mixed_read = mixed").ok_or("mixed read")? + "mixed_read = ".len();
    let occurrence = module
        .occurrences
        .iter()
        .find(|occurrence| occurrence.span.start as usize == start)
        .ok_or("mixed occurrence")?;
    assert_eq!(
        module
            .declarations
            .iter()
            .filter(|declaration| declaration.name == "mixed")
            .count(),
        2,
        "Ruff retains the ordinary constant and later authored import"
    );
    assert!(
        emitter.named_row(&emitter.pushed, occurrence).is_none(),
        "a name alone cannot choose between different written bindings"
    );
    assert!(matches!(
        emitter
            .occurrence_target(&emitter.pushed, occurrence)
            .map_err(|cause| format!("{cause:?}"))?,
        Some((
            backend_semantic::ir::OccurrenceTarget::Foreign(_),
            OccurrenceConfidence::Index
        ))
    ));
    Ok(())
}

#[test]
fn repeated_import_fallback_does_not_guess_a_binding_or_change_ordinary_twin_dedup() -> TestResult {
    let source = concat!(
        "from unavailable_alias_provenance import first as copy\n",
        "from unavailable_alias_provenance import second as copy\n",
        "value = copy\n",
        "def outer():\n    def same(): pass\n    def same(): pass\n    return same()\n",
        "def local(copy): return copy\n",
        "class Scope:\n    import math as copy\n    copy = 1\n    inside = copy\n",
        "mixed = 1\nfrom unavailable_alias_provenance import replacement as mixed\nmixed_read = mixed\n",
    );
    let module = syntax(source)?;
    let mut facts = FactSet::new();
    let mut emitter = Emitter::new(source.as_bytes(), &module, &mut facts, None);
    emitter
        .emit_classes()
        .map_err(|cause| format!("{cause:?}"))?;
    emitter
        .emit_non_class_declarations()
        .map_err(|cause| format!("{cause:?}"))?;
    assert_eq!(
        module
            .declarations
            .iter()
            .enumerate()
            .filter(|(index, declaration)| declaration.name == "same" && emitter.live[*index])
            .count(),
        1,
        "ordinary twin policy stays intact"
    );
    assert_eq!(
        copy_sites(&module).len(),
        2,
        "class/function import sites stay outside module aliases"
    );
    let start = source.find("value = copy").ok_or("fixture read")? + 8;
    let occurrence = module
        .occurrences
        .iter()
        .find(|occurrence| occurrence.span.start as usize == start)
        .ok_or("read")?;
    let target = emitter
        .occurrence_target(&emitter.pushed, occurrence)
        .map_err(|cause| format!("{cause:?}"))?;
    assert!(matches!(
        target,
        Some((
            backend_semantic::ir::OccurrenceTarget::Foreign(_),
            OccurrenceConfidence::Index
        ))
    ));
    let local_start = source.find("return copy").ok_or("local read")? + 7;
    let local = module
        .occurrences
        .iter()
        .find(|occurrence| occurrence.span.start as usize == local_start)
        .ok_or("local occurrence")?;
    assert!(emitter.shadowed_by_local(&emitter.pushed, local));
    let class_start = source.find("inside = copy").ok_or("class read")? + 9;
    let class = module
        .occurrences
        .iter()
        .find(|occurrence| occurrence.span.start as usize == class_start)
        .ok_or("class occurrence")?;
    let row = emitter
        .named_row(&emitter.pushed, class)
        .ok_or("class binding")?;
    let declaration = row.declaration.ok_or("class declaration")?;
    assert_eq!(
        module.declarations[declaration].kind,
        DeclarationKind::Field
    );
    assert!(emitter.owners[declaration].is_some());
    assert_mixed_fallback(&emitter, &module, source)?;
    drop(emitter);
    let mut complete = FactSet::new();
    collect_with_checker(&module, source.as_bytes(), &mut complete, None)
        .map_err(|cause| format!("{cause:?}"))?;
    let ir = image(source, &complete)?;
    for (_, occurrence) in ir.link_occurrences() {
        let Some(link) = ir.link(occurrence.link) else {
            return Err("missing link".into());
        };
        if let LinkTarget::Local(target) = link.target {
            let row = ir.entity(target).ok_or("local target")?;
            if row.authority.publication == Role::PrimaryOwned
                && row.kind == backend_semantic::ir::ItemKind::Alias
            {
                return Err("name-only fallback selected import provenance".into());
            }
        }
    }
    Ok(())
}

fn assert_normalized_fallback_is_ambiguous(source: &str) -> TestResult {
    let module = syntax(source)?;
    let bindings = module
        .declarations
        .iter()
        .filter(|declaration| declaration.name == "K")
        .collect::<Vec<_>>();
    assert_eq!(
        bindings.len(),
        2,
        "Ruff retains two semantic-name-equivalent sites"
    );
    assert_eq!(
        bindings
            .iter()
            .filter(|declaration| declaration.kind == DeclarationKind::Alias)
            .count(),
        if source.starts_with("from ") { 2 } else { 1 }
    );
    let spellings = bindings
        .iter()
        .map(|declaration| {
            &source[declaration.name_span.start as usize..declaration.name_span.end as usize]
        })
        .collect::<HashSet<_>>();
    assert_eq!(spellings, HashSet::from(["K", "K"]));
    let mut facts = FactSet::new();
    let mut emitter = Emitter::new(source.as_bytes(), &module, &mut facts, None);
    emitter
        .emit_non_class_declarations()
        .map_err(|cause| format!("{cause:?}"))?;
    for statement in ["before = K", "after = K"] {
        let start = source
            .find(statement)
            .ok_or("normalized-name fixture use")?
            + statement.len()
            - 1;
        let occurrence = module
            .occurrences
            .iter()
            .find(|occurrence| occurrence.span.start as usize == start)
            .ok_or("normalized-name occurrence")?;
        assert_eq!(occurrence.target, "K");
        assert!(
            emitter.named_row(&emitter.pushed, occurrence).is_none(),
            "raw display spelling cannot select one normalized-equivalent import binding"
        );
        assert!(matches!(
            emitter
                .occurrence_target(&emitter.pushed, occurrence)
                .map_err(|cause| format!("{cause:?}"))?,
            Some((
                backend_semantic::ir::OccurrenceTarget::Foreign(_),
                OccurrenceConfidence::Index
            ))
        ));
    }
    drop(emitter);
    let mut complete = FactSet::new();
    collect_with_checker(&module, source.as_bytes(), &mut complete, None)
        .map_err(|cause| format!("{cause:?}"))?;
    let ir = image(source, &complete)?;
    let mut observed = 0;
    for (_, occurrence) in ir.link_occurrences() {
        let Some(span) = occurrence.source else {
            continue;
        };
        if ["before = K", "after = K"].iter().any(|statement| {
            source
                .find(statement)
                .is_some_and(|start| span.start() as usize == start + statement.len() - 1)
        }) {
            let link = ir
                .link(occurrence.link)
                .ok_or("persisted normalized-name use")?;
            assert!(
                matches!(link.target, LinkTarget::External(_)),
                "no guessed local endpoint survives publication"
            );
            assert_eq!(
                occurrence.confidence,
                backend_semantic::ir::Confidence::Indexed
            );
            observed += 1;
        }
    }
    assert_eq!(
        observed, 2,
        "both earlier and later uses retain honest unresolved evidence"
    );
    Ok(())
}

#[test]
fn normalized_import_fallback_refuses_equivalent_aliases_and_ordinary_collisions() -> TestResult {
    for source in [
        "from a import first as K\nbefore = K\nfrom b import second as K\nafter = K\n",
        "K = 1\nbefore = K\nfrom b import second as K\nafter = K\n",
        "K = 1\nbefore = K\nfrom b import second as K\nafter = K\n",
    ] {
        assert_normalized_fallback_is_ambiguous(source)?;
    }
    Ok(())
}

#[test]
fn normalized_ordinary_fallback_keeps_existing_owner_and_rebinding_policy() -> TestResult {
    let source = "K = 1\nK = 2\nvalue = K\n";
    let module = syntax(source)?;
    let bindings = module
        .declarations
        .iter()
        .filter(|declaration| declaration.name == "K")
        .collect::<Vec<_>>();
    assert_eq!(
        bindings.len(),
        1,
        "ordinary module rebinding remains deduplicated"
    );
    let binding = bindings[0];
    assert_eq!(
        &source[binding.span.start as usize..binding.span.end as usize],
        "K = 1"
    );
    assert_eq!(
        &source[binding.binding_span.start as usize..binding.binding_span.end as usize],
        "K = 2"
    );
    assert_eq!(
        &source[binding.name_span.start as usize..binding.name_span.end as usize],
        "K"
    );
    let mut facts = FactSet::new();
    let mut emitter = Emitter::new(source.as_bytes(), &module, &mut facts, None);
    emitter
        .emit_non_class_declarations()
        .map_err(|cause| format!("{cause:?}"))?;
    let start = source.find("value = K").ok_or("ordinary use")? + "value = ".len();
    let occurrence = module
        .occurrences
        .iter()
        .find(|occurrence| occurrence.span.start as usize == start)
        .ok_or("ordinary occurrence")?;
    let row = emitter
        .named_row(&emitter.pushed, occurrence)
        .ok_or("normalized ordinary binding")?;
    let index = row.declaration.ok_or("ordinary declaration")?;
    assert_eq!(module.declarations[index].name_span, binding.name_span);
    assert_eq!(module.declarations[index].kind, DeclarationKind::Constant);
    assert!(
        matches!(emitter.occurrence_target(&emitter.pushed, occurrence)
        .map_err(|cause| format!("{cause:?}"))?,
        Some((backend_semantic::ir::OccurrenceTarget::Local(entity), OccurrenceConfidence::Index))
            if entity.raw == row.ordinal)
    );
    Ok(())
}
