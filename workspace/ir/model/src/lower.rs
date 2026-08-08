//! Flat, order-independent production API for [`IrPackage`].
//!
//! # Why this exists
//!
//! [`IrPackage::build`] requires a **nested-closure** structure: to create a
//! child you must be lexically inside the parent's closure. That is ergonomic
//! for hand-written test IR but terrible for real language producers (rustdoc
//! JSON, go/types, Roslyn, OXC, …), which receive a **flat list of items with
//! parent pointers**. Every frontend therefore builds its own intermediate tree
//! and then walks it — duplicated machinery that this module eliminates.
//!
//! [`Lowering`] is the flat sink. A producer can:
//!
//! 1. Call [`Lowering::refer`] for any ID it has not yet declared to obtain a
//!    typed [`Ref`] (forward reference).
//! 2. Call [`Lowering::declare`] in any order; the parent need not exist yet.
//! 3. Call [`Lowering::finish`] once to derive the [`Node`] tree, validate, and
//!    produce an [`IrPackage`].
//!
//! [`IrPackage::build`] is **not replaced** — it remains the right API for
//! hand-written IR and every existing test.

use std::{collections::HashSet, fmt, hash::Hash};

use indexmap::IndexMap;
use triomphe::Arc;

use crate::{
    List,
    entry::{Entry, Node, Symbol},
    foreign::ForeignKey,
    index::{RawRef, Ref, UntypedEntryIndex},
    kind::{EntryKind, Kind},
    kinds::{Module, Type},
    package::{IrPackage, PackageId, PackageInfo},
};

// ── Internal representation of one declared item ─────────────────────────────

/// Variant stored per ID in the lowering table.
enum Slot<Id> {
    /// A full owned entry with a kind body.
    Owned {
        sym: Symbol,
        /// `None` means "child of the implicit root module".
        parent: Option<Id>,
        kind: Kind,
    },
    /// A re-export / alias whose target is another entry.
    Reference {
        sym: Symbol,
        /// `None` means "child of the implicit root module".
        parent: Option<Id>,
        target: RawRef,
    },
}

impl<Id> Slot<Id> {
    /// The declared parent, or `None` for "child of the implicit root module".
    fn parent(&self) -> Option<&Id> {
        match self {
            Slot::Owned { parent, .. } | Slot::Reference { parent, .. } => parent.as_ref(),
        }
    }
}

/// Walk state for the parent-chain cycle check in [`Lowering::finish`].
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mark {
    Unseen,
    /// On the chain currently being walked.
    OnPath,
    /// Proven to reach the root without looping.
    Acyclic,
}

// ── Error type ──────────────────────────────────────────────────────────────

/// Errors that [`Lowering::finish`] catches before an [`IrPackage`] is created.
///
/// `finish` collects **all** errors in one pass and reports them together so a
/// producer can fix everything at once rather than in a whack-a-mole loop.
#[derive(Debug)]
pub enum LoweringError<Id: fmt::Debug> {
    /// One or more IDs were referred (or named as a parent) but never declared.
    Undeclared(Vec<Id>),

    /// The same ID was passed to [`Lowering::declare`] (or
    /// [`Lowering::declare_ref`]) more than once.
    Duplicate(Vec<Id>),

    /// A parent-pointer chain loops back to itself.
    ///
    /// # Why this matters
    ///
    /// [`IrPackage::seal`] walks each entry's parent chain in a bare
    /// `while let Some(parent) = ...` loop with no cycle guard.  A cycle from a
    /// buggy producer causes an **infinite loop / hang** at seal time.
    /// Catching it here converts that silent hang into a typed, recoverable
    /// error.
    Cycle(Vec<Id>),
}

impl<Id: fmt::Debug> fmt::Display for LoweringError<Id> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoweringError::Undeclared(ids) => {
                write!(f, "referred but never declared: {:?}", ids)
            }
            LoweringError::Duplicate(ids) => {
                write!(f, "declared more than once: {:?}", ids)
            }
            LoweringError::Cycle(ids) => {
                write!(f, "parent-pointer cycle detected among: {:?}", ids)
            }
        }
    }
}

