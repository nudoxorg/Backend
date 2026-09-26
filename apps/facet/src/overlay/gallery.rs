//! Overlay scenes. States are driven from scene code (scripted steps on the
//! executor clock) until the harness's scripted input lands.
//!
//! - `peeks`: the calm Peeks board — a page fragment with the peek open on
//!   a word and a child chained beside it (the real float layer), then the
//!   ⌘ and ⌥ states, the pinned column, and the package / location /
//!   version peeks. `peeks-narrow`: the same stage at 480 (bottom sheet).
//! - `float-lab`: the layer with pins, a chain and a tip at once.
//! - `float-rise`, `float-sweep`, `float-chain`, `float-back`, `float-pin`,
//!   `float-reverse`, `float-tip`, `float-deepen`: one motion each, for films.

#![allow(clippy::too_many_lines)]

use super::float::{self, FloatKind, FloatRequest, Side};
use super::peek::{self, ExcerptLine, LocationPeek, PackagePeek, Peek, SymbolPeek, VersionPeek};
use super::text::{Link, Role, Sig};
use super::tooltip;
use crate::data::{Directions, FileUses};
use crate::gallery::Scene;
use crate::icons::{Kind, Lang};
use crate::measure::{Measure, Reveal, Set};
use crate::paint::ground;
use crate::theme::{ActiveFacet, Facet};
use crate::tokens::{Face, TypeRole};
use gpui::{
    AnyElement, AnyView, App, AppContext, Bounds, Context, ElementId, IntoElement, ParentElement,
    Pixels, Render, SharedString, Styled, Window, div, point, px, size,
};
use std::time::Duration;

pub(crate) const SCENES: &[Scene] = &[
    Scene {
        id: "peeks",
        title: "Peeks (calm): at rest on a word with a chained child, ⌘ held, ⌥ held, pinned column, package / location / version",
        size: (1440, 1160),
        build: peeks_board,
    },
    Scene {
        id: "peeks-narrow",
        title: "Peeks at 480: the peek is a bottom sheet; the anchor stays lit",
        size: (480, 760),
        build: peeks_narrow,
    },
    Scene {
        id: "float-lab",
        title: "Float layer: pins, a chain with a child, a tip",
        size: (1440, 900),
        build: lab_still,
    },
    Scene {
        id: "float-edges",
        title: "Peeks above/below two anchors pinned to the window's top and bottom edges: both centre on the anchor and flip to the roomier side, with a hairline to the anchor. A tip on a third anchor draws no connector.",
        size: (700, 460),
        build: float_edges,
    },
    Scene {
        id: "float-edges-sides",
        title: "Peeks below two anchors pinned to the window's left and right edges: both shift along the edge to stay 8 px inside instead of centring off-screen.",
        size: (700, 460),
        build: float_edges_sides,
    },
    Scene {
        id: "float-rise",
        title: "Film: a peek opens at 1 ms and rises",
        size: (900, 520),
        build: film_rise,
    },
    Scene {
        id: "float-sweep",
        title: "Film: warm sweep at 400 ms, the card morphs to the next word",
        size: (900, 520),
        build: film_sweep,
    },
    Scene {
        id: "float-chain",
        title: "Film: a child chains beside its parent at 100 ms",
        size: (1100, 560),
        build: film_chain,
    },
    Scene {
        id: "float-back",
        title: "Film: Esc steps back one at 100 ms",
        size: (1100, 560),
        build: film_back,
    },
    Scene {
        id: "float-pin",
        title: "Film: Space pins at 100 ms; the card leaves, its row joins the column",
        size: (1300, 560),
        build: film_pin,
    },
    Scene {
        id: "float-reverse",
        title: "Film: closed at 10 ms, asked for again at 80 ms, reverses from where it was",
        size: (900, 520),
        build: film_reverse,
    },
    Scene {
        id: "float-tip",
        title: "Film: a tip opens at 1 ms and closes at 300 ms",
        size: (700, 300),
        build: film_tip,
    },
    Scene {
        id: "float-deepen",
        title: "Film: resting still on an open peek deepens it at 600 ms (compass bar, file comb)",
        size: (900, 640),
        build: film_deepen,
    },
];

