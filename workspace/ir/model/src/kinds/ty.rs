use std::num::{NonZero, NonZeroU16};

use crate::{List, entry::AttrTok, index::RawRef, visitor::Visitor};

#[derive(Debug, Clone, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum Type {
    /// A receiver/self type such as Rust `Self` or TypeScript `this`.
    SelfType,

    /// A fundamental, language-level built-in type.
    /// Ex: `i32`, `f64`, `bool`.
    Primitive(Primitive),

    /// A fixed-length, heterogeneous collection of types, with optional
    /// per-element labels.
    ///
    /// Ex: `(i32, String)` (positional), `(int start, int end)` in C#,
    /// `[start: number, end: number]` in TypeScript.
    ///
    /// An empty list `()` represents the unit type. C# value tuples and
    /// TypeScript tuple labels are carried losslessly as [`TupleElement::Named`]
    /// entries; unnamed positions use [`TupleElement::Positional`].
    Tuple(List<TupleElement>),

    /// A dynamically-sized view into a contiguous sequence.
    /// Ex: `[u8]` or `[]T`.
    Slice(Box<Type>),

    /// A fixed-size contiguous sequence.
    /// Ex: `[i32; 4]` or `std::array<int, 4>`.
    Array { ty: Box<Type>, length: usize },

    /// An untagged union or sum of types.
    /// Ex: `string | number`.
    Union(List<Type>),

    /// An intersection or combination of types.
    /// Ex: `Serializable & Cloneable`.
    Intersection(List<Type>),

    /// Represents a type that cannot exist (Bottom Type).
    /// Ex: `!` in Rust, `never` in TypeScript, `NoReturn` in Python.
    Never,

    /// The language's genuine **top type**: the one type every value inhabits.
    ///
    /// Ex: `java.lang.Object` (Java), `System.Object` / `object` (C#),
    /// `any` / `interface{}` (Go), `unknown` (TypeScript).
    ///
    /// **This is a real type, not an absence of knowledge.** A top type is
    /// *upcast-only*: every value is assignable *to* it, and nothing is
    /// assignable *from* it without a checked cast. It has a declaration, a
    /// method set, and a place in the subtype lattice.
    ///
    /// It is emphatically **not** the gradual-typing escape hatch — Python's
    /// `typing.Any`, TypeScript's `any`, C#'s `dynamic` — which is
    /// bidirectionally consistent with every type and suppresses checking
    /// rather than constraining it. That is
    /// [`UnknownType::DynamicallyTyped`], and it lives under
    /// [`Type::Unknown`].
    ///
    /// Nor is it "the producer does not know". Every such case is a named
    /// reason under [`Type::Unknown`]; see that variant for why.
    Any,

    /// **The type is not known**, together with the reason it is not known.
    ///
    /// # Why this exists
    ///
    /// [`Type::Any`] used to carry at least four unrelated facts at once, and
    /// a consumer could not tell them apart. A ruff-based census of 87,101
    /// annotation positions across 22 pypi packages measured the collapse:
    ///
    /// | fact | positions | share |
    /// |---|---:|---:|
    /// | unannotated (`ty: None` — not representable *at all*) | 19,741 | 22.7% |
    /// | explicitly dynamic (`typing.Any`, `Literal[…]`, `...`) | 7,586 | 8.7% |
    /// | cross-package nominal, unresolved | 20,227 | 23.2% |
    /// | genuinely structured | 39,531 | 45.4% |
    ///
    /// 41.3% of annotated positions collapsed onto one opcode, and a fourth
    /// case could not be spelled even as `Any`. Of the 20,227 unresolved
    /// nominals, 13,724 (67.8%) carry a bare short name that *does* match a
    /// fully-qualified id the same package declares elsewhere — `click/core.py`
    /// writing `-> "Context"` where the package declares `click.core.Context`.
    /// That is a syntactically-recoverable state, not "genuinely external", and
    /// it gets its own variant so a link pass can find it by grep and by match.
    ///
    /// # The shape
    ///
    /// This mirrors [`Unlocated`](crate::entry::Unlocated), which solved the
    /// identical problem for source locations: named, exhaustively-matched
    /// reasons rather than a bare `None`. See [`UnknownType`] for why the
    /// reason enum is deliberately *not* `#[non_exhaustive]`.
    ///
    /// # Boundary with [`Type::Inferred`]
    ///
    /// [`Type::Inferred`] is a **written source construct** — Rust's `_`,
    /// clang's `__auto_type`, C#'s `var` — where the author explicitly asked
    /// the compiler to infer. `Type::Unknown` is the *absence* of a resolved
    /// type, which the author never asked for. `_` is a request; `Unknown` is
    /// a gap.
    Unknown(UnknownType),

    /// A **nominal** reference to a declared type (record/enum/trait/alias).
    /// Ex: `Bar`, `std::string::String`, `java.util.List`.
    ///
    /// Untyped ([`RawRef`]) because a nominal type may name any type-like kind.
    /// Like every other reference it is `Local` while building and lowered to
    /// `Intro`/`Foreign` by [`seal`](crate::package), so it is
    /// content-addressed in the sealed table.
    Nominal(RawRef),

    /// A **generic application** of a base type to arguments.
    /// Ex: `Bar<u32>`, `Vec<T>`, `HashMap<K, V>`, `List<String>`.
    ///
    /// `base` is normally a [`Type::Nominal`]; `args` are the applied types.
    /// This is what distinguishes `Foo for Bar<u32>` from `Foo for
    /// Bar<String>`.
    Apply { base: Box<Type>, args: List<Type> },

    /// A reference to a **generic parameter**, by name.
    /// Ex: the `T` in `fn id<T>(x: T) -> T`.
    ///
    /// This is a *use* of a type parameter, distinct from its *declaration* in
    /// [`GenericParam`](crate::kinds::GenericParam). Without it, every producer
    /// has to lower `T` to [`Type::Any`], which loses the link between a
    /// signature and the parameter list that binds it — Go, C# and Java all
    /// independently hit this.
    ///
    /// The name is carried verbatim. Note this makes a bare `TypeVar`
    /// *not* alpha-equivalent; identity skeletons deliberately exclude generic
    /// parameter names, so [`skeleton`](crate::skeleton) encodes only the
    /// opcode and not the name.
    TypeVar(String),

    /// A use-site wildcard with optional bound.
    /// Ex: Java `?`, `? extends T`, `? super T`; TypeScript's `unknown` in
    /// variance position; Kotlin's `out`/`in` projections.
    ///
    /// `bound` is `None` for an unbounded wildcard (`?`).
    Wildcard {
        variance: Variance,
        bound: Option<Box<Type>>,
    },

    /// A **callable type** expressed as a structural signature: parameter types,
    /// return type, and an optional calling-convention/ABI string.
    ///
    /// Requested by Go, Rust, C# and C/C++ (4 of 7 producers). Each had
    /// independently lowered anonymous callable types to `Type::Any`:
    ///
    /// - C: `void (*)(int, bool)` — C function pointers.
    /// - Rust: `fn(i32) -> bool` — bare fn types.
    /// - C#: `delegate*<int, bool>` managed delegates and
    ///   `delegate* unmanaged[Cdecl]<int, bool>` unmanaged delegates.
    /// - Go: `func(int) bool` — anonymous function types.
    ///
    /// **ABI slot rationale:** C# `delegate* unmanaged[Cdecl]<>` is real API
    /// surface — two delegates that differ only in calling convention have
    /// distinct ABI contracts and must not unify. The slot is `Option<String>`
    /// and is `None` for managed/default-ABI callable types, matching the
    /// parallel treatment of `Function::abi`. Carrying it here (rather than
    /// hiding it in an attribute) keeps the type structurally comparable and
    /// makes the ABI visible to Trustfall queries without payload inspection.
    FunctionPointer {
        // Types of the positional parameters, in order.
        params: List<Type>,
        // Return type. None for void/unit or absent return annotation.
        ret: Option<Box<Type>>,
        // Calling convention / ABI string, if explicitly specified.
        // Examples: "Cdecl", "StdCall", "C". None = language default.
        abi: Option<String>,
    },

    /// A type with attached metadata annotation.
    ///
    /// Requested by Java, C# and Python (3 of 7 producers). Each language
    /// independently needed to attach source-level annotation metadata to a type
    /// without losing the underlying type identity:
    ///
    /// - Java: `@NonNull String` — nullability and other annotations on types.
    /// - C#: nullable-reference types — `string?` vs `string` (oblivious) vs
    ///   `string` (not-annotated). Without this variant, `notAnnotated` and
    ///   `oblivious` are indistinguishable, a real information loss.
    /// - Python: `Annotated[T, meta]` — PEP 593 metadata attached to a type.
    ///
    /// **Payload choice:** `AttrTok` (the same type used by `Symbol::attrs`) is
    /// reused here rather than a bare `String`. This keeps the schema consistent
    /// and lets downstream consumers handle type annotations with the same
    /// tooling they already use for symbol attributes. `token` carries the
    /// annotation name; `arg` carries its argument if any.
    Annotated {
        // The underlying type being annotated.
        inner: Box<Type>,
        // The annotation, using the same AttrTok type as Symbol::attrs.
        annotation: AttrTok,
    },

    // ─── TypeScript-specific structural types ───────────────────────────────
    /// A TypeScript **conditional type**: `Check extends Extends ? Then : Else`.
    ///
    /// TypeScript conditional types let types branch on assignability:
    /// `T extends string ? A : B` resolves to `A` when `T` is assignable to
    /// `string`, and `B` otherwise. The four arms are all load-bearing:
    /// `check` is the type under test, `extends_ty` is the bound, `then_ty`
    /// is taken when the condition holds, `else_ty` when it fails.
    ///
    /// Previously emitted as `Type::Unsupported` / `Type::Any` by the
    /// TypeScript producer, erasing the entire conditional structure.
    Conditional {
        check: Box<Type>,
        extends_ty: Box<Type>,
        then_ty: Box<Type>,
        else_ty: Box<Type>,
    },

    /// A TypeScript **mapped type**: `{ readonly [P in keyof T]?: T[P] }`.
    ///
    /// Mapped types iterate over the keys of a source type and produce a new
    /// object type. `key_var` is the bound variable name (alpha-equivalent —
    /// excluded from the skeleton); `source` is the type being iterated;
    /// `value` is the body type per property; `readonly` and `optional` are
    /// tri-state modifiers. A bare `bool` cannot represent the three TS states
    /// (`+`/`-`/absent) — see [`MappedModifier`].
    ///
    /// **Identity decision:** `readonly` and `optional` are identity-relevant.
    /// `{ readonly [P in K]: V }` and `{ [P in K]: V }` have different
    /// assignability rules. `key_var` is alpha-equivalent and excluded from the
    /// skeleton, like `TypeVar` names and `GenericParam` names.
    ///
    /// Previously emitted as `Type::Unsupported` / `Type::Any` by the
    /// TypeScript producer.
    Mapped {
        key_var: String,
        source: Box<Type>,
        value: Box<Type>,
        readonly: MappedModifier,
        optional: MappedModifier,
    },

    /// A TypeScript **template literal type**: `` `prefix-${T}` ``.
    ///
    /// Alternates fixed string spans with interpolated type positions via
    /// [`TemplatePart`]. The literal text IS identity-relevant: `` `a-${T}` ``
    /// and `` `b-${T}` `` are distinct types.
    ///
    /// Previously emitted as `Type::Unsupported` / `Type::Any` by the
    /// TypeScript producer.
    TemplateLiteral(List<TemplatePart>),

    // ─── Anonymous structural types (Go + TypeScript) ───────────────────────
    /// An **anonymous structural record** type: Go's `struct { X int }` or a
    /// TypeScript object-literal type `{ x: number; y?: string }`.
    ///
    /// Named records (declared with a name) are `Type::Nominal`. This variant
    /// covers inline structural types with no nominal name. Fields carry name,
    /// type, `optional`, and `readonly` — both Go and TypeScript expose these
    /// at the type level. For Go anonymous interfaces (method sets), `form` is
    /// `AnonRecordForm::Interface` and each method's `ty` is a
    /// `Type::FunctionPointer`. This unifies struct and interface anonymous
    /// types under one variant rather than duplicating encoding infrastructure.
    ///
    /// Previously emitted as `Type::Any` by Go and TypeScript producers.
    AnonymousRecord {
        form: AnonRecordForm,
        members: List<AnonField>,
    },

    // ─── Rust-specific types ────────────────────────────────────────────────
    /// Rust **`impl Trait`** in argument or return position.
    ///
    /// An opaque existential type: the compiler selects a concrete type
    /// statically; the call site only sees the bound set. Distinct from
    /// `dyn Trait` (see [`Type::DynTrait`]): `impl Trait` is zero-cost static
    /// dispatch; `dyn Trait` is a fat-pointer with dynamic dispatch. Collapsing
    /// them into one variant with a flag would obscure this fundamental
    /// semantic difference.
    ///
    /// Previously emitted as `Type::Any` by the Rust producer.
    ImplTrait(List<Type>),

    /// Rust **`dyn Trait`** (dynamic dispatch, fat-pointer object).
    ///
    /// A fat pointer with a vtable, requiring object safety. Previously
    /// squeezed into `Type::Intersection`, which loses the distinction between
    /// a structural intersection (`A & B`) and a trait-object type
    /// (`dyn A + B`). Separate from [`Type::ImplTrait`] — see that variant for
    /// the full rationale.
    DynTrait(List<Type>),

    /// A **written inference request**: Rust's `_` wildcard, clang's
    /// `auto`/`__auto_type`/`decltype(auto)`, C#'s `var`.
    ///
    /// The author explicitly asked the compiler to work the type out. That is
    /// a source construct with a spelling, and it is what separates this from
    /// [`Type::Unknown`]:
    ///
    /// - `Inferred` — the source said "you figure it out". A request.
    /// - `Unknown(reason)` — nobody asked; the producer simply has no type,
    ///   and the reason says why. A gap.
    /// - `Any` — a genuine top type, `Object`/`interface{}`. A real type.
    ///
    /// **Note on foreign-ref fallbacks:** cross-package references do not
    /// belong here either. `Ref::Foreign` already exists for that case; where
    /// a producer cannot yet build one, the honest encoding is
    /// [`UnknownType::UnresolvedExternal`], which keeps the name.
    Inferred,

    /// A **qualified path** expression: `<T as Trait>::Assoc` (Rust) or
    /// `Outer<T>.Inner` (C#).
    ///
    /// `self_ty` is the base type; `trait_ref` is the disambiguation trait
    /// (`None` for C#-style dot-qualified paths); `assoc` is the associated
    /// item name.
    ///
    /// Previously emitted as `Type::Any` by both the Rust and C# producers.
    QualifiedPath {
        self_ty: Box<Type>,
        trait_ref: Option<Box<Type>>,
        assoc: String,
    },
}

