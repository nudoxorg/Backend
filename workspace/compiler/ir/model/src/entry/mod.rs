//! The `Entry` node: symbol, tree edges, and kind payload.
mod location;
mod node;
mod symbol;
mod typed;

use crate::{index::RawRef, kind::Kind, visitor::Visitor};

pub use self::node::Node;

pub use self::{
    location::{ByteSpan, LineCol, SourceFile, SourceLocation, Unlocated},
    symbol::{AttrTok, CfgExpr, Deprecation, DocLink, Symbol, Visibility},
    typed::TypedEntry,
};

/// A single node in the package IR tree.
///
/// Every item in a package---modules, structs, enums, functions, fields,
/// parameters---is represented as an `Entry`. Entries form a tree via
/// parent/child links stored internally.
///
/// # Structure
///
/// An `Entry` bundles three things:
///
/// * **Symbol** — the entry's name, visibility, doc comment, and source
///   location. Accessed via [`Entry::sym`].
/// * **Node** — parent and child indices that define the tree. Accessed via
///   [`Entry::parent`] and [`Entry::children`].
/// * **Kind** — the semantic payload. Accessed via [`Entry::kind`]. This can be
///   either an *owned* [`Kind`] variant (e.g. `Kind::Record(…)`) or a
///   *reference* to another entry, representing a re-export or type alias.
///
/// # Tree structure
///
/// ```text
/// Module "my_pkg"
///   ├── Record "Point"
///   │   ├── Field "x"
///   │   └── Field "y"
///   └── Function "distance"
///       └── Param "other"
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Entry {
    sym: Symbol,
    node: Node,
    kind: EntryInner,
    /// Where this declaration was written.
    ///
    /// # Why this is on `Entry` and not on `Symbol`
    ///
    /// It belongs on `Symbol`, alongside the `source`/`span` pair it
    /// supersedes, and that is where it should end up. It cannot go there
    /// *yet*: `Symbol` has public fields and is built with a struct literal at
    /// 225 sites across 75 files, including three areas under concurrent
    /// ownership and the GUI's separate cargo workspace. Adding a field there
    /// is a single atomic edit to all of them or nothing.
    ///
    /// `Entry` is reachable without that: it is constructed only through
    /// [`Entry::new`] / [`Entry::reference`] and their located counterparts, so
    /// the field can land now and every existing caller keeps compiling with a
    /// value *derived* from the legacy pair — never invented. See
    /// [`SourceLocation::from_legacy`].
    ///
    /// The follow-up is to move this onto `Symbol` and delete `Symbol::source`
    /// and `Symbol::span`, at which point `from_legacy` and this comment both
    /// go away.
    #[serde(default = "SourceLocation::legacy_encoding")]
    location: SourceLocation,
}

impl Entry {
    pub fn sym(&self) -> &Symbol {
        &self.sym
    }

    /// Where this declaration was written.
    ///
    /// Consumers building a source link must match on the variant rather than
    /// reading `sym().source` / `sym().span`: only
    /// [`SourceLocation::Declared`] can be turned into a place a user can go,
    /// and the other variants carry the reason it cannot.
    pub fn location(&self) -> &SourceLocation {
        &self.location
    }

    pub fn parent(&self) -> Option<&RawRef> {
        self.node.parent.as_ref()
    }

    pub fn children(&self) -> &[RawRef] {
        &self.node.children
    }

    pub fn kind(&self) -> &EntryInner {
        &self.kind
    }

    /// Visit every reference this entry holds — kind body, `Type` nominals,
    /// and both `Node` tree edges.
    ///
    /// # Why this is public
    ///
    /// The `Visitor` machinery is crate-private, so from outside `nudox-ir` the
    /// only auditable reference surfaces were `parent()` and `children()` — the
    /// tree edges, which `seal` has always rewritten correctly. That is exactly
    /// where `nudox-store`'s `no_dangling_local_refs_after_seal` looked, so it
    /// asserted the invariant in its own name against the one place it could
    /// not fail, while dangling references sat in the kind bodies and rendered
    /// as `?` for every foreign type in every package.
    ///
    /// A crate that wants an invariant upheld must let its callers check it.
    ///
    /// This walks a clone (`Visitor` exposes only `visit_mut`), so it is for
    /// audits and tests, not for a hot path.
    pub fn for_each_ref(&self, mut f: impl FnMut(&RawRef)) {
        use crate::visitor::Visitor;
        let sink = core::cell::RefCell::new(&mut f);
        let mut probe = self.clone();
        probe.visit_mut(&|r| (sink.borrow_mut())(r));
    }

