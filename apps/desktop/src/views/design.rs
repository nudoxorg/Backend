//! The four canonical Nudox facet boards.
//!
//! These are a product-owned showcase route, not HTML reference screenshots
//! embedded in the application. They use the same theme, bundled fonts,
//! glyphs, cut plates, state colours, and motion preference as the reader, so
//! the visual harness can photograph the exact design vocabulary at the same
//! scale as a real route.

use crate::theme::Theme;
use crate::theme::palette::Paint;
use crate::theme::tokens::{Space, TypeScale, hairline, space};
use crate::ui::{components, glyph, icon, surface, text};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    Div, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled, div,
    px,
};

/// One canonical design-system board.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DesignFacet {
    /// “One mark, one language”.
    Brand,
    /// “Colour with a job”.
    Colour,
    /// “The information language”.
    Language,
    /// Orbit → Package → Page → Source.
    Descent,
}

impl DesignFacet {
    /// Stable state id used by the screenshot harness.
    pub(crate) const fn id(self) -> &'static str {
        match self {
            Self::Brand => "showcase-brand",
            Self::Colour => "showcase-colour",
            Self::Language => "showcase-language",
            Self::Descent => "showcase-descent",
        }
    }

    /// Visible board title.
    const fn title(self) -> &'static str {
        match self {
            Self::Brand => "One mark, one language",
            Self::Colour => "Colour with a job",
            Self::Language => "The information language",
            Self::Descent => "Descent",
        }
    }

    /// Kicker used by the board header.
    const fn kicker(self) -> &'static str {
        match self {
            Self::Brand => "FACET 00",
            Self::Colour => "FACET 01",
            Self::Language => "FACET 02",
            Self::Descent => "FACET 04",
        }
    }

    /// Serif lede used by the board header.
    const fn lede(self) -> &'static str {
        match self {
            Self::Brand => {
                "Nudox reads code across seven ecosystems. Facet is the language it speaks: every edge, stripe and chevron in the app is lifted from the logo and given exactly one job."
            }
            Self::Colour => {
                "Abyss, silver, signal, edge. The palette is the logo, rationed: most of every screen is navy and silver so that the few coloured things can each mean exactly one thing."
            }
            Self::Language => {
                "How Nudox says things without words. Learn it once on this page and every screen reads faster."
            }
            Self::Descent => {
                "The whole app is one zoom. These are the real screens, each one opening out of a point in the one before it."
            }
        }
    }
}

/// Renders one full board in the shell's current appearance and motion mode.
pub(crate) fn showcase(theme: &Theme, facet: DesignFacet, motion_ms: u64) -> impl IntoElement {
    // The showcase participates in the same deterministic capture timeline as
    // the product shell. A short mint glint makes the wave phase observable in
    // normal motion while reduced motion still renders one settled frame.
    let phase = (motion_ms % 620) as f32 / 620.0;
    let mut glint = theme.paint(Paint::Focus);
    glint.alpha = 0.16 + phase * 0.2;
    let glint_width = 120.0 + phase * 360.0;
    let board = surface::ground(theme)
        .id(facet.id())
        .size_full()
        .relative()
        .overflow_y_scroll()
        .p(px(64.0))
        .text_color(theme.paint(Paint::Silver1))
        .child(
            div()
                .absolute()
                .top_0()
                .left_0()
                .w(px(glint_width))
                .h(px(2.0))
                .bg(glint),
        )
        .child(board_header(theme, facet))
        .child(match facet {
            DesignFacet::Brand => brand(theme),
            DesignFacet::Colour => colour(theme),
            DesignFacet::Language => language(theme),
            DesignFacet::Descent => descent(theme),
        });
    board
}

