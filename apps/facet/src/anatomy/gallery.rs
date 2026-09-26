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
use super::{can, contract, does, fork, holds, in_use, k, pipe, prism, recipe, roles};
use crate::gallery::Scene;
use crate::graph::{NodeId, Package, World};
use crate::measure::{Measure, Set};
use crate::fonts::Typeset as _;
use crate::overlay::float;
use crate::overlay::peek::Peek;
use crate::paint::gem::gem;
use crate::semantics::model::{Facts, RecipeView, Shape, Use};
use crate::semantics::recipes::Recipes;
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
        id: "anatomy-getting-one",
        title: "Anatomy: Getting one (present::page::Page) — one route over four steps, ⌥ for its code",
        size: READER,
        build: |_, cx| scene("present::page::Page", cx),
    },
    Scene {
        id: "anatomy-value",
        title: "Anatomy: a fork with Getting one (serde_json::value::Value) — default, folded from, or pick a variant",
        size: READER,
        build: |_, cx| scene("serde_json::value::Value", cx),
    },
    Scene {
        id: "anatomy-deserializer",
        title: "Anatomy: Getting one (serde_json::de::Deserializer) — three makers of equal cost",
        size: READER,
        build: |_, cx| scene("serde_json::de::Deserializer", cx),
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

/// What the page's links know: yours (mint), and the peek they raise — the
/// graph's own symbol card (`graph::peek::symbol_peek`), so page and graph
/// peek alike.
fn links(world: &'static World) -> Links {
    Links {
        yours: Rc::new(move |target| matches!(target, Target::Node(n) if world.yours(*n))),
        peek: Rc::new(move |target| match target {
            Target::Node(n) => Some(Peek::Symbol(crate::graph::peek::symbol_peek(world, *n))),
            Target::Path(_) => None,
        }),
    }
}

struct AnatomyScene {
    node: Option<(NodeId, Page, Vec<(Use, SharedString)>, Option<RecipeView>)>,
}

thread_local! {
    /// The producer table and its runs over the fixture world (built on
    /// first use; the gallery draws on one thread).
    static RECIPES: &'static Recipes = Box::leak(Box::new(Recipes::new(&world().0)));
}

fn scene(qualified: &'static str, cx: &mut App) -> AnyView {
    let (world, names) = world();
    let node = find(world, qualified).map(|node| {
        let uses = mine_uses(world, node, &mut read).into_iter().map(|u| {
            let name = world.name_of(u.caller);
            (u, name)
        });
        let recipe = RECIPES.with(|r| r.getting_one(world, node).or_else(|| r.calling_it(world, node)).map(|s| s.view(world)));
        (node, page(world, names, node), uses.collect(), recipe)
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

fn hero(world: &World, node: NodeId, facts: &Facts, measure: &Measure, reader: f32, cx: &App) -> AnyElement {
    let palette = cx.facet().palette();
    let n = world.node(node);
    let narrow = measure.effective() < 560.0;
    let size = if narrow { 40.0 } else { 64.0 };
    let gem_el = div().flex_none().child(gem(super::icon_kind(n.kind)).size(size * measure.scale()));
    let mut text = div()
        .flex()
        .flex_col()
        .min_w_0()
        .child(
            div()
                .typeset_at(TypeRole { size: (20.0 + 0.02 * reader / measure.scale()).clamp(30.0, 46.0), line: 1.02 * (20.0 + 0.02 * reader / measure.scale()).clamp(30.0, 46.0), ..HERO_NAME }, measure.scale())
                .text_color(palette.ink0.hsla())
                .child(breakable(&n.name)),
        );
    if let Some(doc) = &n.doc {
        let lede = (12.0 + 0.005 * reader / measure.scale()).clamp(15.0, 18.0);
        text = text.child(
            div()
                .mt(k(measure, 6.0))
                .typeset_at(TypeRole { size: lede, line: lede * 1.4, ..LEDE }, measure.scale())
                .text_color(palette.ink2.hsla())
                .child(doc.clone()),
        );
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

#[allow(clippy::too_many_arguments)]
fn body(world: &'static World, node: NodeId, page: Page, uses: Vec<(Use, SharedString)>, recipe_view: Option<RecipeView>, measure: &Measure, reader: f32, cx: &App) -> Vec<AnyElement> {
    let Page { facts, shape, caps, prism: columns, does: members, .. } = page;
    let links = links(world);
    let mut out = vec![hero(world, node, &facts, measure, reader, cx)];
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
    if let Some(view) = recipe_view {
        out.push(recipe("recipe", view, measure, &links).into_any_element());
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
            Some((node, page, uses, recipe_view)) => body(&world().0, *node, page.clone(), uses.clone(), recipe_view.clone(), &measure, width, cx),
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