impl<Id: fmt::Debug> std::error::Error for LoweringError<Id> {}

// ── Lowering ────────────────────────────────────────────────────────────────

/// A flat, order-independent sink for building an [`IrPackage`].
///
/// See the [module documentation](self) for the full usage pattern.
pub struct Lowering<Id: Eq + Hash> {
    /// Canonical index store; mirrors what the nested builder tracks in
    /// `PackageInfo`.  IDs are interned here in first-mention order so that
    /// `refer` and `declare` both produce stable indices.
    info: PackageInfo<Id>,

    /// Declaration table keyed by the *producer* ID, in insertion order
    /// (declaration order). `IndexMap` preserves insertion order so that
    /// `finish` derives children in a deterministic, declaration-ordered
    /// sequence without sorting.
    ///
    /// `Option<Slot>` distinguishes:
    /// - `None`  — the ID was interned via `refer` only; not yet declared.
    /// - `Some`  — the ID was declared (either owned or reference).
    slots: IndexMap<Id, Option<Slot<Id>>>,

    /// IDs that have been declared more than once.  Collected so `finish` can
    /// report all duplicates rather than failing on the first.
    duplicates: Vec<Id>,

    /// Symbol for the implicit root module entry.
    root_sym: Symbol,

    /// Interned cross-package keys, so the 147 references to
    /// `core::clone::Clone` in one crate share one allocation and one later
    /// resolver lookup.
    foreign: HashSet<Arc<ForeignKey>>,
}

impl<Id: Eq + Hash + Clone + fmt::Debug> Lowering<Id> {
    /// Start a new lowering session for `pkg`; `root` is the symbol of the
    /// implicit root module that wraps the whole package.
    pub fn new(pkg: PackageId, root: Symbol) -> Self {
        Lowering {
            info: PackageInfo::new(pkg),
            slots: IndexMap::new(),
            duplicates: Vec::new(),
            root_sym: root,
            foreign: HashSet::new(),
        }
    }

    // ── Internment helpers ────────────────────────────────────────────────────

    /// Intern `id`, returning its arena-local export index. Idempotent: calling
    /// this twice with the same id returns the same index.
    fn intern(&mut self, id: Id) -> UntypedEntryIndex {
        // `create_export` in PackageInfo is already idempotent via IndexSet.
        // We also need to track whether this id has a slot entry.
        self.slots.entry(id.clone()).or_insert(None);
        self.info.create_export(id)
    }

    // ── Public API ────────────────────────────────────────────────────────────

    /// Declare an entry with an owned kind body.
    ///
    /// `parent: None` means "direct child of the root module".
    ///
    /// The parent need not be declared yet — declaration order is irrelevant.
    /// Calling `declare` twice with the same `id` records a duplicate; the
    /// second call is ignored and the error is reported by
    /// [`finish`](Self::finish).
    pub fn declare<T: EntryKind>(
        &mut self,
        id: Id,
        parent: Option<Id>,
        sym: Symbol,
        kind: T,
    ) -> Ref<T> {
        let idx = self.intern(id.clone());

        // Intern the parent so it gets a stable slot even if declared later.
        if let Some(ref p) = parent {
            self.intern(p.clone());
        }

        let slot = self.slots.get_mut(&id).expect("just interned");
        if slot.is_some() {
            // Duplicate declaration — record it; first wins.
            self.duplicates.push(id);
        } else {
            *slot = Some(Slot::Owned {
                sym,
                parent,
                kind: kind.into_kind(),
            });
        }

        Ref::Local(idx.typed())
    }

