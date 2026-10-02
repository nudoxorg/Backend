//! Current owner README paint and native actions, independent of a dossier.

use super::{Ctx, Leaf, activate_readme_link, readme_links};
use crate::model::browse::{BrowseKey, BrowseValue, CargoReadmeDestination, CargoReadmeDocument, CargoReadmeState};
use crate::model::pages::PageKey;
use crate::navigation::{CargoSourceRoute, Intent, Route};
use crate::runtime::store::{CargoReadAdmission, RouteDependencies};
use crate::shell::bodies::state::{Shown, not_ready};
use crate::shell::focus::Target;
use crate::shell::kit::{quiet, text};
use crate::shell::markdown::FollowMarkdownLink;
use crate::shell::reader::Reader;
use facet::{Space, tokens::ty};
use gpui::{App, AppContext as _, Context, ElementId, Global, InteractiveElement, ParentElement, SharedString, Styled, Window, div, px};
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

#[derive(Default)]
struct Cache(Option<(backend_library::CargoPackageReadmeOriginV1, Rc<ShownRows>)>);
impl Global for Cache {}
struct ShownRows { links: Cell<usize>, headings: Cell<usize> }
impl Cache {
    fn for_document(&mut self, document: &CargoReadmeDocument) -> Rc<ShownRows> {
        if let Some((origin, rows)) = &self.0 && origin == &document.origin { return Rc::clone(rows); }
        let rows = Rc::new(ShownRows { links: Cell::new(32), headings: Cell::new(32) });
        // Neutral disclosure state only: this cache never holds read admission.
        self.0 = Some((document.origin.clone(), Rc::clone(&rows)));
        rows
    }
}

pub(super) fn body(place: &Route, ctx: &mut Ctx<'_>, cx: &mut Context<Reader>) -> Vec<Leaf> {
    let dependencies = RouteDependencies::new(place, None);
    let Some(selected) = dependencies.readme() else { return Vec::new(); };
    let key = PageKey::Browse(BrowseKey::CargoReadme(selected.clone()));
    let live = ctx.links.store.read(cx);
    let resource = live.pages().browse(&BrowseKey::CargoReadme(selected.clone()));
    let admission = live.cargo_read_admission(&key, &resource);
    let current = dependencies.current_cargo_readme(live);
    let Some(current) = current else {
        let mut leaves = Vec::new();
        if let Some(BrowseValue::CargoReadme(model)) = resource.loaded_value() {
            let words = ctx.say("Earlier owner README observation retained. Its links are unavailable while the current read is checked.");
            leaves.push(Leaf::new(quiet(words, &ctx.measure, ctx.palette)));
            if let CargoReadmeState::Read(document) = &model.state {
                let snippet = document.source.chars().take(512).collect::<String>();
                leaves.push(Leaf::new(div().role(gpui::Role::Label).aria_label("Earlier README text")
                    .child(quiet(ctx.say(snippet), &ctx.measure, ctx.palette))));
            }
        }
        match admission {
            CargoReadAdmission::Checking => leaves.push(Leaf::new(quiet("Reading Cargo's selected README…", &ctx.measure, ctx.palette))),
            CargoReadAdmission::Fault(error) => leaves.extend(not_ready::<BrowseValue>(&Shown::Fault(&error), &key, "The Cargo README", ctx, cx)),
            CargoReadAdmission::Unavailable(reason) => leaves.extend(not_ready::<BrowseValue>(&Shown::Unavailable(&reason, None), &key, "The Cargo README", ctx, cx)),
            CargoReadAdmission::Current => leaves.push(Leaf::new(quiet("This README reply belongs to a different browse observation.", &ctx.measure, ctx.palette))),
        }
        return leaves;
    };
    match &current.model().state {
        CargoReadmeState::Absent(reason) => {
            let words = match reason {
                backend_library::CargoPackageReadmeAbsenceV1::ManifestDisabled => "The current package manifest explicitly disables its README.",
                backend_library::CargoPackageReadmeAbsenceV1::NoCargoDefault => "The current Cargo observation found no conventional README for this package.",
            };
            vec![Leaf::new(quiet(ctx.say(words), &ctx.measure, ctx.palette))]
        }
        CargoReadmeState::Read(document) => vec![document_leaf(Arc::clone(document), dependencies, current.native_dependency(), place, ctx, cx)],
    }
}

