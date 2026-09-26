//! The upgrade lens (gui-plan §8.4): what moving from the release you pin to
//! the one you are viewing changes — for a symbol, for the crate, and for
//! the places your code uses it.
//!
//! A pure model ([`Crate`], [`ReleaseDiff`], [`Change`], [`UseSite`],
//! [`Impacted`]) and the two views over it: the shelf's one-line
//! [`summary`] (under W-Controls' version comb) and the page's [`lens`]
//! section (above the anatomy). The elements are in [`view`].
//!
//! Signatures are compared as **trees** ([`TypeExpr`], W-Anatomy's
//! language-neutral type), not as text. A change whose two sides spell the
//! same plain words — a lifetime, `Self` for the owner, `std::` for
//! `core::` — is [`Change::respelled`]: it is counted apart and never
//! alarms you. Otherwise [`marked`] walks both trees together and marks
//! exactly the subtrees that differ: old ones struck, new ones underlined
//! in mint; what they share stays plain (`maybe Value` → `maybe V` marks
//! only `Value` / `V`).
//!
//! Re-exports: the same item reachable at several public paths (`toml::Value`
//! and `toml::value::Value`) is one item. Every path is folded to its
//! declared location (the release's `via`) before changes are counted or
//! listed.

use crate::semantics::types::{Nowhere, Piece, Resolve, Scope, TypeExpr, name_colon, parse, split_top};
use gpui::SharedString;
use std::collections::HashMap;

pub mod view;

#[cfg(any(test, feature = "gallery"))]
pub mod fixture;
#[cfg(any(test, feature = "gallery"))]
pub mod json;
#[cfg(test)]
mod tests;

/// What happened to an item between two releases.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum What {
    /// New in the later release.
    Added,
    /// Gone from the later release.
    Removed,
    /// Its signature (or payload type, or derives) changed.
    Changed,
    /// Newly deprecated.
    Deprecated,
    /// Moved to another path with the same signature.
    Renamed,
    /// A field was added to a struct.
    FieldAdded,
    /// A field was removed from a struct.
    FieldRemoved,
    /// A variant was added to an enum.
    VariantAdded,
    /// A variant was removed from an enum.
    VariantRemoved,
}

impl What {
    /// The row's word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Removed => "removed",
            Self::Changed => "changed",
            Self::Deprecated => "deprecated",
            Self::Renamed => "renamed",
            Self::FieldAdded => "field added",
            Self::FieldRemoved => "field removed",
            Self::VariantAdded => "variant added",
            Self::VariantRemoved => "variant removed",
        }
    }
}

/// Whether a change can break code written against the earlier release.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Severity {
    /// Existing callers may stop compiling.
    Breaking,
    /// Existing callers are unaffected.
    Additive,
}

/// One change to one item.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Change {
    /// The item's stable path, as the release keys it.
    pub path: SharedString,
    /// What happened.
    pub what: What,
    /// Breaking or additive.
    pub severity: Severity,
    /// Its signature (or payload type) before, normalized.
    pub before: Option<SharedString>,
    /// Its signature (or payload type) after, normalized.
    pub after: Option<SharedString>,
}

impl Change {
    /// The item's own name (the last path segment).
    #[must_use]
    pub fn name(&self) -> &str {
        self.path.rsplit("::").next().unwrap_or(&self.path)
    }

    /// Whether both sides read the same in plain words: only the Rust
    /// spelling moved (a lifetime, `Self`, a path's prefix). Only a
    /// `Changed` change can be respelled.
    #[must_use]
    pub fn respelled(&self) -> bool {
        self.respelled_in(&Nowhere)
    }

    /// [`Change::respelled`], resolving names through `resolve`.
    #[must_use]
    pub fn respelled_in(&self, resolve: &dyn Resolve) -> bool {
        if self.what != What::Changed {
            return false;
        }
        let (Some(before), Some(after)) = (&self.before, &self.after) else { return false };
        match (shape_of(before), shape_of(after)) {
            (Some(a), Some(b)) => a.plain(resolve) == b.plain(resolve),
            _ => false,
        }
    }

    /// Whether the later release can fail where the earlier one could not
    /// (its result became `X or fails with E`).
    #[must_use]
    pub fn newly_fails(&self) -> bool {
        let fails = |text: &Option<SharedString>| {
            text.as_deref()
                .and_then(shape_of)
                .and_then(|s| match s {
                    Shape::Sig(sig) => sig.ret,
                    Shape::Type(t) => Some(t),
                    Shape::Text(_) => None,
                })
                .is_some_and(|ret| Scope::new(&Nowhere).fallible(&ret).is_some())
        };
        self.what == What::Changed && !fails(&self.before) && fails(&self.after)
    }
}

