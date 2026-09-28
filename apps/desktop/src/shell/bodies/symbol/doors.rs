//! The shell's side of the page's doors: a name on the page lights with its
//! subject (the one hover grammar, `facet::hover`), unfurls its page's peek
//! from the float layer, opens its page on a click, and stands in the
//! reader's keyboard walk. Folds keep their state in the reader's per-symbol
//! disclosure, so an unrolled "and N more" survives a repaint.

use crate::model::pages::{PageKey, SymbolRef};
use crate::navigation::Intent;
use crate::shell::focus::{Act, Target, Targets};
use crate::shell::kit::symbol_route;
use crate::shell::reader::{Reader, SymbolDisclosure, SymbolFold};
use crate::shell::region::Links;
use facet::anatomy::page::{Door, Doors, Fold};
use facet::hover::Subject;
use gpui::{AnyElement, SharedString, WeakEntity};
use std::cell::RefCell;
use std::rc::Rc;

/// Doors for one symbol page.
pub(super) struct ShellDoors<'a> {
    pub package: String,
    pub symbol: SymbolRef,
    pub links: Links,
    pub targets: &'a Targets,
    pub active: bool,
    pub disclosure: SymbolDisclosure,
    pub reader: WeakEntity<Reader>,
    pub said: RefCell<Vec<SharedString>>,
}

impl ShellDoors<'_> {
    /// What the page said, in order.
    pub(super) fn take_said(&self) -> Vec<SharedString> {
        std::mem::take(&mut *self.said.borrow_mut())
    }
}

impl Doors for ShellDoors<'_> {
    fn door(&self, link: &str) -> Option<Door> {
        let target = SymbolRef::new(link).ok()?;
        let route = symbol_route(&self.package, &target)?;
        let store = self.links.store.clone();
        let label = SharedString::from(target.identity().name().to_owned());
        let key = PageKey::Symbol(target);
        let links = self.links.clone();
        let peek_key = key.clone();
        Some(Door {
            subject: Subject::new(link.to_owned()),
            peek: Some(Rc::new(move |bounds| crate::shell::peeks::request(peek_key.clone(), label.clone(), bounds, store.clone()))),
            open: Some(Rc::new(move |_, cx| links.dispatch(Intent::Navigate(route.clone()), cx))),
        })
    }

    fn fold(&self, key: &'static str) -> Option<Fold> {
        let fold = SymbolFold::Relations(key);
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

    fn track(&self, key: SharedString, label: SharedString, door: Option<&Door>, element: AnyElement) -> AnyElement {
        if self.active {
            let act: Act = door.and_then(|door| door.open.clone()).unwrap_or_else(|| Rc::new(|_, _| {}));
            let target = door.and_then(|door| SymbolRef::new(door.subject.0.as_ref()).ok());
            self.targets.push(Target { id: key.clone(), label, act, peek: target.clone().map(PageKey::Symbol), source: target });
        }
        gpui::IntoElement::into_any_element(self.targets.track(key, element))
    }

    fn say(&self, text: &str) {
        self.said.borrow_mut().push(SharedString::from(text.to_owned()));
    }
}