fn document_leaf(document: Arc<CargoReadmeDocument>, dependencies: RouteDependencies, dependency: (PageKey, crate::model::pages::Stamp), place: &Route, ctx: &mut Ctx<'_>, cx: &mut Context<Reader>) -> Leaf {
    let measure = ctx.measure;
    let palette = ctx.palette;
    let rows = cx.default_global::<Cache>().for_document(&document);
    let scope = match document.origin.root_scope {
        backend_library::CargoPackageReadmeRootScopeV1::Package => "package root",
        backend_library::CargoPackageReadmeRootScopeV1::EffectiveWorkspace => "effective workspace root",
    };
    let selection = match document.origin.selection {
        backend_library::CargoPackageReadmeSelectionV1::ManifestPath => "manifest path",
        backend_library::CargoPackageReadmeSelectionV1::ManifestTrueDefault => "manifest readme = true",
        backend_library::CargoPackageReadmeSelectionV1::CargoConventionalDefault => "Cargo default",
        backend_library::CargoPackageReadmeSelectionV1::WorkspaceInherited => "workspace inheritance",
    };
    let mut column = div().flex().flex_col().gap(measure.space(Space::Base)).max_w(px(680.0 * measure.scale()))
        .child(super::head(ctx.say("Read me"), &measure, palette))
        .child(quiet(ctx.say(format!("Current owner README · {} · {scope} · {selection}. Text bytes do not establish semantic indexing.", document.origin.path.as_str())), &measure, palette));
    let rich_document = Arc::clone(&document);
    let rich_dependencies = dependencies.clone();
    let rich_guard = ctx.native_dependency_guard(dependency.clone(), cx);
    let links = ctx.links.clone();
    let recall = ctx.targets.recall();
    let scroll = ctx.reader_scroll.clone();
    let rich = crate::shell::markdown::view(ElementId::Name("cargo-owner-readme-markdown".into()), SharedString::from(Arc::clone(&document.source))).w_full();
    column = column.child(div().set(ty::PROSE, &measure).w_full()
        .on_action::<FollowMarkdownLink>(move |action, window, app| {
            if rich_guard(app) && current_origin(&rich_dependencies, &rich_document, &links, app) {
                let outcome = rich_document.destination(&action.destination);
                activate(outcome, "cargo-owner-readme-markdown".into(), &rich_document, &rich_dependencies, &links, &recall, &scroll, window, app);
            }
        }).child(rich));
    let restore = ctx.targets.left_by(place);
    if let Some(id) = restore.as_deref() {
        if let Some(at) = id.strip_prefix("cargo-readme-link-").and_then(|at| at.parse::<usize>().ok()) { rows.links.set(rows.links.get().max(at.saturating_add(1).min(512))); }
        if let Some(at) = id.strip_prefix("cargo-readme-heading-").and_then(|at| at.parse::<usize>().ok()) { rows.headings.set(rows.headings.get().max(at.saturating_add(1).min(512))); }
    }
    if !document.headings.is_empty() { column = column.child(text(ty::MONO_SMALL, &measure, palette.ink3).child("On this page")); }
    for (index, heading) in document.headings.iter().take(rows.headings.get()).enumerate() {
        let id: SharedString = format!("cargo-readme-heading-{index}").into();
        let action = destination_action(CargoReadmeDestination::Anchor(heading.clone()), id.clone(), Arc::clone(&document), dependencies.clone(), ctx, cx);
        let action = ctx.native_dependency_action(action, dependency.clone(), cx);
        ctx.targets.push(Target { id: id.clone(), label: format!("Go to {}", heading.title).into(), act: action.clone(), peek: None, source: None });
        if restore.as_ref() == Some(&id) { ctx.targets.focus(id.clone()); }
        let mut button = facet::controls::button(id.clone(), heading.title.to_string(), &measure).ghost().size(facet::Control::Small)
            .on_click(move |window, app| action(window, app));
        if let Some(focus) = ctx.native_handle(&id, cx) { button = button.focus_handle(focus); }
        column = column.child(ctx.targets.track(id, div().key_context(crate::shell::keys::NATIVE_CONTROL).child(button)));
    }
    if document.headings.len() > rows.headings.get() {
        column = column.child(more_control("cargo-readme-headings-more", "Show more README headings", Rc::clone(&rows), true, dependency.clone(), ctx, cx));
    }
    if !document.links.is_empty() { column = column.child(text(ty::MONO_SMALL, &measure, palette.ink3).child("Links")); }
    for (index, link) in document.links.iter().take(rows.links.get()).enumerate() {
        let id: SharedString = format!("cargo-readme-link-{index}").into();
        let label = if link.label.is_empty() { Arc::clone(&link.href) } else { Arc::clone(&link.label) };
        if let CargoReadmeDestination::Unavailable(reason) = &link.destination {
            column = column.child(div().role(gpui::Role::Label).aria_label(label.to_string())
                .child(text(ty::PROSE, &measure, palette.ink2).child(ctx.say(label.to_string()))).child(quiet(*reason, &measure, palette)));
            continue;
        }
        let action = destination_action(link.destination.clone(), id.clone(), Arc::clone(&document), dependencies.clone(), ctx, cx);
        let action = ctx.native_dependency_action(action, dependency.clone(), cx);
        ctx.targets.push(Target { id: id.clone(), label: label.to_string().into(), act: action.clone(), peek: None, source: None });
        if restore.as_ref() == Some(&id) { ctx.targets.focus(id.clone()); }
        let mut button = facet::controls::button(id.clone(), label.to_string(), &measure).ghost().size(facet::Control::Small)
            .on_click(move |window, app| action(window, app));
        if let Some(focus) = ctx.native_handle(&id, cx) { button = button.focus_handle(focus); }
        column = column.child(ctx.targets.track(id, div().key_context(crate::shell::keys::NATIVE_CONTROL).child(button)));
    }
    if document.links.len() > rows.links.get() {
        column = column.child(more_control("cargo-readme-links-more", "Show more README links", rows, false, dependency, ctx, cx));
    }
    column = column.child(quiet("The navigation index retains at most 512 headings and 512 links. Each file target is checked by the owner when opened.", &measure, palette));
    Leaf::new(column)
}

