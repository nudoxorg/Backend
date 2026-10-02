//! The exact visible resources and admitted Cargo observations of one route.

use super::{CargoReadAdmission, DataStore, OwnerAttachment, route_package, route_symbol};
use crate::core::{LocalProjectId, ReadPhase, Resource, VersionedRoot, admit_resource};
use crate::model::browse::{BrowseKey, BrowseValue, CargoSourceInventoryKey, TreeModel};
use crate::model::pages::{CargoSourceKey, PackageRef, PageKey, Stamp};
use crate::navigation::{CargoBrowseContext, Overlay, Route, View};
use std::sync::Arc;

/// One read-backed input's admission across the deferred intent boundary.
/// No-gate harness attachments are synthetic and prove no real-owner acceptance.
#[derive(Clone, Debug)]
pub(crate) struct RouteReadLease {
    route: Route,
    overlay: Option<Overlay>,
    root: VersionedRoot,
    attachment: Option<OwnerAttachment>,
    dependency: (PageKey, Stamp),
}

impl RouteReadLease {
    pub(crate) fn capture(store: &DataStore, dependency: (PageKey, Stamp)) -> Option<Self> {
        let snapshot = store.snapshot();
        let lease = Self { route: snapshot.route().clone(), overlay: snapshot.overlay(), root: snapshot.key(),
            attachment: store.current_owner_attachment(), dependency };
        lease.admits(store).then_some(lease)
    }

    pub(crate) fn admits(&self, store: &DataStore) -> bool {
        let snapshot = store.snapshot();
        snapshot.route() == &self.route && snapshot.overlay() == self.overlay
            && self.root.same_authority(snapshot.key()) && store.owner_serving()
            && store.current_owner_attachment() == self.attachment
            && self.attachment.as_ref().is_none_or(|attachment| store.admits_owner_attachment(attachment))
            && RouteDependencies::new(&self.route, self.overlay).admits_native_stamp(store, self.root, &self.dependency)
    }
}

/// A Cargo file and its independently arriving current path inventory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CargoSourceDependencies {
    pub(crate) file: CargoSourceKey,
    pub(crate) inventory: CargoSourceInventoryKey,
}

/// A short-lived projection of the selected current Tree read. Keeping this
/// object does not preserve admission: actions capture its key/stamp and lease.
pub(crate) struct CurrentTreeRead {
    model: Arc<TreeModel>,
    context: Option<CargoBrowseContext>,
    dependency: PageKey,
    stamp: Stamp,
}

impl CurrentTreeRead {
    pub(crate) fn model(&self) -> &TreeModel { &self.model }
    pub(crate) fn native_dependency(&self) -> (PageKey, Stamp) { (self.dependency.clone(), self.stamp) }
}

/// An exact package observation projected from this route's admitted resource.
/// This receipt is never persisted or used in place of event-time admission.
pub(crate) struct CurrentCargoPackage {
    context: CargoBrowseContext,
    package: PackageRef,
    dependency: PageKey,
    stamp: Stamp,
}

impl CurrentCargoPackage {
    pub(crate) fn context(&self) -> &CargoBrowseContext { &self.context }
    pub(crate) fn package(&self) -> &PackageRef { &self.package }
    pub(crate) fn native_dependency(&self) -> (PageKey, Stamp) { (self.dependency.clone(), self.stamp) }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TreeDependency {
    requested_project: LocalProjectId,
    expected: Option<CargoBrowseContext>,
    package: Option<PackageRef>,
}

/// One construction supplies fetching, residency, watching and body gathering.
/// Page readiness selects only content reads; optional chrome does not hold a
/// valid Symbol or Cargo file behind an unavailable semantic package dossier.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RouteDependencies {
    keys: Vec<PageKey>,
    content: Vec<PageKey>,
    cargo: Option<CargoSourceDependencies>,
    tree: Option<TreeDependency>,
}

