//! Exact local registry release history and semantic comparisons.
//!
//! The registry index answers which versions exist and which source trees are
//! already on this machine. The local owner answers which exact trees it has
//! indexed and compares those trees through its semantic diff command. Missing
//! sources and missing comparisons stay missing; a release label is never
//! treated as evidence that another version has the same API.

use crate::core::VersionedRoot;
use crate::host::registry::{Availability, Composition, RegistryFact, Release};
use crate::model::pages::PackageRef;
use crate::runtime::offload::{Answer, Cancellation, Memo};
use backend_client::Session;
use backend_library::{CommandReply, DeclarationChange, PackageReference};
use facet::data::release::{
    Change, Crate, RegistryFact as FacetRegistryFact, ReleaseDiff, Severity, SourceAvailability,
    Version, What,
};
use gpui::{Context, Global};
use std::collections::{HashMap, HashSet};
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

const RELEASE_READS: usize = 12;
const RELEASE_REFRESH: Duration = Duration::from_secs(15);
const RELEASE_LIMIT: usize = 256;
const DIFF_LIMIT: usize = 5_000;

/// Exact key for a local release read. The owner endpoint, selected registry
/// authority and immutable producer root prevent facts from another owner or
/// authority from being presented as current.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ReleaseKey {
    package: PackageRef,
    root: VersionedRoot,
    endpoint: PathBuf,
    /// The selected registry authority, captured when its owner starts.
    authority: Arc<str>,
    /// Only this exact requested release is compared with the pin.
    compare_to: Option<String>,
}

/// A complete local registry listing, with only comparisons the owner
/// successfully made. The model's typed availability/status distinguishes a
/// source missing locally from a source present but not indexed.
pub(crate) struct ReleaseData {
    pub(crate) krate: Arc<Crate>,
    pub(crate) note: Option<Arc<str>>,
}

struct ReleaseReads(Memo<ReleaseKey, Result<Arc<ReleaseData>, Arc<str>>>);

impl Global for ReleaseReads {}

impl ReleaseReads {
    fn new() -> Self {
        Self(Memo::new_cancellable(
            NonZeroUsize::new(RELEASE_READS).expect("positive capacity"),
            read,
        ))
    }
}

/// A release request's nonblocking result. `Unavailable` is a real failure or
/// missing provider, while `Reading` means the exact request is in flight.
pub(crate) enum Read {
    Reading,
    /// Waiting for another bounded registry read to finish before this exact
    /// key can start. The requester is notified when it should retry.
    Waiting,
    Ready(Arc<ReleaseData>),
    Unavailable(Arc<str>),
}

/// Reads exact release data for the current producer root. Work runs off the
/// UI thread and only the view that asked is notified when it lands.
pub(crate) fn get<T: 'static>(
    package: &PackageRef,
    root: VersionedRoot,
    compare_to: Option<&str>,
    cx: &mut Context<T>,
) -> Read {
    let Some(composition) = crate::host::registry::composed() else {
        return Read::Unavailable(Arc::from("the local registry reader is not ready yet"));
    };
    if cx.try_global::<ReleaseReads>().is_none() {
        cx.set_global(ReleaseReads::new());
    }
    let key = ReleaseKey {
        package: package.clone(),
        root,
        endpoint: composition.endpoint,
        authority: composition.authority.clone(),
        compare_to: compare_to.map(str::to_owned),
    };
    let memo = cx.global::<ReleaseReads>().0.clone();
    let endpoint = key.endpoint.clone();
    let authority = key.authority.clone();
    let root = key.root;
    memo.retain_keys(|previous| {
        previous.endpoint == endpoint && previous.authority == authority && previous.root == root
    });
    match memo.get_expiring(&key, RELEASE_REFRESH, cx) {
        Answer::Reading => Read::Reading,
        Answer::Deferred => Read::Waiting,
        Answer::Failed(fault) => Read::Unavailable(Arc::from(fault.to_string())),
        Answer::Ready(value) => match value.as_ref() {
            Ok(data) => Read::Ready(Arc::clone(data)),
            Err(reason) => Read::Unavailable(Arc::clone(reason)),
        },
    }
}

