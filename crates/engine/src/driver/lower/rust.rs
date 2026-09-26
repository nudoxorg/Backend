//! Projects borrowed rust-analyzer HIR facts into the shared canonical fact lane.
//!
//! The projection covers every plane the validated image carries: declaration
//! facts, the recursive declared-type graph, executable signatures with
//! parameter and result facts, oracle-resolved occurrences, and Rustdoc
//! fragments. Emission is two-pass: every module, record, enum, and trait is
//! committed first with its legal diagonal self-nominal, so every nominal,
//! type-child, and product target that follows is a strictly backward fact
//! ordinal. Compound types ride the anonymous type-row pool, so no synthetic
//! carrier declaration is ever invented for `Vec<T>`, `&T`, `[T; N]`, or a
//! function pointer.
//!
//! Lane encoding laws this lowering obeys:
//! - Declarations are emitted declarations-first (type roots, then members in
//!   source order), so every type-record child and nominal target is strictly
//!   backward.
//! - A function is `function(parameter_count, result_count)`; its receiver,
//!   parameters, and result slot are `Parameter` facts pushed immediately
//!   before it, and the function's declared type is the `FunctionPointer` row
//!   over exactly those rows.
//! - A variant is a constructor: its declared type is the `FunctionPointer`
//!   over one anonymous row per field type plus the parent enum's fact
//!   ordinal, with the result flag set. A variant whose field run cannot fit
//!   the bounded type-child lane keeps the parent enum's plain nominal row.
//! - Anonymous type rows are leaf rows only (zero pooled children). Every
//!   compound position is hosted either by the fact it declares or by a
//!   backward `Parameter` carrier fact, because the pooled admission lane
//!   re-lays pooled children per row and cannot soundly address child-bearing
//!   anonymous rows. Nested compounds therefore cost one carrier fact per
//!   nesting level, never a fabricated declaration shape.
//! - Foreign named types (`std` and every other dependency) that
//!   rust-analyzer resolves become external `Nominal` rows bound to their
//!   defining crate module and displayed by their exact written spelling;
//!   an applied foreign type (`HashMap<String, u64>`) still commits its
//!   application structure over those leaf rows. The lane borrows source
//!   bytes only, so no rust-analyzer-rendered qualified path can be
//!   manufactured into any text cell; a foreign position without a written
//!   spelling stays the gap row, and a foreign trait bound keeps its written
//!   spelling as an `Unknown(UnresolvedExternal)` row. Positions the walk cannot prove stay `Unknown` rows with the exact
//!   closed reason, never an invented shape.
//! - Occurrences resolve through rust-analyzer: method calls, field accesses,
//!   and paths with a static target land `Local` or foreign at oracle
//!   confidence; positions the oracle could not resolve stay at syntactic
//!   confidence, and every span is relative to the innermost owning
//!   declaration.
//! - Macro invocation spellings travel in each owning declaration's Rust
//!   extension row. A `macro_rules!` definition commits its own closed
//!   `Macro` row — leaf product, honest unannotated record — exactly as the
//!   C lane commits its macros, so a crate whose only written declaration is
//!   a macro still lowers instead of rejecting as empty. Declarations that
//!   exist only through macro expansion are enumerated from the HIR module
//!   scope, kept only when their `definition_origin` projects into this
//!   source, and emitted exactly like written declarations with their
//!   projected spans.
//! - Associated-type projections and inferred array lengths have no row the
//!   HIR walk can prove, so they fold to the honest `Unknown(OracleGap)`
//!   record instead of a fabricated shape.
//! - Items `#[cfg]`-gated out of the crate never reach HIR, so they never
//!   become facts; the lane proves only what rust-analyzer proved.
//!
//! Fold accounting (the typed reason is part of each bounded lane):
//! | bound | typed reason | behavior at overflow |
//! |---|---|---|
//! | `MAX_TYPE_DEPTH` (16) | `TruncatedAtDepthLimit` | retain an unknown row with that reason |
//! | `MAX_COMPOUND_CHILDREN` (the type-child lane, 255) | `NoIrRepresentation`, or enclosing `OracleGap` without a written shape | retain spelling when available; otherwise retain the gap row |
//! | `MAX_DEDUPED_FOREIGN_ROWS` (512) | `NoSupportedDeclaration` | a distinct foreign spelling beyond the cap rejects exactly |
//! | `TUPLE_FIELD_NAMES` (256 positional spellings) | positional-name fold | a tuple field whose index has no spelling is not materialized |
//! | computed rows (`MAX_COMPUTED_TYPE_ROWS`, 32768) | `ComputedRowCapacity` | a proven let-initializer or method-call result type beyond the cap is dropped, never truncated into a fabricated row |

use std::{collections::HashMap, vec::Vec};

use backend_frontend_rust::legacy::{
    ByteSpan, ModuleDeclaration, RustAnalysisControl, RustAuthority, RustAuthorityError,
    RustDeclaration, RustDefinition, RustFeatureControl, RustFieldAccess, RustProject,
    SemanticKind, SourceByteLimit, SourceOrigin, ra_ap_hir, ra_ap_ide_db, ra_ap_syntax,
};
use backend_semantic::ir::{
    AtomListId, DocFragmentInput, DocLinkTarget, EntityId, EntityKind, ExternalEntityRef,
    ExternalFragmentId, ForeignKey, ForeignOrigin,
    ListSpan, NominalRef, Occurrence, OccurrenceConfidence, OccurrenceTarget, PrimitiveShape,
    ProductChildRole, ReferenceKind, RelSpan, RustFacts, RustOwnership, SemanticProductConstructor,
    SemanticTypeRecord, SemanticTypeTag, TypeParameterListId, TypeReason, TypeWidth,
};
use ra_ap_syntax::{
    AstNode, SyntaxNode,
    ast::{
        self, HasAttrs, HasGenericArgs, HasGenericParams, HasName, HasTypeBounds, HasVisibility,
    },
};
use sha2::{Digest, Sha256};

use crate::driver::{
    lower::{
        EmissionExtension, FactSet, LEAF_PRODUCT, MAX_TYPE_CHILDREN, SemanticFact,
        StagedSourceSpan, portable_admission, portable_count, push_fact,
    },
    types::{CompileControl, FactFault, LoweringUnsupported},
};

mod emit;

/// The bit a `Primitive(Reference)` payload1 cell reserves for mutability.
const REFERENCE_MUTABLE_FLAG: u32 = SemanticTypeRecord::INTEGER_SIGNED_FLAG;
/// Bit offset of the integer width cell above the signedness bit.
const INTEGER_WIDTH_SHIFT: u32 = 1;
/// Bound of the recursive declared-type walk; deeper positions fold to the
/// lane's exact `TruncatedAtDepthLimit` reason instead of unbounded recursion.
const MAX_TYPE_DEPTH: usize = 16;
/// Maximum children of one compound row. This is the shared type-child lane,
/// not a separate Rust fold. A tuple, callable, or application inside the
/// lane keeps every child; only a row past the lane still folds.
const MAX_COMPOUND_CHILDREN: usize = super::MAX_TYPE_CHILDREN;
/// Maximum entries of the anonymous foreign-leaf row dedup table. Measured
/// against the real corpus demand: a fixture crate root re-exports whole
/// dependency surfaces (`itertools`'s written re-export list alone names
/// more distinct foreign spellings than the founding 64-entry bound), so the
/// table is sized an order of magnitude above the largest measured lane.
const MAX_DEDUPED_FOREIGN_ROWS: usize = 512;
/// Maximum entries of the compound-type carrier dedup table. A compound
/// spelling (`[u8]`, `&'a T`, `Vec<u8>`) lowers to the same carrier every
/// time it is written, so a second occurrence must reuse the first carrier
/// rather than mint a byte-identical twin; beyond this many distinct compound
/// spellings the pass rejects exactly instead of growing without bound.
const MAX_DEDUPED_CARRIER_ROWS: usize = 2048;
/// Ecosystem namespace of every foreign key this authority emits.
const CARGO_ECOSYSTEM: &str = "cargo";
/// The receiver's written name; every Rust receiver spells exactly this.
const SELF_NAME: &[u8] = b"self";
/// Fallback binding name for a parameter whose pattern spells no identifier.
const PARAM_FALLBACK_NAME: &[u8] = b"param";

/// Exact direct-authority rejection while rust-analyzer HIR is borrowed.
///
/// The closed terminal is fixed by the shared driver failure match: authority
/// faults keep their full typed cause, and canonical admission rejections
/// keep their bounded typed cause (`LoweringUnsupported::FactRejected`)
/// through the authority's frozen `Admission` boundary.
#[derive(Debug)]
pub(crate) enum RustCollectError {
    /// rust-analyzer could not open, resolve, or query the selected Cargo graph.
    Authority(RustAuthorityError),
    /// Canonical admission rejected one borrowed HIR declaration.
    Lowering(backend_semantic::vocabulary::LoweringUnsupported),
}

/// Runs a non-escaping rust-analyzer transaction and emits the complete
/// borrowed semantic plane.
///
/// The selected Cargo project is caller-owned. This function never derives a
/// project root, invokes rustc, scans source bytes, or substitutes syntax
/// tokens for HIR definitions.
pub(crate) fn collect<'source>(
    project: &RustProject,
    maximum_source_bytes: SourceByteLimit,
    features: RustFeatureControl<'source>,
    control: CompileControl<'_>,
    source: &'source [u8],
    facts: &mut FactSet<'source>,
) -> Result<(), RustCollectError> {
    project
        .analyze_with_features(
            RustAnalysisControl {
                cancelled: control.cancelled,
                maximum_source_bytes,
                deadline: control.deadline,
            },
            features,
            |authority| {
                if authority.source != source {
                    return Err(RustAuthorityError::SourceBinding {
                        expected: source.len(),
                        observed: authority.source.len(),
                    });
                }
                let mut emitter = Emitter::new(&authority, source, facts);
                emitter.run()?;
                // A source whose every written item stayed out of the lane
                // (each behind an unmet `#[cfg]` gate, an unresolved facade
                // re-export, or a `compile_error!` stub) proved by HIR that
                // it has no active declaration: the collected-empty product
                // is its honest parity output, exactly the Go `doc.go` and
                // Clang cfg-gated analogues. Only a written surface with no
                // top-level item at all carries no proof, and stays the
                // lane's exact typed rejection.
                if facts.len() == 0 {
                    let written_items = authority
                        .root
                        .syntax()
                        .children()
                        .any(|child| ast::Item::can_cast(child.kind()));
                    if !written_items {
                        return Err(RustAuthorityError::Admission {
                            cause: LoweringUnsupported::NoSupportedDeclaration,
                        });
                    }
                }
                Ok(())
            },
        )
        .map_err(|cause| match cause {
            RustAuthorityError::Admission { cause } => RustCollectError::Lowering(cause),
            cause => RustCollectError::Authority(cause),
        })
}

