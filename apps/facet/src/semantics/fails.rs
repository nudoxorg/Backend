//! How it fails (gui-plan §8.3): which kinds of an error a callable can
//! give, and which callables give each kind.
//!
//! docs.rs says `Result<Workspace, RuntimeError>` and stops. The index
//! records every mention of a variant in a body (uses, calls, type and has
//! edges). A mention by a callable that *returns* the error builds that
//! kind: a **maker**. A mention by the error's own methods that take a
//! receiver (`fmt`, `source`, `is_retryable`), or by a callable returning
//! something else (`Fault::from_client_error`), only tells kinds apart.
//! Failure spreads through calls: a callable returning E that calls g, which
//! returns E too, can give whatever g gives (`?` carries it up), and the
//! pipe says which call it comes through.
//!
//! This is `graph/fails.js` entry for entry (the golden is
//! `tests/engines.golden`), including its memo: a cycle sees what is known
//! so far, so an answer can depend on which callables were asked first (see
//! the tests). [`KindsLine`] and [`Section`] are what `page.js` shows: the
//! pipe's line under "or fails with E", and the error page's section.

use super::names::{InWorld, Names};
use super::types::{Resolve, Target, split_top};
use crate::graph::model::{Kind, NodeId, Rel, World};
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::rc::Rc;

/// A kind a callable can give, and the call it comes through (`None`: it
/// builds the kind itself).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Given {
    /// The variant.
    pub kind: NodeId,
    /// The callee it is carried up from.
    pub via: Option<NodeId>,
}

/// What a callable can fail with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanFail {
    /// The error type.
    pub error: NodeId,
    /// The kinds it can give: its own first, then each callee's in call order.
    pub kinds: Vec<Given>,
    /// How many kinds the error has.
    pub all: usize,
}

/// Who builds one kind: yours first, then by importance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Makers {
    /// The variant.
    pub kind: NodeId,
    /// The callables that build it directly.
    pub makers: Vec<NodeId>,
}

/// How far an error reaches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reach {
    /// Callables in this world that can fail with it.
    pub can: usize,
    /// Kinds your code tells apart (mentions without making), in kind order.
    pub told: Vec<NodeId>,
}

/// Some kinds, as a line shows them: the first few, then "and N more".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
    /// The kinds shown.
    pub shown: Vec<NodeId>,
    /// How many more there are.
    pub more: usize,
}

impl Listed {
    fn of(kinds: &[NodeId], at_most: usize) -> Self {
        Self { shown: kinds.iter().copied().take(at_most).collect(), more: kinds.len().saturating_sub(at_most) }
    }

    /// `Io, Spawn, DaemonExited or StartTimeout and 2 more`.
    #[must_use]
    pub fn words(&self, world: &World) -> String {
        let names: Vec<&str> = self.shown.iter().map(|&v| world.node(v).name.as_ref()).collect();
        let mut out = match names.split_last() {
            Some((last, rest)) if !rest.is_empty() => format!("{} or {last}", rest.join(", ")),
            _ => names.concat(),
        };
        if self.more > 0 {
            let _ = write!(out, " and {} more", self.more);
        }
        out
    }
}

/// Kinds carried up from one call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Carried {
    /// The kinds (at most four shown).
    pub kinds: Listed,
    /// The callee they come through.
    pub via: NodeId,
}

/// The pipe's line under "or fails with E" (page.js `failKinds`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KindsLine {
    /// The kinds it builds itself (at most six shown); may be empty.
    pub own: Listed,
    /// The kinds carried up, per call (at most three calls).
    pub carried: Vec<Carried>,
    /// How many kinds it gives.
    pub given: usize,
    /// How many kinds the error has.
    pub all: usize,
}

impl KindsLine {
    /// `5 of its 10 kinds`, or `any of its 10 kinds`.
    #[must_use]
    pub fn count(&self) -> String {
        if self.given == self.all {
            format!("any of its {} kinds", self.all)
        } else {
            format!("{} of its {} kinds", self.given, self.all)
        }
    }

    /// The line as read: `Io, Spawn · MissingExecutable through
    /// locald_executable 5 of its 10 kinds`.
    #[must_use]
    pub fn words(&self, world: &World) -> String {
        let mut parts: Vec<String> = Vec::new();
        if !self.own.shown.is_empty() {
            parts.push(self.own.words(world));
        }
        for c in &self.carried {
            parts.push(format!("{} through {}", c.kinds.words(world), world.node(c.via).name));
        }
        format!("{} {}", parts.join(" · "), self.count())
    }
}

