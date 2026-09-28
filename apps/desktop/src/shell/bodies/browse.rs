//! The browsing pages' bodies: the product's words, drawn by
//! `facet::browse`. Your tree is the Library page.

use super::state::{Shown, not_ready, shown};
use super::{Ctx, Leaf, Pages};
use crate::model::browse::{BrowseKey, BrowseValue, TreeModel};
use crate::model::pages::PageKey;
use crate::navigation::BrowseRoute;
use crate::shell::reader::Reader;
use facet::browse::{Alert, AlertTone, LibraryModel, LibraryRole, LibraryRow, Twice, library};
use gpui::{Context, SharedString};
use std::sync::Arc;

pub(super) fn body(route: &BrowseRoute, store: &Pages, ctx: &mut Ctx<'_>, cx: &mut Context<Reader>) -> Vec<Leaf> {
    let key = BrowseKey::from(route);
    let resource = store.browse(&key);
    match shown(&resource) {
        Shown::Ready(BrowseValue::Tree(tree)) => {
            let model = library_model(tree, ctx);
            vec![Leaf::new(library("library", Arc::new(model), &ctx.measure))]
        }
        other => {
            let (name, _) = route.here();
            not_ready(&other, &PageKey::Browse(key), &format!("{name}'s tree"), ctx, cx)
        }
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
        .map(|role| LibraryRole {
            label: say(role.label),
            serving: role.serving.as_deref().map(&mut say),
            rows: role
                .rows
                .iter()
                .map(|row| LibraryRow {
                    name: say(&row.name),
                    at_rest: row.at_rest.as_deref().map(&mut say),
                    why: row.evidence.clone().into(),
                    about: row.description.clone().map(SharedString::from),
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
