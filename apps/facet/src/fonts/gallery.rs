//! `type-sheet`: the `TypeSheet` board's specimens set in the real cuts, then
//! every rung of the type ladder with its resolved cut.

use crate::fonts::{self, Typeset};
use crate::gallery::Scene;
use crate::theme::{ActiveFacet, Facet};
use crate::tokens::{Face, Palette, Tone, TypeRole, ty};
use gpui::{
    AnyElement, AnyView, App, AppContext, Context, FontWeight, IntoElement, ParentElement, Render,
    SharedString, Styled, Window, div, px, relative,
};

pub(crate) const SCENES: &[Scene] = &[
    Scene {
        id: "type-sheet",
        title: "TypeSheet board specimens in the real cuts, then every type-ladder role",
        size: (1440, 3900),
        build: build_type_sheet,
    },
    Scene {
        id: "type-proof",
        title: "One specimen per cut on a 64 px grid, for pixel comparison with Chrome",
        size: (900, 14 * 64),
        build: |_, cx| cx.new(|_| TypeProof).into(),
    },
];

/// The proof rows: the same strings, sizes and tracking as
/// `resources/fonts/proof.html`, which Chrome renders from the variable
/// masters at the board's exact axes.
pub(crate) const PROOF_ROWS: [(&str, TypeRole, &str); 14] = [
    ("hero 640 wdth92 opsz96", ty::HERO, "RelationLabel"),
    ("display-xl 640 opsz96", ty::DISPLAY_XL, "Type sheet"),
    ("display 620 opsz96", ty::DISPLAY, "Colour with a job"),
    ("section 620 22px", SECTION, "Display: Bricolage Grotesque"),
    ("title 650 20px", ty::TITLE, "Relations and implementors"),
    ("book 640 wdth95", ty::BOOK, "serde_json"),
    (
        "geist 400",
        local(Face::Ui, 400.0, 15.0, 20.0, 0.0),
        "Remove serde from backend? 41,208",
    ),
    (
        "geist 500",
        local(Face::Ui, 500.0, 15.0, 20.0, 0.0),
        "Remove serde from backend? 41,208",
    ),
    (
        "geist 600",
        local(Face::Ui, 600.0, 15.0, 20.0, 0.0),
        "Remove serde from backend? 41,208",
    ),
    (
        "mono zero",
        local(Face::Mono, 400.0, 15.0, 20.0, 0.0),
        "pub const fn as_str(self) -> 1.0.193",
    ),
    (
        "mono 500",
        local(Face::Mono, 500.0, 15.0, 20.0, 0.0),
        "pub const fn as_str(self) -> 1.0.193",
    ),
    (
        "mono 600",
        local(Face::Mono, 600.0, 15.0, 20.0, 0.0),
        "pub const fn as_str(self) -> 1.0.193",
    ),
    (
        "serif italic",
        local(Face::Serif, 400.0, 19.0, 28.0, 0.0),
        "A generic serialization and deserialization framework.",
    ),
    (
        "serif upright",
        UPRIGHT,
        "A generic serialization and deserialization framework.",
    ),
];

const UPRIGHT: TypeRole = TypeRole {
    face: Face::Serif,
    weight: 400.0,
    size: 19.0,
    line: 28.0,
    tracking: 0.0,
    italic: false,
};

struct TypeProof;

impl Render for TypeProof {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let facet = cx.facet();
        div()
            .size_full()
            .bg(gpui::rgb(0x0006_0d1b))
            .flex()
            .flex_col()
            .children(PROOF_ROWS.map(|(_, role, text)| {
                div().h(px(64.0)).pl(px(24.0)).flex().items_center().child(
                    div()
                        .typeset(role, &facet)
                        .text_color(gpui::rgb(0x00ff_ffff))
                        .child(text),
                )
            }))
    }
}

fn build_type_sheet(_window: &mut Window, cx: &mut App) -> AnyView {
    cx.new(|_| TypeSheet).into()
}

struct TypeSheet;

