//! The world: every symbol of a workspace and its typed relations, plus the
//! rollups every view derives from them (top-level items, item edges, their
//! importance, your footprint).
//!
//! Pure data, no GPUI elements. Build it by hand with [`World::new`] (tests,
//! the index) or from the prototype's fixture with [`World::from_json`]
//! (`Nudox-Design-System/v4/graph/world.json`). Everything that depends on
//! order follows the prototype (`layout.mjs`, `app.js`): edges keep their
//! input order in every adjacency list, children come back in node order,
//! and rolled-up edges appear in first-seen order, so ties break the same
//! way in every view.

use gpui::SharedString;
use std::collections::HashMap;
use std::fmt;
use std::ops::{BitAnd, BitOr, BitOrAssign};

/// A node's index in [`World::nodes`].
pub type NodeId = u32;

/// What a symbol is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    /// A struct (or record, class).
    Struct,
    /// An enum (sum type).
    Enum,
    /// An untagged union.
    Union,
    /// A trait (interface, contract).
    Trait,
    /// A type alias.
    Type,
    /// A free function.
    Function,
    /// A method (a member).
    Method,
    /// A macro.
    Macro,
    /// A constant or static.
    Constant,
    /// A field (a member).
    Field,
    /// An enum variant (a member).
    Variant,
    /// Anything else the extractor named.
    Other,
}

impl Kind {
    /// Parses the fixture's kind word.
    #[must_use]
    pub fn parse(word: &str) -> Self {
        match word {
            "struct" => Self::Struct,
            "enum" => Self::Enum,
            "union" => Self::Union,
            "trait" => Self::Trait,
            "type" => Self::Type,
            "function" => Self::Function,
            "method" => Self::Method,
            "macro" => Self::Macro,
            "constant" => Self::Constant,
            "field" => Self::Field,
            "variant" => Self::Variant,
            _ => Self::Other,
        }
    }

    /// The fixture's word for it.
    #[must_use]
    pub const fn text(self) -> &'static str {
        match self {
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Union => "union",
            Self::Trait => "trait",
            Self::Type => "type",
            Self::Function => "function",
            Self::Method => "method",
            Self::Macro => "macro",
            Self::Constant => "constant",
            Self::Field => "field",
            Self::Variant => "variant",
            Self::Other => "other",
        }
    }

    /// Types, traits and aliases: things with a "made of" and a "used by".
    #[must_use]
    pub const fn is_type_like(self) -> bool {
        matches!(
            self,
            Self::Struct | Self::Enum | Self::Union | Self::Trait | Self::Type
        )
    }

    /// Parts of a type (the inner member shells).
    #[must_use]
    pub const fn is_part(self) -> bool {
        matches!(self, Self::Field | Self::Variant)
    }
}

/// A set of relation kinds (bits in the fixture's `rel` order).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rel(pub u16);

impl Rel {
    /// No relation.
    pub const NONE: Self = Self(0);
    /// A type has a field or variant of this type.
    pub const HAS: Self = Self(1 << 0);
    /// A callable takes this type.
    pub const TAKES: Self = Self(1 << 1);
    /// A callable gives (returns) this type.
    pub const GIVES: Self = Self(1 << 2);
    /// A supertrait or `is` bound.
    pub const IS: Self = Self(1 << 3);
    /// A derive.
    pub const DERIVES: Self = Self(1 << 4);
    /// A call.
    pub const CALLS: Self = Self(1 << 5);
    /// Any other use in a body.
    pub const USES: Self = Self(1 << 6);
    /// A field or alias of this type.
    pub const TYPE: Self = Self(1 << 7);
    /// A written impl of this trait.
    pub const IMPL: Self = Self(1 << 8);
    /// The names, in bit order.
    pub const NAMES: [&'static str; 9] = [
        "has", "takes", "gives", "is", "derives", "calls", "uses", "type", "impl",
    ];

    /// The relation named `word` (the fixture's `rel` vocabulary).
    #[must_use]
    pub fn named(word: &str) -> Option<Self> {
        Self::NAMES
            .iter()
            .position(|name| *name == word)
            .map(|bit| Self(1 << bit))
    }

    /// Whether any bit of `mask` is set.
    #[must_use]
    pub const fn any(self, mask: Self) -> bool {
        self.0 & mask.0 != 0
    }

