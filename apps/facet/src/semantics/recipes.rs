//! Getting one / Calling it (gui-plan §8.3): how to obtain a thing from plain
//! values, computed rather than written.
//!
//! One table of producers — every public callable, variant, open struct
//! literal and `Default` — keyed by plain-word type keys (`text`, `path`,
//! `number`, `list of #12`, `map text to number`, `any Read`, `#12` for a
//! symbol). Plain values cost nothing; a producer costs one step plus its
//! inputs (+0.45 if it may fail, +0.35 if it may give nothing). A least-cost
//! derivation over that AND-OR graph (Knuth's generalisation of Dijkstra: a
//! producer fires once every input is settled) gives every type its cheapest
//! recipe, once per package perspective. [`Recipes::routes`] ranks the best
//! few ways to a type; [`Recipes::call_route`] roots the same tree at a
//! callable; [`code`] writes a route the way a person would.
//!
//! This is `recipes.js` entry for entry (its golden is
//! `tests/recipes.golden`), with keys read from the language-neutral
//! [`TypeExpr`]: the key vocabulary is Rust's today, like the speller's.

use super::bounds::generics;
use super::members::params;
use super::types::{TypeExpr, parse, split_top};
use crate::graph::model::{Kind, NodeId, World};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Keys that cost nothing.
const GROUND: [&str; 8] = ["text", "path", "number", "bool", "char", "bytes", "nothing", "any"];

/// Whether a key is a plain value (or "any X": a bound, which you bring).
#[must_use]
pub fn is_ground(key: &str) -> bool {
    GROUND.contains(&key) || key.starts_with("any ")
}

const NUM: [&str; 17] = [
    "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128", "isize", "f32", "f64", "NonZeroU32",
    "NonZeroU64", "NonZeroUsize", "Duration",
];
/// Wrappers a key sees through.
const SEE: [&str; 21] = [
    "Option", "Result", "Box", "Rc", "Arc", "Cow", "Pin", "RefCell", "Cell", "Mutex", "RwLock", "Ref", "RefMut",
    "MutexGuard", "RwLockReadGuard", "RwLockWriteGuard", "AsRef", "Into", "Borrow", "ManuallyDrop", "Weak",
];
const LISTS: [&str; 11] = [
    "Vec", "VecDeque", "SmallVec", "LinkedList", "HashSet", "BTreeSet", "IndexSet", "BinaryHeap", "IntoIterator",
    "Iterator", "Peekable",
];
const MAPS: [&str; 3] = ["HashMap", "BTreeMap", "IndexMap"];
/// Number of type arguments represented by a canonical recipe key.
/// Other applied nominal types erase their arguments and cannot prove a call.
pub(crate) fn represented_arguments(name: &str) -> Option<usize> {
    if SEE.contains(&name) || LISTS.contains(&name) {
        Some(1)
    } else if MAPS.contains(&name) {
        Some(2)
    } else {
        None
    }
}

/// Names with a built-in interpretation in the recipe's shape algebra.
pub(crate) fn canonical_name(name: &str) -> bool {
    represented_arguments(name).is_some() || plain(name).is_some()
}

const PLAIN_BOUNDS: [&str; 13] =
    ["Sized", "Send", "Sync", "Clone", "Copy", "Debug", "Unpin", "Default", "PartialEq", "Eq", "Hash", "Ord", "PartialOrd"];

fn plain(last: &str) -> Option<&'static str> {
    Some(match last {
        "String" | "str" | "OsStr" | "OsString" => "text",
        "Path" | "PathBuf" => "path",
        "u8" => "byte",
        "bool" => "bool",
        "char" => "char",
        n if NUM.contains(&n) => "number",
        _ => return None,
    })
}

/// A type as a key, with the top-level `Option` / `Result` read as flags.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeKey {
    /// The key.
    pub key: String,
    /// Gives nothing sometimes (a top-level `Option`).
    pub maybe: bool,
    /// May fail (a top-level `Result`).
    pub fails: bool,
}

/// `byte` read as `number` once the whole key is known (a lone `u8`, or one
/// inside a tuple), as the prototype does.
fn bytes_to_numbers(key: &str) -> String {
    if key == "byte" {
        return "number".to_owned();
    }
    let mut out = String::with_capacity(key.len());
    let b = key.as_bytes();
    let mut k = 0;
    let word = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    while k < key.len() {
        if key[k..].starts_with("byte")
            && (k == 0 || !word(b[k - 1]))
            && b.get(k + 4).is_none_or(|&c| !word(c))
        {
            out.push_str("number");
            k += 4;
        } else {
            let c = key[k..].chars().next().unwrap_or(' ');
            out.push(c);
            k += c.len_utf8();
        }
    }
    out
}

/// Keys types written inside one symbol.
struct Keyer<'a> {
    world: &'a World,
    recipes: &'a Recipes,
    from: NodeId,
    owner: Option<NodeId>,
    gens: HashMap<String, Vec<String>>,
}