// ------------------------------------------------------------------ data

fn key(name: &str) -> ElementId {
    ElementId::Name(SharedString::from(format!("gallery:{name}")))
}

fn method_call() -> Peek {
    Peek::Symbol(SymbolPeek {
        kind: Some(Kind::Variant),
        name: "MethodCall".into(),
        place: "variant, no payload".into(),
        path: "present::relation".into(),
        sentence: Some("A call through a receiver, `value.method()`, resolved by the compiler.".into()),
        ..SymbolPeek::default()
    })
}

fn type_reference() -> Peek {
    Peek::Symbol(SymbolPeek {
        kind: Some(Kind::Variant),
        name: "TypeReference".into(),
        place: "variant, no payload".into(),
        path: "present::relation".into(),
        sentence: Some("A type named in a signature, a field or a bound.".into()),
        ..SymbolPeek::default()
    })
}

/// A word inside a signature that rests into a child peek.
fn child_link(owner: &str, peek: Peek) -> Link {
    let key = key(&format!("{owner}:{}", peek.title()));
    Link::new(key.clone(), move |rect| {
        peek::request(key.clone(), rect, peek.clone()).side(Side::Right)
    })
}

fn semantic_link_kind() -> Peek {
    Peek::Symbol(SymbolPeek {
        kind: Some(Kind::Enum),
        name: "SemanticLinkKind".into(),
        place: "enum in `present::relation`".into(),
        path: "present::relation".into(),
        signature: Some(
            Sig::new()
                .kw("pub enum")
                .sp()
                .ty("SemanticLinkKind")
                .sp()
                .p("{")
                .sp()
                .val("Calls")
                .p(",")
                .sp()
                .link("MethodCall", Role::Value, child_link("SemanticLinkKind", method_call()))
                .p(",")
                .sp()
                .link("TypeReference", Role::Value, child_link("SemanticLinkKind", type_reference()))
                .p(", … }"),
        ),
        sentence: Some("The closed vocabulary of how one symbol touches another.".into()),
        uses: Some(14),
        files: Some(5),
        compass: Some(Directions {
            is: 5,
            made_of: 11,
            from: 26,
            to: 0,
        }),
        file_uses: vec![
            FileUses::new("page.rs", vec![9.0, 12.0, 8.0, 10.0, 7.0, 11.0]).hot(),
            FileUses::new("relation.rs", vec![8.0, 11.0, 7.0]),
            FileUses::new("rose.rs", vec![9.0, 6.0]),
            FileUses::new("ledger.rs", vec![7.0, 10.0]),
            FileUses::new("walk.rs", vec![7.0]).theirs(),
        ],
    })
}

fn relation_direction() -> Peek {
    Peek::Symbol(SymbolPeek {
        kind: Some(Kind::Enum),
        name: "RelationDirection".into(),
        place: "enum in `present::relation`".into(),
        path: "present::relation".into(),
        signature: Some(
            Sig::new()
                .kw("pub enum")
                .sp()
                .ty("RelationDirection")
                .sp()
                .p("{")
                .sp()
                .val("Forward")
                .p(",")
                .sp()
                .val("Backward")
                .sp()
                .p("}"),
        ),
        sentence: Some("Which way an edge points, from the page's side.".into()),
        uses: Some(9),
        ..SymbolPeek::default()
    })
}

fn page_relations() -> Peek {
    Peek::Symbol(SymbolPeek {
        kind: Some(Kind::Method),
        name: "Page::relations".into(),
        place: "method in `present::page`".into(),
        path: "present::page".into(),
        sentence: Some("Every relation group of a page, in rose order.".into()),
        ..SymbolPeek::default()
    })
}

fn serde() -> Peek {
    Peek::Package(PackagePeek {
        name: "serde".into(),
        lang: Some(Lang::Rust),
        version: "1.0.193".into(),
        registry: "crates.io".into(),
        sentence: Some("A generic serialization and deserialization framework.".into()),
        reach: Some(31),
    })
}

