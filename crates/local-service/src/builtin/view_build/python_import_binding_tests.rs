//! Structural import identity regressions observed in the actual SearXNG search chain.
use super::tests::{analyze_source, cross_file_sources, cross_file_view};
use backend_engine::{DeclarationKind, SourceLanguage};
use std::collections::BTreeSet;

#[test]
fn python_from_imports_project_only_real_bindings_and_keep_distinct_repeated_sites()
-> Result<(), String> {
    let cases = [
        ("tests/unit/test_search.py", 4, "copy", "copy"),
        (
            "searx/engines/360search_videos.py",
            6,
            "datetime",
            "datetime",
        ),
        (
            "searx/search/processors/abstract.py",
            14,
            "searx.engines",
            "engines",
        ),
        (
            "searx/search/processors/online_dictionary.py",
            7,
            "searx.sxng_locales",
            "sxng_locales",
        ),
    ];
    let mut inputs = Vec::new();
    for (path, line, module, binding) in cases {
        let source = format!("{}from {module} import {binding}\n", "\n".repeat(line - 1));
        inputs.push((
            path,
            SourceLanguage::Python,
            analyze_source(SourceLanguage::Python, path, &source)?,
        ));
    }
    let repeated_path = "repeated.py";
    let repeated = "from copy import copy; from copy import copy\nfrom copy import copy, copy\n";
    inputs.push((
        repeated_path,
        SourceLanguage::Python,
        analyze_source(SourceLanguage::Python, repeated_path, repeated)?,
    ));
    let (sources, _) = cross_file_sources(&inputs)?;
    let (_, rows) = cross_file_view(&sources)?;
    let plan = super::structural::StructuralProjectionPlan::of(&sources, &BTreeSet::new())
        .map_err(|e| e.to_string())?;
    let mut facts = Vec::new();
    super::query::append_structural_query_facts(&sources, &plan, &mut facts)
        .map_err(|e| e.to_string())?;
    for (path, line, module, binding) in cases {
        let coordinate = format!("fixture::{path}:{line}::{binding}");
        let declarations = rows
            .iter()
            .filter(|row| row.label == coordinate)
            .collect::<Vec<_>>();
        assert_eq!(
            declarations.len(),
            1,
            "one real import binding at {coordinate}"
        );
        assert_eq!(declarations[0].kind, Some(DeclarationKind::Import));
        assert_eq!(
            declarations[0].signature.as_deref(),
            Some(format!("value\n{module}\n{binding}").as_str())
        );
        let search = facts
            .iter()
            .filter(|fact| fact.presentation().coordinate == coordinate)
            .collect::<Vec<_>>();
        assert_eq!(
            search.len(),
            1,
            "search corpus must receive one producer binding"
        );
        assert_eq!(search[0].presentation().id, declarations[0].id.stable_key());
    }
    for line in [1, 2] {
        let coordinate = format!("fixture::{repeated_path}:{line}::copy");
        let declarations = rows
            .iter()
            .filter(|row| row.label == coordinate)
            .collect::<Vec<_>>();
        assert_eq!(
            declarations.len(),
            2,
            "valid same-coordinate occurrences are not deduplicated"
        );
        assert_ne!(declarations[0].id, declarations[1].id);
        let identities = facts
            .iter()
            .filter(|fact| fact.presentation().coordinate == coordinate)
            .map(|fact| fact.presentation().id.clone())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            identities.len(),
            2,
            "both source occurrences retain distinct full search identities"
        );
        assert_eq!(
            identities,
            declarations.iter().map(|row| row.id.stable_key()).collect()
        );
    }
    Ok(())
}