impl Keyer<'_> {
    fn is_var(&self, name: &str) -> bool {
        self.gens.contains_key(name) || (name.len() == 1 && name.chars().all(|c| c.is_ascii_uppercase()))
    }

    /// What a generic stands for, read from its bounds.
    fn bound(&self, name: &str) -> String {
        for b in self.gens.get(name).map(Vec::as_slice).unwrap_or_default() {
            for part in split_plus(b) {
                let p = part.trim();
                for wrapper in ["AsRef", "Into", "Borrow"] {
                    if let Some(rest) = p.strip_prefix(wrapper)
                        && let Some(inner) = rest.trim_start().strip_prefix('<').and_then(|r| r.trim_end().strip_suffix('>'))
                    {
                        let inner = inner.trim();
                        if !inner.is_empty() && inner.chars().all(|c| c.is_alphanumeric() || c == '_' || c == ':')
                            && let Some(w) = plain(inner.rsplit("::").next().unwrap_or(inner))
                        {
                            return if w == "byte" { "number".to_owned() } else { w.to_owned() };
                        }
                    }
                }
                if starts_with_word(p, "ToString") || starts_with_word(p, "Display") {
                    return "text".to_owned();
                }
                if let Some(named) = bound_name(p)
                    && !PLAIN_BOUNDS.contains(&named)
                {
                    return format!("any {named}");
                }
                if let Some(rest) = p.strip_prefix("IntoIterator")
                    && let Some(inner) = rest.trim_start().strip_prefix('<').and_then(|r| r.strip_suffix('>'))
                    && let Some(item) = inner.trim_start().strip_prefix("Item").map(str::trim_start).and_then(|r| r.strip_prefix('='))
                {
                    let item = item.trim();
                    let k = self.key_of(item).key;
                    return if k == "number" && contains_word(item, "u8") { "bytes".to_owned() } else { format!("list of {k}") };
                }
            }
        }
        "any".to_owned()
    }

    fn key_of(&self, source: &str) -> TypeKey {
        let mut out = TypeKey { key: "nothing".to_owned(), maybe: false, fails: false };
        if source.trim().is_empty() {
            return out;
        }
        let expr = parse(source);
        let key = self.ty(&expr, 0, &mut out);
        out.key = bytes_to_numbers(&key);
        out
    }

    fn resolve(&self, last: &str) -> String {
        match self.recipes.resolve(self.world, last, self.from) {
            Some(j) => format!("#{j}"),
            None => last.to_owned(),
        }
    }

    fn ty(&self, expr: &TypeExpr, depth: usize, flags: &mut TypeKey) -> String {
        match expr {
            TypeExpr::Ref { inner, .. } | TypeExpr::Ptr { inner, .. } => self.ty(inner, depth, flags),
            TypeExpr::Tuple(items) => match items.as_slice() {
                [] => "nothing".to_owned(),
                [one] => self.ty(one, depth + 1, flags),
                many => format!("({})", many.iter().map(|t| self.ty(t, depth + 1, flags)).collect::<Vec<_>>().join(", ")),
            },
            TypeExpr::Slice(inner) | TypeExpr::Array { inner, .. } => {
                let k = self.ty(inner, depth + 1, flags);
                if k == "byte" { "bytes".to_owned() } else { format!("list of {k}") }
            }
            TypeExpr::Never => "never".to_owned(),
            TypeExpr::Infer => "_".to_owned(),
            TypeExpr::Any(bounds) => bounds.first().map_or_else(|| "?".to_owned(), |b| self.ty(b, depth, flags)),
            TypeExpr::Func { .. } => "a function".to_owned(),
            TypeExpr::Binding { ty, .. } => self.ty(ty, depth, flags),
            TypeExpr::Assoc { base, name, .. } => {
                if matches!(&**base, TypeExpr::Named { path, .. } if path.first().is_some_and(|p| p == "Self" || self.is_var(p))) {
                    return "any".to_owned();
                }
                // A projection from a concrete type can still name its result.
                let last = name.rsplit("::").next().unwrap_or(name);
                self.named(&[last.to_owned()], &[], depth, flags, true)
            }
            TypeExpr::Named { path, args } => self.named(path, args, depth, flags, false),
        }
    }

    fn named(&self, path: &[String], args: &[TypeExpr], depth: usize, flags: &mut TypeKey, projected: bool) -> String {
        let Some(last) = path.last().map(String::as_str) else { return "?".to_owned() };
        // A generic's associated type is unknown, not an unrelated symbol
        // elsewhere in the world with the same final name (`T::Error`).
        if !projected && path.len() > 1 && path.first().is_some_and(|p| self.is_var(p)) {
            return "any".to_owned();
        }
        if !projected && path.len() == 1 && self.is_var(last) {
            return self.bound(last);
        }
        let see = SEE.contains(&last);
        let inner = if see { depth } else { depth + 1 };
        let keys: Vec<String> = args.iter().map(|a| self.ty(a, inner, flags)).collect();
        let a0 = keys.first().cloned().unwrap_or_else(|| "?".to_owned());
        match last {
            "Option" => {
                if depth == 0 {
                    flags.maybe = true;
                    return a0;
                }
                return format!("maybe {a0}");
            }
            "Result" => {
                if depth == 0 {
                    flags.fails = true;
                }
                return if keys.is_empty() { "nothing".to_owned() } else { a0 };
            }
            _ => {}
        }
        if see {
            return a0;
        }
        if LISTS.contains(&last) {
            return if a0 == "byte" {
                "bytes".to_owned()
            } else if last == "SmallVec" && a0.starts_with("list of ") {
                a0
            } else {
                format!("list of {a0}")
            };
        }
        if MAPS.contains(&last) {
            return format!("map {a0} to {}", keys.get(1).map_or("?", String::as_str));
        }
        if last == "Self" {
            return self.owner.map_or_else(|| "any".to_owned(), |o| format!("#{o}"));
        }
        if let Some(p) = plain(last) {
            return p.to_owned();
        }
        if last.starts_with(|c: char| c.is_ascii_lowercase()) {
            return last.to_owned();
        }
        self.resolve(last)
    }
}

fn starts_with_word(s: &str, w: &str) -> bool {
    s.strip_prefix(w).is_some_and(|rest| !rest.starts_with(|c: char| c.is_alphanumeric() || c == '_'))
}

fn contains_word(s: &str, w: &str) -> bool {
    s.match_indices(w).any(|(at, _)| {
        let before = s[..at].chars().next_back().is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
        let after = s[at + w.len()..].chars().next().is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
        before && after
    })
}

/// `a::b::Name` or `a::b::Name<…>` → `Name` when it starts uppercase.
fn bound_name(p: &str) -> Option<&str> {
    let head = match p.find('<') {
        Some(at) if p.ends_with('>') => &p[..at],
        Some(_) => return None,
        None => p,
    };
    let head = head.trim_end();
    let segs: Vec<&str> = head.split("::").collect();
    if segs.iter().any(|s| s.is_empty() || !s.chars().all(|c| c.is_alphanumeric() || c == '_')) {
        return None;
    }
    let last = *segs.last()?;
    last.starts_with(|c: char| c.is_ascii_uppercase()).then_some(last)
}

/// Splits at top-level `+` (outside `<>` and `()`), as `page.js` `splitPlus`.
fn split_plus(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0_i32;
    let mut start = 0;
    for (k, c) in s.char_indices() {
        match c {
            '<' | '(' => depth += 1,
            '>' | ')' => depth -= 1,
            '+' if depth == 0 => {
                out.push(s[start..k].trim().to_owned());
                start = k + 1;
            }
            _ => {}
        }
    }
    out.push(s[start..].trim().to_owned());
    out.retain(|p| !p.is_empty());
    out
}

/// How a producer makes its output.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum How {
    /// A free function (or an associated function without a receiver).
    Call,
    /// A method on its owner.
    Method,
    /// An enum variant.
    Variant,
    /// A struct literal.
    Literal,
    /// `Default::default()`.
    Default,
}

impl How {
    /// The prototype's word.
    #[must_use]
    pub const fn text(self) -> &'static str {
        match self {
            Self::Call => "call",
            Self::Method => "method",
            Self::Variant => "variant",
            Self::Literal => "literal",
            Self::Default => "default",
        }
    }
}

/// One row of the table: something that makes an `out` from its `ins`.
#[derive(Clone, Debug, PartialEq)]
pub struct Producer {
    /// The symbol.
    pub node: NodeId,
    /// How it makes one.
    pub how: How,
    /// Input keys, in order (a method's receiver first).
    pub ins: Vec<String>,
    /// Input names (`it` for the receiver; a field's name).
    pub names: Vec<String>,
    /// The output key.
    pub out: String,
    /// May fail.
    pub fails: bool,
    /// May give nothing.
    pub maybe: bool,
    /// A record variant (`Foo::Bar { .. }`).
    pub record: bool,
    /// A struct literal with a non-public field (only its own package can build it).
    pub closed: bool,
    /// A tuple struct literal.
    pub tuple: bool,
}

fn is_test(file: &str) -> bool {
    let path_hit = ["tests/", "test/", "benches/"].iter().any(|d| file.starts_with(d) || file.contains(&format!("/{d}")));
    path_hit || file.ends_with("_test.rs") || file.ends_with("_tests.rs") || file.ends_with("/tests.rs")
}