/// The API difference between two releases.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseDiff {
    /// The earlier release.
    pub from: SharedString,
    /// The later release.
    pub to: SharedString,
    /// Every change, as the release lists it (aliases not yet folded).
    pub changes: Vec<Change>,
    /// A breaking change shipped inside one caret-compatibility class (a
    /// minor after 1.0, or a patch).
    pub semver_slip: bool,
}

/// One release of a crate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    /// The version as the registry spells it (build metadata kept).
    pub v: SharedString,
    /// When it was published (ISO 8601).
    pub at: SharedString,
    /// Yanked from the registry.
    pub yanked: bool,
    /// Its source is on this machine (so its API can be compared).
    pub local: bool,
}

/// One place your workspace uses one of the crate's items.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct UseSite {
    /// The item used, resolved to its stable path.
    pub path: SharedString,
    /// The file, from the workspace root.
    pub file: SharedString,
    /// The line.
    pub line: u32,
    /// The statement, trimmed.
    pub text: SharedString,
}

impl UseSite {
    /// `dir/file.rs:91` (the last two path segments).
    #[must_use]
    pub fn place(&self) -> String {
        let mut parts: Vec<&str> = self.file.rsplit('/').take(2).collect();
        parts.reverse();
        format!("{}:{}", parts.join("/"), self.line)
    }
}

/// A use site whose item changed, with the change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Impacted {
    /// Where your code uses it.
    pub site: UseSite,
    /// What changed about it.
    pub change: Change,
}

/// One crate's releases, their API differences, and your uses of it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Crate {
    /// Its name.
    pub name: SharedString,
    /// The release your lockfile builds with.
    pub pinned: SharedString,
    /// Every release, oldest first.
    pub versions: Vec<Version>,
    /// Per local release: alias path → the item's declared path (`via`).
    pub aliases: HashMap<SharedString, HashMap<SharedString, SharedString>>,
    /// Pinned → every other local release, and each consecutive pair.
    pub diffs: Vec<ReleaseDiff>,
    /// Every use your workspace makes of the crate's items.
    pub uses: Vec<UseSite>,
    /// Per diff (`from`, `to`): the uses whose item changed.
    pub impact: Vec<(SharedString, SharedString, Vec<Impacted>)>,
}

impl Crate {
    /// The difference from `from` to `to`, if both are local.
    #[must_use]
    pub fn diff(&self, from: &str, to: &str) -> Option<&ReleaseDiff> {
        self.diffs.iter().find(|d| d.from == from && d.to == to)
    }

    /// The uses the move from `from` to `to` touches.
    #[must_use]
    pub fn impact(&self, from: &str, to: &str) -> &[Impacted] {
        self.impact
            .iter()
            .find(|(a, b, _)| a == from && b == to)
            .map_or(&[], |(_, _, list)| list.as_slice())
    }

    /// The declared path of `path` as `version` (then `other`) knows it.
    #[must_use]
    pub fn canonical(&self, path: &str, version: &str, other: &str) -> SharedString {
        for v in [version, other] {
            if let Some(via) = self.aliases.get(v).and_then(|m| m.get(path)) {
                return via.clone();
            }
        }
        SharedString::from(path.to_owned())
    }

    /// The releases whose move from the pin reaches your code: some use of
    /// yours really changes (a respelled item does not count). The version
    /// comb draws these as its Touches tone.
    #[must_use]
    pub fn touches(&self) -> Vec<SharedString> {
        self.versions
            .iter()
            .filter(|v| v.v != self.pinned)
            .filter(|v| self.impact(&self.pinned, &v.v).iter().any(|u| !u.change.respelled()))
            .map(|v| v.v.clone())
            .collect()
    }

    /// The changes from `from` to `to`, re-export aliases folded into one
    /// change each (first listed wins).
    #[must_use]
    pub fn changes(&self, from: &str, to: &str) -> Vec<Change> {
        let Some(diff) = self.diff(from, to) else { return Vec::new() };
        dedupe(&diff.changes, |p| self.canonical(p, to, from))
    }
}

/// The changes without their re-export duplicates: two changes are one when
/// they say the same thing about the same declared item.
#[must_use]
pub fn dedupe(changes: &[Change], canonical: impl Fn(&str) -> SharedString) -> Vec<Change> {
    let mut seen = std::collections::HashSet::new();
    changes
        .iter()
        .filter(|c| seen.insert((c.what, canonical(&c.path), c.before.clone(), c.after.clone())))
        .cloned()
        .collect()
}

// ------------------------------------------------------------------ semver

