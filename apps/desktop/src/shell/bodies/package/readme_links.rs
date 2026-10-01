//! Bounded actions for the exact README and outline currently on the page.
//! The Markdown component owns parsing and hit testing. This plan only turns
//! worker-indexed destinations into actions, once per loaded page identity.

use super::{decode_fragment, hex};
use crate::model::local_package::{ReadmeHeading, ReadmeLink, readme_fragment_slug, rustdoc_link};
use crate::model::pages::{OutlineTree, PackageRef, ReadmeExactKind, ReadmeExactTargets};
use crate::navigation::{Route, View};
use gpui::{Global, SharedString};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

const UNINDEXED: &str = "This README link was not included in the available link index.";
const UNAVAILABLE: &str = "This link has no verified destination in this package.";
const MAX_INDEXED_LINKS: usize = 512;
const MAX_INDEXED_HEADINGS: usize = 512;

#[derive(Clone, Debug)]
pub(super) enum Outcome {
    External(Arc<str>),
    Anchor(ReadmeHeading),
    File { path: Arc<str>, line: u32 },
    Route(Route),
    Unavailable(&'static str),
}

impl Outcome {
    pub(super) fn note(&self) -> Option<&'static str> {
        match self {
            Self::Unavailable(message) => Some(message),
            _ => None,
        }
    }
}

type HeadingHit = Option<ReadmeHeading>;

#[derive(Clone, Debug)]
enum Candidate {
    External(Arc<str>),
    Anchor(String),
    File { path: Arc<str>, line: u32 },
    Rustdoc,
    Route(Route),
    Unavailable(&'static str),
}

/// One page revision's indexed actions. Holding the Arcs in the key prevents
/// pointer reuse from ever making an old plan look like a new publication.
pub(super) struct Plan {
    source: Arc<str>,
    links: Option<Arc<[ReadmeLink]>>,
    headings: Option<Arc<[ReadmeHeading]>>,
    outline: Option<OutlineTree>,
    exact: Option<ReadmeExactTargets>,
    package: PackageRef,
    outcomes: Vec<Outcome>,
    by_destination: HashMap<Arc<str>, Outcome>,
    by_heading: HashMap<Arc<str>, HeadingHit>,
    shown_links: Cell<usize>,
    shown_headings: Cell<usize>,
    restored_focus: RefCell<Option<SharedString>>,
}

impl Plan {
    pub(super) fn matches(
        &self,
        source: &Arc<str>,
        links: Option<&Arc<[ReadmeLink]>>,
        headings: Option<&Arc<[ReadmeHeading]>>,
        outline: Option<&OutlineTree>,
        exact: Option<&ReadmeExactTargets>,
        package: &PackageRef,
    ) -> bool {
        Arc::ptr_eq(&self.source, source)
            && option_arc_eq(self.links.as_ref(), links)
            && option_arc_eq(self.headings.as_ref(), headings)
            && match (&self.outline, outline) {
                (Some(a), Some(b)) => a.complete == b.complete && Arc::ptr_eq(&a.roots, &b.roots),
                (None, None) => true,
                _ => false,
            }
            && match (&self.exact, exact) {
                (Some(a), Some(b)) => Arc::ptr_eq(&a.links, &b.links),
                (None, None) => true,
                _ => false,
            }
            && &self.package == package
    }

