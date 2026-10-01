//! Reads for the browsing pages: one `project-tree` round trip, lowered to
//! the words every surface shares.

use super::reads::failure;
use crate::model::browse::{
    BrowseKey, BrowseValue, TreeDestination, TreeModel, TreeReleaseLink, TreeRoleLinks,
    TreeRowLinks,
};
use crate::model::pages::{PackageRef, PageValue, ReadFailure};
use backend_library::{ProductText, SurfaceCommand, SurfaceReply, browse::RoleId};
use backend_present::Engine;
use facet::browse::{Alert, AlertTone, LibraryModel, LibraryReleaseLink, LibraryRole, LibraryRow, Twice};
use facet::browse::library::ReleaseHandle;
use facet::icons::Icon;
use gpui::SharedString;
use std::collections::BTreeMap;
use std::sync::Arc;

/// A path may wrap between hops, never inside one: `toml 1.1.5` stays whole.
fn unbroken_hops(path: &str) -> SharedString {
    path.split(" → ")
        .map(|hop| hop.replace(' ', "\u{a0}"))
        .collect::<Vec<_>>()
        .join(" → ")
        .into()
}

fn key_part(into: &mut String, part: &str) {
    use std::fmt::Write as _;
    let _ = write!(into, "{}:", part.len());
    into.push_str(part);
}

/// Exact release source, including registry authority, participates in the
/// element key. A change in availability does not change source identity.
fn release_key(
    version: &str,
    reference: Option<&backend_library::PackageReference>,
    origin: Option<&backend_library::browse::PackageOrigin>,
) -> SharedString {
    use backend_library::browse::PackageOrigin;
    let mut key = String::new();
    key_part(&mut key, version);
    if let Some(reference) = reference {
        key_part(&mut key, "qualified");
        key_part(&mut key, reference.as_str());
    } else {
        match origin {
            Some(PackageOrigin::Registry { source }) => { key_part(&mut key, "registry"); key_part(&mut key, source); }
            Some(PackageOrigin::Git { source }) => { key_part(&mut key, "git"); key_part(&mut key, source); }
            Some(PackageOrigin::Vendored { path }) => { key_part(&mut key, "vendored"); key_part(&mut key, path); }
            Some(PackageOrigin::Unresolved { source }) => {
                key_part(&mut key, "unresolved");
                key_part(&mut key, source.as_deref().unwrap_or("unknown"));
            }
            None => key_part(&mut key, "unknown"),
        }
    }
    key.into()
}

fn row_key_from_releases(name: &str, releases: &[TreeReleaseLink]) -> SharedString {
    let mut key = String::new();
    key_part(&mut key, name);
    let mut exact = releases.iter().map(|release| release.key.as_ref()).collect::<Vec<_>>();
    exact.sort_unstable();
    for release in exact {
        key_part(&mut key, release);
    }
    key.into()
}

pub(crate) fn row_key(row: &backend_present::RowReading, links: Option<&crate::model::browse::TreeRowLinks>) -> SharedString {
    if let Some(links) = links.filter(|links| links.name.as_ref() == row.name.as_str()) {
        return row_key_from_releases(&row.name, &links.releases);
    }
    let mut key = String::new();
    key_part(&mut key, &row.name);
    let mut exact = row.versions.iter().enumerate().map(|(at, version)| {
        release_key(version, row.sources.get(at).and_then(Option::as_ref), row.origins.get(at)).to_string()
    }).collect::<Vec<_>>();
    exact.sort_unstable();
    for release in exact { key_part(&mut key, &release); }
    key.into()
}

