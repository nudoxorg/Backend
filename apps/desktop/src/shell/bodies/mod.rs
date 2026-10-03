//! Plain page bodies: each route rendered from its read model — names,
//! kinds, signatures, docs, members, counts — enough to prove the data flow
//! and to assert rendered content. The designed pages are wave 3; these
//! follow the calm targets' structure (hero, one facts line, sections of
//! mark + name rows) without the page-specific marks other lanes own.
//!
//! A body is a list of [`Leaf`]s: a main block and an optional margin note.
//! The reader sets notes beside their block when its room is wide and folds
//! each under its block otherwise.

mod browse;
pub(crate) mod graph;
mod inbox;
mod orbit;
mod package;
mod retained;
mod settings;
mod source;
pub(crate) use source::paging::PagingState;
mod state;
mod symbol;

use gpui::Window;

/// How many declaration pages are still reading their lines (the harness
/// waits for none before a capture).


use super::focus::{Act, Targets};
use super::kit::HoverIntent;
use super::reader::Reader;
use super::region::Links;
use crate::model::AppSnapshot;
use crate::navigation::{Overlay, Route, View};
use crate::core::Resource;
use crate::model::pages::{
    CargoSourceKey, CargoSourcePage, HealthModel, OrbitModel, PackageDossier, PackageRef, PageKey, SourceView, SymbolPage, SymbolRef,
};
use crate::runtime::store::{DataStore, RouteDependencies};
use facet::{Measure, Palette, Reveal};
use gpui::{AnyElement, Context, FocusHandle, SharedString};
use std::collections::BTreeMap;
use std::rc::Rc;

/// One block of a page, with its margin note.
pub(crate) struct Leaf {
    /// The block.
    pub main: AnyElement,
    /// Its note: beside it when the reader is wide, under it otherwise.
    pub note: Option<AnyElement>,
    /// The block takes the wide measure (`Ctx::wide`) rather than the reading
    /// column: a table, a rail, a ring of names. Prose never does.
    pub wide: bool,
}

impl Leaf {
    pub(crate) fn new(main: impl gpui::IntoElement) -> Self {
        Self {
            main: main.into_any_element(),
            note: None,
            wide: false,
        }
    }

    /// The block may take the wide measure (`Ctx::wide`): in a big window it
    /// fills what the reading column leaves empty.
    pub(crate) fn wide(mut self) -> Self {
        self.wide = true;
        self
    }

    pub(crate) fn with_note(mut self, note: impl gpui::IntoElement) -> Self {
        self.note = Some(note.into_any_element());
        self
    }
}

/// Everything a body builds with.
pub(crate) struct Ctx<'a> {
    /// Only the current page publishes shared motion endpoints.
    pub active: bool,
    /// The current page may claim native input only after its own transition
    /// and the shell's sampled Ask presentation have both released it.
    pub native_input_active: bool,
    /// The folio's measure.
    pub measure: Measure,
    /// The margin's measure (the folio's when notes fold under).
    pub note: Measure,
    /// The measure wide content (tables, rails, comparisons) may take: the
    /// folio's until 1440 px, then growing to fill a big window
    /// (`facet::tokens::fluid::WIDE_FOLIO`). Prose keeps `measure`.
    pub wide: Measure,
    /// The whole room the reader gives a page, gutters excluded, exactly as
    /// this frame's window has it (never a frame behind, as the scroller's
    /// own bounds are): for a page that fills the window rather than a
    /// column of it (the package page's territory).
    pub content: Measure,
    /// The reader's layout modes, for a page whose own arrangement changes
    /// with its room (`Modes::settle`, `Modes::columns`): held through a
    /// hysteresis band so a width on the edge cannot flip it every frame.
    pub modes: facet::fluid::Modes,
    /// The Library's ring of names: their flow (`facet::motion::Flow`).
    pub ring_flow: facet::motion::Flow,
    /// The active palette.
    pub palette: &'static Palette,
    /// Held reveal modes.
    pub reveal: Reveal,
    /// Intents, data, the shell.
    pub links: &'a Links,
    /// Keyboard targets of the reader.
    pub targets: &'a Targets,
    /// Native reading viewport for keyboard-only reveal of a chosen row.
    pub reader_scroll: gpui::ScrollHandle,
    /// Signal the reader to reveal an explicitly routed source line.
    pub reader_reveal: std::rc::Rc<std::cell::Cell<bool>>,
    /// One-time initial focus for the exact arriving place, requested line,
    /// and source revision. It is deliberately separate from live keyboard
    /// focus so later user navigation is never pulled back on rerender.
    pub source_focus_applied: std::rc::Rc<std::cell::Cell<Option<(u64, u32, Option<crate::shell::reader::SourceGeneration>)>>>,
    /// Exact Reader place whose content this body represents.
    pub place_key: u64,
    /// Shell input interruption generation at this body render.
    pub native_return_interruption: u64,
    /// Exact indexed-source revision or owner-verified Cargo file digest.
    pub source_generation: Option<crate::shell::reader::SourceGeneration>,
    /// Source cursor/history retained by Reader across page body unmounts.
    pub source_paging: std::rc::Rc<std::cell::RefCell<Option<PagingState>>>,
    /// Exact tree disclosure and virtual-list scroll state retained across Back.
    pub library_state: std::rc::Rc<std::cell::RefCell<facet::browse::library::State>>,
    /// The page's lens (tab) for declaration pages.
    pub lens: Lens,
    /// The text each body renders, recorded for content assertions.
    pub said: &'a mut Vec<SharedString>,
    /// The hero name's lines, as fitted (the name never ellipsizes).
    pub hero: &'a mut Vec<SharedString>,
    /// Stable, bounded per-declaration disclosure and clip motion state.
    pub symbol_disclosure: super::reader::SymbolDisclosure,
    /// Bounded package outline; expanded only on the current package route.
    pub package_outline_expanded: bool,
    /// Explicit Find choices retained through Compare and route history.
    pub find_held: Vec<facet::browse::find::HeldPackage>,
    /// A declaration page reached by a hop forward from another declaration:
    /// the one it came from, which the page rings where it finds it.
    pub arrived_from: Option<crate::model::pages::SymbolRef>,
}

