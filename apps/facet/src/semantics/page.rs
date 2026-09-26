//! The symbol page as data (gui-plan §8.3): everything the anatomy elements
//! draw, computed from the world with no GPUI, so a test can read the page
//! the way a person would.
//!
//! - [`Shape`]: the anatomy — a **fork** (one of), **holds** (a bracket of
//!   fields), a **pipe** (inputs → output, "or fails with" as its own exit,
//!   bounds as sentences) or a **contract** (you write / you get).
//! - [`caps_of`]: the `can` line.
//! - [`does`]: members grouped by what they do to it, look-alikes folded,
//!   trait-provided members under "through its traits".
//! - [`facts`]: the hero's facts line.
//! - [`in_use`]: up to three real statements from the callers' bodies.

use super::bounds::generics;
use super::caps::{Cap, caps};
use super::members::{Look, Receiver, fold, params};
use super::names::{InWorld, Names, scope};
use super::types::{Scope, TypeExpr, parse, parse_fields, parse_list};
use super::usage::{Needle, mine};
pub use super::model::{Branch, Contract, Does, DoesGroup, Facts, Field, FoldRow, Fork, Holds, Input, MemberRow, Payload, Pipe, Row, Shape, SigLine, Through, Use, Where};
use crate::graph::model::{Kind, NodeId, Package, Rel, World};
use gpui::SharedString;
use std::sync::Arc;

/// Everything the page draws, but In use (which needs source text).
#[derive(Clone)]
pub struct Page {
    /// The symbol.
    pub node: NodeId,
    /// The facts line.
    pub facts: Facts,
    /// The anatomy.
    pub shape: Shape,
    /// The `can` line.
    pub caps: Vec<Cap>,
    /// The prism's columns: left, right.
    pub prism: (Vec<super::model::Column>, Vec<super::model::Column>),
    /// The Does section.
    pub does: Does,
}

/// The prism rows per group on the page: five, or three when four or more
/// groups share the prism.
#[must_use]
pub const fn page_prism_rows(groups: usize) -> usize {
    if groups >= 4 { 3 } else { 5 }
}

/// Builds the page for `i`.
#[must_use]
pub fn page(world: &World, names: &Names, i: NodeId) -> Page {
    let resolver = InWorld { world, names, from: i };
    let groups = super::relations::except(super::relations::relations_of(world, i), &super::relations::page_shows(world.node(i).kind));
    Page {
        node: i,
        facts: facts(world, i),
        shape: shape(world, names, i),
        caps: caps_of(world, i),
        prism: super::relations::prism(world, i, &groups, page_prism_rows(groups.len())),
        does: does(world, &resolver, i),
    }
}

/// The facts line.
#[must_use]
pub fn facts(world: &World, i: NodeId) -> Facts {
    let used = world.used_in(i);
    Facts {
        kind: world.node(i).kind.text(),
        place: world.qual(i),
        used_in: used.len(),
        yours: used.iter().filter(|&&j| world.yours(j)).count(),
    }
}

/// The `can` line.
#[must_use]
pub fn caps_of(world: &World, i: NodeId) -> Vec<Cap> {
    let node = world.node(i);
    let written: Vec<(SharedString, Option<NodeId>)> = node
        .impls
        .iter()
        .filter_map(|imp| imp.trait_.map(|t| (world.node(t).name.clone(), Some(t))))
        .collect();
    caps(&node.derives, &written, &node.impls_ext)
}

fn copies(world: &World, i: NodeId) -> bool {
    let node = world.node(i);
    node.derives.iter().any(|d| d.as_ref() == "Copy")
        || node.impls.iter().any(|imp| imp.trait_.is_some_and(|t| world.node(t).name.as_ref() == "Copy"))
        || node.impls_ext.iter().any(|t| super::caps::trait_name(t) == "Copy")
}