    /// Whether every bit of `mask` is set.
    #[must_use]
    pub const fn contains(self, mask: Self) -> bool {
        self.0 & mask.0 == mask.0
    }

    /// Whether nothing is set.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl BitOr for Rel {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for Rel {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl BitAnd for Rel {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}

/// A package (crate, npm package, …) at one version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Package {
    /// Its name (`backend-present`, `serde_json`).
    pub name: SharedString,
    /// Its version.
    pub version: SharedString,
    /// One of your own packages.
    pub yours: bool,
    /// From a registry (not in the workspace).
    pub external: bool,
    /// Packages it depends on.
    pub deps: Vec<u32>,
}

/// A module of a package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Module {
    /// Its package.
    pub pkg: u32,
    /// `a::b` inside the package (empty for the root).
    pub path: SharedString,
    /// Its source file.
    pub file: SharedString,
}

/// A written `impl Trait for T`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Impl {
    /// The trait, when it is in the world.
    pub trait_: Option<NodeId>,
    /// The impl's line.
    pub line: u32,
    /// The members it writes.
    pub members: Vec<SharedString>,
    /// A blanket or generic impl.
    pub generic: bool,
}

/// One symbol. Items have no parent; members (fields, variants, methods)
/// have their type as parent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    /// What it is.
    pub kind: Kind,
    /// Its name.
    pub name: SharedString,
    /// Its package.
    pub pkg: u32,
    /// Its module.
    pub module: u32,
    /// Its type, for a member.
    pub parent: Option<NodeId>,
    /// First line.
    pub line: u32,
    /// Last line.
    pub end: Option<u32>,
    /// Visibility (`pub`, `pub(crate)`, …).
    pub vis: Option<SharedString>,
    /// The doc's first sentence.
    pub doc: Option<SharedString>,
    /// The signature as written.
    pub sig: Option<SharedString>,
    /// Source file.
    pub file: Option<SharedString>,
    /// A field's or alias's type.
    pub ty: Option<SharedString>,
    /// A callable's return type.
    pub ret: Option<SharedString>,
    /// A method's receiver: `reads`, `changes`, `consumes`.
    pub recv: Option<SharedString>,
    /// A variant's shape: `unit`, `tuple`, `record`.
    pub shape: Option<SharedString>,
    /// A callable's parameters as written.
    pub params: Vec<SharedString>,
    /// Derived traits.
    pub derives: Vec<SharedString>,
    /// Written impls of traits in the world.
    pub impls: Vec<Impl>,
    /// Written impls of traits outside the world.
    pub impls_ext: Vec<SharedString>,
    /// The trait a method arrives through.
    pub via: Option<SharedString>,
    /// Qualifiers (`const`, `async`, `unsafe`).
    pub quals: Vec<SharedString>,
    /// Generic parameters as text.
    pub generics: Option<SharedString>,
    /// Where-clause as text.
    pub where_: Option<SharedString>,
    /// A required trait member.
    pub required: bool,
    /// `#[must_use]`.
    pub must_use: bool,
    /// `#[non_exhaustive]`.
    pub non_exhaustive: bool,
    /// `#[deprecated]`.
    pub deprecated: bool,
    /// Extracted but unreachable from its package's public tree.
    pub orphan: bool,
}

impl Node {
    /// A bare node (tests and hand-built worlds).
    #[must_use]
    pub fn new(kind: Kind, name: impl Into<SharedString>, pkg: u32, module: u32) -> Self {
        Self {
            kind,
            name: name.into(),
            pkg,
            module,
            parent: None,
            line: 0,
            end: None,
            vis: None,
            doc: None,
            sig: None,
            file: None,
            ty: None,
            ret: None,
            recv: None,
            shape: None,
            params: Vec::new(),
            derives: Vec::new(),
            impls: Vec::new(),
            impls_ext: Vec::new(),
            via: None,
            quals: Vec::new(),
            generics: None,
            where_: None,
            required: false,
            must_use: false,
            non_exhaustive: false,
            deprecated: false,
            orphan: false,
        }
    }

    /// The same node as a member of `parent`.
    #[must_use]
    pub const fn member_of(mut self, parent: NodeId) -> Self {
        self.parent = Some(parent);
        self
    }
}

/// A relation: `from` refers to `to` in the ways `rel` says.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Edge {
    /// The referring symbol.
    pub from: NodeId,
    /// The symbol referred to.
    pub to: NodeId,
    /// How.
    pub rel: Rel,
}