// ─── Supporting types for the new variants ──────────────────────────────────

/// Why a type position has no resolved type.
///
/// # Deliberately exhaustive
///
/// **No `#[non_exhaustive]`**, for exactly the reason
/// [`Unlocated`](crate::entry::Unlocated) gives and doctrine §3 states. This
/// enum is internal to the workspace, and its whole value is that adding a
/// reason breaks every match and forces each producer and consumer to decide
/// what the new kind of absence means.
///
/// `#[non_exhaustive]` would achieve the *opposite* of the stated goal: it
/// forces every downstream crate to write a `_ =>` arm, which is precisely the
/// collapse this type exists to undo. A wildcard arm anywhere here recreates
/// `Type::Any`.
///
/// # What does *not* belong here
///
/// - A genuine top type (`Object`, `interface{}`) — that is [`Type::Any`].
/// - A written inference request (`_`, `auto`, `var`) — that is
///   [`Type::Inferred`].
/// - A cross-package reference the producer *can* name — that is
///   `Type::Nominal(Ref::Foreign { .. })`. `Ref::Foreign` carries a real key
///   and a producer needs no registry handle to build one; a producer reaching
///   for [`UnknownType::UnresolvedExternal`] when it holds enough to build a
///   `ForeignKey` is erasing information it has.
#[derive(Debug, Clone, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum UnknownType {
    /// **The source wrote no type here at all.**
    ///
    /// A Python parameter with no annotation, a TypeScript `const` with no
    /// type and no lowered initializer, a Go declaration whose oracle node
    /// carries no `type`. 22.7% of the Python census — and before this
    /// variant existed the IR could not represent it, because the producer's
    /// only move was `.unwrap_or(Type::Any)`, which said "dynamic" about a
    /// position where the source said nothing.
    ///
    /// Distinct from [`UnknownType::DynamicallyTyped`]: `def f(x):` and
    /// `def f(x: Any):` are different source, mean different things to a type
    /// checker, and must not share an encoding.
    Unannotated,

    /// **The source explicitly asked for dynamic typing.**
    ///
    /// Python `typing.Any`, TypeScript `any`, C# `dynamic`. The gradual-typing
    /// escape hatch: bidirectionally consistent with every type, suppressing
    /// checks in both directions.
    ///
    /// Distinct from [`Type::Any`], which is the *top* type — upcast-only,
    /// with a real declaration and method set. `object` constrains; `dynamic`
    /// abdicates. Collapsing them loses the single most load-bearing fact
    /// about a gradually-typed API surface.
    DynamicallyTyped,

    /// **A short, unqualified name that matches a fully-qualified id this same
    /// package declares elsewhere** — the import graph has simply not been
    /// walked.
    ///
    /// 13,724 of the Python census's 20,227 unresolved nominals (67.8%) are
    /// this: `click/core.py` writes `-> "Context"` while the package declares
    /// `click.core.Context`. Nothing external is involved; the producer is one
    /// name-resolution pass short.
    ///
    /// Recorded separately from [`UnknownType::UnresolvedExternal`] because it
    /// is resolvable **without any cross-package linking** — a within-package
    /// pass closes it. Grep for it to size that pass.
    UnresolvedLocalName {
        // The name exactly as written in source (`"Context"`).
        name: String,
    },

    /// **A name that resolves outside this package**, and the producer could
    /// not build a `ForeignKey` for it.
    ///
    /// This is the honest residue of the acquisition boundary: the producer
    /// has a spelling but no canonical cross-package path. A registry link
    /// pass rewrites these into `Type::Nominal(Ref::Foreign { .. })`.
    ///
    /// The `name` is carried, not dropped. Two distinct external types must
    /// stay distinguishable — erasing both to one opcode is what collapsed
    /// overload sets in Java and C# before `Ref::Foreign` existed, and this
    /// variant must not reintroduce that.
    UnresolvedExternal {
        // The name as the producer saw it — qualified where possible.
        name: String,
    },

    /// **The producer's own recursion guard fired**; a real type exists below
    /// this point and was not walked.
    ///
    /// Go, C# and Java all carry a `MAX_DEPTH` and all three returned
    /// `Type::Any` on hitting it. Distinct from every other variant because it
    /// is *our* limit, not the language's and not the oracle's: raising
    /// `MAX_DEPTH` closes it with no new information from anywhere.
    TruncatedAtDepthLimit,

    /// **The language front-end handed the producer nothing usable** at this
    /// position.
    ///
    /// Go's `TypeKind::Invalid`, C#'s `TypeSig::Error`, Java's
    /// `TypeMirror::None`/`Null`, and a rust-analyzer AST node whose type
    /// child is absent because the source did not parse. The upstream tool has
    /// already failed and already reported; the producer is propagating that,
    /// not adding to it.
    ///
    /// Distinct from [`UnknownType::TruncatedAtDepthLimit`]: no amount of
    /// extra effort inside the producer closes this one.
    OracleGap,

    /// **The construct is known and named, and this IR has no slot for it.**
    ///
    /// Go `complex64`/`complex128`/`unsafe.Pointer`, a Java primitive spelling
    /// outside the eight, a TypeScript `TypeGuard` predicate. The producer
    /// knows exactly what it is looking at.
    ///
    /// This is the type-lattice backlog, in-band, in the same spirit as
    /// [`Unlocated::ProducerRecordsNoLocation`](crate::entry::Unlocated):
    /// grep for it to count what is left, and `construct` names each gap so
    /// the count is actionable rather than a number.
    NoIrRepresentation {
        // The source-level construct, e.g. `"complex128"`, `"unsafe.Pointer"`.
        construct: String,
    },
}