    /// Declare a re-export / alias entry.
    ///
    /// The flat equivalent of [`EntryBuilder::create_ref`].  `target` is a
    /// [`Ref`] obtained from an earlier [`declare`](Self::declare) or
    /// [`refer`](Self::refer) call.
    pub fn declare_ref<T: EntryKind>(
        &mut self,
        id: Id,
        parent: Option<Id>,
        sym: Symbol,
        target: Ref<T>,
    ) -> Ref<T> {
        let idx = self.intern(id.clone());

        if let Some(ref p) = parent {
            self.intern(p.clone());
        }

        let slot = self.slots.get_mut(&id).expect("just interned");
        if slot.is_some() {
            self.duplicates.push(id);
        } else {
            *slot = Some(Slot::Reference {
                sym,
                parent,
                target: target.into_raw(),
            });
        }

        Ref::Local(idx.typed())
    }

    /// Return `true` if `id` has already been fully declared via
    /// [`declare`](Self::declare) or [`declare_ref`](Self::declare_ref).
    ///
    /// Returns `false` for ids that have only been *referred* (interned but not
    /// yet declared) and for ids that are completely unknown to this session.
    ///
    /// Use this to guard against duplicate impl declarations before emitting
    /// member entries: if an impl id is already declared, emitting members
    /// with that id as their parent would produce dangling parent links once
    /// the duplicate is reported by [`finish`](Self::finish).
    pub fn is_declared(&self, id: &Id) -> bool {
        self.slots.get(id).is_some_and(|s| s.is_some())
    }

    /// Return a typed [`Ref`] for `id` without declaring it.
    ///
    /// The referenced entry may be declared later (before or after this call)
    /// or may never be declared — in which case [`finish`](Self::finish)
    /// reports it as undeclared.
    pub fn refer<T: EntryKind>(&mut self, id: Id) -> Ref<T> {
        let idx = self.intern(id);
        Ref::Local(idx.typed())
    }

    /// Return a typed [`Ref`] to an entry in a **different** package.
    ///
    /// Unlike [`refer`](Self::refer) this creates **no declaration slot**, so
    /// [`finish`](Self::finish) will not demand a declaration for it. That is
    /// why every producer that hit `LoweringError::Undeclared` on stdlib types
    /// belongs here — the Go producer's `refer()`-everything crash (doctrine §4's
    /// worked example) was fixed by routing to this method.
    ///
    /// The returned ref **names** its target rather than indexing an arena
    /// `seal` discards, so it is meaningful in a sealed table. The previous
    /// implementation returned `Ref::Local(import_index)` "to be resolved
    /// later"; nothing ever resolved it, and every consumer rendered it as `?`.
    pub fn refer_import<T: EntryKind>(&mut self, key: ForeignKey) -> Ref<T> {
        Ref::Foreign {
            key: self.intern_foreign(key),
            target: None,
        }
    }

    /// Intern a foreign key so that N references to `core::clone::Clone` share
    /// one allocation and, later, one resolver lookup.
    fn intern_foreign(&mut self, key: ForeignKey) -> Arc<ForeignKey> {
        if let Some(existing) = self.foreign.get(&key) {
            return existing.clone();
        }
        let arc = Arc::new(key);
        self.foreign.insert(arc.clone());
        arc
    }

    // ── Type construction ─────────────────────────────────────────────────────
    //
    // These exist because building a nominal type by hand is a three-step dance
    // — `refer` for a `Ref<T>`, `into_raw` to erase the marker, then wrap in
    // `Type::Nominal` — and the step needs the sink, which per-language type
    // lowering helpers typically do not have in scope. Every producer that
    // lacked these fell back to `Type::Any` for *all* named types, which erases
    // the entire type graph. Making the correct thing a one-liner is the fix.

    /// A nominal reference to a declared type: `Foo`.
    ///
    /// Order-independent — `id` need not be declared yet, so a field may name a
    /// type defined later in the same package with no pre-pass.
    ///
    /// Prefer this over hand-building `Type::Nominal`; if you find yourself
    /// reaching for [`Type::Any`] because a helper cannot see the sink, thread
    /// the sink instead.
    pub fn nominal<T: EntryKind>(&mut self, id: Id) -> Type {
        Type::Nominal(self.refer::<T>(id).into_raw())
    }

