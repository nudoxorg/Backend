//! Display-only fallback for an exact earlier destination.
//!
//! This module consumes the private snapshot's bounded projection only when
//! the pane has no independently readable, identity-matched Resource. It
//! cannot create a current read, semantic link, or owner action lease.

use super::{Ctx, Leaf};
use crate::model::browse::BrowseKey;
use crate::model::pages::{CargoSourceKey, PackageRef, PageKey};
use crate::model::retained_display::{CaptureCoverage, DisplayBody, RetainedDisplay};
use crate::navigation::{BrowseRoute, OrbitRoute, Overlay, Route};
use crate::runtime::store::DataStore;
use crate::shell::kit::{quiet, text as styled_text};
use crate::shell::reader::Reader;
use facet::{Space, tokens::ty};
use gpui::{Context, ParentElement, Styled, div};
use std::sync::Arc;

/// Any populated Resource owns this slot, even when its shape is refused.
/// Cache fallback never replaces an independently retained/current value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PaneValue { Absent, Exact, Wrong }

pub(super) fn select(store: &DataStore, route: &Route, overlay: Option<Overlay>) -> Option<Arc<RetainedDisplay>> {
    if overlay.is_some() { return None; }
    let display = store.retained_display(route)?;
    if !display.source_matches_route(route) || pane_value(store, route) != PaneValue::Absent { return None; }
    // The frozen cache's Cargo package matcher checks origin and package;
    // the pane additionally requires the captured README body kind.
    if !match route {
        Route::Package(_) => matches!(display.body(), DisplayBody::Markdown { .. }),
        Route::CargoSource(_) => matches!(display.body(), DisplayBody::Source { .. }),
        Route::World | Route::Orbit(OrbitRoute::Browse(_)) => matches!(display.body(), DisplayBody::Reading { .. }),
        Route::Symbol(_) | Route::Orbit(_) => false,
    } { return None; }
    Some(display)
}

/// Only a cursor-memory discriminator; it never supplies a source lease.
pub(super) fn source_generation(store: &DataStore, route: &Route, overlay: Option<Overlay>) -> Option<[u8; 32]> {
    let display = select(store, route, overlay)?;
    Some(display.memory_generation())
}

fn pane_value(store: &DataStore, route: &Route) -> PaneValue {
    match route {
        Route::Orbit(OrbitRoute::Browse(browse)) => {
            let resource = store.pages().browse(&BrowseKey::from(browse));
            let Some(value) = resource.loaded_value() else { return PaneValue::Absent; };
            // A populated slot, including a wrong-shaped payload, blocks
            // fallback. Its producer admission is independent; this cache
            // selector never rehashes/revalidates current Resource bytes.
            let _ = value;
            PaneValue::Exact
        }
        Route::CargoSource(source) => {
            let Some(context) = source.browse.context() else { return PaneValue::Wrong; };
            let Ok(package) = PackageRef::parse(source.package.as_str()) else { return PaneValue::Wrong; };
            let key = CargoSourceKey { context: context.clone(), package, target: source.target.clone() };
            let resource = store.cargo_source(&key);
            let Some(page) = resource.loaded_value() else { return PaneValue::Absent; };
            let _ = page;
            PaneValue::Exact
        }
        Route::Package(package) => {
            let Some(context) = &package.cargo else { return PaneValue::Wrong; };
            let Ok(package_id) = PackageRef::parse(package.package.as_str()) else { return PaneValue::Wrong; };
            let key = BrowseKey::CargoReadme(crate::model::browse::CargoReadmeKey {
                context: context.clone(), package: package_id.clone(),
            });
            let resource = store.pages().browse(&key);
            let Some(value) = resource.loaded_value() else { return PaneValue::Absent; };
            let _ = value;
            PaneValue::Exact
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
    let root = display.observation().root.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    let coverage = match display.coverage() {
        CaptureCoverage::Complete => "closed saved projection".to_owned(),
        CaptureCoverage::VisibleExcerpt { first_line, last_line } => format!("saved excerpt, lines {first_line}–{last_line}"),
    };
    let provenance = ctx.say(format!("Earlier display · observed root {root} · producer epoch {} · {coverage}. Read-only; current owner actions are unavailable.", display.observation().producer_epoch));
    let mut column = div().flex().flex_col().gap(measure.space(Space::Base))
        .child(quiet(provenance, &measure, palette));
    match display.body() {
        DisplayBody::Reading { title, rows } => {
            column = column.child(styled_text(ty::HERO, &measure, palette.ink1).child(ctx.say(title.to_string())));
            let (range, controls) = super::source::retained_row_page(Arc::clone(display), rows.len(), route, ctx, cx);
            for row in &rows[range] {
                if !row.label.is_empty() {
                    column = column.child(styled_text(ty::BODY, &measure, palette.ink1).child(ctx.say(row.label.to_string())));
                }
                if !row.detail.is_empty() {
                    column = column.child(styled_text(ty::BODY, &measure, palette.ink2).child(ctx.say(row.detail.to_string())));
                }
            }
            column = column.child(controls);
        }
        DisplayBody::Source { path, .. } => {
            column = column.child(styled_text(ty::MONO_ROW, &measure, palette.ink2).child(ctx.say(path.to_string())));
            column = column.child(super::source::retained_page(Arc::clone(display), route, ctx, cx));
        }
        DisplayBody::Markdown { .. } => {
            column = column.child(styled_text(ty::MONO_ROW, &measure, palette.ink2).child(ctx.say("Saved README · links unavailable")));
            column = column.child(super::source::retained_page(Arc::clone(display), route, ctx, cx));
        }
    }
    Leaf::new(column)
}