impl UnknownType {
    /// A short, stable, language-neutral tag for this reason.
    ///
    /// Intended for renderers, query filters and metrics that need to *name*
    /// the reason without matching on it. Kept in sync with the variants by
    /// the exhaustive match below.
    pub fn tag(&self) -> &'static str {
        match self {
            UnknownType::Unannotated => "unannotated",
            UnknownType::DynamicallyTyped => "dynamically-typed",
            UnknownType::UnresolvedLocalName { .. } => "unresolved-local-name",
            UnknownType::UnresolvedExternal { .. } => "unresolved-external",
            UnknownType::TruncatedAtDepthLimit => "truncated-at-depth-limit",
            UnknownType::OracleGap => "oracle-gap",
            UnknownType::NoIrRepresentation { .. } => "no-ir-representation",
        }
    }

    /// The source spelling this reason carries, when it carries one.
    ///
    /// `Some` only for the variants that recovered a name; `None` is the
    /// honest answer for the rest, not a missing feature.
    pub fn spelling(&self) -> Option<&str> {
        match self {
            UnknownType::UnresolvedLocalName { name }
            | UnknownType::UnresolvedExternal { name } => Some(name),
            UnknownType::NoIrRepresentation { construct } => Some(construct),
            UnknownType::Unannotated
            | UnknownType::DynamicallyTyped
            | UnknownType::TruncatedAtDepthLimit
            | UnknownType::OracleGap => None,
        }
    }
}