fn glyph_rs() -> Peek {
    let line = |number: usize, code: Sig, lit: bool| ExcerptLine { number, code, lit };
    Peek::Location(LocationPeek {
        name: "glyph.rs".into(),
        path: "present/src/glyph.rs".into(),
        line: 138,
        excerpt: vec![
            line(137, Sig::new(), false),
            line(
                138,
                Sig::new().span("#[derive(Clone, Copy, Debug, Eq, Hash)]", Role::Punct),
                true,
            ),
            line(139, Sig::new().kw("pub enum").sp().ty("RelationLabel").sp().p("{"), true),
            line(140, Sig::new().sp().sp().sp().sp().val("Typed").p("(…),"), false),
            line(141, Sig::new().sp().sp().sp().sp().val("Neighbourhood").p(","), false),
        ],
    })
}

fn release() -> Peek {
    Peek::Version(VersionPeek {
        version: "1.0.200".into(),
        when: "3 weeks ago".into(),
        yours: Some(("from_str".into(), 2)),
        added: 3,
        changed: 5,
    })
}

// ------------------------------------------------------------------ type

const fn role(face: Face, weight: f32, size: f32, line: f32, tracking: f32) -> TypeRole {
    TypeRole {
        face,
        weight,
        size,
        line,
        tracking,
        italic: matches!(face, Face::Serif),
    }
}

const H1: TypeRole = role(Face::Display, 720.0, 34.0, 34.0, -0.03);
const H2: TypeRole = role(Face::Display, 620.0, 19.0, 23.0, -0.02);
const LABEL: TypeRole = role(Face::Ui, 500.0, 11.0, 14.0, 0.0);
const ROW_NAME: TypeRole = role(Face::Mono, 500.0, 13.0, 20.0, 0.0);
const ROW_SAY: TypeRole = role(Face::Serif, 400.0, 14.0, 20.0, 0.0);

// ------------------------------------------------------------------ stage

type Step = Box<dyn FnOnce(&mut Window, &mut App)>;

/// The page fragment: "Made of" and three rows; the first row's name is
/// one text with two rest-able words.
fn fragment(measure: &Measure, hovered: bool, cx: &App) -> AnyElement {
    let palette = cx.facet().palette();
    let word = |name: &str, peek: fn() -> Peek| {
        let key = key(&format!("page:{name}"));
        Link::new(key.clone(), move |rect| peek::request(key.clone(), rect, peek()))
    };
    let name = Sig::new()
        .span("Typed", Role::Strong)
        .p("(")
        .link("SemanticLinkKind", Role::Strong, word("SemanticLinkKind", semantic_link_kind))
        .span(", ", Role::Quiet)
        .link("RelationDirection", Role::Quiet, word("RelationDirection", relation_direction))
        .p(")");
    let kind = |kind: Kind| crate::icons::kind_mark(kind, crate::icons::KindSize::Sm, palette);
    let scale = measure.scale();
    let row = |mark: AnyElement, name: AnyElement, say: Option<&'static str>, lit: bool| {
        let mut row = div()
            .flex()
            .items_center()
            .gap(px(12.0) * scale)
            .px(px(10.0) * scale)
            .mx(-px(10.0) * scale)
            .min_h(px(34.0) * scale)
            .child(mark)
            .child(div().flex_none().child(name));
        if let Some(say) = say {
            row = row.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .set(ROW_SAY, measure)
                    .text_color(palette.ink3.hsla())
                    .child(say),
            );
        }
        if lit {
            row = row.bg(palette.peri.base.alpha(0.06).hsla());
        }
        row
    };
    let plain = |text: &'static str| {
        div()
            .set(ROW_NAME, measure)
            .text_color(palette.ink0.hsla())
            .child(text)
            .into_any_element()
    };
    div()
        .flex()
        .flex_col()
        .gap(px(6.0) * scale)
        .child(
            div()
                .set(H2, measure)
                .text_color(palette.ink0.hsla())
                .mb(px(6.0) * scale)
                .child("Made of"),
        )
        .child(row(
            kind(Kind::Variant),
            name.render(key("page:typed"), ROW_NAME, measure, palette),
            None,
            hovered,
        ))
        .child(row(kind(Kind::Variant), plain("Neighbourhood"), Some("A bounded neighbourhood."), false))
        .child(row(kind(Kind::Variant), plain("Related"), Some("Related, and nothing more."), false))
        .into_any_element()
}