fn read(key: &ReleaseKey, cancellation: &Cancellation) -> Result<Arc<ReleaseData>, Arc<str>> {
    ensure_active(cancellation)?;
    let composition = crate::host::registry::composed()
        .filter(|composition| {
            composition.endpoint == key.endpoint && composition.authority == key.authority
        })
        .ok_or_else(|| {
            Arc::from("the local service connection changed before releases were read")
        })?;
    let pinned = key
        .package
        .verify_registry_manifest()
        .or_else(|| key.package.release())
        .ok_or_else(|| Arc::from("this package is not an exact registry release"))?;
    let published = composition.source.releases(&pinned.name);
    ensure_active(cancellation)?;
    if published.is_empty() {
        return Err(Arc::from(
            "the local registry index has no release records for this package",
        ));
    }

    let mut session = Session::connect(&composition.endpoint)
        .map_err(|error| Arc::<str>::from(format!("could not read indexed releases: {error}")))?;
    let revision = session
        .revision()
        .map_err(|error| Arc::<str>::from(format!("could not read the index revision: {error}")))?;
    ensure_active(cancellation)?;
    if revision.root != key.root.root() {
        return Err(Arc::from(
            "the local index changed before the release read began",
        ));
    }
    let selected = key.compare_to.as_deref().and_then(|version| {
        published
            .iter()
            .find(|entry| entry.release.version.as_str() == version)
    });
    let pin_entry = published.iter().find(|entry| entry.release == pinned);
    let (shown, truncated) = bounded_history(
        &published,
        [Some(&pinned), selected.map(|entry| &entry.release)],
    );

    // Exact membership checks are limited to the pin and the one selected
    // release. An absent package-list row is never inferred from a scan of
    // every owner package, so unobserved entries stay explicitly unknown.
    let mut indexed = HashMap::<String, FacetRegistryFact<bool>>::new();
    let mut owner_refs = HashMap::<String, Option<PackageReference>>::new();
    for entry in [pin_entry, selected].into_iter().flatten() {
        ensure_active(cancellation)?;
        let version = entry.release.version.as_str().to_owned();
        if owner_refs.contains_key(&version) {
            continue;
        }
        let reference = match &entry.availability {
            Availability::Ambiguous { .. } | Availability::UnverifiedArchive(_) => None,
            _ => exact_owner_reference(&mut session, &entry.release, &composition),
        };
        let fact = match &entry.availability {
            Availability::Ambiguous { .. } => FacetRegistryFact::Ambiguous,
            _ if reference.is_some() => FacetRegistryFact::Known(true),
            _ => FacetRegistryFact::Missing,
        };
        indexed.insert(version.clone(), fact);
        owner_refs.insert(version, reference);
    }

    let mut diffs = Vec::new();
    let mut comparison_note = None;
    if let Some(target) = selected {
        if target.release == pinned {
            diffs.push(ReleaseDiff {
                from: pinned.version.as_str().to_owned().into(),
                to: pinned.version.as_str().to_owned().into(),
                changes: Vec::new(),
                semver_slip: false,
            });
        } else if pin_entry.is_none() {
            comparison_note = Some("the pinned release has no exact registry record".to_owned());
        } else {
            let from = owner_refs.get(pinned.version.as_str()).cloned().flatten();
            let to = owner_refs
                .get(target.release.version.as_str())
                .cloned()
                .flatten();
            match (from, to) {
                (Some(from), Some(to)) => {
                    ensure_active(cancellation)?;
                    match session.diff(from.clone(), to.clone()) {
                    Ok(records)
                        if records.len() <= DIFF_LIMIT
                            && records.iter().all(|record| {
                                record.change != DeclarationChange::Indeterminate
                            }) =>
                    {
                        let changes = records
                            .iter()
                            .map(|record| {
                                let (what, severity) = match record.change {
                                        DeclarationChange::Added => {
                                            (What::Added, Severity::Additive)
                                        }
                                    DeclarationChange::Removed => {
                                        (What::Removed, Severity::Breaking)
                                    }
                                    DeclarationChange::Changed => {
                                        (What::Changed, Severity::Breaking)
                                    }
                                    DeclarationChange::Indeterminate => {
                                        unreachable!("checked above")
                                    }
                                };
                                Change {
                                    path: item_path(
                                        record.label.as_str(),
                                        from.as_str(),
                                        to.as_str(),
                                    )
                                    .into(),
                                    what,
                                    severity,
                                    before: None,
                                    after: None,
                                }
                            })
                            .collect::<Vec<_>>();
                        diffs.push(ReleaseDiff {
                            from: pinned.version.as_str().to_owned().into(),
                            to: target.release.version.as_str().to_owned().into(),
                            semver_slip: semver_slip(
                                pinned.version.as_str(),
                                target.release.version.as_str(),
                                changes
                                    .iter()
                                    .any(|change| change.severity == Severity::Breaking),
                            ),
                            changes,
                        });
                    }
                    Ok(_) => {
                        comparison_note = Some(
                            "the exact comparison is incomplete or exceeds the display limit"
                                .to_owned(),
                        );
                    }
                    Err(_) => {
                        comparison_note = Some(
                                "the owner has no exact comparison for the selected pair"
                                    .to_owned(),
                        );
                    }
                    }
                }
                _ => {
                    comparison_note = Some(
                        "one or both exact releases are not indexed by the current owner"
                            .to_owned(),
                    );
                }
            }
        }
    } else if key.compare_to.is_some() {
        comparison_note =
            Some("the selected exact release is absent from the local registry listing".to_owned());
    }
    ensure_active(cancellation)?;
    let after = session.revision().map_err(|error| {
        Arc::<str>::from(format!("could not confirm the index revision: {error}"))
    })?;
    if after.root != key.root.root() {
        return Err(Arc::from(
            "the local index changed while releases were being compared",
        ));
    }

    let versions = shown
        .iter()
        .map(|entry| Version {
            v: entry.release.version.as_str().to_owned().into(),
            at: match &entry.date {
                RegistryFact::Known(date) => {
                    FacetRegistryFact::Known(date.as_ref().to_owned().into())
                }
                RegistryFact::Missing => FacetRegistryFact::Missing,
                RegistryFact::Ambiguous => FacetRegistryFact::Ambiguous,
            },
            yanked: match &entry.yanked {
                RegistryFact::Known(yanked) => FacetRegistryFact::Known(*yanked),
                RegistryFact::Missing => FacetRegistryFact::Missing,
                RegistryFact::Ambiguous => FacetRegistryFact::Ambiguous,
            },
            indexed: indexed
                .get(entry.release.version.as_str())
                .cloned()
                .unwrap_or_else(|| match &entry.availability {
                    Availability::Ambiguous { .. } => FacetRegistryFact::Ambiguous,
                    _ => FacetRegistryFact::Missing,
                }),
            source: match &entry.availability {
                Availability::Download => SourceAvailability::Unavailable,
                Availability::Ambiguous { .. } => SourceAvailability::Ambiguous,
                Availability::UnverifiedArchive(_) => SourceAvailability::UnverifiedArchive,
                Availability::Unpacked(_) | Availability::Archive(_) => {
                    SourceAvailability::Available
                }
            },
        })
        .collect();
    let mut notes = Vec::<String>::new();
    if truncated {
        notes.push(format!("showing up to {RELEASE_LIMIT} entries with the pin and selection retained; other releases are omitted"));
    }
    if let Some(note) = comparison_note {
        notes.push(note);
    }
    if shown
        .iter()
        .any(|entry| matches!(&entry.availability, Availability::Download))
    {
        notes.push("some shown releases need their source added before comparison".to_owned());
    }
    if shown
        .iter()
        .any(|entry| matches!(&entry.availability, Availability::Ambiguous { .. }))
    {
        notes.push("some shown releases have conflicting registry sources; resolve the Cargo registry authority".to_owned());
    }
    if shown
        .iter()
        .any(|entry| matches!(&entry.availability, Availability::UnverifiedArchive(_)))
    {
        notes.push("some shown archives have no trusted checksum and cannot be read".to_owned());
    }
    let note = (!notes.is_empty()).then(|| Arc::from(notes.join(" · ")));
    Ok(Arc::new(ReleaseData {
        krate: Arc::new(Crate {
            name: pinned.name.as_str().to_owned().into(),
            pinned: pinned.version.as_str().to_owned().into(),
            versions,
            aliases: Default::default(),
            diffs,
            // Uses/impact require exact compiler use-site evidence. The
            // current release command does not provide that coverage.
            uses: Vec::new(),
            impact: Vec::new(),
        }),
        note,
    }))
}