/// The least-cost derivation seen from one package.
pub struct Run {
    /// Each key's cheapest cost.
    pub cost: HashMap<String, f64>,
    /// Each key's cheapest producer (`None`: a plain value).
    pub best: HashMap<String, Option<usize>>,
}

/// One input of a step in a recipe tree.
#[derive(Clone, Debug, PartialEq)]
pub struct Kid {
    /// The input's key.
    pub key: String,
    /// Its name.
    pub name: String,
    /// How it is made (`None`: a plain value, or nothing public makes one).
    pub node: Option<Tree>,
    /// Nothing public makes one (a non-plain key with no recipe).
    pub opaque: bool,
}

/// A recipe: a producer and how each input is made.
#[derive(Clone, Debug, PartialEq)]
pub struct Tree {
    /// The producer's row.
    pub q: usize,
    /// Its inputs.
    pub kids: Vec<Kid>,
}

impl Tree {
    /// Steps in the tree.
    #[must_use]
    pub fn size(&self) -> usize {
        1 + self.kids.iter().filter_map(|k| k.node.as_ref()).map(Tree::size).sum::<usize>()
    }
}

/// One way to a type.
#[derive(Clone, Debug, PartialEq)]
pub struct Route {
    /// Its ranking cost.
    pub cost: f64,
    /// How.
    pub tree: Tree,
    /// Other single inputs the same maker accepts (folded look-alikes).
    pub alts: Vec<String>,
}

/// The best ways to a type.
#[derive(Clone, Debug, PartialEq)]
pub struct Routes {
    /// At most three, best first.
    pub routes: Vec<Route>,
    /// Every producer that makes one from something.
    pub makers: usize,
    /// Its own variants (the fork above shows them).
    pub picks: usize,
}

/// The table, the runs per package, and name resolution, for one world.
pub struct Recipes {
    table: Arc<Vec<Producer>>,
    runs: RefCell<HashMap<u32, std::rc::Rc<Run>>>,
    names: Arc<HashMap<String, Vec<NodeId>>>,
}

/// Transferable producer data; per-view caches are attached after loading.
#[derive(Clone)]
pub(crate) struct PreparedRecipes {
    table: Arc<Vec<Producer>>,
    names: Arc<HashMap<String, Vec<NodeId>>>,
}

/// A step that may fail costs this much more.
pub const FAILS: f64 = 0.45;
/// A step that may give nothing costs this much more.
pub const MAYBE: f64 = 0.35;

/// `Default` among a node's derives.
fn derives_default(world: &World, i: NodeId) -> bool {
    world.node(i).derives.iter().any(|d| d.as_ref() == "Default")
}

impl Recipes {
    /// Builds the producer table for `world`.
    #[must_use]
    pub fn new(world: &World) -> Self {
        let mut names: HashMap<String, Vec<NodeId>> = HashMap::new();
        for (i, n) in world.nodes.iter().enumerate() {
            if n.parent.is_none() && matches!(n.kind, Kind::Struct | Kind::Enum | Kind::Trait | Kind::Type | Kind::Union) {
                names.entry(n.name.to_string()).or_default().push(u32::try_from(i).unwrap_or(u32::MAX));
            }
        }
        let mut me = Self { table: Arc::new(Vec::new()), runs: RefCell::new(HashMap::new()), names: Arc::new(names) };
        me.table = Arc::new(me.build(world));
        me
    }

    /// Detaches thread-local recipe caches for transfer from a worker.
    pub(crate) fn into_prepared(self) -> PreparedRecipes {
        PreparedRecipes { table: self.table, names: self.names }
    }

    /// Shares immutable producer data with another worker without copying it.
    pub(crate) fn prepared(&self) -> PreparedRecipes {
        PreparedRecipes { table: self.table.clone(), names: self.names.clone() }
    }

    /// Attaches fresh caches after producer data arrives on the UI thread.
    pub(crate) fn from_prepared(data: PreparedRecipes) -> Self {
        Self { table: data.table, names: data.names, runs: RefCell::new(HashMap::new()) }
    }

    /// The prototype's `resolveName`: same package first, then importance,
    /// then node order.
    pub(crate) fn resolve(&self, world: &World, name: &str, from: NodeId) -> Option<NodeId> {
        let here = world.node(from).pkg;
        let list = self.names.get(name)?;
        let mut best: Option<NodeId> = None;
        for &c in list {
            best = match best {
                None => Some(c),
                Some(b) => {
                    let (sc, sb) = (world.node(c).pkg == here, world.node(b).pkg == here);
                    let better = (sc && !sb) || (sc == sb && world.importance(c) > world.importance(b));
                    Some(if better { c } else { b })
                }
            };
        }
        best
    }

    fn keyer<'a>(&'a self, world: &'a World, from: NodeId, owner: Option<NodeId>) -> Keyer<'a> {
        let n = world.node(from);
        let parent = n.parent.map(|p| world.node(p));
        let lists: Vec<&str> =
            [n.generics.as_deref(), parent.and_then(|p| p.generics.as_deref())].into_iter().flatten().collect();
        let mut gens: HashMap<String, Vec<String>> = HashMap::new();
        for g in generics(&lists, n.where_.as_deref().unwrap_or("")) {
            gens.insert(g.name, g.bounds);
        }
        Keyer { world, recipes: self, from, owner, gens }
    }

    /// The existing canonical representation, reused by discovery's proof layer.
    pub(crate) fn expression_key(&self, world: &World, from: NodeId, owner: Option<NodeId>, expr: &TypeExpr) -> String {
        let mut flags = TypeKey { key: String::new(), maybe: false, fails: false };
        bytes_to_numbers(&self.keyer(world, from, owner).ty(expr, 0, &mut flags))
    }