struct Stage {
    /// Draw the calm board around the stage.
    board: bool,
    /// Draw the pinned column at the right edge (lab).
    pins: bool,
}

impl Render for Stage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let facet = cx.facet();
        let palette = facet.palette();
        let viewport = window.viewport_size();
        let measure = facet.measure(viewport.width);
        let hovered = float::is_open(&key("page:SemanticLinkKind"), window, cx)
            || float::is_open(&key("page:RelationDirection"), window, cx);
        let scale = facet.text_scale;
        let frag_width = (px(640.0) * scale).min(viewport.width - px(32.0));
        let frag_measure = measure.within(frag_width);
        let frag = div()
            .w(frag_width)
            .pt(px(18.0) * scale)
            .px(px(22.0) * scale)
            .pb(px(22.0) * scale)
            .bg(palette.g1.alpha(0.55).hsla())
            .child(fragment(&frag_measure.inset(px(22.0) * scale), hovered, cx));
        let mut root = div()
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(palette.g0.hsla())
            .child(ground());
        if self.board {
            root = root.child(board(frag, &measure, window, cx));
        } else {
            let pad = px(if viewport.width < px(600.0) { 16.0 } else { 40.0 });
            root = root.child(div().absolute().left(pad).top(px(34.0)).child(frag));
        }
        if self.pins {
            let column = measure.within(px(252.0));
            root = root.child(
                div()
                    .absolute()
                    .top_0()
                    .right_0()
                    .bottom_0()
                    .w(px(280.0))
                    .py(px(16.0))
                    .px(px(14.0))
                    .border_l_1()
                    .border_color(palette.line1.hsla())
                    .bg(palette.g0.alpha(0.45).hsla())
                    .child(float::pinned_column(&column, window, cx)),
            );
        }
        root.child(float::layer(window, cx))
    }
}

/// A card shown in place (not floated), at a reveal state.
fn still(peek: &Peek, reveal: Reveal, window: &mut Window, cx: &mut App) -> AnyElement {
    let facet = Facet {
        reveal,
        ..cx.facet()
    };
    let palette = facet.palette();
    let measure = Measure::new(px(392.0 * facet.text_scale), &facet);
    float::plate(FloatKind::Peek, false, palette)
        .child(peek::card(peek, &measure, window, cx))
        .into_any_element()
}

fn labelled(label: &'static str, child: AnyElement, measure: &Measure, cx: &App) -> AnyElement {
    let palette = cx.facet().palette();
    div()
        .flex()
        .flex_col()
        .child(
            div()
                .set(LABEL, measure)
                .text_color(palette.ink4.hsla())
                .mb(px(10.0) * measure.scale())
                .child(label),
        )
        .child(child)
        .into_any_element()
}

fn board(frag: gpui::Div, measure: &Measure, window: &mut Window, cx: &mut App) -> AnyElement {
    let palette = cx.facet().palette();
    let scale = measure.scale();
    let keys = still(&semantic_link_kind(), Reveal { keys: true, xray: false }, window, cx);
    let xray = still(&semantic_link_kind(), Reveal { keys: true, xray: true }, window, cx);
    let pins = div()
        .w(px(280.0) * scale)
        .py(px(16.0) * scale)
        .px(px(14.0) * scale)
        .border_l_1()
        .border_color(palette.line1.hsla())
        .bg(palette.g0.alpha(0.45).hsla())
        .child(float::pinned_column(&measure.within(px(252.0) * scale), window, cx))
        .into_any_element();
    let package = still(&serde(), Reveal::default(), window, cx);
    let location = still(&glyph_rs(), Reveal::default(), window, cx);
    let version = still(&release(), Reveal::default(), window, cx);
    let row = |children: Vec<AnyElement>| {
        div()
            .flex()
            .flex_wrap()
            .items_start()
            .gap(px(28.0) * scale)
            .children(children)
    };
    div()
        .relative()
        .flex()
        .flex_col()
        .gap(px(34.0) * scale)
        .py(px(44.0) * scale)
        .px(px(56.0) * scale)
        .child(div().set(H1, measure).text_color(palette.ink0.hsla()).child("Peeks"))
        .child(labelled(
            "Resting on a word",
            div().h(px(330.0) * scale).child(frag).into_any_element(),
            measure,
            cx,
        ))
        .child(row(vec![
            labelled("⌘ held", keys, measure, cx),
            labelled("⌥ held, or rest again", xray, measure, cx),
            labelled("Pinned, at 1 900 px and wider", pins, measure, cx),
        ]))
        .child(row(vec![
            labelled("A package", package, measure, cx),
            labelled("A location", location, measure, cx),
            labelled("A version", version, measure, cx),
        ]))
        .into_any_element()
}