/// The anatomy.
#[must_use]
pub fn shape(world: &World, names: &Names, i: NodeId) -> Shape {
    let node = world.node(i);
    let kids = world.kids(i);
    let spell = |at: NodeId, text: &str| {
        let resolver = InWorld { world, names, from: at };
        scope(&resolver, at).spell_text(text)
    };
    match node.kind {
        Kind::Enum => Shape::Fork(Fork {
            open: node.non_exhaustive,
            branches: kids
                .iter()
                .copied()
                .filter(|&j| world.node(j).kind == Kind::Variant)
                .map(|j| {
                    let v = world.node(j);
                    let resolver = InWorld { world, names, from: j };
                    let sc = scope(&resolver, j);
                    let payload = match (v.ty.as_deref(), v.shape.as_deref()) {
                        (Some(ty), Some("record")) => Payload::Record(
                            parse_fields(ty)
                                .into_iter()
                                .map(|(name, expr)| (SharedString::from(name.unwrap_or_default()), sc.spell(&expr)))
                                .collect(),
                        ),
                        (Some(ty), _) => Payload::Tuple(parse_list(ty).iter().map(|e| sc.spell(e)).collect()),
                        (None, _) => Payload::Unit,
                    };
                    Branch { node: j, name: v.name.clone(), payload, doc: v.doc.clone() }
                })
                .collect(),
        }),
        Kind::Struct | Kind::Union => {
            let fields: Vec<NodeId> = kids.iter().copied().filter(|&j| world.node(j).kind == Kind::Field).collect();
            let positional = !fields.is_empty() && fields.iter().all(|&j| world.node(j).name.chars().all(|c| c.is_ascii_digit()));
            Shape::Holds(Holds {
                fields: fields
                    .iter()
                    .map(|&j| {
                        let f = world.node(j);
                        Field {
                            node: j,
                            name: (!positional).then(|| f.name.clone()),
                            ty: spell(j, f.ty.as_deref().unwrap_or("")),
                            doc: f.doc.clone(),
                            public: f.vis.is_some(),
                        }
                    })
                    .collect(),
            })
        }
        Kind::Trait => {
            let resolver = InWorld { world, names, from: i };
            let methods: Vec<NodeId> = kids
                .iter()
                .copied()
                .filter(|&j| {
                    let m = world.node(j);
                    m.kind == Kind::Method && m.via.is_none() && !m.name.starts_with("__")
                })
                .collect();
            let (write, get): (Vec<NodeId>, Vec<NodeId>) = methods.iter().partition(|&&j| world.node(j).required);
            Shape::Contract(Contract {
                write: rows(world, &resolver, &write),
                get: rows(world, &resolver, &get),
                implementors: world.ins(i, Rel::IMPL | Rel::DERIVES).len(),
            })
        }
        Kind::Function | Kind::Method => Shape::Pipe(pipe(world, names, i)),
        Kind::Type => {
            let sig = node.sig.as_deref().unwrap_or("");
            let body = sig.split_once('=').map_or(sig, |(_, b)| b).trim().trim_end_matches(';');
            Shape::Alias(spell(i, body))
        }
        Kind::Constant => {
            let sig = node.sig.as_deref().unwrap_or("");
            let ty = sig.split_once(':').map_or("", |(_, t)| t);
            let ty = ty.split('=').next().unwrap_or(ty).trim().trim_end_matches(';');
            Shape::Constant(spell(i, ty))
        }
        _ => Shape::None,
    }
}

fn pipe(world: &World, names: &Names, i: NodeId) -> Pipe {
    let node = world.node(i);
    let resolver = InWorld { world, names, from: i };
    let sc = scope(&resolver, i);
    let mut inputs = Vec::new();
    let owner_copies = node.parent.is_some_and(|p| copies(world, p));
    if let Some(words) = Receiver::of(node.recv.as_deref(), owner_copies).input()
        && node.recv.is_some()
    {
        inputs.push(Input { name: SharedString::new_static(words), ty: None });
    }
    for p in params(&node.params) {
        inputs.push(Input { name: SharedString::from(p.name), ty: Some(sc.spell_text(&p.ty)) });
    }
    let (output, fails) = match node.ret.as_deref() {
        None => (None, None),
        Some(ret) => {
            let expr = parse(ret);
            match sc.fallible(&expr) {
                // ⌥ spells the whole result as written beside the output.
                Some((ok, err)) => (
                    (ok != TypeExpr::Tuple(Vec::new())).then(|| {
                        let mut out = sc.spell(&ok);
                        out.source = SharedString::from(ret.trim().to_owned());
                        out
                    }),
                    Some(err.map(|e| sc.spell(&e))),
                ),
                None => (Some(sc.spell_text(ret)), None),
            }
        }
    };
    let parent = node.parent.map(|p| world.node(p));
    let lists: Vec<&str> =
        [node.generics.as_deref(), parent.and_then(|p| p.generics.as_deref())].into_iter().flatten().collect();
    let wheres = generics(&lists, node.where_.as_deref().unwrap_or(""))
        .into_iter()
        .map(|g| Where {
            sentence: sc.sentence(&g),
            source: SharedString::from(g.constant.clone().unwrap_or_else(|| g.bounds.join(" + "))),
            name: SharedString::from(g.name),
        })
        .collect();
    let mut flags: Vec<&'static str> = node
        .quals
        .iter()
        .filter_map(|q| match q.as_ref() {
            "async" => Some("waits (async)"),
            "unsafe" => Some("you uphold its rules (unsafe)"),
            "const" => Some("runs at compile time too"),
            _ => None,
        })
        .collect();
    if node.must_use {
        flags.push("must use");
    }
    Pipe { inputs, output, fails, wheres, flags }
}

