//! Defines the element library for `interface-gui`.
//! This module owns every reusable element and the one conversion from theme colour to platform
//! colour. Its narrow surface means views compose glyphs, chips, rows, and faults and never touch
//! a pixel, a font, or a colour directly.
//!
//! # The gilt rule
//!
//! [`identity_text`] is the only builder in this crate that may paint the accent ramp. It draws the
//! crumb-trail address and the key tag: identities a reader copies. Navigable text is
//! [`nav_text`] instead — vellum glyphs, a kind-hued glyph chip, a hairline dotted underline — and
//! the two are never confused.

use compiler_ir_vocabulary::EntityKind;
use gpui::{
    AnyElement, Context, Div, FontWeight, Hsla, InteractiveElement, SharedString, Stateful,
    Styled, div, prelude::*,
};
use interface_library::render::common::{
    Affordance, Affordances, Fault, EXAMPLE_PACKAGE_URL,
};

use crate::{
    app::Workspace,
    store::search::LaneChip,
    theme::{self, Theme, tokens::Color, tokens::Face, tokens::Role, tokens::Space},
};

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
        .font_size(gpui::px(theme.pixels(style.size())))
        .line_height(gpui::px(theme.pixels(style.leading())))
        .font_weight(FontWeight::from_number(style.weight().value()))
}

/// A small-caps section label.
#[must_use]
pub fn section_label(theme: &Theme, text: &str) -> Div {
    with_role(div(), theme, Role::Caption)
        .text_color(hsla(theme.palette().text_low()))
        .uppercase()
        .child(text.to_owned())
}

/// One kind glyph: the two letters in the kind hue on the kind's tinted ground.
#[must_use]
pub fn kind_glyph_chip(theme: &Theme, kind: EntityKind) -> Div {
    let appearance = theme.appearance();
    with_role(div(), theme, Role::Dense)
        .flex()
        .items_center()
        .justify_center()
        .w(gpui::px(theme.pixels(Space::Tight.rems()).mul(2.0)))
        .h(gpui::px(theme.pixels(Space::Tight.rems()).mul(2.0)))
        .rounded(theme::tokens::Radius::Chip.rems().get() * theme.root_pixels() / 16.0)
        .bg(hsla(theme::kind_ground(kind, appearance)))
        .text_color(hsla(theme::kind_color(kind, appearance)))
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
    with_role(div(), theme, Role::Ui)
        .text_color(hsla(theme.palette().text()))
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
        theme::Status::Danger
    } else if chip.glyph == "✓" {
        theme::Status::Ok
    } else {
        theme::Status::Warn
    };
    with_role(div(), theme, Role::Dense)
        .flex()
        .gap(gpui::px(theme.pixels(Space::Hair.rems())))
        .px(gpui::px(theme.pixels(Space::Tight.rems())))
        .py(gpui::px(theme.pixels(Space::Hair.rems())))
        .rounded(gpui::px(theme.root_pixels() * 0.25))
        .bg(hsla(status.ground(theme.appearance())))
        .text_color(hsla(status.color(theme.appearance())))
        .child(format!("{} {chip.label} {chip.note}", chip.glyph))
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
    let mut block = with_role(div(), theme, Role::Mono)
        .text_color(hsla(theme::Status::Danger.color(theme.appearance())))
        .child(lines.trim_end().to_owned());
    block = block
        .px(gpui::px(theme.pixels(Space::Snug.rems())))
        .py(gpui::px(theme.pixels(Space::Tight.rems())))
        .rounded(gpui::px(theme.root_pixels() * 0.25))
        .bg(hsla(theme::Status::Danger.ground(theme.appearance())));
    block
}

/// The teaching line shown where the shelf is empty.
#[must_use]
pub fn empty_shelf_hint(theme: &Theme) -> Div {
    text_low(theme, Role::Body, "nothing is on the shelf yet")
        .child(format!("add a package to begin, for example {EXAMPLE_PACKAGE_URL}"))
}

/// A field's visible chrome: role-typed text, a focus ring, and a caret while focused.
#[must_use]
pub fn field(theme: &Theme, role: Role, text: &str, placeholder: &str, focused: bool) -> Div {
    let shown = if text.is_empty() { placeholder } else { text };
    let mut field = with_role(div(), theme, role)
        .flex_1()
        .px(gpui::px(theme.pixels(Space::Tight.rems())))
        .py(gpui::px(theme.pixels(Space::Hair.rems())))
        .rounded(gpui::px(theme.root_pixels() * 0.25))
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
        field = field.child(with_role(div(), theme, role).child("▏".to_owned()));
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
        .id(())
        .flex()
        .items_center()
        .px(gpui::px(theme.pixels(Space::Tight.rems())))
        .py(gpui::px(theme.pixels(Space::Hair.rems())))
        .rounded(gpui::px(theme.root_pixels() * 0.25))
        .bg(hsla(ground))
        .text_color(hsla(ink))
        .cursor_pointer()
        .on_click(on_click)
        .child(label.to_owned())
}

/// Wraps one element so hovering it asks for the target's hover card at most once.
#[must_use]
pub fn hover_card_target(
    element: AnyElement,
    key: crate::store::document::PageKey,
    cx: &Context<Workspace>,
) -> Div {
    let hover_key = key;
    let leave_key = key;
    div()
        .on_hover(cx.listener(move |workspace: &mut Workspace, hovered: &bool, _, cx| {
            if *hovered {
                workspace.open_card(&hover_key);
            } else {
                if workspace.hovered() == Some(&leave_key) {
                    workspace.forget_hover(&leave_key);
                }
            }
            cx.notify();
        }))
        .child(element)
}