/// One row of How it fails: a kind and who builds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailRow {
    /// The variant.
    pub kind: NodeId,
    /// Your code tells this kind apart.
    pub told: bool,
    /// Its makers (at most four); empty: nothing here builds it.
    pub makers: Vec<NodeId>,
    /// How many more makers there are.
    pub more: usize,
}

/// An error page's How it fails section (page.js `howItFails`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    /// One row per kind, in declaration order.
    pub rows: Vec<FailRow>,
    /// Callables in this world that can fail with it.
    pub can: usize,
    /// Kinds your code tells apart.
    pub told: usize,
    /// The kind column's width in characters (the longest kind, capped at
    /// 28, plus one): one column for every row.
    pub column: usize,
}

impl Section {
    /// `12 calls in this world can fail with it · your code tells 1 of its
    /// 10 kinds apart`.
    #[must_use]
    pub fn foot(&self) -> String {
        let mut out = format!("{} call{} in this world can fail with it", self.can, if self.can == 1 { "" } else { "s" });
        if self.told > 0 {
            let _ = write!(out, " · your code tells {} of its {} kinds apart", self.told, self.rows.len());
        }
        out
    }

    /// The section as read, heading to foot.
    #[must_use]
    pub fn words(&self, world: &World) -> String {
        let mut out = String::from("How it fails");
        for row in &self.rows {
            out.push(' ');
            out.push_str(&world.node(row.kind).name);
            out.push(' ');
            if row.makers.is_empty() {
                out.push_str("nothing here builds it");
            } else {
                let names: Vec<&str> = row.makers.iter().map(|&j| world.node(j).name.as_ref()).collect();
                out.push_str(&names.join(" · "));
                if row.more > 0 {
                    let _ = write!(out, " + {}", row.more);
                }
            }
        }
        out.push(' ');
        out.push_str(&self.foot());
        out
    }
}

/// Makers by callable, in first-seen order, each with its kinds.
#[derive(Default)]
struct Direct {
    order: Vec<(NodeId, Vec<NodeId>)>,
    at: HashMap<NodeId, usize>,
}

/// A callable's kinds so far (the memo; a cycle reads it while it fills).
type Known = Rc<RefCell<Vec<Given>>>;

const UNKNOWN: u32 = u32::MAX;
const NONE: u32 = u32::MAX - 1;

/// The engine over one world. Answers are memoised; build one per world
/// (and per page session: see the order note in the module docs).
pub struct Fails<'w> {
    world: &'w World,
    names: &'w Names,
    errors: Box<[Cell<u32>]>,
    aliases: OnceCell<HashMap<u32, NodeId>>,
    direct: RefCell<HashMap<NodeId, Rc<Direct>>>,
    memo: RefCell<HashMap<NodeId, Known>>,
}

fn is_callable(world: &World, j: NodeId) -> bool {
    matches!(world.node(j).kind, Kind::Function | Kind::Method)
}

/// A JS regex `.` stops at these.
fn breaks_line(s: &str) -> bool {
    s.contains(['\n', '\r', '\u{2028}', '\u{2029}'])
}

/// The inside of `[path::]Result<…>`, as fails.js reads a return type
/// (`/^(?:[\w:]+::)?Result\s*<(.*)>$/`).
fn result_args(ret: &str) -> Option<&str> {
    let s = ret.trim();
    let head_end = s.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == ':')).unwrap_or(s.len());
    let head = &s[..head_end];
    let named = head == "Result" || head.strip_suffix("::Result").is_some_and(|p| !p.is_empty());
    let inner = s[head_end..].trim_start().strip_prefix('<')?.strip_suffix('>')?;
    (named && !breaks_line(inner)).then_some(inner)
}

/// The arguments of the first `Result<…>` that runs to the end of an
/// alias's type (`/Result\s*<(.*)>$/`).
fn alias_args(ty: &str) -> Option<&str> {
    let mut from = 0;
    while let Some(at) = ty[from..].find("Result").map(|k| from + k) {
        if let Some(inner) = ty[at + "Result".len()..].trim_start().strip_prefix('<').and_then(|r| r.strip_suffix('>'))
            && !breaks_line(inner)
        {
            return Some(inner);
        }
        from = at + 1;
    }
    None
}

/// `&io::Error<T>` → `Error`: the name fails.js resolves.
fn plain_name(s: &str) -> &str {
    let s = s.trim();
    let s = s.strip_prefix('&').map_or(s, str::trim_start);
    let s = s.find('<').map_or(s, |at| &s[..at]);
    s.rsplit("::").next().unwrap_or(s)
}