/// Admits one fact, retaining the exact typed rejection on failure while
/// preserving the lane ordinal on success.
fn push<'source>(
    facts: &mut FactSet<'source>,
    fact: SemanticFact<'source>,
) -> Result<usize, RustAuthorityError> {
    push_fact(facts, fact).map_err(|cause| RustAuthorityError::Admission {
        cause: backend_semantic::vocabulary::LoweringUnsupported::FactRejected {
            fact: portable_count(cause.fact),
            name_len: portable_count(cause.name_len),
            cause: portable_admission(cause.cause),
        },
    })
}

/// Folds one bounded-lane rejection onto the lane's closed terminal. The
/// shared driver failure match owns the terminal arms and lies outside this
/// module's ownership, so the exact rejected fact stays nameable only at this
/// collapse site; the closed `Admission` terminal retains its typed cause.
fn admission() -> RustAuthorityError {
    RustAuthorityError::Admission {
        cause: LoweringUnsupported::NoSupportedDeclaration,
    }
}

/// Names one parentage staging rejection exactly: the fact ordinal and name
/// length travel with the admission cause so a parentage fault stays
/// diagnosable without consulting emission order.
fn parentage_fault(ordinal: u32, name_len: usize, fault: FactFault) -> RustAuthorityError {
    RustAuthorityError::Admission {
        cause: backend_semantic::vocabulary::LoweringUnsupported::FactRejected {
            fact: portable_count(ordinal as usize),
            name_len: portable_count(name_len),
            cause: portable_admission(fault),
        },
    }
}

/// Refuses generic syntax whose declaration-scoped binding cannot yet be
/// preserved exactly. In particular, a later local trait or a `where`
/// predicate must not be downgraded to an external nominal or duplicated as
/// a second parameter row.
fn unsupported_generic() -> RustAuthorityError {
    RustAuthorityError::Admission {
        cause: LoweringUnsupported::RustGenericParameter,
    }
}

/// Widenes one bounded lane coordinate; unreachable past the lane's fixed
/// bounds, but never silently truncated.
fn coordinate(value: usize) -> Result<u32, RustAuthorityError> {
    u32::try_from(value).map_err(|_| admission())
}

/// One materialized declaration: the HIR definition, its item syntax, its
/// exact name bytes, and its whole-item source span. `expanded` marks
/// declarations whose syntax lives in a macro expansion, so no raw syntax
/// range of it may be addressed in the caller source.
struct Decl<'source> {
    kind: SemanticKind,
    definition: RustDefinition,
    syntax: SyntaxNode,
    /// Exact borrowed name bytes, resolved once at materialization.
    name: &'source [u8],
    span: ByteSpan,
    expanded: bool,
}

/// Canonical positional names of tuple fields, exactly the spellings Rust
/// itself uses for `.0`-style access. The table covers every `u8` index so
/// a tuple struct wider than sixteen fields keeps those fields.
const TUPLE_FIELD_NAME_LIMIT: usize = 256;

const fn tuple_field_name_table() -> (
    [u8; 1024],
    [u16; TUPLE_FIELD_NAME_LIMIT],
    [u16; TUPLE_FIELD_NAME_LIMIT],
) {
    let mut bytes = [0_u8; 1024];
    let mut starts = [0_u16; TUPLE_FIELD_NAME_LIMIT];
    let mut ends = [0_u16; TUPLE_FIELD_NAME_LIMIT];
    let mut at = 0_usize;
    let mut index = 0_usize;
    while index < TUPLE_FIELD_NAME_LIMIT {
        starts[index] = at as u16;
        let mut value = index;
        let mut digits = [0_u8; 3];
        let mut count = 0_usize;
        if value == 0 {
            digits[0] = b'0';
            count = 1;
        } else {
            while value > 0 {
                digits[count] = b'0' + (value % 10) as u8;
                value /= 10;
                count += 1;
            }
        }
        let mut cursor = count;
        while cursor > 0 {
            cursor -= 1;
            bytes[at] = digits[cursor];
            at += 1;
        }
        ends[index] = at as u16;
        index += 1;
    }
    (bytes, starts, ends)
}

const TUPLE_FIELD_NAME_TABLE: (
    [u8; 1024],
    [u16; TUPLE_FIELD_NAME_LIMIT],
    [u16; TUPLE_FIELD_NAME_LIMIT],
) = tuple_field_name_table();

fn tuple_field_name(index: usize) -> Option<&'static [u8]> {
    let (bytes, starts, ends) = &TUPLE_FIELD_NAME_TABLE;
    let start = usize::from(*starts.get(index)?);
    let end = usize::from(*ends.get(index)?);
    bytes.get(start..end)
}

/// One pushed declaration row with the coordinates every later phase needs.
struct Row<'source> {
    ordinal: u32,
    name: &'source [u8],
    span: ByteSpan,
    /// The row's Rust extension facts as pushed, before macros attach.
    extension: RustFacts,
}

/// One macro invocation site with its written spelling and call span.
#[derive(Clone, Copy)]
struct MacroSite<'source> {
    spelling: &'source [u8],
    span: ByteSpan,
    resolved: bool,
}

/// One lowered type position: the lattice record plus the already-interned
/// backward coordinates its record needs as pooled children.
struct Lowered<'source> {
    record: SemanticTypeRecord<'source>,
    children: Vec<u32>,
}

impl<'source> Lowered<'source> {
    /// Wraps one childless lattice record.
    const fn leaf(record: SemanticTypeRecord<'source>) -> Self {
        Self {
            record,
            children: Vec::new(),
        }
    }
}

/// True for the declaration kinds that own a lane type root and therefore
/// must be reserved before any bound may name them.
fn is_type_root(kind: SemanticKind) -> bool {
    matches!(
        kind,
        SemanticKind::Module | SemanticKind::Record | SemanticKind::Enum | SemanticKind::Trait
    )
}

/// True when any written `#[cfg(…)]` (or `#[cfg_attr(…, cfg(…))]`) on one
/// inline module evaluates false under this crate's resolved options.
/// rust-analyzer resolves a disabled module's definition to its enabled
/// same-name sibling (module lookup is by parent and name), so the written
/// walk would otherwise emit a byte-identical twin; the disabled syntax owns
/// no HIR declaration and stays out.
fn module_cfg_disabled(item: &ast::Module, cfg: &ra_ap_hir::CfgOptions) -> bool {
    item.attrs().any(|attr| {
        attr.meta()
            .is_some_and(|meta| meta_cfg_disabled(&meta, cfg))
    })
}

/// Whether one attribute meta disables its item: a `cfg(pred)` whose predicate
/// is provably false, or a `cfg_attr(cond, …)` whose condition is provably true
/// and which applies a disabling meta. An unprovable predicate never disables.
fn meta_cfg_disabled(meta: &ast::Meta, cfg: &ra_ap_hir::CfgOptions) -> bool {
    match meta {
        ast::Meta::CfgMeta(meta) => meta.cfg_predicate().is_some_and(|predicate| {
            cfg.check(&ra_ap_hir::CfgExpr::parse_from_ast(predicate)) == Some(false)
        }),
        ast::Meta::CfgAttrMeta(meta) => {
            let condition = meta.cfg_predicate().is_some_and(|predicate| {
                cfg.check(&ra_ap_hir::CfgExpr::parse_from_ast(predicate)) == Some(true)
            });
            condition && meta.metas().any(|inner| meta_cfg_disabled(&inner, cfg))
        }
        _ => false,
    }
}

/// One `where` predicate bound staged for a declared parameter: the written
/// subject spelling it augments, its exact written bound, and whether a
/// declared parameter claimed it. An unclaimed bound carries its full subject
/// type for the free-predicate lane; a lifetime subject has no type row and
/// stays an exact terminal.
struct WhereBound<'source> {
    subject: &'source [u8],
    ty: Option<ast::Type>,
    bound: ast::TypeBound,
    matched: bool,
}

/// Resolution outcome of one written trait bound at a declaration site.
enum TraitBoundTarget {
    /// Local trait already committed to a lane ordinal.
    Committed(u32),
    /// Trait defined outside this source fragment; the caller keeps its exact
    /// written spelling as an unresolved external row.
    Foreign,
    /// The written bound did not resolve to a trait definition at all.
    Unresolved,
}