/// `(major, minor, patch)` of a version (pre-release and build ignored).
#[must_use]
pub fn semver(v: &str) -> (u64, u64, u64) {
    let core = v.split(['+', '-']).next().unwrap_or(v);
    let mut it = core.split('.').map(|p| p.parse::<u64>().unwrap_or(0));
    (it.next().unwrap_or(0), it.next().unwrap_or(0), it.next().unwrap_or(0))
}

/// A version as people read it: `1.1.6+spec-1.1.0` reads `1.1.6`.
#[must_use]
pub fn short(v: &str) -> &str {
    v.split('+').next().unwrap_or(v)
}

// ------------------------------------------------------------------ signatures as trees

/// A function signature: its parameters (receiver dropped) and result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sig {
    /// `(name, type)`; the name as written, `mut ` dropped.
    pub params: Vec<(Option<String>, TypeExpr)>,
    /// The result (`None`: nothing).
    pub ret: Option<TypeExpr>,
    /// Generic parameter names (lifetimes dropped).
    pub generics: Vec<String>,
}

/// What a change's text is, parsed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Shape {
    /// A function or method.
    Sig(Sig),
    /// A type: a field's or variant's payload, or a type alias's target.
    Type(TypeExpr),
    /// Anything else (`enum Value`, a derive list): compared as text.
    Text(String),
}

impl Shape {
    /// The shape in plain words, for comparison.
    fn plain(&self, resolve: &dyn Resolve) -> String {
        match self {
            Self::Sig(sig) => {
                let scope = Scope::new(resolve).generics(sig.generics.iter().cloned());
                let params: Vec<String> = sig
                    .params
                    .iter()
                    .map(|(name, ty)| format!("{} {}", name.as_deref().unwrap_or(""), scope.spell(ty).plain()))
                    .collect();
                let ret = sig.ret.as_ref().map(|r| scope.spell(r).plain()).unwrap_or_default();
                format!("({}) → {ret}", params.join(", "))
            }
            Self::Type(t) => Scope::new(resolve).spell(t).plain(),
            Self::Text(t) => t.clone(),
        }
    }
}

/// The index just past the bracket that closes the one at `open`.
fn close(text: &str, open: usize, a: char, b: char) -> Option<usize> {
    let mut depth = 0i32;
    for (k, c) in text[open..].char_indices() {
        if c == a {
            depth += 1;
        } else if c == b {
            depth -= 1;
            if depth == 0 {
                return Some(open + k + c.len_utf8());
            }
        }
    }
    None
}

/// Parses a normalized function signature (`pub fn name<T>(a: A) -> R where …`).
#[must_use]
pub fn sig(text: &str) -> Option<Sig> {
    let at = find_word(text, "fn")?;
    let rest = &text[at + 2..];
    let name_end = rest.find(|c: char| c == '<' || c == '(')?;
    let mut cursor = at + 2 + name_end;
    let mut generics = Vec::new();
    if text[cursor..].starts_with('<') {
        let end = close(text, cursor, '<', '>')?;
        generics = split_top(&text[cursor + 1..end - 1], ',')
            .into_iter()
            .map(|g| g.trim().to_owned())
            .filter(|g| !g.starts_with('\''))
            .map(|g| g.split([':', '=']).next().unwrap_or(&g).trim().to_owned())
            .filter(|g| !g.is_empty() && !g.starts_with("const "))
            .collect();
        cursor = end;
    }
    let open = cursor + text[cursor..].find('(')?;
    let end = close(text, open, '(', ')')?;
    let params = split_top(&text[open + 1..end - 1], ',')
        .into_iter()
        .map(|p| p.trim().to_owned())
        .filter(|p| !p.is_empty() && !is_receiver(p))
        .map(|p| match name_colon(&p) {
            Some(colon) => {
                let name = p[..colon].trim();
                let name = name.strip_prefix("mut ").unwrap_or(name).trim().to_owned();
                (Some(name), parse(&p[colon + 1..]))
            }
            None => (None, parse(&p)),
        })
        .collect();
    let tail = text[end..].trim();
    let ret = tail.strip_prefix("->").map(|r| {
        let r = r.trim().trim_end_matches(';');
        let r = match find_word(r, "where") {
            Some(w) => &r[..w],
            None => r,
        };
        parse(r.trim())
    });
    Some(Sig { params, ret, generics })
}

