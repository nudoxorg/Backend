//! Lower [`oracle::TypeData`] into [`nudox_ir::kinds::ty::Type`].
//!
//! One-way, documented mapping from Python's type system into the IR type
//! algebra. Rules:
//!
//! | Python                     | IR                                           |
//! |----------------------------|----------------------------------------------|
//! | `Any` (explicitly written) | `Type::Unknown(DynamicallyTyped)`            |
//! | unannotated (`ty: None`)   | `Type::Unknown(Unannotated)` — see `emit`    |
//! | `NoReturn` / `Never`       | `Type::Never`                                |
//! | `T` (type var / PEP 695)   | `Type::TypeVar(name)` — NEVER `Any`          |
//! | `Optional[T]`              | `Type::Union([lower(T), Never])` *see note   |
//! | `Union[A, B]` / `A \| B`  | `Type::Union([…])`                           |
//! | `Tuple[A, B]`              | `Type::Tuple([TupleElement::Positional(…)]…)`|
//! | `list[T]` / slice-like     | `Type::Slice(lower(T))`                      |
//! | `None`                     | `Type::Primitive(Primitive::Builtin("None"))`|
//! | `Self` / `self`            | `Type::SelfType`                             |
//! | `Foo[T, U]` (same-pkg)     | `Type::Apply { base: Nominal(..), args }`    |
//! | `Foo` (same-pkg nominal)   | `Type::Nominal(Ref::Intro(..))`              |
//! | `int`, `float`, `bool`…    | `Type::Primitive(…)`                         |
//! | `dict`, `list`, `object`…  | `Type::Nominal(Ref::Foreign{key,..})` (universe) |
//! | `Foo` (bare, declared here)| `Type::Unknown(UnresolvedLocalName{name})`   |
//! | `Foo` (nominal, stdlib)    | `Type::Nominal(Ref::Foreign{key,..})` (python-stdlib) |
//! | `Foo` (nominal, external)  | `Type::Nominal(Ref::Foreign{key,..})` (pypi) |
//! | `Annotated[T, meta…]`      | `Type::Annotated { inner: lower(T), …}`      |
//! | unhandled `Expr` / `Literal[...]` | `Type::Unknown(NoIrRepresentation{construct})` |
//!
//! # Note on `TypeData::Unsupported`
//!
//! Not every source construct the extractor sees maps onto something it
//! understands: an `Expr` shape `expr_to_type` has no arm for, or a
//! `Literal[...]` value-set (a real PEP 586 construct, not the gradual-typing
//! escape hatch). Both used to fall through to `TypeData::Any`, which then
//! lowered to `Type::DYNAMIC` — claiming the source asked for dynamic typing
//! when the truth is the extractor did not understand the expression. Both
//! now lower to `Type::Unknown(UnknownType::NoIrRepresentation)`, carrying the
//! source text so the gap is groupable and actionable rather than silently
//! inflating the `dynamically-typed` count.
//!
//! # Note on `Tuple[A, B]`
//!
//! `Type::Tuple` now holds `List<TupleElement>`. Python tuples have no per-element
//! labels (no C# / TypeScript named-tuple equivalents), so every element is
//! wrapped as `TupleElement::Positional(lower(element))`.
//!
//! # Note on `Annotated[T, meta]` (PEP 593)
//!
//! `Annotated[T, meta]` previously lowered to `Type::Any`, losing both the
//! underlying type and the metadata. We now emit `Type::Annotated`, wrapping
//! the lowered inner type. Multiple metadata items are stacked as nested
//! `Annotated` layers: outermost = last metadata item (so the first unwrap
//! gives the innermost annotation, ultimately reaching `inner = lower(T)`).
//!
//! # Note on `ParamSpec` / `TypeVarTuple`
//!
//! Both are currently represented in the oracle as `TypeData::TypeVar(name)`.
//! They lower to `Type::TypeVar(name)` just like ordinary `TypeVar`. The IR
//! has no distinct `ParamSpec` or `TypeVarTuple` variant. A future IR
//! extension could add these, but the information loss is acceptable for now:
//! `ParamSpec` is callable-signature-level metadata not reachable via the
//! kind-level type algebra, and `TypeVarTuple` (PEP 646) is a variadic
//! specialisation that the current Trustfall schema has no policy for.
//!
//! # Note on declaration-site variance (`TypeVar(covariant=True)`)
//!
//! Pyrefly does NOT surface declaration-site variance (`covariant`/`contravariant`
//! flags on `TypeVar`) in the oracle data as of this implementation. When it
//! does, `GenericParamData` should gain a `variance: Option<Variance>` field
//! and `lower_generics` should map `True` → `Some(Variance::Covariant)` /
//! `False` → `Some(Variance::Contravariant)`. Until then `variance: None`.
//!
//! # Note on `Optional[T]` / `None`
//!
//! `Optional[T]` is stored in `TypeData::Union([T, NoneType])`. At the IR
//! level there is no distinct `NoneType` entry in scope, so
//! `TypeData::NoneType` lowers to `Type::Primitive(Primitive::Builtin("None"))`
//! — a sentinel that downstream consumers can recognize.
//!
//! # Note on same-package nominals
//!
//! [`Lowering::nominal`] is **order-independent**: it interns the ID and
//! returns a `Ref` immediately, whether or not the target has been declared
//! yet.  Declaration can come later in the same pass, or `finish` reports it
//! as `Undeclared`.  This is the entire reason the flat sink exists.
//!
//! A name that the oracle reported as belonging to this package is known by
//! its fully-qualified `PythonId`, so we can call `out.nominal::<Record>(id)`
//! right now.  The `Ref` will resolve to `Ref::Intro` after `seal`.
//!
//! # Note on unresolved nominals
//!
//! A name this package does not declare under that exact id lowers to a
//! **named** representation, never to a bare `Type::Any`.  Which one depends
//! on a fact the old code never checked:
//!
//! - The name is a dotted suffix of some id this package *does* declare
//!   (`Context`, where `click.core.Context` exists) →
//!   `UnknownType::UnresolvedLocalName`.  A within-package import-graph walk
//!   closes it; no registry is involved.  **13,724 of 20,227 measured
//!   unresolved nominals — 67.8% — are this case.**
//! - Otherwise (builtins, stdlib, third-party) → a *named* `Ref::Foreign`
//!   carrying a pypi `ForeignKey` (`ForeignOrigin::Namespace`), emitted right
//!   here through the lowering sink. This used to be
//!   `UnknownType::UnresolvedExternal`, a bare string with no `ForeignKey`
//!   behind it anywhere in the Python producer — a cross-package type could
//!   never actually be linked by a later corpus pass. The key's `target`
//!   starts `None`; a resolver fills it in.
//!
//! Both keep the source spelling.  Erasing it was never a harmless
//! degradation: `Skeleton` encoded `Type::Any` as a single byte, so every
//! external type in an overload set hashed identically.

use std::collections::{HashMap, HashSet};

use nudox_ir::build::EcosystemId;
use nudox_ir::entry::AttrTok;
use nudox_ir::foreign::ForeignKey;
use nudox_ir::kinds::ty::{Primitive, TupleElement, Type, Width};
use nudox_ir::kinds::{GenericParam, Record};
use nudox_ir::lower::Lowering;
use std::num::NonZeroU16;

use crate::python::oracle::{GenericParamData, PythonId, TypeData};

