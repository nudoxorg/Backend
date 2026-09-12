//! Defines the element library for `interface-gui`.
//! This module owns every reusable element and the one conversion from theme colour to platform
//! colour. Its narrow surface means views compose glyphs, chips, rows, and faults and never touch
//! a pixel, a font, or a colour directly.
//!
//! # The gilt rule
//!
//! [`identity_text`] is the only builder in this crate that may paint the accent ramp. It draws the
//! crumb-trail address and the key tag: identities a reader copies. Navigable text is
//! [`nav_text`] instead — vellum glyphs, a hairline dotted underline — and the two are never
//! confused.

use core::ops::Range;

use compiler_ir::LinkKind;
use compiler_ir_vocabulary::EntityKind;
use gpui::{
    AbsoluteLength, DefiniteLength, Div, FontWeight, HighlightStyle, Hsla, InteractiveElement,
    SharedString, Stateful, Styled, StyledText, TextStyle, div, prelude::*,
};
use interface_library::render::common::{
    Affordance, Affordances, Fault, EXAMPLE_PACKAGE_URL,
};

use crate::{store::search::LaneChip, theme::{Color, Radius, Role, Space, Status, Theme}};

/// The kind glyph letters, in `EntityKind::ALL` order.
const KIND_GLYPHS: [&str; 15] = [
    "fn", "co", "rc", "mo", "fd", "al", "tr", "im", "en", "va", "st", "re", "pa", "ma", "ns",
];

/// The kind's full word, in `EntityKind::ALL` order.
const KIND_WORDS: [&str; 15] = [
    "function",
    "constant",
    "record",
    "module",
    "field",
    "alias",
    "trait",
    "implementation",
    "enum",
    "variant",
    "static",
    "reexport",
    "parameter",
    "macro",
    "namespace",
];

/// The two-letter glyph one declaration kind is drawn with.
#[must_use]
pub fn kind_glyph(kind: EntityKind) -> &'static str {
    EntityKind::ALL
        .into_iter()
        .zip(KIND_GLYPHS)
        .find_map(|(candidate, glyph)| (candidate == kind).then_some(glyph))
        .unwrap_or("·")
}

/// The full word one declaration kind is drawn with.
#[must_use]
pub fn kind_word(kind: EntityKind) -> &'static str {
    EntityKind::ALL
        .into_iter()
        .zip(KIND_WORDS)
        .find_map(|(candidate, word)| (candidate == kind).then_some(word))
        .unwrap_or("declaration")
}

/// The role word one graph relation is drawn with: a closed table over `LinkKind`.
#[must_use]
pub const fn relation_word(kind: LinkKind) -> &'static str {
    match kind {
        LinkKind::Calls => "calls",
        LinkKind::MethodCall => "method calls",
        LinkKind::TypeReference => "type references",
        LinkKind::Reads => "reads",
        LinkKind::Writes => "writes",
        LinkKind::Imports => "imports",
        LinkKind::Implements => "implements",
        LinkKind::Overrides => "overrides",
        LinkKind::Reexports => "reexports",
        LinkKind::Inherits => "inherits",
        LinkKind::Documents => "documents",
    }
}

/// Converts one theme colour to the platform colour, the only place this happens.
#[must_use]
pub fn hsla(color: Color) -> Hsla {
    Hsla {
        h: color.hue().turns(),
        s: color.chroma().get(),
        l: color.lightness().get(),
        a: color.alpha().get(),
    }
}

/// Applies one text role's family, size, leading, and weight to any element.
#[must_use]
pub fn with_role(element: Div, theme: &Theme, role: Role) -> Div {
    let style = role.style();
    element
        .font_family(SharedString::from(theme.family(style.face())))
        .text_size(gpui::px(theme.pixels(style.size())))
        .line_height(gpui::px(theme.pixels(style.leading())))
        .font_weight(FontWeight(f32::from(style.weight().value())))
}

/// A small-caps section label.
#[must_use]
pub fn section_label(theme: &Theme, text: &str) -> Div {
    with_role(div(), theme, Role::Caption)
        .text_color(hsla(theme.palette().text_low()))
        .child(text.to_uppercase())
}

/// One kind glyph: the two letters in the kind hue on the kind's tinted ground.
#[must_use]
pub fn kind_glyph_chip(theme: &Theme, kind: EntityKind) -> Div {
    let appearance = theme.appearance();
    let edge = theme.pixels(Space::Tight.rems()) * 2.0;
    with_role(div(), theme, Role::Dense)
        .flex()
        .items_center()
        .justify_center()
        .size(gpui::px(edge))
        .rounded(gpui::px(theme.pixels(Radius::Chip.rems())))
        .bg(hsla(crate::theme::kind_ground(kind, appearance)))
        .text_color(hsla(crate::theme::kind_color(kind, appearance)))
        .child(kind_glyph(kind).to_owned())
}

