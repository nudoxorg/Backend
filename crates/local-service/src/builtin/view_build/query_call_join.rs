//! Exercise the public schema example through the source-to-query projection.

use super::super::structural::StructuralProjectionPlan;
use super::super::tests::{analyze_source, cross_file_sources};
use super::append_structural_query_facts;
use backend_engine::{PackageKey, RowId, SourceLanguage};
use backend_extension_trustfall::{
    FieldValue, PackageScopeEvidence, SemanticQueryCancellation, SemanticQueryCorpus,
    SemanticQueryEvent, SemanticQueryEvidence, SemanticQueryFact, SemanticQueryPresentation,
    SemanticQueryRequest, SemanticQueryRow, execute_semantic_query,
};
use futures_util::StreamExt as _;
use std::collections::{BTreeMap, BTreeSet};

fn package_fact(package: PackageKey) -> SemanticQueryFact {
    SemanticQueryFact::new(
        SemanticQueryEvidence::Package(PackageScopeEvidence::new(package)),
        SemanticQueryPresentation {
            id: RowId::Package(package).stable_key(),
            kind: "project".to_owned(),
            coordinate: "fixture".to_owned(),
            name: "fixture".to_owned(),
            signature: None,
            documentation: String::new(),
            score: None,
            project: None,
            parent: None,
            related: Box::new([]),
        },
    )
}

fn schema_worked_query() -> Result<&'static str, String> {
    // Lift the real resource example so schema/documentation drift is visible.
    let resource = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../apps/mcp/src/jsonrpc/resources.rs"
    ));
    resource
        .split_once("Who in this package references one named declaration:")
        .and_then(|(_, text)| text.split_once("```graphql\n"))
        .and_then(|(_, text)| text.split_once("```"))
        .map(|(query, _)| query.trim())
        .ok_or_else(|| "missing referencedBy schema worked query".to_owned())
}

fn referenced_by(
    facts: Vec<SemanticQueryFact>,
    name: &str,
) -> Result<Vec<SemanticQueryRow>, String> {
    let workspace = super::super::super::genesis().map_err(|error| error.to_string())?;
    let corpus =
        SemanticQueryCorpus::admit(workspace.root(), facts).map_err(|error| error.to_string())?;
    let (cancellation, _) = SemanticQueryCancellation::new();
    let request = SemanticQueryRequest::admit_page(
        corpus,
        schema_worked_query()?,
        BTreeMap::from([("name".to_owned(), name.into())]),
        0,
        100,
        cancellation,
    )
    .map_err(|error| error.to_string())?;
    let events = futures_executor::block_on(
        execute_semantic_query(request)
            .map_err(|error| error.to_string())?
            .collect::<Vec<_>>(),
    );
    assert!(
        matches!(events.last(), Some(SemanticQueryEvent::Terminal(terminal)) if terminal.is_complete())
    );
    Ok(events
        .into_iter()
        .filter_map(|event| match event {
            SemanticQueryEvent::Row(row) => Some(row.into_row()),
            SemanticQueryEvent::Terminal(_) => None,
        })
        .collect())
}