fn board_header(theme: &Theme, facet: DesignFacet) -> Div {
    div()
        .w_full()
        .flex()
        .items_start()
        .justify_between()
        .gap(space(Space::Bay))
        .pb(space(Space::Bay))
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .flex()
                .flex_col()
                .gap(space(Space::Tight))
                .child(
                    text::identity_text(theme, TypeScale::Micro)
                        .font_family(theme.specimen())
                        .child(facet.kicker()),
                )
                .child(text::display(theme, TypeScale::Hero).child(facet.title()))
                .child(
                    text::text_at(theme, TypeScale::Body, Paint::Silver2)
                        .font_family(theme.serif_face())
                        .italic()
                        .max_w(px(780.0))
                        .child(facet.lede()),
                ),
        )
        .child(logo_facet(theme, 54.0, Paint::Focus))
}

fn logo_facet(theme: &Theme, side: f32, role: Paint) -> Div {
    let mut edge = theme.paint(role);
    edge.alpha = 0.78;
    div()
        .relative()
        .flex_none()
        .w(px(side))
        .h(px(side))
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .absolute()
                .inset(px(side * 0.22))
                .bg(theme.plane_wash(crate::theme::ramp::Hue::degrees(152.0), 0.18)),
        )
        .child(
            icon::asset(icon::FACET_PATH, side, edge)
                .absolute()
                .inset_0(),
        )
        .child(glyph::package_tile(theme, side > 40.0))
}

fn section_title(theme: &Theme, title: &str, detail: &str) -> Div {
    div()
        .flex()
        .items_baseline()
        .gap(space(Space::Snug))
        .child(text::heading(theme, TypeScale::Section).child(title.to_owned()))
        .child(
            text::text_at(theme, TypeScale::Body, Paint::Silver2)
                .font_family(theme.serif_face())
                .italic()
                .child(detail.to_owned()),
        )
}

fn plate(theme: &Theme, role: Paint, title: &str, body: &str) -> Div {
    surface::cut(theme, role)
        .flex_1()
        .min_w(px(190.0))
        .p(space(Space::Room))
        .flex()
        .flex_col()
        .gap(space(Space::Tight))
        .child(text::heading(theme, TypeScale::Interface).child(title.to_owned()))
        .child(
            text::text_at(theme, TypeScale::Small, Paint::Silver2)
                .font_family(theme.serif_face())
                .italic()
                .child(body.to_owned()),
        )
}

fn brand(theme: &Theme) -> Div {
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(space(Space::Bay))
        .child(
            surface::cut(theme, Paint::Focus)
                .w_full()
                .p(px(28.0))
                .flex()
                .items_center()
                .gap(px(64.0))
                .child(logo_facet(theme, 220.0, Paint::Focus))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(space(Space::Tight))
                        .child(text::display(theme, TypeScale::Hero).child("Nudox"))
                        .child(
                            text::text_at(theme, TypeScale::Section, Paint::Silver1)
                                .font_family(theme.serif_face())
                                .italic()
                                .child("Read any code like it was written for you."),
                        )
                        .child(text::dim(theme).child("The mark is a cue, not decoration.")),
                ),
        )
        .child(section_title(
            theme,
            "Everything comes from the mark",
            "six parts of the logo, six jobs in the app",
        ))
        .child(
            div()
                .w_full()
                .flex()
                .flex_wrap()
                .gap(space(Space::Snug))
                .child(plate(
                    theme,
                    Paint::Periwinkle,
                    "The diamond becomes the stone",
                    "Every kind of thing is a cut stone, lit from the top-left like the mark.",
                ))
                .child(plate(
                    theme,
                    Paint::Silver2,
                    "The stripes become certainty",
                    "Pending, sampled and inferred facts keep their hatch until the feed arrives.",
                ))
                .child(plate(
                    theme,
                    Paint::Mint,
                    "The chevron becomes direction",
                    "The green chevron is the separator, the prompt and the way deeper.",
                )),
        )
        .child(
            div()
                .w_full()
                .flex()
                .flex_wrap()
                .gap(space(Space::Snug))
                .child(plate(
                    theme,
                    Paint::Focus,
                    "The bevel becomes focus",
                    "The periwinkle edge belongs to the keyboard and to state.",
                ))
                .child(plate(
                    theme,
                    Paint::Mint,
                    "The falling bars become combs",
                    "Release bars and activity marks carry a wave under the pointer.",
                ))
                .child(plate(
                    theme,
                    Paint::Stopped,
                    "The cut corner becomes every surface",
                    "Plates, buttons, tooltips and ticks share one 45 degree grammar.",
                )),
        )
        .child(principles(theme))
}

