//! Pages a page draws from besides its own: a Go named type's values are
//! the constants declared after it (`ContinueOnError ErrorHandling = iota`),
//! each its own declaration with its own page. The page asks the store for
//! them and re-renders when one lands.
//!
//! The reader watches only its route's keys (`Reader::keys`), so a companion
//! landing would not repaint it. Until the reader lets a body declare extra
//! keys (a request in W-Page2's CP1), one store subscription per reader,
//! replaced whenever the page's companions change, notifies it: bounded,
//! and silent when nothing lands.

use crate::model::pages::{DeclRef, PageKey, SymbolPage, SymbolRef};
use crate::runtime::store::StoreEvent;
use crate::shell::reader::Reader;
use crate::shell::region::Links;
use backend_library::DeclarationKind;
use gpui::{Context, EntityId, Global, Subscription};
use std::collections::HashMap;

/// The companions of `page`: for a Go named type (`ErrorHandling int`),
/// the constants that follow it in its file, in order.
pub(super) fn of(page: &SymbolPage) -> Vec<DeclRef> {
    if page.identity.language.name() != "go" || page.identity.kind != Some(DeclarationKind::Type) {
        return Vec::new();
    }
    let Some(outline) = page.outline.known() else { return Vec::new() };
    let Some(at) = outline.index.or_else(|| outline.siblings.iter().position(|sibling| sibling.coordinate == page.identity.coordinate)) else {
        return Vec::new();
    };
    outline
        .siblings
        .iter()
        .skip(at + 1)
        .take_while(|sibling| sibling.kind == Some(DeclarationKind::Constant) && sibling.path == page.identity.path)
        .take(64)
        .cloned()
        .collect()
}

#[derive(Default)]
struct Watching {
    readers: HashMap<EntityId, (Vec<PageKey>, Subscription)>,
}

impl Global for Watching {}

/// Asks the store for `companions` and keeps the reader repainting as each
/// lands. Returns their pages as the store has them now, in order (`None`
/// for one still on its way).
pub(super) fn gather(companions: &[DeclRef], links: &Links, active: bool, cx: &mut Context<Reader>) -> Vec<(DeclRef, Option<SymbolPage>)> {
    let keys = companions.iter().map(|decl| PageKey::Symbol(decl.coordinate.clone())).collect::<Vec<_>>();
    if active {
        watch(&keys, links, cx);
    }
    let store = links.store.read(cx);
    companions
        .iter()
        .map(|decl| (decl.clone(), store.symbol(&decl.coordinate).loaded_value().cloned()))
        .collect()
}

fn watch(keys: &[PageKey], links: &Links, cx: &mut Context<Reader>) {
    let reader = cx.entity_id();
    let same = cx.try_global::<Watching>().and_then(|watching| watching.readers.get(&reader)).is_some_and(|(watched, _)| watched == keys);
    if same {
        return;
    }
    if keys.is_empty() {
        if let Some(watching) = cx.try_global::<Watching>().is_some().then(|| cx.global_mut::<Watching>()) {
            watching.readers.remove(&reader);
        }
        return;
    }
    let wanted = keys.to_vec();
    let subscription = cx.subscribe(&links.store, move |_, _, event: &StoreEvent, cx| {
        if wanted.iter().any(|key| event.touches(key)) {
            cx.notify();
        }
    });
    cx.default_global::<Watching>().readers.insert(reader, (keys.to_vec(), subscription));
    let keys = keys.to_vec();
    links.store.update(cx, |store, cx| {
        for key in keys {
            store.ensure(key, cx);
        }
    });
}

/// The companion's coordinate as the plan's link.
pub(super) fn link(symbol: &SymbolRef) -> String {
    symbol.as_str().to_owned()
}
