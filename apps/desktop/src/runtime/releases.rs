//! Exact local registry release history and semantic comparisons.
//!
//! The registry index answers which versions exist and which source trees are
//! already on this machine. The local owner answers which exact trees it has
//! indexed and compares those trees through its semantic diff command. Missing
//! sources and missing comparisons stay missing; a release label is never
//! treated as evidence that another version has the same API.

use crate::core::VersionedRoot;
use crate::host::registry::{Availability, Composition, RegistryFact};
use crate::model::pages::PackageRef;
use crate::runtime::offload::{Answer, Memo};
use backend_client::Session;
use backend_library::{CommandReply, DeclarationChange, PackageReference};
use facet::data::release::{
    Change, Crate, RegistryFact as FacetRegistryFact, ReleaseDiff, Severity, SourceAvailability,
    Version, What,
};
use gpui::{Context, Global};
use std::collections::HashSet;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;

const RELEASE_READS: usize = 12;

/// Exact key for a local release read. The owner endpoint and producer
/// authority prevent data from another owner or an older index root from
/// being presented as current.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ReleaseKey {
    package: PackageRef,
    root: VersionedRoot,
    endpoint: PathBuf,
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
        Self(Memo::new(
            NonZeroUsize::new(RELEASE_READS).expect("positive capacity"),
            read,
        ))
    }
}

/// A release request's nonblocking result. `Unavailable` is a real failure or
/// missing provider, while `Reading` means the exact request is in flight.
pub(crate) enum Read {
    Reading,
    Ready(Arc<ReleaseData>),
    Unavailable(Arc<str>),
}

/// Reads exact release data for the current producer root. Work runs off the
/// UI thread and only the view that asked is notified when it lands.
pub(crate) fn get<T: 'static>(
    package: &PackageRef,
    root: VersionedRoot,
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
    };
    let memo = cx.global::<ReleaseReads>().0.clone();
    match memo.get(&key, cx) {
        Answer::Reading => Read::Reading,
        Answer::Failed(fault) => Read::Unavailable(Arc::from(fault.to_string())),
        Answer::Ready(value) => match value.as_ref() {
            Ok(data) => Read::Ready(Arc::clone(data)),
            Err(reason) => Read::Unavailable(Arc::clone(reason)),
        },
    }
}

