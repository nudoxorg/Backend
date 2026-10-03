//! Bounded persistent route history.
//!
//! A route transition only allocates one `Arc` node while the history is below
//! its cap.  Back/forward snapshots therefore share their unchanged tails;
//! the cap is explicit so a long session cannot grow the immutable snapshot
//! without bound.

use super::route::Route;
use super::presentation::ReadingVisit;
use std::sync::Arc;

/// Maximum number of content routes retained in either history direction.
pub const MAX_ROUTE_HISTORY: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq)]
struct Node {
    route: Route,
    reading: ReadingVisit,
    previous: Option<Arc<Node>>,
    length: usize,
}

/// An immutable, bounded stack of routes with structural sharing.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RouteHistory {
    head: Option<Arc<Node>>,
}

impl RouteHistory {
    /// Returns an empty history.
    #[must_use]
    pub const fn new() -> Self {
        Self { head: None }
    }

    /// Returns the number of retained routes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.head.as_ref().map_or(0, |node| node.length)
    }

    /// Returns whether this history contains no routes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.head.is_none()
    }

    /// Returns the newest route without consuming the history.
    #[must_use]
    pub fn last(&self) -> Option<&Route> {
        self.head.as_deref().map(|node| &node.route)
    }

    /// Borrows recent routes from newest to oldest without cloning the
    /// bounded history. Used to recover a source page's exact tree address.
    pub fn iter(&self) -> impl Iterator<Item = &Route> {
        std::iter::successors(self.head.as_deref(), |node| node.previous.as_deref())
            .map(|node| &node.route)
    }

    /// Adds a route at the newest end, dropping the oldest route at the cap.
    #[must_use]
    pub fn push(&self, route: Route) -> Self {
        let reading = ReadingVisit::cold(&route);
        self.push_visit(route, reading)
    }

    pub fn push_visit(&self, route: Route, reading: ReadingVisit) -> Self {
        if self.len() < MAX_ROUTE_HISTORY {
            return Self {
                head: Some(Arc::new(Node {
                    route,
                    reading,
                    previous: self.head.clone(),
                    length: self.len() + 1,
                })),
            };
        }

        // This path runs only at the explicit cap. Rebuilding at most 64
        // nodes keeps the invariant simple while all normal pushes share the
        // prior Arc tail.
        let mut visits = std::iter::successors(self.head.as_deref(), |node| node.previous.as_deref())
            .take(MAX_ROUTE_HISTORY - 1).map(|node| (node.route.clone(), node.reading.clone())).collect::<Vec<_>>();
        visits.insert(0, (route, reading));
        let mut history = Self::new();
        for (route, reading) in visits.into_iter().rev() { history = history.push_visit(route, reading); }
        history
    }

    /// Removes and returns the newest route and the remaining history.
    #[must_use]
    pub fn pop(&self) -> Option<(Route, Self)> {
        self.pop_visit().map(|(route, _, rest)| (route, rest))
    }

    pub fn pop_visit(&self) -> Option<(Route, ReadingVisit, Self)> {
        self.head.as_ref().map(|node| (node.route.clone(), node.reading.clone(), Self { head: node.previous.clone() }))
    }

    /// Returns routes from newest to oldest for diagnostics and persistence
    /// adapters. The reducer itself uses `push`/`pop` and does not clone this
    /// bounded slice.
    #[must_use]
    pub fn to_vec(&self) -> Vec<Route> {
        let mut routes = Vec::with_capacity(self.len());
        let mut node = self.head.as_deref();
        while let Some(current) = node {
            routes.push(current.route.clone());
            node = current.previous.as_deref();
        }
        routes
    }


}

impl From<Vec<Route>> for RouteHistory {
    /// Builds a history from oldest to newest, respecting the explicit cap.
    fn from(routes: Vec<Route>) -> Self {
        routes
            .into_iter()
            .fold(Self::new(), |history, route| history.push(route))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::PackageId;
    use crate::navigation::{PackageLane, PackageRoute};

    fn route(value: &str) -> Route {
        Route::Package(PackageRoute {
            cargo: None,
            project: None,
            package: PackageId::new(value).expect("package"),
            lane: PackageLane::Overview,
            selected: None,
            at: None,
        })
    }

    #[test]
    fn history_is_bounded_and_newest_first() {
        let mut history = RouteHistory::new();
        for index in 0..(MAX_ROUTE_HISTORY + 3) {
            history = history.push(route(&format!("pkg{index}")));
        }
        assert_eq!(history.len(), MAX_ROUTE_HISTORY);
        assert_eq!(history.last(), Some(&route("pkg66")));
        assert_eq!(history.to_vec().last(), Some(&route("pkg3")));
    }

    #[test]
    fn pop_shares_the_unchanged_tail() {
        let first = RouteHistory::new().push(route("one")).push(route("two"));
        let (latest, rest) = first.pop().expect("latest");
        assert_eq!(latest, route("two"));
        assert_eq!(rest.last(), Some(&route("one")));
    }
}
