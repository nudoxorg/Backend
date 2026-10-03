//! Display-only fallback for an exact earlier destination.
//!
//! This module consumes the private snapshot's bounded projection only when
//! the pane has no independently readable, identity-matched Resource. It
//! cannot create a current read, semantic link, or owner action lease.

use super::{Ctx, Leaf};
use crate::model::browse::{BrowseKey, BrowseValue, CargoReadmeState};
use crate::model::pages::{CargoSourceKey, Known, PackageRef, PageKey};
use crate::model::retained_display::{CaptureCoverage, DisplayBody, DisplaySource, RetainedDisplay};
use crate::navigation::{BrowseRoute, OrbitRoute, Overlay, Route};
use crate::runtime::store::DataStore;
use crate::shell::kit::{quiet, text as styled_text};
use crate::shell::reader::Reader;
use facet::{Space, tokens::ty};
use gpui::{Context, ParentElement, Styled, div};
use std::sync::Arc;

/// A loaded but wrong-shaped value is a refusal, not a reason to paint an
/// unrelated historical projection over the discrepancy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PaneValue { Absent, Exact, Wrong }

pub(super) fn select(store: &DataStore, route: &Route, overlay: Option<Overlay>) -> Option<Arc<RetainedDisplay>> {
    if overlay.is_some() { return None; }
    let display = store.retained_display(route)?;
    if !display.source_matches_route(route) || pane_value(store, route) != PaneValue::Absent { return None; }
    // The frozen cache's Cargo package matcher checks origin and package;
    // the pane additionally requires the captured README body kind.
    if !match route {
        Route::Package(_) => matches!(&display.body, DisplayBody::Markdown { .. }),
        Route::CargoSource(_) => matches!(&display.body, DisplayBody::Source { .. }),
        Route::World | Route::Orbit(OrbitRoute::Browse(_)) => matches!(&display.body, DisplayBody::Reading { .. }),
        Route::Symbol(_) | Route::Orbit(_) => false,
    } { return None; }
    Some(display)
}

/// Only a cursor-memory discriminator; it never supplies a source lease.
pub(super) fn source_generation(store: &DataStore, route: &Route, overlay: Option<Overlay>) -> Option<[u8; 32]> {
    let display = select(store, route, overlay)?;
    match (&display.source, &display.body) {
        (DisplaySource::Cargo { content_digest, .. }, DisplayBody::Source { .. } | DisplayBody::Markdown { .. }) => Some(*content_digest),
        _ => None,
    }
}

fn pane_value(store: &DataStore, route: &Route) -> PaneValue {
    match route {
        Route::Orbit(OrbitRoute::Browse(browse)) => {
            let resource = store.pages().browse(&BrowseKey::from(browse));
            let Some(value) = resource.loaded_value() else { return PaneValue::Absent; };
            let matches = match (browse, value) {
                (BrowseRoute::Tree(project), BrowseValue::Tree(tree)) => tree.request_binding.is_some_and(|binding|
                    binding.has_admissible_shape() && binding.matches_requested_root(&project.path())
                    && binding.matches_effective_workspace_root(&tree.root)),
                (BrowseRoute::FindHome, BrowseValue::Find(find)) => find.prepared.query.is_empty(),
                (BrowseRoute::Find(query), BrowseValue::Find(find)) => find.prepared.query.as_ref() == query.text.as_ref()
                    && match &find.answers { Known::Known(answers) => answers.query == query.text, Known::Unknown(_) => true },
                (BrowseRoute::Compare(selection), BrowseValue::Compare(compare)) => compare.packages.iter()
                    .map(|package| &package.package).eq(selection.packages().iter()),
                _ => false,
            };
            if matches { PaneValue::Exact } else { PaneValue::Wrong }
        }
        Route::CargoSource(source) => {
            let Some(context) = source.browse.context() else { return PaneValue::Wrong; };
            let Ok(package) = PackageRef::parse(source.package.as_str()) else { return PaneValue::Wrong; };
            let key = CargoSourceKey { context: context.clone(), package, target: source.target.clone() };
            let resource = store.cargo_source(&key);
            let Some(page) = resource.loaded_value() else { return PaneValue::Absent; };
            if page.package == key.package && page.target == key.target
                && page.request_binding == context.request_binding()
                && *blake3::hash(page.source.text().as_bytes()).as_bytes() == page.content_digest
            { PaneValue::Exact } else { PaneValue::Wrong }
        }
        Route::Package(package) => {
            let Some(context) = &package.cargo else { return PaneValue::Wrong; };
            let Ok(package_id) = PackageRef::parse(package.package.as_str()) else { return PaneValue::Wrong; };
            let key = BrowseKey::CargoReadme(crate::model::browse::CargoReadmeKey {
                context: context.clone(), package: package_id.clone(),
            });
            let resource = store.pages().browse(&key);
            let Some(value) = resource.loaded_value() else { return PaneValue::Absent; };
            match value {
                BrowseValue::CargoReadme(readme) if readme.package == package_id
                    && readme.request_binding == context.request_binding()
                    && matches!(&readme.state, CargoReadmeState::Read(document)
                        if &document.origin.package == package_id.reference()
                            && document.origin.request_binding == context.request_binding()
                            && *blake3::hash(document.source.as_bytes()).as_bytes() == document.origin.content_digest) => PaneValue::Exact,
                _ => PaneValue::Wrong,
            }
        }
        Route::World => if store.orbit().loaded_value().is_some() { PaneValue::Exact } else { PaneValue::Absent },
        Route::Symbol(_) | Route::Orbit(_) => PaneValue::Wrong,
    }
}

