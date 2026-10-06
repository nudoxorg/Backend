//! Passive graph presentation. No producer authority, old NodeId, scene,
//! search state or deferred callback can cross the publication boundary.

use super::identity::IdentityAdapter;
use crate::{model::pages::{PackageRef, SymbolRef}, navigation::Route};
use facet::graph::{GraphView, Start, presentation::Geometry, scene::Scene};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PresentationVisit {
    pub sequence: u64,
    pub route: Route,
    pub preferred: Option<PackageRef>,
}

struct ExactSelection {
    package: PackageRef,
    symbol: SymbolRef,
}

pub(super) struct RetainedPresentation {
    visit: PresentationVisit,
    geometry: Geometry,
    selected: Option<ExactSelection>,
    had_selection: bool,
}

pub(super) struct Restoration {
    pub start: Start,
    pub status: Option<&'static str>,
}

impl RetainedPresentation {
    pub fn capture(graph: &GraphView, identities: &IdentityAdapter, visit: PresentationVisit) -> Option<Self> {
        let focus = graph.focused();
        Some(Self {
            visit,
            geometry: graph.presentation()?,
            selected: focus.and_then(|node| identities.exact_node(node)).map(|exact| ExactSelection {
                package: exact.package, symbol: exact.symbol,
            }),
            had_selection: focus.is_some(),
        })
    }