fn find_word(text: &str, word: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut from = 0;
    while let Some(k) = text[from..].find(word) {
        let at = from + k;
        let before = at == 0 || !(bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'_');
        let end = at + word.len();
        let after = end >= bytes.len() || !(bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_');
        if before && after {
            return Some(at);
        }
        from = end;
    }
    None
}

fn is_receiver(param: &str) -> bool {
    let p = param.trim();
    let p = p.strip_prefix('&').map_or(p, str::trim_start);
    let p = if p.starts_with('\'') {
        p.split_once(char::is_whitespace).map_or("", |(_, r)| r.trim_start())
    } else {
        p
    };
    let p = p.strip_prefix("mut ").map_or(p, str::trim_start);
    p == "self" || p.starts_with("self:") || p.starts_with("self :")
}

/// What a change's text is: a signature, a type, or plain text.
#[must_use]
pub fn shape_of(text: &str) -> Option<Shape> {
    if let Some(s) = sig(text) {
        return Some(Shape::Sig(s));
    }
    if let Some(at) = find_word(text, "type")
        && let Some(eq) = text[at..].find('=')
    {
        return Some(Shape::Type(parse(text[at + eq + 1..].trim().trim_end_matches(';'))));
    }
    let keyword = ["struct ", "enum ", "trait ", "union ", "const ", "static ", "mod ", "derive(", "impl "];
    if keyword.iter().any(|k| text.contains(k)) {
        return Some(Shape::Text(text.trim().to_owned()));
    }
    Some(Shape::Type(parse(text)))
}

// ------------------------------------------------------------------ marking what differs

/// How one piece of a changed signature reads.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Mark {
    /// Shared by both sides: plain.
    Same,
    /// Only in the earlier release: struck.
    Old,
    /// Only in the later release: underlined in mint.
    New,
}

/// A signature in plain words, piece by piece, with what differs marked.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Marked {
    /// The pieces.
    pub pieces: Vec<(Piece, Mark)>,
    /// The Rust spelling (the tooltip, and ⌥).
    pub source: SharedString,
}

impl Marked {
    /// The words as one string.
    #[must_use]
    pub fn plain(&self) -> String {
        self.pieces.iter().map(|(p, _)| p.text()).collect()
    }

    /// Only the marked words, each run joined, in order.
    #[must_use]
    pub fn marked(&self, mark: Mark) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut open = false;
        for (piece, m) in &self.pieces {
            if *m == mark {
                if !open {
                    out.push(String::new());
                    open = true;
                }
                if let Some(last) = out.last_mut() {
                    last.push_str(piece.text());
                }
            } else {
                open = false;
            }
        }
        out.into_iter().map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()).collect()
    }
}

const HOLE: &str = "\u{27e6}";

/// A type's direct children, and the type with each child replaced by a
/// numbered hole (`⟦0⟧`, `⟦1⟧`, …) — so the speller writes the shared
/// wrapper words and the children can be spelled, and marked, one by one.
fn holes(t: &TypeExpr) -> (Vec<TypeExpr>, TypeExpr) {
    let mut kids = Vec::new();
    let mut hole = |child: &TypeExpr| {
        let k = kids.len();
        kids.push(child.clone());
        TypeExpr::Named { path: vec![format!("{HOLE}{k}")], args: Vec::new() }
    };
    let skeleton = match t {
        TypeExpr::Named { path, args } => TypeExpr::Named { path: path.clone(), args: args.iter().map(&mut hole).collect() },
        TypeExpr::Binding { name, ty } => TypeExpr::Binding { name: name.clone(), ty: Box::new(hole(ty)) },
        TypeExpr::Assoc { base, via, name } => TypeExpr::Assoc { base: Box::new(hole(base)), via: via.clone(), name: name.clone() },
        TypeExpr::Ref { mutable, inner } => TypeExpr::Ref { mutable: *mutable, inner: Box::new(hole(inner)) },
        TypeExpr::Ptr { mutable, inner } => TypeExpr::Ptr { mutable: *mutable, inner: Box::new(hole(inner)) },
        TypeExpr::Slice(inner) => TypeExpr::Slice(Box::new(hole(inner))),
        TypeExpr::Array { inner, len } => TypeExpr::Array { inner: Box::new(hole(inner)), len: len.clone() },
        TypeExpr::Tuple(items) => TypeExpr::Tuple(items.iter().map(&mut hole).collect()),
        TypeExpr::Func { params, ret } => TypeExpr::Func {
            params: params.iter().map(&mut hole).collect(),
            ret: ret.as_ref().map(|r| Box::new(hole(r))),
        },
        TypeExpr::Any(bounds) => TypeExpr::Any(bounds.iter().map(&mut hole).collect()),
        TypeExpr::Never | TypeExpr::Infer => t.clone(),
    };
    (kids, skeleton)
}

