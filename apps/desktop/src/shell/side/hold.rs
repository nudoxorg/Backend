//! Hold and trail (`v6/cohesion/COHESION.md`, "The sidebar: primitives",
//! 7 and 8).
//!
//! **Hold.** What you keep at hand sits above the lenses at every scope as
//! chips with ⌘1–⌘5 (Arc's favourites). The shell already has the place for
//! it: the hand (`model::hand`, at most five cards, ⌘D holds, ⌘1–⌘5 go), so
//! the chips are the hand's cards in the order the hand shows them, and
//! there are no other keys.
//!
//! **Trail.** Where you have been, at the foot of the sidebar: the history
//! made visible and clickable, newest first, without the place you are on.

use crate::model::hand::Held;
use crate::model::pages::{PackageRef, SymbolRef};
use crate::navigation::{BrowseRoute, OrbitRoute, Route};
use facet::icons::Kind;
use gpui::SharedString;

/// How many places the trail shows.
const TRAIL_SHOWN: usize = 4;

/// One held card as a chip.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Chip {
    /// What is held.
    pub held: Held,
    /// Its name.
    pub name: SharedString,
    /// Its mark.
    pub kind: Kind,
    /// Which card it is in the order the hand shows them (0 is ⌘1).
    pub card: usize,
}

/// The hand's cards as chips, in the order they are shown.
pub(super) fn chips(cards: impl IntoIterator<Item = (Held, SharedString, Kind)>) -> Vec<Chip> {
    cards.into_iter().enumerate().map(|(card, (held, name, kind))| Chip { held, name, kind, card }).collect()
}

/// The key cap on a chip: ⌘ and its number.
pub(super) fn cap(card: usize) -> String {
    format!("⌘{}", card + 1)
}

/// One place on the trail.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Step {
    /// Where it was, by name.
    pub label: SharedString,
    /// Where choosing it goes.
    pub route: Route,
}

/// The places behind you, newest first, one row each: a place is not
/// listed twice in a row, and never the place you are on.
pub(super) fn trail(back: &[Route], here: &Route) -> Vec<Step> {
    let here = label(here);
    let mut steps: Vec<Step> = Vec::new();
    for route in back {
        let words = label(route);
        if words == here || steps.last().is_some_and(|step| step.label == words) {
            continue;
        }
        steps.push(Step { label: words, route: route.clone() });
        if steps.len() == TRAIL_SHOWN {
            break;
        }
    }
    steps
}

/// A place's name, as the trail says it.
pub(super) fn label(route: &Route) -> SharedString {
    match route {
        Route::Symbol(route) => SymbolRef::new(route.id.as_str())
            .map_or_else(|_| route.id.as_str().to_owned(), |symbol| symbol.identity().name().to_owned())
            .into(),
        Route::Package(route) => PackageRef::parse(route.package.as_str())
            .map_or_else(|_| route.package.as_str().to_owned(), |package| package.display_name().to_owned())
            .into(),
        Route::Orbit(OrbitRoute::Browse(BrowseRoute::Find(query))) => format!("Find {}", query.text).into(),
        Route::Orbit(_) => "Library".into(),
        Route::World => "Graph".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::PackageId;
    use crate::navigation::{Coordinate, PackageLane, PackageRoute, SymbolRoute, View};

    fn package(name: &str) -> Route {
        Route::Package(PackageRoute {
            project: None,
            package: PackageId::new(&format!("pkg:cargo/{name}@1.0.0")).expect("package"),
            lane: PackageLane::Overview,
            selected: None,
            at: None,
        })
    }

    fn symbol(package: &str, name: &str) -> Route {
        Route::Symbol(SymbolRoute {
            project: None,
            package: PackageId::new(&format!("pkg:cargo/{package}@1.0.0")).expect("package"),
            id: Coordinate::new(&format!("pkg:cargo/{package}@1.0.0::a.rs:1::{name}")).expect("coordinate"),
            at: None,
            view: View::Page,
            line: None,
            selected: None,
        })
    }

    #[test]
    fn a_place_is_named_by_what_it_is() {
        assert_eq!(label(&package("toml")).as_ref(), "toml");
        assert_eq!(label(&symbol("toml", "Value")).as_ref(), "Value");
        assert_eq!(label(&Route::Orbit(OrbitRoute::Home)).as_ref(), "Library");
        assert_eq!(label(&Route::World).as_ref(), "Graph");
    }

    #[test]
    fn the_trail_is_the_places_behind_you_newest_first_without_repeats_and_never_where_you_are() {
        let here = symbol("toml", "Value");
        let back = [symbol("toml", "Value"), package("toml"), package("toml"), symbol("serde_json", "from_str"), Route::World, package("a"), package("b")];
        let steps: Vec<_> = trail(&back, &here).iter().map(|step| step.label.to_string()).collect();
        assert_eq!(steps, ["toml", "from_str", "Graph", "a"], "four places: the same place twice in a row is one, and Value is here");
        assert!(trail(&[], &here).is_empty());
    }

    #[test]
    fn a_chip_carries_the_key_of_its_place_in_the_hand() {
        assert_eq!(cap(0), "⌘1");
        assert_eq!(cap(4), "⌘5");
    }
}