fn principles(theme: &Theme) -> Div {
    let values = [
        (
            "1",
            "Cut, not painted",
            "Flat tones and hard edges. Depth is facets, hatch and motion.",
        ),
        (
            "2",
            "The bevel is the state",
            "Focus, yours, working and waiting all live on the edge.",
        ),
        (
            "3",
            "Marks, not labels",
            "Kinds, modifiers and receivers are icons that explain themselves.",
        ),
        (
            "4",
            "Plain until touched",
            "Dense data rests as texture and answers with a wave.",
        ),
        (
            "5",
            "Motion explains",
            "Things arrive from where you asked, and the altimeter answers.",
        ),
    ];
    div()
        .w_full()
        .flex()
        .flex_wrap()
        .gap(space(Space::Gutter))
        .children(values.into_iter().map(|(number, title, body)| {
            div()
                .flex_1()
                .min_w(px(150.0))
                .border_t(hairline())
                .border_color(theme.paint(Paint::Rule1))
                .pt(space(Space::Snug))
                .flex()
                .flex_col()
                .gap(space(Space::Tight))
                .child(
                    text::display(theme, TypeScale::Section)
                        .text_color(theme.paint(Paint::Mint))
                        .child(number),
                )
                .child(text::heading(theme, TypeScale::Interface).child(title))
                .child(
                    text::dim(theme)
                        .font_family(theme.serif_face())
                        .italic()
                        .child(body),
                )
        }))
}

fn swatch(theme: &Theme, name: &str, value: &str, role: Paint) -> Div {
    surface::cut(theme, role)
        .h(px(96.0))
        .flex_1()
        .min_w(px(130.0))
        .p(space(Space::Snug))
        .flex()
        .flex_col()
        .justify_between()
        .child(text::text_at(theme, TypeScale::Small, Paint::Silver0).child(name.to_owned()))
        .child(
            text::text_at(theme, TypeScale::Small, Paint::Silver2)
                .font_family(theme.specimen())
                .child(value.to_owned()),
        )
}

fn colour(theme: &Theme) -> Div {
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(space(Space::Bay))
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap(space(Space::Gutter))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(360.0))
                        .flex()
                        .flex_col()
                        .gap(space(Space::Snug))
                        .child(section_title(theme, "Ground", "navy, never neutral grey"))
                        .child(div().flex().gap(px(1.0)).children([
                            swatch(theme, "Abyss 0", "--g0", Paint::Abyss0),
                            swatch(theme, "Abyss 1", "--g1", Paint::Abyss1),
                            swatch(theme, "Plate", "--plate", Paint::Abyss2),
                            swatch(theme, "Plate 2", "--plate2", Paint::PeriwinkleSoft),
                            swatch(theme, "Plate 3", "--plate3", Paint::Focus),
                        ])),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(360.0))
                        .flex()
                        .flex_col()
                        .gap(space(Space::Snug))
                        .child(section_title(theme, "Silver", "silver is ownership"))
                        .child(div().flex().gap(px(1.0)).children([
                            swatch(theme, "Silver 0", "--silver0", Paint::Silver0),
                            swatch(theme, "Silver 1", "--silver1", Paint::Silver1),
                            swatch(theme, "Silver 2", "--silver2", Paint::Silver2),
                            swatch(theme, "Silver 3", "--silver3", Paint::Silver3),
                            swatch(theme, "Silver 4", "--silver4", Paint::Rule3),
                        ])),
                ),
        )
        .child(section_title(
            theme,
            "Four voices, one job each",
            "colour is rationed so it can mean something",
        ))
        .child(
            div()
                .w_full()
                .flex()
                .flex_wrap()
                .gap(space(Space::Snug))
                .child(voice(
                    theme,
                    Paint::Mint,
                    "--mint",
                    "Mint acts",
                    "The one thing to do on this view.",
                ))
                .child(voice(
                    theme,
                    Paint::Focus,
                    "--peri",
                    "Periwinkle focuses",
                    "Only the keyboard and selection.",
                ))
                .child(voice(
                    theme,
                    Paint::Waiting,
                    "--amber",
                    "Amber waits",
                    "Something is slow, stale or needs a look.",
                ))
                .child(voice(
                    theme,
                    Paint::Stopped,
                    "--coral",
                    "Coral stops",
                    "A fault, a block or a destructive act.",
                )),
        )
        .child(section_title(theme, "Daylight", "same voices, same bevel"))
        .child(
            components::button_with_state(
                theme,
                "showcase-add-to-library",
                "+  Add to library",
                components::Weight::Primary,
                false,
                true,
            )
            .w(px(156.0)),
        )
}

