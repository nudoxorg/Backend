//! The exact resources read by one visible route, shared by the store and Reader.

use super::{CargoReadAdmission, DataStore, route_package, route_symbol};
use crate::model::browse::{BrowseKey, CargoSourceInventoryKey};
use crate::model::pages::{CargoSourceKey, PackageRef, PageKey};
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

    /// Keeps the existing page readiness rules alongside the resource plan.
    /// The independently arriving Cargo inventory does not hold up file bytes.
    pub(crate) fn content_loaded(&self, store: &DataStore) -> bool {
        !self.waits_for_content
            || self.keys.iter().all(|key| match key {
                PageKey::Symbol(symbol) => store.symbol(symbol).is_loaded(),
                PageKey::Package(package) => store.package(package).is_loaded(),
                PageKey::Orbit => store.orbit().is_loaded(),
                PageKey::Source(symbol) => store.source(symbol).is_loaded(),
                PageKey::CargoSource(file) => {
                    store.cargo_read_admission(key, &store.cargo_source(file))
                        == CargoReadAdmission::Current
                }
                PageKey::Health | PageKey::Browse(_) | PageKey::Search(_) => true,
            })
    }
}