/// The two-pass Rust semantic emitter over one borrowed authority.
struct Emitter<'authority, 'analysis, 'source> {
    authority: &'authority RustAuthority<'analysis>,
    database: &'analysis ra_ap_ide_db::RootDatabase,
    source: &'source [u8],
    facts: &'authority mut FactSet<'source>,
    /// Pushed declaration rows in lane order, for owners and doc links.
    rows: Vec<Row<'source>>,
    /// Pushed HIR definition ordinals, for occurrence target resolution.
    definitions: Vec<(ra_ap_hir::ModuleDef, u32)>,
    /// Pushed named-field ordinals, for field-access occurrence resolution.
    fields: Vec<(ra_ap_hir::Field, u32)>,
    /// HIR definitions the written syntax walk already materialized; the
    /// HIR module-scope walk emits only the uncovered remainder.
    covered_defs: Vec<ra_ap_hir::ModuleDef>,
    /// Written-syntax fields already materialized.
    covered_fields: Vec<ra_ap_hir::Field>,
    /// Written-syntax implementations already materialized.
    covered_impls: Vec<ra_ap_hir::Impl>,
    /// Lane ordinal per materialized declaration index.
    ordinals: Vec<Option<u32>>,
    /// Dedup table of interned foreign leaf rows (resolved external
    /// nominals and unresolved unknowns), keyed by spelling and, for a
    /// resolved row, its external authority.
    foreign_rows: Vec<(&'source [u8], Option<ExternalEntityRef>, u32)>,
    /// Dedup table of compound-type carrier facts, keyed by written spelling.
    carrier_rows: Vec<(&'source [u8], u32)>,
    /// Macro invocation sites collected before emission.
    macro_sites: Vec<MacroSite<'source>>,
    /// Committed `(enclosing item span, kind, name)` keys, so Rust's
    /// single-name law keeps later cfg-disjoint twins out instead of letting
    /// them mint byte-identical declaration identities.
    scoped_names: std::collections::HashSet<(Option<(u32, u32)>, u8, Vec<u8>)>,
    /// Declaration rows ordered by source start for logarithmic owner admission.
    owner_order: Vec<usize>,
    /// The declaration ordinal currently hosting a computed (inferred) type
    /// row, set only while [`Self::emit_computed`] walks its sorted
    /// expression list.
    computed_owner: Option<u32>,
}

/// Which pushed-table entry one occurrence's target resolves through.
enum ResolvedTarget {
    /// A module-scoped HIR definition, when the position proved one.
    Definition(Option<ra_ap_hir::ModuleDef>),
    /// A named struct field.
    NamedField(ra_ap_hir::Field),
}

/// Which written position of a callable anchor one child lowers from.
#[derive(Clone, Copy)]
enum CallablePart {
    /// The parameter at this position.
    Parameter(usize),
    /// The return type.
    Return,
}

/// Maps one resolution state onto the confidence lattice: a macro the oracle
/// resolved is oracle tier, an unresolved spelling stays syntactic.
const fn occurrence_confidence(resolved: bool) -> OccurrenceConfidence {
    if resolved {
        OccurrenceConfidence::Oracle
    } else {
        OccurrenceConfidence::Syntactic
    }
}

/// Extracts the written type of one variant field at a position, from the
/// variant's tuple or record field list.
fn variant_field_anchor(variant: &ast::Variant, position: usize) -> Option<ast::Type> {
    match variant.field_list()? {
        ast::FieldList::RecordFieldList(list) => {
            list.fields().nth(position).and_then(|field| field.ty())
        }
        ast::FieldList::TupleFieldList(list) => {
            list.fields().nth(position).and_then(|field| field.ty())
        }
    }
}

/// Extracts the written syntax anchor of one binding declaration.
fn written_binding_type(syntax: &SyntaxNode) -> Option<ast::Type> {
    if let Some(item) = ast::Const::cast(syntax.clone()) {
        return item.ty();
    }
    if let Some(item) = ast::Static::cast(syntax.clone()) {
        return item.ty();
    }
    ast::TypeAlias::cast(syntax.clone()).and_then(|item| item.ty())
}

/// Extracts the written child anchor at one position of a parent anchor,
/// or `None` when the parent shape does not match the HIR position.
fn child_anchor(anchor: Option<&ast::Type>, position: usize) -> Option<ast::Type> {
    type_argument_anchor(anchor, position).or_else(|| match anchor? {
        ast::Type::RefType(reference) => (position == 0).then(|| reference.ty()).flatten(),
        ast::Type::PtrType(pointer) => (position == 0).then(|| pointer.ty()).flatten(),
        ast::Type::ArrayType(array) => (position == 0).then(|| array.ty()).flatten(),
        ast::Type::SliceType(slice) => (position == 0).then(|| slice.ty()).flatten(),
        ast::Type::TupleType(tuple) => tuple.fields().nth(position),
        _ => None,
    })
}

/// Borrows the written `TypeArg` anchor at one generic-argument position.
fn type_argument_anchor(anchor: Option<&ast::Type>, position: usize) -> Option<ast::Type> {
    match generic_argument_at(anchor, position)? {
        ast::GenericArg::TypeArg(type_argument) => type_argument.ty(),
        ast::GenericArg::AssocTypeArg(binding) => binding.ty(),
        _ => None,
    }
}

/// Returns the written generic argument at one position of a path application.
fn generic_argument_at(anchor: Option<&ast::Type>, position: usize) -> Option<ast::GenericArg> {
    let anchor = anchor?;
    let ast::Type::PathType(path_type) = anchor else {
        return None;
    };
    path_type
        .path()?
        .segment()?
        .generic_arg_list()?
        .generic_args()
        .nth(position)
}

/// Counts every written generic argument, including associated bindings,
/// const arguments, and lifetimes.
fn written_generic_argument_count(anchor: Option<&ast::Type>) -> usize {
    let Some(anchor) = anchor else {
        return 0;
    };
    let ast::Type::PathType(path_type) = anchor else {
        return 0;
    };
    path_type
        .path()
        .and_then(|path| path.segment())
        .and_then(|segment| segment.generic_arg_list())
        .map(|arguments| arguments.generic_args().count())
        .unwrap_or(0)
}

/// Borrows the written trait bounds behind one `impl Trait` or `dyn Trait`
/// anchor in source order.
fn written_trait_bounds(anchor: Option<&ast::Type>) -> Vec<ast::TypeBound> {
    match anchor {
        Some(ast::Type::ImplTraitType(impl_trait)) => impl_trait
            .type_bound_list()
            .map(|bounds| bounds.bounds().collect())
            .unwrap_or_default(),
        Some(ast::Type::DynTraitType(dyn_trait)) => dyn_trait
            .type_bound_list()
            .map(|bounds| bounds.bounds().collect())
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// Keeps repeated wildcard parameters distinct while their display name stays `_`.
fn wildcard_param_discriminator(position: usize) -> [u8; 16] {
    let mut hash = Sha256::new();
    hash.update(b"compiler.rust.wildcard-param.v1\0");
    hash.update((position as u64).to_le_bytes());
    let mut discriminator = [0_u8; 16];
    discriminator.copy_from_slice(&hash.finalize()[..16]);
    discriminator
}

/// Extracts the written anchor of one callable parameter or return position.
fn callable_anchor(anchor: Option<&ast::Type>, part: CallablePart) -> Option<ast::Type> {
    let ast::Type::FnPtrType(pointer) = anchor? else {
        return None;
    };
    match part {
        CallablePart::Return => pointer.ret_type().and_then(|ret| ret.ty()),
        CallablePart::Parameter(position) => {
            let params = pointer.param_list()?;
            let mut seen = 0usize;
            for param in params.params() {
                let Some(ty) = param.ty() else {
                    continue;
                };
                if seen == position {
                    return Some(ty);
                }
                seen += 1;
            }
            None
        }
    }
}

/// Maps one HIR receiver access onto the ownership lattice.
const fn receiver_ownership(access: ra_ap_hir::Access) -> RustOwnership {
    match access {
        ra_ap_hir::Access::Shared => RustOwnership::SharedBorrow,
        ra_ap_hir::Access::Exclusive => RustOwnership::MutableBorrow,
        ra_ap_hir::Access::Owned => RustOwnership::Value,
    }
}

/// Maps one parameter binding onto the ownership lattice: borrows keep their
/// shared or mutable cell, by-value bindings of copy types are values, and
/// every other by-value binding moves.
fn parameter_ownership(
    database: &ra_ap_ide_db::RootDatabase,
    semantic: &ra_ap_hir::Type<'_>,
) -> RustOwnership {
    if let Some((_, mutability)) = semantic.as_reference() {
        return match mutability {
            ra_ap_hir::Mutability::Mut => RustOwnership::MutableBorrow,
            ra_ap_hir::Mutability::Shared => RustOwnership::SharedBorrow,
        };
    }
    if semantic.is_copy(database) {
        RustOwnership::Value
    } else {
        RustOwnership::Moved
    }
}

/// Maps one closed declaration kind onto the entity lattice.
const fn entity_kind(kind: SemanticKind) -> Result<EntityKind, RustAuthorityError> {
    match kind {
        SemanticKind::Module => Ok(EntityKind::Module),
        SemanticKind::Function => Ok(EntityKind::Function),
        SemanticKind::Field => Ok(EntityKind::Field),
        SemanticKind::Record => Ok(EntityKind::Record),
        SemanticKind::Enum => Ok(EntityKind::Enum),
        SemanticKind::Trait => Ok(EntityKind::Trait),
        SemanticKind::Implementation => Ok(EntityKind::Implementation),
        SemanticKind::TypeAlias => Ok(EntityKind::Alias),
        SemanticKind::Constant => Ok(EntityKind::Constant),
        SemanticKind::Static => Ok(EntityKind::Static),
        SemanticKind::Variant => Ok(EntityKind::Variant),
        SemanticKind::Macro => Ok(EntityKind::Macro),
        SemanticKind::LocalBinding | SemanticKind::GenericParameter | SemanticKind::Builtin => {
            Err(RustAuthorityError::MissingSemanticFact { fact: kind })
        }
    }
}

/// Maps one closed declaration kind onto its product constructor.
const fn constructor(kind: SemanticKind) -> Result<SemanticProductConstructor, RustAuthorityError> {
    match kind {
        SemanticKind::Function => Ok(SemanticProductConstructor::function(0, 0)),
        SemanticKind::Record
        | SemanticKind::Module
        | SemanticKind::Field
        | SemanticKind::Implementation => Ok(SemanticProductConstructor::PRODUCT),
        SemanticKind::Enum => Ok(SemanticProductConstructor::UNION),
        SemanticKind::Trait => Ok(SemanticProductConstructor::INTERSECTION),
        SemanticKind::TypeAlias
        | SemanticKind::Constant
        | SemanticKind::Static
        | SemanticKind::Variant => Ok(LEAF_PRODUCT),
        SemanticKind::Macro => Ok(LEAF_PRODUCT),
        SemanticKind::LocalBinding | SemanticKind::GenericParameter | SemanticKind::Builtin => {
            Err(RustAuthorityError::MissingSemanticFact { fact: kind })
        }
    }
}

/// Maps one pushed HIR definition onto its `ModuleDef` key.
fn module_def(definition: &RustDefinition) -> Option<ra_ap_hir::ModuleDef> {
    match definition {
        RustDefinition::Field(_) | RustDefinition::Implementation(_) | RustDefinition::Macro(_) => {
            None
        }
        RustDefinition::Variant(variant) => Some(ra_ap_hir::ModuleDef::from(*variant)),
        RustDefinition::Module(module) => Some(ra_ap_hir::ModuleDef::from(*module)),
        RustDefinition::Trait(trait_) => Some(ra_ap_hir::ModuleDef::from(*trait_)),
        RustDefinition::Function(function) => Some(ra_ap_hir::ModuleDef::from(*function)),
        RustDefinition::Record(adt) | RustDefinition::Enum(adt) => {
            Some(ra_ap_hir::ModuleDef::from(*adt))
        }
        RustDefinition::TypeAlias(alias) => Some(ra_ap_hir::ModuleDef::from(*alias)),
        RustDefinition::Constant(constant) => Some(ra_ap_hir::ModuleDef::from(*constant)),
        RustDefinition::Static(static_) => Some(ra_ap_hir::ModuleDef::from(*static_)),
    }
}

/// Categorizes one resolved path reference: imports, type positions, calls,
/// and value uses, by the path's written syntax position and resolution.
fn reference_kind(path: &ast::Path, resolution: &ra_ap_hir::PathResolution) -> ReferenceKind {
    if let Some(parent) = path.syntax().parent()
        && parent.kind() == ra_ap_syntax::SyntaxKind::USE_TREE
    {
        return ReferenceKind::Import;
    }
    match resolution {
        ra_ap_hir::PathResolution::Def(ra_ap_hir::ModuleDef::Function(_)) => {
            if is_call_position(path) {
                ReferenceKind::FunctionCall
            } else {
                ReferenceKind::VariableUse
            }
        }
        ra_ap_hir::PathResolution::Def(
            ra_ap_hir::ModuleDef::Adt(_)
            | ra_ap_hir::ModuleDef::Trait(_)
            | ra_ap_hir::ModuleDef::TypeAlias(_)
            | ra_ap_hir::ModuleDef::BuiltinType(_)
            | ra_ap_hir::ModuleDef::Module(_),
        ) => ReferenceKind::TypeReference,
        _ => ReferenceKind::VariableUse,
    }
}

/// Categorizes one unresolved path reference from its written syntax position
/// alone: an import-tree path stays an import, a call-position path stays a
/// call, and every other position keeps the honest value-use default. The
/// oracle could not prove a target, so the kind names only the site's shape.
fn unresolved_reference_kind(path: &ast::Path) -> ReferenceKind {
    if let Some(parent) = path.syntax().parent()
        && parent.kind() == ra_ap_syntax::SyntaxKind::USE_TREE
    {
        return ReferenceKind::Import;
    }
    if is_call_position(path) {
        ReferenceKind::FunctionCall
    } else {
        ReferenceKind::VariableUse
    }
}

/// True when the path sits in the callee position of a call expression.
fn is_call_position(path: &ast::Path) -> bool {
    let Some(parent) = path.syntax().parent() else {
        return false;
    };
    let Some(ast::Expr::PathExpr(path_expr)) = ast::Expr::cast(parent) else {
        return false;
    };
    let Some(grandparent) = path_expr.syntax().parent() else {
        return false;
    };
    let Some(call) = ast::CallExpr::cast(grandparent) else {
        return false;
    };
    let Some(callee) = call.expr() else {
        return false;
    };
    callee.syntax().text_range() == path_expr.syntax().text_range()
}

/// Extracts the resolved definition of one path, when it names a declaration.
const fn path_definition(resolution: &ra_ap_hir::PathResolution) -> Option<ra_ap_hir::ModuleDef> {
    match resolution {
        ra_ap_hir::PathResolution::Def(module_def) => Some(*module_def),
        _ => None,
    }
}

/// Builds one self-describing foreign key outside every package lineage,
/// carrying the exact written spelling of the reference site.
fn foreign_universe<'source>(
    written: &'source [u8],
) -> Result<OccurrenceTarget<'source>, RustAuthorityError> {
    let path = core::str::from_utf8(written).unwrap_or("");
    let usable = if path.is_empty() { "unresolved" } else { path };
    let key = ForeignKey::new(
        ForeignOrigin::Universe {
            ecosystem: CARGO_ECOSYSTEM,
        },
        usable,
        path,
        None,
    )
    .map_err(|_| admission())?;
    Ok(OccurrenceTarget::Foreign(key))
}

/// Projects one absolute occurrence span onto its owner's span start. The
/// owner filter proved containment, so both subtractions stay in range.
fn relative_span(span: ByteSpan, owner: ByteSpan) -> Result<RelSpan, RustAuthorityError> {
    let start = span.start.checked_sub(owner.start).ok_or_else(admission)?;
    let end = span.end.checked_sub(owner.start).ok_or_else(admission)?;
    RelSpan::new(start, end).map_err(|_| admission())
}

/// One primitive row with the frozen `repr(u32)` shape discriminant.
#[expect(
    clippy::as_conversions,
    reason = "the lattice freezes PrimitiveShape as a repr(u32) wire discriminant"
)]
fn primitive_record(shape: PrimitiveShape, payload1: u32) -> SemanticTypeRecord<'static> {
    SemanticTypeRecord {
        tag: SemanticTypeTag::Primitive,
        payload0: shape as u32,
        payload1,
        text: None,
        text2: None,
        nominal: None,
        children: ListSpan::new(0, 0),
    }
}

