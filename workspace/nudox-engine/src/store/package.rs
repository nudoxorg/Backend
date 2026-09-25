//! [`PackageView`] and its derived [`PackageIndexes`].
//!
//! # Immutability guarantee
//!
//! A `PackageView` is sealed at construction time and is never mutated
//! afterwards. All mutable index building happens inside
//! [`PackageIndexes::build`] before the view is wrapped in an `Arc`. Once the
//! `Arc<PackageView>` is published into the [`Corpus`], every reader sees a
//! fully consistent snapshot — no partial indexes, no torn reads.
//!
//! # Why `Arc` and not `Rc`?
//!
//! The engine, the graph adapter, MCP tools, and GUI views all hold
//! `Arc<PackageView>` references concurrently from different threads and
//! Tokio tasks. `Arc` is the only choice that keeps the view `Send + Sync`.
//!
//! # GD-34: disposable projections
//!
//! `PackageIndexes` is never persisted. It is always re-derived from the
//! `IrView` at the same `(channel_tip, schema_version)` key. Discarding and
//! rebuilding costs O(n log n) in the number of symbols — fast enough for
//! hot-reload and cheap enough that we do not need a CAS-keyed cache at L1.

use std::{collections::HashMap, sync::Arc};

use nudox_ir::{
    change::{IntroId, PackageLineageId, StableRef},
    kind::KindDiscriminant,
    package::{KeyTier, SealReport},
    reflect::moniker_path,
    view::IrView,
    vocab::Confidence,
};

use crate::store::index::{AliasIndex, NameIndex, PostingList, posting::PostingListBuilder};

pub use self::typerefs::{TypePosition, TypeRef, typerefs_of_entry};

mod typerefs {
    //! Which declarations name which types, and in **what syntactic position**.
    //!
    //! # Why the position is part of the answer
    //!
    //! This module began as a port of
    //! `workspace/registry/graph/reverse_index::typerefs_of_entry`, which
    //! captured two things — the trait an `impl` implements and a trait's
    //! supertraits — and said of everything else that it "will be added in a
    //! later wave". The consequence was not that some questions were slower;
    //! it was that they were *unanswerable*, at the index layer, no matter what
    //! the schema exposed:
    //!
    //! * "what does `Point` implement?" — `Impl::self_ty` was never read at
    //!   all;
    //! * "which functions return `Y`?" / "which fields are of type `Y`?" — the
    //!   example `graph_query`'s own tool description advertises.
    //!
    //! Adding those references to the *same* bucket would have been worse than
    //! leaving them out. `Trait.implementors` is served by this index, and
    //! `impl Debug for dyn Display` names `Display` in its `self_ty`; folding
    //! self-types into the same list as `Impl::of` would report that impl as an
    //! implementor of `Display`. Every reference therefore carries the
    //! [`TypePosition`] it was written in, and each reader asks for the
    //! position it means.
    //!
    //! # Why this takes an [`IrView`] and not an [`Entry`]
    //!
    //! A function's parameter and return *types* are not on the function: it
    //! holds `List<Ref<Param>>`, and the `Type` lives on the `Param` entries
    //! those refs name. A helper handed only an `&Entry` therefore **cannot**
    //! see a signature's types — which is the mechanical reason parameter and
    //! return types were "deferred" rather than merely unimplemented. Taking
    //! the view removes the impossibility instead of documenting it.

    use nudox_ir::{
        change::{IntroId, PackageLineageId, StableRef},
        index::{RawRef, Ref},
        kind::Kind,
        kinds::{
            Param, Type, UnknownType,
            ty::{Primitive, TemplatePart, TupleElement},
        },
        view::IrView,
    };

    /// The syntactic position a type reference occupies in the declaration
    /// that names it.
    ///
    /// # Deliberately exhaustive
    ///
    /// **No `#[non_exhaustive]`**, for the reason doctrine §3 gives: this enum
    /// is internal to the workspace, and its whole value is that adding a
    /// position breaks [`TypePosition::reach`], [`TypePosition::is_relational`]
    /// and every reader's `match`, forcing each one to decide what the new
    /// position means. A wildcard arm anywhere here recreates the single
    /// undifferentiated `mentions` bucket this type exists to replace.
    ///
    /// # What is deliberately *not* a position
    ///
    /// Generic-parameter bounds (`GenericParam::Type::bounds`) and
    /// where-clause predicates. They are constraints on a type *variable*, not
    /// references from this declaration to a concrete type, and indexing them
    /// would make `T: Display` answer "which functions accept `Display`" —
    /// which is the conflation the rest of this type exists to prevent.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub enum TypePosition {
        /// `Impl::of` — the trait an `impl` block implements.
        ///
        /// This is the position `Trait.implementors` reads, and the only one
        /// that means "this declaration is an implementation of that trait".
        ImplementedTrait,

        /// `Impl::self_ty` — the concrete type an `impl` block is *for*.
        ///
        /// The reverse of this position answers "what does `Point`
        /// implement?", which had no index at all before: `self_ty` was never
        /// read here, and `nudox-engine`'s Implementations tab worked around
        /// its absence by scanning the whole `by_kind[Impl]` bucket per symbol.
        ImplSelf,

        /// `Trait::supers` — an explicit supertrait bound on a trait
        /// declaration.
        Supertrait,

        /// `Record::super_types` — a base class or implemented interface.
        ///
        /// Separate from [`TypePosition::ImplementedTrait`] because in Java,
        /// C#, C++ and Go there is no `impl` entry: `class Foo implements Bar`
        /// puts `Bar` here. Reading them as one list would make "implementors"
        /// mean two different things depending on the source language.
        SuperType,

        /// `Field::ty` — a field or property's declared type.
        FieldType,

        /// `Param::ty`, reached through `Function::input_params`.
        Parameter,

        /// `Param::ty`, reached through `Function::output_params`.
        ///
        /// Multi-value returns (Go) and out-parameters are several entries in
        /// that list; each is recorded here, so "functions returning `Y`" is
        /// answered for a language that returns more than one thing.
        Return,

        /// `Function::throws` — a declared checked exception type.
        Throws,

        /// `Alias::target` — the right-hand side of a type alias.
        AliasTarget,

