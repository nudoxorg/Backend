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
    destinations: BTreeMap<Arc<str>, CargoReadmeDestination>,
}

impl CargoReadmeDocument {
    pub(crate) fn prepare(origin: CargoPackageReadmeOriginV1, source: Arc<str>) -> Self {
        let (authored, headings) = owner_readme_navigation(&source);
        let mut destinations = BTreeMap::new();
        let links = authored.iter().map(|link| {
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
            CargoReadmeLink { label: Arc::clone(&link.label), href: Arc::clone(&link.destination), destination }
        }).collect::<Vec<_>>();
        Self { origin, source, links: links.into(), headings, destinations }
    }

    /// Only indexed authored hrefs can be activated by the Markdown callback.
    pub(crate) fn destination(&self, href: &str) -> CargoReadmeDestination {
        self.destinations.get(href).cloned().unwrap_or(CargoReadmeDestination::Unavailable("This link was not captured in the bounded README index."))
    }
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