/// A tri-state modifier on a mapped type's `readonly` or `optional` position.
///
/// TypeScript distinguishes three states:
/// - `Add` (`readonly` / `+readonly` / `?` / `+?`) — explicitly added.
/// - `Remove` (`-readonly` / `-?`) — explicitly removed from the source type.
/// - `Absent` — not mentioned; inherited from the source type.
///
/// A bare `bool` cannot represent this: `Add` and `Absent` would collapse
/// into the same value, losing either the removal or the inheritance case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum MappedModifier {
    /// Modifier explicitly added (`readonly` / `+readonly` / `?` / `+?`).
    Add,
    /// Modifier explicitly removed (`-readonly` / `-?`).
    Remove,
    /// Modifier absent — inherited from the source type.
    Absent,
}

/// A single part of a [`Type::TemplateLiteral`].
///
/// Template literal types alternate fixed string spans with interpolated
/// type positions: `` `error-${Code}: ${Message}` `` is
/// `Literal("error-") | Interpolated(Code) | Literal(": ") | Interpolated(Message)`.
#[derive(Debug, Clone, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum TemplatePart {
    /// A fixed string literal span (e.g. `"error-"`).
    Literal(String),
    /// An interpolated type position (e.g. `Code` or `number`).
    Interpolated(Box<Type>),
}

/// The form of an [`Type::AnonymousRecord`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum AnonRecordForm {
    /// An anonymous struct (`struct { X int }` in Go; `{ x: number }` in
    /// TypeScript).
    Struct,
    /// An anonymous interface / method-set type (`interface { Foo() bool }` in
    /// Go).
    Interface,
}

/// A member of an [`Type::AnonymousRecord`].
///
/// Used for both struct fields (Go / TypeScript) and interface methods (Go).
/// For methods the `ty` field carries a [`Type::FunctionPointer`].
#[derive(Debug, Clone, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct AnonField {
    /// The member name.
    pub name: String,
    /// The member type.
    pub ty: Type,
    /// Whether the member is optional (`foo?: T` in TypeScript). Always
    /// `false` for Go struct fields and interface methods.
    pub optional: bool,
    /// Whether the member is read-only (`readonly foo: T` in TypeScript).
    /// Always `false` for Go struct fields and interface methods.
    pub readonly: bool,
}

/// A single element inside a [`Type::Tuple`], optionally labelled.
///
/// TypeScript and C# both carry per-element labels at the source level:
///
/// - TypeScript: `[start: number, end: number]` — named tuple elements.
/// - C#: `(int start, int end)` — value tuple element names.
///
/// Both languages had previously dropped the label at the IR boundary because
/// `Type::Tuple` was `List<Type>`. TypeScript already preserved the labels
/// internally and discarded them only during lowering; this variant lets it
/// emit them faithfully.
///
/// **Shape choice:** An enum over `Positional(Type)` and `Named { label, ty }`
/// is preferred over `(Option<String>, Type)` because it is self-documenting,
/// makes the presence/absence of a label explicit at the type level rather than
/// as an `Option` that might be interpreted multiple ways, and gives pattern
/// matching clear arms. The tuple form would be slightly more compact but less
/// readable.
#[derive(Debug, Clone, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum TupleElement {
    /// An unlabelled element, as in Rust `(i32, String)`.
    Positional(Type),

    /// A labelled element, as in C# `(int start, int end)` or TypeScript
    /// `[start: number, end: number]`.
    Named {
        // The element label as it appears in source.
        label: String,
        // The element type.
        ty: Type,
    },
}

/// Use-site variance for a [`Type::Wildcard`] or declaration-site variance for
/// a [`crate::kinds::GenericParam`] type parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum Variance {
    /// Unbounded, or bound in neither direction (`?`); or invariant as in most
    /// Rust and Java type parameters.
    Invariant,
    /// Bounded above (`? extends T`, `out T`); or declaration-site covariance
    /// as in C# `out T` on a generic interface or delegate parameter.
    Covariant,
    /// Bounded below (`? super T`, `in T`); or declaration-site contravariance
    /// as in C# `in T` on a generic interface or delegate parameter.
    Contravariant,
}

impl Type {
    /// The source wrote no type here. See [`UnknownType::Unannotated`].
    pub const UNANNOTATED: Self = Type::Unknown(UnknownType::Unannotated);

    /// The source explicitly asked for dynamic typing. See
    /// [`UnknownType::DynamicallyTyped`].
    pub const DYNAMIC: Self = Type::Unknown(UnknownType::DynamicallyTyped);

    /// The producer's recursion guard fired. See
    /// [`UnknownType::TruncatedAtDepthLimit`].
    pub const TRUNCATED: Self = Type::Unknown(UnknownType::TruncatedAtDepthLimit);

    /// The language front-end handed the producer nothing. See
    /// [`UnknownType::OracleGap`].
    pub const ORACLE_GAP: Self = Type::Unknown(UnknownType::OracleGap);

    /// A bare name matching a fully-qualified id this package declares. See
    /// [`UnknownType::UnresolvedLocalName`].
    pub fn unresolved_local(name: impl Into<String>) -> Self {
        Type::Unknown(UnknownType::UnresolvedLocalName { name: name.into() })
    }

    /// A name resolving outside this package. See
    /// [`UnknownType::UnresolvedExternal`].
    pub fn unresolved_external(name: impl Into<String>) -> Self {
        Type::Unknown(UnknownType::UnresolvedExternal { name: name.into() })
    }

    /// A known construct this IR has no slot for. See
    /// [`UnknownType::NoIrRepresentation`].
    pub fn no_ir_representation(construct: impl Into<String>) -> Self {
        Type::Unknown(UnknownType::NoIrRepresentation {
            construct: construct.into(),
        })
    }

    /// The reason this type is unknown, or `None` if it is a known type.
    ///
    /// A consumer that only needs to *report* the gap should use this rather
    /// than matching, so that adding a reason does not break it. A consumer
    /// that must *act* on the gap should match exhaustively.
    pub fn unknown_reason(&self) -> Option<&UnknownType> {
        match self {
            Type::Unknown(r) => Some(r),
            _ => None,
        }
    }

    /// Visit every [`RawRef`] reachable from this type — the `Nominal` at
    /// every position, however deeply nested: generic arguments, tuple
    /// elements, union/intersection members, and the pointee of a
    /// [`Primitive::Reference`], [`Primitive::MutPointer`] or
    /// [`Primitive::ConstPointer`].
    ///
    /// # Why this exists
    ///
    /// Mirrors [`crate::entry::Entry::for_each_ref`], for the same reason:
    /// the `Visitor` machinery is crate-private, so a caller outside
    /// `nudox-ir` had no way to enumerate a type's references except by
    /// hand-writing its own match over `Type`. `nudox-store`'s
    /// `collect_stable_refs` did exactly that, and its match's leaf arm
    /// swallowed `Type::Primitive(_)` whole under a comment claiming nothing
    /// nominal could be reached through it — false for `Reference`,
    /// `MutPointer` and `ConstPointer`, which each nest a `Type`. Nothing
    /// caught it because a hand-written match's failure mode on a forgotten
    /// case is silence: it compiles clean and simply never produces the
    /// posting. `&Config`, `*const T` and `*mut T` were invisible to the
    /// reverse index as a result — "who takes a `&Config`" returned empty
    /// and read as a true negative rather than a gap.
    ///
    /// This method closes that hole structurally rather than by patching the
    /// three missing arms into the caller's match. `#[derive(Visitor)]` on
    /// `Type` and on `Primitive` generates its walk fresh from whatever
    /// fields whatever variants have *today*, visiting every field of every
    /// variant unconditionally — there is no `_ => {}` in it, and no case
    /// list to fall out of sync with the enum. A future variant that nests a
    /// `Type` anywhere in the tree is therefore walked automatically; a
    /// field type the derive cannot walk (e.g. a bare `HashMap<K, Type>`,
    /// which has no `Visitor` impl) fails the *build*, not a reverse-index
    /// query at runtime. Delegating here is what extends that guarantee to
    /// every external caller instead of just to `nudox-ir`'s own internals.
    ///
    /// This walks a clone (`Visitor` exposes only `visit_mut`), so it is for
    /// audits and index-building, not a hot path.
    pub fn for_each_ref(&self, mut f: impl FnMut(&RawRef)) {
        let sink = core::cell::RefCell::new(&mut f);
        let mut probe = self.clone();
        probe.visit_mut(&|r| (sink.borrow_mut())(r));
    }