/// Whether two trees have the same node at the top (so their children can
/// be compared one to one).
fn same_node(a: &TypeExpr, b: &TypeExpr) -> bool {
    use TypeExpr as T;
    match (a, b) {
        (T::Named { path: p, args: x }, T::Named { path: q, args: y }) => p.last() == q.last() && x.len() == y.len(),
        (T::Binding { name: m, .. }, T::Binding { name: n, .. }) => m == n,
        (T::Assoc { name: m, .. }, T::Assoc { name: n, .. }) => m == n,
        (T::Ref { mutable: m, .. }, T::Ref { mutable: n, .. }) | (T::Ptr { mutable: m, .. }, T::Ptr { mutable: n, .. }) => m == n,
        (T::Slice(_), T::Slice(_)) => true,
        (T::Array { len: m, .. }, T::Array { len: n, .. }) => m == n,
        (T::Tuple(x), T::Tuple(y)) | (T::Any(x), T::Any(y)) => x.len() == y.len(),
        (T::Func { params: x, ret: r }, T::Func { params: y, ret: s }) => x.len() == y.len() && r.is_some() == s.is_some(),
        (T::Never, T::Never) | (T::Infer, T::Infer) => true,
        _ => false,
    }
}

fn all(pieces: Vec<Piece>, mark: Mark) -> Vec<(Piece, Mark)> {
    pieces.into_iter().map(|p| (p, mark)).collect()
}

/// Both trees spelled, with the subtrees that differ marked.
fn mark_types(sa: &Scope<'_>, a: &TypeExpr, sb: &Scope<'_>, b: &TypeExpr) -> (Vec<(Piece, Mark)>, Vec<(Piece, Mark)>) {
    let (pa, pb) = (sa.pieces(a), sb.pieces(b));
    let text = |p: &[Piece]| p.iter().map(Piece::text).collect::<String>();
    if text(&pa) == text(&pb) {
        return (all(pa, Mark::Same), all(pb, Mark::Same));
    }
    if !same_node(a, b) {
        // One side wraps the other (`Deserializer` → `Deserializer or fails
        // with Error`): only the wrapping is marked.
        if let Some(outer) = wrapping(sb, b, &text(&pa), Mark::New) {
            return (all(pa, Mark::Same), outer);
        }
        if let Some(outer) = wrapping(sa, a, &text(&pb), Mark::Old) {
            return (outer, all(pb, Mark::Same));
        }
        return (all(pa, Mark::Old), all(pb, Mark::New));
    }
    let (ka, skel_a) = holes(a);
    let (kb, skel_b) = holes(b);
    let kids: Vec<_> = ka.iter().zip(&kb).map(|(x, y)| mark_types(sa, x, sb, y)).collect();
    let (wa, wb) = (sa.pieces(&skel_a), sb.pieces(&skel_b));
    // The wrapper's own words differ (a path's meaning changed): mark them.
    let (ma, mb) = if text(&wa) == text(&wb) { (Mark::Same, Mark::Same) } else { (Mark::Old, Mark::New) };
    (fill(wa, ma, |k| kids.get(k).map(|(x, _)| x.clone())), fill(wb, mb, |k| kids.get(k).map(|(_, y)| y.clone())))
}

/// A skeleton's pieces with each hole replaced by its child's marked pieces;
/// the wrapper's own pieces take `whole`.
fn fill(skeleton: Vec<Piece>, whole: Mark, kid: impl Fn(usize) -> Option<Vec<(Piece, Mark)>>) -> Vec<(Piece, Mark)> {
    let mut out = Vec::new();
    for piece in skeleton {
        let hole = match &piece {
            Piece::Name { text, .. } => text.strip_prefix(HOLE).and_then(|k| k.parse::<usize>().ok()),
            _ => None,
        };
        match hole.and_then(&kid) {
            Some(pieces) => out.extend(pieces),
            None => out.push((piece, whole)),
        }
    }
    out
}

/// If one of `outer`'s children reads `inner`, `outer` spelled with that
/// child plain and everything else marked `mark`.
fn wrapping(scope: &Scope<'_>, outer: &TypeExpr, inner: &str, mark: Mark) -> Option<Vec<(Piece, Mark)>> {
    let (kids, skeleton) = holes(outer);
    let text = |p: &[Piece]| p.iter().map(Piece::text).collect::<String>();
    let at = kids.iter().position(|kid| text(&scope.pieces(kid)) == inner)?;
    Some(fill(scope.pieces(&skeleton), mark, |k| {
        kids.get(k).map(|kid| all(scope.pieces(kid), if k == at { Mark::Same } else { mark }))
    }))
}

fn push_words(out: &mut Vec<(Piece, Mark)>, text: &'static str, mark: Mark) {
    out.push((Piece::Punct(SharedString::new_static(text)), mark));
}