/// One exact-width integer row, packed per the frozen integer-cell encoding.
fn integer_record(width: TypeWidth, signed: bool) -> SemanticTypeRecord<'static> {
    let mut payload1 = width.to_cell() << INTEGER_WIDTH_SHIFT;
    if signed {
        payload1 |= SemanticTypeRecord::INTEGER_SIGNED_FLAG;
    }
    primitive_record(PrimitiveShape::Integer, payload1)
}

/// Maps one HIR builtin onto its exact scalar row by its exact spelling,
/// because the HIR facade wraps the builtin without width introspection.
fn builtin_record(builtin: ra_ap_hir::BuiltinType) -> SemanticTypeRecord<'static> {
    let name = builtin.name();
    match name.as_str() {
        "bool" => primitive_record(PrimitiveShape::Bool, 0),
        "char" => primitive_record(
            PrimitiveShape::UnicodeScalar,
            TypeWidth::Fixed(32).to_cell(),
        ),
        "str" => primitive_record(PrimitiveShape::Str, 0),
        "isize" => primitive_record(PrimitiveShape::NativeSignedInteger, 0),
        "usize" => primitive_record(PrimitiveShape::NativeUnsignedInteger, 0),
        "i8" => integer_record(TypeWidth::Fixed(8), true),
        "i16" => integer_record(TypeWidth::Fixed(16), true),
        "i32" => integer_record(TypeWidth::Fixed(32), true),
        "i64" => integer_record(TypeWidth::Fixed(64), true),
        "i128" => integer_record(TypeWidth::Fixed(128), true),
        "u8" => integer_record(TypeWidth::Fixed(8), false),
        "u16" => integer_record(TypeWidth::Fixed(16), false),
        "u32" => integer_record(TypeWidth::Fixed(32), false),
        "u64" => integer_record(TypeWidth::Fixed(64), false),
        "u128" => integer_record(TypeWidth::Fixed(128), false),
        "f16" => primitive_record(PrimitiveShape::Float, TypeWidth::Fixed(16).to_cell()),
        "f32" => primitive_record(PrimitiveShape::Float, TypeWidth::Fixed(32).to_cell()),
        "f64" => primitive_record(PrimitiveShape::Float, TypeWidth::Fixed(64).to_cell()),
        "f128" => primitive_record(PrimitiveShape::Float, TypeWidth::Fixed(128).to_cell()),
        _ => unknown_record(TypeReason::OracleGap, None),
    }
}

/// One honest unknown row; the text cell is present exactly when the reason
/// demands its spelling, per the closed lattice's cell laws.
#[expect(
    clippy::as_conversions,
    reason = "the lattice freezes TypeReason as a repr(u32) wire discriminant"
)]
fn unknown_record(reason: TypeReason, text: Option<&[u8]>) -> SemanticTypeRecord<'_> {
    SemanticTypeRecord {
        tag: SemanticTypeTag::Unknown,
        payload0: reason as u32,
        payload1: 0,
        text,
        text2: None,
        nominal: None,
        children: ListSpan::new(0, 0),
    }
}

/// Splits one doc line into prose runs and intra-doc links; a link target
/// naming a pushed declaration resolves in [`Emitter::push_doc_lines`].
fn split_links<'source>(line: &'source [u8], fragments: &mut Vec<DocFragmentInput<'source>>) {
    let mut cursor = 0usize;
    while let Some(at) = find(line, b"[`", cursor) {
        let Some(open) = at.checked_add(2) else {
            break;
        };
        let Some(close) = find(line, b"`]", open).and_then(|close| close.checked_add(2)) else {
            break;
        };
        if at > cursor
            && let Some(prose) = line.get(cursor..at)
        {
            fragments.push(DocFragmentInput::Text(prose));
        }
        if let Some(name) = line.get(open..close - 2).filter(|name| !name.is_empty()) {
            fragments.push(DocFragmentInput::Link {
                label: name,
                target: DocLinkTarget::Foreign {
                    ecosystem: CARGO_ECOSYSTEM.as_bytes(),
                    path: name,
                },
            });
        }
        cursor = close;
    }
    if let Some(rest) = line.get(cursor..)
        && !rest.is_empty()
    {
        fragments.push(DocFragmentInput::Text(rest));
    }
}

/// Rewrites a doc link whose target names a pushed declaration to the exact
/// local entity row; every other target keeps its written spelling.
fn resolve_link<'source>(
    fragment: DocFragmentInput<'source>,
    rows: &[(&'source [u8], u32)],
) -> DocFragmentInput<'source> {
    let DocFragmentInput::Link { label, target } = fragment else {
        return fragment;
    };
    let DocLinkTarget::Foreign { path, .. } = target else {
        return DocFragmentInput::Link { label, target };
    };
    match rows.iter().find(|(known, _)| *known == path) {
        Some((_, ordinal)) => DocFragmentInput::Link {
            label,
            target: DocLinkTarget::Local(EntityId::new(*ordinal)),
        },
        None => DocFragmentInput::Link { label, target },
    }
}