#[test]
fn schema_worked_referenced_by_joins_exact_typescript_imports() -> Result<(), String> {
    let written = [
        (
            "repo.ts",
            "export function getTeachersByClass() { return 1; }\n",
        ),
        (
            "alias.ts",
            "import { getTeachersByClass as load } from './repo';\nexport function aliasCaller() { return load(); }\n",
        ),
        (
            "direct.ts",
            "import { getTeachersByClass } from './repo';\nexport function directCaller() { return getTeachersByClass(); }\n",
        ),
        (
            "namespace.ts",
            "import * as repo from './repo';\nexport function namespaceCaller() { return repo.getTeachersByClass(); }\n",
        ),
        (
            "shadow-alias.ts",
            "import { getTeachersByClass as load } from './repo';\nexport function shadowedAlias(load: () => number) { return load(); }\n",
        ),
        (
            "shadow-direct.ts",
            "import { getTeachersByClass } from './repo';\nexport function shadowedDirect(getTeachersByClass: () => number) { return getTeachersByClass(); }\n",
        ),
        ("other.ts", "export function other() { return 2; }\n"),
        (
            "unrelated.ts",
            "import { other as getTeachersByClass } from './other';\nexport function unrelatedCaller() { return getTeachersByClass(); }\n",
        ),
        (
            "bare.ts",
            "export function bareCaller() { return getTeachersByClass(); }\n",
        ),
        (
            "text.ts",
            "import { getTeachersByClass as load } from './repo';\nexport function textOnly() { return 'load()'; }\n",
        ),
    ];
    let files = written
        .iter()
        .map(|(path, source)| {
            Ok((
                *path,
                SourceLanguage::TypeScript,
                analyze_source(SourceLanguage::TypeScript, path, source)?,
            ))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let (sources, package) = cross_file_sources(&files)?;
    let plan = StructuralProjectionPlan::of(&sources, &BTreeSet::new())
        .map_err(|error| error.to_string())?;
    let mut facts = vec![package_fact(package)];
    append_structural_query_facts(&sources, &plan, &mut facts)
        .map_err(|error| error.to_string())?;
    let rows = referenced_by(facts, "getTeachersByClass")?;
    // Same-name import/parameter rows still retain the schema's optional nulls.
    // Only the declaration in repo.ts may acquire these three callers.
    let callers = rows
        .iter()
        .filter(|row| row.get("caller") != Some(&FieldValue::Null))
        .collect::<Vec<_>>();
    let expected: Vec<SemanticQueryRow> = [
        ("aliasCaller", "fixture::alias.ts:2::aliasCaller"),
        ("directCaller", "fixture::direct.ts:2::directCaller"),
        (
            "namespaceCaller",
            "fixture::namespace.ts:2::namespaceCaller",
        ),
    ]
    .into_iter()
    .map(|(name, coordinate)| {
        BTreeMap::from([
            ("caller".into(), name.into()),
            ("callerAt".into(), coordinate.into()),
            ("callerKind".into(), "function".into()),
        ])
    })
    .collect();
    assert_eq!(
        callers.len(),
        expected.len(),
        "unexpected shadowed/unrelated caller: {rows:?}"
    );
    for row in expected {
        assert!(
            callers.contains(&&row),
            "missing exact imported caller {row:?}: {rows:?}"
        );
    }
    Ok(())
}

#[test]
fn query_call_edges_follow_exact_binding_rows_and_omit_absent_targets() -> Result<(), String> {
    use backend_compile::SourceBindingIdentity;
    let files = [
        (
            "repo.ts",
            SourceLanguage::TypeScript,
            analyze_source(
                SourceLanguage::TypeScript,
                "repo.ts",
                "export function target() {}\n",
            )?,
        ),
        (
            "caller.ts",
            SourceLanguage::TypeScript,
            analyze_source(
                SourceLanguage::TypeScript,
                "caller.ts",
                "import { target as invoke } from './repo';\nexport function caller() { invoke(); }\n",
            )?,
        ),
    ];
    let (sources, package) = cross_file_sources(&files)?;
    let plan = StructuralProjectionPlan::of(&sources, &BTreeSet::new())
        .map_err(|error| error.to_string())?;
    let mut facts = vec![package_fact(package)];
    append_structural_query_facts(&sources, &plan, &mut facts)
        .map_err(|error| error.to_string())?;

    // Exercise the exact-site projection contract without fabricating a compiler
    // image: real captured bindings point to replacement admitted row IDs.
    let mut exact = super::super::exact_sites::ExactDeclarationSites::default();
    let mut replacements = BTreeMap::new();
    for (path, _, declarations) in &files {
        let name = if *path == "repo.ts" {
            "target"
        } else {
            "caller"
        };
        let declaration = declarations
            .iter()
            .find(|declaration| declaration.name() == name)
            .ok_or("exact source declaration")?;
        let SourceBindingIdentity::Exact(capture) = declaration.binding_identity() else {
            return Err("fixture lost its exact source binding".to_owned());
        };
        let coordinate = format!("fixture::{path}:{}::{name}", declaration.line());
        let row = facts
            .iter()
            .find(|fact| fact.presentation().coordinate == coordinate)
            .ok_or("source query row")?;
        let replacement = RowId::Symbol(backend_engine::symbol_key(&format!(
            "selected::{path}::{name}"
        )));
        replacements.insert(row.presentation().id.clone(), replacement.stable_key());
        exact.record(package, path, capture.binding(), replacement);
    }
    for fact in &mut facts {
        let mut presentation = fact.presentation().clone();
        if let Some(id) = replacements.get(&presentation.id) {
            presentation.id = id.clone();
        }
        presentation.related = Box::new([]);
        *fact = SemanticQueryFact::new(fact.evidence().clone(), presentation);
    }
    for selected_plan in [&plan, &StructuralProjectionPlan::default()] {
        let mut selected = facts.clone();
        super::append_source_call_edges(&sources, selected_plan, &exact, &mut selected, None)
            .map_err(|error| error.to_string())?;
        let rows = referenced_by(selected, "target")?;
        let callers = rows
            .iter()
            .filter(|row| row.get("caller") != Some(&FieldValue::Null))
            .collect::<Vec<_>>();
        let expected: SemanticQueryRow = BTreeMap::from([
            ("caller".into(), "caller".into()),
            ("callerAt".into(), "fixture::caller.ts:2::caller".into()),
            ("callerKind".into(), "function".into()),
        ]);
        assert_eq!(callers, vec![&expected]);
    }
    facts.retain(|fact| fact.presentation().coordinate != "fixture::repo.ts:1::target");
    super::append_source_call_edges(&sources, &plan, &exact, &mut facts, None)
        .map_err(|error| error.to_string())?;
    assert!(
        facts
            .iter()
            .all(|fact| fact.presentation().related.is_empty()),
        "an absent selected target must not produce a dangling source-call edge"
    );
    referenced_by(facts, "target")?;
    Ok(())
}
