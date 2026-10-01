//! Orbit, plain: one resume line, your projects at the centre, the packages
//! around them as names, and one quiet line about the index. The rings map
//! is wave 3; this is its list form.

use super::state::{Shown, shown};
use super::{Ctx, Leaf};
use crate::model::AppSnapshot;
use crate::model::pages::{IndexedPackage, PackageRef, PageKey, Readiness};
use crate::navigation::{BrowseRoute, Intent, OrbitRoute, Route};
use crate::shell::focus::{Act, Target};
use crate::shell::kit::{HoverIntent, package_route, pending, quiet, text};
use crate::shell::reader::Reader;
use facet::icons::{Kind, KindSize};
use facet::tokens::fluid::PROJECT_GEM;
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
    let mut leaves = crate::shell::onboard::library::notes(snapshot, ctx);
    if let Some(leaf) = resume(snapshot, ctx, cx) {
        leaves.push(leaf);
    }
    let workspace = snapshot.workspace();
    let orbit = store.orbit();
    let indexed = orbit.loaded_value().and_then(|model| model.indexed.known());
    if workspace.projects.is_empty() && indexed.is_some_and(|indexed| indexed.is_empty()) {
        // A Library the index could not answer is not an empty one: say why
        // it could not, with the way to try again, before offering a first run.
        let fault = shown(&orbit);
        if matches!(fault, Shown::Fault(_)) {
            leaves.extend(super::state::not_ready(&fault, &PageKey::Orbit, "The Library", ctx, cx));
        } else {
            leaves.push(crate::shell::onboard::library::empty(ctx));
        }
        return leaves;
    }
    if workspace.projects.is_empty() {
        // An unknown index is not evidence that the library is empty. Keep
        // the add path visible while naming the incomplete read.
        leaves.push(crate::shell::onboard::library::add_another(ctx));
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
        let tree_route = Route::Orbit(OrbitRoute::Browse(BrowseRoute::Tree(project.id.clone())));
        let tree_id: SharedString = format!("orbit-tree-{}", project.id.as_str()).into();
        let tree_links = ctx.links.clone();
        let tree_project = project.id.clone();
        let tree_recall = ctx.targets.recall();
        let tree_leave_id = tree_id.clone();
        let tree_act: Act = Rc::new(move |_, cx| {
            let leaving = tree_links.snapshot(cx).route().clone();
            tree_recall.focus(tree_leave_id.clone());
            tree_recall.remember_leave(leaving, tree_leave_id.clone());
            tree_links.dispatch(Intent::ActivateProject(tree_project.clone()), cx);
            tree_links.dispatch(Intent::Navigate(tree_route.clone()), cx);
        });
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
        if project.phase != crate::model::ProjectPhase::Missing {
            ctx.targets.push(Target {
                id: tree_id.clone(),
                label: format!("{} dependency tree", project.label).into(),
                act: Rc::clone(&tree_act),
                peek: None,
                source: None,
            });
        }
        let state = match project.phase {
            crate::model::ProjectPhase::Indexing => ctx.say("indexing"),
            crate::model::ProjectPhase::Failed => ctx.say("stopped"),
            crate::model::ProjectPhase::Cancelling => ctx.say("stopping"),
            crate::model::ProjectPhase::Cancelled => ctx.say("paused"),
            crate::model::ProjectPhase::Missing => ctx.say("folder missing"),
            _ => SharedString::default(),
        };
        let tile = ctx.targets.track(
                id.clone(),
                div()
                    .id(id)
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(measure.space(Space::Base))
                    .child(crate::shell::onboard::library::tile_gem(project, f32::from(PROJECT_GEM.at(ctx.wide.fluid_room())), active, ctx))
                    .child(text(ty::HEAD, &measure, if active { palette.ink0 } else { palette.ink1 }).child(name))
                    .children((!state.is_empty()).then(|| text(ty::SMALL, &measure, palette.ink3).child(state)))
                    .on_click(move |_: &ClickEvent, window, cx| act(window, cx)),
            );
        let mut project_block = div().flex().flex_col().items_center().gap(measure.space(Space::Snug)).child(tile);
        if project.phase != crate::model::ProjectPhase::Missing {
            let label = ctx.say("Dependency tree ›");
            project_block = project_block.child(ctx.targets.track(
                tree_id.clone(),
                div()
                    .id(tree_id)
                    .cursor_pointer()
                    .min_h(measure.row())
                    .flex()
                    .items_center()
                    .px(measure.space(Space::Base))
                    .hover(|style| style.bg(palette.tint))
                    .child(text(ty::SMALL, &measure, palette.ink2).child(label))
                    .on_click(move |_: &ClickEvent, window, cx| tree_act(window, cx)),
            ));
        }
        centre = centre.child(project_block);
    }
    leaves.push(Leaf::new(centre).wide());
    leaves.extend(crate::shell::onboard::library::indexing(snapshot, ctx, cx));
    leaves.extend(crate::shell::onboard::failure::stopped(&workspace.projects, ctx, cx));
    if !workspace.projects.is_empty() {
        leaves.push(crate::shell::onboard::library::add_another(ctx));
    }
    // The packages around them.
    let orbit = store.orbit();
    match shown(&orbit) {
        Shown::Ready(model) => {
            if let Some(indexed) = model.indexed.known() {
                let mut ring = div().flex().flex_wrap().justify_center().gap(measure.space(Space::Roomy));
                // The names wrap as the room changes; a name that moves to
                // another line glides there (FLIP) instead of jumping.
                // Drawn away from the scroller (leaving under a plate), the
                // Library is inert: its names stand where they lay out, and
                // neither fly with the plate nor leave the live ring's
                // springs mid-flight when the page goes (J9 saw chips fly
                // 700 px as the Library left for a page and came back).
                let flow = if ctx.active { ctx.ring_flow.clone() } else { facet::motion::Flow::new("orbit-inert") };
                flow.epoch((measure.density(), measure.scale().to_bits()));
                // Your projects stand in the middle; the ring is what they
                // use, in the library's own order (the same after a
                // relaunch), one name told apart from its twin. It re-wraps
                // as an install adds packages: a name that must go to
                // another line lands there (the ring's flow is `wrapped`).
                let around = crate::shell::side::beside_your_projects(indexed, &workspace, crate::shell::side::LibraryOrder::Library);
                let apart = crate::shell::side::told_apart(&around);
                let hidden = around.len().saturating_sub(64);
                for (package, apart) in around.into_iter().zip(apart).take(64) {
                    let key = gpui::ElementId::Name(format!("orbit-chip-{}", package.package).into());
                    ring = ring.child(flow.item(key, package_name(package, apart, ctx, cx)));
                }
                leaves.push(Leaf::new(ring).wide());
                if hidden > 0 {
                    let id: SharedString = "orbit-browse-all".into();
                    let label = ctx.say(format!("Browse all {} indexed packages ›", indexed.len()));
                    let links = ctx.links.clone();
                    let act: Act = Rc::new(move |_, cx| {
                        links.dispatch(Intent::Navigate(Route::Orbit(OrbitRoute::Browse(BrowseRoute::FindHome))), cx);
                    });
                    ctx.targets.push(Target { id: id.clone(), label: label.clone(), act: Rc::clone(&act), peek: None, source: None });
                    leaves.push(Leaf::new(div().flex().justify_center().child(ctx.targets.track(
                        id.clone(),
                        div().id(id).cursor_pointer().min_h(measure.row()).flex().items_center()
                            .px(measure.space(Space::Base)).hover(|style| style.bg(palette.tint))
                            .child(text(ty::SMALL, &measure, palette.ink2).child(label))
                            .on_click(move |_: &ClickEvent, window, cx| act(window, cx)),
                    ))));
                }
            } else if let Some(gap) = model.indexed.gap() {
                let words = ctx.say(format!("The packages around your projects are unavailable. {}", crate::shell::kit::gap_words(gap)));
                leaves.push(Leaf::new(div().flex().justify_center().child(quiet(words, &measure, palette))));
            }
        }
        Shown::Pending => leaves.push(Leaf::new(
            div().flex().justify_center().child(pending(px(360.0 * measure.scale()), ty::MONO_ROW, &measure, palette)),
        )),
        fault @ Shown::Fault(_) => {
            leaves.extend(super::state::not_ready(&fault, &PageKey::Orbit, "The packages around your projects", ctx, cx));
        }
        fault @ Shown::Unavailable(..) => {
            leaves.extend(super::state::not_ready(&fault, &PageKey::Orbit, "The packages around your projects", ctx, cx));
        }
    }
    // The counts describe the last revision the owner published. While a
    // project is being indexed they describe something older than what is
    // running, and "0 declarations from 0 of 0 files" reads as a result.
    let running = workspace.projects.iter().any(|project| project.phase == crate::model::ProjectPhase::Indexing);
    if let Some(health) = store.health().loaded_value().filter(|health| !running && health.rows > 0) {
        let (packages, yours) = store.orbit().loaded_value().and_then(|model| model.indexed.known().map(|indexed| {
            let yours = indexed.iter().filter(|package| workspace.projects.iter().any(|project| project.id.as_str() == package.package.as_str())).count();
            (indexed.len(), yours)
        })).unwrap_or((0, 0));
        let arrival = crate::shell::onboard::library::Arrival {
            ready_projects: workspace.projects.iter().filter(|project| project.phase == crate::model::ProjectPhase::Ready).count(),
            packages,
            yours,
            declarations: health.rows,
            files_indexed: health.ingest.files_indexed,
            files_discovered: health.ingest.files_discovered,
        };
        let mut lines = div().flex().flex_col().items_center().gap(measure.space(Space::Snug));
        for line in arrival.says() {
            let words = ctx.say(line);
            // `min_w(0)`: a text in a flex row is as wide as its one line unless it may shrink,
            // and then wraps to the column (200 % text on a phone cut it on both sides).
            lines = lines.child(quiet(words, &measure, palette).min_w(px(0.0)).text_center());
        }
        leaves.push(Leaf::new(div().flex().justify_center().child(lines)));
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
        let view = crate::runtime::hand::hand_view_for(&hand, snapshot, cx);
        let last = view.cards.iter().max_by_key(|card| card.held.touched_at)?;
        let route = crate::shell::root::held_route(&last.held)?;
        let ago = ago(crate::shell::root::now_ms().saturating_sub(last.held.touched_at));
        let mut words = vec![("Continue".to_owned(), false)];
        match view.roads.first().filter(|_| view.status.is_none()) {
            Some(road) => {
                let names: Vec<String> = road.cards.iter().map(|&n| view.cards[n].name.to_string()).collect();
                words.push((names.join(" → "), true));
                words.push((format!("· {} · you left at", road.sentence), false));
            }
            None => words.push(("· you left at".to_owned(), false)),
        }
        words.push((last.name.to_string(), true));
        words.push((ago, false));
        if let Some(status) = &view.status {
            words.push((format!("· {status}"), false));
        }
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

fn package_name(package: &IndexedPackage, apart: Option<SharedString>, ctx: &mut Ctx<'_>, cx: &mut Context<Reader>) -> AnyElement {
    let measure: Measure = ctx.measure;
    let palette: &Palette = ctx.palette;
    let name = ctx.say(package.name.to_string());
    // What tells it apart from a twin of the same name, said with it: its
    // release (a registry package), or the folder it is in.
    let apart = package.package.release_version().map(ToOwned::to_owned).or_else(|| apart.map(|apart| apart.to_string())).map(|apart| ctx.say(apart));
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
                .max_w_full()
                .hover(|style| style.bg(palette.tint))
                .child(crate::shell::kit::kind_mark(Kind::Package, KindSize::Sm, &measure, palette))
                .child(text(ty::MONO_ROW, &measure, ink).min_w(px(0.0)).overflow_hidden().whitespace_nowrap().text_ellipsis().child(name))
                .children(apart.map(|apart| text(ty::MONO_SMALL, &measure, palette.ink3).keyed(SharedString::from(format!("orbit-apart:{}", package.package))).flex_none().whitespace_nowrap().child(apart)))
                .on_click(move |_: &ClickEvent, window, cx| act(window, cx))
                .on_hover(cx.listener(move |reader, hovered: &bool, _, cx| reader.hover_link(warm.clone(), *hovered, cx))),
        )
        .into_any_element()
}
