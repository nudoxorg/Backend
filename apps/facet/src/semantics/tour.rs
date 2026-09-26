//! Start here (gui-plan §8.2 "Start here (T)"): a package's reading path,
//! computed. docs.rs lists a crate's items alphabetically; this names the
//! five or six a newcomer should read, in the order they meet them:
//!
//! - **start here**: the door, a free function used from other packages,
//!   preferring one that hands you the heart; failing that, the heart's own
//!   most-used maker ("how you get one");
//! - **what you hold**: the heart, the weightiest type that is not an error;
//! - **inside it**: at most two of the package's types the heart's fields
//!   and variants are made of;
//! - **what it promises**: the trait with the most implementors plus users;
//! - **when it fails**: the error the door returns, else the weightiest one.
//!
//! A package whose idea is a trait (serde) starts with it, adds the second
//! trait when that weighs a third as much, and keeps the heart only if it
//! carries a quarter of the trait's weight.
//!
//! This is `graph/tour.js` entry for entry (the golden is
//! `tests/engines.golden`, from `tests/fixtures/engines.mjs`). Pure data:
//! the graph flies it and the package page draws it as a strip.

use super::bounds::generics;
use super::names::{InWorld, Names};
use super::types::{Resolve, Target, TypeExpr, parse};
use crate::graph::model::{Kind, NodeId, Rel, World};
use std::cell::RefCell;
use std::collections::HashMap;

/// A tour never has more stops than this.
pub const MAX_STOPS: usize = 6;
/// A tour with fewer stops than this is not shown.
pub const MIN_STOPS: usize = 3;

/// The part a stop plays on the road.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Role {
    /// The trait a trait-first package is about.
    Idea,
    /// Its second trait, when that weighs a third as much.
    OtherHalf,
    /// The door: the call you make first, or how you get the heart.
    StartHere,
    /// The heart.
    WhatYouHold,
    /// One of the heart's parts.
    InsideIt,
    /// The contract.
    WhatItPromises,
    /// The error.
    WhenItFails,
}

impl Role {
    /// The role in words, as the plate and the strip say it.
    #[must_use]
    pub const fn text(self) -> &'static str {
        match self {
            Self::Idea => "the idea",
            Self::OtherHalf => "and the other half",
            Self::StartHere => "start here",
            Self::WhatYouHold => "what you hold",
            Self::InsideIt => "inside it",
            Self::WhatItPromises => "what it promises",
            Self::WhenItFails => "when it fails",
        }
    }
}

/// Why a stop is on the road.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Why {
    /// A trait's implementors (impl and derive edges).
    Doers(usize),
    /// A trait nobody here implements.
    OthersImplement,
    /// The door is a free function.
    CallFirst,
    /// The door is the heart's own maker.
    GetOne,
    /// The heart.
    TurnsOnIt,
    /// A part of the heart (the heart).
    PartOf(NodeId),
    /// The error.
    YouHandle,
}

impl Why {
    /// The reason in words (`486 types do it`, `part of Value`).
    #[must_use]
    pub fn text(self, world: &World) -> String {
        match self {
            // The prototype's words, plural even for one (reported).
            Self::Doers(n) => format!("{n} types do it"),
            Self::OthersImplement => "others implement it".to_owned(),
            Self::CallFirst => "the call you make first".to_owned(),
            Self::GetOne => "how you get one".to_owned(),
            Self::TurnsOnIt => "everything turns on it".to_owned(),
            Self::PartOf(heart) => format!("part of {}", world.node(heart).name),
            Self::YouHandle => "the error you handle".to_owned(),
        }
    }
}

/// One stop on the road.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stop {
    /// The symbol.
    pub node: NodeId,
    /// The part it plays.
    pub role: Role,
    /// Why it is here.
    pub why: Why,
}

/// A package's reading path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tour {
    /// The package.
    pub package: u32,
    /// The stops, in reading order (at most [`MAX_STOPS`]).
    pub stops: Vec<Stop>,
    /// How many items it chose from (public, top level, not tests).
    pub items: usize,
}

impl Tour {
    /// Whether the tour is worth offering (at least [`MIN_STOPS`] stops).
    #[must_use]
    pub fn shown(&self) -> bool {
        self.stops.len() >= MIN_STOPS
    }
}

/// How much other packages use a symbol.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Use {
    /// Distinct top-level items in other packages that refer to it.
    pub n: usize,
    /// How many of those are yours.
    pub yours: usize,
}