    pub(super) fn build(
        source: Arc<str>,
        links: Option<Arc<[ReadmeLink]>>,
        headings: Option<Arc<[ReadmeHeading]>>,
        outline: Option<OutlineTree>,
        exact: Option<ReadmeExactTargets>,
        package: PackageRef,
    ) -> Self {
        let mut by_heading: HashMap<Arc<str>, HeadingHit> = HashMap::new();
        for heading in headings
            .iter()
            .flat_map(|headings| headings.iter().take(MAX_INDEXED_HEADINGS))
        {
            by_heading
                .entry(Arc::clone(&heading.slug))
                .and_modify(|hit| *hit = None)
                .or_insert_with(|| Some(heading.clone()));
        }
        let candidates: Vec<_> = links
            .iter()
            .flat_map(|links| links.iter().take(MAX_INDEXED_LINKS))
            .map(|link| candidate(link.destination.as_ref(), Some(link), &package))
            .collect();
        let mut exact_by_index: HashMap<usize, Option<ReadmeExactKind>> = HashMap::new();
        if let Some(proof) = exact.as_ref().filter(|proof| {
            proof.package == package
                && Arc::ptr_eq(&proof.source, &source)
                && outline.as_ref().is_some_and(|outline| {
                    outline.complete && Arc::ptr_eq(&proof.outline_roots, &outline.roots)
                })
        }) {
            for entry in proof.links.iter().take(MAX_INDEXED_LINKS) {
                let index = usize::from(entry.link_index);
                let Some(link) = links.as_ref().and_then(|links| links.get(index)) else {
                    continue;
                };
                if link.destination != entry.destination
                    || !match &entry.kind {
                        ReadmeExactKind::Source { path, line, .. } => {
                            *line > 0
                                && link.local_file.as_ref() == Some(path)
                                && link.line.unwrap_or(1) == *line
                        }
                        ReadmeExactKind::Rustdoc(_) => link.local_file.is_none(),
                    }
                {
                    continue;
                }
                exact_by_index
                    .entry(index)
                    .and_modify(|hit| *hit = None)
                    .or_insert_with(|| Some(entry.kind.clone()));
            }
        }
        let outcomes: Vec<_> = candidates
            .into_iter()
            .enumerate()
            .map(|(index, candidate)| {
                resolve(
                    candidate,
                    &by_heading,
                    exact_by_index.get(&index).and_then(Option::as_ref),
                    &package,
                )
            })
            .collect();
        let by_destination = links
            .iter()
            .flat_map(|links| links.iter().take(MAX_INDEXED_LINKS))
            .zip(outcomes.iter())
            .map(|(link, outcome)| (Arc::clone(&link.destination), outcome.clone()))
            .collect();
        Self {
            source,
            links,
            headings,
            outline,
            exact,
            package,
            outcomes,
            by_destination,
            by_heading,
            shown_links: Cell::new(32),
            shown_headings: Cell::new(32),
            restored_focus: RefCell::new(None),
        }
    }

    pub(super) fn row(&self, index: usize) -> Outcome {
        self.outcomes
            .get(index)
            .cloned()
            .unwrap_or(Outcome::Unavailable(UNINDEXED))
    }

    pub(super) fn destination(&self, destination: &str) -> Outcome {
        self.by_destination
            .get(destination)
            .cloned()
            .unwrap_or(Outcome::Unavailable(UNINDEXED))
    }

    pub(super) fn heading(&self, slug: &str) -> Outcome {
        self.by_heading
            .get(slug)
            .and_then(Option::as_ref)
            .cloned()
            .map_or(
                Outcome::Unavailable("This heading is not in the available README index."),
                Outcome::Anchor,
            )
    }

    pub(super) fn first_row_id(&self, destination: &str) -> Option<SharedString> {
        let index = self
            .links
            .as_ref()?
            .iter()
            .take(MAX_INDEXED_LINKS)
            .position(|link| link.destination.as_ref() == destination)?;
        self.shown_links
            .set(self.shown_links.get().max(index.saturating_add(1)));
        Some(format!("readme-link-{index}").into())
    }

    pub(super) fn shown_links(&self) -> usize {
        self.shown_links.get()
    }

    pub(super) fn total_links(&self) -> usize {
        self.outcomes.len()
    }

    pub(super) fn show_more_links(&self) {
        self.shown_links.set(
            self.shown_links
                .get()
                .saturating_add(32)
                .min(self.outcomes.len()),
        );
    }

    pub(super) fn shown_headings(&self) -> usize {
        self.shown_headings.get()
    }

    pub(super) fn total_headings(&self) -> usize {
        self.headings
            .as_ref()
            .map_or(0, |headings| headings.len().min(MAX_INDEXED_HEADINGS))
    }

    pub(super) fn show_more_headings(&self) {
        let total = self.total_headings();
        self.shown_headings
            .set(self.shown_headings.get().saturating_add(32).min(total));
    }

    pub(super) fn mark_leaving_for(&self, outcome: &Outcome) {
        if matches!(outcome, Outcome::Route(_)) {
            *self.restored_focus.borrow_mut() = None;
        }
    }

    pub(super) fn restore_focus_once(&self, id: &SharedString) -> bool {
        let mut last = self.restored_focus.borrow_mut();
        if last.as_ref() == Some(id) {
            return false;
        }
        *last = Some(id.clone());
        true
    }
}

fn option_arc_eq<T>(left: Option<&Arc<[T]>>, right: Option<&Arc<[T]>>) -> bool {
    match (left, right) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    }
}

/// One small per-app slot: a second package replaces the first; no unbounded
/// destination or outline cache accumulates while browsing.
#[derive(Default)]
pub(super) struct Cache(Option<Rc<Plan>>);

impl Global for Cache {}