fn stage(board: bool, pins: bool, steps: Vec<(u64, Step)>, window: &mut Window, cx: &mut App) -> AnyView {
    let view = cx.new(|_| Stage { board, pins });
    script(steps, window, cx);
    view.into()
}

/// Runs each step at its virtual time (ms after the scene's first frame).
fn script(steps: Vec<(u64, Step)>, window: &mut Window, cx: &mut App) {
    for (at, step) in steps {
        window
            .spawn(cx, async move |cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(at))
                    .await;
                let _ = cx.update(|window, cx| step(window, cx));
            })
            .detach();
    }
}

fn anchor_of(key: &ElementId, window: &Window, cx: &mut App) -> Bounds<Pixels> {
    float::reported(key, window, cx).unwrap_or_default()
}

fn open(key_name: &'static str, peek: fn() -> Peek, side: Side) -> Step {
    Box::new(move |window, cx| {
        let key = key(key_name);
        let anchor = anchor_of(&key, window, cx);
        float::open(peek::request(key, anchor, peek()).side(side), window, cx);
    })
}

fn rest(key_name: &'static str, peek: fn() -> Peek, side: Side) -> Step {
    Box::new(move |window, cx| {
        let key = key(key_name);
        let anchor = anchor_of(&key, window, cx);
        float::rest(peek::request(key, anchor, peek()).side(side), window, cx);
    })
}

fn leave(key_name: &'static str) -> Step {
    Box::new(move |window, cx| float::leave(&key(key_name), window, cx))
}

fn pin(name: &'static str, peek: fn() -> Peek) -> Step {
    Box::new(move |window, cx| {
        let anchor = Bounds::new(gpui::point(px(-100.0), px(-100.0)), gpui::size(px(1.0), px(1.0)));
        float::open(peek::request(key(&format!("pin:{name}")), anchor, peek()), window, cx);
        float::pin_top(window, cx);
    })
}

fn settle() -> Step {
    Box::new(|window, cx| float::settle_now(window, cx))
}

fn step(f: impl FnOnce(&mut Window, &mut App) + 'static) -> Step {
    Box::new(f)
}

fn tip_request(window: &Window, cx: &mut App) -> FloatRequest {
    let anchor = anchor_of(&key("page:SemanticLinkKind"), window, cx);
    FloatRequest::new(key("tip"), anchor, FloatKind::Tip, |measure, _window, cx| {
        let palette = cx.facet().palette();
        div()
            .px(px(10.0) * measure.scale())
            .py(px(6.0) * measure.scale())
            .set(crate::tokens::ty::SMALL, measure)
            .text_color(palette.ink1.hsla())
            .child("14 uses in 5 files")
            .into_any_element()
    })
}

// ------------------------------------------------------------------ scenes

/// The chain every stage opens: the peek on the page word, then its child
/// on the word inside the peek.
fn chain() -> Vec<(u64, Step)> {
    vec![
        (1, open("page:SemanticLinkKind", semantic_link_kind, Side::Below)),
        (2, settle()),
        (3, open("SemanticLinkKind:MethodCall", method_call, Side::Right)),
        (4, settle()),
    ]
}

fn peeks_board(window: &mut Window, cx: &mut App) -> AnyView {
    let mut steps = vec![
        (1, pin("page-relations", page_relations)),
        (2, pin("semantic", semantic_link_kind)),
    ];
    steps.extend(chain().into_iter().map(|(at, step)| (at + 2, step)));
    stage(true, false, steps, window, cx)
}