impl RouteDependencies {
    pub(crate) fn new(route: &Route, overlay: Option<Overlay>) -> Self {
        let mut cargo = None;
        let mut tree = None;
        let (mut keys, content) = match overlay {
            Some(Overlay::Settings(_)) => (vec![PageKey::Health], Vec::new()),
            Some(Overlay::Inbox) => (Vec::new(), Vec::new()),
            _ => match route {
                Route::Orbit(crate::navigation::OrbitRoute::Browse(browse)) => {
                    let key = PageKey::Browse(browse.into());
                    if let crate::navigation::BrowseRoute::Tree(project) = browse {
                        tree = Some(TreeDependency { requested_project: project.clone(), expected: None, package: None });
                    }
                    // Find owns its editing surface even before its reply.
                    let content = tree.as_ref().map(|_| vec![key.clone()]).unwrap_or_default();
                    (vec![key], content)
                }
                Route::Orbit(_) => (vec![PageKey::Orbit, PageKey::Health], vec![PageKey::Orbit]),
                Route::World => (vec![PageKey::Orbit], Vec::new()),
                Route::Package(package) => {
                    if let Some(context) = &package.cargo {
                        let key = PageKey::Browse(BrowseKey::Tree(context.requested_project().clone()));
                        tree = Some(TreeDependency { requested_project: context.requested_project().clone(), expected: Some(context.clone()), package: route_package(route) });
                        let mut keys = route_package(route).map(PageKey::Package).into_iter().collect::<Vec<_>>();
                        keys.push(key.clone());
                        (keys, vec![key])
                    } else {
                        let keys = route_package(route).map(PageKey::Package).into_iter().collect::<Vec<_>>();
                        (keys.clone(), keys)
                    }
                }
                Route::CargoSource(source) => {
                    if let Some(context) = source.browse.context() {
                        PackageRef::parse(source.package.as_str()).ok().map(|package| {
                            let pair = CargoSourceDependencies {
                                file: CargoSourceKey { context: context.clone(), package: package.clone(), file: source.file.clone() },
                                inventory: CargoSourceInventoryKey { context: context.clone(), package },
                            };
                            let file = PageKey::CargoSource(pair.file.clone());
                            let keys = vec![file.clone(), PageKey::Browse(BrowseKey::CargoSourceInventory(pair.inventory.clone()))];
                            cargo = Some(pair);
                            (keys, vec![file])
                        }).unwrap_or_default()
                    } else {
                        let project = source.browse.requested_project().clone();
                        let key = PageKey::Browse(BrowseKey::Tree(project.clone()));
                        tree = Some(TreeDependency { requested_project: project, expected: None, package: PackageRef::parse(source.package.as_str()).ok() });
                        (vec![key.clone()], vec![key])
                    }
                }
                Route::Symbol(symbol) => {
                    let keys = route_symbol(route).map(|id| match symbol.view {
                        View::Code => vec![PageKey::Source(id.clone()), PageKey::Symbol(id)],
                        View::Page | View::Graph => vec![PageKey::Symbol(id)],
                    }).unwrap_or_default();
                    (keys.clone(), keys)
                }
            },
        };
        for key in Self::chrome_keys(route) {
            if !keys.contains(&key) { keys.push(key); }
        }
        Self { keys, content, cargo, tree }
    }

    /// The titlebar consumes the same route mapping as the owner renewal plan.
    pub(crate) fn chrome_keys(route: &Route) -> Vec<PageKey> {
        if matches!(route, Route::CargoSource(source) if source.browse.context().is_none()) { return Vec::new(); }
        route_symbol(route).map(PageKey::Symbol).into_iter()
            .chain(route_package(route).map(PageKey::Package)).collect()
    }

    pub(crate) fn keys(&self) -> &[PageKey] { &self.keys }
    pub(crate) fn into_keys(self) -> Vec<PageKey> { self.keys }
    pub(crate) fn cargo(&self) -> Option<&CargoSourceDependencies> { self.cargo.as_ref() }

    pub(crate) fn current_tree(&self, store: &DataStore) -> Option<CurrentTreeRead> {
        let selected = self.tree.as_ref()?;
        let key = BrowseKey::Tree(selected.requested_project.clone());
        let resource = store.pages().browse(&key);
        let admission = admit_resource(&resource, store.snapshot().key(), store.owner_serving());
        let BrowseValue::Tree(model) = admission.current_value()? else { return None };
        let context = model.request_binding.and_then(|binding| {
            binding.matches_effective_workspace_root(&model.root).then_some(())?;
            CargoBrowseContext::from_binding_address(selected.requested_project.clone(), binding)
        });
        if selected.expected.as_ref().is_some_and(|expected| context.as_ref() != Some(expected)) { return None; }
        let dependency = PageKey::Browse(key);
        Some(CurrentTreeRead { model: Arc::clone(model), context, stamp: store.stamp(&dependency), dependency })
    }