    pub const U8: Self = Type::Primitive(Primitive::Integer {
        signed: false,
        width: Width::W8,
    });

    pub const U16: Self = Type::Primitive(Primitive::Integer {
        signed: false,
        width: Width::W16,
    });

    pub const U32: Self = Type::Primitive(Primitive::Integer {
        signed: false,
        width: Width::W32,
    });

    pub const U64: Self = Type::Primitive(Primitive::Integer {
        signed: false,
        width: Width::W64,
    });

    pub const U128: Self = Type::Primitive(Primitive::Integer {
        signed: false,
        width: Width::W128,
    });

    pub const I8: Self = Type::Primitive(Primitive::Integer {
        signed: true,
        width: Width::W8,
    });

    pub const I16: Self = Type::Primitive(Primitive::Integer {
        signed: true,
        width: Width::W16,
    });

    pub const I32: Self = Type::Primitive(Primitive::Integer {
        signed: true,
        width: Width::W32,
    });

    pub const I64: Self = Type::Primitive(Primitive::Integer {
        signed: true,
        width: Width::W64,
    });

    pub const I128: Self = Type::Primitive(Primitive::Integer {
        signed: true,
        width: Width::W128,
    });
}

/// A language-level primitive type, independent of any target architecture.
#[derive(Debug, Clone, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum Primitive {
    Integer {
        signed: bool,
        width: Width,
    },

    Float(Width),

    Bool,

    Char,

    /// Type of a string literal (if it exists)
    ///
    /// Note that this is specifically for primitive types, so this should be
    /// equivalent to `str` in Rust or `string` in C#, not Rust's `String` or
    /// C++'s `std::string`.
    // TODO: should rust string literals resolve to BorrowedRef then?
    // TODO: should C/C++ string literals resolve to `char*` or `char[]` instead?
    Str,

    /// A raw, mutable, unmanaged pointer.
    /// Ex: `*mut T`, `int*`.
    MutPointer(Box<Type>),

    /// A raw, const, unmanaged pointer.
    /// Ex: `*const T`, `int *const`.
    ConstPointer(Box<Type>),

    /// A managed reference with optional lifetime/mutability tracking.
    /// Ex: `&'a mut T`.
    ///
    /// `lifetime` carries the producer's spelling and may or may not include
    /// the `'` sigil; render it through
    /// [`lifetime_label`](crate::kinds::lifetime_label) rather than prepending
    /// one, or a producer that stores the source spelling renders `&''a T`.
    Reference {
        lifetime: Option<String>,
        mutable: bool,
        ty: Box<Type>,
    },

    /// An arbitrary primtive type, e.g. Date in JavaScript/TypeScript
    Builtin(String),
}

/// A language-level primitive type, independent of any target architecture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum Width {
    Fixed(NonZeroU16),

    /// Machine-dependent / pointer-sized (e.g., `usize`, `isize`).
    /// Generally not applicable to Floats
    Arch,
}

impl Width {
    /// 8-bit width
    pub const W8: Self = Width::Fixed(NonZero::new(8).unwrap());

    /// 16-bit width
    pub const W16: Self = Width::Fixed(NonZero::new(16).unwrap());

    /// 32-bit width
    pub const W32: Self = Width::Fixed(NonZero::new(32).unwrap());

    /// 64-bit width
    pub const W64: Self = Width::Fixed(NonZero::new(64).unwrap());

    /// 80-bit width
    pub const W80: Self = Width::Fixed(NonZero::new(80).unwrap());

    /// 128-bit width
    pub const W128: Self = Width::Fixed(NonZero::new(128).unwrap());
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{entry::AttrTok, skeleton::function_signature_skeleton};

    // Helper: get a skeleton bytes vec for a single type, by wrapping it as
    // the sole input parameter type of a function signature.  This exercises
    // the same `Skeleton::ty` path without calling the private method directly.
    fn ty_skeleton(t: &Type) -> Vec<u8> {
        function_signature_skeleton(&[Some(t.clone())], &[], &[], &[])
    }

    // ── Feature 1: Type::FunctionPointer ─────────────────────────────────────

    /// Construction and serde round-trip for FunctionPointer.
    #[test]
    fn function_pointer_roundtrip() {
        let fp = Type::FunctionPointer {
            params: [Type::I32, Type::Any].into(),
            ret: Some(Box::new(Type::Primitive(Primitive::Bool))),
            abi: Some("Cdecl".to_owned()),
        };
        let json = serde_json::to_string(&fp).expect("serialize");
        let back: Type = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(fp, back);
    }

    /// FunctionPointer without ABI and with None return round-trips.
    #[test]
    fn function_pointer_no_abi_no_ret_roundtrip() {
        let fp = Type::FunctionPointer {
            params: [].into(),
            ret: None,
            abi: None,
        };
        let json = serde_json::to_string(&fp).expect("serialize");
        let back: Type = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(fp, back);
    }

    /// Two FunctionPointers differing only in ABI must have distinct skeletons.
    #[test]
    fn function_pointer_abi_changes_skeleton() {
        let managed = Type::FunctionPointer {
            params: [Type::I32].into(),
            ret: None,
            abi: None,
        };
        let cdecl = Type::FunctionPointer {
            params: [Type::I32].into(),
            ret: None,
            abi: Some("Cdecl".to_owned()),
        };
        assert_ne!(
            ty_skeleton(&managed),
            ty_skeleton(&cdecl),
            "delegate*<> and delegate* unmanaged[Cdecl]<> must not have the same skeleton"
        );
    }

    /// Two FunctionPointers differing only in param type have distinct skeletons.
    #[test]
    fn function_pointer_param_changes_skeleton() {
        let f_i32 = Type::FunctionPointer {
            params: [Type::I32].into(),
            ret: None,
            abi: None,
        };
        let f_i64 = Type::FunctionPointer {
            params: [Type::I64].into(),
            ret: None,
            abi: None,
        };
        assert_ne!(ty_skeleton(&f_i32), ty_skeleton(&f_i64));
    }

    // ── Feature 3: Variance on GenericParam::Type ─────────────────────────────

    // (Variance itself lives in generics.rs tests; the Type::Wildcard path is
    // already exercised. Here we just confirm the field exists and round-trips.)

    // ── Feature 4: Type::Annotated ───────────────────────────────────────────