/// Every rung of the ladder with a sample in its voice.
const LADDER: [(&str, TypeRole, &str); 22] = [
    ("DISPLAY_XL", ty::DISPLAY_XL, "Descent"),
    ("HERO", ty::HERO, "RelationLabel"),
    ("DISPLAY", ty::DISPLAY, "Colour with a job"),
    ("TITLE", ty::TITLE, "Relations"),
    ("BOOK", ty::BOOK, "serde"),
    ("DEPTH", ty::DEPTH, "Page"),
    ("HEAD", ty::HEAD, "Implementors"),
    ("DIALOG", ty::DIALOG, "Remove serde from backend?"),
    (
        "PROSE",
        ty::PROSE,
        "Serde is a framework for serializing and deserializing Rust data structures efficiently and generically.",
    ),
    ("BODY", ty::BODY, "3 depend on \u{b7} 41,208 depend on it"),
    (
        "ROW",
        ty::ROW,
        "Deserialize \u{b7} Serialize \u{b7} Serializer",
    ),
    ("BUTTON", ty::BUTTON, "Add package"),
    ("SMALL", ty::SMALL, "updated 3 days ago"),
    ("LABEL", ty::LABEL, "FILTER, OR FIND ANY PACKAGE"),
    ("STATUS", ty::STATUS, "hold \u{2318} for keys"),
    (
        "CODE",
        ty::CODE,
        "pub fn to_string<T: ?Sized + Serialize>(value: &T) -> Result<String>",
    ),
    ("MONO_ROW", ty::MONO_ROW, "serde_json::Value 0O1lI"),
    (
        "MONO_SMALL",
        ty::MONO_SMALL,
        "1.0.193 \u{b7} src/de/mod.rs:1204",
    ),
    (
        "LEDE",
        ty::LEDE,
        "A generic serialization and deserialization framework.",
    ),
    ("MARGIN", ty::MARGIN, "The doc follows the cursor."),
    (
        "CAPTION",
        ty::CAPTION,
        "The readable label of one relation group.",
    ),
    ("AXIS", ty::AXIS, "made of"),
];

/// A board-local role (the sheet's own `tsz-*` and meta styles).
const fn local(face: Face, weight: f32, size: f32, line: f32, tracking: f32) -> TypeRole {
    TypeRole {
        face,
        weight,
        size,
        line,
        tracking,
        italic: matches!(face, Face::Serif),
    }
}

const DOC_NO: TypeRole = local(Face::Mono, 400.0, 12.0, 16.0, 0.06);
const SECTION: TypeRole = local(Face::Display, 620.0, 22.0, 28.0, -0.02);
const VARIANT: TypeRole = local(Face::Display, 620.0, 16.0, 21.0, -0.012);
const SUB: TypeRole = local(Face::Serif, 400.0, 14.5, 21.0, 0.0);
const META: TypeRole = local(Face::Mono, 400.0, 10.5, 17.0, 0.0);
const SIZE_NOTE: TypeRole = local(Face::Mono, 400.0, 10.0, 10.0, 0.0);
const FIELD_LABEL: TypeRole = local(Face::Ui, 600.0, 10.5, 14.0, 0.02);
const CCAP: TypeRole = local(Face::Serif, 400.0, 12.5, 18.0, 0.0);
const MONO_15: TypeRole = local(Face::Mono, 400.0, 15.0, 20.0, 0.0);
const SERIF_KEY: TypeRole = local(Face::Mono, 400.0, 10.5, 10.5, 0.0);
const SERIF_CELL: TypeRole = local(Face::Serif, 400.0, 14.0, 21.0, 0.0);
const VERDICT: TypeRole = local(Face::Mono, 600.0, 10.5, 10.5, 0.0);
const BAD_LABEL: TypeRole = local(Face::Ui, 700.0, 9.5, 9.5, 0.08);
const DONT_SERIF: TypeRole = local(Face::Serif, 400.0, 17.0, 22.0, 0.0);
const TOO_SMALL: TypeRole = local(Face::Display, 620.0, 10.0, 10.0, -0.01);
const CCAP_SMALL: TypeRole = local(Face::Serif, 400.0, 11.5, 16.0, 0.0);
const KBD: TypeRole = local(Face::Ui, 600.0, 10.5, 10.5, 0.0);

fn set(role: TypeRole, facet: Facet, color: Tone, text: impl Into<SharedString>) -> gpui::Div {
    div()
        .typeset(role, &facet)
        .text_color(color.hsla())
        .child(text.into())
}

fn section(
    facet: Facet,
    palette: &Palette,
    ruled: bool,
    title: &'static str,
    sub: &'static str,
    body: impl IntoElement,
) -> gpui::Div {
    let mut section = div().flex().flex_col().gap(facet.px(16.0)).min_w_0();
    if ruled {
        section = section
            .border_t_1()
            .border_color(palette.line1.hsla())
            .pt(facet.px(28.0));
    }
    section
        .child(set(SECTION, facet, palette.ink0, title))
        .child(set(SUB, facet, palette.ink2, sub).max_w(facet.px(640.0)))
        .child(body)
}

