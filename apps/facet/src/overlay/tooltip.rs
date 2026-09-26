//! The tooltip: a chamfered `plate3` chip on the float layer
//! ([`FloatKind::Tip`]), not a mechanism of its own. One tip per window, a
//! 450 ms rest, a warm sweep between tipped things (the second tip swaps in
//! at once and the chip glides over), a quick rise and fade.
//!
//! ```ignore
//! div().id("pin").child(icon).tip("Pin beside (Space)")
//! div().id("gem").child(gem).tip_rich("Enum", "A closed set of variants.", &["⌥"])
//! ```
//!
//! The extension wraps the element in a tracked [`float::trigger`], which
//! reports rest and leave with the element's own bounds, re-anchors the tip
//! if the element moves and closes it if the element goes away.

use super::float::{self, FloatKind, FloatRequest, Side, Trigger};
use crate::controls::{KbdVoice, keys};
use crate::measure::{Measure, Set};
use crate::theme::ActiveFacet;
use crate::tokens::{Face, TypeRole};
use gpui::{
    AnyElement, App, Element, ElementId, IntoElement, ParentElement, SharedString, Stateful,
    Styled, Window, div, px,
};
use std::sync::Arc;

const TEXT: TypeRole = TypeRole {
    face: Face::Ui,
    weight: 400.0,
    size: 12.0,
    line: 16.0,
    tracking: 0.0,
    italic: false,
};

const TITLE: TypeRole = TypeRole {
    weight: 500.0,
    ..TEXT
};

/// What a tip says.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TipText {
    /// A title line (rich tips).
    pub title: Option<SharedString>,
    /// The body.
    pub body: SharedString,
    /// A key chord (`⌘ K`), shown after the body.
    pub chord: Vec<SharedString>,
}

/// The content builder for a tip.
pub fn content(text: TipText) -> impl Fn(&Measure, &mut Window, &mut App) -> AnyElement + 'static {
    move |measure: &Measure, _window: &mut Window, cx: &mut App| {
        let palette = cx.facet().palette();
        let scale = measure.scale();
        let mut chip = div()
            .flex()
            .flex_col()
            .gap(px(2.0) * scale)
            .px(px(10.0) * scale)
            .py(px(6.0) * scale);
        if let Some(title) = &text.title {
            chip = chip.child(
                div()
                    .set(TITLE, measure)
                    .text_color(palette.ink0.hsla())
                    .child(title.clone()),
            );
        }
        let mut line = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(8.0) * scale)
            .child(
                div()
                    .set(TEXT, measure)
                    .text_color(if text.title.is_some() {
                        palette.ink2.hsla()
                    } else {
                        palette.ink1.hsla()
                    })
                    .child(text.body.clone()),
            );
        if !text.chord.is_empty() {
            let chord: Vec<&str> = text.chord.iter().map(SharedString::as_ref).collect();
            line = line.child(keys(&chord, KbdVoice::Plain, measure));
        }
        chip.child(line).into_any_element()
    }
}

/// A tip request for `key` at `anchor`.
pub fn request(key: impl Into<ElementId>, anchor: gpui::Bounds<gpui::Pixels>, text: TipText) -> FloatRequest {
    FloatRequest::new(key, anchor, FloatKind::Tip, content(text)).side(Side::Above)
}

/// Tips on stateful elements (the element's id keys the trigger).
pub trait Tipped: Sized {
    /// A one-line tip.
    fn tip(self, text: impl Into<SharedString>) -> Trigger;
    /// A title, a body and a key chord.
    fn tip_rich(self, title: impl Into<SharedString>, body: impl Into<SharedString>, chord: &[&str]) -> Trigger;
    /// Any tip text.
    fn tip_text(self, text: TipText) -> Trigger;
}

impl<E> Tipped for Stateful<E>
where
    Stateful<E>: Element + IntoElement,
    E: Element,
{
    fn tip(self, text: impl Into<SharedString>) -> Trigger {
        self.tip_text(TipText {
            title: None,
            body: text.into(),
            chord: Vec::new(),
        })
    }

    fn tip_rich(self, title: impl Into<SharedString>, body: impl Into<SharedString>, chord: &[&str]) -> Trigger {
        self.tip_text(TipText {
            title: Some(title.into()),
            body: body.into(),
            chord: chord.iter().map(|key| SharedString::from((*key).to_owned())).collect(),
        })
    }

    fn tip_text(self, text: TipText) -> Trigger {
        let id = Element::id(&self).unwrap_or_else(|| ElementId::Name("tip".into()));
        let key = ElementId::NamedChild(Arc::new(id), "tip".into());
        let request_key = key.clone();
        float::trigger(
            key,
            move |bounds| request(request_key.clone(), bounds, text.clone()),
            self,
        )
    }
}