#[derive(Clone, Copy)]
enum NativeActionKind { LocalUi, OwnerSnapshot, Resource }

impl Ctx<'_> {
    /// An action from a drawn control belongs to one Reader visit and one
    /// owner revision. Retained transition bodies and stale pointer events
    /// cannot navigate after that visit has been replaced.
    pub(crate) fn native_action(&self, action: Act, cx: &mut Context<Reader>) -> Act {
        self.native_action_with_inventory(action, None, None, NativeActionKind::Resource, cx)
    }

    /// A current factory selected a read independent of an optional dossier.
    pub(crate) fn native_dependency_action(&self, action: Act, dependency: (PageKey, crate::model::pages::Stamp), cx: &mut Context<Reader>) -> Act {
        self.native_action_with_inventory(action, None, Some(dependency), NativeActionKind::Resource, cx)
    }

    /// A local project or recent route is in the snapshot rather than Orbit
    /// bytes, but entering it still requires the current owner attachment.
    pub(crate) fn native_snapshot_action(&self, action: Act, cx: &mut Context<Reader>) -> Act {
        self.native_action_with_inventory(action, None, None, NativeActionKind::OwnerSnapshot, cx)
    }

    /// Local setup, recovery and disclosure remain available while the
    /// indexing owner starts or fails. They still belong to one Reader visit.
    pub(crate) fn native_local_action(&self, action: Act, cx: &mut Context<Reader>) -> Act {
        self.native_action_with_inventory(action, None, None, NativeActionKind::LocalUi, cx)
    }

    pub(crate) fn native_inventory_action(&self, action: Act, revision: [u8; 32], cx: &mut Context<Reader>) -> Act {
        self.native_action_with_inventory(action, Some(revision), None, NativeActionKind::Resource, cx)
    }

    fn native_action_with_inventory(&self, action: Act, inventory_revision: Option<[u8; 32]>, selected: Option<(PageKey, crate::model::pages::Stamp)>, kind: NativeActionKind, cx: &mut Context<Reader>) -> Act {
        let guard = self.native_guard_with_inventory(inventory_revision, selected, kind, cx);
        std::rc::Rc::new(move |window, app| { if guard(app) { action(window, app); } })
    }

    /// Setup controls require the exact painted visit AND the transient input
    /// generation that exposed them. This predicate is idempotent so native
    /// controls can check before focus on MouseDown and again before activation.
    pub(crate) fn native_local_guard(
        &self,
        cx: &mut Context<Reader>,
    ) -> Rc<dyn Fn(&mut gpui::App) -> bool> {
        let shell = self.links.shell.clone();
        let scope = shell
            .upgrade()
            .and_then(|shell| shell.read(cx).page_input_scope(cx));
        let reader = cx.weak_entity();
        let place = self.place_key;
        let snapshot = self.links.snapshot(cx);
        let route = snapshot.route().clone();
        let overlay = snapshot.overlay();
        let root = snapshot.key();
        Rc::new(move |app| {
            let Some(scope) = scope.as_ref() else { return false };
            shell
                .upgrade()
                .is_some_and(|shell| shell.read(app).admits_page_input_scope(scope, app))
                && reader.upgrade().is_some_and(|reader| {
                    let reader = reader.read(app);
                    reader.native_input_for(&route, overlay)
                        && reader.admits_native_visit(
                            place,
                            &route,
                            overlay,
                            root,
                            &super::reader::NativeActionLease::LocalUi,
                            None,
                            None,
                            app,
                        )
                })
        })
    }

    pub(crate) fn native_dependency_guard(&self, dependency: (PageKey, crate::model::pages::Stamp), cx: &mut Context<Reader>) -> std::rc::Rc<dyn Fn(&mut gpui::App) -> bool> {
        self.native_guard_with_inventory(None, Some(dependency), NativeActionKind::Resource, cx)
    }

    fn native_guard_with_inventory(&self, inventory_revision: Option<[u8; 32]>, selected: Option<(PageKey, crate::model::pages::Stamp)>, kind: NativeActionKind, cx: &mut Context<Reader>) -> std::rc::Rc<dyn Fn(&mut gpui::App) -> bool> {
        let reader = cx.weak_entity();
        let place = self.place_key;
        let snapshot = self.links.snapshot(cx);
        let route = snapshot.route().clone();
        let overlay = snapshot.overlay();
        let root = snapshot.key();
        let source = inventory_revision.is_none().then_some(self.source_generation).flatten();
        let lease = self.native_lease(&route, overlay, inventory_revision.is_some(), selected, kind, cx);
        std::rc::Rc::new(move |app| {
            reader.upgrade().is_some_and(|reader| reader.update(app, |reader, cx| {
                let admitted = reader.admits_native_visit(place, &route, overlay, root, &lease, source, inventory_revision, cx);
                if admitted { reader.cancel_native_return(); }
                admitted
            }))
        })
    }

    fn native_lease(
        &self,
        route: &Route,
        overlay: Option<Overlay>,
        inventory: bool,
        selected: Option<(PageKey, crate::model::pages::Stamp)>,
        kind: NativeActionKind,
        cx: &Context<Reader>,
    ) -> super::reader::NativeActionLease {
        let store = self.links.store.read(cx);
        match kind {
            NativeActionKind::LocalUi => super::reader::NativeActionLease::LocalUi,
            NativeActionKind::OwnerSnapshot => {
                super::reader::NativeActionLease::OwnerSnapshot(store.current_owner_attachment())
            }
            NativeActionKind::Resource => super::reader::NativeActionLease::Resource {
                attachment: store.current_owner_attachment(),
                stamp: selected.or_else(|| RouteDependencies::new(route, overlay).native_stamp(store, inventory)),
            },
        }
    }

    pub(crate) fn native_resource_lease(&self, cx: &Context<Reader>) -> super::reader::NativeActionLease {
        let snapshot = self.links.snapshot(cx);
        self.native_lease(snapshot.route(), snapshot.overlay(), false, None, NativeActionKind::Resource, cx)
    }

    /// A mounted facet row supplies the handle; Reader owns whether a later
    /// deferred transfer is still the settled, current keyboard visit.
    pub(crate) fn native_return_focus(
        &self,
        cx: &mut Context<Reader>,
    ) -> Rc<
        dyn Fn(
            FocusHandle,
            &mut Window,
            &mut gpui::App,
        ) -> facet::browse::library::ReturnDisposition,
    > {
        let reader = cx.weak_entity();
        let place = self.place_key;
        let snapshot = self.links.snapshot(cx);
        let route = snapshot.route().clone();
        let overlay = snapshot.overlay();
        let root = snapshot.key();
        let lease = self.native_lease(&route, overlay, false, None, NativeActionKind::Resource, cx);
        let interruption = self.native_return_interruption;
        Rc::new(move |focus, window, app| {
            let Some(reader) = reader.upgrade() else {
                return facet::browse::library::ReturnDisposition::Invalid;
            };
            let result = reader.update(app, |reader, cx| {
                reader.native_return_disposition(
                    place,
                    &route,
                    overlay,
                    root,
                    &lease,
                    interruption,
                    window,
                    cx,
                )
            });
            if result == facet::browse::library::ReturnDisposition::Applied {
                window.focus(&focus, app);
            }
            result
        })
    }

    /// Native input for a target in the current, mounted Reader body. The
    /// Reader's target list remains the single source of walk order/actions.
    pub(crate) fn native_handle(&self, id: &SharedString, cx: &mut Context<Reader>) -> Option<FocusHandle> {
        self.active.then(|| self.targets.native_handle(id, cx))
    }

    /// Records a string the body puts on screen.
    /// The one line a page says when its route reads no declaration.
    pub(crate) fn unread(&mut self, unread: &crate::runtime::store::Unread) -> Vec<Leaf> {
        let words = match unread {
            crate::runtime::store::Unread::NotADeclaration => "This page's address is not a declaration.".to_owned(),
            crate::runtime::store::Unread::CoordinateOutsidePackage => "This declaration does not belong to the package in this address.".to_owned(),
            crate::runtime::store::Unread::ReleaseNotHere(at) => format!(
                "Release {} is not in this index; only your working copy is. Esc returns to it.",
                at.as_str()
            ),
        };
        let words = self.say(words);
        vec![Leaf::new(super::kit::quiet(words, &self.measure, self.palette))]
    }

    pub(crate) fn say(&mut self, text: impl Into<SharedString>) -> SharedString {
        let text = text.into();
        self.said.push(text.clone());
        text
    }
}