/// Distinct top-level items outside `package` that refer to `i` (item-level
/// in-edges for an item, member-level for a member), and how many are yours.
#[must_use]
pub fn use_of(world: &World, i: NodeId, package: u32) -> Use {
    let sources: Vec<NodeId> = if world.is_item(i) {
        world.item_ins(i).map(|(j, _)| j).collect()
    } else {
        world.in_edges(i).map(|(j, _)| j).collect()
    };
    let mut seen: Vec<NodeId> = Vec::new();
    let mut yours = 0;
    for j in sources {
        let t = world.top(j);
        if world.node(t).pkg == package || seen.contains(&t) {
            continue;
        }
        seen.push(t);
        if world.yours(t) {
            yours += 1;
        }
    }
    Use { n: seen.len(), yours }
}

/// The types `i`'s fields and variants are made of, in first-seen order: the
/// symbols their type keys name (`recipes.js` `tkey`), `i` itself left out.
#[must_use]
pub fn parts_of(world: &World, names: &Names, i: NodeId) -> Vec<NodeId> {
    let mut out: Vec<NodeId> = Vec::new();
    for &j in world.kids(i) {
        let n = world.node(j);
        let Some(ty) = n.ty.as_deref().filter(|_| matches!(n.kind, Kind::Field | Kind::Variant)) else { continue };
        let mut keys = Keys::new(world, names, j, i);
        keys.walk(&parse(ty));
        for t in keys.out {
            if t != i && !out.contains(&t) {
                out.push(t);
            }
        }
    }
    out
}

/// The symbols a type key names. A key keeps what the plain words keep: the
/// first argument of a wrapper (`Option`, `Result`, `Box`, …) and of a list,
/// both sides of a map, every part of a tuple, and a named type itself (its
/// arguments dropped); a generic is never a symbol.
struct Keys<'w> {
    resolver: InWorld<'w>,
    vars: Vec<String>,
    owner: NodeId,
    out: Vec<NodeId>,
}

/// Wrappers a key sees through (`recipes.js` `SEE`).
const SEE: [&str; 21] = [
    "Option", "Result", "Box", "Rc", "Arc", "Cow", "Pin", "RefCell", "Cell", "Mutex", "RwLock", "Ref", "RefMut",
    "MutexGuard", "RwLockReadGuard", "RwLockWriteGuard", "AsRef", "Into", "Borrow", "ManuallyDrop", "Weak",
];
const LISTS: [&str; 11] = [
    "Vec", "VecDeque", "SmallVec", "LinkedList", "HashSet", "BTreeSet", "IndexSet", "BinaryHeap", "IntoIterator",
    "Iterator", "Peekable",
];
const MAPS: [&str; 3] = ["HashMap", "BTreeMap", "IndexMap"];
/// Names a key spells as plain words (`text`, `path`, `number`, …).
const PLAIN: [&str; 26] = [
    "String", "str", "OsStr", "OsString", "Path", "PathBuf", "u8", "bool", "char", "u16", "u32", "u64", "u128",
    "usize", "i8", "i16", "i32", "i64", "i128", "isize", "f32", "f64", "NonZeroU32", "NonZeroU64", "NonZeroUsize",
    "Duration",
];

impl<'w> Keys<'w> {
    fn new(world: &'w World, names: &'w Names, from: NodeId, owner: NodeId) -> Self {
        let n = world.node(from);
        let parent = n.parent.map(|p| world.node(p));
        let lists: Vec<&str> =
            [n.generics.as_deref(), parent.and_then(|p| p.generics.as_deref())].into_iter().flatten().collect();
        let vars = generics(&lists, n.where_.as_deref().unwrap_or("")).into_iter().map(|g| g.name).collect();
        Self { resolver: InWorld { world, names, from }, vars, owner, out: Vec::new() }
    }

    /// A generic: declared, or a lone capital.
    fn is_var(&self, name: &str) -> bool {
        self.vars.iter().any(|v| v == name) || (name.len() == 1 && name.bytes().all(|b| b.is_ascii_uppercase()))
    }