/// The gilt identity line: the one place the accent ramp may carry text.
///
/// Draws the crumb-trail address, the key tag, and nothing else. See the gilt rule on this module.
#[must_use]
pub fn identity_text(theme: &Theme, text: &str) -> Div {
    with_role(div(), theme, Role::Mono)
        .text_color(hsla(theme.palette().accent()))
        .child(text.to_owned())
}

/// Navigable text: vellum glyphs with a hairline dotted underline.
///
/// The click and hover listeners are attached by the view, which owns what navigation means; this
/// builder only draws the affordance so every navigable row in every panel looks the same.
#[must_use]
pub fn nav_text(theme: &Theme, text: &str) -> Div {
    nav_text_at(theme, Role::Ui, theme.palette().text(), text)
}

/// Navigable text at one role and one ink, with the same hairline underline affordance.
///
/// The reader's signature specimen and prose links draw navigable runs at roles [`nav_text`] does
/// not carry; this builder keeps their affordance identical. The ink comes from the vellum ramp or
/// a kind hue — never the accent ramp, whose one builder remains [`identity_text`]. Listeners stay
/// with the view.
#[must_use]
pub fn nav_text_at(theme: &Theme, role: Role, color: Color, text: &str) -> Div {
    with_role(div(), theme, role)
        .text_color(hsla(color))
        .border_b_1()
        .border_color(hsla(theme.palette().border()))
        .child(text.to_owned())
}

/// Inert text: present, unfake, and not navigable.
#[must_use]
pub fn inert_text(theme: &Theme, role: Role, text: &str) -> Div {
    with_role(div(), theme, role)
        .text_color(hsla(theme.palette().text_inert()))
        .child(text.to_owned())
}

/// Ordinary reading text at one role.
#[must_use]
pub fn text(theme: &Theme, role: Role, text: &str) -> Div {
    with_role(div(), theme, role)
        .text_color(hsla(theme.palette().text()))
        .child(text.to_owned())
}

/// Secondary text at one role.
#[must_use]
pub fn text_low(theme: &Theme, role: Role, text: &str) -> Div {
    with_role(div(), theme, role)
        .text_color(hsla(theme.palette().text_low()))
        .child(text.to_owned())
}

/// One lane chip: label, glyph, and note on the lane's own status colour.
#[must_use]
pub fn lane_chip(theme: &Theme, chip: &LaneChip) -> Div {
    let status = if !chip.ran {
        Status::Danger
    } else if chip.glyph == "✓" {
        Status::Ok
    } else {
        Status::Warn
    };
    with_role(div(), theme, Role::Dense)
        .flex()
        .gap(gpui::px(theme.pixels(Space::Hair.rems())))
        .px(gpui::px(theme.pixels(Space::Tight.rems())))
        .py(gpui::px(theme.pixels(Space::Hair.rems())))
        .rounded(gpui::px(theme.pixels(Radius::Chip.rems())))
        .bg(hsla(status.ground(theme.appearance())))
        .text_color(hsla(status.color(theme.appearance())))
        .child(format!("{} {} {}", chip.glyph, chip.label, chip.note))
}

/// How this surface spells the one call that could make progress past a fault.
pub struct ReaderAffordances;

impl Affordances for ReaderAffordances {
    fn spell(&self, affordance: &Affordance) -> Option<String> {
        match affordance {
            Affordance::None => None,
            Affordance::Packages => Some("open the shelf panel".to_owned()),
            Affordance::Health => Some("read capability health".to_owned()),
            Affordance::Add { package } => Some(format!("add {package}")),
            Affordance::Resolve { text } => Some(format!("type {text} in the omnibar")),
            Affordance::Search { query } => Some(format!("search for {query}")),
            Affordance::Show { address } => Some(format!("open {address}")),
        }
    }
}

/// One fault drawn as content: the slug, the operand, the detail, and the next call.
#[must_use]
pub fn fault_block(theme: &Theme, fault: &Fault) -> Div {
    let mut lines = String::new();
    interface_library::render::common::write_fault(&mut lines, fault, &ReaderAffordances);
    with_role(div(), theme, Role::Mono)
        .text_color(hsla(Status::Danger.color(theme.appearance())))
        .px(gpui::px(theme.pixels(Space::Snug.rems())))
        .py(gpui::px(theme.pixels(Space::Tight.rems())))
        .rounded(gpui::px(theme.pixels(Radius::Control.rems())))
        .bg(hsla(Status::Danger.ground(theme.appearance())))
        .child(lines.trim_end().to_owned())
}