    /// A nominal reference to a type in a **different** package.
    ///
    /// There is no kind marker: the old `refer_import::<T>` marker was consumed
    /// by `.typed()` and thrown away on the next line, so producers passed
    /// placeholders (`IrModule` for every Rust target, `Record` for every Go
    /// target) and nothing noticed. [`ForeignKey::kind`] is the honest field —
    /// state it when you know it, omit it when you do not.
    pub fn nominal_import(&mut self, key: ForeignKey) -> Type {
        Type::Nominal(self.refer_import::<Module>(key).into_raw())
    }

    /// `Foo<A, B>` where `Foo` lives in another package.
    ///
    /// With no arguments this is exactly [`nominal_import`](Self::nominal_import),
    /// so a producer lowering a possibly-generic foreign type can call it
    /// unconditionally.
    pub fn apply_import(&mut self, key: ForeignKey, args: impl IntoIterator<Item = Type>) -> Type {
        let args: List<Type> = args.into_iter().collect();
        let base = self.nominal_import(key);
        if args.is_empty() {
            base
        } else {
            Type::Apply {
                base: Box::new(base),
                args,
            }
        }
    }

    /// A generic application of a declared type: `Foo<A, B>`.
    ///
    /// With no arguments this is just [`nominal`](Self::nominal) — callers
    /// lowering a possibly-generic type can use this unconditionally.
    pub fn apply<T: EntryKind>(&mut self, id: Id, args: impl IntoIterator<Item = Type>) -> Type {
        let args: List<Type> = args.into_iter().collect();
        let base = self.nominal::<T>(id);
        if args.is_empty() {
            base
        } else {
            Type::Apply {
                base: Box::new(base),
                args,
            }
        }
    }

    // ── finish ────────────────────────────────────────────────────────────────