/// The lenses of a declaration page.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub(crate) enum Lens {
    /// Signature, docs, members.
    #[default]
    Reference,
    /// Everything it touches.
    Relations,
    /// Where it is used.
    Usage,
    /// When it changed.
    History,
}

impl Lens {
    pub(crate) const ALL: [Self; 4] = [Self::Reference, Self::Relations, Self::Usage, Self::History];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Reference => "Reference",
            Self::Relations => "Relations",
            Self::Usage => "Usage",
            Self::History => "History",
        }
    }
}

/// The resources one body reads, taken from the store before building so
/// the body can hold the reader's context mutably. Cloning a resource
/// shares its value.
#[derive(Clone, Default)]
pub(crate) struct Pages {
    /// An explicit terminal destination bypasses retained values in its body.
    terminal_destination: Option<(PageKey, crate::runtime::store::ContentFailure)>,
    dependencies: Option<RouteDependencies>,
    symbols: BTreeMap<SymbolRef, Resource<SymbolPage>>,
    sources: BTreeMap<SymbolRef, Resource<SourceView>>,
    cargo_sources: BTreeMap<CargoSourceKey, Resource<CargoSourcePage>>,
    packages: BTreeMap<PackageRef, Resource<PackageDossier>>,
    orbit: Option<Resource<OrbitModel>>,
    health: Option<Resource<HealthModel>>,
    browse: BTreeMap<crate::model::browse::BrowseKey, Resource<crate::model::browse::BrowseValue>>,
}

