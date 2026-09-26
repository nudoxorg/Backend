//! Anatomy scenes: the four target pages drawn from the fixture world, one
//! per anatomy — `present::glyph::RelationLabel` (fork),
//! `present::page::RelationGroup` (holds), `serde_json::de::from_str` (pipe
//! with a where-row) and `serde_core::de::Visitor` (contract with folding).
//!
//! Each scene is the reader region of the prototype's page (its window less
//! the 259 px shelf at 1440): a hero (gem, name, lede, facts) for context,
//! then the anatomy, `can`, the prism, Does and In use, with the float layer
//! as the last child so resting on a link raises its peek. The hero here is
//! scene scaffolding; the product's hero is the shell's.

use super::text::Links;
use super::{can, contract, does, fork, holds, in_use, k, pipe, prism, roles};
use crate::gallery::Scene;
use crate::graph::{NodeId, Package, World};
use crate::measure::{Measure, Set};
use crate::overlay::float;
use crate::overlay::peek::{Peek, SymbolPeek};
use crate::paint::gem::gem;
use crate::semantics::model::{Facts, Shape, Use};
use crate::semantics::page::{Page, in_use as mine_uses, page};
use crate::semantics::types::Target;
use crate::semantics::Names;
use crate::theme::ActiveFacet;
use crate::tokens::{Face, TypeRole};
use gpui::{
    AnyElement, AnyView, App, AppContext, Context, IntoElement, ParentElement, Render, SharedString, Styled, Window,
    div, px,
};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

/// The reader region of the prototype's 1440 × 1000 still: less the 259 px
/// shelf, the 50 px titlebar and the 26 px status bar.
const READER: (u32, u32) = (1181, 924);

pub(crate) const SCENES: &[Scene] = &[
    Scene {
        id: "anatomy-fork",
        title: "Anatomy: a fork (present::glyph::RelationLabel) — one of, can, prism, Does",
        size: READER,
        build: |_, cx| scene("present::glyph::RelationLabel", cx),
    },
    Scene {
        id: "anatomy-holds",
        title: "Anatomy: holds (present::page::RelationGroup) — all private, can, prism, Does, In use",
        size: READER,
        build: |_, cx| scene("present::page::RelationGroup", cx),
    },
    Scene {
        id: "anatomy-pipe",
        title: "Anatomy: a pipe with a where-row (serde_json::de::from_str) — or fails with, prism, In use",
        size: READER,
        build: |_, cx| scene("serde_json::de::from_str", cx),
    },
    Scene {
        id: "anatomy-contract",
        title: "Anatomy: a contract with look-alikes folded (serde_core::de::Visitor) — you write, you get, prism",
        size: READER,
        build: |_, cx| scene("serde_core::de::Visitor", cx),
    },
];

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../Nudox-Design-System/v4/graph")
}

/// The fixture world and its names, parsed once per process.
fn world() -> &'static (World, Names) {
    static WORLD: OnceLock<(World, Names)> = OnceLock::new();
    WORLD.get_or_init(|| {
        let bytes = std::fs::read(fixture().join("world.json")).unwrap_or_default();
        let world = World::from_json(&bytes).unwrap_or_else(|_| {
            World::new(Vec::new(), Vec::new(), Vec::new(), Vec::new()).expect("an empty world")
        });
        let names = Names::new(&world);
        (world, names)
    })
}

fn find(world: &World, qualified: &str) -> Option<NodeId> {
    let (place, name) = qualified.rsplit_once("::")?;
    (0..u32::try_from(world.len()).ok()?).find(|&i| world.node(i).name.as_ref() == name && world.qual(i).as_ref() == place)
}

fn read(package: &Package, file: &str) -> Option<Arc<str>> {
    let dir = if package.external { "registry" } else { "repo" };
    std::fs::read_to_string(fixture().join(dir).join(file)).ok().map(Arc::from)
}

/// The peek a link raises: the graph's symbol card for a symbol the world
/// holds.
fn links(world: &'static World) -> Links {
    Links {
        yours: Rc::new(move |target| matches!(target, Target::Node(n) if world.yours(*n))),
        peek: Rc::new(move |target| {
            let Target::Node(n) = target else { return None };
            let node = world.node(*n);
            let place = world.qual(*n);
            Some(Peek::Symbol(SymbolPeek {
                kind: Some(super::icon_kind(node.kind)),
                name: world.name_of(*n),
                place: SharedString::from(format!("{} in `{}`", node.kind.text(), place)),
                path: place,
                sentence: node.doc.clone(),
                ..SymbolPeek::default()
            }))
        }),
    }
}

struct AnatomyScene {
    node: Option<(NodeId, Page, Vec<(Use, SharedString)>)>,
}

fn scene(qualified: &'static str, cx: &mut App) -> AnyView {
    let (world, names) = world();
    let node = find(world, qualified).map(|node| {
        let uses = mine_uses(world, node, &mut read).into_iter().map(|u| {
            let name = world.name_of(u.caller);
            (u, name)
        });
        (node, page(world, names, node), uses.collect())
    });
    cx.new(|_: &mut Context<AnatomyScene>| AnatomyScene { node }).into()
}

