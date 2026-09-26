//! Plain page bodies: each route rendered from its read model — names,
//! kinds, signatures, docs, members, counts — enough to prove the data flow
//! and to assert rendered content. The designed pages are wave 3; these
//! follow the calm targets' structure (hero, one facts line, sections of
//! mark + name rows) without the page-specific marks other lanes own.
//!
//! A body is a list of [`Leaf`]s: a main block and an optional margin note.
//! The reader sets notes beside their block when its room is wide and folds
//! each under its block otherwise.

pub(crate) mod graph;
mod inbox;
mod orbit;
mod package;
mod settings;
mod source;
mod state;
mod symbol;


use super::focus::Targets;
use super::kit::HoverIntent;
use super::reader::Reader;
use super::region::Links;
use crate::model::AppSnapshot;
use crate::navigation::{Overlay, Route, View};
use crate::core::Resource;
use crate::model::pages::{
    HealthModel, OrbitModel, PackageDossier, PackageRef, PageKey, SourceView, SymbolPage, SymbolRef,
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
}

impl Leaf {
    pub(crate) fn new(main: impl gpui::IntoElement) -> Self {
        Self {
            main: main.into_any_element(),
            note: None,
        }
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
    /// The active palette.
    pub palette: &'static Palette,
    /// Held reveal modes.
    pub reveal: Reveal,
    /// Intents, data, the shell.
    pub links: &'a Links,
    /// Keyboard targets of the reader.
    pub targets: &'a Targets,
    /// The page's lens (tab) for declaration pages.
    pub lens: Lens,
    /// The text each body renders, recorded for content assertions.
    pub said: &'a mut Vec<SharedString>,
    /// The hero name's lines, as fitted (the name never ellipsizes).
    pub hero: &'a mut Vec<SharedString>,
}

impl Ctx<'_> {
    /// Records a string the body puts on screen.
    /// The one line a page says when its route reads no declaration.
    pub(crate) fn unread(&mut self, unread: &crate::runtime::store::Unread) -> Vec<Leaf> {
        let words = match unread {
            crate::runtime::store::Unread::NotADeclaration => "This page's address is not a declaration.".to_owned(),
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
    packages: BTreeMap<PackageRef, Resource<PackageDossier>>,
    orbit: Option<Resource<OrbitModel>>,
    health: Option<Resource<HealthModel>>,
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
                PageKey::Package(package) => {
                    pages.packages.insert(package.clone(), store.package(package));
                }
                PageKey::Orbit => pages.orbit = Some(store.orbit()),
                PageKey::Health => pages.health = Some(store.health()),
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

    pub(crate) fn package(&self, package: &PackageRef) -> Resource<PackageDossier> {
        self.packages.get(package).cloned().unwrap_or_else(Resource::not_yet)
    }

    pub(crate) fn orbit(&self) -> Resource<OrbitModel> {
        self.orbit.clone().unwrap_or_else(Resource::not_yet)
    }

    pub(crate) fn health(&self) -> Resource<HealthModel> {
        self.health.clone().unwrap_or_else(Resource::not_yet)
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
    cx: &mut Context<Reader>,
) -> Vec<Leaf> {
    match overlay {
        Some(Overlay::Settings(page)) => return settings::body(page, snapshot, store, ctx, cx),
        Some(Overlay::Inbox) => return inbox::body(ctx),
        Some(Overlay::AddProject | Overlay::CommandPalette) | None => {}
    }
    match route {
        Route::Orbit(_) => orbit::body(snapshot, store, ctx, hover, cx),
        Route::Package(_) => package::body(route, store, ctx, hover, cx),
        Route::Symbol(symbol) => match symbol.view {
            View::Page => symbol::body(route, symbol, store, ctx, hover, cx),
            View::Code => source::body(route, symbol, store, ctx, cx),
            View::Graph => graph::body(route, store, ctx),
        },
        Route::World => graph::body(route, store, ctx),
    }
}