fn peeks_narrow(window: &mut Window, cx: &mut App) -> AnyView {
    stage(
        false,
        false,
        vec![
            (1, open("page:SemanticLinkKind", semantic_link_kind, Side::Below)),
            (2, settle()),
        ],
        window,
        cx,
    )
}

fn lab_still(window: &mut Window, cx: &mut App) -> AnyView {
    let mut steps = vec![
        (1, pin("page-relations", page_relations)),
        (2, pin("serde", serde)),
    ];
    steps.extend(chain().into_iter().map(|(at, step)| (at + 2, step)));
    steps.push((
        8,
        step(|window, cx| {
            let request = tip_request(window, cx);
            float::open(request, window, cx);
        }),
    ));
    steps.push((9, settle()));
    stage(false, true, steps, window, cx)
}

/// A blank stage with small anchor marks pinned to the window's edges: proof
/// that above/below peeks centre on the anchor and flip to the roomier side
/// (top/bottom, `top_bottom_tip: true`) or shift along the edge to stay 8 px
/// inside instead of centring off-screen (left/right, `false`), that a peek
/// draws a hairline connector, and that a tip does not. Split across two
/// scenes (`float-edges`, `float-edges-sides`) rather than one: every
/// leveled kind shares one root slot (`model.rs::show`'s "closes every
/// other open card at that level or deeper"), so a second, third, fourth
/// anchor opened here always *swaps* the same reused card rather than
/// opening beside it. That swap glides on a real, elapsed-time spring
/// (`float.rs::prepaint`, `spec::FOLLOW`; `settle_now` only finishes the
/// open/close presence, not this), and this harness's own script timer
/// turned out not to be safely reproducible once a scene's total virtual
/// span ran past several hundred ms in testing here (identical `--time`s
/// read as different amounts of progress between runs). One swap per scene
/// (baseline anchor, then the one being proven) stays well inside the
/// range that read back identically on every retry.
struct Edges {
    top_bottom_tip: bool,
}

impl Render for Edges {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = cx.facet().palette();
        let mark = |x: f32, y: f32| {
            div()
                .absolute()
                .left(px(x))
                .top(px(y))
                .w(px(40.0))
                .h(px(14.0))
                .bg(palette.peri.base.hsla())
        };
        let stage = div().relative().size_full().bg(palette.g0.hsla()).child(ground());
        let stage = if self.top_bottom_tip {
            stage.child(mark(330.0, 6.0)).child(mark(330.0, 440.0)).child(mark(650.0, 6.0))
        } else {
            stage.child(mark(6.0, 220.0)).child(mark(654.0, 220.0))
        };
        stage.child(float::layer(window, cx))
    }
}

fn edge_peek(name: &'static str) -> Peek {
    Peek::Symbol(SymbolPeek {
        kind: Some(Kind::Enum),
        name: name.into(),
        place: "enum in `present::relation`".into(),
        path: "present::relation".into(),
        sentence: Some("Placement proof near a window edge.".into()),
        uses: Some(3),
        ..SymbolPeek::default()
    })
}

fn open_peek_at(name: &'static str, bounds: Bounds<Pixels>, side: Side) -> Step {
    Box::new(move |window, cx| {
        float::open(peek::request(key(name), bounds, edge_peek(name)).side(side), window, cx);
    })
}

fn open_tip_at(name: &'static str, bounds: Bounds<Pixels>, side: Side) -> Step {
    Box::new(move |window, cx| {
        let content = tooltip::content(tooltip::TipText {
            title: None,
            body: "No connector on a tip.".into(),
            chord: vec![],
        });
        float::open(FloatRequest::new(key(name), bounds, FloatKind::Tip, content).side(side), window, cx);
    })
}

