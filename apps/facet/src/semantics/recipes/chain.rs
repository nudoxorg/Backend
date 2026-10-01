//! The hand's connector (D-Hand, `v4/hand/hand-core.js`): the pieces you
//! hold, arranged by what feeds what, and the verbs between them.
//!
//! A piece is a noun: a type (its own key), a callable (what it takes, what
//! it gives), a trait (a contract: it filters the roads, it joins nothing by
//! itself), or context (a package, a module). A join is the verb from one
//! piece to the next:
//!
//! - **direct**: the first gives exactly what the next takes (an alias is
//!   its target), or a value of a trait the next accepts;
//! - **as**: the first gives "any T" you choose, and the next is a T
//!   (`from_str → Value`);
//! - **steps**: this table's road between two types, at most three steps
//!   (`Value → as_table → Table`).
//!
//! No road, no join: the piece stands apart. Joins through a plain value
//! (text into text) are refused: plain values connect everything, so they
//! connect nothing. The arrangement tries every order of at most five pieces
//! and keeps the one with the most joins; then the one closest to the order
//! you held them in (fewest swapped pairs); then the cheapest.
//!
//! What a type *is* comes from the world's own data: its derives, its
//! impls (in the world or outside it), what a type alias's target is, and
//! the supertraits of every trait it implements. A blanket impl is not in
//! the world, so what only a blanket impl grants (`DeserializeOwned` for
//! every `Deserialize`) is not known here and is not assumed.

use super::{How, Kid, Producer, Recipes, Run, Tree, chain_code, is_ground, key_words};
use crate::graph::model::{Kind, NodeId, World};
use crate::semantics::types::{parse, split_top};
use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};

/// The public-world perspective (every package's public items).
const PUBLIC: u32 = u32::MAX;
/// The longest road a join may take.
pub const MAX_STEPS: usize = 3;
/// A road costlier than this is no road.
const MAX_COST: f64 = 3.6;

/// Views of the same thing and comparisons: never a step.
fn identity(name: &str) -> bool {
    matches!(
        name,
        "as_ref" | "as_mut" | "borrow" | "borrow_mut" | "deref" | "deref_mut" | "clone" | "to_owned" | "into" | "index"
            | "index_mut" | "eq" | "ne" | "cmp" | "partial_cmp" | "lt" | "le" | "gt" | "ge" | "hash"
    )
}

/// What a piece is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// A type: it is its own key.
    Type,
    /// A contract: it filters the roads.
    Trait,
    /// A callable (or a field read): takes its inputs, gives its output.
    Call,
    /// A package, a module, a macro: it joins nothing.
    Context,
}

/// One held thing, as a noun.
#[derive(Clone, Debug, PartialEq)]
pub struct Piece {
    /// The symbol.
    pub node: NodeId,
    /// What it is.
    pub role: Role,
    /// Its producer row (a callable).
    pub q: Option<usize>,
    /// The keys it takes.
    pub ins: Vec<String>,
    /// The key it gives.
    pub out: Option<String>,
    /// It may fail.
    pub fails: bool,
    /// It may give nothing.
    pub maybe: bool,
}

/// A road from one type to another.
#[derive(Clone, Debug, PartialEq)]
pub struct Chain {
    /// Its ranking cost.
    pub cost: f64,
    /// Its producers, first step first.
    pub spine: Vec<usize>,
    /// Each step's input on the spine (`spine[i]`'s `ins[at[i]]`).
    pub at: Vec<usize>,
    /// What it starts from (the value you have, or the trait it stood for).
    pub from: String,
}

impl Chain {
    /// How many steps.
    #[must_use]
    pub fn steps(&self) -> usize {
        self.spine.len()
    }
}

/// How one piece feeds the next.
#[derive(Clone, Debug, PartialEq)]
pub enum JoinHow {
    /// Exactly what the next takes.
    Direct,
    /// "Any T" into a T.
    As,
    /// A road of at most three steps.
    Steps(Chain),
}

/// The verb from one piece to the next.
#[derive(Clone, Debug, PartialEq)]
pub struct Join {
    /// Its cost.
    pub cost: f64,
    /// How.
    pub how: JoinHow,
    /// The key it goes into.
    pub into: String,
}