/// Convert the owner's already admitted tree on the read worker. Render
/// callbacks only borrow the result, even when the project has many packages.
fn prepared_library_model(reading: &backend_present::TreeReading, links: &[TreeRoleLinks]) -> LibraryModel {
    let mut say = |text: &str| -> SharedString { text.to_owned().into() };
    let alerts = reading
        .alerts
        .iter()
        .map(|alert| {
            let tone = if alert.title.ends_with("is unmaintained") || alert.title.ends_with("has a notice") {
                AlertTone::Warn
            } else {
                AlertTone::Fault
            };
            let advisory = match &alert.summary {
                Some(summary) => format!("{} · {summary}", alert.id),
                None => alert.id.clone(),
            };
            // The card's lines are not on screen until it opens: not said.
            Alert { title: say(&alert.title), advisory: advisory.into(), path: unbroken_hops(&alert.why), tone }
        })
        .collect();
    let mut facts = Vec::new();
    if let Some(twice) = &reading.twice_line {
        facts.push(say(twice));
    }
    facts.push(say(&reading.health));
    let roles = reading
        .roles
        .iter()
        .enumerate()
        .map(|(role_at, role)| LibraryRole {
            key: role.id.as_str().into(),
            icon: match role.id {
                RoleId::Tests => Icon::ShieldCheck,
                RoleId::Window => Icon::Mosaic,
                RoleId::Languages => Icon::Book,
                RoleId::Formats => Icon::Split,
                RoleId::Store => Icon::Search,
                RoleId::Concurrency => Icon::Zap,
                RoleId::Network => Icon::Globe,
                RoleId::Hashing => Icon::Seal,
                RoleId::Errors => Icon::Alert,
                RoleId::Observe => Icon::Eye,
                RoleId::Memory => Icon::Diamond,
                RoleId::Os => Icon::Settings,
                RoleId::Other => Icon::Layers,
            },
            label: say(role.label),
            serving: role.serving.as_deref().map(&mut say),
            rows: role
                .rows
                .iter()
                .enumerate()
                .map(|(row_at, row)| {
                    let stable_row_key = row_key(row, links.get(role_at)
                        .filter(|links| links.role == role.id)
                        .and_then(|links| links.rows.get(row_at))
                        .filter(|links| links.name.as_ref() == row.name));
                    LibraryRow {
                    key: stable_row_key.clone(),
                    name: say(&row.name),
                    at_rest: row.at_rest.as_deref().map(&mut say),
                    why: row.evidence.clone().into(),
                    about: row.description.clone().map(SharedString::from),
                    releases: row.versions.iter().enumerate().map(|(version_at, version)| {
                        let destination = links.get(role_at)
                            .filter(|links| links.role == role.id)
                            .and_then(|links| links.rows.get(row_at))
                            .filter(|links| links.name.as_ref() == row.name)
                            .and_then(|links| links.releases.get(version_at))
                            .filter(|link| link.version.as_ref() == version);
                        let key: SharedString = destination
                            .map(|link| link.key.to_string().into())
                            .unwrap_or_else(|| release_key(version, row.sources.get(version_at).and_then(Option::as_ref), row.origins.get(version_at)));
                        match destination.map(|link| &link.destination) {
                            Some(TreeDestination::Open(_package)) => LibraryReleaseLink {
                                key: key.clone(),
                                version: version.clone().into(),
                                target: Some(ReleaseHandle::identified(role_at, row_at, version_at,
                                    role.id.as_str(), stable_row_key.clone(), key.clone())),
                                unavailable: None,
                                source_detail: destination.and_then(|link| link.source_detail.as_ref()).map(|detail| detail.to_string().into()),
                            },
                            Some(TreeDestination::Unavailable(reason)) => LibraryReleaseLink {
                                key,
                                version: version.clone().into(),
                                target: None,
                                unavailable: Some(reason.to_string().into()),
                                source_detail: destination.and_then(|link| link.source_detail.as_ref()).map(|detail| detail.to_string().into()),
                            },
                            None => LibraryReleaseLink {
                                key,
                                version: version.clone().into(),
                                target: None,
                                unavailable: Some("The source for this release was not resolved.".into()),
                                source_detail: None,
                            },
                        }
                    }).collect(),
                }})
                .collect(),
            brings: role.brings.as_deref().map(&mut say),
        })
        .collect();
    // Yours first, then the shortest path in, then by name: what you can act on leads.
    let mut twice = reading.twice.iter().collect::<Vec<_>>();
    twice.sort_by_key(|twice| {
        let yours = twice.copies.iter().any(|(_, yours)| *yours);
        let shortest = twice.paths.iter().map(|path| path.matches(" → ").count()).min().unwrap_or(usize::MAX);
        (!yours, shortest, twice.name.clone())
    });
    // Paths and verdicts unfold on a click: they are not said at rest.
    let twice = twice
        .into_iter()
        .enumerate()
        .map(|(at, twice)| {
            let shown = at < facet::browse::TWICE_AT_REST;
            let mut said = |text: &str| if shown { say(text) } else { SharedString::from(text.to_owned()) };
            Twice {
            name: said(&twice.name),
            copies: twice.copies.iter().map(|(version, yours)| (said(version), *yours)).collect(),
            paths: twice.paths.iter().map(|path| unbroken_hops(path)).collect(),
            verdict: twice.verdict.clone().into(),
        }})
        .collect::<Vec<_>>();
    LibraryModel {
        name: say(&reading.name),
        lede: say(&reading.lede),
        lede_tip: reading.locked_inactive_note.clone().map(SharedString::from),
        note: reading.source_note.as_deref().map(&mut say),
        alerts,
        facts,
        roles,
        twice_heading: reading.twice_heading.as_ref().map(|(title, caption)| (say(title), say(caption))),
        twice,
    }
}