/// Finds the first position of `needle` in `haystack` at or after `from`.
fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    let Some(last) = haystack.len().checked_sub(needle.len()) else {
        return None;
    };
    let mut at = from;
    while at <= last {
        let Some(window) = haystack.get(at..at.checked_add(needle.len())?) else {
            return None;
        };
        if window == needle {
            return Some(at);
        }
        at += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::lower::{AdmissionFault, admit};
    use backend_frontend_rust::legacy::{RustAuthorityError, RustProject, RustToolchain};
    use backend_semantic::ir::{
        DocFactFault, DocFragmentInput, DocLinkTarget, EntityKind, FragmentError, FragmentView,
        Occurrence, OccurrenceConfidence, OccurrenceFault, ReferenceKind, SemanticReader,
        SourceIdentity, TypeFactFault,
    };
    use backend_semantic::vocabulary::{
        CompileRecipeFact, LanguageProfile, NativeTool, RustEdition, Stage,
    };
    use backend_version::{ContentId, SourceFactDomain, ToolchainDomain};
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicBool, AtomicU64, Ordering},
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };
    use thiserror::Error;

    /// Separates concurrently executing fixture roots created in one process.
    static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    /// Typed fixture failure; every assertion failure names what was missing.
    #[derive(Debug, Error)]
    enum TestError {
        #[error("fixture setup failed: {operation}: {source}")]
        Io {
            operation: &'static str,
            #[source]
            source: std::io::Error,
        },
        #[error(transparent)]
        Authority(#[from] RustAuthorityError),
        #[error("Rust authority collection failed: {0:?}")]
        Collection(RustCollectError),
        #[error("lane admission rejected the fact set: {0:?}")]
        Admission(AdmissionFault),
        #[error("owned image build rejected the fact set: {0:?}")]
        Build(#[from] backend_semantic::ir::BuildError),
        #[error("fragment validation rejected the bytes: {0:?}")]
        Validate(#[from] FragmentError),
        #[error("type fact cursor rejected: {0:?}")]
        TypeFact(#[from] TypeFactFault),
        #[error("occurrence cursor rejected: {0:?}")]
        Occurrence(#[from] OccurrenceFault),
        #[error("documentation cursor rejected: {0:?}")]
        Doc(#[from] DocFactFault),
        #[error("fixture scalar conversion failed")]
        Scalar,
        #[error("expected {0}")]
        Missing(&'static str),
        #[error("committed bytes changed")]
        Tail,
    }

    impl From<std::num::TryFromIntError> for TestError {
        fn from(_: std::num::TryFromIntError) -> Self {
            Self::Scalar
        }
    }

    /// One temporary Cargo fixture root, removed on drop.
    struct FixtureRoot(PathBuf);

    impl Drop for FixtureRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Lowers one fixture crate root through the complete Rust lane and
    /// returns its validated compact fragment, proving the untouched output
    /// tail stayed unchanged.
    fn lower(source: &str) -> Result<FragmentView<'static>, TestError> {
        let bytes = lower_bytes(source)?;
        let leaked: &'static [u8] = Box::leak(bytes.into_boxed_slice());
        FragmentView::validate(leaked).map_err(TestError::from)
    }

    /// Lowers one fixture crate root into committed fragment bytes.
    fn lower_bytes(source: &str) -> Result<Vec<u8>, TestError> {
        let (facts, identity, recipe) = collected(source)?;
        let mut output = vec![0xa5_u8; 65_536];
        let length = admit(&facts, identity, recipe, recipe.profile, &mut output)
            .map_err(TestError::Admission)?
            .len();
        if !output[length..].iter().all(|byte| *byte == 0xa5) {
            return Err(TestError::Tail);
        }
        output.truncate(length);
        Ok(output)
    }

    /// Builds the owned semantic image for one fixture source, so assertions
    /// can read the authority planes (entity provenance spans, absolute link
    /// occurrence sites) the compact fragment does not carry.
    fn owned_ir(source: &str) -> Result<backend_semantic::ir::Ir, TestError> {
        let (facts, identity, recipe) = collected(source)?;
        facts
            .build_ir(
                LanguageProfile::Rust(RustEdition::Rust2024),
                identity,
                recipe,
                crate::driver::types::DeclarationScope::fixture(),
            )
            .map_err(TestError::Build)
    }

    /// Stages one fixture crate root through the complete Rust lane and
    /// returns its admitted fact set with the identity and recipe that admit
    /// it. The fixture root is removed before returning; the collected facts
    /// borrow only the caller's source.
    fn collected<'source>(
        source: &'source str,
    ) -> Result<(FactSet<'source>, SourceIdentity, CompileRecipeFact), TestError> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| TestError::Io {
                operation: "clock",
                source: std::io::Error::other(error),
            })?
            .as_nanos();
        let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let root =
            std::env::temp_dir().join(format!("nudox-rust-lane-{nonce}-{pid}-{sequence}"));
        fs::create_dir_all(root.join("src")).map_err(|source| TestError::Io {
            operation: "create fixture",
            source,
        })?;
        let guard = FixtureRoot(root.clone());
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"lane_fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .map_err(|source| TestError::Io {
            operation: "write manifest",
            source,
        })?;
        fs::write(root.join("src/lib.rs"), source).map_err(|source| TestError::Io {
            operation: "write crate root",
            source,
        })?;
        let toolchain = RustToolchain::discover(rustc_path()).map_err(RustAuthorityError::from)?;
        let project = RustProject::open(&root, &toolchain, RustEdition::Rust2024)
            .map_err(RustAuthorityError::from)?;
        let cancelled = AtomicBool::new(false);
        let mut facts = FactSet::new();
        collect(
            &project,
            SourceByteLimit::from(u32::MAX),
            RustFeatureControl::default(),
            CompileControl {
                deadline: Instant::now() + Duration::from_secs(180),
                cancelled: &cancelled,
            },
            source.as_bytes(),
            &mut facts,
        )
        .map_err(TestError::Collection)?;
        let identity = SourceIdentity {
            identity: ContentId::<SourceFactDomain>::from_canonical_bytes(source.as_bytes()),
            byte_len: u32::try_from(source.len())?,
        };
        let recipe = CompileRecipeFact::derive(
            LanguageProfile::Rust(RustEdition::Rust2024),
            Stage::LowerIr,
            NativeTool::Rustc,
            ContentId::<SourceFactDomain>::from_canonical_bytes(source.as_bytes()),
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"rust-hir-lane-fixture"),
        );
        drop(guard);
        Ok((facts, identity, recipe))
    }

    fn rustc_path() -> PathBuf {
        std::env::var_os("RUSTC").map_or_else(|| PathBuf::from("rustc"), PathBuf::from)
    }

    /// Decodes one validated fragment's type rows into owned snapshots.
    fn rows<'fragment>(
        view: &'fragment FragmentView<'fragment>,
    ) -> Result<Vec<backend_semantic::ir::DecodedTypeFact<'fragment>>, TestError> {
        view.type_facts()
            .ok_or(TestError::Missing("type facts"))?
            .map(|fact| fact.map_err(TestError::from))
            .collect()
    }

    /// Finds the declared type row bound to one canonical entity. Entity and
    /// type rows are independently canonicalized, so their ordinals are not
    /// interchangeable.
    fn row_for_entity<'fragment>(
        view: &'fragment FragmentView<'fragment>,
        entity: u32,
    ) -> Result<backend_semantic::ir::DecodedTypeFact<'fragment>, TestError> {
        rows(view)?
            .into_iter()
            .filter(|row| row.owner.raw == entity)
            .next_back()
            .ok_or(TestError::Missing("entity type row"))
    }

    /// Resolves one record's ordered local type children after validation.
    fn local_type_children(
        view: &FragmentView<'_>,
        record: backend_semantic::ir::SemanticTypeRecord<'_>,
    ) -> Result<Vec<backend_semantic::ir::TypeId>, TestError> {
        let end = record
            .children
            .start
            .checked_add(record.children.length)
            .ok_or(TestError::Scalar)?;
        view.type_facts()
            .ok_or(TestError::Missing("type facts"))?
            .children()?
            .filter_map(|child| match child {
                Ok(child) if child.ordinal >= record.children.start && child.ordinal < end => {
                    match child.child.target {
                        backend_semantic::ir::TypeChildTarget::Type(
                            backend_semantic::ir::TypeRef::Local(target),
                        ) => Some(Ok(target)),
                        backend_semantic::ir::TypeChildTarget::Text
                        | backend_semantic::ir::TypeChildTarget::Type(
                            backend_semantic::ir::TypeRef::External(_),
                        ) => None,
                    }
                }
                Ok(_) => None,
                Err(error) => Some(Err(TestError::from(error))),
            })
            .collect()
    }

    /// Returns the canonical name/kind facts for one entity coordinate.
    fn entity_fact<'fragment>(
        view: &'fragment FragmentView<'fragment>,
        target: u32,
    ) -> Result<(&'fragment [u8], EntityKind), TestError> {
        let entity = view
            .entities()
            .find(|entity| entity.entity.raw == target)
            .ok_or(TestError::Missing("target entity"))?;
        let atom = view
            .atoms()
            .nth(usize::try_from(entity.name.raw)?)
            .ok_or(TestError::Missing("target entity atom"))?;
        Ok((atom.bytes, entity.kind))
    }

    /// Finds the fact ordinal whose entity name and kind match.
    fn fact_of(view: &FragmentView<'_>, name: &[u8], kind: EntityKind) -> Result<u32, TestError> {
        for entity in view.entities() {
            if entity.kind != kind {
                continue;
            }
            let atom = usize::try_from(entity.name.raw)?;
            let Some(atom) = view.atoms().nth(atom) else {
                return Err(TestError::Missing("entity atom"));
            };
            if atom.bytes == name {
                return Ok(entity.entity.raw);
            }
        }
        Err(TestError::Missing("entity by name"))
    }

    /// Decodes the Rust extension row bound to one fact, if any.
    fn rust_row(view: &FragmentView<'_>, fact_ordinal: u32) -> Result<Option<[u32; 4]>, TestError> {
        let payload = view
            .language_extension_payload()
            .ok_or(TestError::Missing("extension section"))?;
        let word = |at: usize| -> Result<u32, TestError> {
            let raw = payload
                .get(at..at + 4)
                .ok_or(TestError::Missing("extension word"))?;
            let bytes: [u8; 4] = raw.try_into().map_err(|_| TestError::Scalar)?;
            Ok(u32::from_le_bytes(bytes))
        };
        let directory = 16 + 20 * 3;
        let row_count = word(directory + 4)?;
        let fact_count = word(directory + 8)?;
        let base = usize::try_from(word(directory + 12)?)?;
        if fact_count == 0 {
            return Ok(None);
        }
        let mut slot = None;
        for row in 0..row_count {
            if word(base + usize::try_from(row * 4)?)? == fact_ordinal {
                slot = Some(row);
            }
        }
        let Some(slot) = slot else {
            return Ok(None);
        };
        let facts_at = base + usize::try_from(row_count * 4)?;
        let mut words = [0_u32; 4];
        for (index, word_at) in words.iter_mut().enumerate() {
            *word_at = word(facts_at + usize::try_from(slot * 24)? + index * 4)?;
        }
        Ok(Some(words))
    }

    /// Collects every decoded occurrence.
    fn occurrences<'fragment>(
        view: &'fragment FragmentView<'fragment>,
    ) -> Result<Vec<(u32, Occurrence<'fragment>)>, TestError> {
        view.occurrences()
            .ok_or(TestError::Missing("occurrences"))?
            .map(|fact| {
                fact.map_err(TestError::from)
                    .map(|fact| (fact.owner.raw, fact.occurrence))
            })
            .collect()
    }

    /// The committed type rows of one fixture, shared by the cell assertions.
    fn integer_cells() -> Result<(), TestError> {
        let view = lower(
            "pub struct Scalars {\n    pub a: u8,\n    pub b: i128,\n    pub c: usize,\n    pub d: f32,\n    pub e: bool,\n}\n",
        )?;
        let rows = rows(&view)?;
        let u8_row = &rows[1];
        if u8_row.record.tag != SemanticTypeTag::Primitive
            || u8_row.record.payload0 != PrimitiveShape::Integer as u32
            || u8_row.record.payload1 != (8 << INTEGER_WIDTH_SHIFT)
        {
            return Err(TestError::Missing("u8 width and signedness cells"));
        }
        let i128_row = &rows[2];
        if i128_row.record.payload1
            != (128 << INTEGER_WIDTH_SHIFT) | SemanticTypeRecord::INTEGER_SIGNED_FLAG
        {
            return Err(TestError::Missing("i128 width and signedness cells"));
        }
        let usize_row = &rows[3];
        if usize_row.record.payload0 != PrimitiveShape::NativeUnsignedInteger as u32
            || usize_row.record.payload1 != 0
        {
            return Err(TestError::Missing("usize native unsigned role"));
        }
        let f32_row = &rows[4];
        if f32_row.record.payload0 != PrimitiveShape::Float as u32
            || f32_row.record.payload1 != backend_semantic::ir::TypeWidth::Fixed(32).to_cell()
        {
            return Err(TestError::Missing("f32 width cell"));
        }
        let bool_row = &rows[5];
        if bool_row.record.payload0 != PrimitiveShape::Bool as u32 || bool_row.record.payload1 != 0
        {
            return Err(TestError::Missing("bool shape cell"));
        }
        Ok(())
    }

    /// Exact scalar cells: every integer commits its exact width and
    /// signedness, floats their width, and booleans their shape.
    #[test]
    fn scalar_fields_commit_exact_width_signedness_and_shape_cells() -> Result<(), TestError> {
        integer_cells()
    }

    /// A recursive local nominal rides its own fact ordinal and an applied
    /// foreign type commits its application structure over external nominal
    /// leaf rows, with nested compounds hosted by backward carrier facts.
    #[test]
    fn recursive_nominal_and_foreign_application_commit_their_structures() -> Result<(), TestError>
    {
        let view = lower(
            "pub struct Node {\n    pub next: Option<Box<Node>>,\n    pub name: String,\n}\n",
        )?;
        let node_ordinal = fact_of(&view, b"Node", EntityKind::Record)?;
        let rows = rows(&view)?;
        let node_row = row_for_entity(&view, node_ordinal)?;
        if node_row.record.tag != SemanticTypeTag::Nominal
            || node_row.record.nominal
                != Some(NominalRef::Local(backend_semantic::ir::EntityId::new(
                    node_ordinal,
                )))
        {
            return Err(TestError::Missing("recursive self nominal"));
        }
        let name_ordinal = fact_of(&view, b"name", EntityKind::Field)?;
        let name_row = row_for_entity(&view, name_ordinal)?;
        // rust-analyzer resolves `String`, so it is an external nominal over
        // its defining crate module, displayed by its written spelling.
        if name_row.record.tag != SemanticTypeTag::Nominal
            || !matches!(name_row.record.nominal, Some(NominalRef::External(_)))
            || name_row.record.text != Some(b"String".as_slice())
        {
            return Err(TestError::Missing("foreign String external nominal"));
        }
        let next_ordinal = fact_of(&view, b"next", EntityKind::Field)?;
        let next_row = row_for_entity(&view, next_ordinal)?;
        if next_row.record.tag != SemanticTypeTag::Apply || next_row.record.children.length != 2 {
            return Err(TestError::Missing("foreign generic application structure"));
        }
        // The nested `Box<Node>` compound remains a structured child row;
        // its type coordinate is independent of entity canonical order.
        let nested_application = local_type_children(&view, next_row.record)?
            .into_iter()
            .filter_map(|target| rows.get(target.index()))
            .any(|row| row.record.tag == SemanticTypeTag::Apply && row.record.children.length == 2);
        if !nested_application {
            return Err(TestError::Missing("nested compound carrier fact"));
        }
        Ok(())
    }

    /// Every function commits its receiver, parameters, and result as
    /// `Parameter` facts behind a `function(p, r)` product, and receivers
    /// carry their exact ownership cell.
    #[test]
    fn function_signatures_commit_carriers_and_receiver_ownership() -> Result<(), TestError> {
        let view = lower(
            "pub struct Cafe;\n\nimpl Cafe {\n    pub fn brew(&self, shots: u8) -> u8 { shots }\n    pub fn stir(&mut self) {}\n}\n\npub fn serve(value: String) {}\n",
        )?;
        let brew = fact_of(&view, b"brew", EntityKind::Function)?;
        let rows = rows(&view)?;
        let brew_row = row_for_entity(&view, brew)?;
        let brew_children = local_type_children(&view, brew_row.record)?;
        let [receiver_type, shots_type, result_type] = brew_children.as_slice() else {
            return Err(TestError::Missing(
                "brew function pointer over three carriers",
            ));
        };
        let receiver = &rows[receiver_type.index()];
        let shots = &rows[shots_type.index()];
        let result = &rows[result_type.index()];
        let receiver_referents = local_type_children(&view, receiver.record)?;
        let receiver_referent = receiver_referents
            .first()
            .and_then(|target| rows.get(target.index()))
            .ok_or(TestError::Missing("self receiver referent"))?;
        let cafe = fact_of(&view, b"Cafe", EntityKind::Record)?;
        if receiver.record.tag != SemanticTypeTag::Primitive
            || receiver.record.payload0 != PrimitiveShape::Reference as u32
            || receiver.record.children.length != 1
            || receiver_referent.record.tag != SemanticTypeTag::Nominal
            || receiver_referent.record.nominal
                != Some(NominalRef::Local(backend_semantic::ir::EntityId::new(cafe)))
        {
            return Err(TestError::Missing("self receiver type"));
        }
        if brew_row.record.tag != SemanticTypeTag::FunctionPointer
            || brew_row.record.payload1 != SemanticTypeRecord::FUNCTION_RESULT_COUNT_ONE
            || brew_row.record.children.length != 3
        {
            return Err(TestError::Missing(
                "brew function pointer over three carriers",
            ));
        }
        if shots.record.tag != SemanticTypeTag::Primitive {
            return Err(TestError::Missing("u8 parameter carrier row"));
        }
        if result.record.children.length != 0 {
            return Err(TestError::Missing("u8 result carrier row"));
        }
        // Ownership cells through the Rust extension plane.
        let brew_receiver =
            rust_row(&view, receiver.owner.raw)?.ok_or(TestError::Missing("receiver extension"))?;
        if brew_receiver[0] != 1 {
            return Err(TestError::Missing("shared-borrow receiver ownership"));
        }
        let stir = fact_of(&view, b"stir", EntityKind::Function)?;
        let stir_row = row_for_entity(&view, stir)?;
        let stir_children = local_type_children(&view, stir_row.record)?;
        let stir_receiver_owner = stir_children
            .first()
            .and_then(|target| rows.get(target.index()))
            .ok_or(TestError::Missing("stir receiver type"))?
            .owner
            .raw;
        let stir_receiver = rust_row(&view, stir_receiver_owner)?
            .ok_or(TestError::Missing("stir receiver extension"))?;
        if stir_receiver[0] != 2 {
            return Err(TestError::Missing("mutable-borrow receiver ownership"));
        }
        let serve = fact_of(&view, b"serve", EntityKind::Function)?;
        let serve_row = row_for_entity(&view, serve)?;
        let serve_children = local_type_children(&view, serve_row.record)?;
        let moved_owner = serve_children
            .first()
            .and_then(|target| rows.get(target.index()))
            .ok_or(TestError::Missing("moved parameter type"))?
            .owner
            .raw;
        let moved =
            rust_row(&view, moved_owner)?.ok_or(TestError::Missing("moved parameter extension"))?;
        if moved[0] != 3 {
            return Err(TestError::Missing("moved by-value ownership"));
        }
        Ok(())
    }

    /// Enum variants are constructors: unit variants commit a zero-parameter
    /// function pointer to the parent enum, tuple and record variants commit
    /// one child per field plus the result.
    #[test]
    fn variants_commit_constructor_function_pointers_over_their_fields() -> Result<(), TestError> {
        let view = lower(
            "pub enum Event {\n    Quit,\n    Message(String),\n    Move { x: i32, y: i32 },\n}\n",
        )?;
        let event = fact_of(&view, b"Event", EntityKind::Enum)?;
        let quit = fact_of(&view, b"Quit", EntityKind::Variant)?;
        let message = fact_of(&view, b"Message", EntityKind::Variant)?;
        let mv = fact_of(&view, b"Move", EntityKind::Variant)?;
        let _ = event;
        let quit_row = row_for_entity(&view, quit)?;
        if quit_row.record.tag != SemanticTypeTag::FunctionPointer
            || quit_row.record.payload1 != SemanticTypeRecord::FUNCTION_RESULT_COUNT_ONE
            || quit_row.record.children.length != 1
        {
            return Err(TestError::Missing("unit variant constructor row"));
        }
        let message_row = row_for_entity(&view, message)?;
        if message_row.record.children.length != 2 {
            return Err(TestError::Missing("tuple variant constructor row"));
        }
        let move_row = row_for_entity(&view, mv)?;
        if move_row.record.children.length != 3 {
            return Err(TestError::Missing("record variant constructor row"));
        }
        Ok(())
    }

    /// An impl block links its trait: the trait path in the impl header is a
    /// local oracle-resolved type reference owned by the implementation, and
    /// a method call through the impl resolves to the local method.
    #[test]
    fn trait_impls_link_the_trait_and_method_as_local_occurrences() -> Result<(), TestError> {
        let view = lower(
            "pub trait Service {\n    fn run(&self);\n}\n\npub struct Worker;\n\nimpl Service for Worker {\n    fn run(&self) {}\n}\n\npub fn drive(worker: &Worker) {\n    worker.run();\n}\n",
        )?;
        let service = fact_of(&view, b"Service", EntityKind::Trait)?;
        let occurrences = occurrences(&view)?;
        let trait_edge = occurrences
            .iter()
            .find(|(_, occurrence)| occurrence.kind == ReferenceKind::TypeReference)
            .ok_or(TestError::Missing("impl trait edge"))?;
        if trait_edge.1.target
            != OccurrenceTarget::Local(backend_semantic::ir::EntityId::new(service))
            || trait_edge.1.confidence != OccurrenceConfidence::Oracle
        {
            return Err(TestError::Missing("local trait edge at oracle confidence"));
        }
        let method = occurrences
            .iter()
            .find(|(_, occurrence)| occurrence.kind == ReferenceKind::MethodCall)
            .ok_or(TestError::Missing("method call occurrence"))?;
        let OccurrenceTarget::Local(target) = method.1.target else {
            return Err(TestError::Missing("local method call target"));
        };
        if entity_fact(&view, target.raw)? != (&b"run"[..], EntityKind::Function) {
            return Err(TestError::Missing("local method call target"));
        }
        Ok(())
    }

    /// A method call the oracle cannot resolve stays at syntactic confidence
    /// with a foreign key carrying the written spelling.
    #[test]
    fn unresolved_method_calls_stay_syntactic_foreign() -> Result<(), TestError> {
        let view = lower("pub struct Ghost;\n\npub fn haunt(g: &Ghost) {\n    g.vanish();\n}\n")?;
        let occurrences = occurrences(&view)?;
        let method = occurrences
            .iter()
            .find(|(_, occurrence)| occurrence.kind == ReferenceKind::MethodCall)
            .ok_or(TestError::Missing("method call occurrence"))?;
        if method.1.confidence != OccurrenceConfidence::Syntactic {
            return Err(TestError::Missing(
                "syntactic confidence for unresolved dispatch",
            ));
        }
        if !matches!(method.1.target, OccurrenceTarget::Foreign(_)) {
            return Err(TestError::Missing("foreign target for unresolved dispatch"));
        }
        Ok(())
    }

    /// A field access resolves to the named field's own fact ordinal.
    #[test]
    fn field_accesses_resolve_to_local_field_facts() -> Result<(), TestError> {
        let view = lower(
            "pub struct Panel {\n    pub score: u8,\n}\n\npub fn read(panel: &Panel) -> u8 {\n    panel.score\n}\n",
        )?;
        let score = fact_of(&view, b"score", EntityKind::Field)?;
        let occurrences = occurrences(&view)?;
        let access = occurrences
            .iter()
            .find(|(_, occurrence)| occurrence.kind == ReferenceKind::FieldAccess)
            .ok_or(TestError::Missing("field access occurrence"))?;
        if access.1.target != OccurrenceTarget::Local(backend_semantic::ir::EntityId::new(score))
            || access.1.confidence != OccurrenceConfidence::Oracle
        {
            return Err(TestError::Missing(
                "local field access at oracle confidence",
            ));
        }
        Ok(())
    }

    /// Items `#[cfg]`-gated out of the crate never reach HIR, so they never
    /// become facts; the compiled sibling does.
    #[test]
    fn cfg_gated_declarations_stay_out_of_the_lane() -> Result<(), TestError> {
        let view = lower("#[cfg(any())]\npub struct Ghost;\n\npub struct Real;\n")?;
        for entity in view.entities() {
            let atom = usize::try_from(entity.name.raw)?;
            let Some(atom) = view.atoms().nth(atom) else {
                return Err(TestError::Missing("entity atom"));
            };
            if atom.bytes == b"Ghost" {
                return Err(TestError::Missing("no fact for a cfg-gated-out item"));
            }
        }
        fact_of(&view, b"Real", EntityKind::Record)?;
        Ok(())
    }

    /// Macro-expanded declarations enter only when rust-analyzer projects
    /// their item and exact name origins back into this source. A whole-call
    /// projection for an inner field is rejected rather than published under
    /// the invocation spelling.
    #[test]
    fn macro_expanded_declarations_use_projected_source_origin() -> Result<(), TestError> {
        let view = lower(
            "macro_rules! declare {\n    ($name:ident) => {\n        pub struct $name {\n            pub value: u8,\n        }\n    };\n}\n\ndeclare!(Generated);\n\npub fn touch(generated: &Generated) -> u8 {\n    generated.value\n}\n",
        )?;
        let generated = fact_of(&view, b"Generated", EntityKind::Record)?;
        let generated_row = row_for_entity(&view, generated)?;
        if generated_row.record.nominal
            != Some(NominalRef::Local(backend_semantic::ir::EntityId::new(
                generated,
            )))
        {
            return Err(TestError::Missing(
                "projected expanded declaration structure",
            ));
        }
        if fact_of(&view, b"value", EntityKind::Field).is_ok()
            || fact_of(&view, b"declare!", EntityKind::Field).is_ok()
        {
            return Err(TestError::Missing(
                "no imprecisely projected expansion field",
            ));
        }
        Ok(())
    }

    /// A function-pointer parameter with nine arguments used to fold the
    /// whole pointer once the compound walk passed eight children. The
    /// pointer stays a function row with one child per argument.
    #[test]
    fn a_nine_argument_fn_pointer_keeps_every_argument() -> Result<(), TestError> {
        let view = lower(
            "pub fn probe(value: fn(u8, u8, u8, u8, u8, u8, u8, u8, u8)) {}\n",
        )?;
        let parameter = fact_of(&view, b"value", EntityKind::Parameter)?;
        let row = row_for_entity(&view, parameter)?;
        let children = local_type_children(&view, row.record)?;
        if row.record.tag != SemanticTypeTag::FunctionPointer || children.len() != 9 {
            return Err(TestError::Missing("nine-argument function pointer"));
        }
        Ok(())
    }

    /// A seventeen-field tuple struct used to drop `.16` because only sixteen
    /// positional spellings existed. The last field is a real declaration.
    #[test]
    fn a_seventeen_field_tuple_struct_keeps_the_last_field() -> Result<(), TestError> {
        let view = lower(
            "pub struct Wide(pub u8, pub u8, pub u8, pub u8, pub u8, pub u8, pub u8, pub u8, pub u8, pub u8, pub u8, pub u8, pub u8, pub u8, pub u8, pub u8, pub u8);\n",
        )?;
        fact_of(&view, b"0", EntityKind::Field)?;
        fact_of(&view, b"16", EntityKind::Field)?;
        Ok(())
    }

    /// Tuple-struct fields are HIR fields the written tree does not cast;
    /// they commit under their canonical positional names with exact cells.
    #[test]
    fn tuple_struct_fields_carry_positional_names_and_exact_cells() -> Result<(), TestError> {
        let view = lower("pub struct Pair(pub u8, pub i128);\n")?;
        let zero = fact_of(&view, b"0", EntityKind::Field)?;
        let one = fact_of(&view, b"1", EntityKind::Field)?;
        let rows = rows(&view)?;
        if rows[usize::try_from(zero)?].record.payload1 != (8 << INTEGER_WIDTH_SHIFT)
            || rows[usize::try_from(one)?].record.payload1
                != (128 << INTEGER_WIDTH_SHIFT) | SemanticTypeRecord::INTEGER_SIGNED_FLAG
        {
            return Err(TestError::Missing("tuple field width cells"));
        }
        Ok(())
    }

    /// Rustdoc splits into text and intra-doc links, and a link naming a
    /// pushed declaration resolves to that declaration's fact ordinal.
    #[test]
    fn rustdoc_links_resolve_to_local_declaration_facts() -> Result<(), TestError> {
        let view =
            lower("/// Feeds [`Cafe`] next door.\npub struct Cafe {\n    pub beans: u8,\n}\n")?;
        let mut docs = view.docs().ok_or(TestError::Missing("docs"))?;
        let mut saw_link = false;
        while let Some(fact) = docs.next() {
            let fact = fact.map_err(TestError::from)?;
            if let DocFragmentInput::Link {
                target: DocLinkTarget::Local(owner),
                ..
            } = fact.fragment
                && owner.raw == 0
            {
                saw_link = true;
            }
        }
        if !saw_link {
            return Err(TestError::Missing("local intra-doc link"));
        }
        Ok(())
    }

    /// A source beyond the bounded declaration lane keeps the exact closed
    /// lowering terminal instead of a truncated emission.
    #[test]
    fn sources_beyond_the_lane_capacity_reject_exactly() -> Result<(), TestError> {
        let mut source = String::new();
        for index in 0..crate::driver::lower::MAX_EMISSION_FACTS + 1 {
            source.push_str("pub struct S");
            source.push_str(index.to_string().as_str());
            source.push_str(";\n");
        }
        let outcome = lower_bytes(&source);
        match outcome {
            Err(TestError::Collection(RustCollectError::Lowering(
                backend_semantic::vocabulary::LoweringUnsupported::FactRejected { fact, .. },
            ))) if fact
                == crate::driver::lower::portable_count(
                    crate::driver::lower::MAX_EMISSION_FACTS,
                ) =>
            {
                Ok(())
            }
            Err(_) => Err(TestError::Missing("capacity terminal")),
            Ok(_) => Err(TestError::Missing("capacity rejection")),
        }
    }

    /// A qualified path reference commits its occurrence at the final
    /// segment's name extent: the absolute site bytes are exactly the
    /// identifier, and the site stays inside the owning declaration's
    /// provenance span — the containment law the image build re-checks.
    #[test]
    fn qualified_path_occurrences_commit_their_name_extent() -> Result<(), TestError> {
        let source =
            "pub enum Focus {\n    Near,\n}\n\npub fn aim() -> Focus {\n    Focus::Near\n}\n";
        let ir = owned_ir(source)?;
        let needle = source
            .find("Focus::Near")
            .ok_or(TestError::Missing("qualified path in fixture source"))?;
        let name_at = needle + "Focus::".len();
        let mut verified = false;
        for (_, occurrence) in ir.link_occurrences() {
            let Some(site) = occurrence.source else {
                continue;
            };
            let start = usize::try_from(site.start())?;
            let end = usize::try_from(site.end())?;
            if source.as_bytes().get(start..end) != Some(&b"Near"[..]) {
                continue;
            }
            verified = true;
            // The site is the name extent of the written qualified path.
            if start != name_at {
                return Err(TestError::Missing("occurrence at the name extent"));
            }
            // The site sits inside some owning declaration's provenance span
            // (the `aim` function), the containment law the image build
            // re-checks.
            let containing = ir.canonical_entities().any(|entity| {
                entity.source.is_some_and(|span| {
                    span.start() as usize <= start && end <= span.end() as usize
                })
            });
            if !containing {
                return Err(TestError::Missing("site inside its owner's span"));
            }
            // The link targets the variant entity by name.
            let Some(link) = ir.link(occurrence.link) else {
                return Err(TestError::Missing("occurrence link"));
            };
            let backend_semantic::ir::LinkTarget::Local(target) = link.target else {
                return Err(TestError::Missing("local variant target"));
            };
            let Some(item) = ir.item(target) else {
                return Err(TestError::Missing("target entity"));
            };
            if item.name() != b"Near" {
                return Err(TestError::Missing("variant entity target"));
            }
        }
        if !verified {
            return Err(TestError::Missing("name-extent occurrence site"));
        }
        Ok(())
    }

    /// `Iterator<Item = u8>` and `Iterator<Item = String>` are different
    /// bounds, and `Foo<false>` is not `Foo<true>`.
    #[test]
    fn associated_bindings_and_const_args_stay_distinct() -> Result<(), TestError> {
        let view = lower(
            "pub trait Iter { type Item; }\npub fn a<T: Iter<Item = u8>>() {}\npub fn b<T: Iter<Item = String>>() {}\npub struct Foo<const B: bool>;\npub fn c(_: Foo<false>) {}\npub fn d(_: Foo<true>) {}\n",
        )?;
        let rows = rows(&view)?;
        let mut item_bindings = Vec::new();
        for row in &rows {
            if row.record.tag != SemanticTypeTag::QualifiedPath
                || row.record.text != Some(b"Item".as_slice())
                || row.record.children.length != 1
            {
                continue;
            }
            item_bindings.push(local_type_children(&view, row.record)?);
        }
        if item_bindings.len() < 2 {
            return Err(TestError::Missing("two associated Item bindings"));
        }
        if item_bindings[0] == item_bindings[1] {
            return Err(TestError::Missing(
                "Iterator<Item = u8> and Iterator<Item = String> must differ",
            ));
        }
        let parameter_application = |name: &[u8]| -> Result<Vec<backend_semantic::ir::TypeId>, TestError> {
            let function = row_for_entity(&view, fact_of(&view, name, EntityKind::Function)?)?;
            let parameter = local_type_children(&view, function.record)?
                .first()
                .and_then(|target| rows.get(target.index()))
                .ok_or(TestError::Missing("parameter row"))?;
            if parameter.record.tag != SemanticTypeTag::Apply
                || parameter.record.children.length != 2
            {
                return Err(TestError::Missing("const generic application structure"));
            }
            local_type_children(&view, parameter.record)
        };
        if parameter_application(b"c")? == parameter_application(b"d")? {
            return Err(TestError::Missing("Foo<false> and Foo<true> must differ"));
        }
        Ok(())
    }

    /// `impl Iter<Item = u8>` must keep the binding a type-argument-only walk drops.
    #[test]
    fn impl_trait_associated_bindings_stay_distinct() -> Result<(), TestError> {
        let view = lower(
            "pub trait Iter { type Item; }\npub fn a(_: impl Iter<Item = u8>) {}\npub fn b(_: impl Iter<Item = String>) {}\n",
        )?;
        let rows = rows(&view)?;
        let mut bindings = Vec::new();
        for row in &rows {
            if row.record.tag != SemanticTypeTag::QualifiedPath
                || row.record.text != Some(b"Item".as_slice())
                || row.record.children.length != 1
            {
                continue;
            }
            bindings.push(local_type_children(&view, row.record)?);
        }
        if bindings.len() < 2 {
            return Err(TestError::Missing("two impl Trait Item bindings"));
        }
        if bindings[0] == bindings[1] {
            return Err(TestError::Missing(
                "impl Iter<Item = u8> and impl Iter<Item = String> must differ",
            ));
        }
        Ok(())
    }

    /// A field access inside a macro argument is authority-proven through
    /// macro descent even though the written tree parses the argument as a
    /// token tree. The occurrence keeps the invocation-site field spelling,
    /// its owning function, and the resolved local field target.
    #[test]
    fn macro_field_accesses_commit_oracle_local_field_occurrences() -> Result<(), TestError> {
        let source = "macro_rules! access_field {\n    ($e:expr, $f:ident) => {\n        $e.$f\n    };\n}\n\npub struct Panel {\n    pub score: u8,\n}\n\npub fn read(panel: &Panel) -> u8 {\n    access_field!(panel, score)\n}\n";
        let view = lower(source)?;
        let read = fact_of(&view, b"read", EntityKind::Function)?;
        let score = fact_of(&view, b"score", EntityKind::Field)?;
        let occurrences = occurrences(&view)?;
        let field_accesses = occurrences
            .iter()
            .filter(|(_, occurrence)| occurrence.kind == ReferenceKind::FieldAccess)
            .collect::<Vec<_>>();
        if field_accesses.len() != 1 {
            return Err(TestError::Missing("exactly one macro field access occurrence"));
        }
        let access = field_accesses[0];
        if access.0 != read {
            return Err(TestError::Missing("field access owned by read"));
        }
        if access.1.target != OccurrenceTarget::Local(backend_semantic::ir::EntityId::new(score))
            || access.1.confidence != OccurrenceConfidence::Oracle
        {
            return Err(TestError::Missing(
                "oracle-local field target for macro field access",
            ));
        }
        let name_at = source
            .find("access_field!(panel, score)")
            .ok_or(TestError::Missing("macro invocation in fixture source"))?
            + "access_field!(panel, ".len();
        let ir = owned_ir(source)?;
        let mut verified = false;
        for (_, occurrence) in ir.link_occurrences() {
            let Some(link) = ir.link(occurrence.link) else {
                continue;
            };
            if link.kind != backend_semantic::ir::LinkKind::Reads {
                continue;
            }
            let Some(site) = occurrence.source else {
                continue;
            };
            let start = usize::try_from(site.start())?;
            let end = usize::try_from(site.end())?;
            if source.as_bytes().get(start..end) != Some(b"score") {
                continue;
            }
            verified = true;
            if start != name_at {
                return Err(TestError::Missing("field access at invocation spelling"));
            }
            let backend_semantic::ir::LinkTarget::Local(target) = link.target else {
                return Err(TestError::Missing("local field link target"));
            };
            if ir.item(target).is_none_or(|item| item.name() != b"score") {
                return Err(TestError::Missing("score field link target"));
            }
        }
        if !verified {
            return Err(TestError::Missing("invocation-site field access spelling"));
        }
        Ok(())
    }

    /// A direct field access whose receiver is a macro call is already emitted
    /// by the syntax walk; macro descent must not emit the same projected
    /// field-name span again.
    #[test]
    fn macro_receiver_field_accesses_emit_once() -> Result<(), TestError> {
        let source = "macro_rules! identity {\n    ($e:expr) => {\n        $e\n    };\n}\n\npub struct Panel {\n    pub score: u8,\n}\n\npub fn read(panel: &Panel) -> u8 {\n    identity!(panel).score\n}\n";
        let view = lower(source)?;
        let read = fact_of(&view, b"read", EntityKind::Function)?;
        let score = fact_of(&view, b"score", EntityKind::Field)?;
        let field_accesses = occurrences(&view)?
            .into_iter()
            .filter(|(_, occurrence)| occurrence.kind == ReferenceKind::FieldAccess)
            .collect::<Vec<_>>();
        if field_accesses.len() != 1 {
            return Err(TestError::Missing("exactly one field access occurrence"));
        }
        let (owner, access) = field_accesses[0];
        if owner != read {
            return Err(TestError::Missing("field access owned by read"));
        }
        if access.target != OccurrenceTarget::Local(backend_semantic::ir::EntityId::new(score))
            || access.confidence != OccurrenceConfidence::Oracle
        {
            return Err(TestError::Missing(
                "oracle-local field target for macro-receiver access",
            ));
        }
        Ok(())
    }

    /// A path the oracle could not resolve is retained, never dropped: a
    /// cargo-universe foreign key carrying the exact written spelling at
    /// syntactic confidence, owned by the containing function.
    #[test]
    fn unresolved_paths_stay_foreign_with_their_written_spelling() -> Result<(), TestError> {
        let view = lower("pub fn probe() {\n    vanish_without_trace();\n}\n")?;
        let occurrences = occurrences(&view)?;
        let path = occurrences
            .iter()
            .find(|(_, occurrence)| occurrence.kind == ReferenceKind::FunctionCall)
            .ok_or(TestError::Missing("unresolved path occurrence"))?;
        if path.1.confidence != OccurrenceConfidence::Syntactic {
            return Err(TestError::Missing("syntactic confidence"));
        }
        let backend_semantic::ir::OccurrenceTarget::Foreign(key) = path.1.target else {
            return Err(TestError::Missing("foreign target"));
        };
        if !matches!(
            key.origin,
            backend_semantic::ir::ForeignOrigin::Universe { ecosystem: "cargo" }
        ) {
            return Err(TestError::Missing("cargo-universe origin"));
        }
        if key.path != "vanish_without_trace" || key.display != "vanish_without_trace" {
            return Err(TestError::Missing("written spelling as the key"));
        }
        Ok(())
    }
}