fn sig_line(sc: &Scope<'_>, world: &World, j: NodeId) -> SigLine {
    let m = world.node(j);
    SigLine {
        params: params(&m.params).iter().map(|p| sc.spell_text(&p.ty)).collect(),
        ret: m.ret.as_deref().map(|r| sc.spell_text(r)),
    }
}

/// Member rows for `list`, look-alikes folded.
fn rows(world: &World, resolver: &InWorld<'_>, list: &[NodeId]) -> Vec<Row> {
    let looks: Vec<Look<'_>> = list
        .iter()
        .map(|&j| {
            let m = world.node(j);
            Look { name: m.name.as_ref(), ret: m.ret.as_deref(), recv: m.recv.as_deref() }
        })
        .collect();
    fold(&looks)
        .into_iter()
        .map(|group| {
            if let [k] = group.as_slice() {
                let j = list[*k];
                let inner = InWorld { world: resolver.world, names: resolver.names, from: j };
                let sc = scope(&inner, j);
                let m = world.node(j);
                return Row::One(MemberRow { node: j, name: m.name.clone(), kind: m.kind, sig: sig_line(&sc, world, j), doc: m.doc.clone() });
            }
            let members: Vec<NodeId> = group.iter().map(|&k| list[k]).collect();
            let first = members[0];
            let inner = InWorld { world: resolver.world, names: resolver.names, from: first };
            let sc = scope(&inner, first);
            let prefix = super::members::prefix(world.node(first).name.as_ref()).unwrap_or("").to_owned();
            let mut seen: Vec<String> = Vec::new();
            let mut inputs = Vec::new();
            for &j in &members {
                if let Some(p) = params(&world.node(j).params).into_iter().next()
                    && !seen.contains(&p.ty)
                {
                    let inner = InWorld { world: resolver.world, names: resolver.names, from: j };
                    inputs.push(scope(&inner, j).spell_text(&p.ty));
                    seen.push(p.ty);
                }
            }
            Row::Fold(FoldRow {
                suffixes: members
                    .iter()
                    .map(|&j| SharedString::from(world.node(j).name.get(prefix.len()..).unwrap_or_default().to_owned()))
                    .collect(),
                prefix: SharedString::from(prefix),
                inputs,
                ret: world.node(first).ret.as_deref().map(|r| sc.spell_text(r)),
                members,
            })
        })
        .collect()
}

/// Members grouped by what they do to it.
#[must_use]
pub fn does(world: &World, resolver: &InWorld<'_>, i: NodeId) -> Does {
    let node = world.node(i);
    if node.kind == Kind::Trait {
        return Does::default();
    }
    let methods: Vec<NodeId> = world
        .kids(i)
        .iter()
        .copied()
        .filter(|&j| world.node(j).kind == Kind::Method && !world.node(j).name.starts_with("__"))
        .collect();
    let copy = copies(world, i);
    let own: Vec<NodeId> = methods.iter().copied().filter(|&j| world.node(j).via.is_none()).collect();
    let groups = Receiver::ORDER
        .iter()
        .filter_map(|&receiver| {
            let list: Vec<NodeId> =
                own.iter().copied().filter(|&j| Receiver::of(world.node(j).recv.as_deref(), copy) == receiver).collect();
            (!list.is_empty()).then(|| DoesGroup { receiver, rows: rows(world, resolver, &list) })
        })
        .collect();
    let mut through: Vec<Through> = Vec::new();
    for &j in &methods {
        let Some(via) = world.node(j).via.clone() else { continue };
        let entry = (j, world.node(j).name.clone());
        match through.iter_mut().find(|t| t.trait_name == via) {
            Some(t) => t.members.push(entry),
            None => through.push(Through { trait_name: via, members: vec![entry] }),
        }
    }
    Does { groups, through }
}