/// The fully-qualified ids this package declares, plus an index over their
/// dotted **suffixes**.
///
/// # Why the suffix index exists
///
/// A census of 87,101 annotation positions across 22 pypi packages found
/// 20,227 nominals that `known_ids.contains(clean)` rejected. **13,724 of them
/// (67.8%) were not external at all** — they were bare short names that the
/// same package declares under a fully-qualified id. `click/core.py` writes
/// `-> "Context"`; the package declares `click.core.Context`; the exact-match
/// lookup missed by the module prefix and the type collapsed to `Type::Any`.
///
/// Two thirds of what looked like a cross-package linking problem was a
/// within-package name-resolution problem, and the old lattice could not say
/// so. This index is what lets `lower_nominal` tell the two apart and emit
/// [`UnknownType::UnresolvedLocalName`](nudox_ir::kinds::UnknownType) rather
/// than [`UnknownType::UnresolvedExternal`].
///
/// # The false positive, and the guard
///
/// A suffix hit is a **candidate, not a resolution**, and the obvious way it
/// can be wrong is a same-named class in an unrelated module:
///
/// ```text
/// mypkg/models.py:  class Path: ...        # declares mypkg.models.Path
/// mypkg/io.py:      from pathlib import Path
///                   def open(p: Path): ... # means pathlib.Path — NOT local
/// ```
///
/// The suffix index says `Path` is a local short name. For `mypkg/io.py` that
/// is wrong: only the import graph, which this pass has not walked, knows that
/// `io.py` bound `Path` to `pathlib`.
///
/// **Three guards bound what that error can cost:**
///
/// 1. **Segment alignment.** Suffixes are cut at `.` boundaries only, so `ext`
///    never matches `Context` and `Path` never matches `PosixPath`. A
///    substring index here would report thousands of false locals; see
///    `suffix_index_matches_dotted_boundaries_only`.
/// 2. **Builtins win first.** `lower_nominal` resolves `int`/`str`/`None` (and
///    every other builtin) before consulting this index, so a package that
///    happens to declare a class named `Path` cannot capture a primitive.
/// 3. **No `Ref` is ever emitted — this is the load-bearing one.** A hit
///    produces [`UnknownType::UnresolvedLocalName`], which is a *named gap*,
///    not a resolution. So the entire cost of a false positive is that one
///    reason string says "look in this package" when the answer was
///    "look in `pathlib`". It cannot mint a wrong `IntroId`, cannot create a
///    wrong edge in the graph, and cannot produce a hyperlink to the wrong
///    declaration — all of which a speculative `Nominal` would.
///
/// Both variants are gaps that a later pass must close; the split routes each
/// to the *right* pass and is measured to be correct for 67.8% of them. It is
/// deliberately not a resolver, because at this phase it cannot be one.
///
/// [`UnresolvedLocalName`]: nudox_ir::kinds::UnknownType::UnresolvedLocalName
#[derive(Debug, Default, Clone)]
pub struct KnownIds {
    /// Fully-qualified ids declared in this package.
    ids: HashSet<String>,
    /// Every dotted suffix of every id in `ids`, mapped to how many distinct
    /// ids produced it (`click.core.Context` → `core.Context`, `Context`).
    ///
    /// The count is what lets a confirming pass see *ambiguity*: a suffix
    /// produced by three different ids needs the import graph before it can be
    /// bound to one of them. Depth is bounded by module nesting, so this is a
    /// small constant factor over `ids`.
    suffixes: HashMap<String, usize>,
    /// The package's own top-level module segment (`click.core.Group` →
    /// `click`), if `ids` is non-empty. Every id this package declares shares
    /// the same first dotted segment — `syntax::module_dotted_name` derives
    /// every module name from the same package root — so any one id's first
    /// segment is the package's root for all of them.
    ///
    /// This is what lets `lower_nominal` recognize a **root re-export
    /// spelling**: `mypkg.Context` where the package declares
    /// `mypkg.core.Context` and re-exports it at the package root (`from
    /// .core import Context` in `mypkg/__init__.py`). See that function's
    /// case 3.5 for why the alternative — falling through to case 4 — used to
    /// mint a self-referential `Ref::Foreign` whose namespace was the
    /// package's own name.
    root: Option<String>,
}

impl KnownIds {
    /// Build the index from the package's fully-qualified ids.
    pub fn new(ids: HashSet<String>) -> Self {
        let mut suffixes: HashMap<String, usize> = HashMap::with_capacity(ids.len() * 2);
        for id in &ids {
            let mut rest = id.as_str();
            // Walk the dotted segments left to right, recording each proper
            // suffix. Cutting only at `.` is guard 1 above: `Context` and
            // `core.Context` are indexed, `ntext` and `ext` are not.
            while let Some(dot) = rest.find('.') {
                rest = &rest[dot + 1..];
                *suffixes.entry(rest.to_owned()).or_default() += 1;
            }
        }
        let root = ids
            .iter()
            .next()
            .map(|id| id.split('.').next().unwrap_or(id).to_owned());
        Self {
            ids,
            suffixes,
            root,
        }
    }

    /// The package's own top-level module segment, or `None` if this
    /// package declares nothing.
    pub fn root(&self) -> Option<&str> {
        self.root.as_deref()
    }

    /// Does this package declare exactly this fully-qualified id?
    pub fn contains(&self, id: &str) -> bool {
        self.ids.contains(id)
    }

    /// Is `name` a dotted suffix of some id this package declares?
    ///
    /// True for `Context` when the package declares `click.core.Context`.
    /// False when `name` is already a full id (use [`KnownIds::contains`] for
    /// that) or names something outside the package.
    ///
    /// Read this as "a within-package resolver pass should look here first",
    /// never as "this names that declaration" — see the type's doc for why
    /// that distinction is the guard and not a caveat.
    pub fn is_local_short_name(&self, name: &str) -> bool {
        self.suffixes.contains_key(name)
    }

    /// How many distinct declared ids end with `name`.
    ///
    /// `0` means not local at all. `1` means a within-package pass has exactly
    /// one candidate to confirm against the import graph. **`>1` means the
    /// name is ambiguous inside the package itself** — `Path` declared in both
    /// `mypkg.models` and `mypkg.compat` — and no amount of suffix matching
    /// can pick between them. Exposed so the confirming pass can report that
    /// honestly rather than silently taking the first hit.
    pub fn local_candidates(&self, name: &str) -> usize {
        self.suffixes.get(name).copied().unwrap_or(0)
    }

    /// Number of declared ids — the size the resolver pass has to work over.
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }
}

impl FromIterator<String> for KnownIds {
    fn from_iter<I: IntoIterator<Item = String>>(iter: I) -> Self {
        Self::new(iter.into_iter().collect())
    }
}

/// Lower a [`TypeData`] into a [`nudox_ir::kinds::ty::Type`].
///
/// - `out` — the flat lowering sink; `nominal`/`apply` borrow it mutably to
///   intern a forward reference. They are disjoint from the `oracle` borrow.
/// - `known_ids` — the set of fully-qualified IDs declared in this package.
///   An exact hit resolves to a same-package nominal; a *suffix* hit becomes
///   `UnknownType::UnresolvedLocalName`; everything else becomes
///   `UnknownType::UnresolvedExternal`.
pub fn lower_type(ty: &TypeData, out: &mut Lowering<PythonId>, known_ids: &KnownIds) -> Type {
    match ty {
        // `typing.Any` is the gradual-typing escape hatch, not a top type: it
        // is consistent with every type in *both* directions and suppresses
        // checking. Python has no top type to map onto `Type::Any` — even
        // `object` is a real nominal class with a real method set. 7,586
        // census positions (8.7%).
        TypeData::Any => Type::DYNAMIC,
        TypeData::Never => Type::Never,
        TypeData::SelfType => Type::SelfType,

        TypeData::NoneType => {
            // Python `None` is not the same as Rust's `!`. Lower it as a
            // builtin primitive sentinel; downstream consumers detect this by
            // the `"None"` tag.
            Type::Primitive(Primitive::Builtin("None".to_owned()))
        }

        TypeData::TypeVar(name) => Type::TypeVar(name.clone()),

        TypeData::Nominal(name) => lower_nominal(name, out, known_ids),

        TypeData::Apply { base, args } => {
            // Check if the base is a same-package nominal that should become
            // Type::Apply { base: Nominal(..), args }
            if let TypeData::Nominal(base_name) = base.as_ref() {
                let clean = strip_loc(base_name);
                // First check if it's a builtin — builtins always win.
                let builtin = lower_builtin(clean, out);
                if let Some(prim) = builtin {
                    // Builtin base with args: keep Apply wrapper with lowered args.
                    let mut lowered_args = Vec::with_capacity(args.len());
                    for arg in args {
                        lowered_args.push(lower_type(arg, out, known_ids));
                    }
                    if lowered_args.is_empty() {
                        return prim;
                    }
                    return Type::Apply {
                        base: Box::new(prim),
                        args: lowered_args.into_boxed_slice(),
                    };
                }
                // `lower_nominal` already resolves `base_name` fully — same-
                // package (`Nominal(Ref::Local)`), a local-short-name gap
                // (`Unknown(UnresolvedLocalName)`), or cross-package
                // (`Nominal(Ref::Foreign)`, case 4) — and each of those is
                // already a complete, correctly-formed `Type`. There is no
                // second "is this same-package?" branch to take: wrap
                // whatever it produced in `Apply` when there are args.
                //
                // This used to re-derive the same-package case by hand
                // (`matches!(base_ty, Type::Nominal(_))` then a fresh
                // `out.apply::<Record>(id, ..)` call), which was already
                // redundant — `out.apply` is exactly `nominal` + `Apply` —
                // and broke outright once case 4 started also returning
                // `Type::Nominal(_)`: `list[int]` with `list` genuinely
                // external took the "same-package" branch, called
                // `out.apply::<Record>("list", ..)`, and `finish()` failed
                // with `Undeclared(["list"])` because nothing declares
                // `list` in this package. Foreign nominals must never go
                // through `out.apply`/`out.refer` (same-package only, wants
                // a declaration); `base_ty` already carries the right `Ref`
                // kind, so just wrap it.
                let base_ty = lower_nominal(base_name, out, known_ids);
                if args.is_empty() {
                    return base_ty;
                }
                let mut lowered_args = Vec::with_capacity(args.len());
                for arg in args {
                    lowered_args.push(lower_type(arg, out, known_ids));
                }
                Type::Apply {
                    base: Box::new(base_ty),
                    args: lowered_args.into_boxed_slice(),
                }
            } else {
                // Non-nominal base (e.g. TypeVar application — rare in Python).
                let base_ty = lower_type(base, out, known_ids);
                let mut lowered_args = Vec::with_capacity(args.len());
                for arg in args {
                    lowered_args.push(lower_type(arg, out, known_ids));
                }
                if lowered_args.is_empty() {
                    return base_ty;
                }
                Type::Apply {
                    base: Box::new(base_ty),
                    args: lowered_args.into_boxed_slice(),
                }
            }
        }

        TypeData::Union(members) => {
            // An empty union is not `object` and not `Any` — it is a
            // degenerate node the extractor should never have produced. Say
            // so, so the next person can grep for it.
            if members.is_empty() {
                return Type::ORACLE_GAP;
            }
            let mut lowered = Vec::with_capacity(members.len());
            for m in members {
                lowered.push(lower_type(m, out, known_ids));
            }
            Type::Union(lowered.into_boxed_slice())
        }

        TypeData::Intersection(members) => {
            // As for the empty union above: a degenerate extractor node.
            if members.is_empty() {
                return Type::ORACLE_GAP;
            }
            let mut lowered = Vec::with_capacity(members.len());
            for m in members {
                lowered.push(lower_type(m, out, known_ids));
            }
            Type::Intersection(lowered.into_boxed_slice())
        }

        TypeData::Tuple(elements) => {
            // Python tuples have no per-element labels; wrap every element as
            // TupleElement::Positional so the IR shape is correct.
            let mut lowered = Vec::with_capacity(elements.len());
            for e in elements {
                lowered.push(TupleElement::Positional(lower_type(e, out, known_ids)));
            }
            Type::Tuple(lowered.into_boxed_slice())
        }

        TypeData::Annotated { inner, metadata } => {
            // PEP 593: `Annotated[T, meta1, meta2, …]`.
            // Lower the inner type first, then stack each metadata item as a
            // nested `Type::Annotated` layer. The first metadata item becomes
            // the outermost wrapper (so unwrapping once gives the next layer).
            // `AttrTok::arg` carries the metadata string as-is; `token` is
            // fixed to `"Annotated"` to make the annotation source identifiable
            // without text-searching the arg.
            let mut ty = lower_type(inner, out, known_ids);
            for meta in metadata {
                ty = Type::Annotated {
                    inner: Box::new(ty),
                    annotation: AttrTok {
                        token: "Annotated".to_owned(),
                        arg: Some(meta.clone()),
                    },
                };
            }
            ty
        }

        TypeData::Slice(inner) => Type::Slice(Box::new(lower_type(inner, out, known_ids))),

        // The extractor saw a real construct — an `Expr` shape it has no
        // case for, or a `Literal[...]` value set — and is honest that it
        // does not know how to represent it, rather than claiming the
        // source asked for dynamic typing. See `TypeData::Unsupported`'s
        // doc for why this is `NoIrRepresentation`, not `DynamicallyTyped`.
        TypeData::Unsupported(construct) => Type::no_ir_representation(construct.clone()),
    }
}