impl Pages {
    /// Takes the visible route's complete dependency plan from the store.
    pub(crate) fn gather(store: &DataStore, dependencies: &RouteDependencies) -> Self {
        let mut pages = Self { dependencies: Some(dependencies.clone()), ..Self::default() };
        if let crate::runtime::store::ContentAdmission::Terminal { key, failure } = dependencies.content_admission(store) {
            pages.terminal_destination = Some((key, failure));
        }
        for key in dependencies.keys() {
            match key {
                PageKey::Symbol(symbol) => {
                    pages.symbols.insert(symbol.clone(), store.symbol(symbol));
                }
                PageKey::Source(symbol) => {
                    pages.sources.insert(symbol.clone(), store.source(symbol));
                }
                PageKey::CargoSource(file) => {
                    pages.cargo_sources.insert(file.clone(), store.cargo_source(file));
                }
                PageKey::Package(package) => {
                    pages.packages.insert(package.clone(), store.package(package));
                }
                PageKey::Orbit => pages.orbit = Some(store.orbit()),
                PageKey::Health => pages.health = Some(store.health()),
                PageKey::Browse(key) => {
                    pages.browse.insert(key.clone(), store.pages().browse(key));
                }
                PageKey::Search(_) => {}
            }
        }
        pages
    }

    /// The exact required-content refusal chosen by this captured plan.
    pub(crate) fn content_failure(&self) -> Option<&(PageKey, crate::runtime::store::ContentFailure)> {
        self.terminal_destination.as_ref()
    }

    /// Address dependencies may be retained by a transition; current Cargo
    /// bytes and their admission still come from the live owner store.
    fn dependencies(&self) -> Option<&RouteDependencies> {
        self.dependencies.as_ref()
    }

