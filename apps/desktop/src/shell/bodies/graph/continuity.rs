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