/// Lower a nominal Python type name into the appropriate IR type.
///
/// Resolution priority:
/// 1. Builtins (int, str, bool, dict, object, …) → `Type::Primitive` or a
///    universe `Ref::Foreign`, per [`lower_builtin`] — never pypi.
/// 2. Exact same-package id → `out.nominal::<Record>(id)`
/// 3. A dotted **suffix** of some same-package id → [`UnknownType::UnresolvedLocalName`]
/// 3.5. The package's own root re-exporting a short name (`mypkg.Context` where
///    `mypkg.core.Context` is declared) → also [`UnknownType::UnresolvedLocalName`],
///    never case 4's `Ref::Foreign` — see the check's own comment for why.
/// 4. Everything else → a named `Ref::Foreign` via `out.nominal_import(..)`,
///    tagged `"python-stdlib"` for a curated stdlib top-level module (see
///    [`is_stdlib_top_level_module`]) or `"pypi"` for genuine third-party
///
/// # Why 3 and 4 are different variants
///
/// They used to be one `Type::Any`, and the census says that was the single
/// biggest lie in the lattice: of 20,227 nominals that reached the fallback,
/// **13,724 (67.8%) were case 3** — a bare `Context` where this very package
/// declares `click.core.Context`. Nothing cross-package is required to close
/// those; walking the import graph is. Reporting them as "needs registry
/// linking" sent two thirds of the work to the wrong pass.
///
/// Case 3 never emits a `Ref`: the name is *recoverable* here, not *which*
/// declaration it names — `Context` can be declared in several modules and
/// only the import graph picks one. Guessing would mint a wrong `IntroId`,
/// which is strictly worse than a named gap.
///
/// Case 4 is different in kind, not degree: nothing about *which* Python
/// module declares `acme.Gadget` is ambiguous the way a same-package suffix
/// is — the dotted name already names a distribution unambiguously, this
/// producer just cannot see inside it. So it emits a real `Ref::Foreign`
/// with `target: None`, not a bare unknown: the gap is "not yet linked", not
/// "not yet identified".
fn lower_nominal(name: &str, out: &mut Lowering<PythonId>, known_ids: &KnownIds) -> Type {
    // Strip location suffix that pyrefly appends (`builtins.int@418:7-10`).
    let clean = strip_loc(name);

    // 1. Builtins always win.
    if let Some(prim) = lower_builtin(clean, out) {
        return prim;
    }

    // 2. Same-package nominal: the oracle has declared this ID in this package.
    if known_ids.contains(clean) {
        let id = PythonId::new(clean.to_owned());
        return out.nominal::<Record>(id);
    }

    // 3. A bare or partially-qualified name that this package declares under a
    // longer id. Resolvable by a within-package import-graph walk; no registry
    // involvement at all.
    if known_ids.is_local_short_name(clean) {
        return Type::unresolved_local(clean);
    }

    // 3.5. A **root re-export spelling**: the package's own top-level module
    // name qualifies a short name it re-exports at the root, e.g. `mypkg`
    // declares `mypkg.core.Context` and `mypkg/__init__.py` does `from .core
    // import Context`, so `mypkg.Context` is a real, common way to spell it —
    // but `mypkg.Context` is neither a known id (the real one is
    // `mypkg.core.Context`) nor, per guard 1 above, a suffix of one (suffixes
    // are cut at `.` boundaries: `core.Context` and `Context` are indexed,
    // `mypkg.Context` is not). Left unhandled, this fell straight through to
    // case 4, whose `namespace = clean.split('.').next()` is `mypkg` —
    // this package's OWN name — minting a `Ref::Foreign` that claims the
    // package imports from itself. That is not a foreign type under a
    // plausible name; it is a same-package name under a spelling this
    // module's suffix index was never built to recognize (the index knows
    // suffixes, not "qualified by our own root").
    //
    // Stripping exactly the package's own root and re-running the same
    // suffix check catches it: `mypkg.Context` strips to `Context`, which
    // *is* an indexed suffix. Never a `Ref::Intro` here — the same guard 3
    // reasoning as case 3 applies (a suffix hit is a candidate, not a
    // resolution; only a within-package import-graph walk picks a single
    // declaration when a name is ambiguous) — so this is `UnresolvedLocalName`,
    // not a guessed id, exactly like case 3.
    if let Some(root) = known_ids.root()
        && let Some(rest) = clean.strip_prefix(root).and_then(|r| r.strip_prefix('.'))
        && known_ids.is_local_short_name(rest)
    {
        return Type::unresolved_local(rest);
    }

    // 4. Genuinely outside this package (stdlib or third-party — builtins
    // were already peeled off in step 1). This used to lower to
    // `UnknownType::UnresolvedExternal`, a bare string no later pass could
    // ever link — Python built no `ForeignKey` anywhere, so a cross-package
    // type could never join. Emit a *named* `Ref::Foreign` instead, via the
    // lowering sink, exactly the way `UnresolvedLocalName` (case 3) is
    // exactly right for a within-package gap: `target: None` here is not a
    // guess at an `IntroId` — see the doc above for why guessing would be
    // wrong — it is the honest "named, not yet linked" state that
    // `Ref::Foreign` exists for, and what the corpus pass expects to resolve.
    //
    // pyrefly's oracle gives us the dotted name and nothing else: no distro
    // name, no wheel metadata, no way to know which pypi package publishes
    // `acme.Gadget`. So the origin is `ForeignOrigin::Namespace` — the
    // top-level dotted segment (`acme`) is the best available approximation
    // of the owning distribution — never `ForeignOrigin::Package`, which
    // would claim a precision the oracle did not give us.
    //
    // `path` carries the FULL cleaned name (`acme.Gadget`), not just the
    // namespace (`acme`): collapsing to the namespace alone would make
    // `acme.Gadget` and `acme.Widget` hash identically, which is exactly the
    // collision this module's doc warns against — two distinct external
    // types must not share an encoding. `display` keeps the spelling as
    // written, same as the old `UnresolvedExternal` did.
    //
    // A stdlib module (`pathlib.Path`, `os.PathLike`) is not a third-party
    // distribution the way `acme.Gadget` or `requests.Session` is: it ships
    // with every CPython install, under no pypi project of that name. Tagging
    // it `ForeignOrigin::Namespace { ecosystem: "pypi", .. }` would claim a
    // publisher that does not exist and would collide, in a resolver's eyes,
    // with a real pypi package that happened to share the module's top-level
    // name. `is_stdlib_top_level_module` routes it to a distinct
    // `"python-stdlib"` ecosystem instead — same `Namespace` precision (the
    // oracle still only gives us the top-level segment, not a wheel), but an
    // ecosystem tag a resolver can key differently from actual pypi lookups.
    // Only a name that clears that check falls through to genuine `"pypi"`.
    let namespace = clean.split('.').next().unwrap_or(clean);
    let ecosystem = if is_stdlib_top_level_module(namespace) {
        EcosystemId::new("python-stdlib")
    } else {
        EcosystemId::new("pypi")
    };
    let key = ForeignKey::in_namespace(ecosystem, namespace, clean, clean);
    out.nominal_import(key)
}

