//! Immutable browsing view snapshots prepared on the read worker.
//! No source parsing, grouping, or alignment runs during pointer rendering.

use crate::model::browse::{FindPackage, PackageApi};
use crate::model::pages::{GapReason, Known, SearchPage, SearchRow, PackageDossier, PackageRecord, MatchReason};
use crate::model::pages::package::PackageRecordEvidence;
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
        source_available: row.decl.path.is_some() && row.decl.line.is_some(),
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
    use crate::model::pages::{GapReason, LicenseDeclaration, PackageRef, RecordSource};
    use crate::model::release::{Availability, Offer, Release};
    use std::sync::Arc;
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

    #[test]
    fn catalog_hit_is_not_indexed_evidence_and_compare_names_exact_recovery() {
        let package = PackageRef::parse("pkg:cargo/thiserror@2.0.0").expect("exact Cargo release");
        let release = Release::new("thiserror", "2.0.0").expect("release");
        let catalog = FindPackage {
            package: package.clone(), name: Arc::from("thiserror"), description: None,
            indexed: false, record: None,
            offer: Some(Offer { release, availability: Availability::Download, library: None }),
        };
        let found = prepare_find("thiserror", &Known::unknown(GapReason::NotRecorded, "no indexed answers"), &[catalog], &Known::Known(()));
        assert_eq!(found.candidates.len(), 1);
        assert!(!found.candidates[0].indexed);
        assert!(found.candidates[0].offer.is_some(), "the catalogue offer does not claim an indexed package");

        let mut unread = crate::shell::tests::dossier();
        unread.package = package.clone();
        unread.record = Known::unknown(GapReason::NotRecorded, "library record not found");
        unread.outline = Known::unknown(GapReason::NotRecorded, "indexed declarations not found");
        unread.dependencies = Known::unknown(GapReason::NotRecorded, "dependency edges not recorded");
        unread.dependents = Known::unknown(GapReason::NotRecorded, "dependents not recorded");
        let api = Known::unknown(GapReason::NotRecorded, "indexed declarations not found");
        let comparison = prepare_compare(&[unread.clone()], &[api]);
        let candidate = &comparison.candidates[0];
        assert_eq!(candidate.key.as_ref(), package.as_str());
        assert!(candidate.operations.is_none(), "unread is not a confirmed empty outline");
        assert!(candidate.facts.is_empty(), "catalogue identity cannot create package facts");
        let coverage = candidate.coverage.as_deref().expect("per-side missing record and recovery");
        assert!(coverage.contains("Package record unread") && coverage.contains("Indexed declarations unread"));
        assert!(coverage.contains("Explore it to check the Add to library option"));

        let mut admitted = unread;
        let mut record = crate::shell::tests::dossier().record.known().expect("fixture record").clone();
        record.package = package.clone();
        record.source = RecordSource::Registry;
        record.name = Arc::from("thiserror");
        record.version = Known::Known(Arc::from("2.0.0"));
        record.ecosystem = Known::Known(backend_library::RegistryEcosystem::Cargo);
        record.license = Known::Known(LicenseDeclaration::DeclaredAbsent);
        admitted.record = Known::Known(record);
        let admitted_api = Known::Known(PackageApi { package, items: Arc::from([]), complete: true });
        let comparison = prepare_compare(&[admitted], &[admitted_api]);
        let candidate = &comparison.candidates[0];
        assert!(candidate.operations.as_ref().is_some_and(Vec::is_empty), "complete empty differs from unread");
        assert!(candidate.coverage.is_none(), "admitted evidence needs no Add prerequisite");
        assert!(candidate.facts.iter().any(|(label, value)|
            label.as_ref() == "License as declared" && value.contains("no licence declaration")));
    }
}