    /// Derive the [`Node`] tree from the recorded parent pointers, validate the
    /// package, and return the finished [`IrPackage`].
    ///
    /// # Errors
    ///
    /// Returns the **first** error category found (in priority order):
    /// duplicates → undeclared → cycles.  This ordering is deliberate:
    /// duplicates and undeclared entries make cycle detection unreliable, so
    /// they are reported first.
    ///
    /// # Complexity
    ///
    /// O(n) where n is the number of declared entries:
    /// - one pass to collect children per parent (insertion-ordered),
    /// - one DFS pass for cycle detection (linear in the parent-forest),
    /// - one pass to build and assemble `Entry` values.
    pub fn finish(self) -> Result<IrPackage<Id>, LoweringError<Id>> {
        let Lowering {
            mut info,
            slots,
            duplicates,
            root_sym,
            // Interning is a build-time allocation optimisation only: every
            // `Ref::Foreign` already owns an `Arc` to its key, so the set has
            // no readers after this point.
            foreign: _,
        } = self;

        // ── Validate: duplicates ──────────────────────────────────────────────
        if !duplicates.is_empty() {
            return Err(LoweringError::Duplicate(duplicates));
        }

        // ── Validate: undeclared ──────────────────────────────────────────────
        // Any slot that is still `None` was referred but never declared.
        let undeclared: Vec<Id> = slots
            .iter()
            .filter_map(|(id, slot)| {
                if slot.is_none() {
                    Some(id.clone())
                } else {
                    None
                }
            })
            .collect();
        if !undeclared.is_empty() {
            return Err(LoweringError::Undeclared(undeclared));
        }

        let n = slots.len();

        // Resolve every entry's parent to a *position* in `slots` exactly once
        // (`None` = the implicit root). Both the cycle check and the child
        // derivation read this one table, so no id is hashed twice.
        let parent_pos: Vec<Option<usize>> = slots
            .values()
            .map(|slot| {
                slot.as_ref()
                    .expect("undeclared slots rejected above")
                    .parent()
                    .and_then(|pid| slots.get_index_of(pid))
            })
            .collect();

        // ── Validate: cycles ──────────────────────────────────────────────────
        //
        // Every entry has at most one parent, so the parent graph is
        // *functional* (out-degree ≤ 1). That makes cycle detection a chain
        // walk rather than a graph search: follow parents from each unvisited
        // entry, marking the path; the chain ends at the root, at an entry
        // already proven acyclic, or back on itself — and the last of those is
        // exactly the cycle. Every entry is marked `Acyclic` at most once, so
        // the whole scan is O(n).
        {
            let mut mark = vec![Mark::Unseen; n];
            let mut path: Vec<usize> = Vec::new();

            for start in 0..n {
                if mark[start] != Mark::Unseen {
                    continue;
                }
                let mut cur = Some(start);
                while let Some(pos) = cur {
                    match mark[pos] {
                        // Joined a chain already known to terminate.
                        Mark::Acyclic => break,
                        // Re-entered the walk in progress: the cycle is the
                        // path suffix from where we re-entered.
                        Mark::OnPath => {
                            let from = path.iter().position(|&p| p == pos).expect("marked OnPath");
                            return Err(LoweringError::Cycle(
                                path[from..]
                                    .iter()
                                    .map(|&p| slots.get_index(p).expect("in range").0.clone())
                                    .collect(),
                            ));
                        }
                        Mark::Unseen => {
                            mark[pos] = Mark::OnPath;
                            path.push(pos);
                            cur = parent_pos[pos];
                        }
                    }
                }
                for &p in &path {
                    mark[p] = Mark::Acyclic;
                }
                path.clear();
            }
        }

        // ── Derive children ───────────────────────────────────────────────────
        //
        // `slots` iterates in insertion order, which *is* declaration order, so
        // one append pass yields deterministic child lists with no sorting.
        let mut children: Vec<Vec<usize>> = vec![Vec::new(); n];
        let mut root_children: Vec<usize> = Vec::new();
        for (pos, parent) in parent_pos.iter().enumerate() {
            match parent {
                Some(p) => children[*p].push(pos),
                None => root_children.push(pos),
            }
        }

        // Position → arena index. `create_export` interns, so each of these is
        // the very index `declare`/`refer` already handed back to the producer.
        let idx_of: Vec<UntypedEntryIndex> = slots
            .keys()
            .map(|id| info.create_export(id.clone()))
            .collect();
        let root_idx = info.root_export();

        // ── Build the entries ─────────────────────────────────────────────────
        let mut entries: Vec<(UntypedEntryIndex, Entry)> = Vec::with_capacity(n + 1);

        entries.push((
            root_idx,
            Entry::new(
                root_sym,
                Node::build(
                    None::<RawRef>,
                    root_children.iter().map(|&p| Ref::Local(idx_of[p])),
                ),
                Module.into_kind(),
            ),
        ));

        for (pos, slot) in slots.into_values().enumerate() {
            let node = Node::build(
                Some(Ref::Local(parent_pos[pos].map_or(root_idx, |p| idx_of[p]))),
                children[pos].iter().map(|&p| Ref::Local(idx_of[p])),
            );
            let entry = match slot.expect("undeclared slots rejected above") {
                Slot::Owned { sym, kind, .. } => Entry::new(sym, node, kind),
                Slot::Reference { sym, target, .. } => Entry::reference(sym, node, target),
            };
            entries.push((idx_of[pos], entry));
        }

        Ok(IrPackage::from_parts(info, entries))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use crate::{
        change::{EcosystemId, PackageLineageId, PackageName},
        index::Ref,
        kinds::{Field, FieldKey, Module, Record, Type},
        package::{IrPackage, PackageId},
        test_helpers::sym,
    };

    use super::{Lowering, LoweringError};
    use crate::foreign::Unlinked;

    fn lineage() -> PackageLineageId {
        PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("test-lower"))
    }

    // ── 1. Order independence ─────────────────────────────────────────────────