/// A rolled-up edge between two top-level items (or modules, packages).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rollup {
    /// From.
    pub from: u32,
    /// To.
    pub to: u32,
    /// The union of the relations rolled into it.
    pub rel: Rel,
    /// How many member-level relations it stands for.
    pub weight: u32,
}

/// Compressed adjacency: row `i` is `dst[off[i]..off[i + 1]]`, in input
/// order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Csr {
    off: Vec<u32>,
    dst: Vec<NodeId>,
    rel: Vec<Rel>,
}

impl Csr {
    /// Builds rows over `n` sources from `(source, target, rel)` triples,
    /// keeping their order within each row (a stable counting sort).
    #[must_use]
    pub fn build(n: usize, triples: impl Iterator<Item = (u32, u32, Rel)> + Clone) -> Self {
        let mut off = vec![0_u32; n + 1];
        let mut count = 0_usize;
        for (a, _, _) in triples.clone() {
            off[a as usize + 1] += 1;
            count += 1;
        }
        for i in 0..n {
            off[i + 1] += off[i];
        }
        let mut at: Vec<u32> = off[..n].to_vec();
        let mut dst = vec![0; count];
        let mut rel = vec![Rel::NONE; count];
        for (a, b, bits) in triples {
            let slot = &mut at[a as usize];
            dst[*slot as usize] = b;
            rel[*slot as usize] = bits;
            *slot += 1;
        }
        Self { off, dst, rel }
    }

    /// Row `i`: `(target, rel)` in input order.
    pub fn row(&self, i: NodeId) -> impl ExactSizeIterator<Item = (NodeId, Rel)> + '_ {
        let (a, b) = self.span(i);
        self.dst[a..b].iter().copied().zip(self.rel[a..b].iter().copied())
    }

    /// Row `i`'s targets.
    #[must_use]
    pub fn targets(&self, i: NodeId) -> &[NodeId] {
        let (a, b) = self.span(i);
        &self.dst[a..b]
    }

    /// Row length.
    #[must_use]
    pub fn degree(&self, i: NodeId) -> usize {
        let (a, b) = self.span(i);
        b - a
    }

    fn span(&self, i: NodeId) -> (usize, usize) {
        let i = i as usize;
        if i + 1 >= self.off.len() {
            return (0, 0);
        }
        (self.off[i] as usize, self.off[i + 1] as usize)
    }
}

/// The whole world. Fields are public so tests and the index can read them;
/// build it with [`World::new`] so the derived tables stay consistent.
#[derive(Clone, Debug, PartialEq)]
pub struct World {
    /// Every package.
    pub packages: Vec<Package>,
    /// Every module.
    pub modules: Vec<Module>,
    /// Every symbol.
    pub nodes: Vec<Node>,
    /// Every relation, member-level, in input order.
    pub edges: Vec<Edge>,
    /// The top-level item of every node (itself for an item).
    pub top: Vec<NodeId>,
    /// Top-level items, in node order.
    pub items: Vec<NodeId>,
    /// Item-level edges, rolled up (self-edges dropped), first-seen order.
    pub item_edges: Vec<Rollup>,
    /// Importance: PageRank over item edges, `(pr / max)^0.35` rounded to
    /// 0.001 (members: half their parent's), as the prototype stores it.
    pub importance: Vec<f32>,
    /// The same, unrounded (layout seeds with it).
    pub importance_exact: Vec<f32>,
    /// Distinct items referring to each item.
    pub in_degree: Vec<u32>,
    /// Weight of references from your packages into each item of another.
    pub yours_in: Vec<u32>,
    kids: Csr,
    out: Csr,
    inn: Csr,
    item_out: Csr,
    item_in: Csr,
}

/// Why a world could not be built.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelError(pub String);

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ModelError {}