fn current_origin(dependencies: &RouteDependencies, document: &CargoReadmeDocument, links: &crate::shell::region::Links, app: &App) -> bool {
    dependencies.current_cargo_readme(links.store.read(app)).is_some_and(|current| {
        matches!(&current.model().state, CargoReadmeState::Read(now) if now.origin == document.origin)
    })
}

fn destination_action(destination: CargoReadmeDestination, id: SharedString, document: Arc<CargoReadmeDocument>, dependencies: RouteDependencies, ctx: &Ctx<'_>, _cx: &mut Context<Reader>) -> super::super::Act {
    let links = ctx.links.clone(); let recall = ctx.targets.recall(); let scroll = ctx.reader_scroll.clone();
    Rc::new(move |window, app| {
        if current_origin(&dependencies, &document, &links, app) { activate(destination.clone(), id.clone(), &document, &dependencies, &links, &recall, &scroll, window, app); }
    })
}

fn activate(destination: CargoReadmeDestination, id: SharedString, document: &CargoReadmeDocument, dependencies: &RouteDependencies, links: &crate::shell::region::Links, recall: &crate::shell::focus::Recall, scroll: &gpui::ScrollHandle, window: &mut Window, app: &mut App) {
    match destination {
        CargoReadmeDestination::External(url) => activate_readme_link(readme_links::Outcome::External(url), Some(id), links, recall, scroll, window, app),
        CargoReadmeDestination::Anchor(heading) => activate_readme_link(readme_links::Outcome::Anchor(heading), Some(id), links, recall, scroll, window, app),
        CargoReadmeDestination::Source(address) => {
            let Some(selected) = dependencies.readme() else { return; };
            let Some(current) = dependencies.current_cargo_readme(links.store.read(app)) else { return; };
            if !matches!(&current.model().state, CargoReadmeState::Read(now) if now.origin == document.origin) { return; }
            let dependency = current.native_dependency();
            let Some(route) = crate::core::PackageId::new(selected.package.as_str()).ok()
                .and_then(|package| CargoSourceRoute::readme_link(selected.context.clone(), package, address.clone(), address.source_line())) else { return; };
            recall.focus(id.clone()); recall.remember_leave(links.snapshot(app).route().clone(), id);
            links.dispatch_read(Intent::Navigate(Route::CargoSource(route)), dependency, app);
        }
        CargoReadmeDestination::Unavailable(reason) => super::readme_unavailable(reason, window, app),
    }
}

fn more_control(id: &'static str, label: &'static str, rows: Rc<ShownRows>, headings: bool, dependency: (PageKey, crate::model::pages::Stamp), ctx: &mut Ctx<'_>, cx: &mut Context<Reader>) -> gpui::AnyElement {
    let action: super::super::Act = Rc::new(move |window, _| {
        let count = if headings { &rows.headings } else { &rows.links };
        count.set(count.get().saturating_add(32).min(512)); window.refresh();
    });
    let action = ctx.native_dependency_action(action, dependency, cx);
    let id: SharedString = id.into();
    ctx.targets.push(Target { id: id.clone(), label: label.into(), act: action.clone(), peek: None, source: None });
    let mut button = facet::controls::button(id.clone(), label, &ctx.measure).ghost().size(facet::Control::Small)
        .on_click(move |window, app| action(window, app));
    if let Some(focus) = ctx.native_handle(&id, cx) { button = button.focus_handle(focus); }
    use gpui::IntoElement as _;
    ctx.targets.track(id, div().key_context(crate::shell::keys::NATIVE_CONTROL).child(button)).into_any_element()
}