    /// Declare a child *before* its parent; `finish` must still wire the tree
    /// correctly.
    #[test]
    fn child_declared_before_parent() {
        let mut low: Lowering<usize> = Lowering::new(PackageId::path("pkg"), sym("root"));

        // Declare child first.
        let child_ref: Ref<Field> = low.declare(
            2,
            Some(1), // parent not yet declared
            sym("x"),
            Field::builder().key(FieldKey::Named).ty(Type::I32).build(),
        );

        // Then declare the parent.
        low.declare(
            1,
            None,
            sym("Point"),
            Record::builder().fields([child_ref]).build(),
        );

        let pkg = low.finish().expect("finish must succeed");

        // root + Point + x = 3 entries.
        assert_eq!(pkg.iter().count(), 3);

        // The child's parent must be Point, and Point's parent must be root.
        let point = pkg
            .iter()
            .find(|(_, e)| e.sym().name == "Point")
            .expect("Point not found");
        assert!(
            point.1.parent().is_some(),
            "Point must have a parent (the root module)"
        );

        let x = pkg
            .iter()
            .find(|(_, e)| e.sym().name == "x")
            .expect("x not found");
        assert!(x.1.parent().is_some(), "x must have a parent (Point)");
    }

    // ── 2. Parity with IrPackage::build ──────────────────────────────────────

    /// Build the same logical package via `IrPackage::build` and via
    /// `Lowering`; after sealing, the IntroId sets must be identical.
    #[test]
    fn lowering_parity_with_build() {
        // ── Build via nested closures ─────────────────────────────────────────
        let pkg_nested = IrPackage::build(PackageId::path("pkg"), sym("root"), |mut root| {
            root.create(1usize, sym("Point"), |mut rec| {
                let x = rec.create(2usize, sym("x"), |_| {
                    Field::builder().key(FieldKey::Named).ty(Type::I32).build()
                });
                Record::builder().fields([x]).build()
            });
        });

        // ── Build via Lowering ────────────────────────────────────────────────
        let mut low: Lowering<usize> = Lowering::new(PackageId::path("pkg"), sym("root"));
        let x_ref: Ref<Field> = low.refer(2);
        low.declare(
            1,
            None,
            sym("Point"),
            Record::builder().fields([x_ref]).build(),
        );
        low.declare(
            2,
            Some(1),
            sym("x"),
            Field::builder().key(FieldKey::Named).ty(Type::I32).build(),
        );
        let pkg_flat = low.finish().expect("finish must succeed");

        // ── Compare via seal ──────────────────────────────────────────────────
        let lin = lineage();
        let table_nested = pkg_nested.seal(&lin, &Unlinked).table;
        let table_flat = pkg_flat.seal(&lin, &Unlinked).table;

        assert_eq!(
            table_nested.len(),
            table_flat.len(),
            "both packages must have the same number of entries"
        );

        // Every IntroId in the nested table must exist in the flat table.
        for (intro, _) in table_nested.iter() {
            assert!(
                table_flat.contains(intro),
                "IntroId {intro:?} from nested build missing in flat build"
            );
        }
    }

    // ── 2b. Type construction on the sink ────────────────────────────────────

