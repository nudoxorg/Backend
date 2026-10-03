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
use facet::{Set as _, Space, tokens::ty};
use gpui::{
    App, Context, ElementId, InteractiveElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Window, WindowId, div, px,
};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::Arc;

const MAX_PAGING_VISITS: usize = 8;

/// Reader-owned neutral presentation memory. No producer object, read lease,
/// callback, or source bytes are retained here; prepared navigation stays shared.
#[derive(Default)]
pub(crate) struct PagingMemory {
    visits: VecDeque<PagingVisit>,
}

struct PagingVisit {
    window: WindowId,
    place: u64,
    document: Arc<str>,
    rows: Rc<ShownPages>,
}

/// The actual rendering window supplied by Reader, rather than App-global UI
/// state. Cloning this handle does not grant any content/action admission.
#[derive(Clone)]
pub(crate) struct PagingScope {
    window: WindowId,
    memory: Rc<RefCell<PagingMemory>>,
}

impl PagingScope {
    pub(crate) fn new(window: WindowId, memory: Rc<RefCell<PagingMemory>>) -> Self {
        Self { window, memory }
    }

    fn for_document(
        &self,
        place: u64,
        document: &CargoReadmeDocument,
        active: bool,
    ) -> Rc<ShownPages> {
        self.memory
            .borrow_mut()
            .for_document(self.window, place, document, active)
    }
}

#[derive(Default)]
struct ShownPages {
    links: Cell<CargoReadmeNavigationPage>,
    headings: Cell<CargoReadmeNavigationPage>,
    restored: RefCell<Option<SharedString>>,
}
impl PagingMemory {
    fn for_document(
        &mut self,
        window: WindowId,
        place: u64,
        document: &CargoReadmeDocument,
        active: bool,
    ) -> Rc<ShownPages> {
        if let Some(at) = self.visits.iter().position(|visit| {
            visit.window == window
                && visit.place == place
                && visit.document.as_ref() == document.identity()
        }) {
            if active && at + 1 < self.visits.len() {
                if let Some(visit) = self.visits.remove(at) {
                    let rows = Rc::clone(&visit.rows);
                    self.visits.push_back(visit);
                    return rows;
                }
            }
            return Rc::clone(&self.visits[at].rows);
        }
        let rows = Rc::new(ShownPages::default());
        if active {
            if self.visits.len() >= MAX_PAGING_VISITS {
                self.visits.pop_front();
            }
            self.visits.push_back(PagingVisit {
                window,
                place,
                document: Arc::from(document.identity()),
                rows: Rc::clone(&rows),
            });
        }
        rows
    }
}

#[cfg(test)]
mod paging_tests {
    #![allow(clippy::expect_used, clippy::panic)]
    use super::*;

    fn document() -> Arc<CargoReadmeDocument> {
        let contents = (0..96)
            .map(|at| format!("[Link {at}](https://example.com/{at})\n\n"))
            .collect::<String>();
        let (_, value) =
            crate::runtime::cargo_readme_reads::tests::fixture_with_contents(&contents);
        let crate::model::pages::PageValue::Browse(BrowseValue::CargoReadme(model)) = value else {
            panic!("README model");
        };
        let CargoReadmeState::Read(document) = &model.state else {
            panic!("complete README");
        };
        Arc::clone(document)
    }

    #[test]
    fn window_and_reader_visit_paging_remain_independent_for_shared_document() {
        // Synthetic window IDs test neutral UI storage, not native/owner acceptance.
        // Sharing a memory instance deliberately exercises the stronger boundary.
        let memory = Rc::new(RefCell::new(PagingMemory::default()));
        let first = PagingScope::new(WindowId::from(1), Rc::clone(&memory));
        let second = PagingScope::new(WindowId::from(2), Rc::clone(&memory));
        let document = document();
        let first_rows = first.for_document(7, &document, true);
        first_rows
            .links
            .set(CargoReadmeNavigationPage::containing(64));
        *first_rows.restored.borrow_mut() = Some("first window focus".into());
        let second_rows = second.for_document(7, &document, true);
        assert!(!Rc::ptr_eq(&first_rows, &second_rows));
        assert_eq!(
            second_rows.links.get().range(document.link_count()).start,
            0
        );
        assert!(second_rows.restored.borrow().is_none());
        assert!(Rc::ptr_eq(
            &first_rows,
            &first.for_document(7, &document, true)
        ));
        let next_visit = first.for_document(8, &document, true);
        assert!(!Rc::ptr_eq(&first_rows, &next_visit));
        assert_eq!(next_visit.links.get().range(document.link_count()).start, 0);
        assert_eq!(
            first_rows.links.get().range(document.link_count()).start,
            64
        );
    }