    /// Construction and serde round-trip for Type::Annotated.
    #[test]
    fn annotated_roundtrip() {
        let ann = Type::Annotated {
            inner: Box::new(Type::Primitive(Primitive::Str)),
            annotation: AttrTok {
                token: "NonNull".to_owned(),
                arg: None,
            },
        };
        let json = serde_json::to_string(&ann).expect("serialize");
        let back: Type = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(ann, back);
    }

    /// Annotated with an arg round-trips.
    #[test]
    fn annotated_with_arg_roundtrip() {
        let ann = Type::Annotated {
            inner: Box::new(Type::Any),
            annotation: AttrTok {
                token: "Nullable".to_owned(),
                arg: Some("ALWAYS".to_owned()),
            },
        };
        let json = serde_json::to_string(&ann).expect("serialize");
        let back: Type = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(ann, back);
    }

    /// @NonNull String and String (unannotated) must have different skeletons.
    #[test]
    fn annotated_vs_unannotated_differ_in_skeleton() {
        let raw = Type::Primitive(Primitive::Str);
        let annotated = Type::Annotated {
            inner: Box::new(Type::Primitive(Primitive::Str)),
            annotation: AttrTok {
                token: "NonNull".to_owned(),
                arg: None,
            },
        };
        assert_ne!(
            ty_skeleton(&raw),
            ty_skeleton(&annotated),
            "@NonNull String and String must have different identity skeletons"
        );
    }

    /// Two annotations differing only in token name must have different skeletons.
    #[test]
    fn different_annotation_tokens_differ_in_skeleton() {
        let non_null = Type::Annotated {
            inner: Box::new(Type::Any),
            annotation: AttrTok {
                token: "NonNull".to_owned(),
                arg: None,
            },
        };
        let nullable = Type::Annotated {
            inner: Box::new(Type::Any),
            annotation: AttrTok {
                token: "Nullable".to_owned(),
                arg: None,
            },
        };
        assert_ne!(ty_skeleton(&non_null), ty_skeleton(&nullable));
    }

    // ── Feature 5: TupleElement / Type::Tuple ────────────────────────────────

    /// A positional tuple round-trips.
    #[test]
    fn tuple_positional_roundtrip() {
        let t = Type::Tuple(
            [
                TupleElement::Positional(Type::I32),
                TupleElement::Positional(Type::Any),
            ]
            .into(),
        );
        let json = serde_json::to_string(&t).expect("serialize");
        let back: Type = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(t, back);
    }

    /// A named-element tuple round-trips.
    #[test]
    fn tuple_named_roundtrip() {
        let t = Type::Tuple(
            [
                TupleElement::Named {
                    label: "start".to_owned(),
                    ty: Type::I32,
                },
                TupleElement::Named {
                    label: "end".to_owned(),
                    ty: Type::I32,
                },
            ]
            .into(),
        );
        let json = serde_json::to_string(&t).expect("serialize");
        let back: Type = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(t, back);
    }

    /// A tuple with labels and one without must have DIFFERENT skeletons.
    ///
    /// `(int start, int end)` and `(int, int)` are distinct C# types — the
    /// labelled form supports `.start` / `.end` member access.
    #[test]
    fn named_vs_positional_tuple_differ_in_skeleton() {
        let positional = Type::Tuple(
            [
                TupleElement::Positional(Type::I32),
                TupleElement::Positional(Type::I32),
            ]
            .into(),
        );
        let named = Type::Tuple(
            [
                TupleElement::Named {
                    label: "start".to_owned(),
                    ty: Type::I32,
                },
                TupleElement::Named {
                    label: "end".to_owned(),
                    ty: Type::I32,
                },
            ]
            .into(),
        );
        assert_ne!(
            ty_skeleton(&positional),
            ty_skeleton(&named),
            "(int, int) and (int start, int end) must have different identity skeletons"
        );
    }

    /// Two tuples with different labels must have different skeletons.
    #[test]
    fn different_labels_differ_in_skeleton() {
        let ab = Type::Tuple(
            [
                TupleElement::Named {
                    label: "a".to_owned(),
                    ty: Type::I32,
                },
                TupleElement::Named {
                    label: "b".to_owned(),
                    ty: Type::I32,
                },
            ]
            .into(),
        );
        let xy = Type::Tuple(
            [
                TupleElement::Named {
                    label: "x".to_owned(),
                    ty: Type::I32,
                },
                TupleElement::Named {
                    label: "y".to_owned(),
                    ty: Type::I32,
                },
            ]
            .into(),
        );
        assert_ne!(ty_skeleton(&ab), ty_skeleton(&xy));
    }

    // ── Type::Conditional ─────────────────────────────────────────────────────

    #[test]
    fn conditional_roundtrip() {
        let c = Type::Conditional {
            check: Box::new(Type::TypeVar("T".to_owned())),
            extends_ty: Box::new(Type::Primitive(Primitive::Str)),
            then_ty: Box::new(Type::Primitive(Primitive::Bool)),
            else_ty: Box::new(Type::Never),
        };
        let json = serde_json::to_string(&c).expect("serialize");
        let back: Type = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(c, back);
    }

    /// Two Conditionals differing only in then_ty must have distinct skeletons.
    #[test]
    fn conditional_then_changes_skeleton() {
        let c1 = Type::Conditional {
            check: Box::new(Type::TypeVar("T".to_owned())),
            extends_ty: Box::new(Type::Primitive(Primitive::Str)),
            then_ty: Box::new(Type::Primitive(Primitive::Bool)),
            else_ty: Box::new(Type::Never),
        };
        let c2 = Type::Conditional {
            check: Box::new(Type::TypeVar("T".to_owned())),
            extends_ty: Box::new(Type::Primitive(Primitive::Str)),
            then_ty: Box::new(Type::I32),
            else_ty: Box::new(Type::Never),
        };
        assert_ne!(ty_skeleton(&c1), ty_skeleton(&c2));
    }

    // ── Type::Mapped ──────────────────────────────────────────────────────────

    #[test]
    fn mapped_roundtrip() {
        let m = Type::Mapped {
            key_var: "P".to_owned(),
            source: Box::new(Type::TypeVar("T".to_owned())),
            value: Box::new(Type::Any),
            readonly: MappedModifier::Add,
            optional: MappedModifier::Remove,
        };
        let json = serde_json::to_string(&m).expect("serialize");
        let back: Type = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(m, back);
    }

    /// `{ readonly [P in K]: V }` and `{ [P in K]: V }` must have distinct
    /// skeletons — the readonly modifier is identity-relevant.
    #[test]
    fn mapped_readonly_changes_skeleton() {
        let with_readonly = Type::Mapped {
            key_var: "P".to_owned(),
            source: Box::new(Type::Any),
            value: Box::new(Type::Any),
            readonly: MappedModifier::Add,
            optional: MappedModifier::Absent,
        };
        let without_readonly = Type::Mapped {
            key_var: "P".to_owned(),
            source: Box::new(Type::Any),
            value: Box::new(Type::Any),
            readonly: MappedModifier::Absent,
            optional: MappedModifier::Absent,
        };
        assert_ne!(ty_skeleton(&with_readonly), ty_skeleton(&without_readonly));
    }