    /// The defect these helpers exist to prevent: four independent producers
    /// lowered every named type to `Type::Any`, because building a nominal type
    /// needed the sink and their type helpers did not have it. A field naming a
    /// type declared *later* must survive as a real reference through `seal`.
    #[test]
    fn nominal_survives_forward_reference_and_seal() {
        let mut low: Lowering<usize> = Lowering::new(PackageId::path("pkg"), sym("root"));

        // `Holder.value: Payload` — Payload is not declared until below.
        let value_ty = low.nominal::<Record>(2);
        let field = low.declare(
            3,
            Some(1),
            sym("value"),
            Field::builder().key(FieldKey::Named).ty(value_ty).build(),
        );
        low.declare(
            1,
            None,
            sym("Holder"),
            Record::builder().fields([field]).build(),
        );
        low.declare(2, None, sym("Payload"), Record::builder().build());

        let table = low
            .finish()
            .expect("forward-referenced nominal must resolve")
            .seal(&lineage(), &Unlinked).table;

        let (_, value) = table
            .iter()
            .find(|(_, e)| e.sym().name == "value")
            .expect("field must be sealed");
        let payload = table
            .iter()
            .find(|(_, e)| e.sym().name == "Payload")
            .expect("Payload must be sealed")
            .0;

        match value.kind().as_owned_kind() {
            Some(crate::kind::Kind::Field(f)) => {
                match f.ty.as_ref().expect("field must have a type") {
                    // Post-seal the ref is content-addressed and points at Payload.
                    Type::Nominal(Ref::Intro(id)) => assert_eq!(
                        *id, payload,
                        "the nominal must resolve to Payload's IntroId"
                    ),
                    other => panic!("expected a lowered Type::Nominal, got {other:?}"),
                }
            }
            other => panic!("expected a Field, got {other:?}"),
        }
    }

    /// `apply` with no arguments is exactly `nominal`, so a producer lowering a
    /// possibly-generic type can call it unconditionally.
    #[test]
    fn apply_without_args_is_plain_nominal() {
        let mut low: Lowering<usize> = Lowering::new(PackageId::path("pkg"), sym("root"));
        let bare = low.apply::<Record>(1, []);
        let nominal = low.nominal::<Record>(1);
        assert_eq!(bare, nominal);

        let generic = low.apply::<Record>(1, [Type::I32]);
        assert!(
            matches!(generic, Type::Apply { .. }),
            "with args it must be an Apply, got {generic:?}"
        );
    }

    // ── 3. Forward reference in kind body ────────────────────────────────────

    /// A Record whose `fields` are `refer(field_id)` calls made *before* those
    /// fields are declared.  `finish` must resolve the tree correctly.
    #[test]
    fn forward_ref_in_kind_body() {
        let mut low: Lowering<usize> = Lowering::new(PackageId::path("pkg"), sym("root"));

        // Fields not declared yet; refer to them for use in Record.fields.
        let f1: Ref<Field> = low.refer(10);
        let f2: Ref<Field> = low.refer(11);

        low.declare(
            1,
            None,
            sym("Pair"),
            Record::builder().fields([f1, f2]).build(),
        );

        // Declare the fields after the record.
        low.declare(
            10,
            Some(1),
            sym("first"),
            Field::builder().key(FieldKey::Named).ty(Type::I32).build(),
        );
        low.declare(
            11,
            Some(1),
            sym("second"),
            Field::builder().key(FieldKey::Named).ty(Type::I64).build(),
        );

        let pkg = low.finish().expect("finish must succeed");

        // root + Pair + first + second = 4 entries.
        assert_eq!(pkg.iter().count(), 4);

        let pair = pkg
            .iter()
            .find(|(_, e)| e.sym().name == "Pair")
            .expect("Pair not found");
        assert_eq!(
            pair.1.children().len(),
            2,
            "Pair must have exactly 2 children"
        );
    }

    // ── 4a. LoweringError::Undeclared ─────────────────────────────────────────

    #[test]
    fn error_undeclared() {
        let mut low: Lowering<usize> = Lowering::new(PackageId::path("pkg"), sym("root"));

        // Refer to id 99, which we never declare.
        let _r: Ref<Field> = low.refer(99);

        let Err(err) = low.finish() else {
            panic!("must fail with Undeclared");
        };
        assert!(
            matches!(err, LoweringError::Undeclared(_)),
            "expected Undeclared, got {err:?}"
        );
    }

    // ── 4b. LoweringError::Duplicate ─────────────────────────────────────────

    #[test]
    fn error_duplicate() {
        let mut low: Lowering<usize> = Lowering::new(PackageId::path("pkg"), sym("root"));

        low.declare(1usize, None, sym("A"), Module);
        low.declare(1usize, None, sym("B"), Module); // same id

        let Err(err) = low.finish() else {
            panic!("must fail with Duplicate");
        };
        assert!(
            matches!(err, LoweringError::Duplicate(_)),
            "expected Duplicate, got {err:?}"
        );
    }