impl World {
    /// Builds a world and every derived table.
    ///
    /// # Errors
    /// A node or edge that points outside the tables, or a member whose
    /// parent is itself a member.
    pub fn new(
        packages: Vec<Package>,
        modules: Vec<Module>,
        nodes: Vec<Node>,
        edges: Vec<Edge>,
    ) -> Result<Self, ModelError> {
        let n = nodes.len();
        if u32::try_from(n).is_err() {
            return Err(ModelError(format!("{n} nodes do not fit a NodeId")));
        }
        for (i, module) in modules.iter().enumerate() {
            if module.pkg as usize >= packages.len() {
                return Err(ModelError(format!("module {i} names package {}", module.pkg)));
            }
        }
        for (i, node) in nodes.iter().enumerate() {
            if node.module as usize >= modules.len() || node.pkg as usize >= packages.len() {
                return Err(ModelError(format!(
                    "node {i} ({}) names module {} / package {}",
                    node.name, node.module, node.pkg
                )));
            }
            if let Some(parent) = node.parent {
                let Some(up) = nodes.get(parent as usize) else {
                    return Err(ModelError(format!("node {i} names parent {parent}")));
                };
                if up.parent.is_some() {
                    return Err(ModelError(format!(
                        "node {i} ({}) is a member of a member ({parent})",
                        node.name
                    )));
                }
            }
        }
        for (k, edge) in edges.iter().enumerate() {
            if edge.from as usize >= n || edge.to as usize >= n {
                return Err(ModelError(format!("edge {k} leaves the world")));
            }
        }
        #[allow(clippy::cast_possible_truncation)]
        let top: Vec<NodeId> = (0..n as u32)
            .map(|i| nodes[i as usize].parent.unwrap_or(i))
            .collect();
        #[allow(clippy::cast_possible_truncation)]
        let items: Vec<NodeId> = (0..n as u32)
            .filter(|i| nodes[*i as usize].parent.is_none())
            .collect();
        let kids = Csr::build(
            n,
            nodes
                .iter()
                .enumerate()
                .filter_map(|(i, node)| {
                    #[allow(clippy::cast_possible_truncation)]
                    node.parent.map(|p| (p, i as u32, Rel::NONE))
                }),
        );
        let out = Csr::build(n, edges.iter().map(|e| (e.from, e.to, e.rel)));
        let inn = Csr::build(n, edges.iter().map(|e| (e.to, e.from, e.rel)));

        // Roll member edges up to their items, first-seen order.
        let mut index: HashMap<(u32, u32), usize> = HashMap::new();
        let mut item_edges: Vec<Rollup> = Vec::new();
        for edge in &edges {
            let (a, b) = (top[edge.from as usize], top[edge.to as usize]);
            if a == b {
                continue;
            }
            match index.get(&(a, b)) {
                Some(&k) => {
                    item_edges[k].rel |= edge.rel;
                    item_edges[k].weight += 1;
                }
                None => {
                    index.insert((a, b), item_edges.len());
                    item_edges.push(Rollup {
                        from: a,
                        to: b,
                        rel: edge.rel,
                        weight: 1,
                    });
                }
            }
        }
        let item_out = Csr::build(n, item_edges.iter().map(|e| (e.from, e.to, e.rel)));
        let item_in = Csr::build(n, item_edges.iter().map(|e| (e.to, e.from, e.rel)));

        let mut in_degree = vec![0_u32; n];
        let mut yours_in = vec![0_u32; n];
        for e in &item_edges {
            in_degree[e.to as usize] += 1;
            let (pa, pb) = (nodes[e.from as usize].pkg, nodes[e.to as usize].pkg);
            if packages[pa as usize].yours && !packages[pb as usize].yours {
                yours_in[e.to as usize] += e.weight;
            }
        }
        let (importance_exact, importance) = importance(n, &items, &top, &item_edges);
        Ok(Self {
            packages,
            modules,
            nodes,
            edges,
            top,
            items,
            item_edges,
            importance,
            importance_exact,
            in_degree,
            yours_in,
            kids,
            out,
            inn,
            item_out,
            item_in,
        })
    }

    /// Parses the prototype's fixture (`world.json`).
    ///
    /// # Errors
    /// Malformed JSON, or tables that do not agree (see [`World::new`]).
    pub fn from_json(bytes: &[u8]) -> Result<Self, ModelError> {
        fixture::parse(bytes)
    }

    /// Number of nodes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether the world is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// The node.
    #[must_use]
    pub fn node(&self, i: NodeId) -> &Node {
        &self.nodes[i as usize]
    }

    /// Whether `i` is a top-level item (not a member).
    #[must_use]
    pub fn is_item(&self, i: NodeId) -> bool {
        self.nodes[i as usize].parent.is_none()
    }

