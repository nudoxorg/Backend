//! PLACEHOLDER float layer, until `facet::overlay::float` (W-Float) is
//! exported: one peek at a time, opened by the keyboard (Space) at the
//! focused target, closed by Esc, pinned by Space again.
//!
//! It keeps W-Float's contract so the swap is mechanical: `open` takes a key
//! and an anchor in window coordinates, `step_back` answers whether it
//! closed something, `pin_top` whether it pinned, and the layer renders as
//! the root's last child. The card shows the peek at rest from the calm
//! Peeks board: mark, name, where, one sentence, your uses.

use super::kit::{gap_words, kind_of, text};
use crate::model::pages::{DocFragment, PageKey, SymbolRef};
use crate::runtime::store::DataStore;
use facet::paint::{Bevel, Chamfer, cut};
use facet::tokens::ty;
use facet::{ActiveFacet as _, Measure, Space};
use gpui::{AnyElement, App, Bounds, IntoElement, ParentElement, Pixels, SharedString, Styled, div, px};

/// One open peek.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Peek {
    /// The page it shows.
    pub key: PageKey,
    /// Where it opened from, window coordinates.
    pub anchor: Bounds<Pixels>,
    /// The trigger's words, shown while the page is on its way.
    pub label: SharedString,
}

/// One pinned peek.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Pin {
    /// The declaration.
    pub symbol: SymbolRef,
    /// Its name.
    pub name: SharedString,
    /// Where it lives.
    pub place: SharedString,
}

/// The layer's state.
#[derive(Default)]
pub(crate) struct Floats {
    open: Option<Peek>,
    pins: Vec<Pin>,
}

impl Floats {
    /// Opens `peek` (keyboard: no delay), replacing an open one.
    pub(crate) fn open(&mut self, peek: Peek) {
        self.open = Some(peek);
    }

    /// Closes the topmost float; `false` when nothing was open.
    pub(crate) fn step_back(&mut self) -> bool {
        self.open.take().is_some()
    }

    /// Pins the open peek; `false` when there is none (or it is pinned).
    pub(crate) fn pin_top(&mut self, store: &DataStore) -> bool {
        let Some(PageKey::Symbol(symbol)) = self.open.as_ref().map(|peek| peek.key.clone()) else {
            return false;
        };
        if self.pins.iter().any(|pin| pin.symbol == symbol) {
            return false;
        }
        let identity = symbol.identity();
        let place = identity.project().map_or_else(String::new, |project| project.name().to_owned());
        let name = store
            .symbol(&symbol)
            .loaded_value()
            .map_or_else(|| identity.name().to_owned(), |page| page.identity.name.to_string());
        self.pins.push(Pin {
            symbol,
            name: name.into(),
            place: place.into(),
        });
        self.open = None;
        true
    }

    /// The open peek.
    pub(crate) const fn top(&self) -> Option<&Peek> {
        self.open.as_ref()
    }

    /// Everything pinned, oldest first.
    pub(crate) fn pins(&self) -> &[Pin] {
        &self.pins
    }

    /// Unpins one.
    pub(crate) fn unpin(&mut self, symbol: &SymbolRef) {
        self.pins.retain(|pin| &pin.symbol != symbol);
    }

    /// The layer: the open peek placed beside its anchor, inside the window.
    pub(crate) fn layer(&self, viewport: gpui::Size<Pixels>, store: &DataStore, cx: &App) -> Option<AnyElement> {
        let peek = self.open.as_ref()?;
        let facet = cx.facet();
        let palette = facet.palette();
        let width = px(392.0 * facet.text_scale).min(viewport.width - px(16.0));
        let measure = Measure::new(width, &facet);
        let mut card = cut()
            .chamfer(Chamfer::Float)
            .bevel(Bevel::Rest)
            .fill(palette.glass)
            .floating()
            .w(width)
            .p(measure.space(Space::Gutter))
            .flex()
            .flex_col()
            .gap(measure.space(Space::Base));
        match &peek.key {
            PageKey::Symbol(symbol) => {
                let resource = store.symbol(symbol);
                match resource.loaded_value() {
                    Some(page) => {
                        card = card
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(measure.space(Space::Base))
                                    .child(facet::icons::kind_mark(kind_of(page.identity.kind), facet::icons::KindSize::Md, palette))
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .child(text(ty::MONO_ROW, &measure, palette.ink0).child(page.identity.name.to_string()))
                                            .child(text(ty::SMALL, &measure, palette.ink3).child(format!(
                                                "{} in {}",
                                                page.identity.kind_name(),
                                                symbol.identity().project().map_or("", |project| project.name())
                                            ))),
                                    ),
                            );
                        if let Some(signature) = page.signature.known() {
                            card = card.child(
                                div()
                                    .bg(palette.table)
                                    .px(measure.space(Space::Base))
                                    .py(measure.space(Space::Snug))
                                    .child(
                                        text(ty::MONO_SMALL, &measure, palette.ink1)
                                            .overflow_hidden()
                                            .whitespace_nowrap()
                                            .text_ellipsis()
                                            .child(signature.text.to_string()),
                                    ),
                            );
                        }
                        let prose = DocFragment::plain_text(&page.docs);
                        if let Some(sentence) = prose.split_terminator(['.', '\n']).map(str::trim).find(|line| !line.is_empty()) {
                            card = card.child(text(ty::MARGIN, &measure, palette.ink1).child(format!("{sentence}.")));
                        }
                        match page.references.known() {
                            Some(sites) => {
                                card = card.child(
                                    div()
                                        .flex()
                                        .gap(px(4.0))
                                        .child(text(ty::SMALL, &measure, palette.mint.base).child(sites.len().to_string()))
                                        .child(text(ty::SMALL, &measure, palette.ink3).child("uses in your code")),
                                );
                            }
                            None => {
                                if let Some(gap) = page.references.gap() {
                                    card = card.child(text(ty::SMALL, &measure, palette.ink3).child(gap_words(gap)));
                                }
                            }
                        }
                    }
                    None => {
                        card = card
                            .child(text(ty::MONO_ROW, &measure, palette.ink0).child(peek.label.clone()))
                            .child(super::kit::pending(px(240.0 * facet.text_scale), ty::SMALL, &measure, palette));
                    }
                }
            }
            _ => {
                card = card.child(text(ty::MONO_ROW, &measure, palette.ink0).child(peek.label.clone()));
            }
        }
        // Below the anchor when it fits, above otherwise; shifted inside.
        let gap = px(6.0);
        let below = peek.anchor.origin.y + peek.anchor.size.height + gap;
        let top = if below + px(220.0) < viewport.height { below } else { (peek.anchor.origin.y - px(220.0) - gap).max(px(8.0)) };
        let left = peek.anchor.origin.x.min(viewport.width - width - px(8.0)).max(px(8.0));
        Some(div().absolute().top(top).left(left).child(card).into_any_element())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, size};

    #[test]
    fn step_back_closes_only_what_is_open() {
        let mut floats = Floats::default();
        assert!(!floats.step_back(), "nothing to close");
        floats.open(Peek {
            key: PageKey::Health,
            anchor: Bounds::new(point(px(0.0), px(0.0)), size(px(10.0), px(10.0))),
            label: "x".into(),
        });
        assert!(floats.top().is_some());
        assert!(floats.step_back());
        assert!(floats.top().is_none());
        assert!(!floats.step_back());
    }
}