    fn walk(&mut self, t: &TypeExpr) {
        match t {
            TypeExpr::Ref { inner, .. }
            | TypeExpr::Ptr { inner, .. }
            | TypeExpr::Slice(inner)
            | TypeExpr::Array { inner, .. } => self.walk(inner),
            TypeExpr::Tuple(items) => items.iter().for_each(|x| self.walk(x)),
            TypeExpr::Any(bounds) => bounds.iter().take(1).for_each(|b| self.walk(b)),
            TypeExpr::Binding { ty, .. } => self.walk(ty),
            // `Foo<T>::Bar` keys as `Foo`; `Self::X`, `T::X` and `<T as Tr>::X` as anything.
            TypeExpr::Assoc { base, via: None, .. } => match &**base {
                TypeExpr::Named { path, .. } if path[0] != "Self" && !(path.len() == 1 && self.is_var(&path[0])) => {
                    self.walk(base);
                }
                _ => {}
            },
            TypeExpr::Named { path, args } => self.named(path, args),
            TypeExpr::Assoc { .. } | TypeExpr::Func { .. } | TypeExpr::Never | TypeExpr::Infer => {}
        }
    }

    fn named(&mut self, path: &[String], args: &[TypeExpr]) {
        let Some(last) = path.last().map(String::as_str) else { return };
        if (path.len() > 1 && (path[0] == "Self" || self.is_var(&path[0]))) || (path.len() == 1 && self.is_var(last)) {
            return;
        }
        let kept = if SEE.contains(&last) || LISTS.contains(&last) {
            1
        } else if MAPS.contains(&last) {
            2
        } else {
            0
        };
        if kept > 0 {
            args.iter().take(kept).for_each(|a| self.walk(a));
        } else if last == "Self" {
            self.out.push(self.owner);
        } else if !PLAIN.contains(&last) && !last.starts_with(|c: char| c.is_ascii_lowercase()) {
            // The prototype resolves the last segment alone (`resolveName`).
            if let Some(Target::Node(j)) = self.resolver.named(&[last.to_owned()]) {
                self.out.push(j);
            }
        }
    }
}

/// Whether `ret` names `name` as a whole word (tour.js `retMentions`).
fn mentions(ret: Option<&str>, name: &str) -> bool {
    let Some(ret) = ret else { return false };
    let word = |c: Option<u8>| c.is_some_and(|c| c.is_ascii_alphanumeric() || c == b'_');
    let b = ret.as_bytes();
    ret.match_indices(name).any(|(at, _)| {
        let edge = |a: Option<u8>, z: Option<u8>| word(a) != word(z);
        let first = name.as_bytes().first().copied();
        let last = name.as_bytes().last().copied();
        edge(at.checked_sub(1).map(|k| b[k]), first) && edge(last, b.get(at + name.len()).copied())
    })
}

/// A file the tour skips: tests, benches, examples.
fn is_test(file: &str) -> bool {
    let dir = ["test/", "tests/", "benches/", "examples/"]
        .iter()
        .any(|d| file.starts_with(d) || file.contains(&format!("/{d}")));
    dir || file.ends_with("_test.rs") || file.ends_with("_tests.rs") || file.ends_with("/tests.rs")
}

fn is_type(kind: Kind) -> bool {
    matches!(kind, Kind::Struct | Kind::Enum | Kind::Union | Kind::Type)
}

fn is_error(world: &World, i: NodeId) -> bool {
    world.node(i).name.ends_with("Error")
}

/// One package's reading, as tour.js weighs it (use is memoised).
struct Reading<'w> {
    world: &'w World,
    names: &'w Names,
    candidates: &'w [NodeId],
    package: u32,
    uses: RefCell<HashMap<NodeId, Use>>,
}

/// The first of the highest `rank` (tour.js `best`).
fn best(list: impl IntoIterator<Item = NodeId>, rank: impl Fn(NodeId) -> f64) -> Option<NodeId> {
    list.into_iter().fold(None, |a, b| match a {
        Some(a) if rank(b) <= rank(a) => Some(a),
        _ => Some(b),
    })
}