/// Breaks an identifier at its humps, underscores and `::` (zero-width
/// spaces), so a long name wraps at a word and is never cut.
fn breakable(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 8);
    let mut prev: Option<char> = None;
    for c in name.chars() {
        if let Some(p) = prev
            && ((p.is_lowercase() || p.is_ascii_digit()) && c.is_uppercase() || p == '_' && c != '_' || p == ':' && c != ':')
        {
            out.push('\u{200B}');
        }
        out.push(c);
        prev = Some(c);
    }
    out
}

const HERO_NAME: TypeRole = TypeRole { face: Face::Display, weight: 700.0, size: 46.0, line: 47.0, tracking: -0.035, italic: false };
const LEDE: TypeRole = TypeRole { face: Face::Serif, weight: 400.0, size: 18.0, line: 25.0, tracking: 0.0, italic: true };
const FACTS: TypeRole = TypeRole { face: Face::Ui, weight: 400.0, size: 12.5, line: 18.0, tracking: 0.0, italic: false };

fn hero(world: &World, node: NodeId, facts: &Facts, measure: &Measure, cx: &App) -> AnyElement {
    let palette = cx.facet().palette();
    let n = world.node(node);
    let narrow = measure.effective() < 560.0;
    let size = if narrow { 40.0 } else { 64.0 };
    let gem_el = div().flex_none().child(gem(super::icon_kind(n.kind)).size(size * measure.scale()));
    let mut text = div()
        .flex()
        .flex_col()
        .min_w_0()
        .child(div().set(HERO_NAME, measure).text_color(palette.ink0.hsla()).child(breakable(&n.name)));
    if let Some(doc) = &n.doc {
        text = text.child(div().mt(k(measure, 6.0)).set(LEDE, measure).text_color(palette.ink2.hsla()).child(doc.clone()));
    }
    let head = if narrow {
        div().flex().flex_col().gap(k(measure, 12.0)).child(gem_el).child(text)
    } else {
        div().flex().items_center().gap(measure.fluid(14.0, 22.0)).child(gem_el).child(text)
    };
    let mut line = format!("{} in {} · used in {} place{}", facts.kind, facts.place, facts.used_in, if facts.used_in == 1 { "" } else { "s" });
    if facts.yours > 0 {
        line.push_str(&format!(" · {} in your code", facts.yours));
    }
    div()
        .flex()
        .flex_col()
        .gap(k(measure, 18.0))
        .child(head)
        .child(div().set(FACTS, measure).text_color(palette.ink3.hsla()).child(line))
        .into_any_element()
}

fn body(world: &'static World, node: NodeId, page: Page, uses: Vec<(Use, SharedString)>, measure: &Measure, cx: &App) -> Vec<AnyElement> {
    let Page { facts, shape, caps, prism: columns, does: members, .. } = page;
    let links = links(world);
    let mut out = vec![hero(world, node, &facts, measure, cx)];
    match shape {
        Shape::Fork(f) => out.push(fork("fork", f, measure, &links).into_any_element()),
        Shape::Holds(h) => out.push(holds("holds", h, measure, &links).into_any_element()),
        Shape::Pipe(p) => out.push(pipe("pipe", p, measure, &links).into_any_element()),
        Shape::Contract(c) => out.push(contract("contract", c, measure, &links).into_any_element()),
        Shape::Alias(_) | Shape::Constant(_) | Shape::None => {}
    }
    if !caps.is_empty() {
        out.push(can("can", caps, measure).into_any_element());
    }
    out.push(prism("prism", Target::Node(node), world.node(node).name.clone(), columns, measure, &links).into_any_element());
    if !members.is_empty() {
        out.push(does("does", members, measure, &links).into_any_element());
    }
    if !uses.is_empty() {
        out.push(in_use("in-use", uses, measure, &links).into_any_element());
    }
    out
}

impl Render for AnatomyScene {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let facet = cx.facet();
        let palette = facet.palette();
        let width = f32::from(window.viewport_size().width);
        // The prototype's folio: at most 900 px, padding fluid with the reader.
        let pad_x = (width * 0.04).clamp(16.0, 48.0);
        let pad_top = (width * 0.05).clamp(22.0, 64.0);
        let folio = width.min(900.0);
        let measure = facet.measure(px(folio - pad_x * 2.0));
        let content: Vec<AnyElement> = match &self.node {
            Some((node, page, uses)) => body(&world().0, *node, page.clone(), uses.clone(), &measure, cx),
            None => vec![div().set(roles::QUIET, &measure).text_color(palette.ink3.hsla()).child("The fixture world is not here.").into_any_element()],
        };
        div()
            .size_full()
            .relative()
            .overflow_hidden()
            .bg(palette.g0.hsla())
            .child(
                div().flex().justify_center().child(
                    div()
                        .w(px(folio))
                        .pt(px(pad_top))
                        .px(px(pad_x))
                        .pb(px(80.0))
                        .flex()
                        .flex_col()
                        .gap(k(&measure, 30.0))
                        .children(content),
                ),
            )
            .child(float::layer(window, cx))
    }
}
