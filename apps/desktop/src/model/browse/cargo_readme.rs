//! Owner-selected README content and its bounded, worker-prepared addresses.

use crate::model::local_package::{ReadmeHeading, owner_readme_navigation, readme_external_address, readme_fragment_slug};
use crate::model::pages::PackageRef;
use crate::navigation::{CargoBrowseContext, CargoReadmeLinkAddress};
use backend_library::{CargoPackageReadmeAbsenceV1, CargoPackageReadmeLinkTargetV1, CargoPackageReadmeOriginV1};
use std::collections::BTreeMap;
use std::sync::Arc;

/// One exact package README selector, independent of its semantic dossier.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CargoReadmeKey {
    /// Exact requested project and owner-returned root commitments.
    pub context: CargoBrowseContext,
    /// Full source-qualified package observed by Cargo.
    pub package: PackageRef,
}

/// A prepared navigation hint still requires the README's current lease.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CargoReadmeDestination {
    /// A bounded supported external URI, admitted only at user activation.
    External(Arc<str>),
    /// One exact heading in this Markdown body.
    Anchor(ReadmeHeading),
    /// Owner-relative file address retaining the original root scope.
    Source(CargoReadmeLinkAddress),
    /// The authored link has no supported address in this bounded index.
    Unavailable(&'static str),
}

/// One bounded link row; its label does not establish target content.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CargoReadmeLink {
    /// Bounded exact-origin endpoint identity; occurrence only separates
    /// identical authored destinations within the same immutable document.
    pub id: Arc<str>,
    /// Authored label from the exact Markdown AST.
    pub label: Arc<str>,
    /// Exact href supplied back to the owner for a file read.
    pub href: Arc<str>,
    /// Prepared address projection, never target-file evidence.
    pub destination: CargoReadmeDestination,
}

/// Current Markdown bytes and an exact owner origin, prepared once on a worker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CargoReadmeDocument {
    /// Exact package/project/selection/scope/content receipt.
    pub origin: CargoPackageReadmeOriginV1,
    /// Complete bounded UTF-8 Markdown returned by the owner.
    pub source: Arc<str>,
    /// At most 512 authored link rows, with no client path hints.
    pub links: Arc<[CargoReadmeLink]>,
    /// At most 512 exact heading endpoints.
    pub headings: Arc<[ReadmeHeading]>,
    identity: Arc<str>,
    heading_ids: Arc<[Arc<str>]>,
    destinations: BTreeMap<Arc<str>, CargoReadmeDestination>,
}

/// A remembered focus ID resolved only against this exact immutable origin.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CargoReadmeFocus {
    Link(usize),
    Heading(usize),
}

impl CargoReadmeDocument {
    pub(crate) fn prepare(origin: CargoPackageReadmeOriginV1, source: Arc<str>) -> Self {
        let (authored, headings) = owner_readme_navigation(&source);
        let origin_key = origin_identity(&origin);
        let identity = endpoint_identity(&origin_key, "document", "", 0);
        let heading_ids = headings.iter().map(|heading| {
            endpoint_identity(&origin_key, "heading", &heading.slug, 0)
        }).collect::<Vec<_>>();
        let mut occurrences = BTreeMap::<Arc<str>, usize>::new();
        let mut destinations = BTreeMap::new();
        let links = authored.iter().map(|link| {
            let occurrence = occurrences.entry(Arc::clone(&link.destination)).or_default();
            let id = endpoint_identity(&origin_key, "link", &link.destination, *occurrence);
            *occurrence += 1;
            let destination = if let Some(url) = readme_external_address(&link.destination) {
                CargoReadmeDestination::External(Arc::from(url))
            } else {
                match origin.resolve_relative_href(&link.destination) {
                    Ok(CargoPackageReadmeLinkTargetV1::Anchor { fragment }) => {
                        let slug = readme_fragment_slug(&fragment);
                        let mut hits = headings.iter().filter(|heading| heading.slug.as_ref() == slug);
                        match (hits.next(), hits.next()) {
                            (Some(heading), None) => CargoReadmeDestination::Anchor(heading.clone()),
                            _ => CargoReadmeDestination::Unavailable("This heading is not in the bounded README index."),
                        }
                    }
                    Ok(CargoPackageReadmeLinkTargetV1::File { .. }) => CargoReadmeLinkAddress::new(origin.clone(), &link.destination)
                        .map_or(CargoReadmeDestination::Unavailable("This README link has no admitted relative address."), CargoReadmeDestination::Source),
                    Err(_) => CargoReadmeDestination::Unavailable("This README link is outside its supported package or workspace scope."),
                }
            };
            destinations.entry(Arc::clone(&link.destination)).or_insert_with(|| destination.clone());
            CargoReadmeLink { id, label: Arc::clone(&link.label), href: Arc::clone(&link.destination), destination }
        }).collect::<Vec<_>>();
        Self { origin, source, links: links.into(), headings, identity, heading_ids: heading_ids.into(), destinations }
    }