/// Reads one browsing resource.
///
/// # Errors
/// The owner's failure to read the tree, as a typed fault.
pub fn compose(engine: &mut dyn Engine, key: &BrowseKey) -> Result<PageValue, ReadFailure> {
    match key {
        BrowseKey::Tree(project) => {
            let root = project.service_coordinate().map_err(|error| {
                ReadFailure::Fault(crate::core::ErrorValue::new(
                    crate::core::FaultCode::Protocol,
                    format!("the project path cannot be sent to the owner: {error:?}"),
                ))
            })?;
            let root = ProductText::new(root).map_err(|error| {
                ReadFailure::Fault(crate::core::ErrorValue::new(crate::core::FaultCode::Protocol, error.to_string()))
            })?;
            match engine.surface(SurfaceCommand::ProjectTree { root }).map_err(|error| failure(&error))? {
                SurfaceReply::ProjectTree(tree) => Ok(PageValue::Browse(BrowseValue::Tree(Arc::new(tree_model(&tree))))),
                _ => Err(ReadFailure::Fault(crate::core::ErrorValue::new(
                    crate::core::FaultCode::Protocol,
                    "the project-tree reply changed shape",
                ))),
            }
        }
        BrowseKey::FindHome | BrowseKey::Find(_) | BrowseKey::Compare(_) => Err(ReadFailure::Fault(crate::core::ErrorValue::new(
            crate::core::FaultCode::Protocol,
            "find and compare require the page reader's cancellable read context",
        ))),
    }
}

/// The Library page's model of one tree.
#[must_use]
pub fn tree_model(tree: &backend_library::browse::ProjectTree) -> TreeModel {
    let reading = backend_present::read_tree(tree);
    let sources = TreeSources::new(tree);
    let links: Arc<[TreeRoleLinks]> = reading.roles.iter().map(|role| TreeRoleLinks {
        role: role.id,
        rows: role.rows.iter().map(|row| {
            let releases: Arc<[TreeReleaseLink]> = {
                let mut copies = BTreeMap::<&str, usize>::new();
                for version in &row.versions { *copies.entry(version).or_default() += 1; }
                let mut same_identity = BTreeMap::<String, usize>::new();
                row.versions.iter().enumerate().map(|(at, version)| {
                    let reference = row.sources.get(at).and_then(Option::as_ref);
                    let base = release_key(version, reference, row.origins.get(at));
                    let occurrence = same_identity.entry(base.to_string()).or_default();
                    let key: Arc<str> = Arc::from(format!("{base}#{}", *occurrence));
                    *occurrence += 1;
                    let source_detail = (copies.get(version.as_str()).copied().unwrap_or_default() > 1)
                        .then(|| row.origins.get(at).and_then(origin_detail)).flatten();
                    TreeReleaseLink {
                        version: Arc::from(version.as_str()),
                        key,
                        destination: sources.destination(&row.name, version, reference),
                        source_detail,
                    }
                }).collect::<Vec<_>>().into()
            };
            TreeRowLinks {
                name: Arc::from(row.name.as_str()),
                key: Arc::from(row_key_from_releases(&row.name, &releases).to_string()),
                releases,
            }
        }).collect::<Vec<_>>().into(),
    }).collect::<Vec<_>>().into();
    let prepared = Arc::new(prepared_library_model(&reading, &links));
    TreeModel {
        root: Arc::from(tree.root.as_str()),
        reading,
        links,
        prepared,
    }
}