    pub(crate) fn symbol(&self, symbol: &SymbolRef) -> Resource<SymbolPage> {
        self.symbols.get(symbol).cloned().unwrap_or_else(Resource::not_yet)
    }

    pub(crate) fn source(&self, symbol: &SymbolRef) -> Resource<SourceView> {
        self.sources.get(symbol).cloned().unwrap_or_else(Resource::not_yet)
    }

    pub(crate) fn cargo_source(&self, file: &CargoSourceKey) -> Resource<CargoSourcePage> {
        self.cargo_sources.get(file).cloned().unwrap_or_else(Resource::not_yet)
    }

    pub(crate) fn package(&self, package: &PackageRef) -> Resource<PackageDossier> {
        self.packages.get(package).cloned().unwrap_or_else(Resource::not_yet)
    }

    pub(crate) fn orbit(&self) -> Resource<OrbitModel> {
        self.orbit.clone().unwrap_or_else(Resource::not_yet)
    }

    pub(crate) fn health(&self) -> Resource<HealthModel> {
        self.health.clone().unwrap_or_else(Resource::not_yet)
    }

    pub(crate) fn browse(&self, key: &crate::model::browse::BrowseKey) -> Resource<crate::model::browse::BrowseValue> {
        self.browse.get(key).cloned().unwrap_or_else(Resource::not_yet)
    }
}

/// Builds the body for one place (the current one, or one still leaving).
pub(crate) fn build(
    route: &Route,
    overlay: Option<Overlay>,
    snapshot: &AppSnapshot,
    store: &Pages,
    ctx: &mut Ctx<'_>,
    hover: &mut HoverIntent,
    window: &mut Window,
    cx: &mut Context<Reader>,
) -> Vec<Leaf> {
    // Local transient content is independent of the underlying page's read.
    match overlay {
        Some(Overlay::Settings(page)) => return settings::body(page, snapshot, store, ctx, window, cx),
        Some(Overlay::Inbox) => return inbox::body(ctx),
        Some(Overlay::AddProject | Overlay::CommandPalette) | None => {}
    }
    // A terminal sibling belongs to its own pane. Replacing the whole body
    // here would hide an independent current Source, dossier, or local shell.
    let mut leaves = match route {
        Route::Orbit(crate::navigation::OrbitRoute::Browse(browse)) => browse::body(browse, store, ctx, cx),
        Route::Orbit(_) => orbit::body(snapshot, store, ctx, hover, cx),
        Route::Package(_) => package::body(route, snapshot, store, ctx, hover, cx),
        Route::CargoSource(file) => source::cargo_body(file, store, ctx, window, cx),
        Route::Symbol(symbol) => match symbol.view {
            View::Page => symbol::body(route, symbol, store, ctx, hover, cx),
            View::Code => source::body(route, symbol, store, ctx, window, cx),
            View::Graph => graph::body(route, store, ctx),
        },
        Route::World => graph::body(route, store, ctx),
    };
    if matches!(route, Route::Package(package) if package.cargo.is_some())
        && let Some((PageKey::Browse(crate::model::browse::BrowseKey::Tree(_)), failure)) = store.content_failure()
    {
        let reason = match failure {
            crate::runtime::store::ContentFailure::Fault(error) => error.message().to_owned(),
            crate::runtime::store::ContentFailure::Unavailable(crate::core::UnavailableReason::Unsupported) => "not served by the current owner".to_owned(),
            crate::runtime::store::ContentFailure::Unavailable(crate::core::UnavailableReason::OutOfScope) => "outside the selected owner scope".to_owned(),
        };
        let words = ctx.say(format!("The selected Cargo Tree could not be checked: {reason}. Current semantic facts, if present, remain separate from Cargo source links."));
        leaves.push(Leaf::new(super::kit::quiet(words, &ctx.measure, ctx.palette)));
    }
    retained::append(route, snapshot, ctx, cx, &mut leaves);
    leaves
}

/// A World route with no readable Orbit value can show its exact saved words
/// in the Reader rather than an empty graph surface.
pub(crate) fn saved_world(store: &DataStore, route: &Route, overlay: Option<Overlay>) -> bool {
    matches!(route, Route::World) && retained::select(store, route, overlay).is_some()
}

pub(crate) fn saved_source_generation(store: &DataStore, route: &Route, overlay: Option<Overlay>) -> Option<[u8; 32]> {
    retained::source_generation(store, route, overlay)
}