    fn build(&self, world: &World) -> Vec<Producer> {
        let mut table = Vec::new();
        for (index, n) in world.nodes.iter().enumerate() {
            let i = u32::try_from(index).unwrap_or(u32::MAX);
            if n.orphan {
                continue;
            }
            let file = n.file.clone().or_else(|| world.node(world.top(i)).file.clone()).unwrap_or_default();
            if is_test(&file) {
                continue;
            }
            match n.kind {
                Kind::Function | Kind::Method => {
                    let owner = n.parent;
                    let keyer = self.keyer(world, i, owner);
                    let out = n.ret.as_deref().map_or(TypeKey { key: "nothing".into(), maybe: false, fails: false }, |r| keyer.key_of(r));
                    let mut ins = Vec::new();
                    let mut names = Vec::new();
                    let method = n.recv.as_deref().is_some_and(|r| !r.is_empty());
                    if method && let Some(o) = owner {
                        ins.push(format!("#{o}"));
                        names.push("it".to_owned());
                    }
                    for p in params(&n.params) {
                        ins.push(keyer.key_of(&p.ty).key);
                        names.push(p.name.trim_start_matches('_').to_owned());
                    }
                    table.push(Producer {
                        node: i,
                        how: if method { How::Method } else { How::Call },
                        ins,
                        names,
                        out: out.key,
                        fails: out.fails,
                        maybe: out.maybe,
                        record: false,
                        closed: false,
                        tuple: false,
                    });
                }
                Kind::Variant => {
                    let Some(owner) = n.parent else { continue };
                    let keyer = self.keyer(world, i, Some(owner));
                    let record = n.shape.as_deref() == Some("record");
                    let mut ins = Vec::new();
                    let mut names = Vec::new();
                    for part in n.ty.as_deref().map(|t| split_top(t, ',')).unwrap_or_default() {
                        let colon = if record { part.find(':').filter(|&c| c > 0) } else { None };
                        match colon {
                            Some(c) => {
                                names.push(part[..c].trim().to_owned());
                                ins.push(keyer.key_of(part[c + 1..].trim()).key);
                            }
                            None => {
                                names.push(String::new());
                                ins.push(keyer.key_of(&part).key);
                            }
                        }
                    }
                    table.push(Producer { node: i, how: How::Variant, ins, names, out: format!("#{owner}"), fails: false, maybe: false, record, closed: false, tuple: false });
                }
                Kind::Struct | Kind::Enum => {
                    if n.kind == Kind::Struct && !n.non_exhaustive {
                        let fields: Vec<NodeId> = world.kids(i).iter().copied().filter(|&j| world.node(j).kind == Kind::Field).collect();
                        let ins = fields
                            .iter()
                            .map(|&j| self.keyer(world, j, Some(i)).key_of(world.node(j).ty.as_deref().unwrap_or("")).key)
                            .collect();
                        let names = fields.iter().map(|&j| world.node(j).name.to_string()).collect();
                        table.push(Producer {
                            node: i,
                            how: How::Literal,
                            ins,
                            names,
                            out: format!("#{i}"),
                            fails: false,
                            maybe: false,
                            record: false,
                            closed: fields.iter().any(|&j| world.node(j).vis.as_deref() != Some("pub")),
                            tuple: !fields.is_empty() && fields.iter().all(|&j| world.node(j).name.chars().all(|c| c.is_ascii_digit())),
                        });
                    }
                    if derives_default(world, i) {
                        table.push(Producer { node: i, how: How::Default, ins: Vec::new(), names: Vec::new(), out: format!("#{i}"), fails: false, maybe: false, record: false, closed: false, tuple: false });
                    }
                }
                _ => {}
            }
        }
        table
    }

    /// The table.
    #[must_use]
    pub fn table(&self) -> &[Producer] {
        &self.table
    }

    /// Whether `e` can be used from package `pk`.
    pub(crate) fn usable(world: &World, e: &Producer, pk: u32) -> bool {
        let n = world.node(e.node);
        if e.how == How::Literal && e.closed {
            return n.pkg == pk;
        }
        if e.how == How::Variant {
            let parent = n.parent.map(|p| world.node(p));
            return parent.is_some_and(|p| p.vis.as_deref() == Some("pub")) || n.pkg == pk;
        }
        n.vis.as_deref() == Some("pub") || n.via.is_some() || n.pkg == pk
    }

    pub(crate) fn weight(&self, world: &World, e: &Producer) -> f64 {
        #[allow(clippy::cast_precision_loss)]
        let ins = e.ins.len() as f64;
        1.0 + if e.fails { FAILS } else { 0.0 }
            + if e.maybe { MAYBE } else { 0.0 }
            + if e.how == How::Literal { 0.12 * ins } else { 0.0 }
            - if e.how == How::Default { 0.3 } else { 0.0 }
            + if world.packages[world.node(e.node).pkg as usize].external { 0.05 } else { 0.0 }
    }

    /// The least-cost derivation seen from package `pk` (cached).
    pub fn run(&self, world: &World, pk: u32) -> std::rc::Rc<Run> {
        if let Some(run) = self.runs.borrow().get(&pk) {
            return run.clone();
        }
        let run = std::rc::Rc::new(self.run_with_inputs(world, pk, &HashSet::new()));
        self.runs.borrow_mut().insert(pk, run.clone());
        run
    }

    /// The same derivation with values the caller already has made free.
    /// Graph discovery uses `u32::MAX` as the public-world perspective.
    pub(crate) fn run_with_inputs(&self, world: &World, pk: u32, free: &HashSet<String>) -> Run {
        self.run_with_filter(world, pk, free, |_, _| true)
    }

    /// The same derivation, admitting only producers whose inputs are proven.
    pub(crate) fn run_with_filter(&self, world: &World, pk: u32, free: &HashSet<String>, eligible: impl Fn(usize, &Producer) -> bool) -> Run {
        let mut cost: HashMap<String, f64> = HashMap::new();
        let mut best: HashMap<String, Option<usize>> = HashMap::new();
        let mut done: HashSet<String> = HashSet::new();
        let mut waiting: HashMap<String, Vec<usize>> = HashMap::new();
        let mut left: Vec<i64> = vec![-1; self.table.len()];
        let mut heap: Vec<(f64, String)> = Vec::new();
        fn push(heap: &mut Vec<(f64, String)>, c: f64, k: String) {
            heap.push((c, k));
            let mut a = heap.len() - 1;
            while a > 0 {
                let b = (a - 1) >> 1;
                if heap[b].0 <= heap[a].0 {
                    break;
                }
                heap.swap(a, b);
                a = b;
            }
        }
        fn pop(heap: &mut Vec<(f64, String)>) -> Option<(f64, String)> {
            if heap.is_empty() {
                return None;
            }
            let last = heap.pop()?;
            if heap.is_empty() {
                return Some(last);
            }
            let top = std::mem::replace(&mut heap[0], last);
            let mut a = 0;
            loop {
                let (l, r) = (2 * a + 1, 2 * a + 2);
                let mut m = a;
                if l < heap.len() && heap[l].0 < heap[m].0 {
                    m = l;
                }
                if r < heap.len() && heap[r].0 < heap[m].0 {
                    m = r;
                }
                if m == a {
                    break;
                }
                heap.swap(a, m);
                a = m;
            }
            Some(top)
        }
        let relax = |k: &str, c: f64, q: Option<usize>, cost: &mut HashMap<String, f64>, best: &mut HashMap<String, Option<usize>>, done: &HashSet<String>, heap: &mut Vec<(f64, String)>| {
            if !done.contains(k) && c < cost.get(k).copied().unwrap_or(f64::INFINITY) {
                cost.insert(k.to_owned(), c);
                best.insert(k.to_owned(), q);
                push(heap, c, k.to_owned());
            }
        };
        for g in GROUND {
            relax(g, 0.0, None, &mut cost, &mut best, &done, &mut heap);
        }
        let mut supplied: Vec<&String> = free.iter().collect();
        supplied.sort();
        for g in supplied {
            relax(g, 0.0, None, &mut cost, &mut best, &done, &mut heap);
        }
        for e in self.table.iter() {
            for k in &e.ins {
                if k.starts_with("any ") && !cost.contains_key(k) {
                    relax(k, 0.0, None, &mut cost, &mut best, &done, &mut heap);
                }
            }
        }
        for (q, e) in self.table.iter().enumerate() {
            if is_ground(&e.out) || e.out == "never" || e.out == "?" || e.ins.contains(&e.out) || !Self::usable(world, e, pk) || !eligible(q, e) {
                continue;
            }
            let mut ks: Vec<&String> = Vec::new();
            for k in &e.ins {
                if !ks.contains(&k) {
                    ks.push(k);
                }
            }
            left[q] = i64::try_from(ks.len()).unwrap_or(0);
            if ks.is_empty() {
                let w = self.weight(world, e);
                relax(&e.out, w, Some(q), &mut cost, &mut best, &done, &mut heap);
            } else {
                for k in ks {
                    waiting.entry(k.clone()).or_default().push(q);
                }
            }
        }
        while let Some((_, k)) = pop(&mut heap) {
            if !done.insert(k.clone()) {
                continue;
            }
            let Some(list) = waiting.get(&k).cloned() else { continue };
            for q in list {
                if left[q] <= 0 {
                    continue;
                }
                left[q] -= 1;
                if left[q] > 0 {
                    continue;
                }
                let e = &self.table[q];
                let mut s = self.weight(world, e);
                for x in &e.ins {
                    s += cost.get(x).copied().unwrap_or(f64::INFINITY);
                }
                relax(&e.out, s, Some(q), &mut cost, &mut best, &done, &mut heap);
            }
        }
        Run { cost, best }
    }