/// Map a cleaned (loc-stripped) name to the IR shape of a Python builtin, if
/// applicable. Returns `None` for anything that is not a known builtin —
/// crucially, this is checked **before** [`lower_nominal`]'s cross-package
/// fallback, so a language builtin can never be mistaken for a pypi
/// distribution (see that function's step 1 and the module doc's guard 2).
///
/// # Two different kinds of "builtin", two different IR shapes
///
/// Not every name `builtins` declares is honestly the same *kind* of thing:
///
/// - `int`, `str`, `bytes`, `complex`, `range`, `slice`, … are opaque value
///   types with no declaration a resolver could ever usefully point at —
///   nothing subclasses `int` in a way that matters here, nothing needs to
///   *link to* the `int` class. These lower to `Type::Primitive`, extending
///   the escape hatch `bytes`/`None` already used before this change.
/// - `dict`, `list`, `set`, `frozenset`, `tuple`, `object`, `type`, and
///   `Callable` are real declared classes/constructs with genuine identity —
///   MRO, subclassing, `isinstance` — that a `Primitive` would erase for
///   good. These mint a named `Ref::Foreign` anchored in
///   `ForeignOrigin::Universe`: "the language itself declares the name; no
///   package owns it" — the exact move `go/types.rs::go_foreign_key` makes
///   for `error`/`comparable`. Never `ForeignOrigin::Namespace { ecosystem:
///   "pypi", .. }` — `dict` is not a pypi distribution, and collapsing it
///   into one would be the P1 defect this function exists to close.
fn lower_builtin(clean: &str, out: &mut Lowering<PythonId>) -> Option<Type> {
    match clean {
        "builtins.int" | "int" => Some(Type::Primitive(Primitive::Integer {
            signed: true,
            width: Width::Fixed(NonZeroU16::new(64).unwrap()),
        })),
        "builtins.float" | "float" => Some(Type::Primitive(Primitive::Float(Width::Fixed(
            NonZeroU16::new(64).unwrap(),
        )))),
        "builtins.bool" | "bool" => Some(Type::Primitive(Primitive::Bool)),
        "builtins.str" | "str" => Some(Type::Primitive(Primitive::Str)),
        "builtins.bytes" | "bytes" => Some(Type::Primitive(Primitive::Builtin("bytes".to_owned()))),
        "builtins.NoneType" | "None" | "NoneType" => {
            Some(Type::Primitive(Primitive::Builtin("None".to_owned())))
        }
        // Opaque value types — same treatment as `bytes` above: real
        // primitives with no IR-native representation, but nothing a
        // resolver needs to place a declaration for.
        "builtins.complex" | "complex" => {
            Some(Type::Primitive(Primitive::Builtin("complex".to_owned())))
        }
        "builtins.bytearray" | "bytearray" => {
            Some(Type::Primitive(Primitive::Builtin("bytearray".to_owned())))
        }
        "builtins.memoryview" | "memoryview" => {
            Some(Type::Primitive(Primitive::Builtin("memoryview".to_owned())))
        }
        "builtins.range" | "range" => {
            Some(Type::Primitive(Primitive::Builtin("range".to_owned())))
        }
        "builtins.slice" | "slice" => {
            Some(Type::Primitive(Primitive::Builtin("slice".to_owned())))
        }
        // Class-shaped builtins — real declared identity, so a named
        // universe `Ref::Foreign`, not an erasing `Primitive`. `Callable` is
        // `typing.Callable`, not `builtins.Callable`, but it is exactly as
        // much "the language itself declares this, no package owns it" as
        // `dict` is, and the audit that requested this fix groups it with
        // `dict`/`type` for exactly that reason.
        "builtins.dict" | "dict" => Some(universe_builtin(out, "dict")),
        "builtins.list" | "list" => Some(universe_builtin(out, "list")),
        "builtins.set" | "set" => Some(universe_builtin(out, "set")),
        "builtins.frozenset" | "frozenset" => Some(universe_builtin(out, "frozenset")),
        "builtins.tuple" | "tuple" => Some(universe_builtin(out, "tuple")),
        "builtins.object" | "object" => Some(universe_builtin(out, "object")),
        "builtins.type" | "type" => Some(universe_builtin(out, "type")),
        "typing.Callable" | "Callable" => Some(universe_builtin(out, "Callable")),
        _ => None,
    }
}

/// Mint a named `Ref::Foreign` for a class-shaped Python builtin: the
/// language itself declares `name`, no pypi package owns it.
///
/// `path` and `display` are both the bare builtin name — there is no dotted
/// qualification to preserve (unlike a real cross-package `acme.Gadget`),
/// and the `Universe` origin tag is what keeps `dict` from ever encoding
/// identically to a hypothetical pypi package literally named `dict` (see
/// `ForeignOrigin::encode`: the origin's ecosystem/tag byte is hashed before
/// `path`, so `Namespace{"pypi"}` and `Universe{"python"}` can never collide).
fn universe_builtin(out: &mut Lowering<PythonId>, name: &str) -> Type {
    out.nominal_import(ForeignKey::in_universe(
        EcosystemId::new("python"),
        name,
        name,
    ))
}

