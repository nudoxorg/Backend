//! Signatures and source, set as one shaped text run with typed colour.
//! Type names inside a signature are links; everything else is coloured text.
//! One text element, not a row of word-shaped boxes, so the monospace grid holds.
//!
//! GPUI has no inline-flow element, so rich text is built the other way round:
//! one string, a list of byte ranges with highlight styles, and a parallel list
//! of clickable ranges. That is exactly what a tokenized
//! [`backend_present::Signature`] already is, and it keeps ligatures, wrapping,
//! and caret geometry correct in a way that a flex row of per-token divs never
//! could.
//!
//! A linked type is underlined with a *dotted* rule rather than a solid one,
//! and the reason is honesty: [`backend_present::Resolved::ByName`] is the only
//! resolution the model claims, and it is a spelling match, not a proven
//! semantic edge. A solid underline would promise more than the engine knows.

use crate::theme::Theme;
use crate::theme::kind::kind_glyph;
use crate::theme::palette::Paint;
use crate::theme::tokens::{Space, TypeScale, line_height, space, type_size};
use backend_library::{DeclarationKind, SymbolKey};
use backend_present::{IdentityKey, Signature, TokenKind};
use gpui::{
    App, Div, ElementId, HighlightStyle, InteractiveText, ParentElement, SharedString, StyledText,
    UnderlineStyle, Window, px,
};
use gpui::{FontWeight, Styled};
use std::ops::Range;

/// The pieces an interactive run of text is assembled from.
struct Runs {
    text: SharedString,
    highlights: Vec<(Range<usize>, HighlightStyle)>,
    links: Vec<Range<usize>>,
    targets: Vec<SymbolKey>,
}

/// Returns the signature specimen block, with every resolved type linked.
pub(crate) fn signature_block(
    theme: &Theme,
    signature: &Signature,
    id: impl Into<SharedString>,
    open: impl Fn(SymbolKey, &mut Window, &mut App) + 'static,
) -> Div {
    let runs = signature_runs(theme, signature);
    super::surface::sunken(theme)
        .w_full()
        .px(space(Space::Room))
        .py(space(Space::Base))
        .font_family(theme.specimen())
        .text_size(type_size(TypeScale::Small))
        .line_height(line_height(TypeScale::Body))
        .text_color(theme.paint(Paint::Text))
        .child(interactive(runs, id, open))
}

/// Returns a signature flattened to one line and clipped to a budget.
pub(crate) fn preview(signature: &Signature, budget: usize) -> String {
    let flattened = signature.text();
    let squeezed: String = flattened.split_whitespace().collect::<Vec<_>>().join(" ");
    if squeezed.chars().count() <= budget {
        return squeezed;
    }
    let kept: String = squeezed.chars().take(budget.saturating_sub(1)).collect();
    format!("{kept}…")
}

fn interactive(
    runs: Runs,
    id: impl Into<SharedString>,
    open: impl Fn(SymbolKey, &mut Window, &mut App) + 'static,
) -> InteractiveText {
    let targets = runs.targets;
    let styled = StyledText::new(runs.text).with_highlights(runs.highlights);
    InteractiveText::new(ElementId::Name(id.into()), styled).on_click(
        runs.links,
        move |index, window, cx| {
            if let Some(symbol) = targets.get(index).copied() {
                open(symbol, window, cx);
            }
        },
    )
}

fn signature_runs(theme: &Theme, signature: &Signature) -> Runs {
    let mut text = String::new();
    let mut highlights = Vec::new();
    let mut links = Vec::new();
    let mut targets = Vec::new();
    for token in signature.tokens() {
        let start = text.len();
        text.push_str(token.text());
        let range = start..text.len();
        let symbol = token.target().and_then(|target| match target.key() {
            IdentityKey::Symbol(key) => Some(key),
            IdentityKey::Package(_) | IdentityKey::Absent => None,
        });
        highlights.push((range.clone(), style_for(theme, token.kind(), symbol.is_some())));
        if let Some(key) = symbol {
            links.push(range);
            targets.push(key);
        }
    }
    Runs {
        text: SharedString::from(text),
        highlights,
        links,
        targets,
    }
}

/// Returns the highlight for one token kind, underlined when it is a door.
pub(crate) fn style_for(theme: &Theme, kind: TokenKind, linked: bool) -> HighlightStyle {
    let mut style = HighlightStyle {
        color: Some(ink_for(theme, kind)),
        ..HighlightStyle::default()
    };
    if matches!(kind, TokenKind::Name) {
        style.font_weight = Some(FontWeight::SEMIBOLD);
    }
    if linked {
        style.underline = Some(UnderlineStyle {
            thickness: px(1.0),
            color: Some(super::text::underline_ink(theme, false)),
            wavy: false,
        });
    }
    style
}

fn ink_for(theme: &Theme, kind: TokenKind) -> gpui::Hsla {
    match kind {
        TokenKind::Keyword => theme.on_plane(kind_glyph(DeclarationKind::Module).hue()),
        TokenKind::Name => theme.paint(Paint::TextStrong),
        TokenKind::Type => theme.on_plane(kind_glyph(DeclarationKind::Struct).hue()),
        TokenKind::Binding => theme.on_plane(kind_glyph(DeclarationKind::Field).hue()),
        TokenKind::Literal => theme.on_plane(kind_glyph(DeclarationKind::Constant).hue()),
        TokenKind::Lifetime => theme.on_plane(kind_glyph(DeclarationKind::Macro).hue()),
        TokenKind::Punctuation => theme.paint(Paint::TextFaint),
        TokenKind::Text => theme.paint(Paint::Text),
    }
}

/// Returns a signature flattened to one coloured line, not interactive.
///
/// A result row and a member row want the same colour the page specimen has,
/// without a click target per token; this is the specimen's runs without its
/// links.
pub(crate) fn signature_line(theme: &Theme, signature: &Signature, budget: usize) -> StyledText {
    let mut text = String::new();
    let mut highlights = Vec::new();
    let mut spent = 0_usize;
    for token in signature.tokens() {
        if spent >= budget {
            text.push('…');
            break;
        }
        let squeezed: String = token.text().split_whitespace().collect::<Vec<_>>().join(" ");
        let piece = if token.text().starts_with(char::is_whitespace) && !text.is_empty() {
            format!(" {squeezed}")
        } else {
            squeezed
        };
        let start = text.len();
        text.push_str(&piece);
        spent = spent.saturating_add(piece.chars().count());
        highlights.push((start..text.len(), style_for(theme, token.kind(), false)));
    }
    StyledText::new(SharedString::from(text)).with_highlights(highlights)
}