    pub(crate) fn current_cargo_package(&self, store: &DataStore, package: &PackageRef) -> Option<CurrentCargoPackage> {
        if let Some(cargo) = &self.cargo {
            if &cargo.file.package != package { return None; }
            let dependency = PageKey::CargoSource(cargo.file.clone());
            let resource = store.cargo_source(&cargo.file);
            if store.cargo_read_admission(&dependency, &resource) != CargoReadAdmission::Current { return None; }
            let page = resource.loaded_value()?;
            if &page.package != package || page.request_binding != cargo.file.context.request_binding() || page.file != cargo.file.file { return None; }
            return Some(CurrentCargoPackage { context: cargo.file.context.clone(), package: package.clone(), stamp: store.stamp(&dependency), dependency });
        }
        if self.tree.as_ref()?.package.as_ref().is_some_and(|selected| selected != package) { return None; }
        let tree = self.current_tree(store)?;
        if !tree.model.source_packages.contains(package) { return None; }
        Some(CurrentCargoPackage { context: tree.context?, package: package.clone(), dependency: tree.dependency, stamp: tree.stamp })
    }

    /// One bounded mounted action's exact read slot. Inventory controls use
    /// the independent path receipt rather than the file's bytes.
    pub(crate) fn native_stamp(&self, store: &DataStore, inventory: bool) -> Option<(PageKey, Stamp)> {
        let key = if inventory {
            self.keys.iter().find(|key| matches!(key, PageKey::Browse(BrowseKey::CargoSourceInventory(_))))
        } else {
            self.keys.iter().find(|key| !matches!(key, PageKey::Health))
        }?;
        Some((key.clone(), store.stamp(key)))
    }

    pub(crate) fn admits_native_stamp(&self, store: &DataStore, root: VersionedRoot, expected: &(PageKey, Stamp)) -> bool {
        let (key, stamp) = expected;
        self.keys.contains(key) && store.stamp(key) == *stamp && self.current_key(store, root, key)
    }

    fn current_key(&self, store: &DataStore, root: VersionedRoot, key: &PageKey) -> bool {
        self.key_phase(store, root, key) == ReadPhase::Ready
    }

    fn key_phase(&self, store: &DataStore, root: VersionedRoot, key: &PageKey) -> ReadPhase {
        match key {
            PageKey::Orbit => resource_phase(&store.orbit(), root, store.owner_serving()),
            PageKey::Package(package) => resource_phase(&store.package(package), root, store.owner_serving()),
            PageKey::Browse(browse @ BrowseKey::CargoSourceInventory(_)) => cargo_phase(store, root, key, &store.pages().browse(browse)),
            PageKey::Browse(BrowseKey::Tree(project)) if self.tree.as_ref().is_some_and(|tree| &tree.requested_project == project && tree.expected.is_some()) => {
                let selected = resource_phase(&store.pages().browse(&BrowseKey::Tree(project.clone())), root, store.owner_serving());
                if selected == ReadPhase::Ready && self.current_tree(store).is_none() {
                    // The reply completed, but describes a different binding.
                    // Let the body disclose refusal instead of waiting forever.
                    ReadPhase::Terminal
                } else { selected }
            }
            PageKey::Browse(browse) => resource_phase(&store.pages().browse(browse), root, store.owner_serving()),
            PageKey::CargoSource(file) => cargo_phase(store, root, key, &store.cargo_source(file)),
            PageKey::Source(symbol) => resource_phase(&store.source(symbol), root, store.owner_serving()),
            PageKey::Symbol(symbol) => resource_phase(&store.symbol(symbol), root, store.owner_serving()),
            PageKey::Search(query) => resource_phase(&store.search(query), root, store.owner_serving()),
            PageKey::Health => resource_phase(&store.health(), root, store.owner_serving()),
        }
    }

    pub(crate) fn content_phase(&self, store: &DataStore) -> ReadPhase {
        self.content.iter().map(|key| self.key_phase(store, store.snapshot().key(), key))
            .fold(ReadPhase::Ready, ReadPhase::join)
    }

    pub(crate) fn content_loaded(&self, store: &DataStore) -> bool {
        self.content_phase(store) == ReadPhase::Ready
    }
}

fn resource_phase<T>(resource: &Resource<T>, root: VersionedRoot, serving: bool) -> ReadPhase {
    admit_resource(resource, root, serving).phase()
}

fn cargo_phase<T>(store: &DataStore, root: VersionedRoot, key: &PageKey, resource: &Resource<T>) -> ReadPhase {
    match store.cargo_read_admission(key, resource) {
        CargoReadAdmission::Current if root.same_authority(store.snapshot().key()) => ReadPhase::Ready,
        CargoReadAdmission::Current | CargoReadAdmission::Checking => ReadPhase::Pending,
        CargoReadAdmission::Fault(_) | CargoReadAdmission::Unavailable(_) => ReadPhase::Terminal,
    }
}