fn spec_row(
    facet: Facet,
    palette: &Palette,
    role: TypeRole,
    sample: &'static str,
    meta: [String; 3],
) -> gpui::Div {
    div()
        .flex()
        .items_baseline()
        .gap(facet.px(24.0))
        .py(facet.px(16.0))
        .border_t_1()
        .border_color(palette.line1.hsla())
        .child(set(role, facet, palette.ink0, sample).flex_1().min_w_0())
        .child(
            div()
                .flex()
                .flex_col()
                .items_end()
                .flex_none()
                .children(meta.map(|line| set(META, facet, palette.ink3, line))),
        )
}

fn meta(role: TypeRole, label: &str) -> [String; 3] {
    [
        role.face.family().to_owned(),
        format!(
            "{} / {}px / {}em",
            role.weight,
            role.size,
            format_tracking(role.tracking)
        ),
        label.to_owned(),
    ]
}

fn format_tracking(tracking: f32) -> String {
    if tracking == 0.0 {
        "0".to_owned()
    } else {
        let text = format!("{tracking:.3}");
        text.trim_end_matches('0').to_owned()
    }
}

fn ui_cell(facet: Facet, palette: &Palette, role: TypeRole, sample: &str, note: &str) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap(facet.px(6.0))
        .items_start()
        .child(set(role, facet, palette.ink0, sample.to_owned()))
        .child(set(SIZE_NOTE, facet, palette.ink3, note.to_owned()))
}

fn serif_cell(facet: Facet, palette: &Palette, key: &str, body: impl IntoElement) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap(facet.px(8.0))
        .w(facet.px(260.0))
        .child(set(SERIF_KEY, facet, palette.peri_hi, key.to_owned()))
        .child(body)
}

fn verdict_cell(facet: Facet, palette: &Palette, yes: bool, body: Vec<AnyElement>) -> gpui::Div {
    let voice = if yes { palette.mint } else { palette.coral };
    div()
        .flex()
        .flex_col()
        .gap(facet.px(9.0))
        .px(facet.px(17.0))
        .py(facet.px(15.0))
        .bg(palette.plate)
        .border_1()
        .border_color((if yes { palette.line2 } else { voice.line }).hsla())
        .child(set(
            VERDICT,
            facet,
            voice.base,
            if yes { "\u{2713} do" } else { "\u{2715} don't" },
        ))
        .children(body)
}

fn kbd(facet: Facet, palette: &Palette, key: &str) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .justify_center()
        .min_w(facet.px(18.0))
        .h(facet.px(18.0))
        .px(facet.px(5.0))
        .rounded(facet.px(5.0))
        .bg(palette.plate3)
        .border_1()
        .border_color(palette.line2.hsla())
        .typeset(KBD, &facet)
        .text_color(palette.ink2.hsla())
        .child(key.to_owned())
}

impl TypeSheet {
    fn head(facet: Facet, palette: &Palette) -> gpui::Div {
        div()
            .flex()
            .flex_col()
            .gap(facet.px(8.0))
            .child(set(DOC_NO, facet, palette.mint.base, "FACET 13"))
            .child(set(ty::DISPLAY_XL, facet, palette.ink0, "Type sheet"))
            .child(
                set(
                    ty::LEDE,
                    facet,
                    palette.ink2,
                    "Four faces, one job each: Bricolage names things, Geist frames them, \
                     Geist Mono is the only font allowed to spell an identifier, and \
                     Newsreader italic is the only voice that explains rather than labels.",
                )
                .max_w(facet.px(760.0)),
            )
    }

    fn display(facet: Facet, palette: &Palette) -> gpui::Div {
        section(
            facet,
            palette,
            false,
            "Display: Bricolage Grotesque",
            "Names and headings, never smaller than 14px. Tight tracking so a long symbol \
             name still reads as one word.",
            div()
                .flex()
                .flex_col()
                .gap(facet.px(16.0))
                .child(spec_row(
                    facet,
                    palette,
                    ty::HERO,
                    "RelationLabel",
                    meta(ty::HERO, "hero-name"),
                ))
                .child(spec_row(
                    facet,
                    palette,
                    SECTION,
                    "Made of",
                    meta(SECTION, "section head"),
                ))
                .child(spec_row(
                    facet,
                    palette,
                    VARIANT,
                    "Typed (SemanticLinkKind, RelationDirection)",
                    meta(VARIANT, "variant head"),
                )),
        )
    }