    /// The item `i` belongs to (itself for an item).
    #[must_use]
    pub fn top(&self, i: NodeId) -> NodeId {
        self.top[i as usize]
    }

    /// Members of `i`, in node order.
    #[must_use]
    pub fn kids(&self, i: NodeId) -> &[NodeId] {
        self.kids.targets(i)
    }

    /// What `i` refers to: `(target, rel)`, member-level, in input order.
    pub fn out_edges(&self, i: NodeId) -> impl ExactSizeIterator<Item = (NodeId, Rel)> + '_ {
        self.out.row(i)
    }

    /// What refers to `i`: `(source, rel)`, member-level, in input order.
    pub fn in_edges(&self, i: NodeId) -> impl ExactSizeIterator<Item = (NodeId, Rel)> + '_ {
        self.inn.row(i)
    }

    /// Targets of `i`'s out-edges carrying any bit of `mask` (app.js
    /// `outEdges`).
    #[must_use]
    pub fn outs(&self, i: NodeId, mask: Rel) -> Vec<NodeId> {
        self.out
            .row(i)
            .filter(|(_, rel)| rel.any(mask))
            .map(|(j, _)| j)
            .collect()
    }

    /// Sources of `i`'s in-edges carrying any bit of `mask` (app.js
    /// `inEdges`).
    #[must_use]
    pub fn ins(&self, i: NodeId, mask: Rel) -> Vec<NodeId> {
        self.inn
            .row(i)
            .filter(|(_, rel)| rel.any(mask))
            .map(|(j, _)| j)
            .collect()
    }

    /// Item-level out-edges of item `i`: `(target item, rel)`.
    pub fn item_outs(&self, i: NodeId) -> impl ExactSizeIterator<Item = (NodeId, Rel)> + '_ {
        self.item_out.row(i)
    }

    /// Item-level in-edges of item `i`: `(source item, rel)`.
    pub fn item_ins(&self, i: NodeId) -> impl ExactSizeIterator<Item = (NodeId, Rel)> + '_ {
        self.item_in.row(i)
    }

    /// The neighbourhood the graph lights for `i`: item-level for an item,
    /// member-level for a member (app.js `neighbourhood`).
    #[must_use]
    pub fn neighbours(&self, i: NodeId) -> (Vec<(NodeId, Rel)>, Vec<(NodeId, Rel)>) {
        if self.is_item(i) {
            (self.item_outs(i).collect(), self.item_ins(i).collect())
        } else {
            (self.out_edges(i).collect(), self.in_edges(i).collect())
        }
    }

    /// In one of your packages.
    #[must_use]
    pub fn yours(&self, i: NodeId) -> bool {
        self.packages[self.nodes[i as usize].pkg as usize].yours
    }

    /// In your footprint: something your code reaches (by its item).
    #[must_use]
    pub fn reached(&self, i: NodeId) -> bool {
        self.yours_in[self.top(i) as usize] > 0
    }

    /// Importance in `0..=1` (see [`World::importance`]).
    #[must_use]
    pub fn importance(&self, i: NodeId) -> f32 {
        self.importance[i as usize]
    }

    /// The display name: a member carries its type (`Page::new`,
    /// `Page.relations`), as app.js `nameOf`.
    #[must_use]
    pub fn name_of(&self, i: NodeId) -> SharedString {
        let node = &self.nodes[i as usize];
        match node.parent {
            None => node.name.clone(),
            Some(p) => {
                let sep = if node.kind == Kind::Field { "." } else { "::" };
                format!("{}{sep}{}", self.nodes[p as usize].name, node.name).into()
            }
        }
    }

    /// A package's short name (`backend-` dropped).
    #[must_use]
    pub fn package_short(&self, pkg: u32) -> &str {
        let name = self.packages[pkg as usize].name.as_ref();
        name.strip_prefix("backend-").unwrap_or(name)
    }

    /// Where `i` lives: `present::glyph` (and its type, for a member), as
    /// app.js `qual`.
    #[must_use]
    pub fn qual(&self, i: NodeId) -> SharedString {
        let node = &self.nodes[i as usize];
        let mut out = self.package_short(node.pkg).to_owned();
        let path = &self.modules[node.module as usize].path;
        if !path.is_empty() {
            out.push_str("::");
            out.push_str(path);
        }
        if let Some(p) = node.parent {
            out.push_str("::");
            out.push_str(&self.nodes[p as usize].name);
        }
        out.into()
    }

    /// Distinct places that use `i` or one of its members (app.js
    /// `showFocus` "used in N places"): referring items, minus itself.
    #[must_use]
    pub fn used_in(&self, i: NodeId) -> Vec<NodeId> {
        let mut seen: Vec<NodeId> = Vec::new();
        let mut add = |j: NodeId| {
            let t = self.top(j);
            if t != i && !seen.contains(&t) {
                seen.push(t);
            }
        };
        for (j, _) in self.in_edges(i) {
            add(j);
        }
        for &m in self.kids(i) {
            for (j, _) in self.in_edges(m) {
                add(j);
            }
        }
        seen
    }
}

