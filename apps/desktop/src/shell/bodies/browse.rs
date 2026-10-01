//! The browsing pages' bodies: the product's words, drawn by
//! `facet::browse`. Your tree is the Library page.

use super::state::{Shown, not_ready, shown};
use super::{Ctx, Leaf, Pages};
use crate::model::browse::{BrowseKey, BrowseValue, TreeDestination, TreeModel};
use crate::model::pages::{PageKey, PackageRef, SearchQuery, SymbolRef};
use crate::navigation::{BrowseRoute, CompareSet, Intent, OrbitRoute, Route, View};
use crate::shell::kit::{package_route, symbol_route, symbol_view_route};
use crate::shell::reader::Reader;
use facet::browse::{Alert, AlertTone, LibraryActions, LibraryModel, LibraryReleaseLink, LibraryRole, LibraryRow, Twice, library};
use gpui::{Context, SharedString};
use std::sync::Arc;
use std::rc::Rc;

pub(super) fn body(route: &BrowseRoute, store: &Pages, ctx: &mut Ctx<'_>, cx: &mut Context<Reader>) -> Vec<Leaf> {
    let key = BrowseKey::from(route);
    let resource = store.browse(&key);
    // Find owns a live editing engine, including while a newly admitted
    // query is being read. Replacing it with a generic loading page loses
    // IME composition, focus, the debounce and the package hand.
    if matches!(route, BrowseRoute::Find(_) | BrowseRoute::FindHome) {
        let value = match shown(&resource) { Shown::Ready(BrowseValue::Find(value)) => Some(value.as_ref()), _ => None };
        let model = match value {
            Some(value) => Arc::clone(&value.prepared),
            None => Arc::new(facet::browse::find::Model {
                query: match route { BrowseRoute::Find(query) => query.text.to_string().into(), _ => "".into() },
                candidates: vec![],
                loose: vec![],
                coverage: vec![],
                more_answers: false,
                loading: matches!(shown(&resource), Shown::Pending),
            }),
        };
        // Find's reveal state lives in its native component. Its painted text
        // probes are the authority for what is visible in this frame.
        let actions = find_actions(route, ctx, cx);
        let mut leaves = vec![Leaf::new(facet::browse::find::find("find", model, actions, &ctx.measure))];
        if matches!(shown(&resource), Shown::Fault(_) | Shown::Unavailable(_, _)) {
            leaves.extend(not_ready(&shown(&resource), &PageKey::Browse(key), "Find", ctx, cx));
        }
        return leaves;
    }
    match shown(&resource) {
        Shown::Ready(BrowseValue::Tree(tree)) => {
            let model = library_model(tree, ctx);
            vec![Leaf::new(library("library", Arc::new(model), LibraryActions { open_package: open_package_action(ctx) }, &ctx.measure))]
        }
        Shown::Ready(BrowseValue::Compare(compare)) => {
            let model = Arc::clone(&compare.prepared);
            let actions = compare_actions(route, ctx, cx);
            vec![Leaf::new(facet::browse::compare::compare("compare", model, actions, &ctx.measure))]
        }
        other => {
            let (name, _) = route.here();
            not_ready(&other, &PageKey::Browse(key), &name, ctx, cx)
        }
    }
}

fn open_symbol_action(ctx: &Ctx<'_>, code: bool) -> Rc<dyn Fn(SharedString, &mut gpui::Window, &mut gpui::App)> {
    let links = ctx.links.clone();
    Rc::new(move |key, _, cx| {
        if let Ok(symbol) = SymbolRef::new(&key) && let Some(package) = symbol.package() {
            let route = if code { symbol_view_route(package.as_str(), &symbol, View::Code, None) } else { symbol_route(package.as_str(), &symbol) };
            if let Some(route) = route { links.dispatch(Intent::Navigate(route), cx); }
        }
    })
}

fn open_package_action(ctx: &Ctx<'_>) -> Rc<dyn Fn(SharedString, &mut gpui::Window, &mut gpui::App)> {
    let links = ctx.links.clone();
    Rc::new(move |key, _, cx| {
        if let Ok(package) = PackageRef::parse(&key) && let Some(route) = package_route(&package) { links.dispatch(Intent::Navigate(route), cx); }
    })
}