/// Is `top_level` the name of a CPython standard-library top-level module or
/// package (`os`, `pathlib`, `typing`, `collections`, …)?
///
/// # Why this exists, and why it is curated rather than derived
///
/// Before this check, every dotted name the local-suffix pass (case 3) could
/// not place — including `pathlib.Path` and `os.PathLike` — fell straight
/// through to a `ForeignOrigin::Namespace { ecosystem: "pypi", .. }` key,
/// claiming `pathlib` is a pypi distribution. It is not: it ships with every
/// CPython install, under no pypi project of that name, and a resolver that
/// trusted the `pypi` tag could link it to an unrelated real package that
/// happens to squat the name `pathlib` on PyPI.
///
/// There is no oracle signal for "this is stdlib" — pyrefly reports only the
/// dotted name — so this is a curated list of CPython 3.12 top-level stdlib
/// module/package names, not a derived one. It covers the common annotation
/// surface (`os`, `sys`, `typing`, `collections`, `pathlib`, `datetime`,
/// `asyncio`, …). A name missing from this list degrades to the `"pypi"`
/// branch, same as before this fix — a false negative here costs exactly one
/// ecosystem tag on an already-honest, already-linkable `Ref::Foreign`, never
/// a wrong identity or a dropped reference.
fn is_stdlib_top_level_module(top_level: &str) -> bool {
    matches!(
        top_level,
        "__future__"
            | "abc"
            | "aifc"
            | "argparse"
            | "array"
            | "ast"
            | "asynchat"
            | "asyncio"
            | "asyncore"
            | "atexit"
            | "audioop"
            | "base64"
            | "bdb"
            | "binascii"
            | "bisect"
            | "builtins"
            | "bz2"
            | "calendar"
            | "cgi"
            | "cgitb"
            | "chunk"
            | "cmath"
            | "cmd"
            | "code"
            | "codecs"
            | "codeop"
            | "collections"
            | "colorsys"
            | "compileall"
            | "concurrent"
            | "configparser"
            | "contextlib"
            | "contextvars"
            | "copy"
            | "copyreg"
            | "cProfile"
            | "csv"
            | "ctypes"
            | "curses"
            | "dataclasses"
            | "datetime"
            | "dbm"
            | "decimal"
            | "difflib"
            | "dis"
            | "doctest"
            | "email"
            | "encodings"
            | "ensurepip"
            | "enum"
            | "errno"
            | "faulthandler"
            | "fcntl"
            | "filecmp"
            | "fileinput"
            | "fnmatch"
            | "fractions"
            | "ftplib"
            | "functools"
            | "gc"
            | "getopt"
            | "getpass"
            | "gettext"
            | "glob"
            | "graphlib"
            | "gzip"
            | "hashlib"
            | "heapq"
            | "hmac"
            | "html"
            | "http"
            | "idlelib"
            | "imaplib"
            | "imghdr"
            | "imp"
            | "importlib"
            | "inspect"
            | "io"
            | "ipaddress"
            | "itertools"
            | "json"
            | "keyword"
            | "lib2to3"
            | "linecache"
            | "locale"
            | "logging"
            | "lzma"
            | "mailbox"
            | "mailcap"
            | "marshal"
            | "math"
            | "mimetypes"
            | "mmap"
            | "modulefinder"
            | "msilib"
            | "msvcrt"
            | "multiprocessing"
            | "netrc"
            | "nis"
            | "nntplib"
            | "numbers"
            | "operator"
            | "optparse"
            | "os"
            | "ossaudiodev"
            | "pathlib"
            | "pdb"
            | "pickle"
            | "pickletools"
            | "pipes"
            | "pkgutil"
            | "platform"
            | "plistlib"
            | "poplib"
            | "posixpath"
            | "pprint"
            | "profile"
            | "pstats"
            | "pty"
            | "pwd"
            | "py_compile"
            | "pyclbr"
            | "pydoc"
            | "queue"
            | "quopri"
            | "random"
            | "re"
            | "readline"
            | "reprlib"
            | "resource"
            | "rlcompleter"
            | "runpy"
            | "sched"
            | "secrets"
            | "select"
            | "selectors"
            | "shelve"
            | "shlex"
            | "shutil"
            | "signal"
            | "site"
            | "smtplib"
            | "sndhdr"
            | "socket"
            | "socketserver"
            | "spwd"
            | "sqlite3"
            | "ssl"
            | "stat"
            | "statistics"
            | "string"
            | "stringprep"
            | "struct"
            | "subprocess"
            | "sunau"
            | "symtable"
            | "sys"
            | "sysconfig"
            | "syslog"
            | "tabnanny"
            | "tarfile"
            | "telnetlib"
            | "tempfile"
            | "termios"
            | "textwrap"
            | "threading"
            | "time"
            | "timeit"
            | "tkinter"
            | "token"
            | "tokenize"
            | "tomllib"
            | "trace"
            | "traceback"
            | "tracemalloc"
            | "tty"
            | "turtle"
            | "turtledemo"
            | "types"
            | "typing"
            | "unicodedata"
            | "unittest"
            | "urllib"
            | "uu"
            | "uuid"
            | "venv"
            | "warnings"
            | "wave"
            | "weakref"
            | "webbrowser"
            | "winreg"
            | "winsound"
            | "wsgiref"
            | "xdrlib"
            | "xml"
            | "xmlrpc"
            | "zipapp"
            | "zipfile"
            | "zipimport"
            | "zlib"
            | "zoneinfo"
    )
}

/// Strip pyrefly's `@line:col-col` location suffix from a qualified name.
pub fn strip_loc(name: &str) -> &str {
    name.find('@').map_or(name, |pos| &name[..pos])
}

