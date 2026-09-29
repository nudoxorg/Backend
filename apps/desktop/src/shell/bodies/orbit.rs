//! Orbit, plain: one resume line, your projects at the centre, the packages
//! around them as names, and one quiet line about the index. The rings map
//! is wave 3; this is its list form.

use super::state::{Shown, shown};
use super::{Ctx, Leaf};
use crate::model::AppSnapshot;
use crate::model::pages::{IndexedPackage, PackageRef, PageKey, Readiness};
use crate::navigation::{Intent, Route};
use crate::shell::focus::{Act, Target};
use crate::shell::kit::{HoverIntent, package_route, pending, quiet, text};
use crate::shell::reader::Reader;
use facet::icons::{Kind, KindSize};
use facet::tokens::ty;
use facet::{Measure, Palette, Space};
use gpui::{
    AnyElement, ClickEvent, Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, px,
};
use std::rc::Rc;

pub(super) fn body(
    snapshot: &AppSnapshot,
    store: &super::Pages,
    ctx: &mut Ctx<'_>,
    _hover: &mut HoverIntent,
    cx: &mut Context<Reader>,
) -> Vec<Leaf> {
    let measure = ctx.measure;
    let palette = ctx.palette;
    let mut leaves = Vec::new();
    if let Some(leaf) = resume(snapshot, ctx, cx) {
        leaves.push(leaf);
    }
    let workspace = snapshot.workspace();
    let indexed = store
        .orbit()
        .loaded_value()
        .and_then(|model| model.indexed.known().map(|indexed| indexed.len()))
        .unwrap_or(0);
    if workspace.projects.is_empty() && indexed == 0 {
        let line = ctx.say("Nothing on the shelf yet. Add a folder and its dependencies arrive here.");
        let links = ctx.links.clone();
        leaves.push(Leaf::new(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(measure.space(Space::Gutter))
                .py(measure.space(Space::Chapter))
                .child(facet::paint::gem(Kind::Module).size(56.0 * measure.scale()).opacity(0.5))
                .child(text(ty::LEDE, &measure, palette.ink2).child(line))
                .child(
                    facet::controls::button("add-folder", "Add a folder", &measure)
                        .primary()
                        .on_click(move |_, cx| links.dispatch(Intent::OpenFolderPicker, cx)),
                ),
        ));
        return leaves;
    }
    // Your projects: the centre.
    let mut centre = div()
        .flex()
        .flex_wrap()
        .justify_center()
        .gap(measure.space(Space::Chapter))
        .py(measure.space(Space::Wide));
    for project in workspace.projects.iter() {
        let name = ctx.say(project.label.to_string());
        let active = workspace.active.as_ref() == Some(&project.id);
        let id: SharedString = format!("orbit-project-{}", project.path).into();
        let links = ctx.links.clone();
        let project_id = project.id.clone();
        // Your project reads as a package too (J1 "first look": Orbit →
        // your project → its own page), the same route a local checkout
        // gets everywhere else (`package_route`, reused as-is). Parsing can
        // fail for a path the engine would refuse; then the tile still
        // activates the project, it just has nowhere further to go.
        let project_route = PackageRef::parse(&project.path).ok().and_then(|package| package_route(&package));
        // A click focuses the tile it lands on (so Back, returning here,
        // restores it) and remembers this route was left by it, since
        // `Reader::arrive` unfocuses every new page it draws.
        // Not `ctx.targets.clone()`: this action is stored in that very list
        // (`push` below), and an action that holds its own list is a cycle.
        let recall = ctx.targets.recall();
        let leave_id = id.clone();
        let act: Act = Rc::new(move |_, cx| {
            let leaving = links.snapshot(cx).route().clone();
            recall.focus(leave_id.clone());
            recall.remember_leave(leaving, leave_id.clone());
            links.dispatch(Intent::ActivateProject(project_id.clone()), cx);
            if let Some(route) = project_route.clone() {
                links.dispatch(Intent::Navigate(route), cx);
            }
        });
        ctx.targets.push(Target {
            id: id.clone(),
            label: name.clone(),
            act: Rc::clone(&act),
            peek: None,
            source: None,
        });
        let state = match project.phase {
            crate::model::ProjectPhase::Indexing => ctx.say("indexing"),
            crate::model::ProjectPhase::Failed => ctx.say("stopped"),
            crate::model::ProjectPhase::Missing => ctx.say("folder missing"),
            _ => SharedString::default(),
        };
        centre = centre.child(
            ctx.targets.track(
                id.clone(),
                div()
                    .id(id)
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(measure.space(Space::Base))
                    .child(facet::paint::gem(Kind::Module).size(44.0 * measure.scale()).opacity(if active { 1.0 } else { 0.8 }))
                    .child(text(ty::HEAD, &measure, if active { palette.ink0 } else { palette.ink1 }).child(name))
                    .children((!state.is_empty()).then(|| text(ty::SMALL, &measure, palette.ink3).child(state)))
                    .on_click(move |_: &ClickEvent, window, cx| act(window, cx)),
            ),
        );
    }
    leaves.push(Leaf::new(centre));
    // The packages around them.
    let orbit = store.orbit();
    match shown(&orbit) {
        Shown::Ready(model) => {
            if let Some(indexed) = model.indexed.known() {
                let mut ring = div().flex().flex_wrap().justify_center().gap(measure.space(Space::Roomy));
                // The names wrap as the room changes; a name that moves to
                // another line glides there (FLIP) instead of jumping.
                let flow = facet::motion::Flow::scoped("orbit-ring", cx);
                flow.epoch((measure.density(), measure.scale().to_bits()));
                for package in indexed.iter() {
                    let key = gpui::ElementId::Name(format!("orbit-chip-{}", package.package).into());
                    ring = ring.child(flow.item(key, package_name(package, ctx, cx)));
                }
                leaves.push(Leaf::new(ring));
            }
        }
        Shown::Pending => leaves.push(Leaf::new(
            div().flex().justify_center().child(pending(px(360.0 * measure.scale()), ty::MONO_ROW, &measure, palette)),
        )),
        Shown::Fault(error) => {
            let words = ctx.say(format!("The packages around your projects could not be read: {}", error.message()));
            leaves.push(Leaf::new(quiet(words, &measure, palette)));
        }
        Shown::Unavailable(..) => {}
    }
    if let Some(health) = store.health().loaded_value() {
        let words = ctx.say(format!(
            "{} declarations from {} of {} files",
            health.rows, health.ingest.files_indexed, health.ingest.files_discovered
        ));
        leaves.push(Leaf::new(div().flex().justify_center().child(quiet(words, &measure, palette))));
    }
    leaves
}