fn float_edges(window: &mut Window, cx: &mut App) -> AnyView {
    let view = cx.new(|_| Edges { top_bottom_tip: true });
    let b = |x: f32, y: f32| Bounds::new(point(px(x), px(y)), size(px(40.0), px(14.0)));
    // `edge-top` and the tip are both fresh cards (nothing else is open
    // yet), so they paint at their true target immediately — no spring to
    // wait on. `edge-bottom` reuses that same card (the single root Peek
    // slot), so it gets a full 300 ms of settled dt before its own read.
    script(
        vec![
            (1, open_peek_at("edge-top", b(330.0, 6.0), Side::Above)),
            (1, open_tip_at("edge-tip", b(650.0, 6.0), Side::Below)),
            (50, settle()),
            (100, open_peek_at("edge-bottom", b(330.0, 440.0), Side::Below)),
            (400, settle()),
        ],
        window,
        cx,
    );
    view.into()
}

fn float_edges_sides(window: &mut Window, cx: &mut App) -> AnyView {
    let view = cx.new(|_| Edges { top_bottom_tip: false });
    let b = |x: f32, y: f32| Bounds::new(point(px(x), px(y)), size(px(40.0), px(14.0)));
    // Same reasoning as `float_edges`: `edge-left` is the first (and so
    // fresh) card in this scene, `edge-right` is a swap onto it and gets
    // 300 ms of settled dt before its own read.
    script(
        vec![
            (1, open_peek_at("edge-left", b(6.0, 220.0), Side::Below)),
            (50, settle()),
            (100, open_peek_at("edge-right", b(654.0, 220.0), Side::Below)),
            (400, settle()),
        ],
        window,
        cx,
    );
    view.into()
}

fn film_rise(window: &mut Window, cx: &mut App) -> AnyView {
    stage(false, false, vec![(1, open("page:SemanticLinkKind", semantic_link_kind, Side::Below))], window, cx)
}

fn film_sweep(window: &mut Window, cx: &mut App) -> AnyView {
    stage(
        false,
        false,
        vec![
            (1, rest("page:SemanticLinkKind", semantic_link_kind, Side::Below)),
            (360, settle()),
            (400, leave("page:SemanticLinkKind")),
            (400, rest("page:RelationDirection", relation_direction, Side::Below)),
        ],
        window,
        cx,
    )
}

fn film_chain(window: &mut Window, cx: &mut App) -> AnyView {
    stage(
        false,
        false,
        vec![
            (1, open("page:SemanticLinkKind", semantic_link_kind, Side::Below)),
            (2, settle()),
            (100, open("SemanticLinkKind:MethodCall", method_call, Side::Right)),
        ],
        window,
        cx,
    )
}

fn film_back(window: &mut Window, cx: &mut App) -> AnyView {
    let mut steps = chain();
    steps.push((
        100,
        step(|window, cx| {
            float::step_back(window, cx);
        }),
    ));
    stage(false, false, steps, window, cx)
}

fn film_pin(window: &mut Window, cx: &mut App) -> AnyView {
    stage(
        false,
        true,
        vec![
            (1, pin("page-relations", page_relations)),
            (2, open("page:SemanticLinkKind", semantic_link_kind, Side::Below)),
            (3, settle()),
            (
                100,
                step(|window, cx| {
                    float::pin_top(window, cx);
                }),
            ),
        ],
        window,
        cx,
    )
}

fn film_reverse(window: &mut Window, cx: &mut App) -> AnyView {
    stage(
        false,
        false,
        vec![
            (1, open("page:SemanticLinkKind", semantic_link_kind, Side::Below)),
            (2, settle()),
            (
                10,
                step(|window, cx| {
                    float::close_all(window, cx);
                }),
            ),
            (80, open("page:SemanticLinkKind", semantic_link_kind, Side::Below)),
        ],
        window,
        cx,
    )
}

fn film_tip(window: &mut Window, cx: &mut App) -> AnyView {
    stage(
        false,
        false,
        vec![
            (
                1,
                step(|window, cx| {
                    let request = tip_request(window, cx);
                    float::open(request, window, cx);
                }),
            ),
            (
                300,
                step(|window, cx| {
                    float::close(&key("tip"), window, cx);
                }),
            ),
        ],
        window,
        cx,
    )
}

fn film_deepen(window: &mut Window, cx: &mut App) -> AnyView {
    stage(
        false,
        false,
        vec![(1, rest("page:SemanticLinkKind", semantic_link_kind, Side::Below))],
        window,
        cx,
    )
}