    pub fn restore(self, visit: &PresentationVisit, scene: &Scene, identities: &IdentityAdapter) -> Option<Restoration> {
        if self.visit != *visit { return None; }
        let focus = self.selected.and_then(|selected| {
            match identities.exact_candidates(&selected.symbol, &selected.package).as_slice() {
                [node] => Some(*node),
                _ => None,
            }
        });
        let camera = self.geometry.restore(scene, focus);
        let status = if self.had_selection && focus.is_none() && camera.is_none() {
            Some("The previous selection has no unique exact identity in the changed layout. Selection cleared; showing the whole world.")
        } else if self.had_selection && focus.is_none() {
            Some("The previous selection has no unique exact identity in this graph. Selection cleared.")
        } else if camera.is_none() {
            Some("The graph layout changed without an exact selected anchor. Showing the whole world.")
        } else { None };
        Some(Restoration { start: camera.map_or(Start::World, |camera| Start::Restore { camera, focus }), status })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::identity::ResolvedSymbol;
    use facet::{graph::{Kind, Layout, Module, Node, Package, World}, motion::Camera};
    use std::{collections::BTreeMap, sync::Arc};

    fn package() -> PackageRef { PackageRef::parse("/fixture/owner").expect("exact package") }
    fn visit() -> PresentationVisit { PresentationVisit { sequence: 7, route: Route::World, preferred: Some(package()) } }
    fn symbol(name: &str) -> SymbolRef { SymbolRef::new(name).expect("exact symbol") }
    fn fixture(names: &[&str], identities: &[&str]) -> (Scene, IdentityAdapter) {
        let world = Arc::new(World::new(
            vec![Package { name: "owner".into(), version: "1".into(), yours: true, external: false, deps: vec![] }],
            vec![Module { pkg: 0, path: "module".into(), file: "module.rs".into() }],
            names.iter().map(|name| Node::new(Kind::Function, *name, 0, 0)).collect(), vec![],
        ).expect("valid world"));
        let exact = identities.iter().enumerate().map(|(node, name)| (u32::try_from(node).expect("tiny node"),
            ResolvedSymbol { package: package(), symbol: symbol(name), line: None })).collect();
        let adapter = IdentityAdapter::indexed(&world, &BTreeMap::from([(package(), 0)]), exact);
        (Scene::new(world.clone(), Arc::new(Layout::compute(&world))), adapter)
    }
    fn selected(scene: &Scene) -> (RetainedPresentation, Camera) {
        let camera = Camera::new(f64::from(scene.layout.x[1]) + 8.0, f64::from(scene.layout.y[1]) - 4.0, 91.5);
        (RetainedPresentation { visit: visit(), geometry: Geometry::capture(scene, camera, Some(1)),
            selected: Some(ExactSelection { package: package(), symbol: symbol("exact-B") }), had_selection: true }, camera)
    }

    #[test]
    fn permuted_nodes_restore_b_by_exact_coordinate_and_keep_anchor_offset() {
        let (old, _) = fixture(&["A", "B"], &["exact-A", "exact-B"]);
        let (fresh, identities) = fixture(&["B", "A", "C"], &["exact-B", "exact-A", "exact-C"]);
        let (packet, old_camera) = selected(&old);
        let restored = packet.restore(&visit(), &fresh, &identities).expect("same visit");
        let Start::Restore { camera, focus } = restored.start else { panic!("remapped anchor retains placement") };
        assert_eq!(focus, Some(0), "old raw NodeId 1 now denotes A");
        assert_eq!(camera.w, old_camera.w);
        assert!((camera.x - f64::from(fresh.layout.x[0]) - 8.0).abs() < 1e-10);
        assert!((camera.y - f64::from(fresh.layout.y[0]) + 4.0).abs() < 1e-10);
        assert_eq!(restored.status, None);
    }

    #[test]
    fn missing_or_ambiguous_identity_clears_selection_even_when_node_ids_exist() {
        let (old, _) = fixture(&["A", "B"], &["exact-A", "exact-B"]);
        for names in [["exact-A", "replacement"], ["exact-B", "exact-B"]] {
            let (fresh, identities) = fixture(&["A", "B"], &names);
            let (packet, camera) = selected(&old);
            let restored = packet.restore(&visit(), &fresh, &identities).expect("same visit");
            assert_eq!(restored.start, Start::Restore { camera, focus: None });
            assert!(restored.status.expect("honest cleared status").contains("Selection cleared"));
        }
    }

    #[test]
    fn changed_world_without_a_unique_anchor_fits_with_an_explicit_status() {
        let (old, _) = fixture(&["A", "B"], &["exact-A", "exact-B"]);
        let (fresh, identities) = fixture(&["A", "C", "D"], &["exact-A", "exact-C", "exact-D"]);
        let (packet, _) = selected(&old);
        let restored = packet.restore(&visit(), &fresh, &identities).expect("same visit");
        assert_eq!(restored.start, Start::World);
        assert!(restored.status.expect("reset status").contains("showing the whole world"));
    }

    #[test]
    fn a_later_world_intent_route_or_preferred_package_cannot_restore_old_presentation() {
        let (scene, identities) = fixture(&["A", "B"], &["exact-A", "exact-B"]);
        let mut later_world = visit(); later_world.sequence += 1;
        let mut later_route = visit(); later_route.route = Route::Orbit(crate::navigation::OrbitRoute::Home);
        let mut later_package = visit(); later_package.preferred = None;
        for current in [later_world, later_route, later_package] {
            let (packet, _) = selected(&scene);
            assert!(packet.restore(&current, &scene, &identities).is_none());
        }
    }

    #[test]
    fn world_pan_survives_identical_basis_but_changed_world_resets_without_an_anchor() {
        let (old, _) = fixture(&["A", "B"], &["exact-A", "exact-B"]);
        let camera = Camera::new(31.25, -9.75, 300.5);
        for (names, expected) in [(&["A", "B"][..], Start::Restore { camera, focus: None }),
            (&["A", "C", "D"][..], Start::World)] {
            let (fresh, identities) = fixture(names, &["exact-A", "exact-C"]);
            let packet = RetainedPresentation { visit: visit(), geometry: Geometry::capture(&old, camera, None), selected: None, had_selection: false };
            let restored = packet.restore(&visit(), &fresh, &identities).expect("same visit");
            assert_eq!(restored.start, expected);
            assert_eq!(restored.status.is_some(), expected == Start::World);
        }
    }
}