/// "Continue …": the hand one rung up — its road, the road's sentence, and
/// where you left (the card touched last, and how long ago). With an empty
/// hand, the most recent page behind you ("Continue at RelationLabel").
fn resume(snapshot: &AppSnapshot, ctx: &mut Ctx<'_>, cx: &mut Context<Reader>) -> Option<Leaf> {
    let measure = ctx.measure;
    let palette = ctx.palette;
    let hand = snapshot.session().hand.clone();
    let (words, route) = if hand.is_empty() {
        let route = snapshot.session().back.to_vec().into_iter().find(|route| matches!(route, Route::Symbol(_)))?;
        let Route::Symbol(symbol) = &route else {
            return None;
        };
        let name = backend_present::Identity::parse(symbol.id.as_str()).name().to_owned();
        (vec![("Continue at".to_owned(), false), (name, true)], route)
    } else {
        let view = crate::runtime::fixture_world::hand_view(&hand, cx);
        let last = view.cards.iter().max_by_key(|card| card.held.touched_at)?;
        let route = crate::shell::root::held_route(&last.held)?;
        let ago = ago(crate::shell::root::now_ms().saturating_sub(last.held.touched_at));
        let mut words = vec![("Continue".to_owned(), false)];
        match view.roads.first() {
            Some(road) => {
                let names: Vec<String> = road.cards.iter().map(|&n| view.cards[n].name.to_string()).collect();
                words.push((names.join(" → "), true));
                words.push((format!("· {} · you left at", road.sentence), false));
            }
            None => words.push(("· you left at".to_owned(), false)),
        }
        words.push((last.name.to_string(), true));
        words.push((ago, false));
        (words, route)
    };
    let label = ctx.say(words.iter().map(|(w, _)| w.as_str()).collect::<Vec<_>>().join(" "));
    let links = ctx.links.clone();
    let act: Act = Rc::new(move |_, cx| links.dispatch(Intent::Navigate(route.clone()), cx));
    ctx.targets.push(Target { id: "resume".into(), label, act: Rc::clone(&act), peek: None, source: None });
    let mut line = div().id("resume").flex().flex_wrap().items_baseline().gap(measure.space(Space::Snug)).cursor_pointer();
    for (word, name) in words {
        line = line.child(if name {
            text(ty::MONO_ROW, &measure, palette.ink0).child(word)
        } else {
            text(ty::SMALL, &measure, palette.ink3).child(word)
        });
    }
    Some(Leaf::new(
        div().flex().justify_end().child(ctx.targets.track("resume", line.on_click(move |_: &ClickEvent, window, cx| act(window, cx)))),
    ))
}

/// "just now", "4 min ago", "2 h ago", "3 d ago".
fn ago(ms: u64) -> String {
    let minutes = ms / 60_000;
    match minutes {
        0 => "just now".to_owned(),
        1..=59 => format!("{minutes} min ago"),
        60..=1439 => format!("{} h ago", minutes / 60),
        _ => format!("{} d ago", minutes / 1440),
    }
}

fn package_name(package: &IndexedPackage, ctx: &mut Ctx<'_>, cx: &mut Context<Reader>) -> AnyElement {
    let measure: Measure = ctx.measure;
    let palette: &Palette = ctx.palette;
    let name = ctx.say(package.name.to_string());
    let id: SharedString = format!("orbit-package-{}", package.package).into();
    let route = package_route(&package.package);
    let links = ctx.links.clone();
    let act: Act = Rc::new(move |_, cx| {
        if let Some(route) = route.clone() {
            links.dispatch(Intent::Navigate(route), cx);
        }
    });
    ctx.targets.push(Target {
        id: id.clone(),
        label: name.clone(),
        act: Rc::clone(&act),
        peek: Some(PageKey::Package(package.package.clone())),
        source: None,
    });
    let warm = PageKey::Package(package.package.clone());
    let ink = match package.readiness {
        Readiness::Ready => palette.ink1,
        Readiness::Indexing => palette.ink3,
        Readiness::Failed => palette.coral.base,
    };
    ctx.targets
        .track(
            id.clone(),
            div()
                .id(id)
                .flex()
                .items_center()
                .gap(measure.space(Space::Snug))
                .px(measure.space(Space::Base))
                .h(measure.row())
                .hover(|style| style.bg(palette.tint))
                .child(crate::shell::kit::kind_mark(Kind::Package, KindSize::Sm, &measure, palette))
                .child(text(ty::MONO_ROW, &measure, ink).child(name))
                .on_click(move |_: &ClickEvent, window, cx| act(window, cx))
                .on_hover(cx.listener(move |reader, hovered: &bool, _, cx| reader.hover_link(warm.clone(), *hovered, cx))),
        )
        .into_any_element()
}