/// At most this many uses.
pub const USES: usize = 3;

/// The callers whose bodies may show a use, best first: yours, then other
/// packages, then by importance; callables only (a type only holds it and
/// the prism already says so); at most twelve.
#[must_use]
pub fn callers(world: &World, i: NodeId) -> Vec<NodeId> {
    let node = world.node(i);
    let mut refs: Vec<NodeId> = Vec::new();
    let push = |j: NodeId, refs: &mut Vec<NodeId>| {
        if !refs.contains(&j) {
            refs.push(j);
        }
    };
    for j in world.ins(i, Rel::CALLS | Rel::TAKES | Rel::GIVES | Rel::USES | Rel::TYPE | Rel::HAS) {
        push(j, &mut refs);
    }
    for &m in world.kids(i) {
        for j in world.ins(m, Rel::CALLS) {
            push(j, &mut refs);
        }
    }
    let own: Vec<NodeId> = std::iter::once(i).chain(world.kids(i).iter().copied()).collect();
    let mut list: Vec<NodeId> = refs
        .into_iter()
        .filter(|&j| !own.contains(&j) && world.node(world.top(j)).file.is_some() && !world.node(j).orphan)
        .collect();
    list.sort_by(|&a, &b| {
        let other = |j: NodeId| world.node(j).pkg != node.pkg;
        world
            .yours(b)
            .cmp(&world.yours(a))
            .then_with(|| other(b).cmp(&other(a)))
            .then_with(|| world.importance(b).partial_cmp(&world.importance(a)).unwrap_or(std::cmp::Ordering::Equal))
    });
    list.into_iter()
        .filter(|&j| {
            !matches!(
                world.node(j).kind,
                Kind::Field | Kind::Variant | Kind::Struct | Kind::Enum | Kind::Union | Kind::Type | Kind::Trait
            )
        })
        .take(12)
        .collect()
}

/// Up to three statements that use `i`, mined from its callers' bodies.
/// `read` returns a file's text given its package and path (the index, or
/// the fixture's `repo/` and `registry/` folders).
pub fn in_use(world: &World, i: NodeId, read: &mut dyn FnMut(&Package, &str) -> Option<Arc<str>>) -> Vec<Use> {
    let node = world.node(i);
    let needle = Needle { name: node.name.to_string(), member: node.kind == Kind::Method && node.parent.is_some() };
    let picks = callers(world, i);
    let mut out: Vec<Use> = Vec::new();
    let mut per_package: Vec<(u32, usize)> = Vec::new();
    for &j in &picks {
        if out.len() >= USES {
            break;
        }
        let top = world.node(world.top(j));
        let count = per_package.iter().find(|(p, _)| *p == top.pkg).map_or(0, |(_, c)| *c);
        if count >= 2 && picks.iter().any(|&q| world.node(q).pkg != top.pkg) {
            continue;
        }
        let Some(file) = top.file.clone() else { continue };
        let package = &world.packages[top.pkg as usize];
        let Some(text) = read(package, &file) else { continue };
        let caller = world.node(j);
        let start = if caller.line > 0 { caller.line } else { top.line };
        let end = caller.end.or(top.end).unwrap_or(top.line);
        let Some(excerpt) = mine(&text, start, end, &needle) else { continue };
        match per_package.iter_mut().find(|(p, _)| *p == top.pkg) {
            Some((_, c)) => *c += 1,
            None => per_package.push((top.pkg, 1)),
        }
        out.push(Use {
            caller: j,
            package: SharedString::from(world.package_short(top.pkg).to_owned()),
            file: SharedString::from(file.rsplit('/').next().unwrap_or(&file).to_owned()),
            excerpt,
        });
    }
    out
}