/// PageRank over item edges (rank flows along a dependency: `a` uses `b`
/// gives rank to `b`), 40 rounds, damping 0.85, dangling mass spread
/// evenly. The same arithmetic, in the same order, as `layout.mjs`. Sums to
/// 1 over `items`.
#[must_use]
pub fn pagerank(n: usize, items: &[NodeId], edges: &[Rollup]) -> Vec<f64> {
    let mut out_deg = vec![0.0_f64; n];
    for e in edges {
        out_deg[e.from as usize] += f64::from(e.weight);
    }
    #[allow(clippy::cast_precision_loss)]
    let count = items.len().max(1) as f64;
    let mut pr = vec![0.0_f64; n];
    for &i in items {
        pr[i as usize] = 1.0 / count;
    }
    for _ in 0..40 {
        let mut next = vec![0.0_f64; n];
        let mut dangling = 0.0;
        for &i in items {
            if out_deg[i as usize] == 0.0 {
                dangling += pr[i as usize];
            }
        }
        for e in edges {
            next[e.to as usize] +=
                0.85 * pr[e.from as usize] * f64::from(e.weight) / out_deg[e.from as usize];
        }
        for &i in items {
            next[i as usize] += (0.15 + 0.85 * dangling) / count;
        }
        pr = next;
    }
    pr
}

/// Importance from PageRank: `(pr / max)^0.35` (items) and half the item's
/// value (members), as `(exact f32, rounded to 0.001)` — layout.mjs seeds
/// with the first and writes the second, which every view sorts by.
fn importance(n: usize, items: &[NodeId], top: &[NodeId], edges: &[Rollup]) -> (Vec<f32>, Vec<f32>) {
    let mut imp = vec![0.0_f32; n];
    if items.is_empty() {
        return (imp.clone(), imp);
    }
    let pr = pagerank(n, items, edges);
    let max = items
        .iter()
        .map(|&i| pr[i as usize])
        .fold(f64::MIN, f64::max);
    // layout.mjs keeps `imp` in a Float32Array and writes it rounded to
    // 0.001; members take half their item's unrounded f32 value.
    let round = |v: f32| {
        #[allow(clippy::cast_possible_truncation)]
        let r = ((f64::from(v) * 1000.0).round() / 1000.0) as f32;
        r
    };
    let mut raw = vec![0.0_f32; n];
    for &i in items {
        #[allow(clippy::cast_possible_truncation)]
        {
            raw[i as usize] = (pr[i as usize] / max).powf(0.35) as f32;
        }
    }
    for (i, &t) in top.iter().enumerate() {
        imp[i] = if t as usize == i {
            round(raw[i])
        } else {
            round(raw[t as usize] * 0.5)
        };
        if t as usize != i {
            raw[i] = raw[t as usize] * 0.5;
        }
    }
    (raw, imp)
}

/// The prototype's fixture format.
mod fixture {
    use super::{Edge, Impl, Kind, ModelError, Module, Node, Package, Rel, World};
    use gpui::SharedString;
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct RawWorld {
        packages: Vec<RawPackage>,
        modules: Vec<RawModule>,
        nodes: Vec<RawNode>,
        rel: Vec<String>,
        edges: Vec<(u32, u32, u32)>,
    }

    #[derive(Deserialize)]
    struct RawPackage {
        name: String,
        version: String,
        #[serde(default)]
        yours: bool,
        #[serde(default)]
        external: bool,
        #[serde(default)]
        deps: Vec<u32>,
    }

    #[derive(Deserialize)]
    struct RawModule {
        pkg: u32,
        path: String,
        #[serde(default)]
        file: String,
    }