fn ensure_active(cancellation: &Cancellation) -> Result<(), Arc<str>> {
    if cancellation.is_cancelled() {
        Err(Arc::from("the exact release read was superseded"))
    } else {
        Ok(())
    }
}

/// Returns the exact admitted tree path for this selected registry release.
/// A cargo PURL contains a name and version but no registry identity, so it
/// cannot prove which source authority the owner indexed. No owner-wide
/// listing or same-coordinate fallback is used.
fn exact_owner_reference(
    session: &mut Session,
    release: &Release,
    composition: &Composition,
) -> Option<PackageReference> {
    if matches!(
        composition.source.availability(release),
        Availability::Ambiguous { .. } | Availability::UnverifiedArchive(_)
    ) {
        return None;
    }
    if let Some(tree) = composition.source.tree_of(release)
        && tree.join("Cargo.toml").is_file()
        && let Some(path) = tree.to_str()
        && owner_has_exact_outline(session, path)
        && let Ok(reference) = PackageReference::parse(path.to_owned())
    {
        return Some(reference);
    }
    None
}

fn owner_has_exact_outline(session: &mut Session, coordinate: &str) -> bool {
    session
        .outline(coordinate)
        .is_ok_and(|reply| matches!(reply.reply, CommandReply::Outline(_)))
}