fn read(key: &ReleaseKey) -> Result<Arc<ReleaseData>, Arc<str>> {
    let composition = crate::host::registry::composed()
        .filter(|composition| composition.endpoint == key.endpoint)
        .ok_or_else(|| {
            Arc::from("the local service connection changed before releases were read")
        })?;
    let pinned = key
        .package
        .release()
        .ok_or_else(|| Arc::from("this package is not an exact registry release"))?;
    let published = composition.source.releases(&pinned.name);
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
    if revision.root != key.root.root() {
        return Err(Arc::from(
            "the local index changed before the release read began",
        ));
    }
    let listed = match session
        .packages()
        .map_err(|error| Arc::<str>::from(format!("could not read indexed packages: {error}")))?
        .reply
    {
        CommandReply::Packages(snapshot) => snapshot
            .root
            .rows()
            .iter()
            .filter_map(|row| match row.id {
                backend_library::RowId::Package(_) => PackageRef::parse(&row.label).ok(),
                _ => None,
            })
            .collect::<HashSet<_>>(),
        _ => {
            return Err(Arc::from(
                "the local service returned an unexpected package listing",
            ));
        }
    };

    let Some(from) = exact_owner_reference(&key.package, &listed, &composition) else {
        return Err(Arc::from(
            "the pinned registry release is not present in the local index",
        ));
    };
    let mut diffs = Vec::new();
    let mut any_unread_source = false;
    let mut any_unindexed_source = false;
    let mut ambiguous_sources = Vec::new();
    let mut unverified_archives = Vec::new();
    for release in published.iter().filter(|release| release.release != pinned) {
        match &release.availability {
            Availability::Download => {
                any_unread_source = true;
                continue;
            }
            Availability::Archive(_) => {
                // The archive is on disk but is not yet an indexed source
                // tree. Indexing it is a user action, not a render side effect.
                any_unindexed_source = true;
                continue;
            }
            Availability::Ambiguous { indexes } => {
                ambiguous_sources.extend(indexes.iter().map(|path| path.display().to_string()));
                continue;
            }
            Availability::UnverifiedArchive(path) => {
                unverified_archives.push(path.display().to_string());
                continue;
            }
            Availability::Unpacked(_) => {}
        }
        let Some(tree) = composition.source.tree_of(&release.release) else {
            any_unindexed_source = true;
            continue;
        };
        if !tree.join("Cargo.toml").is_file() {
            any_unindexed_source = true;
            continue;
        }
        let Some(to) = tree
            .to_str()
            .and_then(|tree| PackageRef::parse(tree).ok())
            .filter(|candidate| listed.contains(candidate))
            .and_then(|candidate| PackageReference::parse(candidate.as_str().to_owned()).ok())
        else {
            any_unindexed_source = true;
            continue;
        };
        let from_owner = PackageReference::parse(from.as_str().to_owned())
            .map_err(|_| Arc::<str>::from("the pinned package identity is invalid"))?;
        let records = match session.diff(from_owner, to.clone()) {
            Ok(records) => records,
            Err(_) => {
                any_unindexed_source = true;
                continue;
            }
        };

        // The facet model has no partial-diff state. Do not quietly discard
        // overload ambiguity and then call the remainder a complete diff.
        if records
            .iter()
            .any(|record| record.change == DeclarationChange::Indeterminate)
        {
            any_unindexed_source = true;
            continue;
        }
        let changes = records
            .iter()
            .filter_map(|record| {
                let (what, severity) = match record.change {
                    DeclarationChange::Added => (What::Added, Severity::Additive),
                    DeclarationChange::Removed => (What::Removed, Severity::Breaking),
                    DeclarationChange::Changed => (What::Changed, Severity::Breaking),
                    DeclarationChange::Indeterminate => return None,
                };
                Some(Change {
                    path: item_path(record.label.as_str(), from.as_str(), to.as_str()).into(),
                    what,
                    severity,
                    // DiffRecord currently carries identities and change
                    // kinds, not captured before/after signature text.
                    before: None,
                    after: None,
                })
            })
            .collect::<Vec<_>>();
        let from_version = pinned.version.as_str();
        let to_version = release.release.version.as_str();
        let semver_slip = semver_cmp(from_version, to_version).is_lt()
            && same_caret_class(from_version, to_version)
            && changes
                .iter()
                .any(|change| change.severity == Severity::Breaking);
        diffs.push(ReleaseDiff {
            from: from_version.to_owned().into(),
            to: to_version.to_owned().into(),
            changes,
            semver_slip,
        });
    }
    let after = session.revision().map_err(|error| {
        Arc::<str>::from(format!("could not confirm the index revision: {error}"))
    })?;
    if after.root != key.root.root() {
        return Err(Arc::from(
            "the local index changed while releases were being compared",
        ));
    }

    let versions = published
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
    if any_unread_source {
        notes.push("some releases need a download before they can be compared".to_owned());
    }
    if any_unindexed_source {
        notes.push("some local releases have no exact indexed comparison".to_owned());
    }
    ambiguous_sources.sort();
    ambiguous_sources.dedup();
    if !ambiguous_sources.is_empty() {
        notes.push(format!(
            "some releases have conflicting registry sources ({}); resolve the Cargo registry authority",
            ambiguous_sources.join(", ")
        ));
    }
    unverified_archives.sort();
    unverified_archives.dedup();
    if !unverified_archives.is_empty() {
        notes.push(format!(
            "some local archives have no trusted checksum and cannot be unpacked ({})",
            unverified_archives.join(", ")
        ));
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

/// Finds the exact owner package identity for this release. A local cache
/// root may be indexed under its canonical path even when the page was opened
/// through its package URL; matching the version coordinate is explicit and
/// ambiguity-safe.
fn exact_owner_reference(
    package: &PackageRef,
    listed: &HashSet<PackageRef>,
    composition: &Composition,
) -> Option<PackageRef> {
    if listed.contains(package) {
        return Some(package.clone());
    }
    let pinned = package.release()?;
    let matches = listed
        .iter()
        .filter(|candidate| candidate.release().as_ref() == Some(&pinned))
        .cloned()
        .collect::<Vec<_>>();
    if matches.len() == 1 {
        return matches.into_iter().next();
    }
    if matches.len() > 1 {
        return None;
    }
    let tree = composition.source.tree_of(&pinned)?;
    let candidate = PackageRef::parse(tree.to_str()?).ok()?;
    listed.contains(&candidate).then_some(candidate)
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

/// Whether two exact versions occupy the same Cargo caret compatibility
/// class. Invalid spellings conservatively never establish compatibility.
fn same_caret_class(from: &str, to: &str) -> bool {
    let (Some(a), Some(b)) = (parse_version(from), parse_version(to)) else {
        return false;
    };
    match a {
        (major, _, _) if major > 0 => a.0 == b.0,
        (0, minor, _) if minor > 0 => b.0 == 0 && b.1 == minor,
        (0, 0, patch) => b == (0, 0, patch),
        _ => false,
    }
}

fn semver_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let a = parse_version(a).unwrap_or_default();
    let b = parse_version(b).unwrap_or_default();
    a.cmp(&b)
}

fn parse_version(value: &str) -> Option<(u64, u64, u64)> {
    let core = value.split(['+', '-']).next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    parts.next().is_none().then_some((major, minor, patch))
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
    use super::{item_path, same_caret_class};

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
    fn caret_classes_follow_zero_major_rules() {
        assert!(same_caret_class("1.2.3", "1.9.0"));
        assert!(!same_caret_class("1.2.3", "2.0.0"));
        assert!(same_caret_class("0.3.1", "0.3.9"));
        assert!(!same_caret_class("0.3.1", "0.4.0"));
        assert!(same_caret_class("0.0.1", "0.0.1"));
        assert!(!same_caret_class("0.0.1", "0.0.2"));
    }
}
