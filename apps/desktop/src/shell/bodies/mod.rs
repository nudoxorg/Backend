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
mod settings;
mod source;
pub(crate) use source::paging::PagingState;
mod state;
mod symbol;

use gpui::Window;

/// How many declaration pages are still reading their lines (the harness
/// waits for none before a capture).


use super::focus::Targets;
use super::kit::HoverIntent;
use super::reader::Reader;
use super::region::Links;
use crate::model::AppSnapshot;
use crate::navigation::{Overlay, Route, View};
use crate::core::Resource;
use crate::model::pages::{
    CargoSourceKey, CargoSourcePage, HealthModel, OrbitModel, PackageDossier, PackageRef, PageKey, SourceView, SymbolPage, SymbolRef,
};
use crate::runtime::store::DataStore;
use std::collections::BTreeMap;
use facet::{Measure, Palette, Reveal};
use gpui::{AnyElement, Context, SharedString};

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

impl Ctx<'_> {
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
#[derive(Default)]
pub(crate) struct Pages {
    symbols: BTreeMap<SymbolRef, Resource<SymbolPage>>,
    sources: BTreeMap<SymbolRef, Resource<SourceView>>,
    cargo_sources: BTreeMap<CargoSourceKey, Resource<CargoSourcePage>>,
    packages: BTreeMap<PackageRef, Resource<PackageDossier>>,
    orbit: Option<Resource<OrbitModel>>,
    health: Option<Resource<HealthModel>>,
    browse: BTreeMap<crate::model::browse::BrowseKey, Resource<crate::model::browse::BrowseValue>>,
}

impl Pages {
    /// Takes `keys` from the store.
    pub(crate) fn gather(store: &DataStore, keys: &[PageKey]) -> Self {
        let mut pages = Self::default();
        for key in keys {
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
    match overlay {
        Some(Overlay::Settings(page)) => return settings::body(page, snapshot, store, ctx, window, cx),
        Some(Overlay::Inbox) => return inbox::body(ctx),
        Some(Overlay::AddProject | Overlay::CommandPalette) | None => {}
    }
    match route {
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
    }
}