fn sig_pieces(
    scope: &Scope<'_>,
    params: &[(Option<String>, Vec<(Piece, Mark)>, Mark)],
    ret: Option<Vec<(Piece, Mark)>>,
) -> Vec<(Piece, Mark)> {
    let _ = scope;
    let mut out = Vec::new();
    push_words(&mut out, "(", Mark::Same);
    for (k, (name, ty, name_mark)) in params.iter().enumerate() {
        if k > 0 {
            push_words(&mut out, ", ", Mark::Same);
        }
        if let Some(name) = name {
            // A parameter's name is not a type: quiet mono, never a link.
            out.push((Piece::Prim(SharedString::from(name.clone())), *name_mark));
            out.push((Piece::Space, Mark::Same));
        }
        out.extend(ty.iter().cloned());
    }
    push_words(&mut out, ")", Mark::Same);
    if let Some(ret) = ret {
        out.push((Piece::Space, Mark::Same));
        push_words(&mut out, "→", Mark::Same);
        out.push((Piece::Space, Mark::Same));
        out.extend(ret);
    }
    out
}

/// One side of a change in plain words, nothing marked (an added or removed
/// item, or a respelled one).
#[must_use]
pub fn words(text: &str, resolve: &dyn Resolve) -> Marked {
    let source = SharedString::from(text.to_owned());
    match shape_of(text) {
        Some(Shape::Sig(sig)) => {
            let scope = Scope::new(resolve).generics(sig.generics.iter().cloned());
            let params: Vec<_> = sig
                .params
                .iter()
                .map(|(n, t)| (n.clone(), all(scope.pieces(t), Mark::Same), Mark::Same))
                .collect();
            let ret = sig.ret.as_ref().map(|r| all(scope.pieces(r), Mark::Same));
            Marked { pieces: sig_pieces(&scope, &params, ret), source }
        }
        Some(Shape::Type(t)) => Marked { pieces: all(Scope::new(resolve).pieces(&t), Mark::Same), source },
        _ => Marked { pieces: vec![(Piece::Prim(source.clone()), Mark::Same)], source },
    }
}

/// Both sides of a changed item, what differs marked: old parts struck on
/// the `before` side, new parts underlined on the `after` side.
#[must_use]
pub fn marked(before: &str, after: &str, resolve: &dyn Resolve) -> (Marked, Marked) {
    let (src_a, src_b) = (SharedString::from(before.to_owned()), SharedString::from(after.to_owned()));
    match (shape_of(before), shape_of(after)) {
        (Some(Shape::Sig(a)), Some(Shape::Sig(b))) => {
            let sa = Scope::new(resolve).generics(a.generics.iter().cloned());
            let sb = Scope::new(resolve).generics(b.generics.iter().cloned());
            let (mut pa, mut pb) = (Vec::new(), Vec::new());
            for k in 0..a.params.len().max(b.params.len()) {
                match (a.params.get(k), b.params.get(k)) {
                    (Some((na, ta)), Some((nb, tb))) => {
                        let (ma, mb) = mark_types(&sa, ta, &sb, tb);
                        let names = if na == nb { (Mark::Same, Mark::Same) } else { (Mark::Old, Mark::New) };
                        pa.push((na.clone(), ma, names.0));
                        pb.push((nb.clone(), mb, names.1));
                    }
                    (Some((na, ta)), None) => pa.push((na.clone(), all(sa.pieces(ta), Mark::Old), Mark::Old)),
                    (None, Some((nb, tb))) => pb.push((nb.clone(), all(sb.pieces(tb), Mark::New), Mark::New)),
                    (None, None) => {}
                }
            }
            let (ra, rb) = match (&a.ret, &b.ret) {
                (Some(x), Some(y)) => {
                    let (x, y) = mark_types(&sa, x, &sb, y);
                    (Some(x), Some(y))
                }
                (Some(x), None) => (Some(all(sa.pieces(x), Mark::Old)), None),
                (None, Some(y)) => (None, Some(all(sb.pieces(y), Mark::New))),
                (None, None) => (None, None),
            };
            (
                Marked { pieces: sig_pieces(&sa, &pa, ra), source: src_a },
                Marked { pieces: sig_pieces(&sb, &pb, rb), source: src_b },
            )
        }
        (Some(Shape::Type(a)), Some(Shape::Type(b))) => {
            let (sa, sb) = (Scope::new(resolve), Scope::new(resolve));
            let (ma, mb) = mark_types(&sa, &a, &sb, &b);
            (Marked { pieces: ma, source: src_a }, Marked { pieces: mb, source: src_b })
        }
        _ => {
            let mut x = words(before, resolve);
            let mut y = words(after, resolve);
            if x.plain() != y.plain() {
                x.pieces.iter_mut().for_each(|(_, m)| *m = Mark::Old);
                y.pieces.iter_mut().for_each(|(_, m)| *m = Mark::New);
            }
            (x, y)
        }
    }
}

// ------------------------------------------------------------------ the shelf's one line