    #[derive(Deserialize)]
    struct RawImpl {
        #[serde(rename = "trait")]
        trait_: i64,
        #[serde(default)]
        line: u32,
        #[serde(default)]
        members: Vec<String>,
        #[serde(default)]
        generic: bool,
    }

    #[derive(Deserialize)]
    #[allow(clippy::struct_excessive_bools)]
    struct RawNode {
        k: String,
        n: String,
        p: u32,
        m: u32,
        u: i64,
        #[serde(default)]
        l: u32,
        e: Option<u32>,
        v: Option<String>,
        d: Option<String>,
        s: Option<String>,
        f: Option<String>,
        ty: Option<String>,
        ret: Option<String>,
        recv: Option<String>,
        shape: Option<String>,
        #[serde(default)]
        params: Vec<String>,
        #[serde(default)]
        derives: Vec<String>,
        #[serde(default)]
        impls: Vec<RawImpl>,
        #[serde(default, rename = "implsExt")]
        impls_ext: Vec<String>,
        via: Option<String>,
        #[serde(default)]
        quals: Vec<String>,
        #[serde(rename = "gen")]
        generics: Option<String>,
        wh: Option<String>,
        #[serde(default)]
        required: bool,
        #[serde(default, rename = "mustUse")]
        must_use: bool,
        #[serde(default, rename = "nonExhaustive")]
        non_exhaustive: bool,
        #[serde(default)]
        deprecated: bool,
        #[serde(default)]
        orphan: u8,
    }

    fn text(value: Option<String>) -> Option<SharedString> {
        value.filter(|v| !v.is_empty()).map(SharedString::from)
    }

    fn texts(values: Vec<String>) -> Vec<SharedString> {
        values.into_iter().map(SharedString::from).collect()
    }