fn bounded_history<'a>(
    published: &'a [crate::host::registry::Published],
    required: [Option<&Release>; 2],
) -> (Vec<&'a crate::host::registry::Published>, bool) {
    if published.len() <= RELEASE_LIMIT {
        return (published.iter().collect(), false);
    }
    let mut indexes = (published.len() - RELEASE_LIMIT..published.len()).collect::<Vec<_>>();
    let required = required
        .into_iter()
        .flatten()
        .filter_map(|release| published.iter().position(|entry| &entry.release == release))
        .collect::<HashSet<_>>();
    for index in required.iter().copied() {
        if !indexes.contains(&index) {
            if let Some(remove) = indexes
                .iter()
                .position(|candidate| !required.contains(candidate))
            {
                indexes.remove(remove);
            }
            indexes.push(index);
        }
    }
    indexes.sort_unstable();
    (
        indexes.into_iter().map(|index| &published[index]).collect(),
        true,
    )
}

fn item_path(label: &str, from: &str, to: &str) -> String {
    [from, to]
        .iter()
        .find_map(|root| {
            label
                .strip_prefix(root)
                .and_then(|rest| rest.strip_prefix("::"))
        })
        .unwrap_or(label)
        .to_owned()
}

/// A breaking diff is a semver slip only when strict SemVer parsing proves
/// the target falls under the source release's Cargo caret requirement.
fn semver_slip(from: &str, to: &str, breaking: bool) -> bool {
    if !breaking {
        return false;
    }
    let (Ok(from), Ok(to)) = (semver::Version::parse(from), semver::Version::parse(to)) else {
        return false;
    };
    if to <= from {
        return false;
    }
    semver::VersionReq::parse(&format!("^{from}")).is_ok_and(|requirement| requirement.matches(&to))
}

// --------------------------------------------------------------------------
// Release API helpers

/// `version` as the exact registry listing spells it. No short-version
/// matching is performed because build metadata is part of release identity.
pub(crate) fn spelled(release: &Crate, version: &str) -> Option<gpui::SharedString> {
    release
        .versions
        .iter()
        .find(|known| known.v.as_ref() == version)
        .map(|known| known.v.clone())
}

#[cfg(test)]
mod tests {
    use super::{bounded_history, item_path, semver_slip};
    use crate::host::registry::{Availability, Published, RegistryFact, Release};

    #[test]
    fn item_names_keep_module_and_source_context_without_root_paths() {
        assert_eq!(
            item_path(
                "/cache/toml-0.8.23/src/value.rs:25::Value",
                "/cache/toml-0.8.23",
                "/cache/toml-0.5.11"
            ),
            "src/value.rs:25::Value"
        );
        assert_eq!(
            item_path("toml::Value", "/cache/toml-0.8.23", "/cache/toml-0.5.11"),
            "toml::Value"
        );
    }

    #[test]
    fn semver_slip_uses_strict_cargo_caret_compatibility() {
        assert!(semver_slip("1.2.3", "1.9.0", true));
        assert!(!semver_slip("1.2.3", "2.0.0", true));
        assert!(semver_slip("0.3.1", "0.3.9", true));
        assert!(!semver_slip("0.3.1", "0.4.0", true));
        assert!(!semver_slip("0.0.1", "0.0.2", true));
        assert!(!semver_slip("1.2.3-alpha.1", "1.2.4-beta.1", true));
        assert!(!semver_slip("v1.2.3", "1.2.4", true));
        assert!(!semver_slip("1.2.3+one", "1.2.3+two", true));
        assert!(!semver_slip("1.2.3", "1.2.4", false));
    }

    #[test]
    fn bounded_history_keeps_exact_pin_and_selection_identities() {
        let published = (0..300)
            .map(|minor| Published {
                release: Release::new("sample", &format!("1.{minor}.0")).expect("release"),
                date: RegistryFact::Missing,
                yanked: RegistryFact::Missing,
                availability: Availability::Download,
            })
            .collect::<Vec<_>>();
        let pinned = Release::new("sample", "1.3.0").expect("pin");
        let selected = Release::new("sample", "1.4.0").expect("selection");
        let (shown, truncated) = bounded_history(&published, [Some(&pinned), Some(&selected)]);
        let exact = shown
            .iter()
            .map(|entry| entry.release.version.as_str())
            .collect::<Vec<_>>();
        assert!(truncated);
        assert_eq!(shown.len(), 256);
        assert!(exact.contains(&"1.3.0"));
        assert!(exact.contains(&"1.4.0"));
        assert!(exact.contains(&"1.299.0"));
    }
}