fn find_actions(route: &BrowseRoute, ctx: &Ctx<'_>, cx: &mut Context<Reader>) -> facet::browse::find::Actions {
    let expected = match route { BrowseRoute::Find(query) => Some(query.clone()), _ => None };
    let refine_links = ctx.links.clone();
    let compare_links = ctx.links.clone();
    let reader = cx.weak_entity();
    let acquire = Some(crate::shell::acquire::add_actions(&ctx.links, cx.entity_id()));
    facet::browse::find::Actions {
        acquire,
        scroll: ctx.reader_scroll.clone(),
        initial_held: ctx.find_held.clone(),
        persist_held: Rc::new(move |held, cx| { let _ = reader.update(cx, |reader, cx| reader.set_find_held(held, cx)); }),
        refine: Rc::new(move |text, cx| {
            let query = SearchQuery::new(&text, SearchQuery::DEFAULT_LIMIT).ok();
            refine_links.dispatch(Intent::RefineFind { expected: expected.clone(), query }, cx);
        }),
        open_symbol: open_symbol_action(ctx, false), open_code: open_symbol_action(ctx, true), open_package: open_package_action(ctx),
        compare: Rc::new(move |keys, _, cx| {
            let packages = keys.iter().map(|key| PackageRef::parse(key)).collect::<Result<Vec<_>, _>>();
            if let Ok(packages) = packages && let Ok(selection) = CompareSet::new(packages) {
                compare_links.dispatch(Intent::Navigate(Route::Orbit(OrbitRoute::Browse(BrowseRoute::Compare(selection)))), cx);
            }
        }),
    }
}

fn compare_actions(route: &BrowseRoute, ctx: &Ctx<'_>, cx: &mut Context<Reader>) -> facet::browse::compare::Actions {
    let packages = match route { BrowseRoute::Compare(selection) => selection.packages().to_vec(), _ => vec![] };
    let links = ctx.links.clone();
    let reader = cx.weak_entity();
    let held = ctx.find_held.clone();
    facet::browse::compare::Actions { active: ctx.active && ctx.links.snapshot(cx).overlay().is_none(), scroll: ctx.reader_scroll.clone(), open_package: open_package_action(ctx), open_symbol: open_symbol_action(ctx, false), open_code: open_symbol_action(ctx, true),
        remove_package: Rc::new(move |key, _, cx| {
            if let Ok(selection) = CompareSet::new(packages.iter().filter(|package| package.as_str() != key.as_ref()).cloned()) {
                let updated = held.iter().filter(|package| package.key != key).cloned().collect();
                let _ = reader.update(cx, |reader, cx| reader.set_find_held(updated, cx));
                links.dispatch(Intent::Navigate(Route::Orbit(OrbitRoute::Browse(BrowseRoute::Compare(selection)))), cx);
            }
        })
    }
}

/// A path may wrap between hops, never inside one: `toml 1.1.5` stays whole.
fn unbroken_hops(path: &str) -> SharedString {
    path.split(" → ")
        .map(|hop| hop.replace(' ', "\u{a0}"))
        .collect::<Vec<_>>()
        .join(" → ")
        .into()
}

/// The facet model of one tree, every sentence recorded as said.
fn library_model(tree: &TreeModel, ctx: &mut Ctx<'_>) -> LibraryModel {
    let reading = &tree.reading;
    let mut say = |text: &str| -> SharedString { ctx.say(text.to_owned()) };
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
            label: say(role.label),
            serving: role.serving.as_deref().map(&mut say),
            rows: role
                .rows
                .iter()
                .enumerate()
                .map(|(row_at, row)| LibraryRow {
                    name: say(&row.name),
                    at_rest: row.at_rest.as_deref().map(&mut say),
                    why: row.evidence.clone().into(),
                    about: row.description.clone().map(SharedString::from),
                    releases: row.versions.iter().enumerate().map(|(version_at, version)| {
                        let destination = tree.links.get(role_at)
                            .filter(|links| links.role == role.id)
                            .and_then(|links| links.rows.get(row_at))
                            .filter(|links| links.name.as_ref() == row.name)
                            .and_then(|links| links.releases.get(version_at))
                            .filter(|link| link.version.as_ref() == version);
                        match destination.map(|link| &link.destination) {
                            Some(TreeDestination::Open(package)) => LibraryReleaseLink {
                                version: version.clone().into(),
                                target: Some(package.as_str().to_owned().into()),
                                unavailable: None,
                            },
                            Some(TreeDestination::Unavailable(reason)) => LibraryReleaseLink {
                                version: version.clone().into(),
                                target: None,
                                unavailable: Some(reason.to_string().into()),
                            },
                            None => LibraryReleaseLink {
                                version: version.clone().into(),
                                target: None,
                                unavailable: Some("The source for this release was not resolved.".into()),
                            },
                        }
                    }).collect(),
                })
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
        lede_tip: reading.elsewhere.clone().map(SharedString::from),
        note: reading.source_note.as_deref().map(&mut say),
        alerts,
        facts,
        roles,
        twice_heading: reading.twice_heading.as_ref().map(|(title, caption)| (say(title), say(caption))),
        twice,
    }
}

#[cfg(test)]
#[path = "../browse_tests.rs"]
mod tests;