    pub(crate) fn tree(&self, run: &Run, q: usize, seen: &HashSet<String>) -> Tree {
        let e = &self.table[q];
        let kids = e
            .ins
            .iter()
            .enumerate()
            .map(|(a, k)| {
                let name = e.names.get(a).cloned().unwrap_or_default();
                if is_ground(k) {
                    return Kid { key: k.clone(), name, node: None, opaque: false };
                }
                match run.best.get(k).copied().flatten() {
                    Some(b) if !seen.contains(k) => {
                        let mut s = seen.clone();
                        s.insert(k.clone());
                        Kid { key: k.clone(), name, node: Some(self.tree(run, b, &s)), opaque: false }
                    }
                    _ => Kid { key: k.clone(), name, node: None, opaque: true },
                }
            })
            .collect();
        Tree { q, kids }
    }

    fn cost_of(&self, world: &World, run: &Run, e: &Producer) -> f64 {
        let mut s = self.weight(world, e);
        for k in &e.ins {
            match run.cost.get(k) {
                Some(c) => s += c,
                None => return f64::INFINITY,
            }
        }
        s
    }

    /// The best few ways to the type `i`, one per distinct maker.
    #[must_use]
    pub fn routes(&self, world: &World, i: NodeId, max: usize) -> Routes {
        let n = world.node(i);
        let run = self.run(world, n.pkg);
        let key = format!("#{i}");
        let mut picks = 0;
        let mut cands: Vec<(f64, usize)> = Vec::new();
        for (q, e) in self.table.iter().enumerate() {
            if e.out != key || e.ins.contains(&key) || !Self::usable(world, e, n.pkg) {
                continue;
            }
            let en = world.node(e.node);
            if e.how == How::Variant && en.parent == Some(i) {
                picks += 1;
                continue;
            }
            let c = self.cost_of(world, &run, e);
            if c < f64::INFINITY {
                #[allow(clippy::cast_precision_loss)]
                let adj = (if en.pkg == n.pkg { 0.0 } else { 0.3 }) + 0.04 * e.ins.len() as f64
                    - (if en.parent == Some(i) { 0.15 } else { 0.0 })
                    + (if world.yours(e.node) && !world.yours(i) { 0.2 } else { 0.0 });
                cands.push((c + adj, q));
            }
        }
        cands.sort_by(|a, b| {
            a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal).then_with(|| {
                let (ia, ib) = (world.importance(self.table[a.1].node), world.importance(self.table[b.1].node));
                ib.partial_cmp(&ia).unwrap_or(std::cmp::Ordering::Equal)
            })
        });
        let mut out: Vec<Route> = Vec::new();
        let mut groups: HashMap<(How, Option<NodeId>, String), usize> = HashMap::new();
        let seen: HashSet<String> = std::iter::once(key.clone()).collect();
        for &(c, q) in &cands {
            let e = &self.table[q];
            let en = world.node(e.node);
            let tag = (e.how, en.parent, en.name.to_string());
            if let Some(&r) = groups.get(&tag) {
                if out[r].alts.len() < 8 && e.ins.len() == 1 {
                    out[r].alts.push(e.ins[0].clone());
                }
                continue;
            }
            if out.len() >= max {
                continue;
            }
            let tree = self.tree(&run, q, &seen);
            if !out.is_empty() && (tree.size() > 6 || c > out[0].cost + 3.0) {
                continue;
            }
            groups.insert(tag, out.len());
            out.push(Route { cost: c, tree, alts: Vec::new() });
        }
        Routes { routes: out, makers: cands.len(), picks }
    }

    /// How to call the callable `i`: its own row, each argument derived.
    #[must_use]
    pub fn call_route(&self, world: &World, i: NodeId) -> Option<Route> {
        let q = self.table.iter().position(|e| e.node == i && matches!(e.how, How::Call | How::Method))?;
        let run = self.run(world, world.node(i).pkg);
        Some(Route { cost: self.cost_of(world, &run, &self.table[q]), tree: self.tree(&run, q, &HashSet::new()), alts: Vec::new() })
    }

    /// The row `q`.
    #[must_use]
    pub fn producer(&self, q: usize) -> &Producer {
        &self.table[q]
    }

    /// A key's cost from package `pk` (for the rail's spine).
    #[must_use]
    pub fn cost(&self, world: &World, pk: u32, key: &str) -> Option<f64> {
        self.run(world, pk).cost.get(key).copied()
    }
}

// ------------------------------------------------------------------ code

fn snake(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    let mut prev: Option<char> = None;
    for c in s.chars() {
        if let Some(p) = prev
            && (p.is_ascii_lowercase() || p.is_ascii_digit())
            && c.is_ascii_uppercase()
        {
            out.push('_');
        }
        out.push(c.to_ascii_lowercase());
        prev = Some(c);
    }
    out
}

/// Every run of non-word characters as one `_` (`/\W+/g`).
fn underscored(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut gap = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c);
            gap = false;
        } else if !gap {
            out.push('_');
            gap = true;
        }
    }
    out
}

fn node_of(key: &str) -> Option<NodeId> {
    key.strip_prefix('#').and_then(|n| n.parse().ok())
}

/// A route written the way a person writes it: nested steps become `let`
/// bindings named after their type, a sub-step shared by two inputs is bound
/// once, and `?` follows each step that may fail.
#[must_use]
pub fn code(recipes: &Recipes, world: &World, tree: &Tree) -> String {
    write_code(recipes, world, tree, false)
}

/// A road unwraps optional intermediate calls; the answer stays optional.
pub(crate) fn chain_code(recipes: &Recipes, world: &World, tree: &Tree) -> String {
    write_code(recipes, world, tree, true)
}