impl Reading<'_> {
    fn use_(&self, i: NodeId) -> Use {
        *self.uses.borrow_mut().entry(i).or_insert_with(|| use_of(self.world, i, self.package))
    }

    /// Importance + 0.22·ln(1 + use) + 0.15 if your code uses it.
    #[allow(clippy::cast_precision_loss)]
    fn score(&self, i: NodeId) -> f64 {
        let u = self.use_(i);
        f64::from(self.world.importance(i)) + 0.22 * (u.n as f64).ln_1p() + if u.yours > 0 { 0.15 } else { 0.0 }
    }

    /// Implementors: impl and derive edges into a trait.
    fn doers(&self, t: NodeId) -> usize {
        self.world.ins(t, Rel::IMPL | Rel::DERIVES).len()
    }

    /// Use plus implementors.
    fn weight(&self, t: NodeId) -> usize {
        self.use_(t).n + self.doers(t)
    }

    fn returns(&self, j: NodeId, t: NodeId) -> bool {
        mentions(self.world.node(j).ret.as_deref(), &self.world.node(t).name)
    }

    /// Public, top-level, not orphan, not a test (tour.js `items`).
    fn items(&self) -> Vec<NodeId> {
        let world = self.world;
        self.candidates
            .iter()
            .copied()
            .filter(|&i| {
                let n = world.node(i);
                n.pkg == self.package
                    && !n.orphan
                    && !is_test(n.file.as_deref().unwrap_or(""))
                    && (n.vis.as_deref() == Some("pub") || n.via.is_some())
            })
            .collect()
    }

    /// A free function used from outside, preferring one that hands you the
    /// heart; failing that, the heart's own most-used public maker.
    fn door(&self, items: &[NodeId], heart: Option<NodeId>) -> Option<NodeId> {
        let world = self.world;
        let hands = |j: NodeId| heart.is_some_and(|h| self.returns(j, h));
        let doors = items.iter().copied().filter(|&i| world.node(i).kind == Kind::Function && self.use_(i).n > 0);
        let door = best(doors, |i| self.score(i) + if hands(i) { 0.3 } else { 0.0 });
        if door.is_some() {
            return door;
        }
        let heart = heart?;
        let mut makers: Vec<NodeId> = world
            .kids(heart)
            .iter()
            .copied()
            .filter(|&j| {
                let n = world.node(j);
                matches!(n.kind, Kind::Method | Kind::Function)
                    && n.recv.is_none()
                    && n.vis.as_deref() == Some("pub")
                    && self.returns(j, heart)
            })
            .collect();
        makers.sort_by(|&a, &b| {
            self.use_(b).n.cmp(&self.use_(a).n).then_with(|| world.importance(b).total_cmp(&world.importance(a)))
        });
        makers.first().copied()
    }

    /// At most two of this package's public types the heart is made of.
    fn inside(&self, heart: NodeId) -> Vec<NodeId> {
        let world = self.world;
        let mut parts: Vec<NodeId> = parts_of(world, self.names, heart)
            .into_iter()
            .filter(|&t| {
                let n = world.node(t);
                n.pkg == self.package
                    && n.parent.is_none()
                    && (n.vis.as_deref() == Some("pub") || n.via.is_some())
                    && is_type(n.kind)
                    && !is_error(world, t)
            })
            .collect();
        parts.sort_by(|&a, &b| self.score(b).total_cmp(&self.score(a)));
        parts.truncate(2);
        parts
    }
}

/// `package`'s reading path (tour.js `of`).
#[must_use]
pub fn of(world: &World, names: &Names, package: u32) -> Tour {
    of_items(world, names, package, &world.items)
}