    pub(crate) fn identity(&self) -> &str { &self.identity }

    pub(crate) fn heading_id(&self, index: usize) -> Option<&str> {
        self.heading_ids.get(index).map(AsRef::as_ref)
    }

    /// The rich Markdown callback carries an href, not an occurrence. Equal
    /// hrefs resolve to the same endpoint; the first is the bounded focus tie.
    pub(crate) fn inline_focus_id(&self, href: &str) -> Option<&str> {
        self.links.iter().find(|link| link.href.as_ref() == href
            && !matches!(link.destination, CargoReadmeDestination::Unavailable(_)))
            .map(|link| link.id.as_ref())
    }

    /// No fallback to an ordinal: a different origin or target has no match.
    pub(crate) fn restore_focus(&self, id: &str) -> Option<CargoReadmeFocus> {
        if let Some(index) = self.links.iter().position(|link| link.id.as_ref() == id
            && !matches!(link.destination, CargoReadmeDestination::Unavailable(_))) {
            return Some(CargoReadmeFocus::Link(index));
        }
        self.heading_ids.iter().position(|current| current.as_ref() == id).map(CargoReadmeFocus::Heading)
    }

    /// Only indexed authored hrefs can be activated by the Markdown callback.
    pub(crate) fn destination(&self, href: &str) -> CargoReadmeDestination {
        self.destinations.get(href).cloned().unwrap_or(CargoReadmeDestination::Unavailable("This link was not captured in the bounded README index."))
    }
}

/// UI identity only. Neither this digest nor a native ID admits owner reads.
fn origin_identity(origin: &CargoPackageReadmeOriginV1) -> blake3::Hash {
    use backend_library::{CargoPackageReadmeRootScopeV1 as Scope, CargoPackageReadmeSelectionV1 as Selection};
    let mut hash = blake3::Hasher::new();
    hash.update(b"nudox-cargo-readme-focus-v1");
    identity_field(&mut hash, origin.package.as_str().as_bytes());
    hash.update(&origin.request_binding.schema.to_le_bytes());
    hash.update(&origin.request_binding.requested_root_digest);
    hash.update(&origin.request_binding.effective_workspace_root_digest);
    hash.update(&[match origin.root_scope { Scope::Package => 0, Scope::EffectiveWorkspace => 1 }]);
    identity_field(&mut hash, origin.path.as_str().as_bytes());
    hash.update(&[match origin.selection {
        Selection::ManifestPath => 0, Selection::ManifestTrueDefault => 1,
        Selection::CargoConventionalDefault => 2, Selection::WorkspaceInherited => 3,
    }]);
    hash.update(&origin.content_digest);
    hash.finalize()
}

fn identity_field(hash: &mut blake3::Hasher, bytes: &[u8]) {
    hash.update(&(bytes.len() as u64).to_le_bytes());
    hash.update(bytes);
}

fn endpoint_identity(origin: &blake3::Hash, kind: &str, endpoint: &str, occurrence: usize) -> Arc<str> {
    let mut hash = blake3::Hasher::new();
    hash.update(origin.as_bytes());
    identity_field(&mut hash, kind.as_bytes());
    identity_field(&mut hash, endpoint.as_bytes());
    hash.update(&(occurrence as u64).to_le_bytes());
    Arc::from(format!("cargo-readme-{kind}-{}", hash.finalize().to_hex()))
}

/// README content and exact absence are separate completed owner observations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CargoReadmeState {
    /// Complete Markdown and owner-held scope.
    Read(Arc<CargoReadmeDocument>),
    /// Exact manifest/default absence, never inferred from a file inventory.
    Absent(CargoPackageReadmeAbsenceV1),
}

