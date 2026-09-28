//! Immutable browsing view snapshots prepared on the read worker.
//! No source parsing, grouping, or alignment runs during pointer rendering.

use crate::model::browse::{FindPackage, PackageApi};
use crate::model::pages::{Known, SearchPage, SearchRow, PackageDossier, PackageRecord, MatchReason};
use crate::shell::kit::{gap_words, kind_of};
use gpui::SharedString;

/// Markdown belongs to the documentation page. A compact browse sentence
/// uses the same visible words as the shared prose renderer.
fn summary(text: &str) -> SharedString {
    facet::overlay::text::parse(text.lines().next().unwrap_or(text))
        .iter()
        .map(facet::overlay::text::Piece::text)
        .collect::<String>()
        .trim()
        .to_owned()
        .into()
}

/// A browse excerpt is optional when its first authored line has no words.
/// Reference targets are never resolved or made actionable in this preview.
fn doc_summary(text: &str) -> Option<SharedString> {
    let words = summary(text);
    if words.as_ref().is_empty() { None } else { Some(words) }
}

fn answer(row: &SearchRow) -> facet::browse::find::Answer {
    let signature = row.signature.known().map(|signature| signature.text.to_string());
    facet::browse::find::Answer {
        key: row.decl.coordinate.as_str().to_owned().into(), name: row.decl.name.to_string().into(), kind: kind_of(row.decl.kind),
        context: source_context(&row.decl),
        reason: match row.reason {
            MatchReason::ExactName => "The name matches exactly", MatchReason::Name => "Matched its recorded name",
            MatchReason::Signature => "Matched its recorded signature", MatchReason::Docs => "Matched its authored documentation",
            MatchReason::Producer => "Returned by the local index",
        }.into(),
        summary: row.snippet.as_ref().and_then(|text| doc_summary(text)),
        pipe: signature.as_deref().and_then(|signature| facet::semantics::recorded::callable(signature, &row.decl.name, facet::semantics::recorded::Language::from_name(row.decl.language.name()))),
        signature: signature.map(Into::into),
    }
}

fn source_context(decl: &crate::model::pages::DeclRef) -> Option<SharedString> {
    source_context_parts(decl.path.as_deref(), decl.line)
}