fn voice(theme: &Theme, role: Paint, token: &str, title: &str, body: &str) -> Div {
    surface::cut(theme, role)
        .flex_1()
        .min_w(px(220.0))
        .p(space(Space::Room))
        .flex()
        .flex_col()
        .gap(space(Space::Tight))
        .child(
            text::text_at(theme, TypeScale::Small, Paint::Silver0)
                .font_family(theme.specimen())
                .child(token.to_owned()),
        )
        .child(text::display(theme, TypeScale::Section).child(title.to_owned()))
        .child(
            text::dim(theme)
                .font_family(theme.serif_face())
                .italic()
                .child(body.to_owned()),
        )
}

fn language(theme: &Theme) -> Div {
    let families = [
        (
            "Namespaces",
            Paint::Silver2,
            "frames that hold things",
            "module   package   import",
        ),
        (
            "Types",
            Paint::Mint,
            "cut boxes with storage",
            "struct   class   enum   union   type",
        ),
        (
            "Contracts",
            Paint::Periwinkle,
            "hollow diamonds: promises",
            "trait   interface",
        ),
        (
            "Callables",
            Paint::Focus,
            "chevrons: things that run",
            "function   method   macro",
        ),
        (
            "Values",
            Paint::Waiting,
            "stones and lines: things that are",
            "constant   field   property   variant",
        ),
    ];
    div()
        .w_full()
        .flex()
        .flex_wrap()
        .gap(space(Space::Gutter))
        .child(
            div()
                .flex_1()
                .min_w(px(500.0))
                .flex()
                .flex_col()
                .gap(space(Space::Snug))
                .child(section_title(
                    theme,
                    "What a thing is",
                    "the family, shape and kind",
                ))
                .children(families.into_iter().map(|(name, role, desc, marks)| {
                    surface::cut(theme, role)
                        .w_full()
                        .p(space(Space::Snug))
                        .flex()
                        .items_center()
                        .gap(space(Space::Room))
                        .child(
                            text::heading(theme, TypeScale::Interface)
                                .w(px(120.0))
                                .child(name),
                        )
                        .child(
                            text::dim(theme)
                                .w(px(200.0))
                                .font_family(theme.serif_face())
                                .italic()
                                .child(desc),
                        )
                        .child(
                            text::text_at(theme, TypeScale::Small, Paint::Silver2)
                                .font_family(theme.specimen())
                                .child(marks),
                        )
                })),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(420.0))
                .flex()
                .flex_col()
                .gap(space(Space::Snug))
                .child(section_title(
                    theme,
                    "What is true of it",
                    "marks carry the modifiers",
                ))
                .child(plate(
                    theme,
                    Paint::Periwinkle,
                    "evaluated at compile time",
                    "The upper-case label is now a mark with a tooltip.",
                ))
                .child(plate(
                    theme,
                    Paint::Waiting,
                    "unsafe",
                    "You hold the invariants.",
                ))
                .child(plate(
                    theme,
                    Paint::Stopped,
                    "deprecated",
                    "An old contract carries its warning on the edge.",
                )),
        )
        .child(section_title(
            theme,
            "The bevel is the only state channel",
            "plates have one lit edge, and the content never moves",
        ))
        .child(
            div()
                .w_full()
                .flex()
                .flex_wrap()
                .gap(space(Space::Tight))
                .child(state_card(theme, Paint::Silver2, "Rest"))
                .child(state_card(theme, Paint::Focus, "Focus"))
                .child(state_card(theme, Paint::Mint, "Yours"))
                .child(state_card(theme, Paint::Waiting, "Waiting"))
                .child(state_card(theme, Paint::Stopped, "Stopped")),
        )
        .child(section_title(
            theme,
            "Icons are cut, not drawn",
            "one shape per contract",
        ))
        .child(div().flex().flex_wrap().gap(px(16.0)).children([
            glyph::kind_tile(theme, None, true),
            glyph::package_tile(theme, true),
            glyph::kind_tile(theme, None, false),
            logo_facet(theme, 32.0, Paint::Mint),
            logo_facet(theme, 32.0, Paint::Focus),
            logo_facet(theme, 32.0, Paint::Waiting),
        ]))
}