impl Cache {
    pub(super) fn get_or_build(
        &mut self,
        source: Arc<str>,
        links: Option<Arc<[ReadmeLink]>>,
        headings: Option<Arc<[ReadmeHeading]>>,
        outline: Option<OutlineTree>,
        exact: Option<ReadmeExactTargets>,
        package: PackageRef,
    ) -> Rc<Plan> {
        if let Some(plan) = &self.0
            && plan.matches(
                &source,
                links.as_ref(),
                headings.as_ref(),
                outline.as_ref(),
                exact.as_ref(),
                &package,
            )
        {
            return Rc::clone(plan);
        }
        let plan = Rc::new(Plan::build(
            source, links, headings, outline, exact, package,
        ));
        self.0 = Some(Rc::clone(&plan));
        plan
    }
}

fn candidate(destination: &str, indexed: Option<&ReadmeLink>, package: &PackageRef) -> Candidate {
    if destination.is_empty()
        || destination.len() > crate::model::local_package::MAX_README_LINK_DESTINATION_BYTES
        || destination.chars().any(char::is_control)
    {
        return Candidate::Unavailable("This link has an invalid destination.");
    }
    let lower = destination.to_ascii_lowercase();
    if lower.starts_with("https://") || lower.starts_with("http://") || lower.starts_with("mailto:")
    {
        return if valid_external(destination, &lower) {
            Candidate::External(Arc::from(destination))
        } else {
            Candidate::Unavailable("This external link has an invalid address.")
        };
    }
    if let Some(fragment) = destination.strip_prefix('#') {
        return decode_fragment(fragment)
            .filter(|fragment| !fragment.chars().any(char::is_control))
            .map(|fragment| Candidate::Anchor(readme_fragment_slug(&fragment)))
            .unwrap_or(Candidate::Unavailable(
                "This heading link has an invalid fragment.",
            ));
    }
    if destination.starts_with("pkg:") {
        return PackageRef::parse(destination)
            .ok()
            .and_then(|package| crate::shell::kit::package_route(&package))
            .map_or(
                Candidate::Unavailable("This package address is not valid."),
                Candidate::Route,
            );
    }
    if destination.chars().any(char::is_whitespace)
        || destination
            .split(['/', '#', '?'])
            .next()
            .is_some_and(|head| head.contains(':'))
    {
        return Candidate::Unavailable("This link uses an unsupported or invalid address.");
    }
    if let Some(path) = indexed.and_then(|link| link.local_file.as_deref()) {
        if destination.contains('?') {
            return Candidate::Unavailable("This file link's query cannot be opened locally.");
        }
        if let Some((_, fragment)) = destination.split_once('#') {
            let Some(line) = indexed.and_then(|link| link.line) else {
                return Candidate::Unavailable("This file fragment does not name a verified line.");
            };
            if !valid_line_fragment(fragment, line) {
                return Candidate::Unavailable("This file fragment does not name a verified line.");
            }
        }
        let line = indexed.and_then(|link| link.line).unwrap_or(1);
        if line == 0 {
            return Candidate::Unavailable("This file link has no valid one-based line.");
        }
        if std::path::Path::new(path)
            .strip_prefix(package.as_str())
            .is_err()
        {
            return Candidate::Unavailable(UNAVAILABLE);
        }
        return Candidate::File {
            path: Arc::from(path),
            line,
        };
    }
    if destination.contains(['#', '?']) {
        return Candidate::Unavailable(
            "This Rustdoc fragment or query is not an exact indexed declaration.",
        );
    }
    if rustdoc_link(destination).is_none() {
        return Candidate::Unavailable("This Rustdoc page is not an exact declaration link.");
    }
    Candidate::Rustdoc
}

fn valid_external(destination: &str, lower: &str) -> bool {
    if destination.chars().any(char::is_whitespace) || destination.contains('\\') {
        return false;
    }
    let bytes = destination.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let (Some(high), Some(low)) = (bytes.get(index + 1), bytes.get(index + 2)) else {
                return false;
            };
            let (Some(high), Some(low)) = (hex(*high), hex(*low)) else {
                return false;
            };
            let decoded = (high << 4) | low;
            if decoded.is_ascii_control() {
                return false;
            }
            index += 3;
        } else {
            index += 1;
        }
    }
    if lower.starts_with("mailto:") {
        return destination[7..]
            .split(['?', '#'])
            .next()
            .is_some_and(|recipient| recipient.contains('@'));
    }
    let start = if lower.starts_with("https://") { 8 } else { 7 };
    destination[start..]
        .split(['/', '?', '#'])
        .next()
        .is_some_and(|host| !host.is_empty() && !host.contains('@'))
}