    // ── 4c. LoweringError::Cycle ─────────────────────────────────────────────

    /// Two entries that list each other as parent form an unreachable cycle.
    #[test]
    fn error_cycle() {
        let mut low: Lowering<usize> = Lowering::new(PackageId::path("pkg"), sym("root"));

        // A declares B as its parent, B declares A as its parent.
        low.declare(1usize, Some(2), sym("A"), Module);
        low.declare(2usize, Some(1), sym("B"), Module);

        let Err(err) = low.finish() else {
            panic!("must fail with Cycle");
        };
        assert!(
            matches!(err, LoweringError::Cycle(_)),
            "expected Cycle, got {err:?}"
        );
    }

    /// A one-element cycle: an entry that declares *itself* as its parent.
    /// The degenerate case the chain walk must not mistake for a root.
    #[test]
    fn error_self_cycle() {
        let mut low: Lowering<usize> = Lowering::new(PackageId::path("pkg"), sym("root"));
        low.declare(1usize, Some(1), sym("Ouroboros"), Module);

        let Err(err) = low.finish() else {
            panic!("a self-parenting entry must be rejected");
        };
        match err {
            LoweringError::Cycle(ids) => assert_eq!(ids, vec![1usize]),
            other => panic!("expected Cycle, got {other:?}"),
        }
    }

    /// A long parent chain terminating at the root must be accepted — the walk
    /// marks each entry `Acyclic` once, so this must not be mistaken for a loop
    /// and must not recurse (it is iterative by construction).
    #[test]
    fn deep_chain_is_not_a_cycle() {
        let mut low: Lowering<usize> = Lowering::new(PackageId::path("pkg"), sym("root"));
        for i in 1usize..=1_000 {
            let parent = (i > 1).then_some(i - 1);
            low.declare(i, parent, sym("m"), Module);
        }
        let pkg = low.finish().expect("a deep chain is legal");
        assert_eq!(pkg.iter().count(), 1_001, "root + 1000 nested modules");
    }

    // ── 5. Determinism ───────────────────────────────────────────────────────

    /// Build the same package twice; children must appear in the same order.
    #[test]
    fn deterministic_child_ordering() {
        let build = || {
            let mut low: Lowering<usize> = Lowering::new(PackageId::path("pkg"), sym("root"));
            low.declare(1usize, None, sym("alpha"), Module);
            low.declare(2usize, None, sym("beta"), Module);
            low.declare(3usize, None, sym("gamma"), Module);
            low.finish().expect("finish must succeed")
        };

        let pkg1 = build();
        let pkg2 = build();

        let names1: Vec<&str> = pkg1.iter().map(|(_, e)| e.sym().name.as_str()).collect();
        let names2: Vec<&str> = pkg2.iter().map(|(_, e)| e.sym().name.as_str()).collect();

        assert_eq!(names1, names2, "entry ordering must be deterministic");
    }

    // ── 6. declare_ref (re-exports) ───────────────────────────────────────────

    /// A re-export entry must appear in the tree and be recognized as a
    /// Reference by `Entry::kind`.
    #[test]
    fn declare_ref_reexport() {
        use crate::entry::EntryInner;

        let mut low: Lowering<usize> = Lowering::new(PackageId::path("pkg"), sym("root"));

        // Original entry.
        let orig: Ref<Module> = low.declare(1usize, None, sym("OrigModule"), Module);

        // Re-export it under a different name.
        low.declare_ref::<Module>(2usize, None, sym("AliasModule"), orig);

        let pkg = low.finish().expect("finish must succeed");

        let alias = pkg
            .iter()
            .find(|(_, e)| e.sym().name == "AliasModule")
            .expect("AliasModule not found");

        assert!(
            matches!(alias.1.kind(), EntryInner::Reference(_)),
            "AliasModule must be a Reference entry"
        );
    }
}