    pub(super) fn parse(bytes: &[u8]) -> Result<World, ModelError> {
        let raw: RawWorld =
            serde_json::from_slice(bytes).map_err(|e| ModelError(format!("world.json: {e}")))?;
        let mut bits = Vec::with_capacity(raw.rel.len());
        for word in &raw.rel {
            bits.push(
                Rel::named(word)
                    .ok_or_else(|| ModelError(format!("world.json: unknown relation `{word}`")))?,
            );
        }
        let packages = raw
            .packages
            .into_iter()
            .map(|p| Package {
                name: p.name.into(),
                version: p.version.into(),
                yours: p.yours,
                external: p.external,
                deps: p.deps,
            })
            .collect();
        let modules = raw
            .modules
            .into_iter()
            .map(|m| Module {
                pkg: m.pkg,
                path: m.path.into(),
                file: m.file.into(),
            })
            .collect();
        let nodes = raw
            .nodes
            .into_iter()
            .map(|r| Node {
                kind: Kind::parse(&r.k),
                name: r.n.into(),
                pkg: r.p,
                module: r.m,
                parent: u32::try_from(r.u).ok(),
                line: r.l,
                end: r.e,
                vis: text(r.v),
                doc: text(r.d),
                sig: text(r.s),
                file: text(r.f),
                ty: text(r.ty),
                ret: text(r.ret),
                recv: text(r.recv),
                shape: text(r.shape),
                params: texts(r.params),
                derives: texts(r.derives),
                impls: r
                    .impls
                    .into_iter()
                    .map(|i| Impl {
                        trait_: u32::try_from(i.trait_).ok(),
                        line: i.line,
                        members: texts(i.members),
                        generic: i.generic,
                    })
                    .collect(),
                impls_ext: texts(r.impls_ext),
                via: text(r.via),
                quals: texts(r.quals),
                generics: text(r.generics),
                where_: text(r.wh),
                required: r.required,
                must_use: r.must_use,
                non_exhaustive: r.non_exhaustive,
                deprecated: r.deprecated,
                orphan: r.orphan != 0,
            })
            .collect();
        let edges = raw
            .edges
            .into_iter()
            .map(|(from, to, mask)| {
                let mut rel = Rel::NONE;
                for (bit, r) in bits.iter().enumerate() {
                    if mask & (1 << bit) != 0 {
                        rel |= *r;
                    }
                }
                Edge { from, to, rel }
            })
            .collect();
        World::new(packages, modules, nodes, edges)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::{Edge, Kind, Module, Node, Package, Rel, World};

    /// A small hand-built world: two packages (one yours), a type with
    /// members, a function, and relations between them.
    pub(crate) fn tiny() -> World {
        let packages = vec![
            Package {
                name: "backend-app".into(),
                version: "0.1.0".into(),
                yours: true,
                external: false,
                deps: vec![1],
            },
            Package {
                name: "lib".into(),
                version: "1.0.0".into(),
                yours: false,
                external: true,
                deps: vec![],
            },
        ];
        let modules = vec![
            Module { pkg: 0, path: "".into(), file: "src/lib.rs".into() },
            Module { pkg: 1, path: "de".into(), file: "src/de.rs".into() },
        ];
        let nodes = vec![
            Node::new(Kind::Struct, "Page", 0, 0),               // 0
            Node::new(Kind::Field, "relations", 0, 0).member_of(0), // 1
            Node::new(Kind::Method, "new", 0, 0).member_of(0),    // 2
            Node::new(Kind::Function, "from_str", 1, 1),          // 3
            Node::new(Kind::Trait, "Visitor", 1, 1),              // 4
            Node::new(Kind::Enum, "Error", 1, 1),                 // 5
        ];
        let edges = vec![
            Edge { from: 2, to: 3, rel: Rel::CALLS },
            Edge { from: 1, to: 5, rel: Rel::TYPE },
            Edge { from: 3, to: 4, rel: Rel::USES },
            Edge { from: 3, to: 5, rel: Rel::GIVES },
            Edge { from: 2, to: 5, rel: Rel::GIVES },
        ];
        match World::new(packages, modules, nodes, edges) {
            Ok(world) => world,
            Err(error) => panic!("tiny world: {error}"),
        }
    }

    #[test]
    fn rollups_merge_member_edges_in_first_seen_order() {
        let w = tiny();
        let got: Vec<_> = w.item_edges.iter().map(|e| (e.from, e.to, e.rel, e.weight)).collect();
        assert_eq!(
            got,
            vec![
                (0, 3, Rel::CALLS, 1),
                (0, 5, Rel::TYPE | Rel::GIVES, 2),
                (3, 4, Rel::USES, 1),
                (3, 5, Rel::GIVES, 1),
            ]
        );
        assert_eq!(w.kids(0), &[1, 2]);
        assert_eq!(w.top(2), 0);
        assert_eq!(w.name_of(1).as_ref(), "Page.relations");
        assert_eq!(w.name_of(2).as_ref(), "Page::new");
        assert_eq!(w.qual(3).as_ref(), "lib::de");
        assert_eq!(w.qual(2).as_ref(), "app::Page");
    }

    #[test]
    fn footprint_is_what_your_code_reaches() {
        let w = tiny();
        assert!(w.yours(0) && !w.yours(3));
        assert!(w.reached(3) && w.reached(5), "your Page calls from_str and holds Error");
        assert!(!w.reached(4), "Visitor is reached only through from_str, not by your code");
        assert_eq!(w.yours_in[5], 2);
    }

    #[test]
    fn importance_is_pagerank_normalised_to_the_top_item() {
        let w = tiny();
        // Error is the sink everything flows into: the most important item.
        let top = w.items.iter().copied().max_by(|a, b| w.importance(*a).total_cmp(&w.importance(*b)));
        assert_eq!(top, Some(5));
        assert!((w.importance(5) - 1.0).abs() < 1e-6);
        // Members carry half their item.
        assert!((w.importance(1) - w.importance(0) * 0.5).abs() < 0.0011);
        assert!(w.items.iter().all(|&i| (0.0..=1.0).contains(&w.importance(i))));
    }

    #[test]
    fn a_member_of_a_member_is_refused() {
        let modules = vec![Module { pkg: 0, path: "".into(), file: "".into() }];
        let packages = vec![Package {
            name: "p".into(),
            version: "0".into(),
            yours: false,
            external: false,
            deps: vec![],
        }];
        let nodes = vec![
            Node::new(Kind::Struct, "A", 0, 0),
            Node::new(Kind::Field, "a", 0, 0).member_of(0),
            Node::new(Kind::Field, "b", 0, 0).member_of(1),
        ];
        let error = World::new(packages, modules, nodes, vec![]).err();
        assert!(error.is_some_and(|e| e.0.contains("member of a member")));
    }
}