fn write_code(recipes: &Recipes, world: &World, tree: &Tree, optional: bool) -> String {
    struct Coder<'a> {
        optional: bool,
        root: usize,
        recipes: &'a Recipes,
        world: &'a World,
        lets: Vec<String>,
        bound: HashMap<String, String>,
        used: HashSet<String>,
        calls: HashSet<String>,
    }
    impl Coder<'_> {
        fn walk(&mut self, t: &Tree) {
            let e = self.recipes.producer(t.q);
            if e.how == How::Call {
                self.calls.insert(self.world.node(e.node).name.to_string());
            }
            for k in &t.kids {
                if let Some(n) = &k.node {
                    self.walk(n);
                }
            }
        }
        fn fresh(&mut self, v: &str) -> String {
            let base = if self.calls.contains(v) { format!("the_{v}") } else { v.to_owned() };
            let mut w = base.clone();
            let mut k = 2;
            while self.used.contains(&w) {
                w = format!("{base}{k}");
                k += 1;
            }
            self.used.insert(w.clone());
            w
        }
        fn name_for(&self, key: &str) -> String {
            let list = key.strip_prefix("list of ");
            let base = node_of(list.unwrap_or(key)).map_or_else(|| "value".to_owned(), |j| snake(&self.world.node(j).name));
            if list.is_some() { format!("{base}s") } else { base }
        }
        fn leaf(&self, k: &Kid) -> String {
            if !k.name.is_empty() && k.name != "it" && k.name != "self" && !k.name.chars().all(|c| c.is_ascii_digit()) {
                return k.name.clone();
            }
            if let Some(j) = node_of(&k.key) {
                return snake(&self.world.node(j).name);
            }
            match k.key.as_str() {
                "text" => "text".to_owned(),
                "path" => "path".to_owned(),
                "number" => "n".to_owned(),
                "bool" => "yes".to_owned(),
                "char" => "c".to_owned(),
                "bytes" => "bytes".to_owned(),
                "any" => "value".to_owned(),
                "nothing" => "()".to_owned(),
                other => snake(&underscored(other)),
            }
        }
        fn arg(&mut self, k: &Kid) -> String {
            let Some(node) = &k.node else {
                let v = self.leaf(k);
                self.used.insert(v.clone());
                return v;
            };
            // A sub-step already bound is used by name, never rebuilt.
            if let Some(v) = self.bound.get(&k.key) {
                return v.clone();
            }
            let s = self.one(node);
            if !node.kids.iter().any(|x| x.node.is_some()) && s.encode_utf16().count() <= 40 {
                return s;
            }
            let v = self.fresh(&self.name_for(&k.key));
            self.lets.push(format!("let {v} = {s};"));
            self.bound.insert(k.key.clone(), v.clone());
            v
        }
        fn one(&mut self, t: &Tree) -> String {
            let args: Vec<String> = t.kids.iter().map(|k| self.arg(k)).collect();
            let value = spell(self.recipes, self.world, t, &args);
            let e = self.recipes.producer(t.q);
            if self.optional && t.q != self.root && e.maybe && !e.fails { format!("{value}?") } else { value }
        }
    }
    let mut coder = Coder { optional, root: tree.q, recipes, world, lets: Vec::new(), bound: HashMap::new(), used: HashSet::new(), calls: HashSet::new() };
    coder.walk(tree);
    let last = coder.one(tree);
    let mut out = coder.lets;
    out.push(last);
    out.join("\n")
}

fn spell(recipes: &Recipes, world: &World, t: &Tree, args: &[String]) -> String {
    let e = recipes.producer(t.q);
    let n = world.node(e.node);
    let owner = n.parent.map(|p| world.node(p).name.to_string()).unwrap_or_default();
    let fields = || {
        e.names
            .iter()
            .zip(args)
            .map(|(f, a)| if f == a { f.clone() } else { format!("{f}: {a}") })
            .collect::<Vec<_>>()
            .join(", ")
    };
    let s = match e.how {
        How::Method => {
            let recv = args.first().cloned().unwrap_or_default();
            let wrapped = recv.split_once(' ').is_some_and(|(head, rest)| {
                !head.is_empty() && head.chars().next().is_some_and(|c| c.is_alphanumeric() || c == '_') && head.chars().all(|c| c.is_alphanumeric() || c == '_' || c == ':') && rest.starts_with('{')
            });
            let recv = if wrapped { format!("({recv})") } else { recv };
            format!("{recv}.{}({})", n.name, args.get(1..).unwrap_or_default().join(", "))
        }
        How::Call => format!("{}{}({})", if owner.is_empty() { String::new() } else { format!("{owner}::") }, n.name, args.join(", ")),
        How::Variant => {
            let head = format!("{owner}::{}", n.name);
            if e.ins.is_empty() {
                head
            } else if e.record {
                format!("{head} {{ {} }}", fields())
            } else {
                format!("{head}({})", args.join(", "))
            }
        }
        How::Literal => {
            if e.tuple {
                format!("{}({})", n.name, args.join(", "))
            } else if e.ins.is_empty() {
                n.name.to_string()
            } else {
                format!("{} {{ {} }}", n.name, fields())
            }
        }
        How::Default => format!("{}::default()", n.name),
    };
    if e.fails { format!("{s}?") } else { s }
}

// ------------------------------------------------------------------ rails

/// What a rail starts from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Lead {
    /// Straight into the first step.
    None,
    /// A trivial first step folded into words: "from a Source".
    FromA(String),
    /// The first step's receiver: "from a Foo".
    From(String),
    /// Plain values (at most two): "from text what, a number n".
    Plain(Vec<(String, String)>),
}

/// An input that rides along a step: "+ a DeclarationKind kind".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rider {
    /// Its key.
    pub key: String,
    /// Its name (empty: none shown).
    pub name: String,
    /// Made by a trivial step ("a Foo").
    pub article: bool,
}

/// One step of a rail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    /// The producer.
    pub node: NodeId,
    /// Its words: a name, `Owner::Variant`, `build` or `default`.
    pub verb: String,
    /// May fail.
    pub fails: bool,
    /// May give nothing.
    pub maybe: bool,
    /// Inputs riding along.
    pub riders: Vec<Rider>,
    /// The type this step makes, when another step follows (a station).
    pub station: Option<String>,
}

/// A route read left to right like a transit line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rail {
    /// Where it starts.
    pub lead: Lead,
    /// The steps, first to last.
    pub steps: Vec<Step>,
    /// Other single inputs the same maker takes (at most five shown).
    pub also: Vec<String>,
    /// How many more than shown.
    pub also_more: usize,
    /// The code, for ⌥.
    pub code: String,
}

/// Whether a name is worth showing beside its input.
#[must_use]
pub fn shown_name(name: &str) -> bool {
    !name.is_empty() && name != "it" && !name.chars().all(|c| c.is_ascii_digit())
}

impl Recipes {
    fn trivial(&self, q: usize) -> bool {
        let e = &self.table[q];
        e.ins.is_empty() && matches!(e.how, How::Variant | How::Default | How::Literal)
    }

