//! `SignatureLine` — renders a pre-tokenised signature (GUI-PLAN §11).
//!
//! # Why this takes tokens rather than a string
//!
//! LR-4 says a signature is produced in exactly one place —
//! `nudox_engine::chunk::signature::tokens` — and consumed everywhere. This
//! component is the *consumer* end of that rule: it never parses, never
//! formats, and never decides what a signature says. It decides only how the
//! pieces look, and which of them are links.
//!
//! That split is what lets the symbol-page header, a search hit row, a
//! quick-peek popover, an MCP response and the diff view all show byte-identical
//! signatures. If this component took a `&str` it would be free to disagree with
//! the others, and the first thing a user would notice is that search results
//! and the page they open render types differently.
//!
//! # Shaped-text caching (§1.1.4)
//!
//! Every token's text is a `SharedString` produced at *update* time, not render
//! time. GPUI caches glyph shaping by `(text, font, size)`, so stable
//! `SharedString`s make a scrolling list of signatures nearly free; building
//! them with `format!` in `render` would defeat the cache on every frame.

use gpui::{
    App, ElementId, InteractiveElement as _, IntoElement, ParentElement, RenderOnce,
    SharedString, StatefulInteractiveElement as _, Styled, Window, div,
};

use crate::theme::ext::ThemeExtAccessor as _;
use gpui::prelude::*;

/// The one key, mirrored.
///
/// TODO(wire): replace with `nudox_engine::wire::SymbolKey` (itself a re-export
/// of `nudox_ir::change::StableRef`) once `lindsey` depends on the engine.
/// Per LR-1 there is exactly one key type in the system; this mirror exists
/// only because the dependency edge is not wired yet, and must not outlive it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SymbolKey(pub SharedString);

/// One lexical piece of a rendered signature.
///
/// TODO(wire): replace with `nudox_engine::wire::SigToken`. The variants here
/// mirror it exactly so the swap is a `use` change, not a rewrite.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum SigToken {
    /// A language keyword — `pub`, `fn`, `struct`, `impl`.
    Kw(&'static str),
    /// The declared name of the symbol itself.
    Ident(SharedString),
    /// A type reference. `target` is `Some` when it resolves to a symbol we can
    /// navigate to, which is what makes the token a link rather than plain text.
    Ty {
        /// Display text for the type.
        text: SharedString,
        /// Navigation target, when the type resolves through the corpus.
        target: Option<SymbolKey>,
    },
    /// Punctuation — `(`, `)`, `,`, `->`, `<`, `>`.
    Punct(&'static str),
    /// A single space. Modelled as a token rather than baked into neighbours so
    /// that the renderer controls spacing consistently.
    Ws,
    /// A generic parameter name — `T`, `'a`.
    Generic(SharedString),
}

impl SigToken {
    /// The text this token displays.
    ///
    /// Never empty: an unrenderable token falls back to `"?"` upstream, so a
    /// blank gap in a signature is a bug rather than a silent omission.
    fn text(&self) -> &str {
        match self {
            Self::Kw(s) | Self::Punct(s) => s,
            Self::Ident(s) | Self::Generic(s) => s.as_ref(),
            Self::Ty { text, .. } => text.as_ref(),
            Self::Ws => " ",
            _ => "?",
        }
    }
}

/// A monospace signature with clickable type links.
///
/// `on_navigate` receives the [`SymbolKey`] of a clicked type. The component
/// performs no navigation itself — it reports intent upward, per the one-way
/// data-flow law in `lib.rs`.
#[derive(IntoElement)]
pub struct SignatureLine {
    id: ElementId,
    tokens: Vec<SigToken>,
    on_navigate: Option<Box<dyn Fn(&SymbolKey, &mut Window, &mut App) + 'static>>,
}

impl SignatureLine {
    /// Build a signature line from pre-computed tokens.
    ///
    /// `id` must be stable across frames (LD-19) — animation identity, hover
    /// state and hit-testing all key off it.
    pub fn new(id: impl Into<ElementId>, tokens: Vec<SigToken>) -> Self {
        Self {
            id: id.into(),
            tokens,
            on_navigate: None,
        }
    }

    /// Make resolved `Ty` tokens clickable.
    ///
    /// Without this, `Ty` tokens still render in their distinct colour but do
    /// not respond to the pointer — correct for surfaces like a search row
    /// where the whole row is the click target.
    pub fn on_navigate(
        mut self,
        handler: impl Fn(&SymbolKey, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_navigate = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for SignatureLine {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let ext = cx.theme_ext();
        let mono = ext.type_scale.mono;
        let syn = ext.syntax;
        let kw_colour = syn.kw;
        let ty_colour = syn.ty_name;
        let punct_colour = syn.punct;
        let ident_colour = syn.ident;
        let generic_colour = syn.generic;

        // `Rc` so each `Ty` token's click handler can share one caller-supplied
        // closure without cloning the boxed `dyn Fn` per token.
        let navigate: Option<std::rc::Rc<dyn Fn(&SymbolKey, &mut Window, &mut App)>> =
            self.on_navigate.map(std::rc::Rc::from);

        div()
            .id(self.id.clone())
            .flex()
            .flex_wrap()
            .items_center()
            .font_family("monospace")
            .text_size(mono.size)
            .line_height(mono.line_height)
            .children(self.tokens.into_iter().enumerate().map(|(ix, token)| {
                let text = SharedString::from(token.text().to_owned());

                match &token {
                    // A resolved type is the only interactive token: it gets
                    // hover feedback and an underline so it reads as a link
                    // before the pointer arrives (§5.1 `hover.tint`).
                    SigToken::Ty {
                        target: Some(key), ..
                    } => {
                        let key = key.clone();
                        let navigate = navigate.clone();
                        div()
                            .id(("sig.ty", ix))
                            .text_color(ty_colour)
                            .hover(|s| s.underline())
                            .when_some(navigate, |el, navigate| {
                                el.cursor_pointer().on_click(move |_, window, cx| {
                                    navigate(&key, window, cx)
                                })
                            })
                            .child(text)
                            // `.id()` makes this arm a `Stateful<Div>` while the
                            // others are bare `Div`; erase to `AnyElement` so the
                            // match has one type.
                            .into_any_element()
                    }
                    SigToken::Ty { .. } => {
                        div().text_color(ty_colour).child(text).into_any_element()
                    }
                    SigToken::Kw(_) => {
                        div().text_color(kw_colour).child(text).into_any_element()
                    }
                    SigToken::Punct(_) => {
                        div().text_color(punct_colour).child(text).into_any_element()
                    }
                    SigToken::Generic(_) => div()
                        .text_color(generic_colour)
                        .child(text)
                        .into_any_element(),
                    SigToken::Ws => div().child(text).into_any_element(),
                    _ => div()
                        .text_color(ident_colour)
                        .child(text)
                        .into_any_element(),
                }
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No token may render as empty text.
    ///
    /// A blank run in a signature is invisible to the reader but shifts every
    /// token after it, so it reads as a rendering bug rather than missing data.
    #[test]
    fn every_token_renders_non_empty_text() {
        let tokens = vec![
            SigToken::Kw("pub"),
            SigToken::Ws,
            SigToken::Ident("map_err".into()),
            SigToken::Punct("("),
            SigToken::Generic("T".into()),
            SigToken::Ty {
                text: "Result".into(),
                target: Some(SymbolKey("cargo:core#abc".into())),
            },
            SigToken::Ty {
                text: "Unresolved".into(),
                target: None,
            },
        ];
        for token in &tokens {
            assert!(
                !token.text().is_empty(),
                "token {token:?} rendered empty text"
            );
        }
    }
}