/// The display row is admitted only through its aligned exact Cargo metadata
/// reference. Equal name/version text may identify multiple registry or Git
/// sources; the authority digest keeps those releases separate.
struct TreeSources<'a> {
    by_reference: BTreeMap<backend_library::PackageReference, Vec<&'a backend_library::browse::TreePackage>>,
}

impl<'a> TreeSources<'a> {
    fn new(tree: &'a backend_library::browse::ProjectTree) -> Self {
        let mut by_reference = BTreeMap::new();
        for package in &tree.packages {
            if let Some(reference) = package.source_qualified_reference() {
                by_reference.entry(reference).or_insert_with(Vec::new).push(package);
            }
        }
        Self { by_reference }
    }

    fn exact(
        &self,
        name: &str,
        version: &str,
        reference: Option<&backend_library::PackageReference>,
    ) -> Result<&'a backend_library::browse::TreePackage, &'static str> {
        let Some(reference) = reference else {
            return Err("This release has no current exact Cargo source receipt.");
        };
        let Some(packages) = self.by_reference.get(reference) else {
            return Err("The exact Cargo package was not in this project tree.");
        };
        match packages.as_slice() {
            [package] if package.name == name && package.version == version => Ok(package),
            [_] => Err("The source receipt does not match this displayed release."),
            _ => Err("More than one Cargo package matches this source receipt."),
        }
    }

    fn destination(&self, name: &str, version: &str, reference: Option<&backend_library::PackageReference>) -> TreeDestination {
        if let Err(reason) = self.exact(name, version, reference) {
            return TreeDestination::Unavailable(Arc::from(reason));
        }
        reference.map_or_else(
            || TreeDestination::Unavailable(Arc::from("This release has no current exact Cargo source receipt.")),
            |reference| TreeDestination::Open(PackageRef::from_reference(reference.clone())),
        )
    }
}

fn origin_detail(origin: &backend_library::browse::PackageOrigin) -> Option<Arc<str>> {
    use backend_library::browse::PackageOrigin;
    Some(match origin {
        PackageOrigin::Registry { source } => Arc::from(source.as_str()),
        PackageOrigin::Git { source } => Arc::from(source.as_str()),
        PackageOrigin::Vendored { path } => Arc::from(format!("path: {path}")),
        PackageOrigin::Unresolved { source } => source.as_deref().map(Arc::from)?,
    })
}

/// Merge real package identities once, off the UI thread. Exact names lead;
/// local indexed evidence breaks ties, never popularity or invented fitness.
pub(super) fn find_packages(
    query: &str,
    indexed: &[&backend_library::Row],
    catalog: &[backend_library::RegistryPackageRecord],
    source: Option<&dyn crate::host::registry::RegistrySource>,
) -> Vec<crate::model::browse::FindPackage> {
    use crate::model::browse::FindPackage;
    use crate::model::pages::PackageRef;
    use std::collections::BTreeMap;
    let mut candidates = BTreeMap::<PackageRef, FindPackage>::new();
    for row in indexed {
        if !matches!(row.id, backend_library::RowId::Package(_)) { continue; }
        let Ok(package) = PackageRef::parse(&row.label) else { continue; };
        let offer = source.and_then(|source| indexed_release(source, &package));
        // A registry tree is named as its crate, not its directory.
        let name: Arc<str> = offer.as_ref().map_or_else(|| Arc::from(package.display_name()), |offer| Arc::from(offer.release.name.as_str()));
        candidates.insert(package.clone(), FindPackage { package, name, description: None, indexed: true, record: None, offer });
    }
    for row in catalog {
        let record = super::page_mapping::registry_record(row);
        let candidate = candidates.entry(record.package.clone()).or_insert_with(|| FindPackage {
            package: record.package.clone(), name: record.name.clone(), description: None, indexed: false, record: None, offer: None,
        });
        candidate.name = record.name.clone();
        candidate.description = record.description.known().cloned();
        candidate.record = Some(record);
    }
    if let Some(source) = source {
        offer_releases(query, source, &mut candidates);
    }
    rank_packages(query, candidates.into_values().collect())
}