impl<'w> Fails<'w> {
    /// An engine over `world`.
    #[must_use]
    pub fn new(world: &'w World, names: &'w Names) -> Self {
        Self {
            world,
            names,
            errors: (0..world.len()).map(|_| Cell::new(UNKNOWN)).collect(),
            aliases: OnceCell::new(),
            direct: RefCell::new(HashMap::new()),
            memo: RefCell::new(HashMap::new()),
        }
    }

    fn resolve(&self, name: &str, from: NodeId) -> Option<NodeId> {
        match (InWorld { world: self.world, names: self.names, from }).named(&[name.to_owned()]) {
            Some(Target::Node(j)) => Some(j),
            _ => None,
        }
    }

    /// Each package's first `type Result` alias.
    fn alias(&self, pkg: u32) -> Option<NodeId> {
        let aliases = self.aliases.get_or_init(|| {
            let mut out = HashMap::new();
            for &a in &self.world.items {
                let n = self.world.node(a);
                if n.kind == Kind::Type && n.name.as_ref() == "Result" {
                    out.entry(n.pkg).or_insert(a);
                }
            }
            out
        });
        aliases.get(&pkg).copied()
    }

    /// The E of `j`'s `Result<_, E>` (or, for a one-argument `Result<T>`, of
    /// its package's `type Result<T> = Result<T, E>`), when E is a type in
    /// this world.
    #[must_use]
    pub fn error_of(&self, j: NodeId) -> Option<NodeId> {
        let cell = &self.errors[j as usize];
        match cell.get() {
            UNKNOWN => {}
            NONE => return None,
            e => return Some(e),
        }
        let e = self.find_error(j);
        cell.set(e.unwrap_or(NONE));
        e
    }

    fn find_error(&self, j: NodeId) -> Option<NodeId> {
        let n = self.world.node(j);
        let args = result_args(n.ret.as_deref()?)?;
        let parts = split_top(args, ',');
        let e = if parts.len() >= 2 {
            self.resolve(plain_name(&parts[1]), j)
        } else {
            // The package's alias; fails.js reads it from the alias's `ty`.
            let alias = self.alias(n.pkg)?;
            let inner = alias_args(self.world.node(alias).ty.as_deref()?)?;
            let parts = split_top(inner, ',');
            if parts.len() >= 2 { self.resolve(plain_name(&parts[1]), alias) } else { None }
        }?;
        matches!(self.world.node(e).kind, Kind::Enum | Kind::Struct | Kind::Type | Kind::Union).then_some(e)
    }

    /// E's kinds: its variants, in declaration order.
    #[must_use]
    pub fn kinds(&self, e: NodeId) -> Vec<NodeId> {
        self.world.kids(e).iter().copied().filter(|&v| self.world.node(v).kind == Kind::Variant).collect()
    }

    /// A maker of E: a callable returning E that is not one of E's own
    /// methods with a receiver.
    fn maker(&self, j: NodeId, e: NodeId) -> bool {
        let n = self.world.node(j);
        is_callable(self.world, j) && self.error_of(j) == Some(e) && !(n.parent == Some(e) && n.recv.is_some())
    }

    fn direct(&self, e: NodeId) -> Rc<Direct> {
        if let Some(d) = self.direct.borrow().get(&e) {
            return d.clone();
        }
        let mut d = Direct::default();
        for v in self.kinds(e) {
            for j in self.world.ins(v, Rel::USES | Rel::CALLS | Rel::TYPE | Rel::HAS) {
                if !self.maker(j, e) {
                    continue;
                }
                let at = *d.at.entry(j).or_insert_with(|| {
                    d.order.push((j, Vec::new()));
                    d.order.len() - 1
                });
                let kinds = &mut d.order[at].1;
                if !kinds.contains(&v) {
                    kinds.push(v);
                }
            }
        }
        let d = Rc::new(d);
        self.direct.borrow_mut().insert(e, d.clone());
        d
    }

    /// What `j` can give: the kinds it builds, then each callee's that
    /// returns the same error, keeping the first call each kind came
    /// through. `None` when `j` does not return an error in this world.
    #[must_use]
    pub fn fails_of(&self, j: NodeId) -> Option<CanFail> {
        let error = self.error_of(j)?;
        let kinds = self.fill(j)?.borrow().clone();
        Some(CanFail { error, kinds, all: self.kinds(error).len() })
    }