/// One held piece in a road, with the join that brought it in.
#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    /// The piece.
    pub piece: Piece,
    /// How the previous piece feeds it (`None` for the first).
    pub join: Option<Join>,
}

/// Pieces joined into one road.
#[derive(Clone, Debug, PartialEq)]
pub struct Road {
    /// In flow order.
    pub links: Vec<Link>,
    /// Held traits it passes through.
    pub through: Vec<NodeId>,
}

/// The hand, arranged.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Arrangement {
    /// Joined roads, longest first.
    pub roads: Vec<Road>,
    /// What joins nothing, in the order it was held.
    pub apart: Vec<Piece>,
}

/// A road's one sentence: "from text to Table, in two steps".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sentence {
    /// What it starts from.
    pub from: String,
    /// What it ends with.
    pub to: String,
    /// A round trip: the held type it passes through ("and back").
    pub via: Option<String>,
    /// How many steps.
    pub steps: usize,
    /// Held traits it goes through.
    pub through: Vec<String>,
}

const NUM: [&str; 11] = ["no", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten"];

impl Sentence {
    /// The sentence as words.
    #[must_use]
    pub fn text(&self) -> String {
        let n = NUM.get(self.steps).map_or_else(|| self.steps.to_string(), |n| (*n).to_owned());
        let s = if self.steps == 1 { "" } else { "s" };
        let mut out = match &self.via {
            Some(via) => format!("from {} to {via} and back", self.from),
            None => format!("from {} to {}", self.from, self.to),
        };
        if !self.through.is_empty() {
            out.push_str(&format!(" through {}", self.through.join(" and ")));
        }
        out.push_str(&format!(", in {n} step{s}"));
        out
    }
}

#[derive(PartialEq)]
struct Queued(f64, String);

impl Eq for Queued {}

impl Ord for Queued {
    fn cmp(&self, other: &Self) -> Ordering {
        other.0.total_cmp(&self.0).then_with(|| other.1.cmp(&self.1))
    }
}

impl PartialOrd for Queued {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// The connector over one world and its producer table. Build one per
/// world and keep it: it memoises traits, roads and joins.
pub struct Connector<'a> {
    recipes: &'a Recipes,
    world: &'a World,
    by_in: HashMap<String, Vec<usize>>,
    by_out: HashMap<String, Vec<usize>>,
    trait_nodes: HashMap<String, Vec<NodeId>>,
    traits: RefCell<HashMap<NodeId, HashSet<String>>>,
    roads: RefCell<HashMap<(NodeId, NodeId), Option<Chain>>>,
}

/// A trait path's last name, without generic arguments or lifetimes
/// (`for<'de> de::Deserialize<'de>` → `Deserialize`).
fn trait_word(bound: &str) -> Option<String> {
    let mut b = bound.trim();
    if let Some(rest) = b.strip_prefix("for<")
        && let Some(close) = rest.find('>')
    {
        b = rest[close + 1..].trim();
    }
    let b = b.trim_start_matches('?');
    let head = b.split('<').next().unwrap_or(b).trim();
    let last = head.rsplit("::").next().unwrap_or(head).trim();
    (!last.is_empty() && !last.starts_with('\'') && last.chars().next().is_some_and(char::is_alphabetic)).then(|| last.to_owned())
}

impl<'a> Connector<'a> {
    /// A connector over `world` and its `recipes`.
    #[must_use]
    pub fn new(recipes: &'a Recipes, world: &'a World) -> Self {
        let mut by_in: HashMap<String, Vec<usize>> = HashMap::new();
        let mut by_out: HashMap<String, Vec<usize>> = HashMap::new();
        for (q, e) in recipes.table().iter().enumerate() {
            let mut seen = HashSet::new();
            for k in &e.ins {
                if seen.insert(k) {
                    by_in.entry(k.clone()).or_default().push(q);
                }
            }
            by_out.entry(e.out.clone()).or_default().push(q);
        }
        let mut trait_nodes: HashMap<String, Vec<NodeId>> = HashMap::new();
        for (n, node) in world.nodes.iter().enumerate() {
            if node.kind == Kind::Trait
                && let Ok(n) = NodeId::try_from(n)
            {
                trait_nodes.entry(node.name.to_string()).or_default().push(n);
            }
        }
        Self {
            recipes,
            world,
            by_in,
            by_out,
            trait_nodes,
            traits: RefCell::new(HashMap::new()),
            roads: RefCell::new(HashMap::new()),
        }
    }

    /// Every trait `i` is, by name (see the module's note on blanket impls).
    #[must_use]
    pub fn traits_of(&self, i: NodeId) -> HashSet<String> {
        if let Some(known) = self.traits.borrow().get(&i) {
            return known.clone();
        }
        let world = self.world;
        let node = world.node(i);
        let mut out: HashSet<String> = node.derives.iter().map(ToString::to_string).collect();
        let mut implemented: Vec<NodeId> = Vec::new();
        for imp in &node.impls {
            if let Some(t) = imp.trait_ {
                out.insert(world.node(t).name.to_string());
                implemented.push(t);
            }
        }
        for path in &node.impls_ext {
            if let Some(word) = trait_word(path) {
                out.insert(word);
            }
        }
        // A type alias is its target (`type Table = Map<String, Value>`).
        if node.kind == Kind::Type
            && let Some(target) = self.alias_target(i)
        {
            out.extend(self.traits_of(target));
        }
        // Implementing a trait means being each of its supertraits too.
        let mut seen: HashSet<NodeId> = HashSet::new();
        while let Some(t) = implemented.pop() {
            if !seen.insert(t) {
                continue;
            }
            for word in self.supertraits(t) {
                // The supertrait's own node: in the same package when there
                // is one of that name, else the only one.
                let named = self.trait_nodes.get(&word).map(Vec::as_slice).unwrap_or_default();
                let home = named.iter().copied().find(|&n| world.node(n).pkg == world.node(t).pkg);
                if let Some(s) = home.or(match named {
                    [only] => Some(*only),
                    _ => None,
                }) {
                    implemented.push(s);
                }
                out.insert(word);
            }
        }
        self.traits.borrow_mut().insert(i, out.clone());
        out
    }

    /// A trait's supertraits, by name, from its signature (`trait X: A + B`).
    fn supertraits(&self, t: NodeId) -> Vec<String> {
        let sig = self.world.node(t).sig.as_deref().unwrap_or("");
        let Some(head) = sig.split_once(':').map(|(_, rest)| rest) else {
            return Vec::new();
        };
        let head = head.split(" where ").next().unwrap_or(head);
        let head = head.split('{').next().unwrap_or(head);
        split_top(head, '+').iter().filter_map(|bound| trait_word(bound)).collect()
    }

    /// The node a type alias stands for, when it is one in the world.
    fn alias_target(&self, i: NodeId) -> Option<NodeId> {
        let sig = self.world.node(i).sig.as_deref()?;
        let rhs = sig.split_once('=')?.1.trim().trim_end_matches(';');
        let key = self.recipes.expression_key(self.world, i, None, &parse(rhs));
        key.strip_prefix('#')?.parse().ok()
    }

    /// A key as its alias's target (`#Table` → `#Map`).
    fn target(&self, key: &str) -> String {
        key.strip_prefix('#')
            .and_then(|n| n.parse::<NodeId>().ok())
            .filter(|&n| self.world.node(n).kind == Kind::Type)
            .and_then(|n| self.alias_target(n))
            .map_or_else(|| key.to_owned(), |n| format!("#{n}"))
    }

    /// Whether the type behind `key` is a `t`.
    fn is_a(&self, key: &str, t: &str) -> bool {
        key.strip_prefix('#').and_then(|n| n.parse::<NodeId>().ok()).is_some_and(|n| self.traits_of(n).contains(t))
    }

    /// `i` as a noun.
    #[must_use]
    pub fn piece(&self, i: NodeId) -> Piece {
        let world = self.world;
        let node = world.node(i);
        let own = format!("#{i}");
        let plain = |role| Piece { node: i, role, q: None, ins: Vec::new(), out: None, fails: false, maybe: false };
        match node.kind {
            Kind::Struct | Kind::Enum | Kind::Union | Kind::Type => {
                Piece { node: i, role: Role::Type, q: None, ins: vec![own.clone()], out: Some(own), fails: false, maybe: false }
            }
            Kind::Trait => plain(Role::Trait),
            _ => {
                let table = self.recipes.table();
                let found = table
                    .iter()
                    .position(|e| e.node == i && matches!(e.how, How::Call | How::Method | How::Variant));
                if let Some(q) = found {
                    let e = &table[q];
                    return Piece {
                        node: i,
                        role: Role::Call,
                        q: Some(q),
                        ins: e.ins.iter().filter(|k| *k != "nothing").cloned().collect(),
                        out: Some(e.out.clone()),
                        fails: e.fails,
                        maybe: e.maybe,
                    };
                }
                if node.kind == Kind::Field
                    && let (Some(owner), Some(ty)) = (node.parent, node.ty.as_deref())
                {
                    let out = self.recipes.expression_key(world, i, Some(owner), &parse(ty));
                    return Piece { node: i, role: Role::Call, q: None, ins: vec![format!("#{owner}")], out: Some(out), fails: false, maybe: false };
                }
                plain(Role::Context)
            }
        }
    }

    /// The best road from a value of type `from` to a `to` (the prototype's
    /// `convert`): at most [`MAX_STEPS`] steps, or none.
    #[must_use]
    pub fn convert(&self, from: NodeId, to: NodeId) -> Option<Chain> {
        if let Some(known) = self.roads.borrow().get(&(from, to)) {
            return known.clone();
        }
        let road = self.search(from, to);
        self.roads.borrow_mut().insert((from, to), road.clone());
        road
    }

    #[allow(clippy::too_many_lines)]
    fn search(&self, from: NodeId, to: NodeId) -> Option<Chain> {
        let (world, recipes) = (self.world, self.recipes);
        let have_key = format!("#{from}");
        let want = format!("#{to}");
        let have: HashSet<String> = std::iter::once(have_key.clone()).collect();
        // A trait you have stands for the value of yours that is one.
        let mut traits: HashMap<String, String> = HashMap::new();
        let mut names: Vec<String> = self.traits_of(from).into_iter().collect();
        names.sort();
        for t in names {
            traits.entry(format!("any {t}")).or_insert_with(|| have_key.clone());
        }
        let r0: Run = recipes.run_with_inputs(world, PUBLIC, &have);
        let home: HashSet<u32> = [from, to].iter().map(|&n| world.node(n).pkg).collect();
        let away = |e: &Producer| if home.contains(&world.node(e.node).pkg) { 0.0 } else { 0.6 };
        let others = |e: &Producer, j: usize| {
            let mut s = 0.0;
            for (a, k) in e.ins.iter().enumerate() {
                if a == j || k == "nothing" {
                    continue;
                }
                match r0.cost.get(k) {
                    Some(c) => s += c + 0.3,
                    None => return f64::INFINITY,
                }
            }
            s
        };
        let mut cost: HashMap<String, f64> = HashMap::new();
        let mut best: HashMap<String, (usize, usize)> = HashMap::new();
        let mut done: HashSet<String> = HashSet::new();
        let mut heap = BinaryHeap::new();
        // Wrapping a value in a variant only to ask the enum what it is
        // (`Value::String(t).as_table()`) says nothing; nor does a round trip.
        let hollow = |k: &str, e: &Producer, best: &HashMap<String, (usize, usize)>| {
            if e.how != How::Method || e.ins.first().is_none_or(|first| first != k) {
                return false;
            }
            let Some(&(q, _)) = best.get(k) else { return false };
            let made = recipes.producer(q);
            made.how == How::Variant || made.names.iter().any(|n| n == world.node(e.node).name.as_ref())
        };
        cost.insert(have_key.clone(), 0.0);
        heap.push(Queued(0.0, have_key.clone()));
        for k in traits.keys() {
            cost.insert(k.clone(), 0.1);
            heap.push(Queued(0.1, k.clone()));
        }
        while let Some(Queued(c, k)) = heap.pop() {
            if !done.insert(k.clone()) {
                continue;
            }
            if c > MAX_COST {
                break;
            }
            if k == want {
                continue;
            }
            for &q in self.by_in.get(&k).into_iter().flatten() {
                let e = recipes.producer(q);
                if matches!(e.out.as_str(), "never" | "?" | "nothing")
                    || have.contains(&e.out)
                    || e.ins.contains(&e.out)
                    || !Recipes::usable(world, e, PUBLIC)
                    || (is_ground(&e.out) && e.out != want)
                    || hollow(&k, e, &best)
                    || identity(&world.node(e.node).name)
                {
                    continue;
                }
                let Some(j) = e.ins.iter().position(|x| x == &k) else { continue };
                let s = c + recipes.weight(world, e) + away(e) + others(e, j);
                if s.is_finite() && !done.contains(&e.out) && s < cost.get(&e.out).copied().unwrap_or(f64::INFINITY) {
                    cost.insert(e.out.clone(), s);
                    best.insert(e.out.clone(), (q, j));
                    heap.push(Queued(s, e.out.clone()));
                }
            }
        }
        let mut candidates: Vec<(f64, usize, usize)> = Vec::new();
        for &q in self.by_out.get(&want).into_iter().flatten() {
            let e = recipes.producer(q);
            if have.contains(&e.out) || e.ins.contains(&e.out) || !Recipes::usable(world, e, PUBLIC) || identity(&world.node(e.node).name) {
                continue;
            }
            let mut pick: Option<(f64, usize)> = None;
            for (j, k) in e.ins.iter().enumerate() {
                if !done.contains(k) || hollow(k, e, &best) {
                    continue;
                }
                let s = cost.get(k).copied().unwrap_or(f64::INFINITY) + recipes.weight(world, e) + away(e) + others(e, j);
                if s.is_finite() && pick.is_none_or(|(c, _)| s < c) {
                    pick = Some((s, j));
                }
            }
            if let Some((c, j)) = pick {
                candidates.push((c, q, j));
            }
        }
        candidates.sort_by(|a, b| {
            a.0.total_cmp(&b.0)
                .then_with(|| world.importance(recipes.producer(b.1).node).total_cmp(&world.importance(recipes.producer(a.1).node)))
                .then(a.1.cmp(&b.1))
        });
        for (c, q, j) in candidates {
            if c > MAX_COST {
                break;
            }
            let mut spine = vec![q];
            let mut at = vec![j];
            let mut k = recipes.producer(q).ins[j].clone();
            let mut seen = HashSet::new();
            while !have.contains(&k) && !traits.contains_key(&k) {
                if !seen.insert(k.clone()) || spine.len() > MAX_STEPS {
                    break;
                }
                let Some(&(b, a)) = best.get(&k) else { break };
                spine.insert(0, b);
                at.insert(0, a);
                k = recipes.producer(b).ins[a].clone();
            }
            if (!have.contains(&k) && !traits.contains_key(&k)) || spine.len() > MAX_STEPS {
                continue;
            }
            return Some(Chain { cost: c, spine, at, from: k });
        }
        None
    }

    /// What `k` (given) feeds into `t` (taken), if anything.
    fn link(&self, k: &str, t: &str) -> Option<(f64, JoinHow)> {
        if is_plain(k) || is_plain(t) {
            return None;
        }
        if k == t || self.target(k) == self.target(t) {
            return Some((0.1, JoinHow::Direct));
        }
        if let Some(tt) = t.strip_prefix("any ")
            && self.is_a(k, tt)
        {
            return Some((0.2, JoinHow::Direct));
        }
        if let Some(kt) = k.strip_prefix("any ")
            && self.is_a(t, kt)
        {
            return Some((0.25, JoinHow::As));
        }
        let (a, b) = (k.strip_prefix('#')?.parse().ok()?, t.strip_prefix('#')?.parse().ok()?);
        self.convert(a, b).map(|chain| (1.0 + chain.cost, JoinHow::Steps(chain)))
    }

    /// The verb from `a` into `b`, if `a` feeds `b`.
    #[must_use]
    pub fn join(&self, a: &Piece, b: &Piece) -> Option<Join> {
        let out = a.out.as_ref().filter(|out| a.node != b.node && out.as_str() != "nothing")?;
        let mut best: Option<Join> = None;
        let mut takes: Vec<&String> = b.ins.iter().collect();
        takes.dedup();
        for t in takes {
            if let Some((cost, how)) = self.link(out, t)
                && best.as_ref().is_none_or(|j| cost < j.cost)
            {
                best = Some(Join { cost, how, into: t.clone() });
            }
        }
        best
    }

    /// The hand arranged: `held` in the order it was held.
    #[must_use]
    pub fn arrange(&self, held: &[NodeId]) -> Arrangement {
        let pieces: Vec<Piece> = held.iter().map(|&i| self.piece(i)).collect();
        let at: HashMap<NodeId, usize> = held.iter().enumerate().map(|(n, &i)| (i, n)).collect();
        let (live, rest): (Vec<Piece>, Vec<Piece>) = pieces.into_iter().partition(|p| matches!(p.role, Role::Type | Role::Call));
        let mut joins: HashMap<(NodeId, NodeId), Option<Join>> = HashMap::new();
        let mut join = |a: &Piece, b: &Piece| joins.entry((a.node, b.node)).or_insert_with(|| self.join(a, b)).clone();
        let mut best: Option<((usize, i64, f64), Vec<usize>)> = None;
        for order in permutations(live.len()) {
            let (mut n, mut cost, mut swaps) = (0usize, 0.0f64, 0i64);
            for w in order.windows(2) {
                if let Some(j) = join(&live[w[0]], &live[w[1]]) {
                    n += 1;
                    cost += j.cost;
                }
            }
            for x in 0..order.len() {
                for y in x + 1..order.len() {
                    if at[&live[order[x]].node] > at[&live[order[y]].node] {
                        swaps += 1;
                    }
                }
            }
            let score = (n, -swaps, -cost);
            let better = best.as_ref().is_none_or(|(s, _)| {
                score.0 > s.0 || (score.0 == s.0 && (score.1 > s.1 || (score.1 == s.1 && score.2 > s.2 + 1e-9)))
            });
            if better {
                best = Some((score, order));
            }
        }
        let mut runs: Vec<Vec<Link>> = Vec::new();
        let mut current: Vec<Link> = Vec::new();
        for &k in best.map(|(_, order)| order).unwrap_or_default().iter() {
            let piece = live[k].clone();
            let joined = current.last().and_then(|last: &Link| join(&last.piece, &piece));
            match joined {
                Some(j) => current.push(Link { piece, join: Some(j) }),
                None => {
                    if !current.is_empty() {
                        runs.push(std::mem::take(&mut current));
                    }
                    current.push(Link { piece, join: None });
                }
            }
        }
        if !current.is_empty() {
            runs.push(current);
        }
        let (mut joined, singles): (Vec<Vec<Link>>, Vec<Vec<Link>>) = runs.into_iter().partition(|run| run.len() > 1);
        joined.sort_by(|a, b| b.len().cmp(&a.len()).then(at[&a[0].piece.node].cmp(&at[&b[0].piece.node])));
        let mut apart: Vec<Piece> = singles.into_iter().map(|mut run| run.remove(0).piece).collect();
        // A held trait filters: the roads that go through it say so; one
        // that no road goes through stands apart.
        let mut roads: Vec<Road> = joined.into_iter().map(|links| Road { links, through: Vec::new() }).collect();
        for piece in rest {
            let traited = piece.role == Role::Trait;
            let name = self.world.node(piece.node).name.to_string();
            let mut used = false;
            if traited {
                for road in &mut roads {
                    if self.goes_through(road, &name) {
                        road.through.push(piece.node);
                        used = true;
                    }
                }
            }
            if !used {
                apart.push(piece);
            }
        }
        apart.sort_by_key(|p| at[&p.node]);
        Arrangement { roads, apart }
    }

    /// Whether a road passes through the trait `name`: one of its types is
    /// one, or a join goes through "any `name`".
    fn goes_through(&self, road: &Road, name: &str) -> bool {
        let any = format!("any {name}");
        road.links.iter().any(|link| {
            (link.piece.role == Role::Type && link.piece.out.as_deref().is_some_and(|k| self.is_a(k, name)))
                || link.piece.out.as_deref() == Some(any.as_str())
                || link.join.as_ref().is_some_and(|j| j.into == any)
        })
    }

    /// How many steps a road takes: each call, and each road between.
    #[must_use]
    pub fn steps(&self, road: &Road) -> usize {
        road.links
            .iter()
            .map(|link| {
                usize::from(link.piece.role == Role::Call)
                    + match link.join.as_ref().map(|j| &j.how) {
                        Some(JoinHow::Steps(chain)) => chain.steps(),
                        _ => 0,
                    }
            })
            .sum()
    }

    /// The names a join's road steps through (`as_table`).
    #[must_use]
    pub fn verbs(&self, join: &Join) -> Vec<String> {
        let JoinHow::Steps(chain) = &join.how else { return Vec::new() };
        chain
            .spine
            .iter()
            .map(|&q| {
                let e = self.recipes.producer(q);
                let node = self.world.node(e.node);
                match (e.how, node.parent) {
                    (How::Variant, Some(p)) => format!("{}::{}", self.world.node(p).name, node.name),
                    _ => node.name.to_string(),
                }
            })
            .collect()
    }

    /// A key in words, a type named with its crate when two held types
    /// share its name (`toml Value`).
    fn words(&self, key: &str, twins: &HashSet<String>) -> String {
        if let Some(n) = key.strip_prefix('#').and_then(|n| n.parse::<NodeId>().ok()) {
            let node = self.world.node(n);
            if twins.contains(node.name.as_ref()) {
                return format!("{} {}", self.world.package_short(node.pkg).replace('-', "_"), node.name);
            }
            return node.name.to_string();
        }
        key.strip_prefix("any ").map_or_else(|| key_words(self.world, key), ToOwned::to_owned)
    }

    /// The road's one sentence.
    #[must_use]
    pub fn sentence(&self, road: &Road, twins: &HashSet<String>) -> Sentence {
        let first = &road.links[0].piece;
        let last = &road.links[road.links.len() - 1].piece;
        let from = match first.role {
            Role::Call => first.ins.iter().find(|k| !k.starts_with('#')).or(first.ins.first()).cloned().unwrap_or_default(),
            _ => first.out.clone().unwrap_or_default(),
        };
        let to = last.out.clone().unwrap_or_default();
        let via = (from == to)
            .then(|| road.links.iter().rev().find(|link| link.piece.role == Role::Type))
            .flatten()
            .and_then(|link| link.piece.out.as_deref())
            .map(|k| self.words(k, twins));
        Sentence {
            from: self.words(&from, twins),
            to: self.words(&to, twins),
            via,
            steps: self.steps(road),
            through: road.through.iter().map(|&t| self.world.node(t).name.to_string()).collect(),
        }
    }

    /// A join's road as the code you would write, from the value you have.
    #[must_use]
    pub fn code(&self, chain: &Chain) -> String {
        let mut tree: Option<Tree> = None;
        for (step, (&q, &j)) in chain.spine.iter().zip(&chain.at).enumerate() {
            let e = self.recipes.producer(q);
            let kids = e
                .ins
                .iter()
                .enumerate()
                .map(|(a, key)| {
                    let name = e.names.get(a).cloned().unwrap_or_default();
                    if a == j {
                        match (step, tree.take()) {
                            (0, _) | (_, None) => Kid { key: chain.from.clone(), name, node: None, opaque: false },
                            (_, Some(inner)) => Kid { key: key.clone(), name, node: Some(inner), opaque: false },
                        }
                    } else {
                        Kid { key: key.clone(), name, node: None, opaque: !is_ground(key) }
                    }
                })
                .collect();
            tree = Some(Tree { q, kids });
        }
        tree.map(|tree| chain_code(self.recipes, self.world, &tree)).unwrap_or_default()
    }
}

/// Plain values connect everything, so they connect nothing.
fn is_plain(key: &str) -> bool {
    matches!(key, "text" | "path" | "number" | "bool" | "char" | "bytes" | "nothing" | "any")
}

/// Every order of `0..n` (at most five pieces: 120 orders).
fn permutations(n: usize) -> Vec<Vec<usize>> {
    fn go(rest: &mut Vec<usize>, prefix: &mut Vec<usize>, out: &mut Vec<Vec<usize>>) {
        if rest.is_empty() {
            out.push(prefix.clone());
            return;
        }
        for k in 0..rest.len() {
            let x = rest.remove(k);
            prefix.push(x);
            go(rest, prefix, out);
            prefix.pop();
            rest.insert(k, x);
        }
    }
    let mut out = Vec::new();
    go(&mut (0..n).collect(), &mut Vec::new(), &mut out);
    out
}

#[cfg(test)]
#[path = "chain/tests.rs"]
mod tests;