    /// A route as a rail: the spine follows the costliest input, the other
    /// inputs ride along (`recipes.js` `rail`).
    #[must_use]
    pub fn rail(&self, world: &World, route: &Route) -> Rail {
        let pk = world.node(self.table[route.tree.q].node).pkg;
        let run = self.run(world, pk);
        struct Raw<'t> {
            q: usize,
            sides: Vec<&'t Kid>,
        }
        let mut raw: Vec<Raw<'_>> = Vec::new();
        let mut cur = Some(&route.tree);
        while let Some(t) = cur {
            let mut spine: Option<usize> = None;
            let mut best = -1.0_f64;
            for (a, k) in t.kids.iter().enumerate() {
                if k.node.is_some() {
                    let v = run.cost.get(&k.key).copied().unwrap_or(0.0);
                    if v > best {
                        best = v;
                        spine = Some(a);
                    }
                }
            }
            let sides = t.kids.iter().enumerate().filter(|(a, _)| Some(*a) != spine).map(|(_, k)| k).filter(|k| !(k.node.is_none() && k.key == "nothing")).collect();
            raw.insert(0, Raw { q: t.q, sides });
            cur = spine.and_then(|a| t.kids[a].node.as_ref());
        }
        let mut lead = Lead::None;
        if raw.len() > 1 && self.trivial(raw[0].q) {
            lead = Lead::FromA(self.table[raw[0].q].out.clone());
            raw.remove(0);
        }
        if lead == Lead::None {
            let e0 = &self.table[raw[0].q];
            if e0.how == How::Method && raw[0].sides.first().is_some_and(|k| k.name == "it") {
                let first = raw[0].sides.remove(0);
                lead = Lead::From(first.key.clone());
            } else if !raw[0].sides.is_empty() && raw[0].sides.iter().all(|k| k.node.is_none()) {
                let take = raw[0].sides.len().min(2);
                let plain: Vec<(String, String)> = raw[0]
                    .sides
                    .drain(..take)
                    .map(|k| (k.key.clone(), if shown_name(&k.name) { k.name.clone() } else { String::new() }))
                    .collect();
                lead = Lead::Plain(plain);
            }
        }
        let last = raw.len() - 1;
        let steps = raw
            .iter()
            .enumerate()
            .map(|(a, r)| {
                let e = &self.table[r.q];
                let n = world.node(e.node);
                let verb = match e.how {
                    How::Variant => format!("{}::{}", n.parent.map(|p| world.node(p).name.to_string()).unwrap_or_default(), n.name),
                    How::Literal => "build".to_owned(),
                    How::Default => "default".to_owned(),
                    How::Call | How::Method => n.name.to_string(),
                };
                Step {
                    node: e.node,
                    verb,
                    fails: e.fails,
                    maybe: e.maybe,
                    riders: r
                        .sides
                        .iter()
                        .map(|k| Rider {
                            key: k.key.clone(),
                            name: if shown_name(&k.name) { k.name.clone() } else { String::new() },
                            article: k.node.as_ref().is_some_and(|t| self.trivial(t.q)),
                        })
                        .collect(),
                    station: (a < last).then(|| e.out.clone()),
                }
            })
            .collect();
        let first_in = self.table[route.tree.q].ins.first();
        let alts: Vec<&String> = route.alts.iter().filter(|k| Some(*k) != first_in).collect();
        let mut also: Vec<String> = Vec::new();
        for k in &alts {
            if !also.contains(k) {
                also.push((*k).clone());
            }
        }
        also.truncate(5);
        Rail { lead, steps, also, also_more: alts.len().saturating_sub(5), code: code(self, world, &route.tree) }
    }
}

/// A key in plain words (`recipes.js` `words`, as text).
#[must_use]
pub fn key_words(world: &World, key: &str) -> String {
    if let Some(j) = node_of(key) {
        return world.node(j).name.to_string();
    }
    if let Some(rest) = key.strip_prefix("list of ") {
        return format!("list of {}", key_words(world, rest));
    }
    if let Some(rest) = key.strip_prefix("maybe ") {
        return format!("maybe {}", key_words(world, rest));
    }
    if let Some(rest) = key.strip_prefix("map ")
        && let Some((a, b)) = rest.split_once(" to ")
    {
        return format!("map {} → {}", key_words(world, a), key_words(world, b));
    }
    if let Some(rest) = key.strip_prefix("any ") {
        return format!("any {rest}");
    }
    match key {
        "text" => "text",
        "path" => "a path",
        "number" => "a number",
        "bool" => "yes or no",
        "char" => "a character",
        "bytes" => "bytes",
        "any" => "anything",
        "nothing" => "nothing",
        other => other,
    }
    .to_owned()
}

impl Rail {
    /// The rail as the words a reader sees, in order.
    #[must_use]
    pub fn words(&self, world: &World) -> String {
        let mut out: Vec<String> = Vec::new();
        match &self.lead {
            Lead::None => {}
            Lead::FromA(k) => out.push(format!("from a {}", key_words(world, k))),
            Lead::From(k) => out.push(format!("from {}", key_words(world, k))),
            Lead::Plain(list) => out.push(format!(
                "from {}",
                list.iter()
                    .map(|(k, n)| if n.is_empty() { key_words(world, k) } else { format!("{} {n}", key_words(world, k)) })
                    .collect::<Vec<_>>()
                    .join(" , ")
            )),
        }
        for s in &self.steps {
            out.push(s.verb.clone());
            if s.fails || s.maybe {
                out.push("?".to_owned());
            }
            for r in &s.riders {
                let a = if r.article { "a " } else { "" };
                let name = if r.name.is_empty() { String::new() } else { format!(" {}", r.name) };
                out.push(format!("+ {a}{}{name}", key_words(world, &r.key)));
            }
            if let Some(st) = &s.station {
                out.push(key_words(world, st));
            }
        }
        if !self.also.is_empty() {
            let more = if self.also_more > 0 { format!(" and {} more", self.also_more) } else { String::new() };
            out.push(format!("also from {}{more}", self.also.iter().map(|k| key_words(world, k)).collect::<Vec<_>>().join(" , ")));
        }
        out.join(" ")
    }
}

/// A Getting one or Calling it section.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Section {
    /// Getting one: the rails and the foot.
    Getting {
        /// The rails, best first.
        rails: Vec<Rail>,
        /// How many producers make one.
        makers: usize,
        /// Its own variants, shown above.
        picks: usize,
    },
    /// Only its own variants make one.
    PickOnly(usize),
    /// Nothing public makes one from plain values.
    Received {
        /// Whether anything makes one at all (else: "from its own crate").
        makers: usize,
    },
    /// Calling it: the rail rooted at the callable.
    Calling {
        /// The rail.
        rail: Rail,
        /// Inputs nothing public makes from plain values.
        need: Vec<String>,
    },
}

