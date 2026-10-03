//! Current owner README paint and native actions, independent of a dossier.

use super::{Ctx, Leaf, activate_readme_link, readme_links};
use crate::model::browse::{
    BrowseKey, BrowseValue, CargoReadmeDestination, CargoReadmeDocument, CargoReadmeFocus,
    CargoReadmeNavigationPage, CargoReadmeState,
};
use crate::model::pages::PageKey;
use crate::navigation::{CargoSourceRoute, Intent, Route};
use crate::runtime::store::{CargoReadAdmission, RouteDependencies};
use crate::shell::bodies::state::{Shown, not_ready};
use crate::shell::focus::Target;
use crate::shell::kit::{quiet, text};
use crate::shell::markdown::FollowMarkdownLink;
use crate::shell::reader::Reader;
use facet::{Space, tokens::ty};
use gpui::{
    App, AppContext as _, Context, ElementId, Global, InteractiveElement, ParentElement,
    SharedString, Styled, Window, div, px,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

#[derive(Default)]
struct Cache(Option<(backend_library::CargoPackageReadmeOriginV1, Rc<ShownPages>)>);
impl Global for Cache {}
struct ShownPages {
    links: Cell<CargoReadmeNavigationPage>,
    headings: Cell<CargoReadmeNavigationPage>,
    restored: RefCell<Option<SharedString>>,
}
impl Cache {
    fn for_document(&mut self, document: &CargoReadmeDocument) -> Rc<ShownPages> {
        if let Some((origin, rows)) = &self.0
            && origin == &document.origin
        {
            return Rc::clone(rows);
        }
        let rows = Rc::new(ShownPages {
            links: Cell::default(),
            headings: Cell::default(),
            restored: RefCell::default(),
        });
        // Neutral disclosure state only: this cache never holds read admission.
        self.0 = Some((document.origin.clone(), Rc::clone(&rows)));
        rows
    }
}

pub(super) fn body(place: &Route, ctx: &mut Ctx<'_>, cx: &mut Context<Reader>) -> Vec<Leaf> {
    let dependencies = RouteDependencies::new(place, None);
    let Some(selected) = dependencies.readme() else {
        return Vec::new();
    };
    let key = PageKey::Browse(BrowseKey::CargoReadme(selected.clone()));
    let live = ctx.links.store.read(cx);
    let resource = live
        .pages()
        .browse(&BrowseKey::CargoReadme(selected.clone()));
    let admission = live.cargo_read_admission(&key, &resource);
    let current = dependencies.current_cargo_readme(live);
    let Some(current) = current else {
        let mut leaves = Vec::new();
        if let Some(BrowseValue::CargoReadme(model)) = resource.loaded_value() {
            let words = ctx.say("Earlier owner README observation retained. Its links are unavailable while the current read is checked.");
            leaves.push(Leaf::new(quiet(words, &ctx.measure, ctx.palette)));
            if let CargoReadmeState::Read(document) = &model.state {
                let snippet = document.source.chars().take(512).collect::<String>();
                leaves.push(Leaf::new(
                    div()
                        .role(gpui::Role::Label)
                        .aria_label("Earlier README text")
                        .child(quiet(ctx.say(snippet), &ctx.measure, ctx.palette)),
                ));
            }
        }
        match admission {
            CargoReadAdmission::Checking => leaves.push(Leaf::new(quiet(
                "Reading Cargo's selected README…",
                &ctx.measure,
                ctx.palette,
            ))),
            CargoReadAdmission::Fault(error) => leaves.extend(not_ready::<BrowseValue>(
                &Shown::Fault(&error),
                &key,
                "The Cargo README",
                ctx,
                cx,
            )),
            CargoReadAdmission::Unavailable(reason) => leaves.extend(not_ready::<BrowseValue>(
                &Shown::Unavailable(&reason, None),
                &key,
                "The Cargo README",
                ctx,
                cx,
            )),
            CargoReadAdmission::Current => leaves.push(Leaf::new(quiet(
                "This README needs its exact current Cargo Tree receipt before its links can open.",
                &ctx.measure,
                ctx.palette,
            ))),
        }
        return leaves;
    };
    match &current.model().state {
        CargoReadmeState::Absent(reason) => {
            let words = match reason {
                backend_library::CargoPackageReadmeAbsenceV1::ManifestDisabled => {
                    "The current package manifest explicitly disables its README."
                }
                backend_library::CargoPackageReadmeAbsenceV1::NoCargoDefault => {
                    "The current Cargo observation found no conventional README for this package."
                }
            };
            vec![Leaf::new(quiet(ctx.say(words), &ctx.measure, ctx.palette))]
        }
        CargoReadmeState::Read(document) => vec![document_leaf(
            Arc::clone(document),
            dependencies,
            current.native_dependency(),
            place,
            ctx,
            cx,
        )],
    }
}

fn document_leaf(
    document: Arc<CargoReadmeDocument>,
    dependencies: RouteDependencies,
    dependency: (PageKey, crate::model::pages::Stamp),
    place: &Route,
    ctx: &mut Ctx<'_>,
    cx: &mut Context<Reader>,
) -> Leaf {
    let measure = ctx.measure;
    let palette = ctx.palette;
    let rows = cx.default_global::<Cache>().for_document(&document);
    let paint_place = ctx.place_key;
    let paint_stamp = dependency.1;
    let painted = document.paint(paint_place, paint_stamp);
    let scope = match document.origin.root_scope {
        backend_library::CargoPackageReadmeRootScopeV1::Package => "package root",
        backend_library::CargoPackageReadmeRootScopeV1::EffectiveWorkspace => {
            "effective workspace root"
        }
    };
    let selection = match document.origin.selection {
        backend_library::CargoPackageReadmeSelectionV1::ManifestPath => "manifest path",
        backend_library::CargoPackageReadmeSelectionV1::ManifestTrueDefault => {
            "manifest readme = true"
        }
        backend_library::CargoPackageReadmeSelectionV1::CargoConventionalDefault => "Cargo default",
        backend_library::CargoPackageReadmeSelectionV1::WorkspaceInherited => {
            "workspace inheritance"
        }
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
    let document_id = SharedString::from(painted.identity().document_id());
    let rich_id = document_id.clone();
    let rich = crate::shell::markdown::scoped_view(
        ElementId::Name(document_id),
        SharedString::from(Arc::clone(&document.source)),
        painted.identity(),
    )
    .w_full();
    column = column.child(
        div()
            .set(ty::PROSE, &measure)
            .w_full()
            .on_action::<FollowMarkdownLink>(move |action, window, app| {
                if rich_guard(app)
                    && current_origin(&rich_dependencies, &rich_document, &links, app)
                {
                    let outcome = rich_document
                        .paint(paint_place, paint_stamp)
                        .destination(&action.destination);
                    let id = rich_document
                        .inline_focus_id(&action.destination)
                        .map_or_else(|| rich_id.clone(), SharedString::from);
                    activate(
                        outcome,
                        id,
                        &rich_document,
                        &rich_dependencies,
                        &links,
                        &recall,
                        &scroll,
                        window,
                        app,
                    );
                }
            })
            .child(rich),
    );
    let restore = ctx.targets.left_by(place);
    if *rows.restored.borrow() != restore {
        if let Some(endpoint) = restore.as_deref().and_then(|id| document.restore_focus(id)) {
            let (page, at) = match endpoint {
                CargoReadmeFocus::Link(at) => (&rows.links, at),
                CargoReadmeFocus::Heading(at) => (&rows.headings, at),
            };
            page.set(CargoReadmeNavigationPage::containing(at));
        }
        *rows.restored.borrow_mut() = restore.clone();
    }
    let heading_range = rows.headings.get().range(document.heading_count());
    if !heading_range.is_empty() {
        column = column.child(text(ty::MONO_SMALL, &measure, palette.ink3).child(format!(
            "On this page · {}–{} of {}",
            heading_range.start + 1,
            heading_range.end,
            document.heading_count()
        )));
    }
    for index in heading_range {
        let Some(heading) = painted.heading(index) else {
            continue;
        };
        let Some(id) = document.heading_id(index) else {
            continue;
        };
        let id = SharedString::from(id);
        let heading_title = if heading.title.is_empty() {
            "Untitled heading"
        } else {
            heading.title.as_ref()
        };
        let action = destination_action(
            CargoReadmeDestination::Anchor(heading.clone()),
            id.clone(),
            Arc::clone(&document),
            dependencies.clone(),
            ctx,
            cx,
        );
        let action = ctx.native_dependency_action(action, dependency.clone(), cx);
        ctx.targets.push(Target {
            id: id.clone(),
            label: format!("Go to {heading_title}").into(),
            act: action.clone(),
            peek: None,
            source: None,
        });
        if restore.as_ref() == Some(&id) {
            ctx.targets.focus(id.clone());
        }
        let mut button = facet::controls::button(id.clone(), heading_title.to_owned(), &measure)
            .ghost()
            .size(facet::Control::Small)
            .on_click(move |window, app| action(window, app));
        if let Some(focus) = ctx.native_handle(&id, cx) {
            button = button.focus_handle(focus);
        }
        column = column.child(
            ctx.targets.track(
                id,
                div()
                    .key_context(crate::shell::keys::NATIVE_CONTROL)
                    .child(button),
            ),
        );
    }
    column = column.child(page_controls(
        &document,
        Rc::clone(&rows),
        true,
        document.heading_count(),
        dependency.clone(),
        ctx,
        cx,
    ));
    let link_range = rows.links.get().range(document.link_count());
    if !link_range.is_empty() {
        column = column.child(text(ty::MONO_SMALL, &measure, palette.ink3).child(format!(
            "Links · {}–{} of {}",
            link_range.start + 1,
            link_range.end,
            document.link_count()
        )));
    }
    for index in link_range {
        let Some(link) = painted.link(index) else {
            continue;
        };
        let id = SharedString::from(Arc::clone(&link.id));
        let label = if link.label.is_empty() {
            Arc::clone(&link.href)
        } else {
            Arc::clone(&link.label)
        };
        if let CargoReadmeDestination::Unavailable(reason) = &link.destination {
            column = column.child(
                div()
                    .role(gpui::Role::Label)
                    .aria_label(label.to_string())
                    .child(
                        text(ty::PROSE, &measure, palette.ink2).child(ctx.say(label.to_string())),
                    )
                    .child(quiet(*reason, &measure, palette)),
            );
            continue;
        }
        let action = destination_action(
            link.destination,
            id.clone(),
            Arc::clone(&document),
            dependencies.clone(),
            ctx,
            cx,
        );
        let action = ctx.native_dependency_action(action, dependency.clone(), cx);
        ctx.targets.push(Target {
            id: id.clone(),
            label: label.to_string().into(),
            act: action.clone(),
            peek: None,
            source: None,
        });
        if restore.as_ref() == Some(&id) {
            ctx.targets.focus(id.clone());
        }
        let mut button = facet::controls::button(id.clone(), label.to_string(), &measure)
            .ghost()
            .size(facet::Control::Small)
            .on_click(move |window, app| action(window, app));
        if let Some(focus) = ctx.native_handle(&id, cx) {
            button = button.focus_handle(focus);
        }
        column = column.child(
            ctx.targets.track(
                id,
                div()
                    .key_context(crate::shell::keys::NATIVE_CONTROL)
                    .child(button),
            ),
        );
    }
    column = column.child(page_controls(
        &document,
        rows,
        false,
        document.link_count(),
        dependency,
        ctx,
        cx,
    ));
    Leaf::new(column)
}

fn current_origin(
    dependencies: &RouteDependencies,
    document: &CargoReadmeDocument,
    links: &crate::shell::region::Links,
    app: &App,
) -> bool {
    dependencies.current_cargo_readme(links.store.read(app)).is_some_and(|current| {
        matches!(&current.model().state, CargoReadmeState::Read(now) if now.origin == document.origin)
    })
}

fn destination_action(
    destination: CargoReadmeDestination,
    id: SharedString,
    document: Arc<CargoReadmeDocument>,
    dependencies: RouteDependencies,
    ctx: &Ctx<'_>,
    _cx: &mut Context<Reader>,
) -> super::super::Act {
    let links = ctx.links.clone();
    let recall = ctx.targets.recall();
    let scroll = ctx.reader_scroll.clone();
    Rc::new(move |window, app| {
        if current_origin(&dependencies, &document, &links, app) {
            activate(
                destination.clone(),
                id.clone(),
                &document,
                &dependencies,
                &links,
                &recall,
                &scroll,
                window,
                app,
            );
        }
    })
}

fn activate(
    destination: CargoReadmeDestination,
    id: SharedString,
    document: &CargoReadmeDocument,
    dependencies: &RouteDependencies,
    links: &crate::shell::region::Links,
    recall: &crate::shell::focus::Recall,
    scroll: &gpui::ScrollHandle,
    window: &mut Window,
    app: &mut App,
) {
    match destination {
        CargoReadmeDestination::External(url) => activate_readme_link(
            readme_links::Outcome::External(url),
            Some(id),
            links,
            recall,
            scroll,
            window,
            app,
        ),
        CargoReadmeDestination::Anchor(heading) => activate_readme_link(
            readme_links::Outcome::Anchor(heading),
            Some(id),
            links,
            recall,
            scroll,
            window,
            app,
        ),
        CargoReadmeDestination::Source(address) => {
            let Some(selected) = dependencies.readme() else {
                return;
            };
            let Some(current) = dependencies.current_cargo_readme(links.store.read(app)) else {
                return;
            };
            if !matches!(&current.model().state, CargoReadmeState::Read(now) if now.origin == document.origin)
            {
                return;
            }
            let dependency = current.native_dependency();
            let Some(route) = crate::core::PackageId::new(selected.package.as_str())
                .ok()
                .and_then(|package| {
                    CargoSourceRoute::readme_link(
                        selected.context.clone(),
                        package,
                        address.clone(),
                        address.source_line(),
                    )
                })
            else {
                return;
            };
            recall.focus(id.clone());
            recall.remember_leave(links.snapshot(app).route().clone(), id);
            links.dispatch_read(Intent::Navigate(Route::CargoSource(route)), dependency, app);
        }
        CargoReadmeDestination::Unavailable(reason) => {
            super::readme_unavailable(reason, window, app)
        }
    }
}

fn page_controls(
    document: &CargoReadmeDocument,
    rows: Rc<ShownPages>,
    headings: bool,
    total: usize,
    dependency: (PageKey, crate::model::pages::Stamp),
    ctx: &mut Ctx<'_>,
    cx: &mut Context<Reader>,
) -> gpui::AnyElement {
    let page = if headings {
        rows.headings.get()
    } else {
        rows.links.get()
    };
    let kind = if headings { "headings" } else { "links" };
    let mut controls = div().flex().gap(ctx.measure.space(Space::Snug));
    for (direction, target) in [("Previous", page.previous()), ("Next", page.next(total))] {
        let Some(target) = target else {
            continue;
        };
        let id: SharedString = format!("{}-{kind}-{direction}", document.identity()).into();
        let label: SharedString = format!("{direction} README {kind}").into();
        let pages = Rc::clone(&rows);
        let action: super::super::Act = Rc::new(move |window, _| {
            let page = if headings {
                &pages.headings
            } else {
                &pages.links
            };
            page.set(target);
            window.refresh();
        });
        let action = ctx.native_dependency_action(action, dependency.clone(), cx);
        ctx.targets.push(Target {
            id: id.clone(),
            label: label.clone(),
            act: action.clone(),
            peek: None,
            source: None,
        });
        let mut button = facet::controls::button(id.clone(), label, &ctx.measure)
            .ghost()
            .size(facet::Control::Small)
            .on_click(move |window, app| action(window, app));
        if let Some(focus) = ctx.native_handle(&id, cx) {
            button = button.focus_handle(focus);
        }
        controls = controls.child(
            ctx.targets.track(
                id,
                div()
                    .key_context(crate::shell::keys::NATIVE_CONTROL)
                    .child(button),
            ),
        );
    }
    use gpui::IntoElement as _;
    controls.into_any_element()
}