        /// `Const::ty` or `Static::ty` — the declared type of a value.
        ValueType,
    }

    /// How much of a type expression a position's references are taken from.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Reach {
        /// Only the head of the expression: `Vec<Point>` yields `Vec`.
        ///
        /// Correct for the positions that express a *relationship between two
        /// declarations*. `impl Display for Vec<Point>` implements `Display`
        /// for `Vec`, and reporting it under `Point` would answer "what does
        /// `Point` implement?" with an impl that is not one.
        Head,

        /// Every nominal named anywhere in the expression: `Vec<Point>` yields
        /// both `Vec` and `Point`.
        ///
        /// Correct for the positions that express a *value shape*. A caller
        /// asking "which functions return `Point`" means `-> Option<Point>`
        /// too; head-only would answer `Option` and silently omit the function
        /// they were looking for.
        Whole,
    }

    impl TypePosition {
        /// Whether this position names a *relationship between declarations*
        /// (`impl … for`, `: Supertrait`, `extends`) rather than a use of a
        /// type as a value shape.
        ///
        /// This is exactly the set the `mentions` edge has always carried, and
        /// it is what keeps that edge's meaning unchanged now that the index
        /// holds more than it used to.
        pub fn is_relational(self) -> bool {
            match self {
                TypePosition::ImplementedTrait
                | TypePosition::Supertrait
                | TypePosition::SuperType => true,
                TypePosition::ImplSelf
                | TypePosition::FieldType
                | TypePosition::Parameter
                | TypePosition::Return
                | TypePosition::Throws
                | TypePosition::AliasTarget
                | TypePosition::ValueType => false,
            }
        }

        fn reach(self) -> Reach {
            match self {
                TypePosition::ImplementedTrait
                | TypePosition::ImplSelf
                | TypePosition::Supertrait
                | TypePosition::SuperType => Reach::Head,
                TypePosition::FieldType
                | TypePosition::Parameter
                | TypePosition::Return
                | TypePosition::Throws
                | TypePosition::AliasTarget
                | TypePosition::ValueType => Reach::Whole,
            }
        }
    }

    /// One type reference written by one declaration.
    ///
    /// Ordered by `(position, target)` so that a caller can sort and
    /// deduplicate a signature's references without imposing an order of its
    /// own — the row order of the forward `signatureTypes` edge is a thing a
    /// user sees, and `HashMap` order is not allowed to reach a screen.
    #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct TypeRef {
        /// Where in the declaration the reference was written.
        pub position: TypePosition,
        /// The declaration being referred to.
        pub target: StableRef,
    }

    /// Every type reference the declaration at `intro` writes, sorted and
    /// deduplicated.
    ///
    /// Returns `[]` for an intro that is not live in `view`, for a reference
    /// entry with no owned kind, and for kinds that name no types
    /// (`Module`, `Enum`, `Variant`, `Reexport`).
    ///
    /// # `Kind::Param` contributes nothing of its own
    ///
    /// A parameter's type is recorded against the *function*, under
    /// [`TypePosition::Parameter`] or [`TypePosition::Return`], because that
    /// is the declaration a caller means when they ask "which functions take
    /// `Y`". Emitting it a second time against the `Param` entry would double
    /// every signature posting and put un-navigable `Param` rows in the
    /// answer.
    pub fn typerefs_of_entry(view: &IrView, intro: IntroId) -> Vec<TypeRef> {
        entry_type_facts(view, intro).refs
    }

    /// Resolved type references plus nominal spellings at those same positions
    /// that did not become a [`StableRef`].
    pub(crate) struct EntryTypeFacts {
        pub(crate) refs: Vec<TypeRef>,
        pub(crate) unresolved: Vec<(TypePosition, String)>,
    }

    /// The type references and unresolved nominal spellings `intro` writes.
    pub(crate) fn entry_type_facts(view: &IrView, intro: IntroId) -> EntryTypeFacts {
        let mut refs: Vec<TypeRef> = Vec::new();
        let mut unresolved: Vec<(TypePosition, String)> = Vec::new();
        let package = view.package();

        let Some(entry) = view.entry(intro) else {
            return EntryTypeFacts { refs, unresolved };
        };
        let Some(kind) = entry.kind().as_owned_kind() else {
            return EntryTypeFacts { refs, unresolved };
        };

        match kind {
            Kind::Impl(impl_) => {
                if let Some(of_ty) = &impl_.of {
                    push_type(
                        &mut refs,
                        &mut unresolved,
                        TypePosition::ImplementedTrait,
                        of_ty,
                        package,
                    );
                }
                push_type(
                    &mut refs,
                    &mut unresolved,
                    TypePosition::ImplSelf,
                    &impl_.self_ty,
                    package,
                );
            }
            Kind::Trait(trait_) => {
                for super_ty in &trait_.supers {
                    push_type(
                        &mut refs,
                        &mut unresolved,
                        TypePosition::Supertrait,
                        super_ty,
                        package,
                    );
                }
            }
            Kind::Record(record) => {
                for super_ty in &record.super_types {
                    push_type(
                        &mut refs,
                        &mut unresolved,
                        TypePosition::SuperType,
                        super_ty,
                        package,
                    );
                }
            }
            Kind::Field(field) => {
                if let Some(ty) = &field.ty {
                    push_type(
                        &mut refs,
                        &mut unresolved,
                        TypePosition::FieldType,
                        ty,
                        package,
                    );
                }
            }
            Kind::Function(function) => {
                for param in &function.input_params {
                    push_param(
                        &mut refs,
                        &mut unresolved,
                        TypePosition::Parameter,
                        param,
                        view,
                    );
                }
                for param in &function.output_params {
                    push_param(&mut refs, &mut unresolved, TypePosition::Return, param, view);
                }
                for thrown in &function.throws {
                    push_type(
                        &mut refs,
                        &mut unresolved,
                        TypePosition::Throws,
                        thrown,
                        package,
                    );
                }
            }
            Kind::Alias(alias) => {
                if let Some(target) = &alias.target {
                    push_type(
                        &mut refs,
                        &mut unresolved,
                        TypePosition::AliasTarget,
                        target,
                        package,
                    );
                }
            }
            Kind::Const(const_) => {
                push_type(
                    &mut refs,
                    &mut unresolved,
                    TypePosition::ValueType,
                    &const_.ty,
                    package,
                );
            }
            Kind::Static(static_) => {
                push_type(
                    &mut refs,
                    &mut unresolved,
                    TypePosition::ValueType,
                    &static_.ty,
                    package,
                );
            }
            // These kinds name no types of their own. An `Enum`'s payload
            // types live on its `Variant`s' `Field` entries, and a `Variant`'s
            // on its own `Field`s, so both are already covered by
            // `Kind::Field` above; recording them again here would double the
            // postings for every payload-carrying variant.
            Kind::Module(_) | Kind::Enum(_) | Kind::Variant(_) | Kind::Reexport(_)
            // See the doc comment: a parameter's type belongs to its function.
            | Kind::Param(_) => {}
        }

        refs.sort_unstable();
        refs.dedup();
        unresolved.sort_unstable();
        unresolved.dedup();
        EntryTypeFacts { refs, unresolved }
    }

    /// Record every target `ty` names, at the reach `position` calls for.
    ///
    /// Spellings that do not become a [`StableRef`] are recorded alongside,
    /// at the same reach: a head-only position does not treat a generic
    /// argument as the relationship, and a whole-expression position does.
    fn push_type(
        out: &mut Vec<TypeRef>,
        unresolved: &mut Vec<(TypePosition, String)>,
        position: TypePosition,
        ty: &Type,
        package: &PackageLineageId,
    ) {
        let mut targets: Vec<StableRef> = Vec::new();
        let mut spellings: Vec<String> = Vec::new();
        match position.reach() {
            Reach::Head => {
                targets.extend(head_stable_ref(ty, package));
                spellings.extend(head_unresolved_spelling(ty));
            }
            Reach::Whole => {
                collect_stable_refs(ty, package, &mut targets);
                collect_unresolved_spellings(ty, &mut spellings);
            }
        }
        out.extend(
            targets
                .into_iter()
                .map(|target| TypeRef { position, target }),
        );
        unresolved.extend(spellings.into_iter().map(|spelling| (position, spelling)));
    }

    /// Record the type of the `Param` entry `param` names.
    ///
    /// A `Ref::Foreign` parameter — a signature whose parameter *entry* lives
    /// in another package — contributes nothing, because its `Type` is not in
    /// this view to read. That is a real gap rather than a silent one: the
    /// forward `signatureTypes` edge is derived from this same function, so
    /// what the index cannot see, no reader is told it saw.
    fn push_param(
        out: &mut Vec<TypeRef>,
        unresolved: &mut Vec<(TypePosition, String)>,
        position: TypePosition,
        param: &Ref<Param>,
        view: &IrView,
    ) {
        let Ref::Intro(id) = param else {
            return;
        };
        let Some(entry) = view.entry(*id) else {
            return;
        };
        let Some(Kind::Param(p)) = entry.kind().as_owned_kind() else {
            return;
        };
        if let Some(ty) = &p.ty {
            push_type(out, unresolved, position, ty, view.package());
        }
    }

    /// The single nominal at the head of a type expression, if there is one.
    ///
    /// Generic applications (`Iterator<Item = u32>`) recurse into `base`, so
    /// `impl Iterator<Item = u32> for Foo` is still an implementation of
    /// `Iterator`.
    fn head_stable_ref(ty: &Type, package: &PackageLineageId) -> Option<StableRef> {
        match ty {
            Type::Nominal(raw_ref) => raw_ref_to_stable(raw_ref, package),
            Type::Apply { base, .. } => head_stable_ref(base, package),
            Type::Annotated { inner, .. } => head_stable_ref(inner, package),
            _ => None,
        }
    }

    /// Every nominal named anywhere inside a type expression.
    ///
    /// # Why this delegates rather than hand-walks
    ///
    /// This used to be a hand-written exhaustive match over every `Type`
    /// variant, on the theory that `Type` being deliberately not
    /// `#[non_exhaustive]` meant a new variant would break the match at
    /// compile time and force its author to decide whether it could carry a
    /// nominal. That theory had a hole: the match's leaf arm collapsed
    /// `Type::Primitive(_)` whole, under a comment claiming "nothing nominal
    /// can be reached through them" — false for `Primitive::Reference`,
    /// `Primitive::MutPointer` and `Primitive::ConstPointer`, each of which
    /// nests a `Type`. `Primitive` is its own enum with its own variants,
    /// and this function never matched on it at all, so the exhaustiveness
    /// this comment relied on never covered the case that broke. `&Config`,
    /// `*const T` and `*mut T` were invisible to the reverse index as a
    /// result: "who takes a `&Config`" returned empty and read as a true
    /// negative, exactly the way `Type::Apply`'s *arguments* went unindexed
    /// for as long as this module existed, for the identical reason.
    ///
    /// [`Type::for_each_ref`] closes that hole structurally instead of
    /// adding the three missing arms here. It is generated by
    /// `#[derive(Visitor)]` fresh from whatever fields whatever variants
    /// `Type` and `Primitive` have *today*, visiting every field of every
    /// variant unconditionally — there is no `_ => {}` in it and no
    /// hand-maintained case list to fall out of sync with the enum. A future
    /// variant that nests a `Type` anywhere in the tree, in `Primitive` or
    /// otherwise, is walked automatically; a field the derive cannot walk
    /// fails the *build*, not a reverse-index query at runtime. That is the
    /// property this function needs and a hand-written match cannot give it:
    /// exhaustiveness enforced by the compiler on the producer of the walk,
    /// not by a reviewer remembering to update every consumer of it.
    ///
    /// # Depth
    ///
    /// There is no recursion guard. Producers already cap their own lowering
    /// depth and record hitting it as
    /// `UnknownType::TruncatedAtDepthLimit`, so a `Type` reaching this
    /// function is bounded by construction; adding a second, silent cap here
    /// would truncate an answer without recording that it had (doctrine §8).
    fn collect_stable_refs(ty: &Type, package: &PackageLineageId, out: &mut Vec<StableRef>) {
        ty.for_each_ref(|raw_ref| out.extend(raw_ref_to_stable(raw_ref, package)));
    }

    fn raw_ref_to_stable(raw: &RawRef, package: &PackageLineageId) -> Option<StableRef> {
        match raw {
            Ref::Intro(id) => Some(StableRef::new(package.clone(), *id)),
            // A cross-package reference earns a posting only once it is
            // *linked* — the index maps a `StableRef` to the entries that name
            // it, and a named-but-unlinked reference has no `StableRef` to key
            // on. This is why "who implements this trait" is empty for a
            // foreign trait whose package is not in the corpus, and why it
            // starts working the moment one is.
            Ref::Foreign { target, .. } => target.clone(),
            // `Ref::Local` really does not appear in a sealed table now: the
            // import arena that used to hide behind this variant is gone, and
            // `seal` reports any residual local in `SealReport::unmapped_local`.
            Ref::Local(_) => None,
        }
    }

    /// The unresolved nominal at the head of `ty`, if the head did not link.
    fn head_unresolved_spelling(ty: &Type) -> Option<String> {
        match ty {
            Type::Nominal(raw_ref) => unresolved_spelling_of_raw(raw_ref),
            Type::Apply { base, .. } => head_unresolved_spelling(base),
            Type::Annotated { inner, .. } => head_unresolved_spelling(inner),
            Type::Unknown(unknown) => unresolved_spelling_of_unknown(unknown),
            Type::SelfType
            | Type::Primitive(_)
            | Type::Tuple(_)
            | Type::Slice(_)
            | Type::Array { .. }
            | Type::Union(_)
            | Type::Intersection(_)
            | Type::Never
            | Type::Any
            | Type::TypeVar(_)
            | Type::Wildcard { .. }
            | Type::FunctionPointer { .. }
            | Type::Conditional { .. }
            | Type::Mapped { .. }
            | Type::TemplateLiteral(_)
            | Type::AnonymousRecord { .. }
            | Type::ImplTrait(_)
            | Type::DynTrait(_)
            | Type::Inferred
            | Type::QualifiedPath { .. } => None,
        }
    }

    /// Every unresolved nominal spelling anywhere in `ty`.
    ///
    /// Exhaustive over [`Type`] and [`Primitive`] because
    /// [`Type::for_each_ref`] only visits [`RawRef`]s. An
    /// [`UnknownType::UnresolvedExternal`] carries a spelling and no ref,
    /// so a ref-only walk would drop it.
    fn collect_unresolved_spellings(ty: &Type, out: &mut Vec<String>) {
        match ty {
            Type::SelfType | Type::Never | Type::Any | Type::Inferred | Type::TypeVar(_) => {}
            Type::Primitive(primitive) => collect_unresolved_in_primitive(primitive, out),
            Type::Tuple(elements) => {
                for element in elements.iter() {
                    match element {
                        TupleElement::Positional(inner) => collect_unresolved_spellings(inner, out),
                        TupleElement::Named { ty, .. } => collect_unresolved_spellings(ty, out),
                    }
                }
            }
            Type::Slice(inner) | Type::Array { ty: inner, .. } => {
                collect_unresolved_spellings(inner, out);
            }
            Type::Union(parts)
            | Type::Intersection(parts)
            | Type::ImplTrait(parts)
            | Type::DynTrait(parts) => {
                for part in parts.iter() {
                    collect_unresolved_spellings(part, out);
                }
            }
            Type::Unknown(unknown) => out.extend(unresolved_spelling_of_unknown(unknown)),
            Type::Nominal(raw) => out.extend(unresolved_spelling_of_raw(raw)),
            Type::Apply { base, args } => {
                collect_unresolved_spellings(base, out);
                for arg in args.iter() {
                    collect_unresolved_spellings(arg, out);
                }
            }
            Type::Wildcard { bound, .. } => {
                if let Some(bound) = bound {
                    collect_unresolved_spellings(bound, out);
                }
            }
            Type::FunctionPointer { params, ret, .. } => {
                for param in params.iter() {
                    collect_unresolved_spellings(param, out);
                }
                if let Some(ret) = ret {
                    collect_unresolved_spellings(ret, out);
                }
            }
            Type::Annotated { inner, .. } => collect_unresolved_spellings(inner, out),
            Type::Conditional {
                check,
                extends_ty,
                then_ty,
                else_ty,
            } => {
                collect_unresolved_spellings(check, out);
                collect_unresolved_spellings(extends_ty, out);
                collect_unresolved_spellings(then_ty, out);
                collect_unresolved_spellings(else_ty, out);
            }
            Type::Mapped { source, value, .. } => {
                collect_unresolved_spellings(source, out);
                collect_unresolved_spellings(value, out);
            }
            Type::TemplateLiteral(parts) => {
                for part in parts.iter() {
                    if let TemplatePart::Interpolated(inner) = part {
                        collect_unresolved_spellings(inner, out);
                    }
                }
            }
            Type::AnonymousRecord { members, .. } => {
                for member in members.iter() {
                    collect_unresolved_spellings(&member.ty, out);
                }
            }
            Type::QualifiedPath {
                self_ty, trait_ref, ..
            } => {
                collect_unresolved_spellings(self_ty, out);
                if let Some(trait_ref) = trait_ref {
                    collect_unresolved_spellings(trait_ref, out);
                }
            }
        }
    }

    fn collect_unresolved_in_primitive(primitive: &Primitive, out: &mut Vec<String>) {
        match primitive {
            Primitive::Integer { .. }
            | Primitive::Float(_)
            | Primitive::Bool
            | Primitive::Char
            | Primitive::Str
            | Primitive::Builtin(_) => {}
            Primitive::MutPointer(inner) | Primitive::ConstPointer(inner) => {
                collect_unresolved_spellings(inner, out);
            }
            Primitive::Reference { ty, .. } => collect_unresolved_spellings(ty, out),
        }
    }

    fn unresolved_spelling_of_unknown(unknown: &UnknownType) -> Option<String> {
        match unknown {
            UnknownType::UnresolvedLocalName { name }
            | UnknownType::UnresolvedExternal { name }
                if !name.is_empty() =>
            {
                Some(name.clone())
            }
            UnknownType::UnresolvedLocalName { .. }
            | UnknownType::UnresolvedExternal { .. }
            | UnknownType::Unannotated
            | UnknownType::DynamicallyTyped
            | UnknownType::TruncatedAtDepthLimit
            | UnknownType::OracleGap
            | UnknownType::NoIrRepresentation { .. } => None,
        }
    }

    /// A nominal ref that did not become a [`StableRef`], as the producer
    /// spelled it.
    fn unresolved_spelling_of_raw(raw: &RawRef) -> Option<String> {
        match raw {
            Ref::Intro(_) => None,
            Ref::Foreign {
                target: Some(_), ..
            } => None,
            Ref::Foreign {
                target: None, key, ..
            } => {
                let display = key.display.as_ref();
                if display.is_empty() {
                    let path = key.path.as_ref();
                    if path.is_empty() {
                        None
                    } else {
                        Some(path.to_owned())
                    }
                } else {
                    Some(display.to_owned())
                }
            }
            Ref::Local(_) => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Key provenance
// ---------------------------------------------------------------------------

/// What this view knows about *how its keys were minted*.
///
/// # The defect this closes
///
/// `IntroId` is minted through a disambiguator ladder, and two of its rungs
/// (`Span`, `Ordinal`) are not functions of the declaration's content. Sealing
/// already recorded exactly which declarations took them —
/// [`SealReport::non_structural_keys`] — and `nudox-store` then **dropped the
/// whole report on the floor**, so a `PackageView` could not answer "is this
/// key fragile" and no layer above it could either. The user-visible
/// consequence: a key that stops resolving after `select_version` is
/// indistinguishable from a deleted symbol, and MCP's `select_version` schema
/// had to say so in prose because it had nothing better.
///
/// Measured on the provisioned corpus: 32,339 order-dependent groups across 49
/// of 79 packages, and 24 declarations across log/memchr/jackson-databind/
/// lodash that provably changed their published key between real releases.
///
/// # MCP-SURFACE-PLAN §4.14: `forced_keys` was the wrong source
///
/// This type used to project [`SealReport::forced_keys`] instead of
/// `non_structural_keys`. `forced_keys` only names declarations that pass
/// 2.5 **escalated**; sealing's pass 2 can also mint `Disambiguator::Span`
/// directly for a same-key collision on a non-function, non-impl kind
/// (`seal.rs`'s "other collision" arm), and when the colliding spans already
/// differ that id is already unique, so pass 2.5 never runs and the
/// declaration never enters `forced_keys`. Measured over 22 crates.io
/// packages / 77,873 declarations: 1,085 declarations were truly `Span`-keyed,
/// and `forced_keys` named only 41 of them — the other 1,044 were reported
/// `Structural` ("content-derived, safe to cache") to every MCP caller.
/// `non_structural_keys` is read off the actual `Disambiguator` variant that
/// minted each id, so it has no such blind spot.
///
/// # Why `Unrecorded` is a variant and not a default
///
/// A `PackageView` can be built from a table that this process never sealed —
/// every hand-built test fixture is one. Defaulting those to "all structural"
/// would be a silent repair in exactly doctrine §8's sense: it presents the
/// degraded case (we did not look) as the good one (we looked and found
/// nothing). So the absence is a *value*, it survives all the way onto the
/// graph vertex as the string `Unrecorded`, and
/// [`PackageView::key_tier`] returns `None` rather than
/// `Some(KeyTier::Structural)` for it.
#[derive(Debug, Clone, Default)]
pub enum KeyProvenance {
    /// The seal report reached this view. Every intro's tier is known: those
    /// listed here are escalated, and every other live intro is
    /// [`KeyTier::Structural`].
    ///
    /// Only the escalated set is stored. A row per declaration would cost a
    /// 30k-entry map on the larger corpus packages to record that nothing
    /// happened, and absence is already meaningful.
    Sealed(HashMap<IntroId, KeyTier>),

    /// No seal report reached this view, so no tier is known for any key.
    ///
    /// The default, because that is what a `PackageView` built straight from
    /// an `IrView` honestly has.
    #[default]
    Unrecorded,
}

impl KeyProvenance {
    /// Project a [`SealReport`] into the half a *consumer of keys* needs.
    ///
    /// Takes the report by reference and copies only `non_structural_keys`:
    /// the rest of the report (`linked`, `unlinked`, `collisions`,
    /// `forced_keys`) is producer-quality telemetry with no bearing on
    /// whether a caller's key is trustworthy, and keeping it alive in every
    /// resident `PackageView` would hold a `Vec` of every foreign key the
    /// package names for the life of the process. See the type docs for why
    /// `non_structural_keys` and not `forced_keys` — reading `forced_keys`
    /// here was the defect.
    pub fn from_seal_report(report: &SealReport) -> Self {
        KeyProvenance::Sealed(report.non_structural_keys.iter().copied().collect())
    }

    /// The tier that minted `intro`, or `None` when this view has no record.
    ///
    /// `None` is "we did not look", never "nothing was escalated" — see the
    /// type docs for why those must stay distinguishable.
    pub fn tier_of(&self, intro: IntroId) -> Option<KeyTier> {
        match self {
            KeyProvenance::Sealed(escalated) => Some(
                escalated
                    .get(&intro)
                    .copied()
                    .unwrap_or(KeyTier::Structural),
            ),
            KeyProvenance::Unrecorded => None,
        }
    }

    /// How many of this package's declarations have a key that is *not*
    /// content-derived, or `None` when this view has no record.
    ///
    /// Derived from the stored map rather than counted alongside it (doctrine
    /// §8), so it cannot drift from the tiers it summarises.
    pub fn fragile_key_count(&self) -> Option<usize> {
        match self {
            KeyProvenance::Sealed(escalated) => Some(
                escalated
                    .values()
                    .filter(|tier| !tier.is_content_derived())
                    .count(),
            ),
            KeyProvenance::Unrecorded => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Provenance
// ---------------------------------------------------------------------------

/// How confidently this package's IR was produced.
///
/// `#[non_exhaustive]` so that new provenance tiers (e.g. `SyncedLocal` with
/// a generation id) can be added without breaking `match` arms in callers.
///
/// See GUI-LOCAL-PLAN §L2.1 for the full provenance vocabulary; the variants
/// here are the subset that L1 needs to record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Provenance {
    /// Produced on this machine from source we can see.
    ///
    /// This is the default for every package produced by [`ProducerSource`] or
    /// [`FixtureSource`]. LR-10: local is the truth; this is not the
    /// exception.
    ///
    /// [`ProducerSource`]: crate::store::source::producer::ProducerSource
    /// [`FixtureSource`]: crate::store::source::fixtures::FixtureSource
    TrustedLocal,
    /// The package was not re-produced in this session; its IR was loaded from
    /// a prior snapshot. Still trusted, but the snapshot generation may be
    /// behind the current source.
    SnapshotLocal,
}

// ---------------------------------------------------------------------------
// PackageIndexes
// ---------------------------------------------------------------------------

/// All derived, rebuildable indexes for one package.
///
/// Built by [`PackageIndexes::build`] from a sealed [`IrView`].
/// Never persisted (GD-34).
pub struct PackageIndexes {
    /// Case-folded name → introductions, for prefix and fuzzy search (§L3.1).
    pub by_name: NameIndex,
    /// `KindDiscriminant` → introductions, for kind facets.
    ///
    /// Each entry appears in exactly one bucket (the bucket for its own
    /// discriminant). This lets the search layer cheaply filter to, e.g., all
    /// functions without scanning the full entry set.
    pub by_kind: HashMap<KindDiscriminant, Vec<IntroId>>,
    /// Reverse occurrence postings: `target StableRef → owners`.
    ///
    /// Only occurrences with `confidence >= Confidence::Index` are projected
    /// here (the graph-worthy floor, mirroring the prior art).
    pub usages: PostingList<StableRef, IntroId>,
    /// Reverse type-reference postings: `target StableRef → (position, owner)`.
    ///
    /// One entry per `(declaration, position, target)` written anywhere in a
    /// declaration's signature — see [`typerefs_of_entry`] for the exact set
    /// and [`TypePosition`] for what each position means.
    ///
    /// # Why the position is a *value* and not a second index
    ///
    /// Keying by `StableRef` alone and carrying the position in the posting
    /// means one `BTreeMap` probe answers every question about a type at once:
    /// `mentions_of` and `implementors_of` and `type_refs_in` all cost one
    /// lookup plus a linear filter over that type's own postings, which is
    /// bounded by the *answer*. A map per position would multiply the probe
    /// count by the number of positions and give the two structures somewhere
    /// to drift apart.
    ///
    /// Postings sort by `(position, owner)`, so each position's owners are a
    /// contiguous, ascending run — the row order of every reverse edge built
    /// on this index is therefore deterministic without a second sort.
    pub type_refs: PostingList<StableRef, (TypePosition, IntroId)>,
    /// Fully-qualified path per intro, precomputed once.
    ///
    /// The path is the `moniker_path` of the entry: root-first, dot-joined
    /// symbol names. Precomputed so that search result rendering never
    /// re-walks the parent chain per query.
    pub paths: HashMap<IntroId, Arc<str>>,
    /// Exact re-export alias path → declaration(s), inverted from every
    /// entry's `Symbol.aliases` (address scheme Stage 2, §4.10). See
    /// [`crate::store::index::AliasIndex`]'s module docs for why this maps
    /// straight to definitions and needs no `Reexport` vertex.
    pub by_alias: AliasIndex,
    /// Whether this package's producer ever called `record_occurrence` /
    /// `record_foreign_occurrence` — i.e. whether `usages` (and the raw
    /// per-entry occurrence lists behind it) can be trusted as an answer at
    /// all, as opposed to an unasked question.
    ///
    /// # Why this has to be a package-wide fact, not a per-query one
    ///
    /// `usages_of` returning `&[]` is structurally identical whether nothing
    /// in the package ever calls `target`, or nothing in the package was ever
    /// looked at. Only one language frontend (Rust, as of this writing) calls
    /// `record_occurrence`; every other producer lowers declarations and
    /// stops, so for six of the seven supported languages *every* `usages_of`
    /// answer is the second case wearing the first case's clothes. A caller
    /// one function away from `usages_of` has no way to tell them apart
    /// unless the package itself remembers whether it was ever asked.
    ///
    /// # The genuinely-empty package, and why it is folded into `false`
    ///
    /// A real package can legitimately contain zero graph-worthy occurrences
    /// — a types-only crate, a single free function with no internal calls.
    /// That case is indistinguishable, from inside `build()`, from a producer
    /// that never records anything: both leave `view.all_occurrences()`
    /// empty. Reporting `false` (not recorded) for both is the safe
    /// direction: it under-claims — a genuinely reference-free package's
    /// `refs` answer carries a coverage note it did not strictly need — never
    /// over-claims. The alternative, reporting `true` whenever the count is
    /// merely zero, is exactly the defect this field exists to close.
    pub occurrences_recorded: bool,
    /// Which [`TypePosition`]s this package's producer ever wrote at least
    /// one [`TypeRef`] at, across every declaration.
    ///
    /// Same discipline as [`Self::occurrences_recorded`], applied to the
    /// structural-typed edges instead of the occurrence graph: a producer
    /// that never emits `TypePosition::ImplementedTrait` (every non-Rust
    /// language — `impl` blocks are a Rust-only construct) makes
    /// `Trait.implementors` empty *by construction* for that language, which
    /// looks identical, from an empty result alone, to a trait that
    /// genuinely has no implementors. `implementors` uses this as a
    /// corpus-wide fact. The other reverse edges do not: a package that
    /// records `Return` for a different symbol still leaves an unlinked
    /// return of *this* symbol unresolved.
    pub type_positions_recorded: std::collections::BTreeSet<TypePosition>,
    /// Nominal spellings written at a [`TypePosition`] that did not become a
    /// [`StableRef`]. Sorted and deduplicated per position.
    ///
    /// A reverse edge keys on resolved identity, so these names never appear
    /// in [`Self::type_refs`]. They are why an empty `returnedBy` can mean
    /// "the return type spelled this symbol and did not link" rather than
    /// "nothing returns it".
    pub unresolved_nominals: std::collections::BTreeMap<TypePosition, Vec<String>>,
}

impl PackageIndexes {
    /// Build all indexes from a sealed [`IrView`] in a single pass.
    ///
    /// Complexity: O(n log n) dominated by sort+dedup of posting lists and
    /// path-string allocation. Called once per package load.
    ///
    /// # Why the walk is `entries_sorted`, not `entries`
    ///
    /// Every index here is *derived* state that ends up on a user's screen, so
    /// the walk that feeds them all is the single place determinism has to be
    /// established. `IrView::entries` yields in `HashMap` order — a function of
    /// std's per-process `RandomState` seed — so a walk through it makes each
    /// index individually responsible for re-imposing an order, and the one
    /// index that forgot (`by_name`) leaked the seed all the way to the search
    /// ranking. Fixing it here means a *future* index added to this loop is
    /// deterministic without its author having to know that.
    pub fn build(view: &IrView) -> Self {
        let mut by_name = NameIndex::new();
        let mut by_kind: HashMap<KindDiscriminant, Vec<IntroId>> = HashMap::new();
        let mut usages_builder: PostingListBuilder<StableRef, IntroId> = PostingListBuilder::new();
        let mut type_refs_builder: PostingListBuilder<StableRef, (TypePosition, IntroId)> =
            PostingListBuilder::new();
        let mut paths: HashMap<IntroId, Arc<str>> = HashMap::new();
        let mut by_alias = AliasIndex::new();
        let mut type_positions_recorded: std::collections::BTreeSet<TypePosition> =
            std::collections::BTreeSet::new();
        let mut unresolved_nominals: std::collections::BTreeMap<
            TypePosition,
            std::collections::BTreeSet<String>,
        > = std::collections::BTreeMap::new();

        // Single pass over the declaration table, in IntroId order.
        for (intro, entry) in view.entries_sorted() {
            // -- NameIndex ---------------------------------------------------
            by_name.insert(entry.sym().name.clone(), intro);

            // -- by_kind -----------------------------------------------------
            let disc = entry
                .kind()
                .discriminant()
                .unwrap_or(KindDiscriminant::Reexport);
            by_kind.entry(disc).or_default().push(intro);

            // -- paths -------------------------------------------------------
            if let Some(path) = moniker_path(view.table(), intro) {
                paths.insert(intro, Arc::from(path.as_str()));
            }

            // -- by_alias (address scheme Stage 2, §4.10) --------------------
            // `Symbol.aliases` is already populated by all seven producers
            // and already read by `chunk::head::shortest_alias_module_path`
            // one entry at a time; inverting it here at load time is the
            // "escape hatch that is already paid for" (§4.4).
            for alias in &entry.sym().aliases {
                by_alias.insert(alias.clone(), intro);
                // Name search is intentionally leaf-oriented. Keep the full
                // public path in the exact alias index above, while also
                // making its final binding visible to ordinary prefix search:
                // `serde::de::Deserializer` must be discoverable by
                // searching for `Deserializer`.
                if let Some(leaf) = alias.rsplit("::").next()
                    && !leaf.is_empty()
                {
                    by_name.insert(leaf, intro);
                }
            }

            // -- type_refs ---------------------------------------------------
            // Every type this declaration names, tagged with the position it
            // was written in. `typerefs_of_entry` takes the view rather than
            // `entry` because a function's parameter and return types live on
            // its `Param` entries, not on the function.
            let facts = typerefs::entry_type_facts(view, intro);
            for TypeRef { position, target } in facts.refs {
                type_positions_recorded.insert(position);
                type_refs_builder.push(target.clone(), (position, intro));
            }
            for (position, spelling) in facts.unresolved {
                unresolved_nominals
                    .entry(position)
                    .or_default()
                    .insert(spelling);
            }
        }

        // -- usages (occurrence postings, graph-worthy only) -----------------
        // Mirrors the prior art: filter to confidence >= Index.
        //
        // `occurrences_recorded` is read from the *unfiltered* iterator
        // deliberately: the question it answers is "did the producer ever
        // call `record_occurrence`", not "did any of those calls clear the
        // graph-worthy confidence floor". Collapsing the two would report
        // `false` for a package whose producer runs but only ever emits
        // syntax-tier facts, which is a confidence problem, not a coverage
        // one — the wrong field to blame it on.
        let mut occurrences_recorded = false;
        for (owner, occ) in view.all_occurrences() {
            occurrences_recorded = true;
            if occ.confidence >= Confidence::Index {
                usages_builder.push(occ.target.clone(), owner);
            }
        }

        // Sort by_kind vecs for determinism.
        for list in by_kind.values_mut() {
            list.sort_unstable();
            list.dedup();
        }

        Self {
            by_name,
            by_kind,
            usages: usages_builder.finish(),
            type_refs: type_refs_builder.finish(),
            paths,
            by_alias,
            occurrences_recorded,
            type_positions_recorded,
            unresolved_nominals: unresolved_nominals
                .into_iter()
                .map(|(position, spellings)| (position, spellings.into_iter().collect()))
                .collect(),
        }
    }

    /// The sorted, deduplicated owners that hold a graph-worthy occurrence
    /// whose target is `target`.
    pub fn usages_of(&self, target: &StableRef) -> &[IntroId] {
        self.usages.get(target)
    }

    /// Every `(position, owner)` pair that names `ty`, in `(position, owner)`
    /// order.
    ///
    /// One `BTreeMap` probe. Callers that want a single position should use
    /// [`PackageIndexes::type_refs_in`], which filters this slice.
    pub fn type_refs_of(&self, ty: &StableRef) -> &[(TypePosition, IntroId)] {
        self.type_refs.get(ty)
    }

    /// The entries that name `ty` in exactly `position`, in ascending
    /// [`IntroId`] order.
    pub fn type_refs_in(&self, ty: &StableRef, position: TypePosition) -> Vec<IntroId> {
        self.type_refs_of(ty)
            .iter()
            .filter(|(p, _)| *p == position)
            .map(|(_, intro)| *intro)
            .collect()
    }

    /// Whether this package's producer ever wrote a [`TypeRef`] at `position`
    /// — see [`Self::type_positions_recorded`]'s doc comment for what that
    /// distinguishes it from ("nothing here" vs. "this language cannot say
    /// this").
    pub fn records_type_position(&self, position: TypePosition) -> bool {
        self.type_positions_recorded.contains(&position)
    }

    /// Spellings written at `position` that did not become a [`StableRef`].
    ///
    /// Empty when this package linked every nominal at that position, or
    /// never wrote one. The slice is sorted.
    pub fn unresolved_nominals_at(&self, position: TypePosition) -> &[String] {
        self.unresolved_nominals
            .get(&position)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// The entries that name `ty` in a *relational* position — the trait an
    /// impl implements, a supertrait bound, a base class or interface.
    ///
    /// This is the `mentions` edge's contract and it is deliberately narrower
    /// than "every entry whose signature names `ty`": mixing the two would
    /// mean a function taking `&dyn Display` were reported alongside the impls
    /// of `Display`. Ask [`PackageIndexes::type_refs_in`] for a signature
    /// position.
    ///
    /// Returns an owned `Vec` rather than a slice because it is a *view* over
    /// several positions of one posting list, not a stored list of its own —
    /// which is what stops the relational and signature answers from drifting.
    pub fn mentions_of(&self, ty: &StableRef) -> Vec<IntroId> {
        let mut owners: Vec<IntroId> = self
            .type_refs_of(ty)
            .iter()
            .filter(|(position, _)| position.is_relational())
            .map(|(_, intro)| *intro)
            .collect();
        owners.sort_unstable();
        owners.dedup();
        owners
    }

    /// The precomputed fully-qualified path for `intro`, if present.
    pub fn path_of(&self, intro: IntroId) -> Option<&Arc<str>> {
        self.paths.get(&intro)
    }
}

// ---------------------------------------------------------------------------
// PackageView
// ---------------------------------------------------------------------------

/// One package's sealed IR plus all derived indexes. Immutable once built.
///
/// Always held as `Arc<PackageView>` — see the module-level docs for why.
///
/// # Accessing entries
///
/// Go through `view()` to reach the underlying [`IrView`]. The indexes
/// (`indexes()`) answer "which intros match this query?" and the view answers
/// "give me the entry for this intro".
pub struct PackageView {
    /// The sealed declaration table and occurrence map for this package.
    view: IrView,
    /// How this package's IR was obtained.
    provenance: Provenance,
    /// Source and producer metadata retained with the immutable snapshot.
    metadata: crate::PackageMetadata,
    /// How this package's `IntroId`s were minted, when sealing told us.
    keys: KeyProvenance,
    /// All derived indexes, built once from `view` at construction time.
    indexes: PackageIndexes,
    /// Occurrence owners or targets the producer submitted that sealing did
    /// not attach. Absent from a view built without a [`SealReport`] — those
    /// tables never had a rejection count to copy.
    occurrence_attachments_rejected: bool,
}

/// Deliberately *not* derived.
///
/// A derived `Debug` would recurse through the whole `IrView` — every entry,
/// every type, every occurrence — so one `tracing::debug!` on a `LoadEvent`
/// would dump an entire package's IR into the log. The summary below is what a
/// human actually wants when a load event goes past: which package, how big,
/// and where it came from.
impl core::fmt::Debug for PackageView {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PackageView")
            .field("package", &format_args!("{}", self.view.package()))
            .field("entries", &self.view.table().len())
            .field("provenance", &self.provenance)
            .finish_non_exhaustive()
    }
}

impl PackageView {
    /// Build a `PackageView` from an `IrView` whose [`SealReport`] is **not**
    /// available, computing all indexes.
    ///
    /// The resulting view answers [`PackageView::key_tier`] with `None` for
    /// every symbol: not "no key was escalated", but "nobody told us". That
    /// distinction survives all the way to the graph, where it is the string
    /// `Unrecorded`.
    ///
    /// Every *production* load path has the report in hand and must call
    /// [`PackageView::build_sealed`] instead; this constructor exists for the
    /// hand-built tables in tests and for `IrView`s reconstituted from a
    /// snapshot, neither of which ever ran `seal` in this process.
    pub fn build(view: IrView, provenance: Provenance) -> Self {
        Self::with_keys(
            view,
            provenance,
            KeyProvenance::Unrecorded,
            crate::PackageMetadata::default(),
            false,
        )
    }

    /// Build a `PackageView` from a table this process just sealed, carrying
    /// the report's key-provenance facts onto the view.
    ///
    /// This is the constructor the load path uses. It takes the whole
    /// [`SealReport`] rather than a pre-projected map so that the *decision*
    /// about which parts of the report matter to a reader lives in
    /// [`KeyProvenance::from_seal_report`], next to the reasoning, instead of
    /// being re-made at each call site.
    pub fn build_sealed(view: IrView, provenance: Provenance, report: &SealReport) -> Self {
        Self::build_sealed_with_metadata(
            view,
            provenance,
            report,
            crate::PackageMetadata::default(),
        )
    }

    /// Build a sealed package while retaining its source metadata.
    pub fn build_sealed_with_metadata(
        view: IrView,
        provenance: Provenance,
        report: &SealReport,
        metadata: crate::PackageMetadata,
    ) -> Self {
        Self::with_keys(
            view,
            provenance,
            KeyProvenance::from_seal_report(report),
            metadata,
            report.rejected_facts.undeclared_occurrence_owners > 0
                || report.rejected_facts.undeclared_occurrence_targets > 0,
        )
    }

    /// The shared constructor. After this call the view and its indexes are
    /// frozen; wrap the result in `Arc::new` before sharing.
    fn with_keys(
        view: IrView,
        provenance: Provenance,
        keys: KeyProvenance,
        metadata: crate::PackageMetadata,
        occurrence_attachments_rejected: bool,
    ) -> Self {
        let indexes = PackageIndexes::build(&view);
        Self {
            view,
            provenance,
            metadata,
            keys,
            indexes,
            occurrence_attachments_rejected,
        }
    }

    /// The lineage of this package (ecosystem + name).
    ///
    /// Forwarded from the inner [`IrView`] so callers do not need to reach
    /// through `view()`.
    pub fn lineage(&self) -> &PackageLineageId {
        self.view.package()
    }

    /// Borrow the sealed declaration table and occurrence map.
    pub fn view(&self) -> &IrView {
        &self.view
    }

    /// The provenance of this package's IR.
    pub fn provenance(&self) -> Provenance {
        self.provenance
    }

    /// Optional metadata extracted from the package source.
    pub fn metadata(&self) -> &crate::PackageMetadata {
        &self.metadata
    }

    /// The concrete version this generation was loaded at, when known.
    ///
    /// Forwarded from [`crate::PackageMetadata::version`] — see that field's
    /// own doc comment for why a materialized view needs to be able to answer
    /// this at all.
    pub fn version(&self) -> Option<&str> {
        self.metadata.version.as_deref()
    }

    /// Attach a resident version before sharing this view.
    ///
    /// A builder-style method rather than a constructor parameter: most
    /// callers of [`PackageView::build`] have no version to give (tests that
    /// predate the version-carrying `PackageMetadata` field, or genuinely
    /// version-less fixtures), and threading an extra parameter through
    /// `build`/`build_sealed`/`build_sealed_with_metadata` would touch every
    /// existing call site for a field only some of them have. A caller that
    /// already holds a `SealReport`-backed `PackageMetadata` (the production
    /// load path, `store::source::producer::PackageDescriptor::metadata`)
    /// sets `version` on it directly instead and passes it to
    /// `build_sealed_with_metadata`.
    pub fn with_version(mut self, version: Option<String>) -> Self {
        self.metadata.version = version;
        self
    }

    /// What sealing recorded about how this package's keys were minted.
    pub fn key_provenance(&self) -> &KeyProvenance {
        &self.keys
    }

    /// Which disambiguator tier minted `intro`'s key, or `None` when this view
    /// carries no seal report.
    ///
    /// The question a caller holding a key across a version switch has to ask
    /// before it treats a failed lookup as a deletion. See [`KeyProvenance`].
    pub fn key_tier(&self, intro: IntroId) -> Option<KeyTier> {
        self.keys.tier_of(intro)
    }

    /// The derived indexes built from this package's IR.
    pub fn indexes(&self) -> &PackageIndexes {
        &self.indexes
    }

    /// Whether sealing rejected occurrence owners or targets for this package.
    ///
    /// `false` when no [`SealReport`] reached the view. Those rejected facts
    /// are not in the occurrence map, so an empty `usages` posting cannot
    /// tell "nothing calls this" from "the call was dropped at the boundary".
    pub fn occurrence_attachments_rejected(&self) -> bool {
        self.occurrence_attachments_rejected
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use nudox_ir::{
        apply::PristineIntroTable,
        change::{EcosystemId, IntroId, PackageLineageId, PackageName, StableRef},
        entry::{Node, Symbol, Visibility},
        index::Ref,
        kind::Kind,
        kinds::{Function, Module, Trait, Type},
        view::IrView,
        vocab::{Confidence, Occurrence, ReferenceKind, RelSpan},
    };

    fn intro(n: u8) -> IntroId {
        IntroId::from_raw([n; 32])
    }

    fn lineage() -> PackageLineageId {
        PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("test-store"))
    }

    fn sym(name: &str) -> Symbol {
        Symbol {
            name: name.to_owned(),
            visibility: Visibility::Public,
            documentation: String::new(),
            source: std::path::PathBuf::new(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        }
    }

    fn make_view() -> IrView {
        let mut table = PristineIntroTable::new();
        table.insert_live(
            intro(1),
            nudox_ir::entry::Entry::new(
                sym("root"),
                Node::build(None::<nudox_ir::index::RawRef>, []),
                Kind::Module(Module),
            ),
            None,
        );
        table.insert_live(
            intro(2),
            nudox_ir::entry::Entry::new(
                sym("my_fn"),
                Node::build(None::<nudox_ir::index::RawRef>, []),
                Kind::Function(Function::builder().build()),
            ),
            Some(intro(1)),
        );
        IrView::with_package(lineage(), table)
    }

    #[test]
    fn build_populates_by_kind_and_by_name() {
        let view = make_view();
        let indexes = PackageIndexes::build(&view);

        // NameIndex: both "root" and "my_fn" must be present.
        assert!(!indexes.by_name.get_exact("root").is_empty());
        assert!(!indexes.by_name.get_exact("my_fn").is_empty());

        // by_kind: Module bucket has intro(1), Function bucket has intro(2).
        let modules = indexes.by_kind.get(&KindDiscriminant::Module).unwrap();
        assert!(modules.contains(&intro(1)));
        let fns = indexes.by_kind.get(&KindDiscriminant::Function).unwrap();
        assert!(fns.contains(&intro(2)));
    }

    #[test]
    fn usages_floor_is_confidence_index() {
        let mut table = PristineIntroTable::new();
        table.insert_live(
            intro(1),
            nudox_ir::entry::Entry::new(
                sym("root"),
                Node::build(None::<nudox_ir::index::RawRef>, []),
                Kind::Module(Module),
            ),
            None,
        );
        table.insert_live(
            intro(2),
            nudox_ir::entry::Entry::new(
                sym("caller"),
                Node::build(None::<nudox_ir::index::RawRef>, []),
                Kind::Function(Function::builder().build()),
            ),
            Some(intro(1)),
        );
        let mut view = IrView::with_package(lineage(), table);

        let target = StableRef::new(lineage(), intro(99));

        // Below the floor — must NOT appear.
        view.add_occurrence(
            intro(2),
            Occurrence::new(
                target.clone(),
                ReferenceKind::FunctionCall,
                Confidence::Syntactic,
                RelSpan::new(0, 5),
            ),
        );

        let indexes = PackageIndexes::build(&view);
        assert_eq!(indexes.usages_of(&target), &[] as &[IntroId]);
    }

    #[test]
    fn usages_at_index_confidence_appear() {
        let mut table = PristineIntroTable::new();
        table.insert_live(
            intro(1),
            nudox_ir::entry::Entry::new(
                sym("root"),
                Node::build(None::<nudox_ir::index::RawRef>, []),
                Kind::Module(Module),
            ),
            None,
        );
        table.insert_live(
            intro(2),
            nudox_ir::entry::Entry::new(
                sym("caller"),
                Node::build(None::<nudox_ir::index::RawRef>, []),
                Kind::Function(Function::builder().build()),
            ),
            Some(intro(1)),
        );
        let mut view = IrView::with_package(lineage(), table);

        let target = StableRef::new(lineage(), intro(99));
        view.add_occurrence(
            intro(2),
            Occurrence::new(
                target.clone(),
                ReferenceKind::FunctionCall,
                Confidence::Index,
                RelSpan::new(0, 5),
            ),
        );

        let indexes = PackageIndexes::build(&view);
        assert_eq!(indexes.usages_of(&target), &[intro(2)]);
    }

    #[test]
    fn mentions_from_trait_supers() {
        let base_intro = intro(10);
        let base_ref = Type::Nominal(Ref::Intro(base_intro));
        let mut table = PristineIntroTable::new();
        table.insert_live(
            intro(1),
            nudox_ir::entry::Entry::new(
                sym("root"),
                Node::build(None::<nudox_ir::index::RawRef>, []),
                Kind::Module(Module),
            ),
            None,
        );
        table.insert_live(
            intro(10),
            nudox_ir::entry::Entry::new(
                sym("BaseTrait"),
                Node::build(None::<nudox_ir::index::RawRef>, []),
                Kind::Trait(Trait::builder().build()),
            ),
            Some(intro(1)),
        );
        table.insert_live(
            intro(11),
            nudox_ir::entry::Entry::new(
                sym("DerivedTrait"),
                Node::build(None::<nudox_ir::index::RawRef>, []),
                Kind::Trait(Trait::builder().supers([base_ref]).build()),
            ),
            Some(intro(1)),
        );

        let view = IrView::with_package(lineage(), table);
        let indexes = PackageIndexes::build(&view);

        let base_sr = StableRef::new(lineage(), base_intro);
        assert_eq!(indexes.mentions_of(&base_sr), vec![intro(11)]);
        assert_eq!(
            indexes.type_refs_in(&base_sr, TypePosition::Supertrait),
            vec![intro(11)],
            "a supertrait bound must be recorded under Supertrait, not under \
             ImplementedTrait — `Trait.implementors` reads the latter"
        );
        assert!(
            indexes
                .type_refs_in(&base_sr, TypePosition::ImplementedTrait)
                .is_empty(),
            "DerivedTrait extends BaseTrait; it does not implement it"
        );
    }

    // -----------------------------------------------------------------------
    // Signature positions
    //
    // Each test names one position and asserts the *position*, not merely that
    // a posting exists: the whole point of the position is that folding two of
    // them together gives a wrong answer to a real question, so a test that
    // only checked "intro N is in the index somewhere" would pass against the
    // conflation it exists to prevent.
    // -----------------------------------------------------------------------

    /// The full signature fixture: a `Point` record with a typed field, a
    /// `distance(p: Point) -> Wrapper<Point>` function whose parameter and
    /// return types are separate `Param` entries, a `Display` trait, and
    /// `impl Display for Point`.
    ///
    /// | intro | name | kind |
    /// |---|---|---|
    /// | 1 | root | Module |
    /// | 2 | Point | Record |
    /// | 3 | Point.x | Field (ty = Point, self-referential on purpose) |
    /// | 4 | distance | Function |
    /// | 5 | distance.p | Param (input, ty = Point) |
    /// | 6 | distance.return | Param (output, ty = Wrapper<Point>) |
    /// | 7 | Display | Trait |
    /// | 8 | PointDisplay | Impl (of = Display, self_ty = Point) |
    /// | 9 | Wrapper | Record |
    fn signature_view() -> IrView {
        use nudox_ir::kinds::{Field, FieldKey, Impl, Param, Record};

        let point = Type::Nominal(Ref::Intro(intro(2)));
        let mut table = PristineIntroTable::new();

        let mut live = |id: IntroId, name: &str, kind: Kind, parent: Option<IntroId>| {
            table.insert_live(
                id,
                nudox_ir::entry::Entry::new(
                    sym(name),
                    Node::build(None::<nudox_ir::index::RawRef>, []),
                    kind,
                ),
                parent,
            );
        };

        live(intro(1), "root", Kind::Module(Module), None);
        live(
            intro(2),
            "Point",
            Kind::Record(Record::builder().fields([Ref::Intro(intro(3))]).build()),
            Some(intro(1)),
        );
        live(
            intro(3),
            "x",
            Kind::Field(
                Field::builder()
                    .key(FieldKey::Named)
                    .ty(point.clone())
                    .build(),
            ),
            Some(intro(2)),
        );
        live(
            intro(4),
            "distance",
            Kind::Function(
                Function::builder()
                    .input_params([Ref::Intro(intro(5))])
                    .output_params([Ref::Intro(intro(6))])
                    .build(),
            ),
            Some(intro(1)),
        );
        live(
            intro(5),
            "p",
            Kind::Param(Param::builder().ty(point.clone()).build()),
            Some(intro(4)),
        );
        live(
            intro(6),
            "return",
            Kind::Param(
                Param::builder()
                    .ty(Type::Apply {
                        base: Box::new(Type::Nominal(Ref::Intro(intro(9)))),
                        args: [point.clone()].into(),
                    })
                    .build(),
            ),
            Some(intro(4)),
        );
        live(
            intro(7),
            "Display",
            Kind::Trait(Trait::builder().build()),
            Some(intro(1)),
        );
        live(
            intro(8),
            "PointDisplay",
            Kind::Impl(
                Impl::builder()
                    .of(Type::Nominal(Ref::Intro(intro(7))))
                    .self_ty(point)
                    .build(),
            ),
            Some(intro(1)),
        );
        live(
            intro(9),
            "Wrapper",
            Kind::Record(Record::builder().build()),
            Some(intro(1)),
        );

        IrView::with_package(lineage(), table)
    }

    fn point_ref() -> StableRef {
        StableRef::new(lineage(), intro(2))
    }

    /// `impl Display for Point` must make `Point` findable as the *self* type,
    /// which is what "what does `Point` implement?" resolves through. Before
    /// this, `Impl::self_ty` was never read by the index at all.
    #[test]
    fn impl_self_type_is_indexed_separately_from_the_implemented_trait() {
        let indexes = PackageIndexes::build(&signature_view());

        assert_eq!(
            indexes.type_refs_in(&point_ref(), TypePosition::ImplSelf),
            vec![intro(8)],
            "PointDisplay is the impl whose Self type is Point"
        );
        assert!(
            indexes
                .type_refs_in(&point_ref(), TypePosition::ImplementedTrait)
                .is_empty(),
            "Point is implemented *for*, not implemented — folding self_ty into \
             the ImplementedTrait position is what would break Trait.implementors"
        );

        let display = StableRef::new(lineage(), intro(7));
        assert_eq!(
            indexes.type_refs_in(&display, TypePosition::ImplementedTrait),
            vec![intro(8)],
        );
        assert!(
            indexes
                .type_refs_in(&display, TypePosition::ImplSelf)
                .is_empty(),
        );
    }

    /// A function's parameter and return types live on separate `Param`
    /// entries; both must be attributed to the *function*, and the two must
    /// not be confused — "returns Y" and "takes Y" are different questions.
    #[test]
    fn parameter_and_return_types_are_attributed_to_the_function_and_kept_apart() {
        let indexes = PackageIndexes::build(&signature_view());

        assert_eq!(
            indexes.type_refs_in(&point_ref(), TypePosition::Parameter),
            vec![intro(4)],
            "distance takes a Point"
        );
        assert_eq!(
            indexes.type_refs_in(&point_ref(), TypePosition::Return),
            vec![intro(4)],
            "distance returns Wrapper<Point>, and a caller asking for Point \
             means that too"
        );

        let wrapper = StableRef::new(lineage(), intro(9));
        assert_eq!(
            indexes.type_refs_in(&wrapper, TypePosition::Return),
            vec![intro(4)],
        );
        assert!(
            indexes
                .type_refs_in(&wrapper, TypePosition::Parameter)
                .is_empty(),
            "Wrapper appears only in the return position"
        );

        // The `Param` entries themselves contribute nothing: their type is
        // recorded against the function that declares them.
        for param in [intro(5), intro(6)] {
            assert!(
                !indexes
                    .type_refs_of(&point_ref())
                    .iter()
                    .any(|(_, owner)| *owner == param),
                "Param entry {param:?} must not own a posting of its own"
            );
        }
    }

    /// A field's declared type is indexed against the field entry, so
    /// "which fields are of type Y" is one probe.
    #[test]
    fn field_types_are_indexed_against_the_field() {
        let indexes = PackageIndexes::build(&signature_view());
        assert_eq!(
            indexes.type_refs_in(&point_ref(), TypePosition::FieldType),
            vec![intro(3)],
        );
    }

    // -------------------------------------------------------------------
    // Regression: `Primitive::{Reference,MutPointer,ConstPointer}` nest a
    // `Type`, and `collect_stable_refs` (the `Reach::Whole` walk that
    // `TypePosition::FieldType` uses) once treated `Type::Primitive(_)` as a
    // leaf outright, dropping every `&T`, `&mut T`, `*const T` and `*mut T`
    // from the reverse index. "Who has a field of type `Config`" returned
    // empty for a field of type `&Config` and looked like a true negative.
    //
    // Every `Type` built below is real, hand-assembled IR — not a fixture
    // shaped to make an assertion pass — so a regression here means the
    // walk genuinely stopped reaching the pointee, not that a mock forgot
    // to wire one up.
    // -------------------------------------------------------------------

    /// A record with one field per way a language can address another type
    /// without naming it directly, plus one case where the nominal sits two
    /// levels down (`&Vec<Config>`: through the `Reference`'s `ty` and then
    /// through `Apply`'s `args`).
    ///
    /// | intro | name | ty |
    /// |---|---|---|
    /// | 1 | root | Module |
    /// | 2 | Config | Record |
    /// | 3 | Vec | Record (stand-in generic container) |
    /// | 4 | by_ref | Field, `&Config` |
    /// | 5 | by_mut_ref | Field, `&mut Config` |
    /// | 6 | by_const_ptr | Field, `*const Config` |
    /// | 7 | by_mut_ptr | Field, `*mut Config` |
    /// | 8 | by_ref_to_vec | Field, `&Vec<Config>` |
    fn pointer_and_reference_view() -> IrView {
        use nudox_ir::kinds::{Field, FieldKey, Record, ty::Primitive};

        let config = Type::Nominal(Ref::Intro(intro(2)));
        let reference = |mutable: bool, ty: Type| {
            Type::Primitive(Primitive::Reference {
                lifetime: None,
                mutable,
                ty: Box::new(ty),
            })
        };

        let mut table = PristineIntroTable::new();
        let mut live = |id: IntroId, name: &str, kind: Kind, parent: Option<IntroId>| {
            table.insert_live(
                id,
                nudox_ir::entry::Entry::new(
                    sym(name),
                    Node::build(None::<nudox_ir::index::RawRef>, []),
                    kind,
                ),
                parent,
            );
        };
        live(intro(1), "root", Kind::Module(Module), None);
        live(
            intro(2),
            "Config",
            Kind::Record(Record::builder().build()),
            Some(intro(1)),
        );
        live(
            intro(3),
            "Vec",
            Kind::Record(Record::builder().build()),
            Some(intro(1)),
        );

        // `field` takes its own exclusive borrow of `live` for the rest of
        // its scope, so every direct `live(...)` call above must come first.
        let mut field = |id: IntroId, name: &str, ty: Type| {
            live(
                id,
                name,
                Kind::Field(Field::builder().key(FieldKey::Named).ty(ty).build()),
                Some(intro(1)),
            );
        };

        field(intro(4), "by_ref", reference(false, config.clone()));
        field(intro(5), "by_mut_ref", reference(true, config.clone()));
        field(
            intro(6),
            "by_const_ptr",
            Type::Primitive(Primitive::ConstPointer(Box::new(config.clone()))),
        );
        field(
            intro(7),
            "by_mut_ptr",
            Type::Primitive(Primitive::MutPointer(Box::new(config.clone()))),
        );
        field(
            intro(8),
            "by_ref_to_vec",
            reference(
                false,
                Type::Apply {
                    base: Box::new(Type::Nominal(Ref::Intro(intro(3)))),
                    args: [config].into(),
                },
            ),
        );

        IrView::with_package(lineage(), table)
    }

    fn config_ref() -> StableRef {
        StableRef::new(lineage(), intro(2))
    }

    /// `&Config` as a field type must make `Config` findable through it.
    #[test]
    fn shared_reference_field_type_reaches_its_pointee() {
        let indexes = PackageIndexes::build(&pointer_and_reference_view());
        assert!(
            indexes
                .type_refs_in(&config_ref(), TypePosition::FieldType)
                .contains(&intro(4)),
            "&Config must reach Config through Primitive::Reference, not be \
             swallowed as an opaque primitive leaf"
        );
    }

    /// `&mut Config` as a field type must make `Config` findable through it.
    #[test]
    fn mutable_reference_field_type_reaches_its_pointee() {
        let indexes = PackageIndexes::build(&pointer_and_reference_view());
        assert!(
            indexes
                .type_refs_in(&config_ref(), TypePosition::FieldType)
                .contains(&intro(5)),
            "&mut Config must reach Config through Primitive::Reference"
        );
    }

    /// `*const Config` as a field type must make `Config` findable through it.
    #[test]
    fn const_pointer_field_type_reaches_its_pointee() {
        let indexes = PackageIndexes::build(&pointer_and_reference_view());
        assert!(
            indexes
                .type_refs_in(&config_ref(), TypePosition::FieldType)
                .contains(&intro(6)),
            "*const Config must reach Config through Primitive::ConstPointer"
        );
    }

    /// `*mut Config` as a field type must make `Config` findable through it.
    #[test]
    fn mut_pointer_field_type_reaches_its_pointee() {
        let indexes = PackageIndexes::build(&pointer_and_reference_view());
        assert!(
            indexes
                .type_refs_in(&config_ref(), TypePosition::FieldType)
                .contains(&intro(7)),
            "*mut Config must reach Config through Primitive::MutPointer"
        );
    }

    /// `&Vec<Config>` reaches `Config` two levels down: through the
    /// `Reference`'s `ty` (a `Primitive` nesting a `Type`) and then through
    /// `Apply`'s `args` (an ordinary recursive case that was never broken).
    /// Both hops must work together, not just each in isolation.
    #[test]
    fn reference_to_generic_application_reaches_the_nested_pointee() {
        let indexes = PackageIndexes::build(&pointer_and_reference_view());
        assert!(
            indexes
                .type_refs_in(&config_ref(), TypePosition::FieldType)
                .contains(&intro(8)),
            "&Vec<Config> must reach Config two levels down"
        );
    }

    /// Every one of the five pointer/reference fields above must be present
    /// at once — this is the full posting set `Config`'s reverse index
    /// should carry, not five isolated coincidences.
    #[test]
    fn all_pointer_and_reference_field_types_are_indexed_together() {
        let indexes = PackageIndexes::build(&pointer_and_reference_view());
        let mut found = indexes.type_refs_in(&config_ref(), TypePosition::FieldType);
        found.sort_unstable();
        assert_eq!(
            found,
            vec![intro(4), intro(5), intro(6), intro(7), intro(8)],
            "Config must be reachable through &T, &mut T, *const T, *mut T, \
             and &Vec<T> — got: {found:?}"
        );
    }

    /// `mentions` must keep meaning exactly what it meant: the trait an impl
    /// implements and a supertrait bound — *not* every signature position.
    ///
    /// This is the regression guard for the conflation: with `Point` named in
    /// four positions, `mentions_of(Point)` must still be empty, because none
    /// of them is relational.
    #[test]
    fn mentions_stays_relational_even_when_the_type_is_named_everywhere() {
        let indexes = PackageIndexes::build(&signature_view());

        assert!(
            indexes.mentions_of(&point_ref()).is_empty(),
            "Point is a field type, a parameter type, a return type and an impl \
             self type — and none of those is a relationship between \
             declarations. got: {:?}",
            indexes.mentions_of(&point_ref())
        );
        assert_eq!(
            indexes.mentions_of(&StableRef::new(lineage(), intro(7))),
            vec![intro(8)],
            "Display is implemented by PointDisplay"
        );
    }

    /// A generic application contributes its *head* to the relational
    /// positions and *every* nominal to the signature positions.
    ///
    /// `impl Display for Wrapper<Point>` is not an implementation for `Point`;
    /// `fn f() -> Wrapper<Point>` is a function a `Point` search should find.
    #[test]
    fn generic_arguments_reach_signature_positions_but_not_relational_ones() {
        use nudox_ir::kinds::Impl;

        let mut table = PristineIntroTable::new();
        table.insert_live(
            intro(1),
            nudox_ir::entry::Entry::new(
                sym("root"),
                Node::build(None::<nudox_ir::index::RawRef>, []),
                Kind::Module(Module),
            ),
            None,
        );
        table.insert_live(
            intro(2),
            nudox_ir::entry::Entry::new(
                sym("WrapperDisplay"),
                Node::build(None::<nudox_ir::index::RawRef>, []),
                Kind::Impl(
                    Impl::builder()
                        .of(Type::Nominal(Ref::Intro(intro(50))))
                        .self_ty(Type::Apply {
                            base: Box::new(Type::Nominal(Ref::Intro(intro(51)))),
                            args: [Type::Nominal(Ref::Intro(intro(52)))].into(),
                        })
                        .build(),
                ),
            ),
            Some(intro(1)),
        );

        let view = IrView::with_package(lineage(), table);
        let indexes = PackageIndexes::build(&view);

        let wrapper = StableRef::new(lineage(), intro(51));
        let inner = StableRef::new(lineage(), intro(52));

        assert_eq!(
            indexes.type_refs_in(&wrapper, TypePosition::ImplSelf),
            vec![intro(2)],
            "the impl is for Wrapper<…>, so Wrapper is the self type"
        );
        assert!(
            indexes.type_refs_of(&inner).is_empty(),
            "`impl Display for Wrapper<Point>` is not an implementation for \
             Point; a Head-reach position must not record the argument"
        );
    }

    #[test]
    fn paths_precomputed() {
        let view = make_view();
        let indexes = PackageIndexes::build(&view);

        // intro(2) = "my_fn" whose parent is intro(1) = "root".
        let path = indexes.path_of(intro(2)).expect("path must be present");
        assert_eq!(path.as_ref(), "root.my_fn");
    }

    /// **Guard for MCP-SURFACE-PLAN §4.14.** `seal()`'s pass 2 has a branch
    /// (`seal.rs`'s "other collision" arm) that mints `Disambiguator::Span`
    /// directly for any same-key collision that is neither `Function` nor
    /// `Impl` — no signature or trait-impl skeleton to try first, so nothing
    /// routes it through pass 2.5's escalation loop. If the colliding
    /// declarations' spans already differ (true here: `LIMIT_A` and
    /// `LIMIT_B` occupy different byte ranges), the resulting ids are
    /// already distinct and pass 2.5 never touches this group at all — so it
    /// never appears in `SealReport::forced_keys`.
    ///
    /// This is exactly the shape that made `KeyProvenance::from_seal_report`
    /// (when it read `forced_keys`) lie: both declarations are genuinely
    /// `Span`-keyed, but `forced_keys` never named them, so their reported
    /// tier was `Structural` — "content-derived, safe to cache" — for a key
    /// that is neither. An agent trusting that would cache the key across a
    /// version bump and have it silently stop resolving.
    ///
    /// Two `Const`s (not `Function`, not `Impl`) sharing a name under one
    /// module reproduce the shape without needing a real producer: `seal()`
    /// itself does not care whether the input came from a language producer
    /// or was hand-built, so this exercises the real `seal()` code path, not
    /// a stand-in for it.
    #[test]
    fn key_tier_reports_span_for_a_non_function_non_impl_collision_pass_2_mints_directly() {
        use nudox_ir::{
            build::{Const, IrPackage, PackageId, Type},
            change::{EcosystemId, PackageName},
            entry::{Symbol, Visibility},
            foreign::Unlinked,
        };

        fn sym_at(name: &str, start: usize, end: usize) -> Symbol {
            Symbol {
                name: name.to_owned(),
                visibility: Visibility::Public,
                documentation: String::new(),
                source: std::path::PathBuf::new(),
                span: start..end,
                aliases: Box::new([]),
                deprecation: None,
                doc_links: Box::new([]),
                attrs: Box::new([]),
                cfg: None,
            }
        }

        let mut next = 0usize;
        let mut next_id = move || {
            next += 1;
            next
        };

        // Two top-level `Const`s, same name ⇒ same `(kind, ancestor-path,
        // leaf-name)` base key, but DIFFERENT spans — the premise that lets
        // pass 2 resolve the collision with `Span` alone, with no repeat
        // collision for pass 2.5 to ever see.
        let pkg: IrPackage<usize> = IrPackage::build(
            PackageId::path("colliding-consts"),
            sym_at("root", 0, 0),
            |mut root| {
                root.create(next_id(), sym_at("LIMIT", 10, 20), |_| {
                    Const::builder().ty(Type::I32).build()
                });
                root.create(next_id(), sym_at("LIMIT", 30, 40), |_| {
                    Const::builder().ty(Type::I32).build()
                });
            },
        );

        let lineage = PackageLineageId::new(
            EcosystemId::new("test"),
            PackageName::new("colliding-consts"),
        );
        let outcome = pkg.seal(&lineage, &Unlinked);

        assert!(
            outcome.report.collisions.is_empty(),
            "the escalation ladder must resolve this collision, not drop a \
             declaration: {:?}",
            outcome.report.collisions
        );

        let limits: Vec<IntroId> = outcome
            .table
            .iter()
            .filter(|(_, e)| e.sym().name == "LIMIT")
            .map(|(intro, _)| intro)
            .collect();
        assert_eq!(
            limits.len(),
            2,
            "both consts must seal to distinct, live IntroIds"
        );

        // Sanity check on the test's own premise: this collision must
        // resolve inside pass 2, with pass 2.5 never running for it — or
        // this test is not exercising the blind spot §4.14 describes.
        assert!(
            outcome.report.forced_keys.is_empty(),
            "test premise violated: pass 2.5 ran for this group (forced_keys \
             = {:?}); the collision must resolve directly in pass 2 via \
             differing spans for this guard to mean anything",
            outcome.report.forced_keys
        );

        let view = IrView::with_package(lineage, outcome.table);
        let pkg_view = PackageView::build_sealed(view, Provenance::TrustedLocal, &outcome.report);

        for limit in limits {
            assert_eq!(
                pkg_view.key_tier(limit),
                Some(KeyTier::Span),
                "declaration {limit:?} was minted with Disambiguator::Span by \
                 pass 2's direct fallback (never touched by pass 2.5), but \
                 KeyProvenance reports its tier as {:?} — an agent reading \
                 `keyTier` would wrongly believe this key is safe to cache \
                 across a version switch",
                pkg_view.key_tier(limit)
            );
        }
    }
}