/// The crate-wide line under the comb: "73 breaking · 78 added · 5 respelled
/// · none of your 80 uses change".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    /// The release viewed is on this machine (its API could be compared).
    pub local: bool,
    /// Breaking changes that really change something.
    pub breaking: usize,
    /// Additive changes.
    pub added: usize,
    /// Changes that only moved the Rust spelling.
    pub respelled: usize,
    /// Your uses of the crate.
    pub uses: usize,
    /// Your uses whose item really changes.
    pub changing: usize,
    /// Breaking inside a compatibility class: "minor" or "patch".
    pub slip: Option<&'static str>,
}

impl Summary {
    /// The line in segments: `(text, emphasized)` — the counts are
    /// emphasized, the words and separators are not.
    #[must_use]
    pub fn segments(&self, to: &str) -> Vec<(String, bool)> {
        if !self.local {
            return vec![(format!("{} is not on this machine; only its date is known", short(to)), false)];
        }
        let mut out = Vec::new();
        if self.breaking > 0 {
            out.push((self.breaking.to_string(), true));
            out.push((" breaking".to_owned(), false));
        } else {
            out.push(("nothing breaking".to_owned(), false));
        }
        out.push((format!(" · {} added", self.added), false));
        if self.respelled > 0 {
            out.push((format!(" · {} respelled", self.respelled), false));
        }
        if self.uses > 0 {
            let s = if self.uses == 1 { "" } else { "s" };
            if self.changing > 0 {
                out.push((" · ".to_owned(), false));
                out.push((self.changing.to_string(), true));
                out.push((format!(" of your {} use{s} change", self.uses), false));
            } else {
                out.push((format!(" · none of your {} use{s} change", self.uses), false));
            }
        }
        out
    }

    /// The line as plain text.
    #[must_use]
    pub fn words(&self, to: &str) -> String {
        self.segments(to).into_iter().map(|(t, _)| t).collect()
    }

    /// "breaking in a patch release", when a breaking change slipped in.
    #[must_use]
    pub fn slip_words(&self) -> Option<String> {
        self.slip.map(|class| format!("breaking in a {class} release"))
    }
}

/// The shelf line for moving `krate` from `from` to `to`.
#[must_use]
pub fn summary(krate: &Crate, from: &str, to: &str) -> Summary {
    let Some(diff) = krate.diff(from, to) else {
        return Summary { local: false, breaking: 0, added: 0, respelled: 0, uses: krate.uses.len(), changing: 0, slip: None };
    };
    let changes = krate.changes(from, to);
    let respelled = changes.iter().filter(|c| c.respelled()).count();
    let breaking = changes.iter().filter(|c| c.severity == Severity::Breaking && !c.respelled()).count();
    let added = changes.iter().filter(|c| c.severity == Severity::Additive).count();
    let changing = krate.impact(from, to).iter().filter(|u| !u.change.respelled()).count();
    let (a, b) = (semver(from), semver(to));
    let slip = diff.semver_slip.then_some(if a.0 == b.0 && a.1 == b.1 { "patch" } else { "minor" });
    Summary { local: true, breaking, added, respelled, uses: krate.uses.len(), changing, slip }
}

// ------------------------------------------------------------------ the page's section

/// One change to the symbol, as a row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// What happened.
    pub what: What,
    /// The item's name.
    pub name: SharedString,
    /// Its path.
    pub path: SharedString,
    /// Breaking or additive.
    pub severity: Severity,
    /// Only its Rust spelling moved.
    pub respelled: bool,
    /// It can now fail where it could not.
    pub newly_fails: bool,
    /// The earlier side (removed and changed items).
    pub before: Option<Marked>,
    /// The later side (added and changed items).
    pub after: Option<Marked>,
}

/// The upgrade lens on a symbol's page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lens {
    /// The crate.
    pub krate: SharedString,
    /// Viewing a later release than the pin ("Upgrading"), or an earlier one
    /// ("Going back").
    pub forward: bool,
    /// The release viewed, as people read it.
    pub to: SharedString,
    /// The release viewed is on this machine.
    pub local: bool,
    /// Your uses of the crate.
    pub uses: usize,
    /// Your uses whose item really changes (the first 4 are shown).
    pub affected: Vec<Impacted>,
    /// Your uses whose item was only respelled (one quiet line).
    pub respelled: Vec<Impacted>,
    /// This symbol's own changes, re-exports folded (the first 8 shown).
    pub rows: Vec<Row>,
}

/// `a::b::c` reads `b::c`.
fn tail(path: &str) -> String {
    let parts: Vec<&str> = path.rsplit("::").take(2).collect();
    parts.into_iter().rev().collect::<Vec<_>>().join("::")
}