/// One exact owner read, independent of semantic indexing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CargoReadmeModel {
    /// Exact source-qualified package returned by the owner.
    pub package: PackageRef,
    /// Full requested/effective root binding returned by this read.
    pub request_binding: backend_library::browse::ProjectTreeRequestBindingV1,
    /// Revalidated Cargo observation revision.
    pub source_revision: [u8; 32],
    /// Complete README or exact Cargo-selected absence.
    pub state: CargoReadmeState,
}

#[cfg(test)]
mod focus_tests {
    #![allow(clippy::expect_used, clippy::panic)]
    use super::*;

    fn document(contents: &str) -> CargoReadmeDocument {
        let (_, _, mut result) = crate::runtime::cargo_readme_reads::tests::fixture();
        let backend_library::CargoPackageReadmeResultV1::Read { readme, .. } = &mut result else { panic!("fixture read"); };
        readme.contents = contents.into();
        readme.content_digest = *blake3::hash(contents.as_bytes()).as_bytes();
        let origin = CargoPackageReadmeOriginV1::from_result(&result).expect("exact origin");
        CargoReadmeDocument::prepare(origin, Arc::from(contents))
    }

    #[test]
    fn back_focus_matches_exact_endpoint_and_never_the_new_ordinal() {
        let old = document("# Guide\n\n[Old target](../src/old.rs#L7)\n\n[Other](../src/other.rs)\n");
        let remembered = old.links[0].id.clone();
        assert_eq!(old.restore_focus(&remembered), Some(CargoReadmeFocus::Link(0)));
        let repeated = CargoReadmeDocument::prepare(old.origin.clone(), Arc::clone(&old.source));
        assert_eq!(repeated.restore_focus(&remembered), Some(CargoReadmeFocus::Link(0)), "repaint of the same immutable read keeps the endpoint");

        let new = document("# Guide\n\n[New target](../src/new.rs#L7)\n\n[Old target](../src/old.rs#L7)\n");
        assert_eq!(new.restore_focus(&remembered), None, "a remembered row cannot transfer to another origin or the replacement at ordinal zero");
        assert_eq!(new.restore_focus(old.heading_id(0).expect("old heading")), None, "even equal heading words require the same Markdown origin");
        assert_eq!(new.restore_focus("cargo-readme-link-0"), None, "legacy ordinal IDs have no semantic fallback");

        let mut reordered = old.clone();
        reordered.links = Arc::from([old.links[1].clone(), old.links[0].clone()]);
        assert_eq!(reordered.restore_focus(&remembered), Some(CargoReadmeFocus::Link(1)), "bounded presentation order never defines endpoint identity");
        assert_eq!(reordered.links[1].href, old.links[0].href);
    }

    #[test]
    fn duplicate_destinations_only_use_occurrence_as_a_tie_break_and_ids_are_bounded() {
        let document = document("# Guide\n\n[First](../src/lib.rs#L7)\n\n[Different](../src/other.rs)\n\n[Again](../src/lib.rs#L7)\n");
        assert_eq!(document.links[0].href, document.links[2].href);
        assert_ne!(document.links[0].id, document.links[2].id);
        assert_eq!(document.restore_focus(&document.links[2].id), Some(CargoReadmeFocus::Link(2)));
        assert_eq!(document.inline_focus_id("../src/lib.rs#L7"), Some(document.links[0].id.as_ref()), "inline navigation remembers an exact native endpoint rather than the Markdown block ordinal");
        for link in document.links.iter() { assert!(link.id.len() <= 90); }
        let mut changed = document.origin.clone();
        changed.request_binding.requested_root_digest = [3; 32];
        let other_request = CargoReadmeDocument::prepare(changed, Arc::clone(&document.source));
        assert_eq!(other_request.restore_focus(&document.links[0].id), None, "an equal package/path in another requested browse context cannot borrow focus");
        let mut changed = document.origin.clone();
        changed.path = backend_library::CargoPackageSourcePathV1::new("other/README.md").expect("different origin path");
        let other_origin = CargoReadmeDocument::prepare(changed, Arc::clone(&document.source));
        assert_eq!(other_origin.restore_focus(&document.links[0].id), None, "same href under another README directory has another endpoint");
    }
}