    fn ui(facet: Facet, palette: &Palette) -> gpui::Div {
        section(
            facet,
            palette,
            true,
            "UI: Geist",
            "Everything that is chrome rather than content, labels, buttons, prose, in a \
             plain, quiet weight.",
            div()
                .flex()
                .flex_wrap()
                .items_start()
                .gap(facet.px(24.0))
                .pt(facet.px(6.0))
                .child(ui_cell(
                    facet,
                    palette,
                    FIELD_LABEL,
                    "Filter, or find any package",
                    "10.5 / 600 \u{b7} field label",
                ))
                .child(ui_cell(
                    facet,
                    palette,
                    ty::BUTTON,
                    "Add package",
                    "12 / 500 \u{b7} button",
                ))
                .child(ui_cell(
                    facet,
                    palette,
                    local(Face::Ui, 400.0, 13.0, 18.0, 0.0),
                    "3 depend on \u{b7} 41,208 depend on it",
                    "13 / 400 \u{b7} body",
                ))
                .child(ui_cell(
                    facet,
                    palette,
                    ty::DIALOG,
                    "Remove serde from backend?",
                    "15 / 500 \u{b7} dialog title",
                )),
        )
    }

    /// The board's code block: `.code` lines with 4ch indents, and a caption.
    fn code_block(facet: Facet, palette: &Palette) -> gpui::Div {
        let line = |indent: f32, text: &'static str| {
            div()
                .flex()
                .child(div().w(facet.px(ty::CODE.size * 0.6 * indent)))
                .child(text)
        };
        let block = div()
            .flex()
            .flex_col()
            .typeset(ty::CODE, &facet)
            .text_color(palette.ink1.hsla())
            .child(line(0.0, "pub const fn as_str(self) -> &'static str {"))
            .child(line(4.0, "match self {"))
            .child(line(
                8.0,
                "Self::Typed(kind, dir) => typed_words(kind, dir),",
            ))
            .child(line(4.0, "}"))
            .child(line(0.0, "}"));
        div()
            .flex()
            .flex_col()
            .gap(facet.px(8.0))
            .child(block)
            .child(set(
                CCAP,
                facet,
                palette.ink3,
                "Geist Mono, 12.5px / 400, tabular figures on. The only font that ever spells an \
             identifier.",
            ))
    }

    /// A path and a version, each one mono run.
    fn code_runs(facet: Facet, palette: &Palette) -> gpui::Div {
        let version = div()
            .flex()
            .typeset(MONO_15, &facet)
            .text_color(palette.ink0.hsla())
            .child("1.0.")
            .child(div().text_color(palette.mint.base.hsla()).child("193"));
        div()
            .flex()
            .flex_col()
            .gap(facet.px(8.0))
            .child(set(MONO_15, facet, palette.ink0, "RelationLabel::Typed"))
            .child(version)
            .child(set(
                CCAP,
                facet,
                palette.ink3,
                "Paths and versions read as one mono run, no separators change font.",
            ))
    }

    fn code(facet: Facet, palette: &Palette) -> gpui::Div {
        section(
            facet,
            palette,
            true,
            "Code: Geist Mono",
            "Every identifier, path, and version, always. It is how you tell a fact from a \
             sentence at a glance.",
            div()
                .flex()
                .flex_wrap()
                .items_start()
                .gap(facet.px(20.0))
                .child(Self::code_block(facet, palette))
                .child(Self::code_runs(facet, palette)),
        )
    }

    fn serif(facet: Facet, palette: &Palette) -> gpui::Div {
        let body = |text: &'static str| set(SERIF_CELL, facet, palette.ink1, text);
        section(
            facet,
            palette,
            true,
            "Serif: Newsreader, always italic",
            "Ledes, captions, margin notes, quiet hints, the voice that explains rather than labels.",
            div()
                .flex()
                .flex_wrap()
                .gap(facet.px(30.0))
                .pt(facet.px(4.0))
                .child(serif_cell(facet, palette, "lede", body("A generic serialization and deserialization framework.")))
                .child(serif_cell(facet, palette, "caption", body("The readable label of one relation group.")))
                .child(serif_cell(
                    facet,
                    palette,
                    "margin note",
                    body("The doc follows the cursor. On a Page, source peels up from under the doc."),
                ))
                .child(serif_cell(
                    facet,
                    palette,
                    "quiet hint",
                    div()
                        .flex()
                        .items_center()
                        .gap(facet.px(4.0))
                        .typeset(SERIF_CELL, &facet)
                        .text_color(palette.ink1.hsla())
                        .child("hold")
                        .child(kbd(facet, palette, "\u{2318}"))
                        .child("for keys"),
                )),
        )
    }

    /// The modifier rule's two bodies: marks with a tooltip, uppercase labels.
    fn modifier_rule(facet: Facet, palette: &Palette) -> (AnyElement, AnyElement) {
        let mark = || {
            div()
                .size(facet.px(13.0))
                .border_1()
                .border_color(palette.ink3.hsla())
                .rounded(facet.px(3.0))
        };
        let label = |text: &'static str| {
            div()
                .flex()
                .items_center()
                .h(facet.px(16.0))
                .px(facet.px(7.0))
                .rounded(facet.px(3.0))
                .bg(palette.coral.soft)
                .typeset(BAD_LABEL, &facet)
                .text_color(palette.coral.base.hsla())
                .child(text)
        };
        let marks = div()
            .flex()
            .items_center()
            .gap(facet.px(8.0))
            .child(mark())
            .child(mark())
            .child(mark().border_color(palette.amber.base.hsla()))
            .child(set(
                ty::SMALL,
                facet,
                palette.ink2,
                "a mark, with a tooltip",
            ));
        let labels = div()
            .flex()
            .gap(facet.px(8.0))
            .child(label("CONST"))
            .child(label("ASYNC"))
            .child(label("UNSAFE"));
        (marks.into_any_element(), labels.into_any_element())
    }

    fn rules(facet: Facet, palette: &Palette) -> gpui::Div {
        let pair = |good: Vec<AnyElement>, bad: Vec<AnyElement>| {
            div()
                .flex()
                .gap(facet.px(28.0))
                .child(verdict_cell(facet, palette, true, good).flex_1())
                .child(verdict_cell(facet, palette, false, bad).flex_1())
        };
        let (marks, labels) = Self::modifier_rule(facet, palette);
        let any = |element: gpui::Div| element.into_any_element();
        section(
            facet,
            palette,
            true,
            "Do and don't",
            "Three rules that hold everywhere else on this page.",
            div()
                .flex()
                .flex_col()
                .gap(facet.px(14.0))
                .pt(facet.px(6.0))
                .child(pair(vec![marks], vec![labels]))
                .child(pair(
                    vec![any(set(MONO_15, facet, palette.ink0, "RelationLabel"))],
                    vec![any(set(DONT_SERIF, facet, palette.ink1, "RelationLabel"))],
                ))
                .child(pair(
                    vec![
                        any(set(VARIANT, facet, palette.ink0, "Neighbourhood")),
                        any(set(
                            CCAP_SMALL,
                            facet,
                            palette.ink3,
                            "16px, the display font's floor",
                        )),
                    ],
                    vec![
                        any(set(TOO_SMALL, facet, palette.ink1, "Neighbourhood")),
                        any(set(
                            CCAP_SMALL,
                            facet,
                            palette.ink3,
                            "10px: the grotesque goes thin and cramped",
                        )),
                    ],
                )),
        )
    }

    fn ladder(facet: Facet, palette: &Palette) -> gpui::Div {
        section(
            facet,
            palette,
            true,
            "Ladder: every role",
            "Each token role in the cut it resolves to, at the active text scale.",
            div()
                .flex()
                .flex_col()
                .gap(facet.px(16.0))
                .children(LADDER.map(|(name, role, sample)| {
                    let cut = fonts::cut(role);
                    spec_row(
                        facet,
                        palette,
                        role,
                        sample,
                        [
                            format!("{} {}", cut.family, if cut.italic { "italic" } else { "" })
                                .trim_end()
                                .to_owned(),
                            format!(
                                "{} / {}px / {}px / {}em",
                                role.weight,
                                role.size,
                                role.line,
                                format_tracking(role.tracking)
                            ),
                            format!("ty::{name}"),
                        ],
                    )
                })),
        )
    }
}

impl Render for TypeSheet {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let facet = cx.facet();
        let palette = facet.palette();
        div()
            .size_full()
            .bg(palette.g1)
            .text_color(palette.ink1.hsla())
            .font_weight(FontWeight::NORMAL)
            .child(
                div()
                    .w(relative(1.0))
                    .px(facet.px(64.0))
                    .py(facet.px(56.0))
                    .flex()
                    .flex_col()
                    .gap(facet.px(36.0))
                    .child(Self::head(facet, palette))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(34.0 * facet.text_scale))
                            .child(Self::display(facet, palette))
                            .child(Self::ui(facet, palette))
                            .child(Self::code(facet, palette))
                            .child(Self::serif(facet, palette))
                            .child(Self::rules(facet, palette))
                            .child(Self::ladder(facet, palette)),
                    ),
            )
    }
}