impl Lens {
    /// "Upgrading to 1.1.6" / "Going back to 0.5.11".
    #[must_use]
    pub fn heading(&self) -> String {
        format!("{} to {}", if self.forward { "Upgrading" } else { "Going back" }, self.to)
    }

    /// Whether the section says anything beyond "not on this machine".
    #[must_use]
    pub fn compared(&self) -> bool {
        self.local
    }

    /// "none of the 80 places your code uses toml change" (`None` when your
    /// code does not use the crate).
    #[must_use]
    pub fn your_code(&self) -> Option<String> {
        if self.uses == 0 {
            return None;
        }
        Some(if self.affected.is_empty() {
            format!("none of the {} places your code uses {} change", self.uses, self.krate)
        } else {
            format!("{} of the {} places your code uses {} change", self.affected.len(), self.uses, self.krate)
        })
    }

    /// "4 touch toml::from_str, respelled but the same in plain words:" and
    /// the places (at most [`SITES`], then "and N more").
    #[must_use]
    pub fn respelled_line(&self) -> Option<(String, Vec<String>, Option<String>)> {
        let first = self.respelled.first()?;
        let n = self.respelled.len();
        let distinct: std::collections::HashSet<&str> = self.respelled.iter().map(|u| u.change.path.as_ref()).collect();
        let lead = format!(
            "{n} touch{} {}{}, respelled but the same in plain words: ",
            if n == 1 { "es" } else { "" },
            tail(&first.change.path),
            if distinct.len() > 1 { " and others" } else { "" },
        );
        let places = self.respelled.iter().take(SITES).map(|u| u.site.place()).collect();
        let more = (n > SITES).then(|| format!(" and {} more", n - SITES));
        Some((lead, places, more))
    }

    /// The words under the rows: "and 3 more changes to it".
    #[must_use]
    pub fn more(&self) -> Option<String> {
        (self.rows.len() > ROWS).then(|| format!("and {} more changes to it", self.rows.len() - ROWS))
    }
}

/// Rows the section shows before "and N more".
pub const ROWS: usize = 8;
/// Affected uses the section shows as statements.
pub const SITES: usize = 4;

/// Whether `path` is `symbol` or one of its members, both folded to their
/// declared paths.
///
/// A member's declared path hangs under its owner's shortest public path
/// (`toml::Deserializer::new`), while the owner's own declared path may be a
/// private module (`toml::de::deserializer::Deserializer`): so the owner is
/// folded on its own, both as written and as declared.
fn belongs(krate: &Crate, path: &str, symbol: &str, from: &str, to: &str) -> bool {
    let canonical = |p: &str| krate.canonical(p, to, from);
    let symbol = canonical(symbol);
    let item = canonical(path);
    let owner = |p: &str| p.rsplit_once("::").map(|(owner, _)| canonical(owner));
    item == symbol || owner(path).is_some_and(|o| o == symbol) || owner(&item).is_some_and(|o| o == symbol)
}

/// The lens for `symbol` (a stable path) while `krate` is viewed at `to`
/// against its pin.
#[must_use]
pub fn lens(krate: &Crate, symbol: &str, to: &str, resolve: &dyn Resolve) -> Lens {
    let from = krate.pinned.as_ref();
    let forward = semver(from) < semver(to);
    let impact = krate.impact(from, to);
    let (respelled, affected): (Vec<Impacted>, Vec<Impacted>) = impact.iter().cloned().partition(|u| u.change.respelled_in(resolve));
    let mut rows: Vec<Row> = krate
        .changes(from, to)
        .into_iter()
        .filter(|c| belongs(krate, &c.path, symbol, from, to))
        .map(|c| {
            let is_respelled = c.respelled_in(resolve);
            let (before, after) = match (c.what, &c.before, &c.after) {
                (What::Changed, Some(a), Some(b)) if !is_respelled => {
                    let (x, y) = marked(a, b, resolve);
                    (Some(x), Some(y))
                }
                (What::Changed, _, Some(b)) => (None, Some(words(b, resolve))),
                (_, a, b) => (a.as_deref().map(|t| words(t, resolve)), b.as_deref().map(|t| words(t, resolve))),
            };
            Row {
                what: c.what,
                name: SharedString::from(c.name().to_owned()),
                newly_fails: c.newly_fails(),
                respelled: is_respelled,
                severity: c.severity,
                path: c.path,
                before,
                after,
            }
        })
        .collect();
    // What really changes first, breaking before additive; respelled last.
    rows.sort_by_key(|r| (r.respelled, r.severity != Severity::Breaking));
    Lens {
        krate: krate.name.clone(),
        forward,
        to: SharedString::from(short(to).to_owned()),
        local: krate.diff(from, to).is_some(),
        uses: krate.uses.len(),
        affected,
        respelled,
        rows,
    }
}