    #[test]
    fn changed_document_origin_or_content_never_restores_old_paging_state() {
        let memory = Rc::new(RefCell::new(PagingMemory::default()));
        let scope = PagingScope::new(WindowId::from(1), memory);
        let document = document();
        let rows = scope.for_document(7, &document, true);
        rows.links.set(CargoReadmeNavigationPage::containing(64));
        let mut origin = document.origin.clone();
        let contents: Arc<str> = Arc::from(format!("{}\nchanged bytes", document.source));
        origin.content_digest = *blake3::hash(contents.as_bytes()).as_bytes();
        let changed =
            CargoReadmeDocument::prepare(origin, contents, &|| false).expect("prepared new bytes");
        let changed_rows = scope.for_document(7, &changed, true);
        assert!(!Rc::ptr_eq(&rows, &changed_rows));
        assert_eq!(
            changed_rows.links.get().range(changed.link_count()).start,
            0
        );
        let mut origin = document.origin.clone();
        origin.path = backend_library::CargoPackageSourcePathV1::new("README-other.md")
            .expect("typed relative path");
        let changed = CargoReadmeDocument::prepare(origin, Arc::clone(&document.source), &|| false)
            .expect("prepared different origin");
        assert!(!Rc::ptr_eq(&rows, &scope.for_document(7, &changed, true)));
    }

    #[test]
    fn neutral_paging_retention_is_bounded_and_passive_plates_do_not_renew_it() {
        let document = document();
        let mut memory = PagingMemory::default();
        let window = WindowId::from(1);
        let first = memory.for_document(window, 0, &document, true);
        for place in 1..MAX_PAGING_VISITS as u64 {
            memory.for_document(window, place, &document, true);
        }
        let passive = memory.for_document(window, 0, &document, false);
        assert!(Rc::ptr_eq(&first, &passive));
        for place in 100..164 {
            memory.for_document(window, place, &document, false);
        }
        assert_eq!(memory.visits.len(), MAX_PAGING_VISITS);
        memory.for_document(window, MAX_PAGING_VISITS as u64, &document, true);
        assert_eq!(memory.visits.len(), MAX_PAGING_VISITS);
        assert!(!memory.visits.iter().any(|visit| visit.place == 0));
        assert!(!Rc::ptr_eq(
            &first,
            &memory.for_document(window, 0, &document, true)
        ));
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
        if let Some(BrowseValue::CargoReadme(model)) = resource.loaded_value()
            && model.package == selected.package
            && model.request_binding == selected.context.request_binding()
        {
            match &model.state {
                CargoReadmeState::Read(document)
                    if &document.origin.package == selected.package.reference()
                        && document.origin.request_binding == selected.context.request_binding()
                        && *blake3::hash(document.source.as_bytes()).as_bytes() == document.origin.content_digest => {
                    let words = ctx.say("Earlier owner README observation retained. Its links are unavailable while the current read is checked.");
                    leaves.push(Leaf::new(quiet(words, &ctx.measure, ctx.palette)));
                    let snippet = document.source.chars().take(512).collect::<String>();
                    leaves.push(Leaf::new(
                        div()
                            .id("cargo-readme-retained-snippet")
                            .role(gpui::Role::Label)
                            .aria_label("Earlier README text")
                            .child(quiet(ctx.say(snippet), &ctx.measure, ctx.palette)),
                    ));
                }
                CargoReadmeState::Absent(_) => leaves.push(Leaf::new(quiet(
                    "An earlier Cargo observation selected no README; the current selection is still being checked.",
                    &ctx.measure, ctx.palette,
                ))),
                CargoReadmeState::Read(_) => leaves.push(Leaf::new(quiet(
                    "The retained README source origin does not match this address; its text cannot be shown here.",
                    &ctx.measure, ctx.palette,
                ))),
            }
        } else if resource.loaded_value().is_some() {
            leaves.push(Leaf::new(quiet(
                "The retained README belongs to another package, binding or source origin; its text cannot be shown here.",
                &ctx.measure, ctx.palette,
            )));
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
    let rows = ctx
        .readme_paging
        .for_document(ctx.place_key, &document, ctx.active);
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
    let link_disclosure = Arc::clone(&document);
    let rich = crate::shell::markdown::scoped_view(
        ElementId::Name(document_id),
        SharedString::from(Arc::clone(&document.source)),
        painted.identity(),
    )
    .w_full()
    .link_availability(move |url| link_disclosure.link_actionable(url))
    .link_admission(Rc::clone(&rich_guard));
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
        let target_action = ctx.target_dependency_action(action, dependency.clone(), cx);
        let action = target_action.callback();
        ctx.targets.push(Target {
            id: id.clone(),
            label: format!("Go to {heading_title}").into(),
            action: target_action.clone(),
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
                    .id(id.clone())
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
        let target_action = ctx.target_dependency_action(action, dependency.clone(), cx);
        let action = target_action.callback();
        ctx.targets.push(Target {
            id: id.clone(),
            label: label.to_string().into(),
            action: target_action.clone(),
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
        let target_action = ctx.target_dependency_action(action, dependency.clone(), cx);
        let action = target_action.callback();
        ctx.targets.push(Target {
            id: id.clone(),
            label: label.clone(),
            action: target_action.clone(),
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