fn valid_line_fragment(fragment: &str, line: u32) -> bool {
    let Some(rest) = fragment.strip_prefix('L') else {
        return false;
    };
    let (first, end) = rest
        .split_once('-')
        .map_or((rest, None), |(first, end)| (first, Some(end)));
    first.parse::<u32>().ok() == Some(line)
        && end.is_none_or(|end| {
            end.strip_prefix('L')
                .and_then(|end| end.parse::<u32>().ok())
                .is_some_and(|last| last >= line)
        })
}

fn resolve(
    candidate: Candidate,
    headings: &HashMap<Arc<str>, HeadingHit>,
    exact: Option<&ReadmeExactKind>,
    package: &PackageRef,
) -> Outcome {
    match candidate {
        Candidate::External(url) => Outcome::External(url),
        Candidate::Anchor(slug) => headings
            .get(slug.as_str())
            .and_then(Option::as_ref)
            .cloned()
            .map_or(
                Outcome::Unavailable("This heading is not in the available README index."),
                Outcome::Anchor,
            ),
        Candidate::File { path, line } => match exact {
            Some(ReadmeExactKind::Source { symbol, .. }) => crate::shell::kit::symbol_view_route(
                package.as_str(),
                symbol,
                View::Code,
                Some(line),
            )
            .map_or(Outcome::File { path, line }, Outcome::Route),
            _ => Outcome::File { path, line },
        },
        Candidate::Rustdoc => match exact {
            Some(ReadmeExactKind::Rustdoc(symbol)) => {
                crate::shell::kit::symbol_route(package.as_str(), symbol).map_or(
                    Outcome::Unavailable("This Rustdoc declaration cannot be routed."),
                    Outcome::Route,
                )
            }
            _ => Outcome::Unavailable(
                "This Rustdoc declaration is not uniquely proved by the current complete outline.",
            ),
        },
        Candidate::Route(route) => Outcome::Route(route),
        Candidate::Unavailable(reason) => Outcome::Unavailable(reason),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(destination: &str, local_file: Option<&str>, line: Option<u32>) -> ReadmeLink {
        ReadmeLink {
            label: Arc::from(destination),
            destination: Arc::from(destination),
            local_file: local_file.map(Arc::from),
            line,
        }
    }

    fn plan(links: Vec<ReadmeLink>, headings: Vec<ReadmeHeading>) -> Plan {
        let package = crate::shell::tests::dossier().package;
        Plan::build(
            Arc::from("# indexed"),
            Some(links.into()),
            Some(headings.into()),
            None,
            None,
            package,
        )
    }

    #[test]
    fn utf8_duplicate_fragment_resolves_only_the_indexed_heading() {
        let heading = ReadmeHeading {
            slug: Arc::from("café-1"),
            element_id: Arc::from("readme-heading-42"),
            title: Arc::from("Café"),
            level: 2,
        };
        let plan = plan(
            vec![
                link("#caf%C3%A9-1", None, None),
                link("#caf%C3%A9-2", None, None),
            ],
            vec![heading],
        );
        assert!(matches!(plan.row(0), Outcome::Anchor(_)));
        assert!(matches!(plan.row(1), Outcome::Unavailable(_)));
        assert!(matches!(
            plan.destination("#caf%C3%A9-1"),
            Outcome::Anchor(_)
        ));
        assert!(matches!(
            plan.destination("#caf%C3%A9"),
            Outcome::Unavailable(_)
        ));
    }

    #[test]
    fn unsafe_uri_and_unverified_fragments_have_explicit_outcomes() {
        let package = crate::shell::tests::dossier().package;
        let path = format!("{}/src/lib.rs", package.as_str());
        let plan = plan(
            vec![
                link("https://example.test/%0A", None, None),
                link("javascript:alert(1)", None, None),
                link("http://", None, None),
                link("https://example.test/%ZZ", None, None),
                link("src/lib.rs#L12garbage", Some(&path), None),
                link("src/lib.rs?mode=raw", Some(&path), None),
                link("src/lib.rs#L12-L14", Some(&path), Some(12)),
                link("https://example.test/path", None, None),
                link("#a%00b", None, None),
                link("src/lib.rs", Some(&path), Some(0)),
            ],
            vec![],
        );
        for index in [0, 1, 2, 3, 4, 5, 8, 9] {
            assert!(
                matches!(plan.row(index), Outcome::Unavailable(_)),
                "row {index}"
            );
        }
        assert!(matches!(plan.row(6), Outcome::File { line: 12, .. }));
        assert!(matches!(plan.row(7), Outcome::External(_)));
    }

    #[test]
    fn duplicate_heading_slug_cannot_select_one_arbitrarily() {
        let heading = ReadmeHeading {
            slug: Arc::from("same"),
            element_id: Arc::from("readme-heading-4"),
            title: Arc::from("Same"),
            level: 2,
        };
        let mut duplicate = heading.clone();
        duplicate.element_id = Arc::from("readme-heading-40");
        let plan = plan(vec![link("#same", None, None)], vec![heading, duplicate]);
        assert!(matches!(plan.row(0), Outcome::Unavailable(_)));
        assert!(matches!(plan.heading("same"), Outcome::Unavailable(_)));
    }

    #[test]
    fn exact_rustdoc_route_requires_matching_worker_proof() {
        let dossier = crate::shell::tests::dossier();
        let package = dossier.package;
        let outline = dossier.outline.known().expect("fixture outline").clone();
        let symbol = outline
            .walk()
            .find(|node| node.decl.name.as_ref() == "Outline")
            .expect("exact fixture declaration")
            .decl
            .coordinate
            .clone();
        let source: Arc<str> = Arc::from("# index");
        let links: Arc<[ReadmeLink]> = Arc::from([link("outline/struct.Outline.html", None, None)]);
        let exact = ReadmeExactTargets {
            package: package.clone(),
            source: Arc::clone(&source),
            outline_roots: Arc::clone(&outline.roots),
            links: Arc::from([crate::model::pages::ReadmeExactTarget {
                link_index: 0,
                destination: Arc::from("outline/struct.Outline.html"),
                kind: ReadmeExactKind::Rustdoc(symbol),
            }]),
        };
        let complete = Plan::build(
            Arc::clone(&source),
            Some(Arc::clone(&links)),
            Some(Arc::from([])),
            Some(outline.clone()),
            Some(exact.clone()),
            package.clone(),
        );
        assert!(matches!(complete.row(0), Outcome::Route(Route::Symbol(_))));
        let stale = Plan::build(
            Arc::from("# index"),
            Some(Arc::clone(&links)),
            Some(Arc::from([])),
            Some(outline.clone()),
            Some(exact.clone()),
            package.clone(),
        );
        assert!(matches!(stale.row(0), Outcome::Unavailable(_)));
        let mut partial = outline;
        partial.complete = false;
        let partial = Plan::build(
            source,
            Some(links),
            Some(Arc::from([])),
            Some(partial),
            Some(exact),
            package,
        );
        assert!(matches!(partial.row(0), Outcome::Unavailable(_)));
    }

    #[test]
    fn absent_navigation_index_cannot_become_an_empty_complete_index() {
        let package = crate::shell::tests::dossier().package;
        let plan = Plan::build(
            Arc::from("[link](https://example.test)"),
            None,
            None,
            None,
            None,
            package,
        );
        assert!(
            matches!(plan.destination("https://example.test"), Outcome::Unavailable(message) if message == UNINDEXED)
        );
        assert_eq!(plan.total_links(), 0);
        assert_eq!(plan.total_headings(), 0);
    }

    #[test]
    fn plan_identity_and_incremental_rows_remain_bounded() {
        let package = crate::shell::tests::dossier().package;
        let links: Arc<[ReadmeLink]> = (0..100)
            .map(|index| link(&format!("https://example.test/{index}"), None, None))
            .collect::<Vec<_>>()
            .into();
        let headings: Arc<[ReadmeHeading]> = Arc::from([]);
        let source: Arc<str> = Arc::from("# index");
        let plan = Plan::build(
            Arc::clone(&source),
            Some(Arc::clone(&links)),
            Some(Arc::clone(&headings)),
            None,
            None,
            package.clone(),
        );
        assert!(plan.matches(&source, Some(&links), Some(&headings), None, None, &package));
        assert_eq!(plan.shown_links(), 32);
        plan.show_more_links();
        assert_eq!(plan.shown_links(), 64);
        assert_eq!(
            plan.first_row_id("https://example.test/99"),
            Some("readme-link-99".into())
        );
        assert_eq!(plan.shown_links(), 100);
        assert!(!plan.matches(
            &Arc::from("# index"),
            Some(&links),
            Some(&headings),
            None,
            None,
            &package
        ));
        let id: SharedString = "readme-link-99".into();
        assert!(plan.restore_focus_once(&id));
        assert!(!plan.restore_focus_once(&id));
        plan.mark_leaving_for(&plan.row(99));
        // External links do not enter route history and leave focus alone.
        assert!(!plan.restore_focus_once(&id));
        let routed =
            Outcome::Route(crate::shell::kit::package_route(&package).expect("package route"));
        plan.mark_leaving_for(&routed);
        assert!(plan.restore_focus_once(&id));
    }
}
