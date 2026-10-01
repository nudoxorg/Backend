//! The shell's side of the page: a name on the page lights with its subject
//! (the one hover grammar, `facet::hover`), unfurls its page's peek from the
//! float layer, opens its page on a click, and stands in the reader's
//! keyboard walk. The page's filters and folds live in the reader's
//! per-symbol disclosure, so they survive a repaint.

use crate::model::pages::{PageKey, SymbolRef};
use crate::navigation::Intent;
use crate::shell::focus::{Act as Action, Target, Targets};
use crate::shell::kit::symbol_route;
use crate::shell::reader::{Reader, SymbolDisclosure};
use crate::shell::region::Links;
use facet::anatomy::page::{Door, Doors, Fold};
use facet::anatomy::symbol::key::{FoldKey, Key, Sec};
use facet::anatomy::symbol::{Act, Change, Host, Spots, Ui};
use facet::hover::Subject;
use gpui::{AnyElement, ScrollHandle, SharedString, WeakEntity};
use std::cell::RefCell;
use std::rc::Rc;

/// The host of one symbol page.
pub(super) struct ShellHost<'a> {
    pub package: String,
    pub symbol: SymbolRef,
    pub links: Links,
    pub targets: &'a Targets,
    pub active: bool,
    /// The declaration this page was reached from (a hop forward), ringed.
    pub from: Option<SymbolRef>,
    pub disclosure: SymbolDisclosure,
    pub reader: WeakEntity<Reader>,
    pub scroll: ScrollHandle,
    pub said: RefCell<Vec<SharedString>>,
}

impl ShellHost<'_> {
    /// What the page said, in order.
    pub(super) fn take_said(&self) -> Vec<SharedString> {
        std::mem::take(&mut *self.said.borrow_mut())
    }
}

impl Doors for ShellHost<'_> {
    fn door(&self, link: &str) -> Option<Door> {
        let target = SymbolRef::new(link).ok()?;
        // A door to the page you are on is no door (and must not carry this
        // page's title key a second time).
        if target == self.symbol {
            return None;
        }
        let route = symbol_route(&self.package, &target)?;
        let from = self.from.as_ref() == Some(&target);
        let store = self.links.store.clone();
        let label = SharedString::from(target.identity().name().to_owned());
        let key = PageKey::Symbol(target);
        let links = self.links.clone();
        let peek_key = key.clone();
        Some(Door {
            subject: Subject::new(link.to_owned()),
            peek: Some(Rc::new(move |bounds| crate::shell::peeks::request(peek_key.clone(), label.clone(), bounds, store.clone()))),
            open: Some(Rc::new(move |_, cx| links.dispatch(Intent::Navigate(route.clone()), cx))),
            from,
        })
    }

    fn fold(&self, _: &'static str) -> Option<Fold> {
        None
    }

    fn track(&self, key: SharedString, label: SharedString, door: Option<&Door>, element: AnyElement) -> AnyElement {
        if self.active {
            let act: Action = door.and_then(|door| door.open.clone()).unwrap_or_else(|| Rc::new(|_, _| {}));
            let target = door.and_then(|door| SymbolRef::new(door.subject.0.as_ref()).ok());
            self.targets.push(Target { id: key.clone(), label, act, peek: target.clone().map(PageKey::Symbol), source: target });
        }
        gpui::IntoElement::into_any_element(self.targets.track(key, element))
    }

    fn up(&self) -> Option<Rc<dyn Fn(&mut gpui::Window, &mut gpui::App)>> {
        let links = self.links.clone();
        Some(Rc::new(move |_, cx| links.dispatch(Intent::ZoomOut, cx)))
    }

    fn mark(&self, link: &str) -> Option<gpui::ElementId> {
        SymbolRef::new(link).ok().map(|symbol| crate::shell::kit::shared_id(&symbol))
    }

    fn say(&self, text: &str) {
        self.said.borrow_mut().push(SharedString::from(text.to_owned()));
    }
}

impl Host for ShellHost<'_> {
    fn ui(&self) -> Ui {
        self.disclosure.ui.clone()
    }

    fn change(&self, change: Change) -> Act {
        let (reader, symbol) = (self.reader.clone(), self.symbol.clone());
        Rc::new(move |_, cx| {
            let _ = reader.update(cx, |reader, cx| reader.change_symbol(symbol.clone(), &change, cx));
        })
    }

    fn open_source(&self, path: &str, line: u32) -> Act {
        let links = self.links.clone();
        let (path, line): (std::sync::Arc<str>, u32) = (std::sync::Arc::from(path), line);
        Rc::new(move |_, cx| links.dispatch(Intent::OpenSource { path: std::sync::Arc::clone(&path), line }, cx))
    }

    fn unfold(&self, key: &FoldKey) -> Option<Fold> {
        let fold = key.clone();
        let (reader, symbol) = (self.reader.clone(), self.symbol.clone());
        let toggle_fold = fold.clone();
        Some(Fold {
            open: self.disclosure.is_open(&fold),
            presence: self.disclosure.unroll(fold),
            toggle: Rc::new(move |_, cx| {
                let _ = reader.update(cx, |reader, cx| reader.toggle_symbol(symbol.clone(), toggle_fold.clone(), cx));
            }),
        })
    }

    fn lookup(&self, target: &str) -> Option<Act> {
        let links = self.links.clone();
        let package = self.package.clone();
        // A link to a declaration goes to its page; a path goes to Find.
        if let Ok(symbol) = SymbolRef::new(target)
            && symbol.identity().path().is_some()
            && let Some(route) = symbol_route(&package, &symbol)
        {
            return Some(Rc::new(move |_, cx| links.dispatch(Intent::Navigate(route.clone()), cx)));
        }
        let query = crate::model::pages::SearchQuery::new(target, 50).ok()?;
        Some(Rc::new(move |_, cx| {
            links.dispatch(Intent::Navigate(crate::navigation::Route::Orbit(crate::navigation::OrbitRoute::Browse(crate::navigation::BrowseRoute::Find(query.clone())))), cx);
        }))
    }

    fn target(&self, key: &Key, label: SharedString, act: Act, element: AnyElement) -> AnyElement {
        let id = key.text();
        if self.active {
            self.targets.push(Target { id: id.clone(), label, act, peek: None, source: None });
        }
        gpui::IntoElement::into_any_element(self.targets.track(id, element))
    }

    fn reveal(&self, section: Sec) -> Act {
        let (spots, scroll) = (self.spots(), self.scroll.clone());
        Rc::new(move |_, _| {
            if let Some(bounds) = spots.get(section) {
                let offset = scroll.offset();
                let delta = bounds.top() - scroll.bounds().top() - gpui::px(20.0);
                scroll.set_offset(gpui::point(offset.x, offset.y - delta));
            }
        })
    }

    fn spots(&self) -> Rc<Spots> {
        Rc::clone(&self.disclosure.spots)
    }

    fn flow(&self) -> facet::motion::Flow {
        // A page drawn away from the scroller (leaving, folding, under a
        // plate) is inert: it stands where it was. A fresh flow places each
        // part where it lays out, so a still copy neither glides (a moving
        // part is drawn above the page, out of the fold's reach) nor moves
        // the live page's springs.
        if self.active { self.disclosure.flow.clone() } else { facet::motion::Flow::new("s6-inert") }
    }
}