    /// Visit every [`UnknownType`](crate::kinds::ty::UnknownType) this entry
    /// holds — the reason at every unresolved type position across its signature,
    /// fields and impl-of, however deeply nested.
    ///
    /// The read-only twin of [`for_each_ref`](Self::for_each_ref): a
    /// cross-package mention a producer could not lower into a `Ref::Foreign`
    /// is carried as [`UnknownType::UnresolvedExternal`], which holds no
    /// [`RawRef`] and so is invisible to `for_each_ref`. This is how a
    /// reference-set / audit pass reaches those edges. Rides the same
    /// `#[derive(Visitor)]` walk, so it cannot drift out of sync with the type
    /// graph; being read-only, it needs no clone.
    pub fn for_each_unknown(&self, mut f: impl FnMut(&crate::kinds::ty::UnknownType)) {
        use crate::visitor::Visitor;
        self.visit_unknowns(&mut f);
    }

    /// Construct an owned entry from its symbol, node, and kind.
    ///
    /// The `node` encodes the parent/children structural edges. For tests that
    /// build a [`crate::apply::PristineIntroTable`] directly (bypassing the
    /// `seal` pass), use [`crate::apply::PristineIntroTable::insert_live`] to
    /// record the parent edge separately and pass a `Node` built from
    /// `Node::build(None::<RawRef>, [])` — the table is the authority on
    /// structural edges in the sealed representation.
    ///
    /// The entry's [`location`](Self::location) is **derived** from the
    /// legacy `sym.source` / `sym.span` pair, which cannot carry line
    /// information — so an entry built this way is never
    /// [`SourceLocation::Declared`] and can never be a jump target. A producer
    /// that knows where its declaration is should call
    /// [`Entry::new_located`]; every remaining caller of this constructor is
    /// an open item on `docs/LIMITATIONS.md` L31.
    pub fn new(sym: Symbol, node: Node, kind: Kind) -> Self {
        let location = SourceLocation::from_legacy(&sym.source, &sym.span);
        Self::new_located(sym, node, kind, location)
    }

    /// Construct an owned entry that knows where it was written.
    ///
    /// `location` is the authority; `sym.source` and `sym.span` are expected to
    /// be its [`SourceLocation::legacy_pair`] projection, and a caller that
    /// builds them any other way is asserting two different origins for one
    /// declaration.
    pub fn new_located(sym: Symbol, node: Node, kind: Kind, location: SourceLocation) -> Self {
        Self {
            sym,
            node,
            kind: EntryInner::Owned(kind),
            location,
        }
    }

    /// Construct a re-export / alias entry; see [`Entry::new`] for how its
    /// source location is derived.
    pub fn reference(sym: Symbol, node: Node, idx: RawRef) -> Self {
        let location = SourceLocation::from_legacy(&sym.source, &sym.span);
        Self::reference_located(sym, node, idx, location)
    }

    /// Construct a re-export / alias entry that knows where the *re-export*
    /// was written.
    ///
    /// Note the subject: this is where the `pub use` is, not where the item it
    /// names is declared. Both are useful and they are different places; the
    /// target's own entry carries the other one.
    pub fn reference_located(
        sym: Symbol,
        node: Node,
        idx: RawRef,
        location: SourceLocation,
    ) -> Self {
        Self {
            sym,
            node,
            kind: EntryInner::Reference(idx),
            location,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum EntryInner {
    Owned(Kind),
    Reference(RawRef),
}

impl EntryInner {
    /// Returns the inner [`Kind`] if this is an `Owned` entry, else `None`.
    ///
    /// Use this to match on the specific kind variant after retrieving an
    /// entry from an [`crate::apply::PristineIntroTable`] or [`crate::view::IrView`].
    pub fn as_owned_kind(&self) -> Option<&Kind> {
        match self {
            EntryInner::Owned(k) => Some(k),
            EntryInner::Reference(_) => None,
        }
    }

    /// Returns the [`KindDiscriminant`] of the owned kind, or `None` for a
    /// reference entry.
    pub fn discriminant(&self) -> Option<crate::kind::KindDiscriminant> {
        match self {
            EntryInner::Owned(k) => Some(k.discriminant()),
            EntryInner::Reference(_) => None,
        }
    }
}