    /// The key_var name is alpha-equivalent — two Mapped types differing only
    /// in key_var name must have the SAME skeleton.
    #[test]
    fn mapped_key_var_is_alpha_equivalent_in_skeleton() {
        let with_p = Type::Mapped {
            key_var: "P".to_owned(),
            source: Box::new(Type::Any),
            value: Box::new(Type::Any),
            readonly: MappedModifier::Absent,
            optional: MappedModifier::Absent,
        };
        let with_k = Type::Mapped {
            key_var: "K".to_owned(),
            source: Box::new(Type::Any),
            value: Box::new(Type::Any),
            readonly: MappedModifier::Absent,
            optional: MappedModifier::Absent,
        };
        assert_eq!(
            ty_skeleton(&with_p),
            ty_skeleton(&with_k),
            "key_var name is alpha-equivalent — must not change the skeleton"
        );
    }

    // ── Type::TemplateLiteral ─────────────────────────────────────────────────

    #[test]
    fn template_literal_roundtrip() {
        let tl = Type::TemplateLiteral(
            [
                TemplatePart::Literal("error-".to_owned()),
                TemplatePart::Interpolated(Box::new(Type::TypeVar("Code".to_owned()))),
            ]
            .into(),
        );
        let json = serde_json::to_string(&tl).expect("serialize");
        let back: Type = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(tl, back);
    }

    /// `error-${T}` and `warning-${T}` must have distinct skeletons.
    #[test]
    fn template_literal_text_changes_skeleton() {
        let error_t = Type::TemplateLiteral(
            [
                TemplatePart::Literal("error-".to_owned()),
                TemplatePart::Interpolated(Box::new(Type::TypeVar("T".to_owned()))),
            ]
            .into(),
        );
        let warning_t = Type::TemplateLiteral(
            [
                TemplatePart::Literal("warning-".to_owned()),
                TemplatePart::Interpolated(Box::new(Type::TypeVar("T".to_owned()))),
            ]
            .into(),
        );
        assert_ne!(ty_skeleton(&error_t), ty_skeleton(&warning_t));
    }

    // ── Type::AnonymousRecord ─────────────────────────────────────────────────

    #[test]
    fn anonymous_record_struct_roundtrip() {
        let ar = Type::AnonymousRecord {
            form: AnonRecordForm::Struct,
            members: [
                AnonField {
                    name: "x".to_owned(),
                    ty: Type::I32,
                    optional: false,
                    readonly: false,
                },
                AnonField {
                    name: "y".to_owned(),
                    ty: Type::I32,
                    optional: true,
                    readonly: true,
                },
            ]
            .into(),
        };
        let json = serde_json::to_string(&ar).expect("serialize");
        let back: Type = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(ar, back);
    }

    #[test]
    fn anonymous_record_interface_roundtrip() {
        let ar = Type::AnonymousRecord {
            form: AnonRecordForm::Interface,
            members: [AnonField {
                name: "Foo".to_owned(),
                ty: Type::FunctionPointer {
                    params: [].into(),
                    ret: None,
                    abi: None,
                },
                optional: false,
                readonly: false,
            }]
            .into(),
        };
        let json = serde_json::to_string(&ar).expect("serialize");
        let back: Type = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(ar, back);
    }

    /// `{ x: number }` and `{ x?: number }` must have different skeletons.
    #[test]
    fn anonymous_record_optional_changes_skeleton() {
        let required = Type::AnonymousRecord {
            form: AnonRecordForm::Struct,
            members: [AnonField {
                name: "x".to_owned(),
                ty: Type::I32,
                optional: false,
                readonly: false,
            }]
            .into(),
        };
        let optional = Type::AnonymousRecord {
            form: AnonRecordForm::Struct,
            members: [AnonField {
                name: "x".to_owned(),
                ty: Type::I32,
                optional: true,
                readonly: false,
            }]
            .into(),
        };
        assert_ne!(ty_skeleton(&required), ty_skeleton(&optional));
    }

    /// Struct form and interface form must have different skeletons.
    #[test]
    fn anonymous_record_form_changes_skeleton() {
        let s = Type::AnonymousRecord {
            form: AnonRecordForm::Struct,
            members: [].into(),
        };
        let i = Type::AnonymousRecord {
            form: AnonRecordForm::Interface,
            members: [].into(),
        };
        assert_ne!(ty_skeleton(&s), ty_skeleton(&i));
    }

    // ── Type::ImplTrait + Type::DynTrait ──────────────────────────────────────

    #[test]
    fn impl_trait_roundtrip() {
        let it = Type::ImplTrait([Type::Any, Type::SelfType].into());
        let json = serde_json::to_string(&it).expect("serialize");
        let back: Type = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(it, back);
    }

    #[test]
    fn dyn_trait_roundtrip() {
        let dt = Type::DynTrait([Type::Any].into());
        let json = serde_json::to_string(&dt).expect("serialize");
        let back: Type = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(dt, back);
    }

    /// `impl Trait` and `dyn Trait` with the same bounds must have DIFFERENT
    /// skeletons — they are distinct types with distinct dispatch semantics.
    #[test]
    fn impl_trait_vs_dyn_trait_differ_in_skeleton() {
        let impl_t = Type::ImplTrait([Type::Any].into());
        let dyn_t = Type::DynTrait([Type::Any].into());
        assert_ne!(ty_skeleton(&impl_t), ty_skeleton(&dyn_t));
    }

    #[test]
    fn impl_trait_bounds_change_skeleton() {
        let impl_a = Type::ImplTrait([Type::Any].into());
        let impl_b = Type::ImplTrait([Type::Never].into());
        assert_ne!(ty_skeleton(&impl_a), ty_skeleton(&impl_b));
    }

    // ── Type::Inferred ────────────────────────────────────────────────────────

    #[test]
    fn inferred_roundtrip() {
        let json = serde_json::to_string(&Type::Inferred).expect("serialize");
        let back: Type = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(Type::Inferred, back);
    }

    /// Inferred and Any must have distinct skeletons.
    #[test]
    fn inferred_vs_any_differ_in_skeleton() {
        assert_ne!(ty_skeleton(&Type::Inferred), ty_skeleton(&Type::Any));
    }

    // ── Type::Unknown / UnknownType (CC-2) ────────────────────────────────────

    /// Every reason, listed once. Adding a variant to `UnknownType` without
    /// adding it here fails to compile — the destructuring match below is
    /// exhaustive on purpose.
    fn every_reason() -> Vec<UnknownType> {
        let all = vec![
            UnknownType::Unannotated,
            UnknownType::DynamicallyTyped,
            UnknownType::UnresolvedLocalName {
                name: "Context".to_owned(),
            },
            UnknownType::UnresolvedExternal {
                name: "click.core.Context".to_owned(),
            },
            UnknownType::TruncatedAtDepthLimit,
            UnknownType::OracleGap,
            UnknownType::NoIrRepresentation {
                construct: "complex128".to_owned(),
            },
        ];
        // Compile-time completeness check: this match has no `_` arm, so a new
        // `UnknownType` variant breaks the build here and forces whoever adds
        // it to extend `all` above.
        for r in &all {
            match r {
                UnknownType::Unannotated
                | UnknownType::DynamicallyTyped
                | UnknownType::UnresolvedLocalName { .. }
                | UnknownType::UnresolvedExternal { .. }
                | UnknownType::TruncatedAtDepthLimit
                | UnknownType::OracleGap
                | UnknownType::NoIrRepresentation { .. } => {}
            }
        }
        all
    }

