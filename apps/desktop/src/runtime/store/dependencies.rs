//! The exact resources read by one visible route, shared by the store and Reader.

use super::{CargoReadAdmission, DataStore, route_package, route_symbol};
use crate::core::{VersionedRoot, admit_resource};
use crate::model::browse::{BrowseKey, CargoSourceInventoryKey};
use crate::model::pages::{CargoSourceKey, PackageRef, PageKey, Stamp};
use crate::navigation::{Overlay, Route, View};

/// A Cargo file and its independently arriving current path inventory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CargoSourceDependencies {
    pub(crate) file: CargoSourceKey,
    pub(crate) inventory: CargoSourceInventoryKey,
}

/// One construction supplies fetching, residency, watching and body gathering.
/// Lines and presentation positions do not change resource identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RouteDependencies {
    keys: Vec<PageKey>,
    cargo: Option<CargoSourceDependencies>,
    /// World's Orbit read is resident data, not the graph's page content.
    waits_for_content: bool,
}

impl RouteDependencies {
    pub(crate) fn new(route: &Route, overlay: Option<Overlay>) -> Self {
        let mut cargo = None;
        let keys = match overlay {
            Some(Overlay::Settings(_)) => vec![PageKey::Health],
            Some(Overlay::Inbox) => Vec::new(),
            _ => match route {
                Route::Orbit(crate::navigation::OrbitRoute::Browse(browse)) => {
                    vec![PageKey::Browse(browse.into())]
                }
                Route::Orbit(_) => vec![PageKey::Orbit, PageKey::Health],
                Route::World => vec![PageKey::Orbit],
                Route::Package(_) => route_package(route)
                    .map(PageKey::Package)
                    .into_iter()
                    .collect(),
                Route::CargoSource(route) => PackageRef::parse(route.package.as_str())
                    .ok()
                    .map(|package| {
                        let pair = CargoSourceDependencies {
                            file: CargoSourceKey {
                                project: route.project.clone(),
                                package: package.clone(),
                                file: route.file.clone(),
                            },
                            inventory: CargoSourceInventoryKey {
                                project: route.project.clone(),
                                package,
                            },
                        };
                        let keys = vec![
                            PageKey::CargoSource(pair.file.clone()),
                            PageKey::Browse(BrowseKey::CargoSourceInventory(
                                pair.inventory.clone(),
                            )),
                        ];
                        cargo = Some(pair);
                        keys
                    })
                    .unwrap_or_default(),
                Route::Symbol(symbol) => route_symbol(route)
                    .map(|id| match symbol.view {
                        View::Code => vec![PageKey::Source(id.clone()), PageKey::Symbol(id)],
                        View::Page | View::Graph => vec![PageKey::Symbol(id)],
                    })
                    .unwrap_or_default(),
            },
        };
        Self {
            keys,
            cargo,
            waits_for_content: !matches!(route, Route::World),
        }
    }

    pub(crate) fn keys(&self) -> &[PageKey] {
        &self.keys
    }

    pub(crate) fn into_keys(self) -> Vec<PageKey> {
        self.keys
    }

    pub(crate) fn cargo(&self) -> Option<&CargoSourceDependencies> {
        self.cargo.as_ref()
    }

    /// One bounded, mounted action's exact read slot. Snapshot-only controls
    /// capture no slot; a Cargo inventory action uses its independent path
    /// receipt rather than the file's bytes.
    pub(crate) fn native_stamp(&self, store: &DataStore, inventory: bool) -> Option<(PageKey, Stamp)> {
        let key = if inventory {
            self.keys.iter().find(|key| matches!(key, PageKey::Browse(BrowseKey::CargoSourceInventory(_))))
        } else {
            self.keys.iter().find(|key| !matches!(key, PageKey::Health))
        }?;
        Some((key.clone(), store.stamp(key)))
    }

    /// The same dependency map used for fetching decides whether a retained
    /// native action still reads a current, completed value. The owner lease
    /// and exact visit are checked by Reader before this slot test.
    pub(crate) fn admits_native_stamp(
        &self,
        store: &DataStore,
        root: VersionedRoot,
        expected: &(PageKey, Stamp),
    ) -> bool {
        let (key, stamp) = expected;
        if !self.keys.contains(key) || store.stamp(key) != *stamp { return false; }
        let serving = store.owner_serving();
        match key {
            PageKey::Orbit => admit_resource(&store.orbit(), root, serving).allows_actions(),
            PageKey::Package(package) => admit_resource(&store.package(package), root, serving).allows_actions(),
            PageKey::Browse(browse @ BrowseKey::CargoSourceInventory(_)) => {
                store.cargo_read_admission(key, &store.pages().browse(browse)) == CargoReadAdmission::Current
            }
            PageKey::Browse(browse) => admit_resource(&store.pages().browse(browse), root, serving).allows_actions(),
            PageKey::CargoSource(file) => store.cargo_read_admission(key, &store.cargo_source(file)) == CargoReadAdmission::Current,
            PageKey::Source(symbol) => admit_resource(&store.source(symbol), root, serving).allows_actions(),
            PageKey::Symbol(symbol) => admit_resource(&store.symbol(symbol), root, serving).allows_actions(),
            PageKey::Search(query) => admit_resource(&store.search(query), root, serving).allows_actions(),
            PageKey::Health => admit_resource(&store.health(), root, serving).allows_actions(),
        }
    }

    /// Keeps current page readiness alongside the resource plan.
    /// The independently arriving Cargo inventory does not hold up file bytes.
    pub(crate) fn content_loaded(&self, store: &DataStore) -> bool {
        !self.waits_for_content
            || self.keys.iter().all(|key| match key {
                PageKey::Symbol(symbol) => admit_resource(
                    &store.symbol(symbol),
                    store.snapshot().key(),
                    store.owner_serving(),
                )
                .allows_actions(),
                PageKey::Package(package) => admit_resource(
                    &store.package(package),
                    store.snapshot().key(),
                    store.owner_serving(),
                )
                .allows_actions(),
                PageKey::Orbit => admit_resource(
                    &store.orbit(),
                    store.snapshot().key(),
                    store.owner_serving(),
                )
                .allows_actions(),
                PageKey::Source(symbol) => admit_resource(
                    &store.source(symbol),
                    store.snapshot().key(),
                    store.owner_serving(),
                )
                .allows_actions(),
                PageKey::CargoSource(file) => {
                    store.cargo_read_admission(key, &store.cargo_source(file))
                        == CargoReadAdmission::Current
                }
                PageKey::Health | PageKey::Browse(_) | PageKey::Search(_) => true,
            })
    }
}