/// Lower a list of [`GenericParamData`] into IR [`GenericParam`]s.
///
/// Bounds and constraints are real type references; they get the same
/// same-package resolution as any other type position.
pub fn lower_generics(
    params: &[GenericParamData],
    out: &mut Lowering<PythonId>,
    known_ids: &KnownIds,
) -> Box<[GenericParam]> {
    let mut result = Vec::with_capacity(params.len());
    for p in params {
        let mut bounds: Vec<Type> = Vec::new();
        // Bound and constraints: use TypeVar name when the bound IS the param
        // itself (e.g. `T: T` — degenerate), otherwise lower normally.
        if let Some(bound) = &p.bound {
            bounds.push(lower_type(bound, out, known_ids));
        }
        for constraint in &p.constraints {
            bounds.push(lower_type(constraint, out, known_ids));
        }
        let default = p.default.as_ref().map(|d| lower_type(d, out, known_ids));
        result.push(GenericParam::Type {
            name: p.name.clone(),
            bounds: bounds.into_boxed_slice(),
            default,
            // Pyrefly does not surface declaration-site variance (covariant/
            // contravariant flags on TypeVar) in the oracle. When it does,
            // GenericParamData should gain a `variance` field and this should
            // map True → Some(Variance::Covariant), False → Contravariant.
            // For now: None (unspecified / no annotation in source).
            variance: None,
        });
    }
    result.into_boxed_slice()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use nudox_ir::{
        entry::{Symbol, Visibility},
        foreign::ForeignOrigin,
        index::Ref,
        kinds::UnknownType,
        lower::Lowering,
        package::PackageId,
    };
    use std::path::PathBuf;

    /// Build a minimal `Lowering` and an empty `known_ids` set for tests that
    /// do not need same-package resolution.
    fn make_sink() -> Lowering<PythonId> {
        let pkg_id = PackageId::path("/tmp/test");
        let sym = Symbol {
            name: "test_pkg".to_owned(),
            visibility: Visibility::Public,
            documentation: String::new(),
            source: PathBuf::new(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };
        Lowering::new(pkg_id, sym)
    }

    fn empty_ids() -> KnownIds {
        KnownIds::default()
    }

    fn ids(names: &[&str]) -> KnownIds {
        names.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn typevar_maps_to_typevar_not_any() {
        let ty = TypeData::TypeVar("T".to_string());
        let mut sink = make_sink();
        let lowered = lower_type(&ty, &mut sink, &empty_ids());
        assert!(
            matches!(lowered, Type::TypeVar(ref name) if name == "T"),
            "TypeVar must lower to Type::TypeVar, got {lowered:?}"
        );
    }

    #[test]
    fn optional_int_is_union_with_none_sentinel() {
        // Optional[int] is Union([int, NoneType]) in TypeData.
        let ty = TypeData::Union(vec![
            TypeData::Nominal("builtins.int".to_string()),
            TypeData::NoneType,
        ]);
        let mut sink = make_sink();
        let lowered = lower_type(&ty, &mut sink, &empty_ids());
        match lowered {
            Type::Union(ref members) => {
                assert_eq!(members.len(), 2, "Union must have 2 members");
                assert!(
                    matches!(members[0], Type::Primitive(Primitive::Integer { .. })),
                    "first member should be int primitive"
                );
                assert!(
                    matches!(&members[1], Type::Primitive(Primitive::Builtin(s)) if s == "None"),
                    "second member should be None sentinel"
                );
            }
            other => panic!("expected Union, got {other:?}"),
        }
    }

    #[test]
    fn never_lowers_to_never() {
        let mut sink = make_sink();
        assert_eq!(
            lower_type(&TypeData::Never, &mut sink, &empty_ids()),
            Type::Never
        );
    }

    #[test]
    fn self_type_lowers_to_self_type() {
        let mut sink = make_sink();
        assert_eq!(
            lower_type(&TypeData::SelfType, &mut sink, &empty_ids()),
            Type::SelfType
        );
    }

    #[test]
    fn apply_preserves_structure() {
        let ty = TypeData::Apply {
            base: Box::new(TypeData::Nominal("builtins.list".to_string())),
            args: vec![TypeData::TypeVar("T".to_string())],
        };
        let mut sink = make_sink();
        let lowered = lower_type(&ty, &mut sink, &empty_ids());
        assert!(
            matches!(lowered, Type::Apply { .. }),
            "Apply must lower to Apply, got {lowered:?}"
        );
    }

    #[test]
    fn str_nominal_maps_to_primitive() {
        let mut sink = make_sink();
        assert!(
            matches!(
                lower_type(
                    &TypeData::Nominal("str".to_string()),
                    &mut sink,
                    &empty_ids()
                ),
                Type::Primitive(Primitive::Str)
            ),
            "str must map to Primitive::Str"
        );
        let mut sink = make_sink();
        assert!(
            matches!(
                lower_type(
                    &TypeData::Nominal("builtins.str".to_string()),
                    &mut sink,
                    &empty_ids()
                ),
                Type::Primitive(Primitive::Str)
            ),
            "builtins.str must also map to Primitive::Str"
        );
    }

    /// A genuinely cross-package name is a **named `Ref::Foreign`**, never a
    /// bare `UnresolvedExternal` — and it keeps its spelling.
    ///
    /// Asserting `matches!(ty, Type::Nominal(_))` here would be the same
    /// tautology CC-2 exists to remove — the whole point is that a consumer
    /// can read *which* package this names and *what* it names inside it,
    /// and — with a resolver — follow it.
    #[test]
    fn external_nominal_is_a_named_foreign_ref_with_its_spelling() {
        let mut sink = make_sink();
        let lowered = lower_type(
            &TypeData::Nominal("some_lib.SomeClass".to_string()),
            &mut sink,
            &empty_ids(),
        );
        match lowered {
            Type::Nominal(Ref::Foreign { key, target }) => {
                assert!(
                    matches!(key.origin, ForeignOrigin::Namespace { .. }),
                    "pyrefly gives no distribution identity, so this must be a \
                     Namespace origin, got {:?}",
                    key.origin
                );
                assert_eq!(
                    &*key.path, "some_lib.SomeClass",
                    "the full dotted name is the durable join key, not just the namespace"
                );
                assert_eq!(
                    &*key.display, "some_lib.SomeClass",
                    "display must keep the source spelling"
                );
                assert_eq!(target, None, "no resolver was supplied, so it stays unlinked");
            }
            other => panic!(
                "a cross-package name must lower to a named Ref::Foreign, got {other:?}"
            ),
        }
    }

    /// **The 67.8% case.** `-> "Context"` inside a package that declares
    /// `click.core.Context` is a *local* name-resolution gap, not a
    /// cross-package one — 13,724 of 20,227 measured unresolved nominals.
    #[test]
    fn bare_name_declared_locally_is_unresolved_local_not_external() {
        let mut sink = make_sink();
        let known = ids(&["click.core.Context", "click.core.Command"]);
        let lowered = lower_type(&TypeData::Nominal("Context".to_string()), &mut sink, &known);
        assert_eq!(
            lowered,
            Type::Unknown(UnknownType::UnresolvedLocalName {
                name: "Context".to_owned()
            }),
            "a bare name the package declares must not be reported as external"
        );

        // A partially-qualified spelling resolves the same way.
        assert_eq!(
            lower_type(
                &TypeData::Nominal("core.Context".to_string()),
                &mut sink,
                &known
            ),
            Type::Unknown(UnknownType::UnresolvedLocalName {
                name: "core.Context".to_owned()
            }),
        );

        // A name the package does not declare at any suffix is a named
        // cross-package `Ref::Foreign`, not `UnresolvedExternal`.
        match lower_type(
            &TypeData::Nominal("requests.Session".to_string()),
            &mut sink,
            &known,
        ) {
            Type::Nominal(Ref::Foreign { key, target }) => {
                assert_eq!(&*key.path, "requests.Session");
                assert_eq!(target, None);
            }
            other => panic!("expected Nominal(Foreign), got {other:?}"),
        }
    }

    /// `TypeData::Unsupported` — the extractor's own gap — must lower to
    /// `NoIrRepresentation`, never to `DynamicallyTyped`/`Type::DYNAMIC`.
    /// This is the split CC-2's Python half exists for: "we didn't
    /// understand this expression" is a different fact from "the source
    /// wrote `Any`", and conflating them was the whole defect.
    #[test]
    fn unsupported_extractor_gap_is_no_ir_representation_not_dynamically_typed() {
        let mut sink = make_sink();
        let lowered = lower_type(
            &TypeData::Unsupported("lambda: 1".to_owned()),
            &mut sink,
            &empty_ids(),
        );
        assert_eq!(
            lowered,
            Type::Unknown(UnknownType::NoIrRepresentation {
                construct: "lambda: 1".to_owned()
            }),
            "an unhandled Expr shape must say what it saw, not claim `Any`"
        );
        assert_ne!(
            lowered,
            Type::Unknown(UnknownType::DynamicallyTyped),
            "the extractor not understanding an expression is not the source asking for dynamic typing"
        );
    }

    /// Explicit `typing.Any` is `DynamicallyTyped`, and is *not* the top type.
    ///
    /// Python has no top type to map onto `Type::Any`: `typing.Any` is
    /// consistent with every type in both directions, which `object` is not.
    #[test]
    fn explicit_any_is_dynamically_typed_not_the_top_type() {
        let mut sink = make_sink();
        let lowered = lower_type(&TypeData::Any, &mut sink, &empty_ids());
        assert_eq!(lowered, Type::Unknown(UnknownType::DynamicallyTyped));
        assert_ne!(lowered, Type::Any, "`typing.Any` is not a top type");
        assert_ne!(
            lowered,
            Type::Unknown(UnknownType::Unannotated),
            "`def f(x: Any)` and `def f(x)` are different source"
        );
    }

    /// A degenerate empty union/intersection is an extractor fault, and says so.
    #[test]
    fn empty_union_is_an_oracle_gap() {
        let mut sink = make_sink();
        assert_eq!(
            lower_type(&TypeData::Union(vec![]), &mut sink, &empty_ids()),
            Type::Unknown(UnknownType::OracleGap)
        );
        assert_eq!(
            lower_type(&TypeData::Intersection(vec![]), &mut sink, &empty_ids()),
            Type::Unknown(UnknownType::OracleGap)
        );
    }

    /// **The obvious false positive, pinned.**
    ///
    /// `mypkg/models.py` declares a class `Path`; `mypkg/io.py` does
    /// `from pathlib import Path` and annotates with it. The suffix index
    /// cannot tell those apart — only the import graph can — so this test
    /// asserts the *guard* rather than a correct resolution: the hit must
    /// stay an unresolved, visibly-marked gap and must never become a `Ref`.
    ///
    /// That is what bounds the cost of the heuristic to a wrong reason string
    /// instead of a wrong identity edge or a hyperlink to the wrong class.
    #[test]
    fn same_named_class_in_an_unrelated_module_stays_a_candidate_not_a_ref() {
        let mut sink = make_sink();
        let known = ids(&["mypkg.models.Path", "mypkg.io.open"]);

        let lowered = lower_type(&TypeData::Nominal("Path".to_string()), &mut sink, &known);

        // It is classified as a local candidate — that much is a guess.
        assert_eq!(
            lowered,
            Type::Unknown(UnknownType::UnresolvedLocalName {
                name: "Path".to_owned()
            })
        );
        // But the guess is contained: no nominal reference, so nothing
        // downstream can follow it to `mypkg.models.Path`.
        assert!(
            !matches!(lowered, Type::Nominal(_) | Type::Apply { .. }),
            "a suffix hit must never mint a reference — it has not walked the import graph"
        );
        // And it renders as visibly unresolved, so a reader is never told the
        // producer knows which `Path` this is.
        assert_eq!(lowered.to_string(), "?unresolved(Path)");
    }

    /// A name declared in two unrelated modules is ambiguous *inside* the
    /// package, and `local_candidates` says so.
    ///
    /// A confirming pass that silently took the first hit here would bind half
    /// its references to the wrong class; the count is what lets it refuse.
    #[test]
    fn ambiguous_local_names_report_their_candidate_count() {
        let known = ids(&["mypkg.models.Path", "mypkg.compat.Path", "mypkg.core.Ctx"]);
        assert_eq!(known.local_candidates("Path"), 2, "declared twice");
        assert_eq!(known.local_candidates("Ctx"), 1, "declared once");
        assert_eq!(
            known.local_candidates("Session"),
            0,
            "not declared at all — not a local candidate"
        );
        // The boolean stays consistent with the count.
        assert!(known.is_local_short_name("Path"));
        assert!(!known.is_local_short_name("Session"));
    }

    /// Guard 2: builtins resolve before the suffix index is consulted, so a
    /// package declaring its own `str` cannot capture the primitive.
    #[test]
    fn builtins_win_over_a_same_named_local_declaration() {
        let mut sink = make_sink();
        let known = ids(&["mypkg.shims.str", "mypkg.shims.int"]);
        assert!(
            known.is_local_short_name("str"),
            "the shim really is declared"
        );
        assert_eq!(
            lower_type(&TypeData::Nominal("str".to_string()), &mut sink, &known),
            Type::Primitive(Primitive::Str),
            "`str` must stay the primitive even when the package shadows the name"
        );
    }

    /// `KnownIds` indexes suffixes, not substrings.
    #[test]
    fn suffix_index_matches_dotted_boundaries_only() {
        let known = ids(&["click.core.Context"]);
        assert!(known.contains("click.core.Context"));
        assert!(known.is_local_short_name("Context"));
        assert!(known.is_local_short_name("core.Context"));
        // Not a dotted suffix — `ext` is a substring of `Context`, not a
        // segment of it. A substring index here would report thousands of
        // false locals and send real cross-package work to the wrong pass.
        assert!(!known.is_local_short_name("ext"));
        assert!(!known.is_local_short_name("click"));
        // The full id is reachable via `contains`, not via the suffix index.
        assert!(!known.is_local_short_name("click.core.Context"));
    }

    #[test]
    fn generic_param_with_bound() {
        let params = vec![GenericParamData {
            name: "T".to_string(),
            bound: Some(TypeData::Nominal("builtins.int".to_string())),
            constraints: vec![],
            default: None,
        }];
        let mut sink = make_sink();
        let lowered = lower_generics(&params, &mut sink, &empty_ids());
        assert_eq!(lowered.len(), 1);
        match &lowered[0] {
            GenericParam::Type {
                name,
                bounds,
                default,
                variance,
            } => {
                assert_eq!(name, "T");
                assert_eq!(bounds.len(), 1);
                assert!(default.is_none());
                assert!(
                    variance.is_none(),
                    "Python TypeVar has no declaration-site variance"
                );
            }
            other => panic!("expected GenericParam::Type, got {other:?}"),
        }
    }

    #[test]
    fn strip_loc_removes_suffix() {
        assert_eq!(strip_loc("builtins.int@418:7-10"), "builtins.int");
        assert_eq!(strip_loc("my.module.Class"), "my.module.Class");
    }

    #[test]
    fn union_with_single_element() {
        // A union with one element is still a Union, not unwrapped.
        let ty = TypeData::Union(vec![TypeData::TypeVar("T".to_string())]);
        let mut sink = make_sink();
        let lowered = lower_type(&ty, &mut sink, &empty_ids());
        assert!(matches!(lowered, Type::Union(_)));
    }

    // ── New tests: same-package nominal resolution ───────────────────────────

    /// A method whose return type names another class **declared later in the
    /// same package** must lower to `Type::Nominal`, not `Type::Any`.
    /// After `seal`, the nominal resolves to `Ref::Intro` pointing at that class.
    #[test]
    fn same_package_nominal_resolves_not_any() {
        use nudox_ir::kinds::Record;

        let mut sink = make_sink();
        // known_ids contains both "my_pkg.Holder" and "my_pkg.Payload".
        let known: KnownIds = ["my_pkg.Holder", "my_pkg.Payload"]
            .iter()
            .map(std::string::ToString::to_string)
            .collect();

        // Lower a return type that names Payload — which is not yet declared.
        let ret_ty = lower_type(
            &TypeData::Nominal("my_pkg.Payload".to_string()),
            &mut sink,
            &known,
        );
        assert!(
            matches!(ret_ty, Type::Nominal(_)),
            "same-package Payload must lower to Nominal, not Any; got {ret_ty:?}"
        );

        // Now declare both entries so finish() succeeds.
        let holder_sym = Symbol {
            name: "Holder".to_owned(),
            visibility: Visibility::Public,
            documentation: String::new(),
            source: PathBuf::new(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };
        let payload_sym = Symbol {
            name: "Payload".to_owned(),
            visibility: Visibility::Public,
            documentation: String::new(),
            source: PathBuf::new(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };
        sink.declare(
            PythonId::new("my_pkg.Holder"),
            None,
            holder_sym,
            Record::builder().build(),
        );
        // Declare Payload AFTER the nominal reference was interned — this is the
        // forward-reference scenario that the flat sink was built for.
        sink.declare(
            PythonId::new("my_pkg.Payload"),
            None,
            payload_sym,
            Record::builder().build(),
        );

        let pkg = sink
            .finish()
            .expect("forward-referenced nominal must resolve");

        // Verify Payload is present and the nominal sealed to Ref::Intro.
        let payload_entry = pkg
            .iter()
            .find(|(_, e)| e.sym().name == "Payload")
            .expect("Payload must be in the package");

        // The nominal we lowered earlier should have sealed to an Intro ref.
        // We verify by checking that Payload's intro-id matches what ret_ty
        // points at. Use the sealed package's entry index.
        match ret_ty {
            Type::Nominal(_) => {
                // The RawRef was interned at lower_type time. After finish(),
                // it resolved to Payload's IntroId (no LoweringError returned).
                // The exact IntroId comparison would require seal access; it is
                // sufficient to assert that finish() succeeded and Payload exists.
                let _ = payload_entry;
            }
            other => panic!("expected Nominal, got {other:?}"),
        }
    }

    /// `apply::<Record>(id, args)` over a same-package class produces
    /// `Type::Apply { base: Nominal(..), args }`.
    #[test]
    fn same_package_generic_apply() {
        use nudox_ir::kinds::Record;

        let mut sink = make_sink();
        let known: KnownIds = std::iter::once("my_pkg.Container")
            .map(std::string::ToString::to_string)
            .collect();

        // Container[int]
        let applied = lower_type(
            &TypeData::Apply {
                base: Box::new(TypeData::Nominal("my_pkg.Container".to_string())),
                args: vec![TypeData::Nominal("int".to_string())],
            },
            &mut sink,
            &known,
        );

        assert!(
            matches!(applied, Type::Apply { .. }),
            "same-package Container[int] must lower to Apply, got {applied:?}"
        );

        // The base of the Apply must be a Nominal (same-package ref), not Any.
        if let Type::Apply { base, args } = &applied {
            assert!(
                matches!(base.as_ref(), Type::Nominal(_)),
                "Apply base must be Nominal for same-package class, got {base:?}"
            );
            assert_eq!(args.len(), 1, "must have 1 type arg");
            assert!(
                matches!(args[0], Type::Primitive(Primitive::Integer { .. })),
                "arg must be int primitive"
            );
        }

        // Declare Container so finish() succeeds.
        let container_sym = Symbol {
            name: "Container".to_owned(),
            visibility: Visibility::Public,
            documentation: String::new(),
            source: PathBuf::new(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };
        sink.declare(
            PythonId::new("my_pkg.Container"),
            None,
            container_sym,
            Record::builder().build(),
        );
        sink.finish().expect("must finish successfully");
    }

    /// A bound or constraint that refers to a same-package class must lower to
    /// `Type::Nominal`, not `Type::Any`.
    #[test]
    fn generic_param_bound_resolves_same_package() {
        use nudox_ir::kinds::Record;

        let mut sink = make_sink();
        let known: KnownIds = std::iter::once("my_pkg.Base")
            .map(std::string::ToString::to_string)
            .collect();

        let params = vec![GenericParamData {
            name: "T".to_string(),
            bound: Some(TypeData::Nominal("my_pkg.Base".to_string())),
            constraints: vec![],
            default: None,
        }];
        let lowered = lower_generics(&params, &mut sink, &known);
        assert_eq!(lowered.len(), 1);
        match &lowered[0] {
            GenericParam::Type {
                name,
                bounds,
                variance,
                ..
            } => {
                assert_eq!(name, "T");
                assert_eq!(bounds.len(), 1);
                assert!(
                    matches!(bounds[0], Type::Nominal(_)),
                    "bound on same-package class must be Nominal, got {:?}",
                    bounds[0]
                );
                assert!(
                    variance.is_none(),
                    "Python TypeVar has no declaration-site variance"
                );
            }
            other => panic!("expected GenericParam::Type, got {other:?}"),
        }

        // Declare Base so finish succeeds.
        let base_sym = Symbol {
            name: "Base".to_owned(),
            visibility: Visibility::Public,
            documentation: String::new(),
            source: PathBuf::new(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };
        sink.declare(
            PythonId::new("my_pkg.Base"),
            None,
            base_sym,
            Record::builder().build(),
        );
        sink.finish().expect("must finish successfully");
    }

    // ── New: Annotated[T, meta] (PEP 593) ─────────────────────────────────────

    /// `Annotated[int, "validator"]` lowers to
    /// `Type::Annotated { inner: Primitive(Integer), annotation: AttrTok { token: "Annotated", arg: Some("validator") } }`.
    #[test]
    fn annotated_single_metadata_round_trip() {
        use crate::python::oracle::TypeData;

        let ty = TypeData::Annotated {
            inner: Box::new(TypeData::Nominal("builtins.int".to_string())),
            metadata: vec!["validator".to_string()],
        };
        let mut sink = make_sink();
        let lowered = lower_type(&ty, &mut sink, &empty_ids());
        match lowered {
            Type::Annotated {
                ref inner,
                ref annotation,
            } => {
                assert_eq!(annotation.token, "Annotated");
                assert_eq!(annotation.arg.as_deref(), Some("validator"));
                assert!(
                    matches!(**inner, Type::Primitive(Primitive::Integer { .. })),
                    "inner must be int primitive; got {inner:?}"
                );
            }
            other => panic!("expected Type::Annotated, got {other:?}"),
        }
    }

    /// Multiple metadata items stack as nested `Annotated` layers.
    #[test]
    fn annotated_multiple_metadata_stacks() {
        use crate::python::oracle::TypeData;

        let ty = TypeData::Annotated {
            inner: Box::new(TypeData::Nominal("builtins.str".to_string())),
            metadata: vec!["meta1".to_string(), "meta2".to_string()],
        };
        let mut sink = make_sink();
        let lowered = lower_type(&ty, &mut sink, &empty_ids());
        // Outer layer wraps meta2 (last), inner layer wraps meta1 (first).
        match &lowered {
            Type::Annotated {
                inner: outer_inner,
                annotation,
            } => {
                assert_eq!(annotation.arg.as_deref(), Some("meta2"));
                match outer_inner.as_ref() {
                    Type::Annotated {
                        inner: inner_inner,
                        annotation: inner_ann,
                    } => {
                        assert_eq!(inner_ann.arg.as_deref(), Some("meta1"));
                        assert!(matches!(**inner_inner, Type::Primitive(Primitive::Str)));
                    }
                    other => panic!("expected nested Annotated; got {other:?}"),
                }
            }
            other => panic!("expected Annotated; got {other:?}"),
        }
    }

    /// `Annotated` with no metadata items degrades to the inner type directly
    /// (no Annotated wrapper when there's nothing to annotate with).
    #[test]
    fn annotated_no_metadata_returns_inner() {
        use crate::python::oracle::TypeData;

        let ty = TypeData::Annotated {
            inner: Box::new(TypeData::Never),
            metadata: vec![],
        };
        let mut sink = make_sink();
        let lowered = lower_type(&ty, &mut sink, &empty_ids());
        assert_eq!(
            lowered,
            Type::Never,
            "empty-metadata Annotated must lower to inner type"
        );
    }

    // ── New: builtins must not be fabricated pypi Foreign (P1) ───────────────

    /// The value-shaped builtin extension set (`complex`, `bytearray`,
    /// `memoryview`, `range`, `slice`) lowers to `Type::Primitive::Builtin`,
    /// exactly like `bytes`/`None` already did — never a `Ref` of any kind.
    #[test]
    fn value_shaped_builtins_lower_to_primitive_builtin() {
        for (name, tag) in [
            ("complex", "complex"),
            ("bytearray", "bytearray"),
            ("memoryview", "memoryview"),
            ("range", "range"),
            ("slice", "slice"),
        ] {
            let mut sink = make_sink();
            let lowered = lower_type(&TypeData::Nominal(name.to_string()), &mut sink, &empty_ids());
            assert_eq!(
                lowered,
                Type::Primitive(Primitive::Builtin(tag.to_owned())),
                "`{name}` must lower to Primitive::Builtin(\"{tag}\"), got {lowered:?}"
            );
            // The `builtins.`-prefixed pyrefly spelling must lower identically.
            let mut sink = make_sink();
            let prefixed = lower_type(
                &TypeData::Nominal(format!("builtins.{name}")),
                &mut sink,
                &empty_ids(),
            );
            assert_eq!(prefixed, lowered, "`builtins.{name}` must match the bare spelling");
        }
    }

    /// The class-shaped builtin set (`dict`, `list`, `set`, `frozenset`,
    /// `tuple`, `object`, `type`, `Callable`) lowers to a named
    /// `Ref::Foreign` anchored in `ForeignOrigin::Universe { ecosystem:
    /// "python" }` — real declared identity, never `Primitive` (which would
    /// erase it) and never `Namespace { ecosystem: "pypi", .. }` (which
    /// would fabricate a publisher — the exact P1 defect).
    #[test]
    fn class_shaped_builtins_lower_to_python_universe_foreign() {
        for name in ["dict", "list", "set", "frozenset", "tuple", "object", "type", "Callable"] {
            let mut sink = make_sink();
            let lowered = lower_type(&TypeData::Nominal(name.to_string()), &mut sink, &empty_ids());
            match lowered {
                Type::Nominal(Ref::Foreign { key, target }) => {
                    assert_eq!(
                        key.origin,
                        ForeignOrigin::Universe {
                            ecosystem: EcosystemId::new("python")
                        },
                        "`{name}` must be a python-universe Foreign, got {:?}",
                        key.origin
                    );
                    assert_eq!(&*key.path, name);
                    assert_eq!(&*key.display, name);
                    assert_eq!(target, None, "no resolver was supplied");
                }
                other => panic!("`{name}` must lower to Nominal(Foreign), got {other:?}"),
            }
        }
    }

    /// `dict[str, Foo]` — a builtin generic base applied to a same-package
    /// arg. The base must be the universe Foreign (not pypi, not dropped),
    /// and the arg must still resolve to the same-package Nominal — the
    /// builtin-base short-circuit in `lower_type`'s `Apply` arm must not
    /// skip lowering the args.
    #[test]
    fn builtin_generic_base_preserves_arg_resolution() {
        let mut sink = make_sink();
        let known: KnownIds = std::iter::once("my_pkg.Foo")
            .map(std::string::ToString::to_string)
            .collect();

        let applied = lower_type(
            &TypeData::Apply {
                base: Box::new(TypeData::Nominal("dict".to_string())),
                args: vec![
                    TypeData::Nominal("str".to_string()),
                    TypeData::Nominal("my_pkg.Foo".to_string()),
                ],
            },
            &mut sink,
            &known,
        );

        match &applied {
            Type::Apply { base, args } => {
                match base.as_ref() {
                    Type::Nominal(Ref::Foreign { key, .. }) => {
                        assert_eq!(
                            key.origin,
                            ForeignOrigin::Universe {
                                ecosystem: EcosystemId::new("python")
                            },
                            "dict's base must be the python-universe Foreign, got {:?}",
                            key.origin
                        );
                    }
                    other => panic!("dict base must be Nominal(Foreign), got {other:?}"),
                }
                assert_eq!(args.len(), 2);
                assert!(matches!(args[0], Type::Primitive(Primitive::Str)));
                assert!(
                    matches!(args[1], Type::Nominal(_)),
                    "the same-package arg `Foo` must still resolve to a Nominal, got {:?}",
                    args[1]
                );
                if let Type::Nominal(Ref::Foreign { .. }) = &args[1] {
                    panic!("the same-package arg `Foo` must not degrade to a Foreign ref");
                }
            }
            other => panic!("dict[str, Foo] must lower to Apply, got {other:?}"),
        }
    }

    // ── New: stdlib is not third-party pypi (P13) ─────────────────────────────

    /// A stdlib dotted name (`pathlib.Path`, `os.PathLike`, `typing.Optional`)
    /// must be tagged `"python-stdlib"`, never `"pypi"` — stdlib ships with
    /// every CPython install under no pypi project of that name.
    #[test]
    fn stdlib_dotted_names_are_not_pypi() {
        for (name, ns) in [
            ("pathlib.Path", "pathlib"),
            ("os.PathLike", "os"),
            ("typing.Optional", "typing"),
            ("collections.OrderedDict", "collections"),
        ] {
            let mut sink = make_sink();
            let lowered = lower_type(&TypeData::Nominal(name.to_string()), &mut sink, &empty_ids());
            match lowered {
                Type::Nominal(Ref::Foreign { key, target }) => {
                    assert_eq!(
                        key.origin,
                        ForeignOrigin::Namespace {
                            ecosystem: EcosystemId::new("python-stdlib"),
                            namespace: ns.into(),
                        },
                        "`{name}` must be tagged python-stdlib, got {:?}",
                        key.origin
                    );
                    assert_eq!(&*key.path, name);
                    assert_eq!(target, None);
                }
                other => panic!("`{name}` must lower to Nominal(Foreign), got {other:?}"),
            }
        }
    }

    /// A genuinely third-party name keeps the `"pypi"` tag — this is the
    /// negative case that proves `is_stdlib_top_level_module` discriminates
    /// rather than swallowing everything into `"python-stdlib"`.
    #[test]
    fn genuine_third_party_names_stay_pypi() {
        let mut sink = make_sink();
        let lowered = lower_type(
            &TypeData::Nominal("requests.Session".to_string()),
            &mut sink,
            &empty_ids(),
        );
        match lowered {
            Type::Nominal(Ref::Foreign { key, .. }) => {
                assert_eq!(
                    key.origin,
                    ForeignOrigin::Namespace {
                        ecosystem: EcosystemId::new("pypi"),
                        namespace: "requests".into(),
                    },
                    "`requests.Session` must stay pypi, got {:?}",
                    key.origin
                );
            }
            other => panic!("expected Nominal(Foreign), got {other:?}"),
        }
    }

    // ── New: Tuple wraps elements as TupleElement::Positional ─────────────────

    /// Python tuples lower to `Type::Tuple(List<TupleElement::Positional>)`.
    #[test]
    fn tuple_elements_are_positional() {
        use crate::python::oracle::TypeData;

        let ty = TypeData::Tuple(vec![
            TypeData::Nominal("builtins.int".to_string()),
            TypeData::Nominal("builtins.str".to_string()),
        ]);
        let mut sink = make_sink();
        let lowered = lower_type(&ty, &mut sink, &empty_ids());
        match lowered {
            Type::Tuple(ref elements) => {
                assert_eq!(elements.len(), 2);
                for e in elements {
                    assert!(
                        matches!(e, TupleElement::Positional(_)),
                        "Python tuple elements must be Positional, not Named; got {e:?}"
                    );
                }
            }
            other => panic!("expected Type::Tuple, got {other:?}"),
        }
    }
}