pub(super) fn append(
    route: &Route,
    snapshot: &crate::model::AppSnapshot,
    ctx: &mut Ctx<'_>,
    cx: &mut Context<Reader>,
    leaves: &mut Vec<Leaf>,
) {
    let Some(display) = select(ctx.links.store.read(cx), route, snapshot.overlay()) else { return; };
    // The Resource and the cached projection are separately scoped. This
    // branch never changes Pages, content admission, or any callback lease.
    if matches!(route, Route::World) {
        let orbit = ctx.links.store.read(cx).orbit();
        leaves.extend(super::state::not_ready(
            &super::state::shown(&orbit), &PageKey::Orbit, "World", ctx, cx,
        ));
    }
    leaves.push(leaf(&display, route, ctx, cx));
}

fn leaf(display: &Arc<RetainedDisplay>, route: &Route, ctx: &mut Ctx<'_>, cx: &mut Context<Reader>) -> Leaf {
    let measure = ctx.measure;
    let palette = ctx.palette;
    let root = display.observation.root.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    let coverage = match &display.coverage {
        CaptureCoverage::Complete => "closed saved projection".to_owned(),
        CaptureCoverage::VisibleExcerpt { first_line, last_line } => format!("saved excerpt, lines {first_line}–{last_line}"),
    };
    let provenance = ctx.say(format!("Earlier display · observed root {root} · producer epoch {} · {coverage}. Read-only; current owner actions are unavailable.", display.observation.producer_epoch));
    let mut column = div().flex().flex_col().gap(measure.space(Space::Base))
        .child(quiet(provenance, &measure, palette));
    match &display.body {
        DisplayBody::Reading { title, rows } => {
            column = column.child(styled_text(ty::HERO, &measure, palette.ink1).child(ctx.say(title.to_string())));
            for row in rows {
                if !row.label.is_empty() {
                    column = column.child(styled_text(ty::BODY, &measure, palette.ink1).child(ctx.say(row.label.to_string())));
                }
                if !row.detail.is_empty() {
                    column = column.child(styled_text(ty::BODY, &measure, palette.ink2).child(ctx.say(row.detail.to_string())));
                }
            }
        }
        DisplayBody::Source { path, text, first_line } => {
            column = column.child(styled_text(ty::MONO_ROW, &measure, palette.ink2).child(ctx.say(path.to_string())));
            column = column.child(super::source::retained_page(Arc::clone(display), Arc::clone(text), *first_line, route, ctx, cx));
        }
        DisplayBody::Markdown { source } => {
            column = column.child(styled_text(ty::MONO_ROW, &measure, palette.ink2).child(ctx.say("Saved README · links unavailable")));
            column = column.child(super::source::retained_page(Arc::clone(display), Arc::clone(source), 1, route, ctx, cx));
        }
    }
    Leaf::new(column)
}