/// A package's reading path from its prepared top-level item index.
/// Candidate order must match the world index to preserve ranking ties.
#[must_use]
pub fn of_items(world: &World, names: &Names, package: u32, candidates: &[NodeId]) -> Tour {
    let r = Reading { world, names, candidates, package, uses: RefCell::new(HashMap::new()) };
    let items = r.items();
    let types: Vec<NodeId> = items.iter().copied().filter(|&i| is_type(world.node(i).kind)).collect();
    let errors: Vec<NodeId> = types.iter().copied().filter(|&i| is_error(world, i)).collect();
    let mut heart = best(types.iter().copied().filter(|&i| !is_error(world, i)), |i| r.score(i));
    let door = r.door(&items, heart);
    let inside = heart.map_or_else(Vec::new, |h| r.inside(h));
    let mut traits: Vec<NodeId> = items.iter().copied().filter(|&i| world.node(i).kind == Kind::Trait).collect();
    traits.sort_by_key(|&t| std::cmp::Reverse(r.weight(t)));
    let contract = traits.first().copied();
    let fails = errors
        .iter()
        .copied()
        .find(|&e| door.is_some_and(|d| r.returns(d, e)) || heart.is_some_and(|h| r.returns(h, e)))
        .or_else(|| best(errors.iter().copied(), |i| r.score(i)));

    let mut stops: Vec<Stop> = Vec::new();
    let mut add = |node: Option<NodeId>, role: Role, why: Why| {
        if let Some(node) = node
            && !stops.iter().any(|s| s.node == node)
        {
            stops.push(Stop { node, role, why });
        }
    };
    // A package whose idea is a trait starts with its traits, and keeps its
    // heart only if the heart carries weight.
    if let Some(c) = contract
        && heart.is_none_or(|h| r.weight(c) >= 2 * r.use_(h).n + 5)
    {
        add(Some(c), Role::Idea, Why::Doers(r.doers(c)));
        if let Some(&second) = traits.get(1)
            && r.weight(second) * 3 >= r.weight(c)
        {
            add(Some(second), Role::OtherHalf, Why::Doers(r.doers(second)));
        }
        if heart.is_some_and(|h| r.use_(h).n * 4 < r.weight(c)) {
            heart = None;
        }
    }
    let opens = door.map(|d| if world.node(d).kind == Kind::Function { Why::CallFirst } else { Why::GetOne });
    add(door, Role::StartHere, opens.unwrap_or(Why::CallFirst));
    add(heart, Role::WhatYouHold, Why::TurnsOnIt);
    if let Some(h) = heart {
        for &t in &inside {
            add(Some(t), Role::InsideIt, Why::PartOf(h));
        }
    }
    let promise = contract.map(|c| match r.doers(c) {
        0 => Why::OthersImplement,
        n => Why::Doers(n),
    });
    add(contract, Role::WhatItPromises, promise.unwrap_or(Why::OthersImplement));
    add(fails, Role::WhenItFails, Why::YouHandle);
    stops.truncate(MAX_STOPS);
    Tour { package, stops, items: items.len() }
}

/// A lede as prose: markdown links and intra-doc links become their text,
/// and `**`, `__` and backticks drop out (tour.js `plain`).
#[must_use]
pub fn plain(lede: &str) -> String {
    drop_marks(&intra_links(&links(lede)))
}

/// `[text](target)` → `text`.
fn links(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut at = 0;
    while at < s.len() {
        if s[at..].starts_with('[')
            && let Some(close) = s[at + 1..].find(']').map(|k| at + 1 + k)
            && close > at + 1
            && s[close + 1..].starts_with('(')
            && let Some(end) = s[close + 2..].find(')').map(|k| close + 2 + k)
        {
            out.push_str(&s[at + 1..close]);
            at = end + 1;
            continue;
        }
        let c = s[at..].chars().next().unwrap_or(' ');
        out.push(c);
        at += c.len_utf8();
    }
    out
}

/// ``[`Name`]`` or `[Name]` → `Name`.
fn intra_links(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut at = 0;
    while at < s.len() {
        if s[at..].starts_with('[') {
            let open = at + 1 + usize::from(s[at + 1..].starts_with('`'));
            let run = s[open..].find([']', '`']).map_or(s.len(), |k| open + k);
            let close = run + usize::from(s[run..].starts_with('`'));
            if run > open && s[close..].starts_with(']') {
                out.push_str(&s[open..run]);
                at = close + 1;
                continue;
            }
        }
        let c = s[at..].chars().next().unwrap_or(' ');
        out.push(c);
        at += c.len_utf8();
    }
    out
}

/// Drops `**`, `__` and backticks.
fn drop_marks(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(c) = rest.chars().next() {
        if rest.starts_with("**") || rest.starts_with("__") {
            rest = &rest[2..];
        } else {
            if c != '`' {
                out.push(c);
            }
            rest = &rest[c.len_utf8()..];
        }
    }
    out
}

#[cfg(test)]
pub(crate) mod tests;

#[cfg(test)]
mod indexed_tests {
    #[test]
    fn package_index_preserves_semantic_tours_and_excludes_foreign_items() {
        let mut world = crate::graph::model::tests::tiny();
        for node in &mut world.nodes { node.vis = Some("pub".into()); }
        world.nodes[3].ret = Some("Result<(), Error>".into());
        let names = super::Names::new(&world);
        for package in 0..world.packages.len() as u32 {
            let candidates: Vec<_> = world.items.iter().copied().filter(|&i| world.node(i).pkg == package).collect();
            assert_eq!(super::of_items(&world, &names, package, &candidates), super::of(&world, &names, package));
        }
        let without_error = super::of_items(&world, &names, 1, &[0, 3, 4]);
        assert!(without_error.stops.iter().all(|stop| stop.node != 0 && stop.node != 5));
    }
}