/// The teaching line shown where the shelf is empty.
#[must_use]
pub fn empty_shelf_hint(theme: &Theme) -> Div {
    text_low(
        theme,
        Role::Body,
        &format!("nothing is on the shelf yet; add a package to begin, for example {EXAMPLE_PACKAGE_URL}"),
    )
}

/// A field's visible chrome: role-typed text, a focus ring, and a caret while focused.
#[must_use]
pub fn field(theme: &Theme, role: Role, text: &str, placeholder: &str, focused: bool) -> Div {
    let shown = if text.is_empty() { placeholder } else { text };
    let mut field = with_role(div(), theme, role)
        .flex_1()
        .flex()
        .items_center()
        .gap(gpui::px(1.0))
        .px(gpui::px(theme.pixels(Space::Tight.rems())))
        .py(gpui::px(theme.pixels(Space::Hair.rems())))
        .rounded(gpui::px(theme.pixels(Radius::Control.rems())))
        .bg(hsla(theme.palette().element()))
        .border_1()
        .border_color(hsla(if focused {
            theme.palette().accent()
        } else {
            theme.palette().border()
        }));
    if text.is_empty() {
        field = field.child(inert_text(theme, role, shown));
    } else {
        field = field.child(text_low(theme, role, shown));
    }
    if focused {
        field = field.child(
            with_role(div(), theme, role)
                .text_color(hsla(theme.palette().text()))
                .child("▏".to_owned()),
        );
    }
    field
}

/// A clickable chip with a click listener attached, the button of this interface.
#[must_use]
pub fn button(
    theme: &Theme,
    label: &str,
    emphasized: bool,
    on_click: impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> Stateful<Div> {
    let ground = if emphasized {
        theme.palette().accent()
    } else {
        theme.palette().element()
    };
    let ink = if emphasized {
        theme.palette().on_accent()
    } else {
        theme.palette().text()
    };
    with_role(div(), theme, Role::Ui)
        .id(SharedString::from(label))
        .flex()
        .items_center()
        .px(gpui::px(theme.pixels(Space::Tight.rems())))
        .py(gpui::px(theme.pixels(Space::Hair.rems())))
        .rounded(gpui::px(theme.pixels(Radius::Chip.rems())))
        .bg(hsla(ground))
        .text_color(hsla(ink))
        .cursor_pointer()
        .on_click(on_click)
        .child(label.to_owned())
}

/// The glyph one copy affordance carries, and the check it flips to while the copy is fresh.
///
/// The view owns when the check clears; this builder only spells both states.
#[must_use]
pub const fn copy_glyph(copied: bool) -> &'static str {
    if copied {
        "✓"
    } else {
        "⧉"
    }
}

/// The gilt identity line with its copy glyph appended.
///
/// The glyph inherits the accent ink from [`identity_text`], so the copy affordance stays inside
/// the one builder the gilt rule licenses. The click listener stays with the view, which owns
/// what was copied and for how long the check shows.
#[must_use]
pub fn copyable_identity(theme: &Theme, text: &str, copied: bool) -> Div {
    identity_text(theme, text)
        .flex()
        .items_center()
        .gap(gpui::px(theme.pixels(Space::Hair.rems())))
        .child(copy_glyph(copied).to_owned())
}

/// One paragraph as a single flowing text element with styled runs, not one element per run.
///
/// The whole string lays out as one text element, so it wraps at whatever width the measure
/// gives it, the way prose should. `highlights` and `families` carry byte ranges into `text`;
/// they must be sorted by start and must not overlap, which is on the caller because the caller
/// assembled the string and knows where its runs begin and end.
#[must_use]
pub fn styled_paragraph(
    theme: &Theme,
    role: Role,
    ink: Color,
    text: SharedString,
    highlights: Vec<(Range<usize>, HighlightStyle)>,
    families: Vec<(Range<usize>, SharedString)>,
) -> StyledText {
    let style = role.style();
    let text_style = TextStyle {
        color: hsla(ink),
        font_family: SharedString::from(theme.family(style.face())),
        font_size: AbsoluteLength::Pixels(gpui::px(theme.pixels(style.size()))),
        line_height: DefiniteLength::Absolute(AbsoluteLength::Pixels(gpui::px(
            theme.pixels(style.leading()),
        ))),
        font_weight: FontWeight(f32::from(style.weight().value())),
        ..TextStyle::default()
    };
    StyledText::new(text)
        .with_default_highlights(&text_style, highlights)
        .with_font_family_overrides(families)
}