/// How many releases of crates this machine has one query offers.
const OFFERS: usize = 16;

/// The release an indexed root is, when it is one of the source's trees: it
/// is in the library already.
fn indexed_release(source: &dyn crate::host::registry::RegistrySource, package: &crate::model::pages::PackageRef) -> Option<crate::model::release::Offer> {
    use crate::model::release::{Availability, Offer};
    if !package.is_local() {
        return None;
    }
    let root = std::path::Path::new(package.as_str());
    let release = source.release_of(root)?;
    Some(Offer { release, availability: Availability::Unpacked(root.to_path_buf()), library: Some(package.clone()) })
}

/// Crates this machine can supply without the network, at their newest
/// release here, which the library does not have yet: each is an offer to
/// add it, keyed by its package URL, with its manifest's description.
fn offer_releases(
    query: &str,
    source: &dyn crate::host::registry::RegistrySource,
    candidates: &mut std::collections::BTreeMap<crate::model::pages::PackageRef, crate::model::browse::FindPackage>,
) {
    use crate::model::browse::FindPackage;
    use crate::model::pages::PackageRef;
    use crate::model::release::{Availability, Offer};
    for release in source.offline(query, OFFERS) {
        if candidates.values().any(|candidate| candidate.offer.as_ref().is_some_and(|offer| offer.release == release)) {
            continue;
        }
        let Ok(package) = PackageRef::parse(&release.purl()) else { continue };
        let availability = source.availability(&release);
        let description = match &availability {
            Availability::Unpacked(tree) => crate::model::source_facts::manifest::read(tree).and_then(|manifest| manifest.description).map(Arc::from),
            Availability::Archive(_)
            | Availability::UnverifiedArchive(_)
            | Availability::Ambiguous { .. }
            | Availability::Download => None,
        };
        let name = Arc::from(release.name.as_str());
        let candidate = candidates.entry(package.clone()).or_insert_with(|| FindPackage { package, name, description: None, indexed: false, record: None, offer: None });
        if candidate.description.is_none() {
            candidate.description = description;
        }
        candidate.offer = Some(Offer { release, availability, library: None });
    }
}

fn rank_packages(query: &str, mut candidates: Vec<crate::model::browse::FindPackage>) -> Vec<crate::model::browse::FindPackage> {
    let query = query.trim().to_lowercase();
    candidates.retain(|candidate| candidate.name.to_lowercase().contains(&query)
        || candidate.description.as_ref().is_some_and(|description| description.to_lowercase().contains(&query)));
    candidates.sort_by_cached_key(|candidate| {
        let name = candidate.name.to_lowercase();
        (name != query, !name.starts_with(&query), !candidate.indexed, name, candidate.package.clone())
    });
    candidates.truncate(64);
    candidates
}

#[cfg(test)]
mod find_tests {
    use super::*;
    use crate::model::browse::FindPackage;
    use crate::model::pages::PackageRef;