pub fn prepare_find(query: &str, answers: &Known<SearchPage>, packages: &[FindPackage], package_coverage: &Known<()>) -> facet::browse::find::Model {
    use facet::browse::find::{Candidate, Model};
    let query = SharedString::from(query.to_owned());
    let mut candidates = packages.iter().map(|package| Candidate {
        // A registry release reads as its crate and version, wherever its tree is.
        key: package.package.as_str().to_owned().into(),
        name: package.offer.as_ref().map_or_else(|| package.name.to_string(), |offer| offer.release.name.to_string()).into(),
        version: package.offer.as_ref().map(|offer| offer.release.version.short().to_owned()).or_else(|| package.package.version().map(str::to_owned)).map(Into::into),
        indexed: package.indexed, summary: package.description.as_ref().map(|text| summary(text)),
        facts: package.record.as_ref().map(record_facts).unwrap_or_default(), answers: vec![],
        offer: package.offer.as_ref().map(crate::shell::acquire::facet_offer),
    }).collect::<Vec<_>>();
    let mut loose = Vec::new();
    if let Some(answers) = answers.known() {
        for row in answers.rows.iter().filter(|row|
            crate::shell::kit::search_result_route(row, crate::navigation::View::Page).is_some()) {
            if let Some(package) = row.decl.coordinate.package() {
                let key = SharedString::from(package.as_str().to_owned());
                let at = candidates.iter().position(|candidate| candidate.key == key).unwrap_or_else(|| {
                    candidates.push(Candidate { key: key.clone(), name: package.display_name().to_owned().into(), version: package.version().map(|v| v.to_owned().into()),
                        indexed: true, summary: None, facts: vec![], answers: vec![], offer: None });
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
        use backend_library::AdvisoryCoverage;
        let (label, detail) = match advisory.coverage {
            AdvisoryCoverage::Complete => ("Advisories in the configured authority", format!("{} matching this release; complete coverage", advisory.advisories)),
            AdvisoryCoverage::Partial => ("Advisories in the configured authority", format!("{} matching in partial coverage", advisory.advisories)),
            AdvisoryCoverage::Unknown | AdvisoryCoverage::Unavailable if advisory.advisories == 0 =>
                ("Advisory coverage", "No advisory feed coverage is established for this release".to_owned()),
            AdvisoryCoverage::Unknown | AdvisoryCoverage::Unavailable =>
                ("Advisories in the configured authority", format!("{} matching; coverage is not established", advisory.advisories)),
        };
        facts.push((label.into(), detail.into()));
    }
    facts
}

fn package_facts(dossier: &PackageDossier, record: Option<&PackageRecord>) -> Vec<(SharedString, SharedString)> {
    let mut facts = record.map(record_facts).unwrap_or_default();
    if let Some(dependencies) = dossier.dependencies.known() {
        facts.push(("Declared dependency edges".into(), format!("{} edges; not a cost against your tree", dependencies.len()).into()));
    }
    if !dossier.package.is_local() && let Some(dependents) = dossier.dependents.known() {
        facts.push(("Dependents recorded here".into(), dependents.len().to_string().into()));
    }
    facts
}

pub fn prepare_compare(packages: &[PackageDossier], apis: &[Known<PackageApi>]) -> facet::browse::compare::Model {
    use facet::browse::compare::{Candidate, Model, Operation};
    let candidates = packages.iter().enumerate().map(|(at, package)| {
        let record_evidence = package.record_evidence();
        let record = match record_evidence {
            PackageRecordEvidence::Bound(record) => Some(record),
            PackageRecordEvidence::Unread(_) | PackageRecordEvidence::Unbound => None,
        };
        let offered_api = apis.get(at).and_then(|api| api.known());
        let api = offered_api.filter(|api| api.package.reference() == package.package.reference()
            && api.items.iter().all(|item| item.decl.coordinate.package()
                .is_some_and(|owner| owner.reference() == package.package.reference())));
        let mut gaps = Vec::new();
        match record_evidence {
            PackageRecordEvidence::Unread(gap) => gaps.push(format!("Package record unread: {}", gap_words(gap))),
            PackageRecordEvidence::Unbound => gaps.push("Package facts did not match this exact source address".to_owned()),
            PackageRecordEvidence::Bound(_) => {}
        }
        if offered_api.is_some() && api.is_none() {
            gaps.push("Indexed declarations did not match this source address".to_owned());
        }
        if let Some(gap) = apis.get(at).and_then(|api| api.gap()) { gaps.push(format!("Indexed declarations unread: {}", gap_words(gap))); }
        if api.is_some_and(|api| !api.complete) { gaps.push("Indexed outline is partial".to_owned()); }
        let needs_index = api.is_none()
            && matches!(record_evidence, PackageRecordEvidence::Unread(gap) if gap.reason == GapReason::NotRecorded)
            && matches!(package.package.reference(), backend_library::PackageReference::Purl(_));
        if needs_index {
            gaps.push("This exact release is not recorded in the current library. Explore it to check the Add to library option before comparing declarations.".to_owned());
        }
        let coverage = (!gaps.is_empty()).then(|| gaps.join("; ").into());
        let operations = api.map(|api| api.items.iter().map(|item| {
            let signature = item.signature.known().map(|sig| sig.text.to_string());
            Operation { path: item.decl.path.as_ref().map(|path| path.to_string().into()), answer: facet::browse::find::Answer {
                key: item.decl.coordinate.as_str().to_owned().into(), name: item.decl.name.to_string().into(), kind: kind_of(item.decl.kind), reason: "Recorded in the indexed outline".into(),
                context: source_context(&item.decl),
                summary: item.summary.as_ref().and_then(|text| doc_summary(text)),
                pipe: signature.as_deref().and_then(|signature| facet::semantics::recorded::callable(signature, &item.decl.name, facet::semantics::recorded::Language::from_name(item.decl.language.name()))), signature: signature.map(Into::into),
                source_available: item.decl.path.is_some() && item.decl.line.is_some(),
            }}
        }).collect());
        let origin = match package.package.reference() {
            backend_library::PackageReference::Purl(_) => format!("Registry release · {}", package.package.as_str()),
            backend_library::PackageReference::Local(_) => format!("Local source · {}", package.package.as_str()),
        };
        Candidate { key: package.package.as_str().to_owned().into(), name: record.map_or_else(|| package.package.display_name().to_owned(), |record| record.name.to_string()).into(), origin: origin.into(),
            version: record.and_then(|record| record.version.known()).map(|version| version.to_string().into()),
            description: record.and_then(|record| record.description.known()).map(|description| summary(description)),
            facts: package_facts(package, record), operations, complete: api.is_some_and(|api| api.complete), coverage }
    }).collect::<Vec<_>>();
    let model = Model::new(candidates);
    model
}