    /// The memo entry for `j`, filled on first ask. A cycle back into a
    /// callable still being filled sees its kinds so far.
    fn fill(&self, j: NodeId) -> Option<Known> {
        let e = self.error_of(j)?;
        if let Some(known) = self.memo.borrow().get(&j) {
            return Some(known.clone());
        }
        let out = Rc::new(RefCell::new(Vec::new()));
        self.memo.borrow_mut().insert(j, out.clone());
        let direct = self.direct(e);
        if let Some(&at) = direct.at.get(&j) {
            out.borrow_mut().extend(direct.order[at].1.iter().map(|&kind| Given { kind, via: None }));
        }
        for g in self.world.outs(j, Rel::CALLS) {
            if g == j || !is_callable(self.world, g) || self.error_of(g) != Some(e) {
                continue;
            }
            let Some(theirs) = self.fill(g) else { continue };
            let theirs: Vec<NodeId> = theirs.borrow().iter().map(|x| x.kind).collect();
            let mut mine = out.borrow_mut();
            for kind in theirs {
                if !mine.iter().any(|x| x.kind == kind) {
                    mine.push(Given { kind, via: Some(g) });
                }
            }
        }
        Some(out)
    }

    /// Per kind of E, who builds it directly: yours first, then importance.
    #[must_use]
    pub fn makers_of(&self, e: NodeId) -> Vec<Makers> {
        let mut per: Vec<Makers> = self.kinds(e).into_iter().map(|kind| Makers { kind, makers: Vec::new() }).collect();
        for (j, kinds) in &self.direct(e).order {
            for v in kinds {
                if let Some(m) = per.iter_mut().find(|m| m.kind == *v) {
                    m.makers.push(*j);
                }
            }
        }
        let w = self.world;
        for m in &mut per {
            m.makers.sort_by(|&a, &b| w.yours(b).cmp(&w.yours(a)).then_with(|| w.importance(b).total_cmp(&w.importance(a))));
        }
        per
    }

    /// How many callables in this world can fail with E, and which of its
    /// kinds your code tells apart.
    #[must_use]
    pub fn reach_of(&self, e: NodeId) -> Reach {
        let n = u32::try_from(self.world.len()).unwrap_or(u32::MAX);
        let can = (0..n).filter(|&j| is_callable(self.world, j) && self.error_of(j) == Some(e)).count();
        let mut told = Vec::new();
        for v in self.kinds(e) {
            let tells = self.world.ins(v, Rel::USES | Rel::CALLS | Rel::TYPE).into_iter().any(|j| self.world.yours(j) && !self.maker(j, e));
            if tells {
                told.push(v);
            }
        }
        Reach { can, told }
    }

    /// The pipe's line under "or fails with E": the kinds `j` builds, then
    /// each group carried up from a call, then the count. `None` when it
    /// gives no kinds.
    #[must_use]
    pub fn kinds_line(&self, j: NodeId) -> Option<KindsLine> {
        let r = self.fails_of(j)?;
        if r.kinds.is_empty() || r.all == 0 {
            return None;
        }
        let own: Vec<NodeId> = r.kinds.iter().filter(|g| g.via.is_none()).map(|g| g.kind).collect();
        let mut through: Vec<(NodeId, Vec<NodeId>)> = Vec::new();
        for g in &r.kinds {
            let Some(via) = g.via else { continue };
            match through.iter_mut().find(|(v, _)| *v == via) {
                Some((_, kinds)) => kinds.push(g.kind),
                None => through.push((via, vec![g.kind])),
            }
        }
        Some(KindsLine {
            own: Listed::of(&own, 6),
            carried: through.iter().take(3).map(|(via, kinds)| Carried { kinds: Listed::of(kinds, 4), via: *via }).collect(),
            given: r.kinds.len(),
            all: r.all,
        })
    }

    /// An error's How it fails section: one row per kind with its makers
    /// (four, then "+ N"). `None` for a type with no kinds (a struct error)
    /// or that nothing in this world fails with.
    #[must_use]
    pub fn how_it_fails(&self, e: NodeId) -> Option<Section> {
        let kinds = self.kinds(e);
        if kinds.is_empty() {
            return None;
        }
        let reach = self.reach_of(e);
        if reach.can == 0 {
            return None;
        }
        let rows = self
            .makers_of(e)
            .into_iter()
            .map(|m| FailRow {
                kind: m.kind,
                told: reach.told.contains(&m.kind),
                more: m.makers.len().saturating_sub(4),
                makers: m.makers.into_iter().take(4).collect(),
            })
            .collect();
        let wide = kinds.iter().map(|&v| self.world.node(v).name.encode_utf16().count()).max().unwrap_or(0);
        Some(Section { rows, can: reach.can, told: reach.told.len(), column: wide.min(28) + 1 })
    }
}

#[cfg(test)]
pub(crate) mod tests;