    /// Every reason round-trips through serde.
    #[test]
    fn every_unknown_reason_roundtrips() {
        for r in every_reason() {
            let ty = Type::Unknown(r.clone());
            let json = serde_json::to_string(&ty).expect("serialize");
            let back: Type = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(ty, back, "round-trip failed for {r:?}");
        }
    }

    /// **The point of CC-2.** No two reasons may share a skeleton — if any pair
    /// collides, a consumer cannot tell them apart and `Type::Any` has simply
    /// been renamed.
    #[test]
    fn every_unknown_reason_has_a_distinct_skeleton() {
        let reasons = every_reason();
        for (i, a) in reasons.iter().enumerate() {
            for b in reasons.iter().skip(i + 1) {
                assert_ne!(
                    ty_skeleton(&Type::Unknown(a.clone())),
                    ty_skeleton(&Type::Unknown(b.clone())),
                    "{a:?} and {b:?} must not share a skeleton"
                );
            }
        }
    }

    /// The three "I have no type" spellings are three different facts.
    ///
    /// `Any` is a top type, `Inferred` is a written inference request, and
    /// `Unknown(_)` is a gap. Before CC-2 the first and third were one variant.
    #[test]
    fn any_inferred_and_unknown_are_three_distinct_types() {
        let any = ty_skeleton(&Type::Any);
        let inferred = ty_skeleton(&Type::Inferred);
        let unannotated = ty_skeleton(&Type::UNANNOTATED);
        let dynamic = ty_skeleton(&Type::DYNAMIC);
        assert_ne!(any, inferred);
        assert_ne!(any, unannotated, "`object` is not `no annotation`");
        assert_ne!(any, dynamic, "a top type is not the dynamic escape hatch");
        assert_ne!(inferred, unannotated, "`_` is a request, not an absence");
        assert_ne!(unannotated, dynamic, "`def f(x)` is not `def f(x: Any)`");
    }

    /// The two name-carrying reasons discriminate on the name.
    ///
    /// This is what stops `f(Context)` and `f(Command)` — both unresolved —
    /// from producing one signature skeleton and therefore one `IntroId`.
    #[test]
    fn unresolved_names_discriminate_in_the_skeleton() {
        let ctx = Type::unresolved_external("click.core.Context");
        let cmd = Type::unresolved_external("click.core.Command");
        assert_ne!(
            ty_skeleton(&ctx),
            ty_skeleton(&cmd),
            "two distinct external types must not collide"
        );

        let local_ctx = Type::unresolved_local("Context");
        assert_ne!(
            ty_skeleton(&local_ctx),
            ty_skeleton(&Type::unresolved_local("Command")),
        );
        // Same spelling, different *reason* — still distinct, because which
        // pass can close the gap is itself a fact.
        assert_ne!(
            ty_skeleton(&local_ctx),
            ty_skeleton(&Type::unresolved_external("Context")),
            "a locally-declared short name is not a cross-package reference"
        );
    }

    /// `NoIrRepresentation` keeps `complex128` and `unsafe.Pointer` apart.
    #[test]
    fn no_ir_representation_discriminates_on_construct() {
        assert_ne!(
            ty_skeleton(&Type::no_ir_representation("complex128")),
            ty_skeleton(&Type::no_ir_representation("unsafe.Pointer")),
        );
    }

    /// The convenience constants build the variants they claim to.
    #[test]
    fn constructors_build_the_documented_variants() {
        assert_eq!(Type::UNANNOTATED, Type::Unknown(UnknownType::Unannotated));
        assert_eq!(Type::DYNAMIC, Type::Unknown(UnknownType::DynamicallyTyped));
        assert_eq!(
            Type::TRUNCATED,
            Type::Unknown(UnknownType::TruncatedAtDepthLimit)
        );
        assert_eq!(Type::ORACLE_GAP, Type::Unknown(UnknownType::OracleGap));
        assert_eq!(
            Type::unresolved_local("Ctx"),
            Type::Unknown(UnknownType::UnresolvedLocalName {
                name: "Ctx".to_owned()
            })
        );
    }

    /// `unknown_reason` answers for unknowns and only for unknowns.
    #[test]
    fn unknown_reason_is_none_for_known_types() {
        assert_eq!(Type::I32.unknown_reason(), None);
        assert_eq!(Type::Any.unknown_reason(), None, "a top type is known");
        assert_eq!(Type::Inferred.unknown_reason(), None, "`_` is a request");
        assert_eq!(
            Type::DYNAMIC.unknown_reason(),
            Some(&UnknownType::DynamicallyTyped)
        );
    }

    /// Tags are unique — a metrics sink keyed on `tag()` must not merge rows.
    #[test]
    fn reason_tags_are_unique_and_spellings_are_carried() {
        let mut tags: Vec<&str> = every_reason().iter().map(|r| r.tag()).collect();
        let count = tags.len();
        tags.sort_unstable();
        tags.dedup();
        assert_eq!(tags.len(), count, "two reasons share a tag");

        assert_eq!(
            UnknownType::UnresolvedLocalName {
                name: "Context".to_owned()
            }
            .spelling(),
            Some("Context")
        );
        assert_eq!(UnknownType::Unannotated.spelling(), None);
    }

    // ── Type::QualifiedPath ───────────────────────────────────────────────────

    #[test]
    fn qualified_path_roundtrip() {
        let qp = Type::QualifiedPath {
            self_ty: Box::new(Type::TypeVar("T".to_owned())),
            trait_ref: Some(Box::new(Type::Any)),
            assoc: "Item".to_owned(),
        };
        let json = serde_json::to_string(&qp).expect("serialize");
        let back: Type = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(qp, back);
    }

    #[test]
    fn qualified_path_no_trait_ref_roundtrip() {
        let qp = Type::QualifiedPath {
            self_ty: Box::new(Type::Any),
            trait_ref: None,
            assoc: "Inner".to_owned(),
        };
        let json = serde_json::to_string(&qp).expect("serialize");
        let back: Type = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(qp, back);
    }

    /// `<T as Iterator>::Item` and `<T as Iterator>::IntoIter` must have
    /// distinct skeletons.
    #[test]
    fn qualified_path_assoc_changes_skeleton() {
        let item = Type::QualifiedPath {
            self_ty: Box::new(Type::TypeVar("T".to_owned())),
            trait_ref: Some(Box::new(Type::Any)),
            assoc: "Item".to_owned(),
        };
        let into_iter = Type::QualifiedPath {
            self_ty: Box::new(Type::TypeVar("T".to_owned())),
            trait_ref: Some(Box::new(Type::Any)),
            assoc: "IntoIter".to_owned(),
        };
        assert_ne!(ty_skeleton(&item), ty_skeleton(&into_iter));
    }

    /// A QualifiedPath with a trait_ref and one without must have distinct
    /// skeletons: `<T as Foo>::Bar` and `T.Bar` are structurally different.
    #[test]
    fn qualified_path_trait_ref_presence_changes_skeleton() {
        let with_trait = Type::QualifiedPath {
            self_ty: Box::new(Type::Any),
            trait_ref: Some(Box::new(Type::Any)),
            assoc: "Bar".to_owned(),
        };
        let without_trait = Type::QualifiedPath {
            self_ty: Box::new(Type::Any),
            trait_ref: None,
            assoc: "Bar".to_owned(),
        };
        assert_ne!(ty_skeleton(&with_trait), ty_skeleton(&without_trait));
    }
}