fn source_context_parts(path: Option<&str>, line: Option<u32>) -> Option<SharedString> {
    match (path, line) {
        (Some(path), Some(line)) => Some(format!("{path}:{line}").into()),
        (Some(path), None) => Some(path.to_owned().into()),
        (None, Some(line)) => Some(format!("line {line}").into()),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn same_named_index_rows_keep_their_recorded_source_locations() {
        let a = source_context_parts(Some("src/de.rs"), Some(82));
        let b = source_context_parts(Some("src/map.rs"), Some(19));
        assert_eq!(a.as_ref().map(SharedString::as_ref), Some("src/de.rs:82"));
        assert_eq!(b.as_ref().map(SharedString::as_ref), Some("src/map.rs:19"));
        assert_ne!(a, b);
        assert_eq!(source_context_parts(None, None), None);
    }

    #[test]
    fn compact_docs_use_shared_prose_labels_without_discarding_the_sentence() {
        assert_eq!(doc_summary("Deserialization TOML [value][crate::Value]"), Some("Deserialization TOML value".into()));
        assert_eq!(doc_summary("Read the [Value](crate::Value) in `toml`."), Some("Read the Value in toml.".into()));
        assert_eq!(summary("*Fast* `read` for packages").as_ref(), "Fast read for packages");
    }
}

pub fn prepare_find(query: &str, answers: &Known<SearchPage>, packages: &[FindPackage], package_coverage: &Known<()>) -> facet::browse::find::Model {
    use facet::browse::find::{Candidate, Model};
    let query = SharedString::from(query.to_owned());
    let mut candidates = packages.iter().map(|package| Candidate {
        key: package.package.as_str().to_owned().into(), name: package.name.to_string().into(), version: package.package.version().map(|v| v.to_owned().into()),
        indexed: package.indexed, summary: package.description.as_ref().map(|text| summary(text)),
        facts: package.record.as_ref().map(record_facts).unwrap_or_default(), answers: vec![],
    }).collect::<Vec<_>>();
    let mut loose = Vec::new();
    if let Some(answers) = answers.known() {
        for row in answers.rows.iter() {
            if let Some(package) = row.decl.coordinate.package() {
                let key = SharedString::from(package.as_str().to_owned());
                let at = candidates.iter().position(|candidate| candidate.key == key).unwrap_or_else(|| {
                    candidates.push(Candidate { key: key.clone(), name: package.display_name().to_owned().into(), version: package.version().map(|v| v.to_owned().into()),
                        indexed: true, summary: None, facts: vec![], answers: vec![] });
                    candidates.len() - 1
                });
                // A declaration returned by the local index is positive
                // indexing evidence, even if package catalog discovery was partial.
                candidates[at].indexed = true;
                candidates[at].answers.push(answer(row));
            } else { loose.push(answer(row)); }
        }
    }
    let mut coverage = Vec::new();
    if let Some(gap) = package_coverage.gap() { coverage.push(gap_words(gap)); }
    if let Some(answers) = answers.known() {
        let text = if answers.coverage.is_complete() { "Searched the declarations in the local index" } else { "Declaration search has partial coverage" };
        coverage.push(text.into());
    } else if !query.is_empty() && let Some(gap) = answers.gap() {
        coverage.push(gap_words(gap));
    }
    Model { query, candidates, loose, coverage, more_answers: answers.known().is_some_and(|page| page.next.is_some()), loading: false }
}

fn record_facts(record: &PackageRecord) -> Vec<(SharedString, SharedString)> {
    let mut facts = Vec::new();
    if let Some(ecosystem) = record.ecosystem.known() { facts.push(("Ecosystem".into(), ecosystem.to_string().into())); }
    if let Some(license) = record.license.known() { facts.push(("License as declared".into(), license.to_string().into())); }
    if let Some(standing) = record.standing.known() { facts.push(("Release standing".into(), standing.name().into())); }
    if let Some(bytes) = record.bytes.known() { facts.push(("Package archive".into(), format!("{bytes} bytes").into())); }
    if let Some(advisory) = record.advisory.known() {
        // A known zero with incomplete coverage is not a clean health verdict.
        facts.push(("Advisories in the configured authority".into(), format!("{} matching this release", advisory.advisories).into()));
    }
    facts
}

fn package_facts(dossier: &PackageDossier) -> Vec<(SharedString, SharedString)> {
    let mut facts = dossier.record.known().map(record_facts).unwrap_or_default();
    if let Some(dependencies) = dossier.dependencies.known() {
        facts.push(("Declared dependency edges".into(), format!("{} edges; not a cost against your tree", dependencies.len()).into()));
    }
    if let Some(dependents) = dossier.dependents.known() { facts.push(("Dependents recorded here".into(), dependents.len().to_string().into())); }
    facts
}

pub fn prepare_compare(packages: &[PackageDossier], apis: &[Known<PackageApi>]) -> facet::browse::compare::Model {
    use facet::browse::compare::{Candidate, Model, Operation};
    let candidates = packages.iter().enumerate().map(|(at, package)| {
        let record = package.record.known();
        let api = apis.get(at).and_then(|api| api.known());
        let coverage = apis.get(at).and_then(|api| api.gap()).map(gap_words)
            .or_else(|| api.filter(|api| !api.complete).map(|_| "Indexed outline is partial".into()));
        let operations = api.map(|api| api.items.iter().map(|item| {
            let signature = item.signature.known().map(|sig| sig.text.to_string());
            Operation { path: item.decl.path.as_ref().map(|path| path.to_string().into()), answer: facet::browse::find::Answer {
                key: item.decl.coordinate.as_str().to_owned().into(), name: item.decl.name.to_string().into(), kind: kind_of(item.decl.kind), reason: "Recorded in the indexed outline".into(),
                context: source_context(&item.decl),
                summary: item.summary.as_ref().and_then(|text| doc_summary(text)),
                pipe: signature.as_deref().and_then(|signature| facet::semantics::recorded::callable(signature, &item.decl.name, facet::semantics::recorded::Language::from_name(item.decl.language.name()))), signature: signature.map(Into::into),
            }}
        }).collect());
        Candidate { key: package.package.as_str().to_owned().into(), name: record.map_or_else(|| package.package.display_name().to_owned(), |record| record.name.to_string()).into(),
            version: record.and_then(|record| record.version.known()).map(|version| version.to_string().into()),
            description: record.and_then(|record| record.description.known()).map(|description| summary(description)),
            facts: package_facts(package), operations, complete: api.is_some_and(|api| api.complete), coverage }
    }).collect::<Vec<_>>();
    let model = Model::new(candidates);
    model
}