fn state_card(theme: &Theme, role: Paint, title: &str) -> Div {
    surface::cut(theme, role)
        .flex_1()
        .min_w(px(130.0))
        .p(space(Space::Room))
        .child(text::heading(theme, TypeScale::Interface).child(title.to_owned()))
}

fn descent(theme: &Theme) -> Div {
    let steps = [
        (
            "depth 0 · ⌘0",
            "Orbit",
            "Choose a stone. The package sits on the first ring because backend uses it directly.",
        ),
        (
            "depth 1 · ↳ on a stone",
            "Package",
            "The stone opens into its territory: the code reaches it here.",
        ),
        (
            "depth 2 · ↳ on a symbol",
            "Page",
            "A cell opens into a page with its margin and relations.",
        ),
        (
            "depth 3 · ⌘E",
            "Source",
            "The signature opens into the file it came from.",
        ),
    ];
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(space(Space::Bay))
        .child(
            surface::cut(theme, Paint::Focus)
                .w_full()
                .p(space(Space::Gutter))
                .flex()
                .flex_wrap()
                .items_end()
                .gap(space(Space::Room))
                .children(steps.into_iter().enumerate().map(|(index, (depth, title, body))| {
                    div()
                        .flex_1()
                        .min_w(px(210.0))
                        .flex()
                        .flex_col()
                        .gap(space(Space::Tight))
                        .when(index > 0, |step| step.border_l(hairline()).border_color(theme.paint(Paint::Leaf)).pl(space(Space::Room)))
                        .child(text::identity_text(theme, TypeScale::Micro).font_family(theme.specimen()).child(depth))
                        .child(text::display(theme, TypeScale::Section).child(title))
                        .child(text::dim(theme).font_family(theme.serif_face()).italic().child(body))
                })),
        )
        .child(
            div()
                .w_full()
                .flex()
                .flex_wrap()
                .gap(space(Space::Snug))
                .child(plate(theme, Paint::Silver2, "Why depth instead of destinations", "Home, Explore, Projects and a package page are rooms with four layouts. They are one path of magnifications."))
                .child(plate(theme, Paint::Mint, "Going back up", "⌘− surfaces one depth and returns to Orbit from anywhere."))
                .child(plate(theme, Paint::Focus, "A link is a coordinate", "nudox://cargo/serde@1.0.210/de/Deserializer#source is a coordinate, not a new destination.")),
        )
}
