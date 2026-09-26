//! Peeks through W-Float's float layer: the shell's read models become
//! `facet::overlay::peek` cards. A card's content reads the store every
//! frame, so a peek opened before its page lands fills in when it does.

use super::kit::kind_of;
use crate::model::pages::{DocFragment, PageKey, SignatureText, SymbolRef, TokenClass};
use crate::runtime::store::DataStore;
use facet::overlay::peek::{Peek, SymbolPeek};
use facet::overlay::text::{Role, Sig};
use facet::overlay::{FloatKind, FloatRequest};
use gpui::{App, Bounds, ElementId, Entity, Pixels, SharedString, Window};

/// The float key a peek of `key` opens under.
pub(crate) fn float_key(key: &PageKey) -> ElementId {
    ElementId::Name(SharedString::from(format!("peek:{key}")))
}

/// A float request for the page `key`, anchored at `anchor`, drawn from
/// whatever the store holds for it at each frame.
pub(crate) fn request(key: PageKey, label: SharedString, anchor: Bounds<Pixels>, store: Entity<DataStore>) -> FloatRequest {
    let float = float_key(&key);
    FloatRequest::new(float, anchor, FloatKind::Peek, move |measure, window, cx| {
        let peek = peek_of(&key, &label, store.read(cx));
        facet::overlay::peek::content(peek)(measure, window, cx)
    })
}

/// The card for `key` as the store has it now.
fn peek_of(key: &PageKey, label: &SharedString, store: &DataStore) -> Peek {
    let PageKey::Symbol(symbol) = key else {
        return Peek::Symbol(SymbolPeek {
            name: label.clone(),
            ..SymbolPeek::default()
        });
    };
    let page = store.symbol(symbol);
    let Some(page) = page.loaded_value() else {
        return Peek::Symbol(SymbolPeek {
            name: symbol.identity().name().to_owned().into(),
            place: where_of(symbol),
            path: symbol.identity().to_string().into(),
            ..SymbolPeek::default()
        });
    };
    let sentence = DocFragment::plain_text(&page.docs)
        .split_terminator(['.', '\n'])
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| SharedString::from(format!("{line}.")));
    Peek::Symbol(SymbolPeek {
        kind: Some(kind_of(page.identity.kind)),
        name: page.identity.name.to_string().into(),
        place: format!("{} in `{}`", page.identity.kind_name(), where_of(symbol)).into(),
        path: symbol.identity().to_string().into(),
        signature: page.signature.known().map(sig),
        sentence,
        uses: page.references.known().map(|sites| sites.len()),
        ..SymbolPeek::default()
    })
}

fn where_of(symbol: &SymbolRef) -> SharedString {
    let identity = symbol.identity();
    let mut parts = Vec::new();
    if let Some(project) = identity.project() {
        parts.push(project.name().to_owned());
    }
    if let Some(path) = identity.path() {
        let stem = path.stem();
        if !stem.is_empty() && !matches!(stem, "lib" | "mod" | "main") {
            parts.push(stem.to_owned());
        }
    }
    parts.join("::").into()
}

/// A classified signature as the peek's one-line run.
fn sig(signature: &SignatureText) -> Sig {
    let mut out = Sig::new();
    for token in signature.tokens.iter() {
        let text = signature.token_text(token).to_owned();
        let role = match token.class {
            TokenClass::Keyword => Role::Keyword,
            TokenClass::Name | TokenClass::Type => Role::Type,
            TokenClass::Binding => Role::Param,
            TokenClass::Lifetime => Role::Macro,
            TokenClass::Literal => Role::Value,
            TokenClass::Punctuation | TokenClass::Text => Role::Punct,
        };
        out = out.span(text, role);
    }
    if signature.tokens.is_empty() {
        out = out.span(signature.text.to_string(), Role::Punct);
    }
    out
}

/// Whether the float layer shows a card for `key`.
pub(crate) fn is_open(key: &PageKey, window: &Window, cx: &mut App) -> bool {
    facet::overlay::float::is_open(&float_key(key), window, cx)
}