    #[test]
    fn unavailable_release_keys_preserve_exact_registry_transport_and_git_commit() {
        use backend_library::browse::PackageOrigin;
        let registry = PackageOrigin::Registry { source: "registry+https://registry.example/index".to_owned() };
        let sparse = PackageOrigin::Registry { source: "sparse+https://registry.example/index".to_owned() };
        let git_a = PackageOrigin::Git { source: "git+https://example.test/lib?branch=main#aaaaaaaa".to_owned() };
        let git_b = PackageOrigin::Git { source: "git+https://example.test/lib?branch=main#bbbbbbbb".to_owned() };
        assert_ne!(release_key("1.0.0", None, Some(&registry)), release_key("1.0.0", None, Some(&sparse)));
        assert_ne!(release_key("1.0.0", None, Some(&git_a)), release_key("1.0.0", None, Some(&git_b)));
        assert_ne!(release_key("1.0.0", None, Some(&registry)), release_key("1.0.0", None, Some(&git_a)));
    }

    fn candidate(name: &str, indexed: bool) -> FindPackage {
        FindPackage { package: PackageRef::parse(&format!("pkg:cargo/{name}@1.0.0")).unwrap(), name: Arc::from(name), description: None, indexed, record: None, offer: None }
    }

    #[test]
    fn exact_package_names_beat_indexed_partial_matches_and_keep_identity() {
        let ranked = rank_packages("TOML", vec![candidate("toml_edit", true), candidate("toml", false), candidate("serde", true)]);
        assert_eq!(ranked.iter().map(|row| row.name.as_ref()).collect::<Vec<_>>(), ["toml", "toml_edit"]);
        assert!(!ranked[0].indexed);
        assert_eq!(ranked[0].package.as_str(), "pkg:cargo/toml@1.0.0");
    }

    #[test]
    fn unmatched_packages_are_empty_and_large_catalogs_are_bounded() {
        assert!(rank_packages("not-present", vec![candidate("toml", true)]).is_empty());
        let rows = (0..200).map(|at| candidate(&format!("item{at:03}"), true)).collect();
        let ranked = rank_packages("item", rows);
        assert_eq!(ranked.len(), 64);
        assert_eq!(ranked.last().unwrap().name.as_ref(), "item063");
    }

    #[test]
    fn tree_links_use_only_exact_owner_observed_cargo_source_receipts() {
        use backend_advisory::{AdvisoryAuthority, normalize_package};
        use backend_library::browse::{build_tree, metadata_input};
        const METADATA: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../crates/library/browse/fixtures/tree-2026-09-27/metadata.json"));
        const LOCKFILE: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../crates/library/browse/fixtures/tree-2026-09-27/Cargo.lock"));
        let input = metadata_input(METADATA, "aarch64-apple-darwin", Some(LOCKFILE)).expect("actual Cargo metadata fixture");
        let authority = AdvisoryAuthority::new(1);
        let observe = |name: &str, version: &str| {
            let package = normalize_package("cargo", name).expect("test package");
            authority.observe(&package, version, false, false, 0, false)
        };
        let mut tree = build_tree(&input, &observe);
        let serde = tree.package("serde", "1.0.219").expect("resolved Cargo package").clone();
        let receipt = serde.source_qualified_reference().expect("owner receipt");
        let TreeDestination::Open(exact) = TreeSources::new(&tree).destination(&serde.name, &serde.version, Some(&receipt))
            else { panic!("complete metadata receipt opens its exact source") };
        assert_eq!(exact.reference(), &receipt);
        assert!(exact.as_str().contains("?cargo-authority="));
        assert_ne!(exact.as_str(), "pkg:cargo/serde@1.0.219");

        assert!(matches!(TreeSources::new(&tree).destination(&serde.name, &serde.version, None), TreeDestination::Unavailable(_)));
        let different = backend_library::PackageReference::parse(
            "pkg:cargo/serde@1.0.219?cargo-authority=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        ).expect("other source address");
        assert!(matches!(TreeSources::new(&tree).destination(&serde.name, &serde.version, Some(&different)), TreeDestination::Unavailable(_)));
        tree.packages = tree.packages.iter().cloned().chain(std::iter::once(serde.clone())).collect();
        assert!(matches!(TreeSources::new(&tree).destination(&serde.name, &serde.version, Some(&receipt)), TreeDestination::Unavailable(_)), "ambiguous rows cannot lend each other file authority");
    }
}