impl Section {
    /// The section's heading.
    #[must_use]
    pub const fn heading(&self) -> &'static str {
        match self {
            Self::Calling { .. } => "Calling it",
            _ => "Getting one",
        }
    }

    /// The foot line (after the rails).
    #[must_use]
    pub fn foot(&self, world: &World) -> String {
        match self {
            Self::Getting { makers, picks, .. } => {
                let mut parts = vec![format!("{makers} way{} in this world make{} one", if *makers == 1 { "" } else { "s" }, if *makers == 1 { "s" } else { "" })];
                if *picks > 0 {
                    parts.push(format!("or pick one of its {picks} variant{} above", if *picks == 1 { "" } else { "s" }));
                }
                parts.push("⌥ for code".to_owned());
                parts.join(" · ")
            }
            Self::PickOnly(picks) => format!("Pick one of its {picks} variant{} above; nothing else in this world makes one.", if *picks == 1 { "" } else { "s" }),
            Self::Received { makers } => format!(
                "Nothing public makes one from plain values. You receive it{}; the prism shows from where.",
                if *makers > 0 { "" } else { " from its own crate" }
            ),
            Self::Calling { need, .. } if need.is_empty() => "every argument, from plain values · ⌥ for code".to_owned(),
            Self::Calling { need, .. } => format!(
                "you need {} — nothing public makes one from plain values",
                need.iter().map(|k| key_words(world, k)).collect::<Vec<_>>().join(" , ")
            ),
        }
    }

    /// The whole section as the words a reader sees.
    #[must_use]
    pub fn words(&self, world: &World) -> String {
        let mut out = vec![self.heading().to_owned()];
        match self {
            Self::Getting { rails, .. } => out.extend(rails.iter().map(|r| r.words(world))),
            Self::Calling { rail, .. } => out.push(rail.words(world)),
            Self::PickOnly(_) | Self::Received { .. } => {}
        }
        out.push(self.foot(world));
        out.join(" ")
    }
}

impl Recipes {
    /// Getting one, for a type (`None` for anything else).
    #[must_use]
    pub fn getting_one(&self, world: &World, i: NodeId) -> Option<Section> {
        if !matches!(world.node(i).kind, Kind::Struct | Kind::Enum | Kind::Union | Kind::Type) {
            return None;
        }
        let Routes { routes, makers, picks } = self.routes(world, i, 3);
        Some(if routes.is_empty() && picks > 0 {
            Section::PickOnly(picks)
        } else if routes.is_empty() {
            Section::Received { makers }
        } else {
            Section::Getting { rails: routes.iter().map(|r| self.rail(world, r)).collect(), makers, picks }
        })
    }

    /// Calling it, for a callable whose arguments are not all plain.
    #[must_use]
    pub fn calling_it(&self, world: &World, i: NodeId) -> Option<Section> {
        if !matches!(world.node(i).kind, Kind::Function | Kind::Method) {
            return None;
        }
        let route = self.call_route(world, i)?;
        let deep = route.tree.kids.iter().any(|k| k.node.is_some());
        let need: Vec<String> = route.tree.kids.iter().filter(|k| k.opaque && !is_ground(&k.key)).map(|k| k.key.clone()).collect();
        if !deep && need.is_empty() {
            return None;
        }
        Some(Section::Calling { rail: self.rail(world, &route), need })
    }
}

// ------------------------------------------------------------------ views

use super::model::{RailView, RecipeView, StepView};
use super::types::{Piece, Target};
use gpui::SharedString;

fn word(w: &str) -> Piece {
    Piece::Word(SharedString::from(w.to_owned()))
}

/// A key as pieces to draw: symbols are links, plain values words.
#[must_use]
pub fn key_pieces(world: &World, key: &str) -> Vec<Piece> {
    if let Some(j) = node_of(key) {
        return vec![Piece::Name { text: world.node(j).name.clone(), target: Target::Node(j) }];
    }
    let mut out = Vec::new();
    for (prefix, words) in [("list of ", "list of"), ("maybe ", "maybe")] {
        if let Some(rest) = key.strip_prefix(prefix) {
            out.push(word(words));
            out.push(Piece::Space);
            out.extend(key_pieces(world, rest));
            return out;
        }
    }
    if let Some(rest) = key.strip_prefix("map ")
        && let Some((a, b)) = rest.split_once(" to ")
    {
        out.push(word("map"));
        out.push(Piece::Space);
        out.extend(key_pieces(world, a));
        out.push(Piece::Space);
        out.push(Piece::Punct(SharedString::new_static("→")));
        out.push(Piece::Space);
        out.extend(key_pieces(world, b));
        return out;
    }
    if let Some(rest) = key.strip_prefix("any ") {
        let target = world
            .nodes
            .iter()
            .position(|n| n.kind == Kind::Trait && n.name.as_ref() == rest)
            .and_then(|j| u32::try_from(j).ok())
            .map_or_else(|| Target::Path(SharedString::from(rest.to_owned())), Target::Node);
        return vec![word("any"), Piece::Space, Piece::Name { text: SharedString::from(rest.to_owned()), target }];
    }
    vec![word(&key_words(world, key))]
}

fn named(world: &World, key: &str, name: &str) -> Vec<Piece> {
    let mut out = key_pieces(world, key);
    if !name.is_empty() {
        out.push(Piece::Space);
        out.push(Piece::Var(SharedString::from(name.to_owned())));
    }
    out
}

impl Rail {
    /// The rail ready to draw.
    #[must_use]
    pub fn view(&self, world: &World, call: bool) -> RailView {
        let mut lead = Vec::new();
        match &self.lead {
            Lead::None => {}
            Lead::FromA(k) => {
                lead.extend([word("from a"), Piece::Space]);
                lead.extend(key_pieces(world, k));
            }
            Lead::From(k) => {
                lead.extend([word("from"), Piece::Space]);
                lead.extend(key_pieces(world, k));
            }
            Lead::Plain(list) => {
                lead.extend([word("from"), Piece::Space]);
                for (n, (k, name)) in list.iter().enumerate() {
                    if n > 0 {
                        lead.push(Piece::Punct(SharedString::new_static(", ")));
                    }
                    lead.extend(named(world, k, name));
                }
            }
        }
        let steps = self
            .steps
            .iter()
            .map(|s| StepView {
                verb: SharedString::from(s.verb.clone()),
                target: Target::Node(s.node),
                fails: s.fails,
                maybe: s.maybe,
                riders: s
                    .riders
                    .iter()
                    .map(|r| {
                        let mut p = vec![word("+"), Piece::Space];
                        if r.article {
                            p.extend([word("a"), Piece::Space]);
                        }
                        p.extend(named(world, &r.key, &r.name));
                        p
                    })
                    .collect(),
                station: s.station.as_ref().map(|k| key_pieces(world, k)),
            })
            .collect();
        let mut also = Vec::new();
        if !self.also.is_empty() {
            also.extend([word("also from"), Piece::Space]);
            for (n, k) in self.also.iter().enumerate() {
                if n > 0 {
                    also.push(Piece::Punct(SharedString::new_static(", ")));
                }
                also.extend(key_pieces(world, k));
            }
            if self.also_more > 0 {
                also.extend([Piece::Space, word(&format!("and {} more", self.also_more))]);
            }
        }
        RailView { lead, steps, also, code: SharedString::from(self.code.clone()), call }
    }
}

impl Section {
    /// The section ready to draw.
    #[must_use]
    pub fn view(&self, world: &World) -> RecipeView {
        let foot = SharedString::from(self.foot(world));
        match self {
            Self::Getting { rails, .. } => RecipeView {
                heading: self.heading(),
                rails: rails.iter().map(|r| r.view(world, false)).collect(),
                foot: Some(foot),
                sentence: None,
            },
            Self::Calling { rail, .. } => {
                RecipeView { heading: self.heading(), rails: vec![rail.view(world, true)], foot: Some(foot), sentence: None }
            }
            Self::PickOnly(_) | Self::Received { .. } => {
                RecipeView { heading: self.heading(), rails: Vec::new(), foot: None, sentence: Some(foot) }
            }
        }
    }
}
