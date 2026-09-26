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

impl<'authority, 'analysis, 'source> Emitter<'authority, 'analysis, 'source> {
    fn new(
        authority: &'authority RustAuthority<'analysis>,
        source: &'source [u8],
        facts: &'authority mut FactSet<'source>,
    ) -> Self {
        Self {
            database: authority.database,
            authority,
            source,
            facts,
            rows: Vec::new(),
            definitions: Vec::new(),
            fields: Vec::new(),
            covered_defs: Vec::new(),
            covered_fields: Vec::new(),
            covered_impls: Vec::new(),
            ordinals: Vec::new(),
            foreign_rows: Vec::new(),
            carrier_rows: Vec::new(),
            macro_sites: Vec::new(),
            scoped_names: std::collections::HashSet::new(),
            owner_order: Vec::new(),
            computed_owner: None,
        }
    }

    /// Streams the complete semantic plane in lane order.
    fn run(&mut self) -> Result<(), RustAuthorityError> {
        let declarations = self.materialize_declarations()?;
        self.ordinals = declarations.iter().map(|_| None).collect();
        self.collect_macro_sites()?;
        self.emit_type_roots(&declarations)?;
        self.emit_members(&declarations)?;
        self.emit_reexports()?;
        self.rebuild_owner_order();
        self.emit_parentage()?;
        self.attach_macros()?;
        self.emit_occurrences()?;
        self.emit_computed()?;
        self.emit_docs(&declarations)?;
        self.emit_reexport_docs()?;
        Ok(())
    }

    /// Borrows exact source bytes for a previously validated span.
    fn bytes_of(&self, span: ByteSpan) -> Result<&'source [u8], RustAuthorityError> {
        let start = usize::try_from(span.start)
            .map_err(|source| RustAuthorityError::Coordinate { span, source })?;
        let end = usize::try_from(span.end)
            .map_err(|source| RustAuthorityError::Coordinate { span, source })?;
        self.source
            .get(start..end)
            .ok_or(RustAuthorityError::InvalidSpan {
                span,
                source_bytes: self.source.len(),
            })
    }

    /// Borrows exact source bytes for one syntax node's validated span.
    fn bytes_of_node(&self, node: &SyntaxNode) -> Result<&'source [u8], RustAuthorityError> {
        let span = self.authority.span(node)?;
        self.bytes_of(span)
    }

    /// Locates one borrowed doc line's exact byte span in the caller source.
    /// The parsed tree's text lives in its own buffer, so the line is found
    /// by content; a line absent from the source stays unborrowed.
    fn span_of_text(&self, text: &str) -> Option<ByteSpan> {
        let needle = text.as_bytes();
        if needle.is_empty() || self.source.len() < needle.len() {
            return None;
        }
        let first = needle[0];
        self.source
            .iter()
            .enumerate()
            .filter(|(_, byte)| **byte == first)
            .map(|(start, _)| start)
            .find(|&start| {
                self.source
                    .get(start..start + needle.len())
                    .is_some_and(|window| window == needle)
            })
            .and_then(|start| {
                let start = u32::try_from(start).ok()?;
                let end = start.checked_add(u32::try_from(needle.len()).ok()?)?;
                Some(ByteSpan { start, end })
            })
    }

    /// Materializes every lane-admissible declaration once, with its exact
    /// name bytes and whole-item span. A `macro_rules!` definition keeps its
    /// closed `Macro` row, so a crate whose only declaration is a macro
    /// lowers like any other. After the written syntax walk, the HIR
    /// module-scope walk appends the uncovered remainder: declarations that
    /// exist only through macro expansion and tuple-struct fields the
    /// written tree does not cast.
    fn materialize_declarations(&mut self) -> Result<Vec<Decl<'source>>, RustAuthorityError> {
        let authority = self.authority;
        let mut declarations = Vec::new();
        // rust-analyzer projects a `#[cfg]`-disabled module through the written
        // syntax walk even though it omits every other cfg-disabled item from
        // HIR, and its module lookup resolves both same-name branches to the
        // one enabled definition. Its whole subtree therefore leaks with it
        // (the nested items are syntactically present). Such a module is
        // dropped, subtree and all, exactly as the module-scope walk already
        // omits it.
        let crate_cfg = self.crate_cfg_options();
        let mut disabled_spans: Vec<ByteSpan> = Vec::new();
        for declaration in authority.declarations() {
            let span = authority.span(&declaration.syntax)?;
            if let RustDefinition::Module(_) = &declaration.definition
                && let Some(item) = ast::Module::cast(declaration.syntax.clone())
                && module_cfg_disabled(&item, &crate_cfg)
            {
                disabled_spans.push(span);
                continue;
            }
            if disabled_spans
                .iter()
                .any(|disabled| disabled.start <= span.start && span.end <= disabled.end)
            {
                continue;
            }
            // An anonymous `const _: () = …;` carries no name of its own. The
            // authority's first-identifier fallback would otherwise name it
            // after an incidental path in its initializer (`assert`),
            // collapsing every such compile-time assertion at one scope onto a
            // fabricated duplicate. Anonymous items are not name-addressable;
            // they stay out, exactly as the module-scope walk drops an absent
            // name.
            if let RustDefinition::Constant(_) = &declaration.definition
                && let Some(item) = ast::Const::cast(declaration.syntax.clone())
                && item.name().is_none()
            {
                continue;
            }
            let name_span = self.declaration_name(&declaration)?;
            let name = self.bytes_of(name_span)?;
            // Rust's single-name law (E0428) makes any two same-name items
            // in one scope cfg-disjoint twins; an authority that does not
            // evaluate the gate (a tool attribute such as
            // `#[rustversion::since]`) streams both. Commit the first and
            // keep each later twin out rather than minting byte-identical
            // declaration identities the image build must reject.
            //
            // An implementation is keyed on its full written signature
            // instead of its bare name. E0428 never governs impl blocks, so
            // a source legally carries many written impls whose self type
            // spells the same bytes — several inherent blocks for one type,
            // or several traits impl'd for one self type (`IntoIterator` and
            // `TryFrom<&[T]>` both written `for &'a GenericArray<T, N>`).
            // `declaration_name` deliberately narrows an impl's name to its
            // self type alone (see its doc comment), so keying this gate on
            // that name alone would treat every one of those legal siblings
            // as the same cfg twin and silently drop all but the first,
            // orphaning its members to a fabricated root scope. The richer
            // signature key (trait spelling, self type, generics/where
            // text, and ordered member names) mirrors what the
            // trait/generics/member-aware declaration identity already
            // discriminates real siblings on, so distinct siblings pass
            // this gate untouched while a genuine tool-attribute twin
            // (identical trait, self type, generics, and members) still
            // dedups here exactly as every other declaration kind does.
            let scope = self.enclosing_item_span(&declaration.syntax)?;
            let key = if declaration.kind == SemanticKind::Implementation {
                self.impl_signature_key(&declaration, name)?
            } else {
                name.to_vec()
            };
            let admitted = self
                .scoped_names
                .insert((scope, declaration.kind as u8, key));
            if !admitted {
                // A written implementation the HIR module-scope walk below
                // can independently rediscover (unlike an ordinary named
                // item, which that walk never re-enumerates once the
                // written-syntax pass has cast it) must still be marked
                // covered here, or the twin this gate just dropped comes
                // back as a second, byte-identical admission and the image
                // build honestly — but wrongly — rejects it as a
                // `DuplicateDeclarationIdentity` collision with itself.
                if let RustDefinition::Implementation(implementation) = &declaration.definition {
                    self.covered_impls.push(*implementation);
                }
                continue;
            }
            match &declaration.definition {
                RustDefinition::Field(field) => self.covered_fields.push(*field),
                RustDefinition::Implementation(implementation) => {
                    self.covered_impls.push(*implementation);
                }
                other => {
                    if let Some(definition) = module_def(other) {
                        self.covered_defs.push(definition);
                    }
                }
            }
            declarations.push(Decl {
                kind: declaration.kind,
                definition: declaration.definition,
                syntax: declaration.syntax,
                name,
                span,
                expanded: false,
            });
        }
        for declaration in authority.module_declarations() {
            let ModuleDeclaration {
                definition,
                item,
                name: name_span,
                syntax,
            } = declaration;
            let SourceOrigin::Local(span) = item else {
                continue;
            };
            if self.is_covered(&definition) {
                continue;
            }
            // A walked field without a name node is a tuple field; it keeps
            // its canonical positional spelling, exactly the name Rust uses
            // for `.0`-style access. Any other unnamed position stays out.
            let name = match name_span {
                Some(name_span) => self.bytes_of(name_span)?,
                None => {
                    let RustDefinition::Field(field) = &definition else {
                        continue;
                    };
                    let index = field.index();
                    let Some(name) = tuple_field_name(usize::from(index)) else {
                        continue;
                    };
                    if field.name(self.database).as_str().as_bytes() != name {
                        // A named expansion field whose projected name is
                        // unavailable is not a positional field. Keeping it
                        // would mint a false `.0`-style declaration.
                        continue;
                    }
                    name
                }
            };
            let expanded = authority.is_macro_expansion(&syntax);
            declarations.push(Decl {
                kind: definition.kind(),
                definition,
                syntax,
                name,
                span,
                expanded,
            });
        }
        Ok(declarations)
    }

    /// The exact span of the innermost enclosing item, so single-name-law
    /// twins are keyed by their true scope: two nested helpers in different
    /// functions stay distinct, while two same-name items in one function
    /// body are the E0428 twins the image build cannot represent twice.
    /// `None` marks a top-level item, whose scope is the crate file itself.
    fn enclosing_item_span(
        &self,
        syntax: &SyntaxNode,
    ) -> Result<Option<(u32, u32)>, RustAuthorityError> {
        let Some(item) = syntax.ancestors().skip(1).find_map(ast::Item::cast) else {
            return Ok(None);
        };
        let span = self.authority.span(item.syntax())?;
        Ok(Some((span.start, span.end)))
    }

    /// Clones this source crate's resolved conditional-compilation options,
    /// owned so the written walk can hold them while it mutates its cover
    /// sets. The options already include the caller's Cargo feature policy and
    /// target, so they evaluate exactly as rust-analyzer's own HIR did.
    fn crate_cfg_options(&self) -> ra_ap_hir::CfgOptions {
        self.authority
            .semantics
            .hir_file_to_module_def(self.authority.source_file)
            .map(|root| root.krate(self.database).cfg(self.database).clone())
            .unwrap_or_default()
    }

    /// True when the written syntax walk already materialized one HIR
    /// definition, so the module-scope walk must not emit it twice.
    fn is_covered(&self, definition: &RustDefinition) -> bool {
        match definition {
            RustDefinition::Field(field) => self.covered_fields.contains(field),
            RustDefinition::Implementation(implementation) => {
                self.covered_impls.contains(implementation)
            }
            other => module_def(other).is_some_and(|key| self.covered_defs.contains(&key)),
        }
    }

    /// Borrows the exact name bytes of one declaration. Implementations name
    /// by their whole written self type (the closed lane has no anonymous
    /// rows), so two blocks whose self types differ only in generic
    /// arguments (`U<N, false>` versus `U<N, true>`) commit distinct
    /// declarations instead of byte-identical twins; falling back to the
    /// last path segment alone would collapse them and the image build
    /// would honestly reject the collision. A non-path self type (`&[u8]`,
    /// a tuple, a trait object) owns no name segment; its exact written
    /// spelling is likewise the only self-describing name — the authority's
    /// first-identifier rule would otherwise name the block after its first
    /// method or generic parameter and collapse distinct impls together.
    fn declaration_name(
        &self,
        declaration: &RustDeclaration,
    ) -> Result<ByteSpan, RustAuthorityError> {
        if declaration.kind == SemanticKind::Implementation
            && let Some(implementation) = ast::Impl::cast(declaration.syntax.clone())
            && let Some(self_ty) = implementation.self_ty()
        {
            return self.authority.span(self_ty.syntax());
        }
        self.authority.declaration_name(declaration)
    }

    /// Borrows one declaration's exact name bytes from the caller source.
    fn name_of(&self, declaration: &Decl<'source>) -> Result<&'source [u8], RustAuthorityError> {
        Ok(declaration.name)
    }

    /// Builds the written-syntax same-name gate's key for one implementation:
    /// its exact written signature rather than its bare self-type name.
    /// E0428 never governs impl blocks, so the gate must not collapse two
    /// distinct legal siblings — different traits impl'd for one self type,
    /// or several inherent blocks with different members — that merely
    /// spell the same self type. It must still collapse two byte-identical
    /// written twins straddling an unevaluated tool-attribute cfg such as
    /// `#[rustversion::since]`/`#[rustversion::before]` (a pattern `anyhow`,
    /// `thiserror`, `serde`, `proc-macro2`, and `semver` all use), so every
    /// axis the declaration identity itself later discriminates on enters
    /// this key too: the trait spelling (or its absence, for an inherent
    /// block), the self type spelling, the written generic parameter and
    /// where-clause text, and the ordered list of associated item names.
    /// A twin pair writes byte-identical text on both branches by
    /// definition, so it produces one key and collapses exactly as before;
    /// two siblings that differ on any of these axes produce distinct keys
    /// and both survive to reach the trait/generics/member-aware identity
    /// pass.
    fn impl_signature_key(
        &self,
        declaration: &RustDeclaration,
        self_type: &[u8],
    ) -> Result<Vec<u8>, RustAuthorityError> {
        fn push_optional(out: &mut Vec<u8>, spelling: Option<&[u8]>) {
            match spelling {
                Some(spelling) => {
                    out.push(1);
                    out.extend_from_slice(&(spelling.len() as u64).to_le_bytes());
                    out.extend_from_slice(spelling);
                }
                None => out.push(0),
            }
        }

        let mut key = Vec::with_capacity(64 + self_type.len());
        let Some(item) = ast::Impl::cast(declaration.syntax.clone()) else {
            // No borrowable written syntax (a macro-expansion projection):
            // the self type spelling is the only honest signature left.
            key.extend_from_slice(self_type);
            return Ok(key);
        };
        match item.trait_() {
            Some(trait_ty) => push_optional(&mut key, Some(self.bytes_of_node(trait_ty.syntax())?)),
            None => push_optional(&mut key, None),
        }
        push_optional(&mut key, Some(self_type));
        match item.generic_param_list() {
            Some(generics) => push_optional(&mut key, Some(self.bytes_of_node(generics.syntax())?)),
            None => push_optional(&mut key, None),
        }
        match item.where_clause() {
            Some(where_clause) => {
                push_optional(&mut key, Some(self.bytes_of_node(where_clause.syntax())?));
            }
            None => push_optional(&mut key, None),
        }
        let members: Vec<ast::AssocItem> = item
            .assoc_item_list()
            .map(|list| list.assoc_items().collect())
            .unwrap_or_default();
        key.extend_from_slice(&(members.len() as u64).to_le_bytes());
        for member in &members {
            let (tag, spelling) = match member {
                ast::AssocItem::Const(item) => (
                    0u8,
                    item.name()
                        .map(|name| self.bytes_of_node(name.syntax()))
                        .transpose()?,
                ),
                ast::AssocItem::Fn(item) => (
                    1,
                    item.name()
                        .map(|name| self.bytes_of_node(name.syntax()))
                        .transpose()?,
                ),
                ast::AssocItem::TypeAlias(item) => (
                    2,
                    item.name()
                        .map(|name| self.bytes_of_node(name.syntax()))
                        .transpose()?,
                ),
                ast::AssocItem::MacroCall(item) => (
                    3,
                    item.path()
                        .map(|path| self.bytes_of_node(path.syntax()))
                        .transpose()?,
                ),
            };
            key.push(tag);
            push_optional(&mut key, spelling);
        }
        Ok(key)
    }

    /// Borrows the written leaf name of one type position, when the position
    /// spells a path (`Option`, `std::vec::Vec` yields `Vec`).
    fn written_type_name(&self, anchor: Option<&ast::Type>) -> Option<&'source [u8]> {
        let ast::Type::PathType(path_type) = anchor? else {
            return None;
        };
        let segment = path_type.path()?.segments().last()?;
        let name_ref = segment.name_ref()?;
        let span = self.authority.span(name_ref.syntax()).ok()?;
        self.bytes_of(span).ok()
    }

    /// The unresolved record for a foreign named type: the exact written
    /// spelling when the source wrote one, otherwise the gap reason whose
    /// record lawfully carries no text cell.
    fn unresolved_record(&self, anchor: Option<&ast::Type>) -> SemanticTypeRecord<'source> {
        match self.written_type_name(anchor) {
            Some(text) => unknown_record(TypeReason::UnresolvedExternal, Some(text)),
            None => unknown_record(TypeReason::OracleGap, None),
        }
    }

    /// Collects every macro invocation site once: written path spelling,
    /// exact call span, and whether rust-analyzer resolved the definition.
    fn collect_macro_sites(&mut self) -> Result<(), RustAuthorityError> {
        let authority = self.authority;
        for call in authority.macro_calls() {
            let Some(path) = call.path() else {
                continue;
            };
            let spelling = self.bytes_of_node(path.syntax())?;
            // The occurrence site is the written macro path — the name a
            // reader searched for — never the whole invocation call, whose
            // argument text can carry arbitrary bytes.
            let span = authority.span(path.syntax())?;
            let resolved = authority.resolve_macro(&call).is_some();
            self.macro_sites.push(MacroSite {
                spelling,
                span,
                resolved,
            });
        }
        Ok(())
    }

    /// Reserves every module, record, enum, and trait with its legal diagonal
    /// self-nominal, so any later type can name any type root. The reservation
    /// pass carries no extension, because a local trait declared later in the
    /// file has no ordinal yet when an earlier root's bounds are built; the
    /// second pass rebuilds each root's real extension after every local
    /// ordinal exists and reattaches it against the exact parameter range
    /// captured at that declaration's own close point.
    fn emit_type_roots(
        &mut self,
        declarations: &[Decl<'source>],
    ) -> Result<(), RustAuthorityError> {
        let placeholder = self.empty_extension(RustOwnership::Value)?;
        for index in 0..declarations.len() {
            let kind = declarations[index].kind;
            if !is_type_root(kind) {
                continue;
            }
            let declaration = &declarations[index];
            let mut fact = SemanticFact::new(
                entity_kind(kind)?,
                self.name_of(declaration)?,
                constructor(kind)?,
            )
            .with_visibility(self.declaration_visibility(declaration));
            if kind != SemanticKind::Module {
                let own_ordinal = coordinate(self.facts.len())?;
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Nominal);
                record.nominal = Some(NominalRef::Local(EntityId::new(own_ordinal)));
                fact = fact.typed(record);
            }
            let ordinal = push(self.facts, fact)?;
            self.register(ordinal, index, declaration, placeholder)?;
        }
        for index in 0..declarations.len() {
            let kind = declarations[index].kind;
            if !is_type_root(kind) {
                continue;
            }
            let declaration = &declarations[index];
            let extension = self.base_extension(RustOwnership::Value, declaration)?;
            let Some(ordinal) = self.ordinals.get(index).copied().flatten() else {
                continue;
            };
            let range = self
                .facts
                .type_parameter_range(extension.where_clauses.raw)
                .map_err(|_| admission())?;
            let free_range = self
                .facts
                .free_predicate_range(extension.free_predicates.raw)
                .map_err(|_| admission())?;
            let slot = usize::try_from(ordinal).map_err(|_| admission())?;
            self.facts
                .attach_extension_with_type_parameters(
                    slot,
                    EmissionExtension::Rust(extension),
                    range,
                )
                .map_err(|_| admission())?;
            self.facts
                .set_free_predicate_range(slot, &EmissionExtension::Rust(extension), free_range)
                .map_err(|_| admission())?;
            if let Some(row) = self.rows.iter_mut().find(|row| row.ordinal == ordinal) {
                row.extension = extension;
            }
        }
        Ok(())
    }

    /// Pass two: fields, variants, implementations, callables, aliases, and
    /// value bindings in source order, each with its projected declared type.
    fn emit_members(&mut self, declarations: &[Decl<'source>]) -> Result<(), RustAuthorityError> {
        for index in 0..declarations.len() {
            let declaration = &declarations[index];
            match declaration.kind {
                SemanticKind::Field => self.emit_field(index, declaration)?,
                SemanticKind::Variant => self.emit_variant(index, declaration)?,
                SemanticKind::Implementation => self.emit_implementation(index, declaration)?,
                SemanticKind::Function => self.emit_function(index, declaration)?,
                SemanticKind::TypeAlias | SemanticKind::Constant | SemanticKind::Static => {
                    self.emit_binding(index, declaration)?
                }
                SemanticKind::Macro => self.emit_macro(index, declaration)?,
                SemanticKind::Module
                | SemanticKind::Record
                | SemanticKind::Enum
                | SemanticKind::Trait
                | SemanticKind::LocalBinding
                | SemanticKind::GenericParameter
                | SemanticKind::Builtin => {}
            }
        }
        Ok(())
    }

    /// Emits one record field with its HIR-projected declared type.
    fn emit_field(
        &mut self,
        index: usize,
        declaration: &Decl<'source>,
    ) -> Result<(), RustAuthorityError> {
        let RustDefinition::Field(field) = &declaration.definition else {
            return Ok(());
        };
        let semantic = field.ty(self.database);
        let anchor = if declaration.expanded {
            None
        } else {
            ast::RecordField::cast(declaration.syntax.clone()).and_then(|item| item.ty())
        };
        let lowered = self.lower_pending_type(&semantic, anchor.as_ref(), MAX_TYPE_DEPTH)?;
        self.push_typed(index, declaration, lowered, RustOwnership::Value)
    }

    /// Emits one enum variant as the constructor it is: its declared type is
    /// the `FunctionPointer` over one hosted row per field type plus the
    /// parent enum's fact ordinal, with the result flag set. A variant whose
    /// field run cannot fit the bounded type-child lane, or whose parent enum
    /// carries no lane ordinal, keeps the parent enum's plain nominal row
    /// instead — still proven, just not the constructor signature.
    fn emit_variant(
        &mut self,
        index: usize,
        declaration: &Decl<'source>,
    ) -> Result<(), RustAuthorityError> {
        let RustDefinition::Variant(variant) = &declaration.definition else {
            return Ok(());
        };
        let parent = variant.parent_enum(self.database);
        let parent_ordinal = self.ordinal_of_adt(ra_ap_hir::Adt::from(parent));
        let fields = variant.fields(self.database);
        let commits_signature = parent_ordinal.is_some() && fields.len() + 1 <= MAX_TYPE_CHILDREN;
        if !commits_signature {
            let record = match parent_ordinal {
                Some(ordinal) => {
                    let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Nominal);
                    record.nominal = Some(NominalRef::Local(EntityId::new(ordinal)));
                    record
                }
                None => unknown_record(TypeReason::OracleGap, None),
            };
            return self.push_typed(
                index,
                declaration,
                Lowered::leaf(record),
                RustOwnership::Value,
            );
        }
        let variant_syntax = if declaration.expanded {
            None
        } else {
            ast::Variant::cast(declaration.syntax.clone())
        };
        let mut children = Vec::new();
        for (position, field) in fields.iter().enumerate() {
            let semantic = field.ty(self.database);
            let anchor = variant_syntax
                .as_ref()
                .and_then(|variant| variant_field_anchor(variant, position));
            match self.lower_target(&semantic, anchor, MAX_TYPE_DEPTH - 1)? {
                Some(target) => children.push(target),
                None => {
                    let record = match parent_ordinal {
                        Some(ordinal) => {
                            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Nominal);
                            record.nominal = Some(NominalRef::Local(EntityId::new(ordinal)));
                            record
                        }
                        None => unknown_record(TypeReason::OracleGap, None),
                    };
                    return self.push_typed(
                        index,
                        declaration,
                        Lowered::leaf(record),
                        RustOwnership::Value,
                    );
                }
            }
        }
        children.push(parent_ordinal.unwrap_or_default());
        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::FunctionPointer);
        record.payload1 = SemanticTypeRecord::FUNCTION_RESULT_COUNT_ONE;
        self.push_typed(
            index,
            declaration,
            Lowered { record, children },
            RustOwnership::Value,
        )
    }

    /// Emits one inherent or trait implementation with its self type. The
    /// trait reference is recorded beside the fact so coordinate-free
    /// identity can distinguish `impl Trait for Self` splits that share one
    /// written self type; an inherent implementation records its distinct
    /// tag instead of a fabricated trait shape.
    fn emit_implementation(
        &mut self,
        index: usize,
        declaration: &Decl<'source>,
    ) -> Result<(), RustAuthorityError> {
        let RustDefinition::Implementation(implementation) = &declaration.definition else {
            return Ok(());
        };
        let semantic = implementation.self_ty(self.database);
        let anchor = if declaration.expanded {
            None
        } else {
            ast::Impl::cast(declaration.syntax.clone()).and_then(|item| item.self_ty())
        };
        let lowered = self.lower_pending_type(&semantic, anchor.as_ref(), MAX_TYPE_DEPTH)?;
        let trait_ref = self.impl_trait_ref(declaration)?;
        self.push_typed(index, declaration, lowered, RustOwnership::Value)?;
        if let Some(trait_ref) = trait_ref
            && let Some(Some(ordinal)) = self.ordinals.get(index).copied()
        {
            self.facts
                .set_impl_trait(ordinal, trait_ref)
                .map_err(|_| admission())?;
        }
        Ok(())
    }

    /// Resolves one implementation's trait reference without inventing a
    /// shape. An inherent block keeps its tag; a trait block naming a
    /// committed lane-local trait keeps that ordinal, while a foreign or
    /// unresolved trait keeps its exact written spelling. `None` means the
    /// expanded syntax owns no borrowable spelling, so identity falls back
    /// to the inherent tag rather than manufacturing bytes.
    fn impl_trait_ref(
        &mut self,
        declaration: &Decl<'source>,
    ) -> Result<Option<crate::driver::lower::StagedImplTrait<'source>>, RustAuthorityError> {
        if declaration.expanded {
            return Ok(None);
        }
        let Some(item) = ast::Impl::cast(declaration.syntax.clone()) else {
            return Ok(None);
        };
        let Some(trait_ty) = item.trait_() else {
            return Ok(Some(crate::driver::lower::StagedImplTrait::Inherent));
        };
        match self.trait_bound_constraint(&trait_ty)? {
            TraitBoundTarget::Committed(ordinal) => {
                Ok(Some(crate::driver::lower::StagedImplTrait::Local {
                    target: ordinal,
                    spelling: self.bytes_of_node(trait_ty.syntax())?,
                }))
            }
            TraitBoundTarget::Foreign | TraitBoundTarget::Unresolved => {
                let spelling = self.bytes_of_node(trait_ty.syntax())?;
                Ok(Some(crate::driver::lower::StagedImplTrait::Foreign(
                    spelling,
                )))
            }
        }
    }

    /// Emits one type alias, constant, or static binding from its HIR
    /// semantic type, anchored on the written type when the source spells
    /// one. `static mut` carries the mutable-borrow ownership cell; every
    /// other binding is a plain value.
    fn emit_binding(
        &mut self,
        index: usize,
        declaration: &Decl<'source>,
    ) -> Result<(), RustAuthorityError> {
        let semantic = declaration.definition.semantic_type(self.database).ok_or(
            RustAuthorityError::MissingSemanticFact {
                fact: declaration.kind,
            },
        )?;
        let anchor = if declaration.expanded {
            None
        } else {
            written_binding_type(&declaration.syntax)
        };
        let ownership = match &declaration.definition {
            RustDefinition::Static(static_) if static_.is_mut(self.database) => {
                RustOwnership::MutableBorrow
            }
            _ => RustOwnership::Value,
        };
        let lowered = self.lower_pending_type(&semantic, anchor.as_ref(), MAX_TYPE_DEPTH)?;
        self.push_typed(index, declaration, lowered, ownership)
    }

    /// Emits one `macro_rules!` definition with the honest unwritten type:
    /// the lane proves the macro's name and extent, never a replacement-list
    /// type, so the fact keeps the unannotated record exactly as the C lane
    /// keeps its macros. The written visibility prefix stays the lane's only
    /// visibility witness (`#[macro_export]` is not a written `pub`).
    fn emit_macro(
        &mut self,
        index: usize,
        declaration: &Decl<'source>,
    ) -> Result<(), RustAuthorityError> {
        let extension = self.base_extension(RustOwnership::Value, declaration)?;
        let fact = SemanticFact::new(
            EntityKind::Macro,
            self.name_of(declaration)?,
            constructor(declaration.kind)?,
        )
        .with_visibility(self.declaration_visibility(declaration))
        .typed(unknown_record(TypeReason::Unannotated, None))
        .with_extension(EmissionExtension::Rust(extension));
        let ordinal = push(self.facts, fact)?;
        self.register(ordinal, index, declaration, extension)
    }

    /// Emits one function: its receiver, parameters, and result slot first,
    /// then the function fact whose product and `FunctionPointer` type
    /// children are exactly those rows.
    fn emit_function(
        &mut self,
        index: usize,
        declaration: &Decl<'source>,
    ) -> Result<(), RustAuthorityError> {
        let RustDefinition::Function(function) = &declaration.definition else {
            return Ok(());
        };
        let fn_item = if declaration.expanded {
            None
        } else {
            ast::Fn::cast(declaration.syntax.clone())
        };
        if fn_item.is_none() && !declaration.expanded {
            return Err(RustAuthorityError::MissingSemanticFact {
                fact: SemanticKind::Function,
            });
        }
        let mut parameter_ordinals = Vec::new();
        let mut signature_children = Vec::new();

        if let Some(receiver) = function.self_param(self.database) {
            let semantic = receiver.ty(self.database);
            let lowered = self.lower_pending_type(&semantic, None, MAX_TYPE_DEPTH)?;
            let ownership = receiver_ownership(receiver.access(self.database));
            let ordinal = self.push_parameter(SELF_NAME, lowered, ownership, None)?;
            parameter_ordinals.push(ordinal);
            signature_children.push(ordinal);
        }

        let parameters = function.params_without_self(self.database);
        let written: Vec<ast::Param> = fn_item
            .as_ref()
            .and_then(|item| item.param_list())
            .map(|list| list.params().collect())
            .unwrap_or_default();
        for (position, parameter) in parameters.iter().enumerate() {
            let semantic = parameter.ty().clone();
            let anchor = written.get(position).and_then(|param| param.ty());
            let name = self.parameter_name(
                written.get(position),
                parameter.name(self.database),
                position,
            );
            let lowered = self.lower_pending_type(&semantic, anchor.as_ref(), MAX_TYPE_DEPTH)?;
            let ownership = parameter_ownership(self.database, &semantic);
            let wildcard = name == b"_";
            let ordinal = self.push_parameter(name, lowered, ownership, wildcard.then_some(position))?;
            parameter_ordinals.push(ordinal);
            signature_children.push(ordinal);
        }

        let returns = function.ret_type(self.database);
        let result_anchor = fn_item
            .as_ref()
            .and_then(|item| item.ret_type())
            .and_then(|ret| ret.ty());
        let lowered = self.lower_pending_type(&returns, result_anchor.as_ref(), MAX_TYPE_DEPTH)?;
        let mut fact = SemanticFact::new(
            EntityKind::Parameter,
            self.name_of(declaration)?,
            LEAF_PRODUCT,
        )
        .with_visibility(self.declaration_visibility(declaration))
        .typed(lowered.record)
        .with_extension(EmissionExtension::Rust(
            self.empty_extension(RustOwnership::Value)?,
        ));
        for target in lowered.children {
            fact = fact.type_child(target, None, 0);
        }
        let result_ordinal = coordinate(push(self.facts, fact)?)?;
        signature_children.push(result_ordinal);

        let arity = coordinate(parameter_ordinals.len())?;
        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::FunctionPointer);
        record.payload1 = SemanticTypeRecord::FUNCTION_RESULT_COUNT_ONE;
        let extension = self.base_extension(RustOwnership::Value, declaration)?;
        let mut fact = SemanticFact::new(
            EntityKind::Function,
            self.name_of(declaration)?,
            SemanticProductConstructor::function(arity, 1),
        )
        .with_visibility(self.declaration_visibility(declaration))
        .typed(record)
        .with_extension(EmissionExtension::Rust(extension));
        for ordinal in &parameter_ordinals {
            fact = fact.child(ProductChildRole::FunctionParameter, *ordinal);
        }
        fact = fact.child(ProductChildRole::FunctionResult, result_ordinal);
        for ordinal in &signature_children {
            fact = fact.type_child(*ordinal, None, 0);
        }
        let ordinal = push(self.facts, fact)?;
        // Signature carriers share names and types across functions; bind
        // each to its executable so identical carriers stay distinct.
        let executable = coordinate(ordinal)?;
        let name_len = declaration.name.len();
        for carrier in parameter_ordinals.iter().copied().chain([result_ordinal]) {
            self.facts
                .attach_parent(carrier, executable)
                .map_err(|fault| parentage_fault(carrier, name_len, fault))?;
        }
        self.register(ordinal, index, declaration, extension)
    }

    /// Pushes one signature carrier fact (receiver or parameter) with its
    /// lowered record and the ownership-only extension row its position owns.
    fn push_parameter(
        &mut self,
        name: &'source [u8],
        lowered: Lowered<'source>,
        ownership: RustOwnership,
        wildcard_position: Option<usize>,
    ) -> Result<u32, RustAuthorityError> {
        let mut fact = SemanticFact::new(EntityKind::Parameter, name, LEAF_PRODUCT)
            .typed(lowered.record)
            .with_extension(EmissionExtension::Rust(self.empty_extension(ownership)?));
        for target in lowered.children {
            fact = fact.type_child(target, None, 0);
        }
        if let Some(position) = wildcard_position {
            fact = fact.with_identity_discriminator(wildcard_param_discriminator(position));
        }
        coordinate(push(self.facts, fact)?)
    }

    /// Borrows one written parameter name: the binding identifier when the
    /// pattern spells one, otherwise the analyzer's own parameter name found
    /// in the source text, otherwise the static fallback binding name. A
    /// wildcard pattern binds nothing, so two same-typed `_` parameters would
    /// otherwise mint byte-identical siblings; the display name stays `_` and
    /// the positional index is carried in the identity discriminator so a
    /// wildcard cannot collide with a parameter the source actually named
    /// `_0`.
    fn parameter_name(
        &self,
        written: Option<&ast::Param>,
        hir_name: Option<ra_ap_hir::Name>,
        _position: usize,
    ) -> &'source [u8] {
        if let Some(param) = written {
            if let Some(pattern) = param.pat() {
                if let ast::Pat::IdentPat(ident) = &pattern
                    && let Some(name) = ident.name()
                    && let Ok(bytes) = self.bytes_of_node(name.syntax())
                {
                    return bytes;
                }
                if matches!(&pattern, ast::Pat::WildcardPat(_)) {
                    return b"_";
                }
                if let Ok(bytes) = self.bytes_of_node(pattern.syntax()) {
                    return bytes;
                }
            }
        }
        if let Some(name) = hir_name
            && let Some(span) = self.span_of_text(name.as_str())
            && let Ok(bytes) = self.bytes_of(span)
        {
            return bytes;
        }
        PARAM_FALLBACK_NAME
    }

    /// Captures only the written visibility prefix; omitted visibility is Rust-private.
    fn declaration_visibility(
        &self,
        declaration: &Decl<'source>,
    ) -> backend_semantic::ir::Visibility {
        if declaration.expanded {
            return backend_semantic::ir::Visibility::Unknown;
        }
        let Some(visibility) = ast::AnyHasVisibility::cast(declaration.syntax.clone())
            .and_then(|item| item.visibility())
        else {
            return backend_semantic::ir::Visibility::Private;
        };
        let range = visibility.syntax().text_range();
        let Some(start) = usize::try_from(u32::from(range.start())).ok() else {
            return backend_semantic::ir::Visibility::Unknown;
        };
        let Some(end) = usize::try_from(u32::from(range.end())).ok() else {
            return backend_semantic::ir::Visibility::Unknown;
        };
        let Some(bytes) = self.source.get(start..end) else {
            return backend_semantic::ir::Visibility::Unknown;
        };
        if bytes == b"pub(crate)" {
            backend_semantic::ir::Visibility::Package
        } else if bytes.starts_with(b"pub(") {
            backend_semantic::ir::Visibility::Restricted
        } else {
            backend_semantic::ir::Visibility::Public
        }
    }

    /// Pushes one member fact with a projected record and its base Rust
    /// extension row, then registers the row.
    fn push_typed(
        &mut self,
        index: usize,
        declaration: &Decl<'source>,
        lowered: Lowered<'source>,
        ownership: RustOwnership,
    ) -> Result<(), RustAuthorityError> {
        let extension = self.base_extension(ownership, declaration)?;
        let mut fact = SemanticFact::new(
            entity_kind(declaration.kind)?,
            self.name_of(declaration)?,
            constructor(declaration.kind)?,
        )
        .with_visibility(self.declaration_visibility(declaration))
        .typed(lowered.record)
        .with_extension(EmissionExtension::Rust(extension));
        if let Some(discriminator) = self.expanded_impl_discriminator(declaration) {
            fact = fact.with_identity_discriminator(discriminator);
        }
        for target in lowered.children {
            fact = fact.type_child(target, None, 0);
        }
        let ordinal = push(self.facts, fact)?;
        self.register(ordinal, index, declaration, extension)
    }

    /// Mints the identity discriminator of one macro-expanded implementation
    /// from the expansion's own trait and self-type spelling. A macro that
    /// stamps several impl blocks for one self type projects them all onto the
    /// single invocation span, so the written projections cannot separate
    /// them: the whole-item span cannot frame a per-impl trait reference, and
    /// member names have no same-source spelling at a shared call-site range.
    /// The expansion buffer is the authority's own projected Rust syntax for
    /// those impls, so the header it spells is authority truth; only its
    /// domain-separated digest enters identity, keeping every written impl's
    /// variant byte-identical.
    fn expanded_impl_discriminator(&self, declaration: &Decl<'source>) -> Option<[u8; 16]> {
        if !declaration.expanded {
            return None;
        }
        let item = ast::Impl::cast(declaration.syntax.clone())?;
        let trait_spelling = item
            .trait_()
            .map(|trait_ty| trait_ty.syntax().to_string())
            .unwrap_or_else(|| "inherent".to_owned());
        let self_spelling = item.self_ty()?.syntax().to_string();
        let mut hash = Sha256::new();
        hash.update(b"compiler.rust.expanded-impl.v1\0");
        hash.update(trait_spelling.as_bytes());
        hash.update([0]);
        hash.update(self_spelling.as_bytes());
        let mut discriminator = [0_u8; 16];
        discriminator.copy_from_slice(&hash.finalize()[..16]);
        Some(discriminator)
    }

    /// Builds one declaration's base Rust extension row: the ownership cell,
    /// its written lifetime spellings, and its where-clause coordinate. The
    /// coordinate is the shared zero-based staging start; its exact length is
    /// captured with the extension transaction, so an empty list never needs
    /// a language-specific one-based sentinel. Macro spellings attach
    /// in a later phase. Expansion-derived declarations carry the empty
    /// lifetime list and no bounds, because their generic syntax lives in
    /// the expansion buffer, not this source.
    fn base_extension(
        &mut self,
        ownership: RustOwnership,
        declaration: &Decl<'source>,
    ) -> Result<RustFacts, RustAuthorityError> {
        if declaration.expanded {
            return self.empty_extension(ownership);
        }
        let syntax = &declaration.syntax;
        let lifetimes = self.lifetime_atoms(syntax)?;
        let free_start = coordinate(self.facts.free_predicate_len)?;
        let (where_clauses, const_defaults) =
            match self.where_rows(&declaration.definition, syntax)? {
                Some((start, list)) => (TypeParameterListId::new(start), list),
                None => (
                    TypeParameterListId::new(coordinate(self.facts.type_parameter_len)?),
                    AtomListId::new(0),
                ),
            };
        Ok(RustFacts {
            ownership,
            lifetimes,
            where_clauses,
            macros: AtomListId::new(0),
            const_defaults,
            free_predicates: backend_semantic::ir::FreePredicateListId::new(free_start),
        })
    }

    /// Builds the ownership-only extension row of a signature carrier: no
    /// lifetimes, no where rows of its own, and no macro spellings yet.
    fn empty_extension(
        &mut self,
        ownership: RustOwnership,
    ) -> Result<RustFacts, RustAuthorityError> {
        let empty_atoms = self.facts.intern_atom_list(&[]).map_err(|_| admission())?;
        Ok(RustFacts {
            ownership,
            lifetimes: empty_atoms,
            where_clauses: TypeParameterListId::new(coordinate(self.facts.type_parameter_len)?),
            macros: empty_atoms,
            const_defaults: empty_atoms,
            free_predicates: backend_semantic::ir::FreePredicateListId::new(coordinate(
                self.facts.free_predicate_len,
            )?),
        })
    }

    /// Interns one declaration's written lifetime parameters as extension
    /// atoms and returns their pooled list.
    fn lifetime_atoms(&mut self, syntax: &SyntaxNode) -> Result<AtomListId, RustAuthorityError> {
        let mut atoms = Vec::new();
        let generics = ast::AnyHasGenericParams::cast(syntax.clone());
        if let Some(list) = generics.as_ref().and_then(|item| item.generic_param_list()) {
            for generic in list.generic_params() {
                let ast::GenericParam::LifetimeParam(lifetime) = generic else {
                    continue;
                };
                let Some(lifetime) = lifetime.lifetime() else {
                    continue;
                };
                let spelling = self.bytes_of_node(lifetime.syntax())?;
                let atom = self.facts.intern_atom(spelling).map_err(|_| admission())?;
                atoms.push(atom);
            }
        }
        self.facts.intern_atom_list(&atoms).map_err(|_| admission())
    }

    /// Pushes exactly one pooled row per HIR generic parameter, in written
    /// order, and preserves each written bound run. HIR gives the ordered
    /// parameter set and kinds (including `Self` and argument `impl Trait`,
    /// which have no written row and are skipped); the exact `T: A + B`
    /// spelling and order come from the mapped written `type_bound_list`.
    /// Inline bounds come first, then every matching `where` predicate in
    /// source order, merged into that parameter's single run so a bound is
    /// never duplicated onto a second parameter row. A lifetime parameter
    /// keeps its lifetime bounds, a const parameter keeps its declared type,
    /// and a written parameter with no HIR counterpart (or a kind mismatch) is
    /// an exact unsupported terminal rather than a shifted or invented row.
    /// A `where` predicate whose subject is not a declared parameter lowers
    /// into the free-predicate lane instead of erasing the predicate.
    /// Const-generic value defaults are preserved as their exact written
    /// expression spellings in a suffix atom list paired with the trailing
    /// const parameters; because Rust makes every declared default trailing
    /// (E0128), no const parameter without a default can follow one that has a
    /// default, and no empty atom is ever emitted.
    fn where_rows(
        &mut self,
        definition: &RustDefinition,
        syntax: &SyntaxNode,
    ) -> Result<Option<(u32, AtomListId)>, RustAuthorityError> {
        let start = coordinate(self.facts.type_parameter_len)?;
        let generics = ast::AnyHasGenericParams::cast(syntax.clone());
        let written: Vec<ast::GenericParam> = generics
            .as_ref()
            .and_then(|generics| generics.generic_param_list())
            .map(|list| list.generic_params().collect())
            .unwrap_or_default();
        let mut written = written.into_iter();
        let mut parameters_written = false;
        // Const-generic value defaults are preserved as their exact written
        // expression spellings, in written order, with a const parameter that
        // declares no default contributing nothing. Rust requires every
        // generic parameter with a default to be trailing (E0128), so the
        // declared const defaults are always a suffix of the const parameter
        // sequence; a consumer pairs this list with that suffix. A parameter
        // without a default is never given an empty atom, which the fragment
        // wire format rejects as invalid.
        let mut const_atoms: Vec<Option<u32>> = Vec::new();
        // Stage every `where` predicate bound in source order, keyed by its
        // written subject spelling, before pairing parameters. A predicate
        // augments a declared parameter when its subject is a simple
        // parameter path (`T`, `'a`); any other subject (`Vec<T>`,
        // `T::Item`, `Self`, and every higher-ranked `for<'a> &'a L: Into`
        // predicate, whose binder names no declaration-scoped row) stages
        // for the free-predicate lane with its exact written subject and
        // bound.
        let mut where_bounds: Vec<WhereBound<'source>> = Vec::new();
        if let Some(where_clause) = generics
            .as_ref()
            .and_then(|generics| generics.where_clause())
        {
            for predicate in where_clause.predicates() {
                let (subject, ty) = if let Some(lifetime) = predicate.lifetime() {
                    (self.bytes_of_node(lifetime.syntax())?, None)
                } else if let Some(ty) = predicate.ty() {
                    let subject = match self.simple_parameter_subject(&ty) {
                        Some(subject) => subject,
                        None => self.bytes_of_node(ty.syntax())?,
                    };
                    (subject, Some(ty))
                } else {
                    return Err(unsupported_generic());
                };
                let Some(bounds) = predicate.type_bound_list() else {
                    return Err(unsupported_generic());
                };
                let bounds: Vec<ast::TypeBound> = bounds.bounds().collect();
                if bounds.is_empty() {
                    return Err(unsupported_generic());
                }
                for bound in bounds {
                    where_bounds.push(WhereBound {
                        subject,
                        ty: ty.clone(),
                        bound,
                        matched: false,
                    });
                }
            }
        }
        // HIR lists lifetimes before type/const parameters regardless of
        // written order, and positional pairing would silently misalign
        // `fn f<T, 'a>`. Pair in written order by kind and name instead:
        // implicit HIR parameters (`Self`, `impl Trait` arguments) have no
        // written row and never pair.
        let mut hir: Vec<ra_ap_hir::GenericParam> = Vec::new();
        for parameter in definition.generic_params(self.database) {
            if matches!(
                parameter,
                ra_ap_hir::GenericParam::TypeParam(type_param)
                    if type_param.is_implicit(self.database)
            ) {
                continue;
            }
            hir.push(parameter);
        }
        for written in written.by_ref() {
            // Written bytes of the parameter name for HIR pairing.
            // Lifetimes keep their leading quote (`'a`); raw identifiers
            // shed `r#` to match HIR's unescaped symbol text.
            let (kind, spelled) = match &written {
                ast::GenericParam::TypeParam(param) => {
                    let Some(name) = param.name() else {
                        continue;
                    };
                    (0_u8, self.bytes_of_node(name.syntax())?)
                }
                ast::GenericParam::ConstParam(param) => {
                    let Some(name) = param.name() else {
                        continue;
                    };
                    (1_u8, self.bytes_of_node(name.syntax())?)
                }
                ast::GenericParam::LifetimeParam(param) => {
                    let Some(lifetime) = param.lifetime() else {
                        continue;
                    };
                    (2_u8, self.bytes_of_node(lifetime.syntax())?)
                }
            };
            let spelled = spelled.strip_prefix(b"r#").unwrap_or(spelled);
            let Some(position) = hir.iter().position(|parameter| {
                let (hir_kind, hir_name) = match parameter {
                    ra_ap_hir::GenericParam::TypeParam(_) => (0_u8, parameter.name(self.database)),
                    ra_ap_hir::GenericParam::ConstParam(_) => (1_u8, parameter.name(self.database)),
                    ra_ap_hir::GenericParam::LifetimeParam(_) => {
                        (2_u8, parameter.name(self.database))
                    }
                };
                hir_kind == kind && hir_name.as_str().as_bytes() == spelled
            }) else {
                return Err(unsupported_generic());
            };
            let parameter = hir.remove(position);
            match (parameter, written) {
                (
                    ra_ap_hir::GenericParam::TypeParam(type_param),
                    ast::GenericParam::TypeParam(param),
                ) => {
                    let Some(name) = param.name() else {
                        continue;
                    };
                    let name = self.bytes_of_node(name.syntax())?;
                    let bounds = param
                        .type_bound_list()
                        .map(|bounds| bounds.bounds().collect::<Vec<_>>())
                        .unwrap_or_default();
                    // A default type binds the declaration-scoped parameter
                    // it augments. Lower it through the same target path as
                    // any other written type so local nominals stay ordinal
                    // references and foreign spellings stay unresolved
                    // externals; a default the lane cannot encode is an
                    // exact terminal, never an erased None.
                    let default = match (param.default_type(), type_param.default(self.database)) {
                        (None, None) => None,
                        (Some(written), semantic) => {
                            Some(self.default_type_target(&written, semantic.as_ref())?)
                        }
                        // A HIR default with no written row cannot be placed
                        // without inventing source evidence.
                        (None, Some(_)) => return Err(unsupported_generic()),
                    };
                    let mut staged_bounds = Vec::with_capacity(bounds.len());
                    for bound in &bounds {
                        staged_bounds.push(self.staged_bound(bound)?);
                    }
                    for where_bound in where_bounds.iter_mut() {
                        if where_bound.subject == name {
                            let staged = self.staged_bound(&where_bound.bound)?;
                            staged_bounds.push(staged);
                            where_bound.matched = true;
                        }
                    }
                    self.facts
                        .push_type_parameter_with_bounds(
                            name,
                            &staged_bounds,
                            default,
                            backend_semantic::ir::ExtensionTypeParameterKind::Type {
                                inference: backend_semantic::ir::TypeParameterInference::Ordinary,
                            },
                            backend_semantic::ir::Variance::Invariant,
                            backend_semantic::ir::TypeParameterRequirements::none(),
                        )
                        .map_err(|_| admission())?;
                    parameters_written = true;
                }
                (ra_ap_hir::GenericParam::ConstParam(_), ast::GenericParam::ConstParam(param)) => {
                    let Some(name) = param.name() else {
                        continue;
                    };
                    // A const value default is preserved as its exact written
                    // expression spelling; a parameter without one contributes
                    // no atom to the defaults suffix.
                    let default_atom = match param.default_val() {
                        Some(value) => {
                            let spelling = self.bytes_of_node(value.syntax())?;
                            Some(self.facts.intern_atom(spelling).map_err(|_| admission())?)
                        }
                        None => None,
                    };
                    let Some(value_type) = param.ty() else {
                        continue;
                    };
                    let spelling = self.bytes_of_node(value_type.syntax())?;
                    let value_type = self
                        .host(
                            Lowered::leaf(unknown_record(
                                TypeReason::NoIrRepresentation,
                                Some(spelling),
                            )),
                            Some(&value_type),
                        )?
                        .ok_or_else(admission)?;
                    self.facts
                        .push_type_parameter_with_bounds(
                            self.bytes_of_node(name.syntax())?,
                            &[],
                            None,
                            backend_semantic::ir::ExtensionTypeParameterKind::ConstValue {
                                value_type,
                            },
                            backend_semantic::ir::Variance::Invariant,
                            backend_semantic::ir::TypeParameterRequirements::none(),
                        )
                        .map_err(|_| admission())?;
                    const_atoms.push(default_atom);
                    parameters_written = true;
                }
                (
                    ra_ap_hir::GenericParam::LifetimeParam(_),
                    ast::GenericParam::LifetimeParam(param),
                ) => {
                    let Some(lifetime) = param.lifetime() else {
                        continue;
                    };
                    let lifetime_bytes = self.bytes_of_node(lifetime.syntax())?;
                    let bounds = param
                        .type_bound_list()
                        .map(|bounds| bounds.bounds().collect::<Vec<_>>())
                        .unwrap_or_default();
                    let mut staged_bounds = Vec::with_capacity(bounds.len());
                    for bound in &bounds {
                        let Some(bound_lifetime) = bound.lifetime() else {
                            return Err(unsupported_generic());
                        };
                        staged_bounds.push(
                            backend_semantic::ir::ExtensionTypeParameterBound::Lifetime(
                                self.bytes_of_node(bound_lifetime.syntax())?,
                            ),
                        );
                    }
                    for where_bound in where_bounds.iter_mut() {
                        if where_bound.subject == lifetime_bytes {
                            let Some(bound_lifetime) = where_bound.bound.lifetime() else {
                                return Err(unsupported_generic());
                            };
                            let staged =
                                backend_semantic::ir::ExtensionTypeParameterBound::Lifetime(
                                    self.bytes_of_node(bound_lifetime.syntax())?,
                                );
                            staged_bounds.push(staged);
                            where_bound.matched = true;
                        }
                    }
                    self.facts
                        .push_type_parameter_with_bounds(
                            lifetime_bytes,
                            &staged_bounds,
                            None,
                            backend_semantic::ir::ExtensionTypeParameterKind::Lifetime,
                            backend_semantic::ir::Variance::Invariant,
                            backend_semantic::ir::TypeParameterRequirements::none(),
                        )
                        .map_err(|_| admission())?;
                    parameters_written = true;
                }
                // A written parameter whose HIR kind disagrees, or a written
                // parameter the HIR does not model, cannot be placed without
                // inventing a declaration-scoped binding.
                _ => return Err(unsupported_generic()),
            }
        }
        if !hir.is_empty() {
            // A modeled HIR parameter with no written row cannot be placed
            // without inventing a declaration-scoped binding.
            return Err(unsupported_generic());
        }
        for where_bound in where_bounds.iter().filter(|bound| !bound.matched) {
            // A `where` predicate whose subject is not a declared parameter
            // (`Vec<T>: Clone`, `T::Item: Clone`, `Self: Sized`) lowers its
            // subject type and ordered bound into the free-predicate lane. A
            // lifetime subject has no subject type row and stays an exact
            // terminal rather than being erased.
            let Some(ty) = where_bound.ty.clone() else {
                return Err(unsupported_generic());
            };
            let subject = self.free_subject_target(&ty)?;
            let bound = self.staged_bound(&where_bound.bound)?;
            self.facts
                .push_free_predicate(subject, &[bound])
                .map_err(|_| admission())?;
        }
        // E0128 makes every declared default trailing, so a const parameter
        // without a default may never follow one that declares a default. A
        // producer that violates the invariant cannot be paired with the
        // suffix list and stays an exact terminal rather than shifting it.
        if let Some(first) = const_atoms.iter().position(Option::is_some)
            && const_atoms[first..].iter().any(Option::is_none)
        {
            return Err(unsupported_generic());
        }
        let const_atoms: Vec<u32> = const_atoms.into_iter().flatten().collect();
        let const_defaults = self
            .facts
            .intern_atom_list(&const_atoms)
            .map_err(|_| admission())?;
        Ok(parameters_written.then_some((start, const_defaults)))
    }

    /// Borrows the written subject of one `where` predicate that augments a
    /// declared parameter: a path with a single, argument-free segment. A
    /// qualified path (`T::Item`), an applied type (`Vec<T>`), a bare `Self`,
    /// or any non-path type has no declared-parameter row and returns `None`.
    fn simple_parameter_subject(&self, ty: &ast::Type) -> Option<&'source [u8]> {
        let ast::Type::PathType(path_type) = ty else {
            return None;
        };
        let path = path_type.path()?;
        let mut segments = path.segments();
        let segment = segments.next()?;
        if segments.next().is_some() || segment.generic_arg_list().is_some() {
            return None;
        }
        let name_ref = segment.name_ref()?;
        let span = self.authority.span(name_ref.syntax()).ok()?;
        self.bytes_of(span).ok()
    }

    /// Lowers one free-predicate subject to its hosted staged coordinate. A
    /// bare `Self` keeps its `SelfType` leaf, a simple path keeps its exact
    /// spelling as a `TypeVar` row, and any other written subject keeps its
    /// full spelling as an honest `NoIrRepresentation` unknown rather than a
    /// fabricated application shape.
    fn free_subject_target(&mut self, ty: &ast::Type) -> Result<u32, RustAuthorityError> {
        if let ast::Type::PathType(path_type) = ty
            && let Some(path) = path_type.path()
            && path.qualifier().is_none()
        {
            let mut segments = path.segments();
            if let Some(segment) = segments.next()
                && segments.next().is_none()
                && segment.generic_arg_list().is_none()
                && let Some(name_ref) = segment.name_ref()
                && let Ok(spelling) = self.bytes_of_node(name_ref.syntax())
                && spelling == b"Self"
            {
                return self
                    .host(
                        Lowered::leaf(SemanticTypeRecord::leaf(SemanticTypeTag::SelfType)),
                        None,
                    )?
                    .ok_or_else(admission);
            }
        }
        if let Some(spelling) = self.simple_parameter_subject(ty) {
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::TypeVar);
            record.text = Some(spelling);
            return self
                .host(Lowered::leaf(record), None)?
                .ok_or_else(admission);
        }
        let spelling = self.bytes_of_node(ty.syntax())?;
        self.host(
            Lowered::leaf(unknown_record(
                TypeReason::NoIrRepresentation,
                Some(spelling),
            )),
            None,
        )?
        .ok_or_else(admission)
    }

    /// Stages one written bound: a lifetime bound keeps its exact spelling, and
    /// a trait bound resolves through the same committed/foreign/unresolved
    /// lattice as an inline bound.
    fn staged_bound(
        &mut self,
        bound: &ast::TypeBound,
    ) -> Result<backend_semantic::ir::ExtensionTypeParameterBound<'source>, RustAuthorityError>
    {
        if let Some(lifetime) = bound.lifetime() {
            return Ok(backend_semantic::ir::ExtensionTypeParameterBound::Lifetime(
                self.bytes_of_node(lifetime.syntax())?,
            ));
        }
        if let Some(bound_ty) = bound.ty() {
            return Ok(backend_semantic::ir::ExtensionTypeParameterBound::Type(
                self.generic_type_bound_target(&bound_ty)?,
            ));
        }
        Err(unsupported_generic())
    }

    /// Resolves one written trait bound. A committed local trait keeps its lane
    /// ordinal; a foreign trait is named by the caller as an unresolved
    /// external row; a local trait with no reserved lane root stays an exact
    /// terminal instead of being invented as an external symbol.
    fn trait_bound_constraint(
        &mut self,
        bound_ty: &ast::Type,
    ) -> Result<TraitBoundTarget, RustAuthorityError> {
        let ast::Type::PathType(path_type) = bound_ty else {
            return Ok(TraitBoundTarget::Unresolved);
        };
        let Some(path) = path_type.path() else {
            return Ok(TraitBoundTarget::Unresolved);
        };
        let Some((ra_ap_hir::PathResolution::Def(ra_ap_hir::ModuleDef::Trait(trait_)), _)) =
            self.authority.resolve_path(&path)
        else {
            return Ok(TraitBoundTarget::Unresolved);
        };
        if let Some(ordinal) = self.ordinal_of_trait(trait_) {
            return Ok(TraitBoundTarget::Committed(ordinal));
        }
        // Every lane-admissible local trait is a reserved type root before any
        // bound resolves, so a local lookup failure means the trait has no
        // lane row at all (e.g. declared in a function body). That cannot be
        // downgraded to an external symbol, so it stays an exact terminal.
        if self
            .authority
            .definition_origin(trait_, SemanticKind::Trait)
            .is_local()
        {
            return Err(unsupported_generic());
        }
        Ok(TraitBoundTarget::Foreign)
    }

    /// Lowers one default type to its scoped row. A bare path naming a
    /// committed lane-local type keeps that type's fact ordinal, so the
    /// default binds the declaration it augments; every other form lowers
    /// through the shared target path (foreign spellings stay unresolved
    /// externals, never fabricated nominals). When the HIR lends no semantic
    /// type for an otherwise-written default (rust-analyzer does not model
    /// every alias generic default), the written spelling is preserved as an
    /// honest `NoIrRepresentation` unknown instead of rejecting the whole
    /// declaration. A default the lane cannot even borrow stays an exact
    /// terminal.
    fn default_type_target(
        &mut self,
        written: &ast::Type,
        semantic: Option<&ra_ap_hir::Type<'_>>,
    ) -> Result<u32, RustAuthorityError> {
        if let ast::Type::PathType(path_type) = written
            && let Some(path) = path_type.path()
            && path.qualifier().is_none()
            && path
                .segments()
                .all(|segment| segment.generic_arg_list().is_none())
            && let Some((ra_ap_hir::PathResolution::Def(ra_ap_hir::ModuleDef::Adt(adt)), _)) =
                self.authority.resolve_path(&path)
            && let Some(ordinal) = self.ordinal_of_adt(adt)
        {
            return Ok(ordinal);
        }
        if let Some(semantic) = semantic {
            return self
                .lower_target(semantic, Some(written.clone()), MAX_TYPE_DEPTH - 1)?
                .ok_or_else(unsupported_generic);
        }
        let spelling = self.bytes_of_node(written.syntax())?;
        self.host(
            Lowered::leaf(unknown_record(
                TypeReason::NoIrRepresentation,
                Some(spelling),
            )),
            Some(written),
        )?
        .ok_or_else(unsupported_generic)
    }

    /// Preserves every non-lifetime bound. A foreign trait keeps its exact
    /// written spelling as an unresolved external row (never a fabricated
    /// nominal); a bound form the HIR does not expose as a trait keeps its
    /// exact written spelling as an explicit `NoIrRepresentation` unknown.
    fn generic_type_bound_target(&mut self, bound: &ast::Type) -> Result<u32, RustAuthorityError> {
        match self.trait_bound_constraint(bound)? {
            TraitBoundTarget::Committed(ordinal) => {
                return self.lower_trait_bound_application(bound, ordinal, MAX_TYPE_DEPTH - 1);
            },
            TraitBoundTarget::Foreign => {
                let spelling = self.bytes_of_node(bound.syntax())?;
                let record = self.unresolved_record_with(Some(spelling));
                return self
                    .host(Lowered::leaf(record), Some(bound))?
                    .ok_or_else(unsupported_generic);
            }
            TraitBoundTarget::Unresolved => {}
        }
        let spelling = self.bytes_of_node(bound.syntax())?;
        self.host(
            Lowered::leaf(unknown_record(
                TypeReason::NoIrRepresentation,
                Some(spelling),
            )),
            Some(bound),
        )?
        .ok_or_else(unsupported_generic)
    }

    /// Registers one pushed declaration row, its HIR definition ordinal, and
    /// its lane ordinal for the later phases. The authority's whole-item span
    /// is bound here as the row's provenance span: it is the exact basis every
    /// later owner-relative occurrence span is measured against, so the
    /// shared containment law holds by construction.
    fn register(
        &mut self,
        ordinal: usize,
        index: usize,
        declaration: &Decl<'source>,
        extension: RustFacts,
    ) -> Result<(), RustAuthorityError> {
        let Some(ordinal) = u32::try_from(ordinal).ok() else {
            return Ok(());
        };
        let span = StagedSourceSpan::new(declaration.span.start, declaration.span.end)
            .ok_or_else(admission)?;
        self.facts
            .attach_source_span(ordinal, span)
            .map_err(|fault| parentage_fault(ordinal, declaration.name.len(), fault))?;
        self.rows.push(Row {
            ordinal,
            name: self.name_of(declaration)?,
            span: declaration.span,
            extension,
        });
        match &declaration.definition {
            RustDefinition::Field(field) => self.fields.push((*field, ordinal)),
            other => {
                if let Some(definition) = module_def(other) {
                    self.definitions.push((definition, ordinal));
                }
            }
        }
        if let Some(slot) = self.ordinals.get_mut(index) {
            *slot = Some(ordinal);
        }
        Ok(())
    }

    /// Looks up the pushed ordinal of one lane-local ADT.
    fn ordinal_of_adt(&self, adt: ra_ap_hir::Adt) -> Option<u32> {
        self.definitions
            .iter()
            .find(|(definition, _)| definition == &ra_ap_hir::ModuleDef::Adt(adt))
            .map(|(_, ordinal)| *ordinal)
    }

    /// Looks up the pushed ordinal of one lane-local trait.
    fn ordinal_of_trait(&self, trait_: ra_ap_hir::Trait) -> Option<u32> {
        self.definitions
            .iter()
            .find(|(definition, _)| definition == &ra_ap_hir::ModuleDef::Trait(trait_))
            .map(|(_, ordinal)| *ordinal)
    }

    /// Looks up the pushed ordinal of any pushed HIR definition.
    fn ordinal_of_definition(&self, definition: &ra_ap_hir::ModuleDef) -> Option<u32> {
        self.definitions
            .iter()
            .find(|(known, _)| known == definition)
            .map(|(_, ordinal)| *ordinal)
    }

    /// Looks up the pushed ordinal of one pushed named field.
    fn ordinal_of_field(&self, field: &ra_ap_hir::Field) -> Option<u32> {
        self.fields
            .iter()
            .find(|(known, _)| known == field)
            .map(|(_, ordinal)| *ordinal)
    }

    /// Picks the innermost pushed row whose span contains `span`.
    fn owner_of(&self, span: ByteSpan) -> Option<u32> {
        let mut low = 0;
        let mut high = self.owner_order.len();
        while low < high {
            let middle = low + (high - low) / 2;
            let row = &self.rows[self.owner_order[middle]];
            if row.span.start <= span.start {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        self.owner_order[..low].iter().rev().find_map(|index| {
            let row = &self.rows[*index];
            (span.end <= row.span.end).then_some(row.ordinal)
        })
    }

    /// Picks the innermost strictly-containing pushed row for `span`,
    /// excluding the row itself. Siblings from macro expansion may share a
    /// span with their site; equal spans never parent each other, so such
    /// siblings resolve to their shared outer container (or root) instead
    /// of an arbitrary sibling. A row with no strictly-containing row is a
    /// parentage root.
    fn enclosing_row(&self, span: ByteSpan, ordinal: u32) -> Option<u32> {
        let mut low = 0;
        let mut high = self.owner_order.len();
        while low < high {
            let middle = low + (high - low) / 2;
            let row = &self.rows[self.owner_order[middle]];
            if row.span.start <= span.start {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        self.owner_order[..low].iter().rev().find_map(|index| {
            let row = &self.rows[*index];
            (row.ordinal != ordinal
                && span.end <= row.span.end
                && (row.span.start < span.start || span.end < row.span.end))
                .then_some(row.ordinal)
        })
    }

    /// Binds lexical parentage from declaration spans after every row
    /// exists. Only relationships whose owner survived emission are bound:
    /// a row with no strictly-containing row is an explicit parentage
    /// root, never a fabricated child. Signature carriers are bound by
    /// their emitters; anonymous hosted rows keep their honest unavailable
    /// state until a use-site owner exists.
    fn emit_parentage(&mut self) -> Result<(), RustAuthorityError> {
        for index in 0..self.rows.len() {
            let (ordinal, span, name_len) = {
                let row = &self.rows[index];
                (row.ordinal, row.span, row.name.len())
            };
            match self.enclosing_row(span, ordinal) {
                Some(parent) => self
                    .facts
                    .attach_parent(ordinal, parent)
                    .map_err(|fault| parentage_fault(ordinal, name_len, fault))?,
                None => self
                    .facts
                    .mark_parentage_root(ordinal)
                    .map_err(|fault| parentage_fault(ordinal, name_len, fault))?,
            }
        }
        Ok(())
    }

    /// Builds the source-ordered declaration index once, after all declaration
    /// rows exist. Rows are nested or disjoint in the syntax tree; reverse
    /// search therefore selects the innermost containing declaration while
    /// avoiding a full scan for every occurrence.
    fn rebuild_owner_order(&mut self) {
        self.owner_order = (0..self.rows.len()).collect();
        self.owner_order.sort_by_key(|index| {
            let span = self.rows[*index].span;
            (span.start, span.end)
        });
    }

    /// Lowers a type against the fact ordinal that the caller will push next.
    fn lower_pending_type(
        &mut self,
        semantic: &ra_ap_hir::Type<'_>,
        anchor: Option<&ast::Type>,
        depth: usize,
    ) -> Result<Lowered<'source>, RustAuthorityError> {
        self.lower_type(semantic, anchor, depth)
    }

    /// Hosts one lowered child position and returns the backward coordinate
    /// its parent record references: a nominal leaf names its fact ordinal
    /// directly, every other leaf interns as a zero-child anonymous row
    /// (foreign unknowns deduplicated by spelling), and every compound
    /// becomes a backward `Parameter` carrier fact whose own type children
    /// are the hosted coordinates of its components. Anonymous rows never
    /// carry pooled children — the pooled lane re-lays children per row, so
    /// a child-bearing row cannot be addressed soundly — and compounds never
    /// stay rowless, so no proven shape is ever erased. `Ok(None)` means the
    /// position could not be hosted at all (no pushed fact exists yet to own
    /// a row), and the parent folds to a rowless honest unknown.
    fn host(
        &mut self,
        lowered: Lowered<'source>,
        anchor: Option<&ast::Type>,
    ) -> Result<Option<u32>, RustAuthorityError> {
        // A computed (inferred) type's nested positions are never a pending
        // declared fact: no new row follows this lowering, so the reserved-
        // anchor convention below (which assumes the caller pushes a fact at
        // exactly `self.facts.len()` immediately after) would stage an
        // anonymous row owned by an ordinal that is never fulfilled. Route
        // straight into the computed lane instead, which owns every child
        // position through the already-hosted computed row.
        if self.computed_owner.is_some() {
            return self.host_computed(lowered).map(Some);
        }
        if lowered.children.is_empty() {
            if let Some(NominalRef::Local(target)) = lowered.record.nominal {
                return Ok(Some(target.raw));
            }
            let foreign = match (lowered.record.tag, lowered.record.nominal) {
                (SemanticTypeTag::Unknown, _) => lowered.record.text.map(|text| (text, None)),
                (SemanticTypeTag::Nominal, Some(NominalRef::External(external))) => {
                    lowered.record.text.map(|text| (text, Some(external)))
                }
                _ => None,
            };
            if let Some((text, external)) = foreign {
                for (known, known_external, ordinal) in &self.foreign_rows {
                    if *known == text && *known_external == external {
                        return Ok(Some(*ordinal));
                    }
                }
                // The dedup table is a bounded lane: a distinct foreign
                // spelling beyond its cap must reject exactly, never silently
                // mint an unbounded duplicate row.
                if self.foreign_rows.len() >= MAX_DEDUPED_FOREIGN_ROWS {
                    return Err(admission());
                }
            }
            let anchor_row = coordinate(self.facts.len())?;
            let row = self
                .facts
                .intern_reserved_anchor_type_row(anchor_row, lowered.record)
                .map_err(|_| admission())?;
            if let Some((text, external)) = foreign {
                self.foreign_rows.push((text, external, row));
            }
            return Ok(Some(row));
        }
        let Some(name) = self.carrier_name(anchor) else {
            return self.host(
                Lowered::leaf(unknown_record(TypeReason::OracleGap, None)),
                None,
            );
        };
        // A compound spelling lowers to the same carrier every time it is
        // written, so a repeat occurrence must reuse the first carrier
        // ordinal. Emitting one row per occurrence minted byte-identical
        // twins that the image build honestly rejected as a duplicate.
        for (known, ordinal) in &self.carrier_rows {
            if *known == name {
                return Ok(Some(*ordinal));
            }
        }
        if self.carrier_rows.len() >= MAX_DEDUPED_CARRIER_ROWS {
            return Err(admission());
        }
        let extension = self.empty_extension(RustOwnership::Value)?;
        let mut fact = SemanticFact::new(EntityKind::Parameter, name, LEAF_PRODUCT)
            .typed(lowered.record)
            .with_extension(EmissionExtension::Rust(extension));
        for target in lowered.children {
            fact = fact.type_child(target, None, 0);
        }
        let ordinal = coordinate(push(self.facts, fact)?)?;
        self.carrier_rows.push((name, ordinal));
        Ok(Some(ordinal))
    }

    /// Borrows the exact written text of one compound anchor as its carrier
    /// fact's name, falling back to the written leaf spelling; `None` when
    /// the anchor spells nothing this source owns.
    fn carrier_name(&self, anchor: Option<&ast::Type>) -> Option<&'source [u8]> {
        let anchor = anchor?;
        if let Ok(bytes) = self.bytes_of_node(anchor.syntax()) {
            return Some(bytes);
        }
        self.written_type_name(Some(anchor))
    }

    /// Lowers one HIR type position into its lattice record, using the
    /// written syntax only to borrow exact spellings (foreign leaf names,
    /// array lengths, reference lifetimes).
    fn lower_type(
        &mut self,
        semantic: &ra_ap_hir::Type<'_>,
        anchor: Option<&ast::Type>,
        depth: usize,
    ) -> Result<Lowered<'source>, RustAuthorityError> {
        if depth == 0 {
            return Ok(Lowered::leaf(unknown_record(
                TypeReason::TruncatedAtDepthLimit,
                None,
            )));
        }
        if semantic.is_unknown() {
            return Ok(Lowered::leaf(unknown_record(TypeReason::OracleGap, None)));
        }
        if let Some((inner, mutability)) = semantic.as_reference() {
            return self.lower_reference(&inner, mutability, anchor, depth);
        }
        if let Some((inner, mutability)) = semantic.as_raw_ptr() {
            return self.lower_pointer(&inner, mutability, anchor, depth);
        }
        if let Some(builtin) = semantic.as_builtin() {
            return Ok(Lowered::leaf(builtin_record(builtin)));
        }
        if semantic.is_never() {
            return Ok(Lowered::leaf(SemanticTypeRecord::leaf(
                SemanticTypeTag::Never,
            )));
        }
        if semantic.is_unit() {
            return Ok(Lowered::leaf(SemanticTypeRecord::leaf(
                SemanticTypeTag::Tuple,
            )));
        }
        if semantic.is_tuple() {
            return self.lower_tuple(semantic, anchor, depth);
        }
        if let Some(element) = semantic.as_slice() {
            return self.lower_child_pair(&element, SemanticTypeTag::Slice, anchor, depth);
        }
        if let Some((element, _length)) = semantic.as_array(self.database) {
            return self.lower_array(&element, anchor, depth);
        }
        if semantic.is_closure() || semantic.as_callable(self.database).is_some() {
            return self.lower_callable(semantic, anchor, depth);
        }
        if let Some((adt, arguments)) = semantic.as_adt_with_args() {
            return self.lower_adt(adt, &arguments, anchor, depth);
        }
        if let Some(trait_) = semantic.as_dyn_trait() {
            let written = self.written_type_name(anchor);
            return match self.ordinal_of_trait(trait_) {
                Some(ordinal) => {
                    let bound_ty = written_trait_bounds(anchor)
                        .first()
                        .and_then(|bound| bound.ty());
                    let target = match bound_ty {
                        Some(bound_ty) => self.lower_trait_bound_application(
                            &bound_ty,
                            ordinal,
                            depth - 1,
                        )?,
                        None => ordinal,
                    };
                    Ok(Lowered {
                        record: SemanticTypeRecord::leaf(SemanticTypeTag::DynTrait),
                        children: vec![target],
                    })
                }
                None => Ok(Lowered::leaf(self.unresolved_record_with(written))),
            };
        }
        if let Some(bounds) = semantic.as_impl_traits(self.database) {
            return self.lower_impl_trait(bounds.collect(), anchor);
        }
        if let Some(parameter) = semantic.as_type_param(self.database) {
            return self.lower_type_param(parameter, anchor);
        }
        Ok(Lowered::leaf(unknown_record(TypeReason::OracleGap, None)))
    }

    /// Lowers one shared or exclusive borrow over its referent row.
    fn lower_reference(
        &mut self,
        inner: &ra_ap_hir::Type<'_>,
        mutability: ra_ap_hir::Mutability,
        anchor: Option<&ast::Type>,
        depth: usize,
    ) -> Result<Lowered<'source>, RustAuthorityError> {
        let target = match self.lower_target(inner, child_anchor(anchor, 0), depth)? {
            Some(target) => target,
            None => return Ok(self.folded_rowless(anchor)),
        };
        let mut record = primitive_record(PrimitiveShape::Reference, 0);
        if matches!(mutability, ra_ap_hir::Mutability::Mut) {
            record.payload1 = REFERENCE_MUTABLE_FLAG;
        }
        record.text = self.written_lifetime(anchor);
        Ok(Lowered {
            record,
            children: vec![target],
        })
    }

    /// Lowers one raw pointer over its pointee row.
    fn lower_pointer(
        &mut self,
        inner: &ra_ap_hir::Type<'_>,
        mutability: ra_ap_hir::Mutability,
        anchor: Option<&ast::Type>,
        depth: usize,
    ) -> Result<Lowered<'source>, RustAuthorityError> {
        let target = match self.lower_target(inner, child_anchor(anchor, 0), depth)? {
            Some(target) => target,
            None => return Ok(self.folded_rowless(anchor)),
        };
        let shape = match mutability {
            ra_ap_hir::Mutability::Mut => PrimitiveShape::MutPointer,
            ra_ap_hir::Mutability::Shared => PrimitiveShape::ConstPointer,
        };
        Ok(Lowered {
            record: primitive_record(shape, 0),
            children: vec![target],
        })
    }

    /// Lowers one one-child compound (a slice) over its element row.
    fn lower_child_pair(
        &mut self,
        inner: &ra_ap_hir::Type<'_>,
        tag: SemanticTypeTag,
        anchor: Option<&ast::Type>,
        depth: usize,
    ) -> Result<Lowered<'source>, RustAuthorityError> {
        let target = match self.lower_target(inner, child_anchor(anchor, 0), depth)? {
            Some(target) => target,
            None => return Ok(self.folded_rowless(anchor)),
        };
        Ok(Lowered {
            record: SemanticTypeRecord::leaf(tag),
            children: vec![target],
        })
    }

    /// Lowers one array: the element row plus the exact written length text
    /// the Array tag requires. Without a written length the position folds to
    /// the honest gap reason instead of a fabricated size.
    fn lower_array(
        &mut self,
        element: &ra_ap_hir::Type<'_>,
        anchor: Option<&ast::Type>,
        depth: usize,
    ) -> Result<Lowered<'source>, RustAuthorityError> {
        let target = match self.lower_target(element, child_anchor(anchor, 0), depth)? {
            Some(target) => target,
            None => return Ok(self.folded_rowless(anchor)),
        };
        let Some(text) = (match anchor {
            Some(ast::Type::ArrayType(array)) => array
                .const_arg()
                .and_then(|argument| argument.expr())
                .and_then(|expr| self.bytes_of_node(expr.syntax()).ok()),
            _ => None,
        }) else {
            return Ok(Lowered::leaf(unknown_record(TypeReason::OracleGap, None)));
        };
        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::ArrayConstExpression);
        record.text = Some(text);
        Ok(Lowered {
            record,
            children: vec![target],
        })
    }

    /// Lowers one tuple: one child per element, unlabeled, because HIR tuple
    /// positions carry no element names.
    fn lower_tuple(
        &mut self,
        semantic: &ra_ap_hir::Type<'_>,
        anchor: Option<&ast::Type>,
        depth: usize,
    ) -> Result<Lowered<'source>, RustAuthorityError> {
        let fields = semantic.tuple_fields(self.database);
        if fields.len() > MAX_COMPOUND_CHILDREN {
            return Ok(Lowered::leaf(unknown_record(TypeReason::OracleGap, None)));
        }
        let mut children = Vec::new();
        for (position, element) in fields.iter().enumerate() {
            match self.lower_target(element, child_anchor(anchor, position), depth - 1)? {
                Some(target) => children.push(target),
                None => return Ok(self.folded_rowless(anchor)),
            }
        }
        Ok(Lowered {
            record: SemanticTypeRecord::leaf(SemanticTypeTag::Tuple),
            children,
        })
    }

    /// Lowers one callable position (function value, closure, or fn trait
    /// impl) as a structural `FunctionPointer` over its signature.
    fn lower_callable(
        &mut self,
        semantic: &ra_ap_hir::Type<'_>,
        anchor: Option<&ast::Type>,
        depth: usize,
    ) -> Result<Lowered<'source>, RustAuthorityError> {
        let Some(callable) = semantic.as_callable(self.database) else {
            return Ok(Lowered::leaf(unknown_record(TypeReason::OracleGap, None)));
        };
        let parameters = callable.params();
        let returns = callable.return_type().clone();
        if parameters.len() + usize::from(!returns.is_unit()) > MAX_COMPOUND_CHILDREN {
            return Ok(Lowered::leaf(unknown_record(TypeReason::OracleGap, None)));
        }
        let mut children = Vec::new();
        for (position, parameter) in parameters.iter().enumerate() {
            let lowered_parameter = parameter.ty().clone();
            let written = callable_anchor(anchor, CallablePart::Parameter(position));
            match self.lower_target(&lowered_parameter, written, depth - 1)? {
                Some(target) => children.push(target),
                None => return Ok(self.folded_rowless(anchor)),
            }
        }
        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::FunctionPointer);
        if !returns.is_unit() {
            let written = callable_anchor(anchor, CallablePart::Return);
            match self.lower_target(&returns, written, depth - 1)? {
                Some(target) => children.push(target),
                None => return Ok(self.folded_rowless(anchor)),
            }
            record.payload1 = SemanticTypeRecord::FUNCTION_RESULT_COUNT_ONE;
        }
        Ok(Lowered { record, children })
    }

    /// Lowers one ADT position: a lane-local ADT names its backward nominal
    /// row directly (or an `Apply` over its written arguments), and a foreign
    /// ADT keeps its exact written spelling as the honest unresolved row.
    fn lower_adt(
        &mut self,
        adt: ra_ap_hir::Adt,
        arguments: &[Option<ra_ap_hir::Type<'_>>],
        anchor: Option<&ast::Type>,
        depth: usize,
    ) -> Result<Lowered<'source>, RustAuthorityError> {
        let written = self.written_type_name(anchor);
        let Some(ordinal) = self.ordinal_of_adt(adt) else {
            let base_record = self.foreign_adt_record(adt, written);
            let argument_children =
                self.lower_written_application_arguments(arguments, anchor, depth)?;
            if argument_children.is_empty() {
                return Ok(Lowered::leaf(base_record));
            }
            if argument_children.len() + 1 > MAX_COMPOUND_CHILDREN {
                return Ok(Lowered::leaf(match written {
                    Some(text) => unknown_record(TypeReason::NoIrRepresentation, Some(text)),
                    None => unknown_record(TypeReason::OracleGap, None),
                }));
            }
            let Some(base) = self.host(Lowered::leaf(base_record), None)? else {
                return Ok(self.folded_rowless(anchor));
            };
            let mut children = vec![base];
            children.extend(argument_children);
            return Ok(Lowered {
                record: SemanticTypeRecord::leaf(SemanticTypeTag::Apply),
                children,
            });
        };
        let argument_children = self.lower_written_application_arguments(arguments, anchor, depth)?;
        if argument_children.is_empty() {
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Nominal);
            record.nominal = Some(NominalRef::Local(EntityId::new(ordinal)));
            return Ok(Lowered::leaf(record));
        }
        if argument_children.len() + 1 > MAX_COMPOUND_CHILDREN {
            return Ok(Lowered::leaf(match written {
                Some(text) => unknown_record(TypeReason::NoIrRepresentation, Some(text)),
                None => unknown_record(TypeReason::OracleGap, None),
            }));
        }
        let mut children = vec![ordinal];
        children.extend(argument_children);
        Ok(Lowered {
            record: SemanticTypeRecord::leaf(SemanticTypeTag::Apply),
            children,
        })
    }

    /// Lowers every written generic argument on one application anchor,
    /// including associated bindings, const arguments, and lifetimes.
    fn lower_written_application_arguments(
        &mut self,
        arguments: &[Option<ra_ap_hir::Type<'_>>],
        anchor: Option<&ast::Type>,
        depth: usize,
    ) -> Result<Vec<u32>, RustAuthorityError> {
        let written_count = written_generic_argument_count(anchor);
        if written_count == 0 {
            return Ok(Vec::new());
        }
        let hir_type_arguments: Vec<ra_ap_hir::Type<'_>> = arguments
            .iter()
            .filter_map(|argument| argument.as_ref())
            .cloned()
            .collect();
        let mut hir_cursor = 0usize;
        let mut children = Vec::with_capacity(written_count);
        for position in 0..written_count {
            let Some(generic_argument) = generic_argument_at(anchor, position) else {
                return Ok(Vec::new());
            };
            let target = match generic_argument {
                ast::GenericArg::TypeArg(_) => {
                    let Some(hir_argument) = hir_type_arguments.get(hir_cursor) else {
                        return Ok(Vec::new());
                    };
                    hir_cursor += 1;
                    self.lower_target(
                        hir_argument,
                        type_argument_anchor(anchor, position),
                        depth - 1,
                    )?
                }
                other => {
                    let lowered = self.lower_written_generic_argument(other, depth - 1)?;
                    self.host(lowered, anchor)?
                }
            };
            match target {
                Some(target) => children.push(target),
                None => return Ok(Vec::new()),
            }
        }
        Ok(children)
    }

    /// Lowers one non-`TypeArg` generic argument from its written syntax.
    fn lower_written_generic_argument(
        &mut self,
        argument: ast::GenericArg,
        depth: usize,
    ) -> Result<Lowered<'source>, RustAuthorityError> {
        match argument {
            ast::GenericArg::AssocTypeArg(binding) => {
                let name = binding
                    .name_ref()
                    .and_then(|name| self.bytes_of_node(name.syntax()).ok());
                let value = if let Some(ty) = binding.ty() {
                    match self.authority.semantics.resolve_type(&ty) {
                        Some(semantic) => self.lower_type(&semantic, Some(&ty), depth)?,
                        None => Lowered::leaf(unknown_record(TypeReason::OracleGap, None)),
                    }
                } else if let Some(konst) = binding
                    .const_arg()
                    .and_then(|argument| argument.expr())
                {
                    self.const_argument_lowered(self.bytes_of_node(konst.syntax()).ok())
                } else {
                    Lowered::leaf(unknown_record(TypeReason::OracleGap, None))
                };
                let Some(target) = self.host(value, binding.ty().as_ref())? else {
                    return Ok(Lowered::leaf(unknown_record(TypeReason::OracleGap, None)));
                };
                Ok(self.assoc_binding_lowered(name, target))
            }
            ast::GenericArg::ConstArg(konst) => {
                let text = konst
                    .expr()
                    .and_then(|expr| self.bytes_of_node(expr.syntax()).ok());
                Ok(self.const_argument_lowered(text))
            }
            ast::GenericArg::LifetimeArg(lifetime) => {
                let text = lifetime
                    .lifetime()
                    .and_then(|lifetime| self.bytes_of_node(lifetime.syntax()).ok());
                Ok(self.lifetime_argument_lowered(text))
            }
            ast::GenericArg::TypeArg(type_argument) => {
                if let Some(ty) = type_argument.ty() {
                    match self.authority.semantics.resolve_type(&ty) {
                        Some(semantic) => self.lower_type(&semantic, Some(&ty), depth),
                        None => Ok(Lowered::leaf(unknown_record(TypeReason::OracleGap, None))),
                    }
                } else {
                    Ok(Lowered::leaf(unknown_record(TypeReason::OracleGap, None)))
                }
            }
        }
    }

    /// `Item = u8` is not `Item = String`, and neither is a bare `Iterator`.
    fn assoc_binding_lowered(
        &self,
        name: Option<&'source [u8]>,
        value: u32,
    ) -> Lowered<'source> {
        let Some(name) = name else {
            return Lowered::leaf(unknown_record(TypeReason::OracleGap, None));
        };
        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::QualifiedPath);
        record.text = Some(name);
        Lowered {
            record,
            children: vec![value],
        }
    }

    /// `Foo<N>` and `Foo<M>` stay distinct even when the const is not a type.
    fn const_argument_lowered(&self, text: Option<&'source [u8]>) -> Lowered<'source> {
        let Some(text) = text else {
            return Lowered::leaf(unknown_record(TypeReason::OracleGap, None));
        };
        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::TypeVar);
        record.text = Some(text);
        Lowered::leaf(record)
    }

    /// A lifetime generic argument keeps its exact written spelling.
    fn lifetime_argument_lowered(&self, text: Option<&'source [u8]>) -> Lowered<'source> {
        let Some(text) = text else {
            return Lowered::leaf(unknown_record(TypeReason::OracleGap, None));
        };
        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Inferred);
        record.text = Some(text);
        Lowered::leaf(record)
    }

    /// Lowers one committed trait bound with its written generic arguments.
    fn lower_trait_bound_application(
        &mut self,
        bound: &ast::Type,
        trait_ordinal: u32,
        depth: usize,
    ) -> Result<u32, RustAuthorityError> {
        let argument_children =
            self.lower_written_application_arguments(&[], Some(bound), depth)?;
        if argument_children.is_empty() {
            return Ok(trait_ordinal);
        }
        if argument_children.len() + 1 > MAX_COMPOUND_CHILDREN {
            return self
                .host(
                    Lowered::leaf(unknown_record(
                        TypeReason::NoIrRepresentation,
                        self.bytes_of_node(bound.syntax()).ok(),
                    )),
                    Some(bound),
                )?
                .ok_or_else(unsupported_generic);
        }
        let mut children = vec![trait_ordinal];
        children.extend(argument_children);
        self.host(
            Lowered {
                record: SemanticTypeRecord::leaf(SemanticTypeTag::Apply),
                children,
            },
            Some(bound),
        )?
        .ok_or_else(unsupported_generic)
    }

    /// Lowers one `impl Trait` position over its local bound rows; any bound
    /// resolving outside the fragment folds to the written unresolved row.
    /// `impl Trait` spells no path, so the shared leaf-name reader returns
    /// `None` for it; the whole written bound run is borrowed instead,
    /// mirroring the declaration-site foreign bound's exact written text.
    fn lower_impl_trait(
        &mut self,
        bounds: Vec<ra_ap_hir::Trait>,
        anchor: Option<&ast::Type>,
    ) -> Result<Lowered<'source>, RustAuthorityError> {
        let written = anchor.and_then(|anchor| self.bytes_of_node(anchor.syntax()).ok());
        if bounds.len() > MAX_COMPOUND_CHILDREN {
            return Ok(Lowered::leaf(match written {
                Some(text) => unknown_record(TypeReason::NoIrRepresentation, Some(text)),
                None => unknown_record(TypeReason::OracleGap, None),
            }));
        }
        let written_bounds = written_trait_bounds(anchor);
        let mut children = Vec::new();
        for (index, trait_) in bounds.into_iter().enumerate() {
            match self.ordinal_of_trait(trait_) {
                Some(ordinal) => {
                    let bound_ty = written_bounds.get(index).and_then(|bound| bound.ty());
                    let target = match bound_ty {
                        Some(bound_ty) => self.lower_trait_bound_application(
                            &bound_ty,
                            ordinal,
                            MAX_TYPE_DEPTH - 1,
                        )?,
                        None => ordinal,
                    };
                    children.push(target);
                }
                None => {
                    return Ok(Lowered::leaf(self.unresolved_record_with(written)));
                }
            }
        }
        Ok(Lowered {
            record: SemanticTypeRecord::leaf(SemanticTypeTag::ImplTrait),
            children,
        })

    }

    /// Lowers one generic-parameter use: the implicit trait `Self` stays a
    /// `SelfType` leaf, every written parameter keeps its exact spelling as
    /// a `TypeVar` row, and an unwritten use folds to the gap reason.
    fn lower_type_param(
        &mut self,
        parameter: ra_ap_hir::TypeParam,
        anchor: Option<&ast::Type>,
    ) -> Result<Lowered<'source>, RustAuthorityError> {
        if parameter.is_implicit(self.database) {
            return Ok(Lowered::leaf(SemanticTypeRecord::leaf(
                SemanticTypeTag::SelfType,
            )));
        }
        let Some(text) = self.written_type_name(anchor) else {
            return Ok(Lowered::leaf(unknown_record(TypeReason::OracleGap, None)));
        };
        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::TypeVar);
        record.text = Some(text);
        Ok(Lowered::leaf(record))
    }

    /// Lowers one position and interns it into a pooled row coordinate (or
    /// returns the nominal fact ordinal it names); `None` folds the parent.
    fn lower_target(
        &mut self,
        semantic: &ra_ap_hir::Type<'_>,
        anchor: Option<ast::Type>,
        depth: usize,
    ) -> Result<Option<u32>, RustAuthorityError> {
        let lowered = self.lower_type(semantic, anchor.as_ref(), depth)?;
        self.host(lowered, anchor.as_ref())
    }

    /// The rowless fold for a compound whose pooled rows cannot be lawfully
    /// interned yet: the exact written spelling, or the gap reason.
    fn folded_rowless(&self, anchor: Option<&ast::Type>) -> Lowered<'source> {
        Lowered::leaf(self.unresolved_record(anchor))
    }

    /// Same as [`Emitter::unresolved_record`] for an already-borrowed
    /// spelling.
    fn unresolved_record_with(
        &self,
        written: Option<&'source [u8]>,
    ) -> SemanticTypeRecord<'source> {
        match written {
            Some(text) => unknown_record(TypeReason::UnresolvedExternal, Some(text)),
            None => unknown_record(TypeReason::OracleGap, None),
        }
    }

    /// The record for one foreign ADT that rust-analyzer resolved: an
    /// external nominal whose authority is the ADT's defining crate module
    /// (`core::option`, `alloc::boxed`), displayed by the exact written
    /// spelling. The authority is derived from the oracle's resolution, never
    /// from the spelling, so std and prelude types are never unresolved.
    /// Without a written spelling the display cell cannot be borrowed and the
    /// position keeps the gap reason.
    fn foreign_adt_record(
        &self,
        adt: ra_ap_hir::Adt,
        written: Option<&'source [u8]>,
    ) -> SemanticTypeRecord<'source> {
        let (Some(text), Some(fragment)) = (written, self.foreign_module_fragment(adt)) else {
            return self.unresolved_record_with(written);
        };
        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Nominal);
        record.nominal = Some(NominalRef::External(ExternalEntityRef::bind(fragment, 0)));
        record.text = Some(text);
        record
    }

    /// The external fragment authority of one foreign ADT's defining module:
    /// the canonical crate name followed by the module path from the crate
    /// root, the same module-granular authority the TypeScript lane binds.
    fn foreign_module_fragment(&self, adt: ra_ap_hir::Adt) -> Option<ExternalFragmentId> {
        let module = adt.module(self.database);
        let krate = module.krate(self.database).display_name(self.database)?;
        let mut canonical = String::from(CARGO_ECOSYSTEM);
        canonical.push(':');
        canonical.push_str(krate.canonical_name().as_str());
        for segment in module.path_to_root(self.database).into_iter().rev() {
            if let Some(name) = segment.name(self.database) {
                canonical.push_str("::");
                canonical.push_str(name.as_str());
            }
        }
        Some(ExternalFragmentId::from_canonical_bytes(canonical.as_bytes()))
    }

    /// Borrows the written lifetime behind one reference anchor.
    fn written_lifetime(&self, anchor: Option<&ast::Type>) -> Option<&'source [u8]> {
        match anchor? {
            ast::Type::RefType(reference) => {
                let lifetime = reference.lifetime()?;
                let span = self.authority.span(lifetime.syntax()).ok()?;
                self.bytes_of(span).ok()
            }
            _ => None,
        }
    }

    /// Attaches each macro invocation spelling to the innermost pushed
    /// declaration containing it, replacing that row's extension facts.
    fn attach_macros(&mut self) -> Result<(), RustAuthorityError> {
        if self.macro_sites.is_empty() {
            return Ok(());
        }
        let mut owners: HashMap<u32, usize> = HashMap::new();
        let mut owned_atoms: Vec<Vec<u32>> = Vec::new();
        let mut spellings: HashMap<&'source [u8], u32> = HashMap::new();
        for site in &self.macro_sites {
            let Some(owner) = self.owner_of(site.span) else {
                continue;
            };
            let interned = match spellings.get(site.spelling) {
                Some(atom) => *atom,
                None => {
                    let atom = self
                        .facts
                        .intern_atom(site.spelling)
                        .map_err(|_| admission())?;
                    spellings.insert(site.spelling, atom);
                    atom
                }
            };
            let position = match owners.get(&owner) {
                Some(position) => *position,
                None => {
                    let position = owned_atoms.len();
                    owners.insert(owner, position);
                    owned_atoms.push(Vec::new());
                    position
                }
            };
            if let Some(atoms) = owned_atoms.get_mut(position) {
                atoms.push(interned);
            }
        }
        for (owner, position) in owners {
            let Some(atoms) = owned_atoms.get(position) else {
                continue;
            };
            let list = self
                .facts
                .intern_atom_list(atoms)
                .map_err(|_| admission())?;
            let Some(row) = self.rows.iter_mut().find(|row| row.ordinal == owner) else {
                continue;
            };
            row.extension.macros = list;
            let updated = row.extension;
            let ordinal = usize::try_from(owner).map_err(|_| admission())?;
            self.facts
                .attach_extension_with_type_parameters(
                    ordinal,
                    EmissionExtension::Rust(updated),
                    self.facts
                        .captured_type_parameter_range(ordinal)
                        .map_err(|_| admission())?,
                )
                .map_err(|_| admission())?;
            self.facts
                .set_free_predicate_range(
                    ordinal,
                    &EmissionExtension::Rust(updated),
                    self.facts
                        .captured_free_predicate_range(ordinal)
                        .map_err(|_| admission())?,
                )
                .map_err(|_| admission())?;
        }
        Ok(())
    }

    /// Emits every oracle-resolved occurrence: method calls, top-level path
    /// references, and macro invocations, each owned by the innermost
    /// containing declaration and measured relative to that owner.
    fn emit_occurrences(&mut self) -> Result<(), RustAuthorityError> {
        let authority = self.authority;
        let method_calls: Vec<_> = authority.method_calls().collect();
        let mut emitted_method_spans = Vec::new();
        for call in &method_calls {
            let Some(name) = call.syntax.name_ref() else {
                continue;
            };
            let span = match call.projected_span {
                Some(span) => span,
                None => authority.span(name.syntax())?,
            };
            if emitted_method_spans.contains(&span) {
                continue;
            }
            emitted_method_spans.push(span);
            let definition = call.target.map(ra_ap_hir::ModuleDef::from);
            // A dispatch the oracle resolved is oracle tier; a method call
            // rust-analyzer could not resolve stays syntactic confidence
            // with its written spelling as a foreign key.
            let confidence = occurrence_confidence(call.target.is_some());
            self.emit_one_occurrence(
                span,
                ReferenceKind::MethodCall,
                ResolvedTarget::Definition(definition),
                confidence,
                None,
            )?;
        }
        let accesses: Vec<RustFieldAccess> = authority.field_accesses().collect();
        let mut emitted_field_spans = Vec::new();
        for access in &accesses {
            let Some(name) = access.syntax.name_ref() else {
                continue;
            };
            let span = authority.span(name.syntax())?;
            if emitted_field_spans.contains(&span) {
                continue;
            }
            emitted_field_spans.push(span);
            let confidence = occurrence_confidence(access.target.is_some());
            let target = access
                .target
                .map_or(ResolvedTarget::Definition(None), ResolvedTarget::NamedField);
            self.emit_one_occurrence(span, ReferenceKind::FieldAccess, target, confidence, None)?;
        }
        for (span, target) in self.record_literal_field_accesses()? {
            if emitted_field_spans.contains(&span) {
                continue;
            }
            emitted_field_spans.push(span);
            let confidence = occurrence_confidence(target.is_some());
            let target = target.map_or(ResolvedTarget::Definition(None), ResolvedTarget::NamedField);
            self.emit_one_occurrence(span, ReferenceKind::FieldAccess, target, confidence, None)?;
        }
        for (span, target) in self.macro_field_accesses()? {
            if emitted_field_spans.contains(&span) {
                continue;
            }
            emitted_field_spans.push(span);
            let confidence = occurrence_confidence(target.is_some());
            let target = target.map_or(ResolvedTarget::Definition(None), ResolvedTarget::NamedField);
            self.emit_one_occurrence(span, ReferenceKind::FieldAccess, target, confidence, None)?;
        }
        let mut emitted_function_call_spans = Vec::new();
        let paths: Vec<_> = authority.top_level_paths().collect();
        for path in &paths {
            // A path rust-analyzer could not resolve keeps its exact written
            // spelling as a self-describing foreign key instead of being
            // dropped: the reference site is authority-proven even where the
            // target is not.
            let resolved = authority
                .resolve_path(path)
                .map(|(resolution, _)| resolution);
            // The occurrence site is the written name extent — the final
            // path segment's identifier — never the whole qualified or
            // generic path, so the committed site's source bytes are the
            // identifier a reader searched for. The key text stays the whole
            // written path, which is the self-describing foreign spelling.
            let full = authority.span(path.syntax())?;
            let span = path
                .segments()
                .last()
                .and_then(|segment| segment.name_ref())
                .and_then(|name| authority.span(name.syntax()).ok())
                .unwrap_or(full);
            let written = self.bytes_of(full)?;
            let kind = match &resolved {
                Some(resolution) => reference_kind(path, resolution),
                None => unresolved_reference_kind(path),
            };
            let confidence = match resolved {
                Some(_) => OccurrenceConfidence::Oracle,
                None => OccurrenceConfidence::Syntactic,
            };
            if kind == ReferenceKind::FunctionCall {
                if emitted_function_call_spans.contains(&span) {
                    continue;
                }
                emitted_function_call_spans.push(span);
            }
            self.emit_one_occurrence(
                span,
                kind,
                ResolvedTarget::Definition(resolved.as_ref().and_then(path_definition)),
                confidence,
                Some(written),
            )?;
        }
        for (span, target) in self.macro_function_calls()? {
            if emitted_function_call_spans.contains(&span) {
                continue;
            }
            emitted_function_call_spans.push(span);
            let confidence = occurrence_confidence(target.is_some());
            self.emit_one_occurrence(
                span,
                ReferenceKind::FunctionCall,
                ResolvedTarget::Definition(target),
                confidence,
                None,
            )?;
        }
        let sites: Vec<MacroSite<'source>> = self.macro_sites.to_vec();
        for site in &sites {
            let confidence = occurrence_confidence(site.resolved);
            self.emit_one_occurrence(
                site.span,
                ReferenceKind::MacroInvocation,
                ResolvedTarget::Definition(None),
                confidence,
                Some(site.spelling),
            )?;
        }
        Ok(())
    }

    /// Streams every written record-literal field name with the named field
    /// rust-analyzer resolved it to, when it resolved one.
    fn record_literal_field_accesses(
        &self,
    ) -> Result<Vec<(ByteSpan, Option<ra_ap_hir::Field>)>, RustAuthorityError> {
        let authority = self.authority;
        let mut accesses = Vec::new();
        for syntax in authority.root.syntax().descendants() {
            let Some(field) = ast::RecordExprField::cast(syntax) else {
                continue;
            };
            let Some(name) = field.field_name() else {
                continue;
            };
            let span = authority.span(name.syntax())?;
            let target = authority
                .semantics
                .resolve_record_field(&field)
                .map(|(field, _, _)| field);
            accesses.push((span, target));
        }
        Ok(accesses)
    }

    /// Streams field accesses discovered through macro expansion. A macro
    /// argument is a token tree in the source file; descending each token
    /// reaches the expanded `FieldExpr` or record-literal field
    /// rust-analyzer inferred and projects the written field identifier back
    /// onto this source buffer.
    fn macro_field_accesses(
        &self,
    ) -> Result<Vec<(ByteSpan, Option<ra_ap_hir::Field>)>, RustAuthorityError> {
        let authority = self.authority;
        let mut accesses = Vec::new();
        for macro_call in authority.macro_calls() {
            let Some(token_tree) = macro_call.token_tree() else {
                continue;
            };
            for token in token_tree
                .syntax()
                .descendants_with_tokens()
                .filter_map(|element| element.into_token())
            {
                for descended in authority.semantics.descend_into_macros_no_opaque(token, false) {
                    if let Some((projected_span, target)) =
                        Self::projected_macro_field_access(authority, &descended)
                    {
                        accesses.push((projected_span, target));
                    }
                }
            }
        }
        Ok(accesses)
    }

    /// Projects one macro-descended token onto a field-access site when it
    /// names a `FieldExpr` or record-literal field in the expansion.
    fn projected_macro_field_access(
        authority: &RustAuthority<'_>,
        descended: &ra_ap_hir::InFile<ra_ap_syntax::SyntaxToken>,
    ) -> Option<(ByteSpan, Option<ra_ap_hir::Field>)> {
        let parent = descended.value.parent()?;
        for node in parent.ancestors() {
            if let Some(syntax) = ast::FieldExpr::cast(node.clone()) {
                let Some(name) = syntax.name_ref() else {
                    continue;
                };
                let Ok(Some(projected_span)) = authority.projected_span(name.syntax()) else {
                    continue;
                };
                let target = authority.resolve_field_target(&syntax);
                return Some((projected_span, target));
            }
            if let Some(field) = ast::RecordExprField::cast(node.clone()) {
                let Some(name) = field.field_name() else {
                    continue;
                };
                let Ok(Some(projected_span)) = authority.projected_span(name.syntax()) else {
                    continue;
                };
                let target = authority
                    .semantics
                    .resolve_record_field(&field)
                    .map(|(field, _, _)| field);
                return Some((projected_span, target));
            }
        }
        None
    }

    /// Streams function calls discovered through macro expansion. A macro
    /// argument is a token tree in the source file; descending each token
    /// reaches the expanded `CallExpr` rust-analyzer inferred and projects
    /// the written callee identifier back onto this source buffer.
    fn macro_function_calls(
        &self,
    ) -> Result<Vec<(ByteSpan, Option<ra_ap_hir::ModuleDef>)>, RustAuthorityError> {
        let authority = self.authority;
        let mut calls = Vec::new();
        for macro_call in authority.macro_calls() {
            let Some(token_tree) = macro_call.token_tree() else {
                continue;
            };
            for token in token_tree
                .syntax()
                .descendants_with_tokens()
                .filter_map(|element| element.into_token())
            {
                for descended in authority.semantics.descend_into_macros_no_opaque(token, false) {
                    let Some(call) = descended
                        .value
                        .parent()
                        .and_then(|node| node.ancestors().find_map(ast::CallExpr::cast))
                    else {
                        continue;
                    };
                    let Some(callee) = call.expr() else {
                        continue;
                    };
                    let Some(path_expr) = ast::PathExpr::cast(callee.syntax().clone()) else {
                        continue;
                    };
                    let Some(path) = path_expr.path() else {
                        continue;
                    };
                    if !is_call_position(&path) {
                        continue;
                    }
                    let Some(name) = path
                        .segments()
                        .last()
                        .and_then(|segment| segment.name_ref())
                    else {
                        continue;
                    };
                    let Ok(Some(projected_span)) = authority.projected_span(name.syntax()) else {
                        continue;
                    };
                    let target = authority
                        .resolve_path(&path)
                        .and_then(|(resolution, _)| path_definition(&resolution));
                    calls.push((projected_span, target));
                }
            }
        }
        Ok(calls)
    }

    /// Emits one occurrence fact, resolving the target through the pushed
    /// definition and field tables and folding to a self-describing foreign
    /// key when the target lives outside this fragment. The reference site
    /// (`span`) is the written name extent; `key` carries the canonical
    /// written spelling the foreign key names when it differs from the site
    /// (a whole qualified path, a macro path).
    ///
    /// Owner-less disposition: positions with no owning declaration
    /// (module-level `use` items) stay unowned and unemitted. The shared
    /// wire format carries no owner-less occurrence — `push_occurrence`
    /// requires an owning fact ordinal — and the shared containment law
    /// rejects an occurrence span that escapes its owner's provenance span
    /// at build time, so the only possible emission would fabricate a
    /// containing relation that no declaration proves: the exact boundary
    /// the C lane records as "containment is not ownership". The skip is
    /// the honest unowned disposition, never a silent truncation of the
    /// written spelling, which the foreign key still carries whenever an
    /// owned sibling site or a doc link names it.
    fn emit_one_occurrence(
        &mut self,
        span: ByteSpan,
        kind: ReferenceKind,
        resolved: ResolvedTarget,
        confidence: OccurrenceConfidence,
        key: Option<&'source [u8]>,
    ) -> Result<(), RustAuthorityError> {
        let Some(owner) = self.owner_of(span) else {
            return Ok(());
        };
        let owner_span = self
            .rows
            .iter()
            .find(|row| row.ordinal == owner)
            .map(|row| row.span);
        let Some(owner_span) = owner_span else {
            return Ok(());
        };
        // A macro invocation's written reference site is its path spelling,
        // not the whole invocation call: the call's token text can carry a
        // line-continuation backslash inside a string literal, which is not a
        // canonical foreign path. Every other occurrence kind borrows the
        // exact written span.
        let written = match key {
            Some(key) => key,
            None => self.bytes_of(span)?,
        };
        let ordinal = match resolved {
            ResolvedTarget::Definition(Some(definition)) => self.ordinal_of_definition(&definition),
            ResolvedTarget::NamedField(field) => self.ordinal_of_field(&field),
            ResolvedTarget::Definition(None) => None,
        };
        let target = match ordinal {
            Some(ordinal) => OccurrenceTarget::Local(EntityId::new(ordinal)),
            None => foreign_universe(written)?,
        };
        let relative = relative_span(span, owner_span)?;
        self.facts
            .push_occurrence(
                owner,
                Occurrence {
                    target,
                    kind,
                    confidence,
                    span: relative,
                },
            )
            .map_err(|_| admission())
    }

    /// Streams every declaration's Rustdoc into borrowed fragments: prose
    /// lines, fenced code, and intra-doc links whose target names a pushed
    /// declaration link locally.
    fn emit_docs(&mut self, declarations: &[Decl<'source>]) -> Result<(), RustAuthorityError> {
        for index in 0..declarations.len() {
            let Some(Some(owner)) = self.ordinals.get(index).copied() else {
                continue;
            };
            // The RA declaration visitor owns the Rustdoc plane even when it
            // lends no lines for this declaration.
            self.facts
                .mark_documentation_captured(owner)
                .map_err(|_| admission())?;
            let declaration = &declarations[index];
            let mut lines: Vec<&'source [u8]> = Vec::new();
            {
                let authority = self.authority;
                let emitter = &*self;
                authority.visit_documentation(&declaration.syntax, |line| {
                    if let Some(span) = emitter.span_of_text(line)
                        && let Ok(bytes) = emitter.bytes_of(span)
                    {
                        lines.push(bytes);
                    }
                });
            }
            self.push_doc_lines(owner, &lines)?;
        }
        Ok(())
    }

    /// Pushes the borrowed doc lines of one declaration as fragments.
    fn push_doc_lines(
        &mut self,
        owner: u32,
        lines: &[&'source [u8]],
    ) -> Result<(), RustAuthorityError> {
        let mut fragments: Vec<DocFragmentInput<'source>> = Vec::new();
        let mut inside_fence = false;
        for line in lines {
            if line.is_empty() {
                if !matches!(fragments.last(), Some(DocFragmentInput::SoftBreak)) {
                    fragments.push(DocFragmentInput::SoftBreak);
                }
                continue;
            }
            if line.starts_with(b"```") {
                inside_fence = !inside_fence;
                continue;
            }
            if inside_fence {
                fragments.push(DocFragmentInput::Code(line));
                continue;
            }
            split_links(line, &mut fragments);
        }
        let locals: Vec<(&'source [u8], u32)> = self
            .rows
            .iter()
            .map(|row| (row.name, row.ordinal))
            .collect();
        for fragment in fragments
            .iter()
            .copied()
            .map(|fragment| resolve_link(fragment, &locals))
        {
            self.facts
                .push_doc(owner, fragment)
                .map_err(|_| admission())?;
        }
        Ok(())
    }

    /// Emits one leaf `Reexport` entity per resolvable local binding in a
    /// written `use` item; a glob import proves no honest single-name
    /// declaration, so its written law admits no fact at all.
    fn emit_reexports(&mut self) -> Result<(), RustAuthorityError> {
        for reexport in self.authority.reexports() {
            if !reexport.resolved {
                continue;
            }
            let name_span = self.authority.span(&reexport.name)?;
            let name = self.bytes_of(name_span)?;
            // Rust's single-name law (E0428) applies to a reexported binding
            // exactly as it does to any other declaration: an authority that
            // does not evaluate every cfg gate (or a facade re-exporting the
            // same name from two branches) can stream cfg-disjoint twins.
            // Reuse the declaration walk's dedup key so a later twin is
            // dropped instead of minting a byte-identical `Reexport`
            // identity the image build must reject.
            let scope = self.enclosing_item_span(reexport.item.syntax())?;
            if !self
                .scoped_names
                .insert((scope, EntityKind::Reexport as u8, name.to_vec()))
            {
                continue;
            }
            let extension = self.empty_extension(RustOwnership::Value)?;
            let fact = SemanticFact::new(EntityKind::Reexport, name, LEAF_PRODUCT)
                .with_visibility(self.visibility_of(&reexport.item.syntax().clone()))
                .typed(unknown_record(TypeReason::NoIrRepresentation, Some(name)))
                .with_extension(EmissionExtension::Rust(extension));
            let ordinal = coordinate(push(self.facts, fact)?)?;
            self.rows.push(Row {
                ordinal,
                name,
                span: self.authority.span(reexport.item.syntax())?,
                extension,
            });
        }
        Ok(())
    }

    /// Captures the written visibility prefix of one syntax item.
    fn visibility_of(&self, syntax: &ra_ap_syntax::SyntaxNode) -> backend_semantic::ir::Visibility {
        let Some(visibility) = ast::AnyHasVisibility::cast(syntax.clone())
            .and_then(|item| item.visibility())
        else {
            return backend_semantic::ir::Visibility::Private;
        };
        let range = visibility.syntax().text_range();
        let Some(start) = usize::try_from(u32::from(range.start())).ok() else {
            return backend_semantic::ir::Visibility::Unknown;
        };
        let Some(end) = usize::try_from(u32::from(range.end())).ok() else {
            return backend_semantic::ir::Visibility::Unknown;
        };
        let Some(bytes) = self.source.get(start..end) else {
            return backend_semantic::ir::Visibility::Unknown;
        };
        if bytes == b"pub(crate)" {
            backend_semantic::ir::Visibility::Package
        } else if bytes.starts_with(b"pub(") {
            backend_semantic::ir::Visibility::Restricted
        } else {
            backend_semantic::ir::Visibility::Public
        }
    }

    /// Emits proven let initializer and method-call result types in source order.
    fn emit_computed(&mut self) -> Result<(), RustAuthorityError> {
        let mut expressions = Vec::new();
        for expression in self.authority.inferred_let_initializers() {
            let span = self.authority.span(expression.expression.syntax())?;
            if let Some(inferred) = expression
                .inferred
                .filter(|inferred| !inferred.original.is_unknown())
            {
                expressions.push((span, inferred.original, None));
            }
        }
        for call in self.authority.method_calls() {
            let span = match call.projected_span {
                Some(span) => span,
                None => self.authority.span(call.syntax.syntax())?,
            };
            if let Some(inferred) = call
                .inferred
                .filter(|inferred| !inferred.original.is_unknown())
            {
                expressions.push((span, inferred.original, None));
            }
        }
        expressions.sort_by_key(|(span, _, _)| span.start);
        for (span, semantic, anchor) in expressions {
            let Some(owner) = self.owner_of(span) else {
                continue;
            };
            self.computed_owner = Some(owner);
            let lowered = self.lower_pending_type(&semantic, anchor.as_ref(), MAX_TYPE_DEPTH)?;
            self.host_computed(lowered)?;
            self.computed_owner = None;
        }
        Ok(())
    }

    /// Commits one lowered inferred type into the schema-2 computed segment.
    fn host_computed(&mut self, lowered: Lowered<'source>) -> Result<u32, RustAuthorityError> {
        let owner = self.computed_owner.ok_or_else(admission)?;
        for target in lowered.children {
            self.facts
                .computed_type_child(target, None, 0)
                .map_err(|_| admission())?;
        }
        self.facts
            .intern_computed_type_row(owner, lowered.record)
            .map_err(|_| admission())
    }

    /// Pushes documentation belonging to each emitted re-export declaration.
    fn emit_reexport_docs(&mut self) -> Result<(), RustAuthorityError> {
        for reexport in self.authority.reexports() {
            if !reexport.resolved {
                continue;
            }
            let name = self.bytes_of(self.authority.span(&reexport.name)?)?;
            let item_span = self.authority.span(reexport.item.syntax())?;
            let Some(owner) = self
                .rows
                .iter()
                .find(|row| row.name == name && row.span == item_span)
                .map(|row| row.ordinal)
            else {
                continue;
            };
            let mut lines = Vec::new();
            {
                let authority = self.authority;
                let emitter = &*self;
                authority.visit_documentation(reexport.item.syntax(), |line| {
                    if let Some(span) = emitter.span_of_text(line)
                        && let Ok(bytes) = emitter.bytes_of(span)
                    {
                        lines.push(bytes);
                    }
                });
            }
            self.push_doc_lines(owner, &lines)?;
        }
        Ok(())
    }
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

    /// Returns the absolute byte offset of `field_name` inside `surrounding`
    /// within `source`.
    fn field_name_offset_in_source(
        source: &str,
        surrounding: &str,
        field_name: &str,
    ) -> Result<usize, TestError> {
        let anchor = source
            .find(surrounding)
            .ok_or(TestError::Missing("fixture snippet in source"))?;
        let offset = surrounding
            .find(field_name)
            .ok_or(TestError::Missing("field name in fixture snippet"))?;
        Ok(anchor + offset)
    }

    /// Projects one emitted `FieldAccess` occurrence onto its absolute source
    /// span through the owning declaration's provenance span.
    fn field_access_absolute_span(
        source: &str,
        owner: u32,
        occurrence: &Occurrence<'_>,
    ) -> Result<(usize, usize), TestError> {
        let ir = owned_ir(source)?;
        let owner_span = ir
            .item(backend_semantic::ir::EntityId::new(owner))
            .and_then(|entity| entity.source())
            .ok_or(TestError::Missing("owner provenance span"))?;
        let base = usize::try_from(owner_span.start())?;
        let start = base + usize::try_from(occurrence.span.start)?;
        let end = base + usize::try_from(occurrence.span.end)?;
        Ok((start, end))
    }

    /// Proves the emitted `FieldAccess` occurrence site is exactly the written
    /// field name. A sibling `VariableUse` on the same identifier does not
    /// satisfy this check.
    fn assert_field_access_site_is_field_name(
        source: &str,
        owner: u32,
        occurrence: &Occurrence<'_>,
        field_name: &[u8],
        expected_name_at: Option<usize>,
    ) -> Result<(usize, usize), TestError> {
        if occurrence.kind != ReferenceKind::FieldAccess {
            return Err(TestError::Missing("FieldAccess occurrence"));
        }
        let (start, end) = field_access_absolute_span(source, owner, occurrence)?;
        if source.as_bytes().get(start..end) != Some(field_name) {
            return Err(TestError::Missing("FieldAccess site is the field name"));
        }
        if let Some(expected) = expected_name_at {
            if start != expected {
                return Err(TestError::Missing("FieldAccess at expected field-name offset"));
            }
        }
        Ok((start, end))
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

    /// A function call inside a macro argument is authority-proven through
    /// macro descent even though the written tree parses the argument as a
    /// token tree. The occurrence keeps the invocation-site callee spelling,
    /// its owning function, and the resolved local function target.
    #[test]
    fn macro_function_calls_commit_oracle_local_call_occurrences() -> Result<(), TestError> {
        let source = "macro_rules! invoke {\n    ($e:expr) => { $e };\n}\n\npub fn target() {}\n\npub fn caller() {\n    invoke!(target());\n}\n";
        let view = lower(source)?;
        let caller = fact_of(&view, b"caller", EntityKind::Function)?;
        let target = fact_of(&view, b"target", EntityKind::Function)?;
        let function_calls = occurrences(&view)?
            .into_iter()
            .filter(|(_, occurrence)| occurrence.kind == ReferenceKind::FunctionCall)
            .collect::<Vec<_>>();
        if function_calls.len() != 1 {
            return Err(TestError::Missing("exactly one macro function call occurrence"));
        }
        let (owner, call) = function_calls[0];
        if owner != caller {
            return Err(TestError::Missing("function call owned by caller"));
        }
        if call.target != OccurrenceTarget::Local(backend_semantic::ir::EntityId::new(target))
            || call.confidence != OccurrenceConfidence::Oracle
        {
            return Err(TestError::Missing(
                "oracle-local function target for macro function call",
            ));
        }
        let name_at = source
            .find("invoke!(target())")
            .ok_or(TestError::Missing("macro invocation in fixture source"))?
            + "invoke!(".len();
        let ir = owned_ir(source)?;
        let mut verified = false;
        for (_, occurrence) in ir.link_occurrences() {
            let Some(link) = ir.link(occurrence.link) else {
                continue;
            };
            if link.kind != backend_semantic::ir::LinkKind::Calls {
                continue;
            }
            let Some(site) = occurrence.source else {
                continue;
            };
            let start = usize::try_from(site.start())?;
            let end = usize::try_from(site.end())?;
            if source.as_bytes().get(start..end) != Some(b"target") {
                continue;
            }
            verified = true;
            if start != name_at {
                return Err(TestError::Missing("function call at invocation spelling"));
            }
            let backend_semantic::ir::LinkTarget::Local(link_target) = link.target else {
                return Err(TestError::Missing("local function link target"));
            };
            if ir.item(link_target).is_none_or(|item| item.name() != b"target") {
                return Err(TestError::Missing("target function link target"));
            }
        }
        if !verified {
            return Err(TestError::Missing("invocation-site function call spelling"));
        }
        Ok(())
    }

    /// A source-level function call whose argument contains a macro is already
    /// emitted by the path walk; macro descent must not emit the same
    /// callee-name span again.
    #[test]
    fn macro_receiver_function_calls_emit_once() -> Result<(), TestError> {
        let source = "macro_rules! identity {\n    ($e:expr) => { $e };\n}\n\npub fn target() {}\n\npub fn caller() {\n    target(identity!(()));\n}\n";
        let view = lower(source)?;
        let caller = fact_of(&view, b"caller", EntityKind::Function)?;
        let target = fact_of(&view, b"target", EntityKind::Function)?;
        let function_calls = occurrences(&view)?
            .into_iter()
            .filter(|(_, occurrence)| occurrence.kind == ReferenceKind::FunctionCall)
            .collect::<Vec<_>>();
        if function_calls.len() != 1 {
            return Err(TestError::Missing("exactly one function call occurrence"));
        }
        let (owner, call) = function_calls[0];
        if owner != caller {
            return Err(TestError::Missing("function call owned by caller"));
        }
        if call.target != OccurrenceTarget::Local(backend_semantic::ir::EntityId::new(target))
            || call.confidence != OccurrenceConfidence::Oracle
        {
            return Err(TestError::Missing(
                "oracle-local function target for macro-receiver call",
            ));
        }
        Ok(())
    }

    /// A record-literal field name is a field occurrence even though it is not
    /// a `FieldExpr`; the written `score` in `Point { score: 1 }` resolves to
    /// the local field at oracle confidence, owned by `build`.
    #[test]
    fn struct_literal_field_emits_oracle_local_field_access() -> Result<(), TestError> {
        let source = "pub struct Point {\n    pub score: u8,\n}\n\npub fn build() -> Point {\n    Point { score: 1 }\n}\n";
        let literal = "Point { score: 1 }";
        let name_at = field_name_offset_in_source(source, literal, "score")?;
        let view = lower(source)?;
        let build = fact_of(&view, b"build", EntityKind::Function)?;
        let score = fact_of(&view, b"score", EntityKind::Field)?;
        let field_accesses = occurrences(&view)?
            .into_iter()
            .filter(|(_, occurrence)| occurrence.kind == ReferenceKind::FieldAccess)
            .collect::<Vec<_>>();
        if field_accesses.len() != 1 {
            return Err(TestError::Missing("exactly one struct literal field access"));
        }
        let (owner, access) = field_accesses[0];
        if owner != build {
            return Err(TestError::Missing("field access owned by build"));
        }
        if access.target != OccurrenceTarget::Local(backend_semantic::ir::EntityId::new(score))
            || access.confidence != OccurrenceConfidence::Oracle
        {
            return Err(TestError::Missing(
                "oracle-local field target for struct literal field",
            ));
        }
        assert_field_access_site_is_field_name(source, owner, &access, b"score", Some(name_at))?;
        Ok(())
    }

    /// Field-init shorthand still emits exactly one field access for the
    /// written field name; a variable use of the same name may remain.
    #[test]
    fn struct_literal_field_shorthand_emits_exactly_one_field_access() -> Result<(), TestError> {
        let source = "pub struct Point {\n    pub score: u8,\n}\n\npub fn build(score: u8) -> Point {\n    Point { score }\n}\n";
        let literal = "Point { score }";
        let name_at = field_name_offset_in_source(source, literal, "score")?;
        let view = lower(source)?;
        let build = fact_of(&view, b"build", EntityKind::Function)?;
        let score = fact_of(&view, b"score", EntityKind::Field)?;
        let field_accesses = occurrences(&view)?
            .into_iter()
            .filter(|(_, occurrence)| occurrence.kind == ReferenceKind::FieldAccess)
            .collect::<Vec<_>>();
        if field_accesses.len() != 1 {
            return Err(TestError::Missing(
                "exactly one struct literal shorthand field access",
            ));
        }
        let (owner, access) = field_accesses[0];
        if owner != build {
            return Err(TestError::Missing("field access owned by build"));
        }
        if access.target != OccurrenceTarget::Local(backend_semantic::ir::EntityId::new(score))
            || access.confidence != OccurrenceConfidence::Oracle
        {
            return Err(TestError::Missing(
                "oracle-local field target for shorthand struct literal field",
            ));
        }
        let (site_start, site_end) =
            assert_field_access_site_is_field_name(source, owner, &access, b"score", Some(name_at))?;
        let type_at = source
            .find(literal)
            .ok_or(TestError::Missing("literal in fixture source"))?;
        if site_start == type_at || source.as_bytes().get(site_start..site_end) == Some(b"Point") {
            return Err(TestError::Missing(
                "FieldAccess is the field name, not the record type path",
            ));
        }
        Ok(())
    }

    /// A record literal only inside a macro argument is authority-proven
    /// through macro descent with the invocation-site field spelling.
    #[test]
    fn struct_literal_field_inside_macro_emits_once() -> Result<(), TestError> {
        let source = "macro_rules! build {\n    ($e:expr) => { $e };\n}\n\npub struct Point {\n    pub score: u8,\n}\n\npub fn build() -> Point {\n    build!(Point { score: 1 })\n}\n";
        let invocation = "build!(Point { score: 1 })";
        let name_at = field_name_offset_in_source(source, invocation, "score")?;
        let view = lower(source)?;
        let build = fact_of(&view, b"build", EntityKind::Function)?;
        let score = fact_of(&view, b"score", EntityKind::Field)?;
        let field_accesses = occurrences(&view)?
            .into_iter()
            .filter(|(_, occurrence)| occurrence.kind == ReferenceKind::FieldAccess)
            .collect::<Vec<_>>();
        if field_accesses.len() != 1 {
            return Err(TestError::Missing(
                "exactly one macro struct literal field access",
            ));
        }
        let (owner, access) = field_accesses[0];
        if owner != build {
            return Err(TestError::Missing("field access owned by build"));
        }
        if access.target != OccurrenceTarget::Local(backend_semantic::ir::EntityId::new(score))
            || access.confidence != OccurrenceConfidence::Oracle
        {
            return Err(TestError::Missing(
                "oracle-local field target for macro struct literal field",
            ));
        }
        assert_field_access_site_is_field_name(source, owner, &access, b"score", Some(name_at))?;
        Ok(())
    }

    /// A struct literal wrapped entirely by a macro is authority-proven only
    /// through macro descent at the invocation-site field spelling.
    #[test]
    fn struct_literal_field_macro_only_emits_once() -> Result<(), TestError> {
        let source = "macro_rules! identity {\n    ($e:expr) => { $e };\n}\n\npub struct Point {\n    pub score: u8,\n}\n\npub fn build() -> Point {\n    identity!(Point { score: 1 })\n}\n";
        let invocation = "identity!(Point { score: 1 })";
        let name_at = field_name_offset_in_source(source, invocation, "score")?;
        let view = lower(source)?;
        let build = fact_of(&view, b"build", EntityKind::Function)?;
        let score = fact_of(&view, b"score", EntityKind::Field)?;
        let field_accesses = occurrences(&view)?
            .into_iter()
            .filter(|(_, occurrence)| occurrence.kind == ReferenceKind::FieldAccess)
            .collect::<Vec<_>>();
        if field_accesses.len() != 1 {
            return Err(TestError::Missing(
                "exactly one macro-only struct literal field access",
            ));
        }
        let (owner, access) = field_accesses[0];
        if owner != build {
            return Err(TestError::Missing("field access owned by build"));
        }
        if access.target != OccurrenceTarget::Local(backend_semantic::ir::EntityId::new(score))
            || access.confidence != OccurrenceConfidence::Oracle
        {
            return Err(TestError::Missing(
                "oracle-local field target for macro-only struct literal field",
            ));
        }
        assert_field_access_site_is_field_name(source, owner, &access, b"score", Some(name_at))?;
        Ok(())
    }

    /// A source record literal whose field value contains a macro is walked
    /// directly and reached by macro descent; the shared `emitted_field_spans`
    /// list must commit exactly one field access at the field-name span.
    #[test]
    fn struct_literal_field_macro_descent_shares_emitted_field_spans() -> Result<(), TestError> {
        let source = "macro_rules! identity {\n    ($e:expr) => { $e };\n}\n\npub struct Point {\n    pub score: u8,\n}\n\npub fn build() -> Point {\n    Point { score: identity!(1) }\n}\n";
        let literal = "Point { score: identity!(1) }";
        let name_at = field_name_offset_in_source(source, literal, "score")?;
        let view = lower(source)?;
        let build = fact_of(&view, b"build", EntityKind::Function)?;
        let score = fact_of(&view, b"score", EntityKind::Field)?;
        let field_accesses = occurrences(&view)?
            .into_iter()
            .filter(|(_, occurrence)| occurrence.kind == ReferenceKind::FieldAccess)
            .collect::<Vec<_>>();
        if field_accesses.len() != 1 {
            return Err(TestError::Missing(
                "exactly one struct literal field access with macro in value",
            ));
        }
        let (owner, access) = field_accesses[0];
        if owner != build {
            return Err(TestError::Missing("field access owned by build"));
        }
        if access.target != OccurrenceTarget::Local(backend_semantic::ir::EntityId::new(score))
            || access.confidence != OccurrenceConfidence::Oracle
        {
            return Err(TestError::Missing(
                "oracle-local field target for source literal with macro value",
            ));
        }
        assert_field_access_site_is_field_name(source, owner, &access, b"score", Some(name_at))?;
        Ok(())
    }

    /// An unresolved record-literal field name stays a foreign spelling at
    /// syntactic confidence instead of fabricating a local field target.
    #[test]
    fn struct_literal_field_unresolved_stays_foreign() -> Result<(), TestError> {
        let source = "pub struct Point {\n    pub score: u8,\n}\n\npub fn build() -> Point {\n    Point { missing: 1 }\n}\n";
        let literal = "Point { missing: 1 }";
        let name_at = field_name_offset_in_source(source, literal, "missing")?;
        let view = lower(source)?;
        let build = fact_of(&view, b"build", EntityKind::Function)?;
        let field_accesses = occurrences(&view)?
            .into_iter()
            .filter(|(_, occurrence)| occurrence.kind == ReferenceKind::FieldAccess)
            .collect::<Vec<_>>();
        if field_accesses.len() != 1 {
            return Err(TestError::Missing(
                "exactly one unresolved struct literal field access",
            ));
        }
        let (owner, access) = field_accesses[0];
        if owner != build {
            return Err(TestError::Missing("field access owned by build"));
        }
        if access.confidence != OccurrenceConfidence::Syntactic {
            return Err(TestError::Missing(
                "syntactic confidence for unresolved struct literal field",
            ));
        }
        let OccurrenceTarget::Foreign(key) = access.target else {
            return Err(TestError::Missing(
                "foreign target for unresolved struct literal field",
            ));
        };
        if key.path != "missing" || key.display != "missing" {
            return Err(TestError::Missing(
                "missing written spelling as the foreign key",
            ));
        }
        assert_field_access_site_is_field_name(source, owner, &access, b"missing", Some(name_at))?;
        Ok(())
    }

    /// The record type path in a struct literal is not a field access.
    #[test]
    fn struct_literal_field_does_not_emit_type_path() -> Result<(), TestError> {
        let source = "pub struct Point {\n    pub score: u8,\n}\n\npub fn build() -> Point {\n    Point { score: 1 }\n}\n";
        let literal = "Point { score: 1 }";
        let type_at = source
            .find(literal)
            .ok_or(TestError::Missing("literal in fixture source"))?;
        let view = lower(source)?;
        let score = fact_of(&view, b"score", EntityKind::Field)?;
        let field_accesses = occurrences(&view)?
            .into_iter()
            .filter(|(_, occurrence)| occurrence.kind == ReferenceKind::FieldAccess)
            .collect::<Vec<_>>();
        if field_accesses.len() != 1 {
            return Err(TestError::Missing("only the field name is a field access"));
        }
        let (owner, access) = field_accesses[0];
        if access.target != OccurrenceTarget::Local(backend_semantic::ir::EntityId::new(score)) {
            return Err(TestError::Missing("field access targets score, not Point"));
        }
        let (site_start, site_end) =
            assert_field_access_site_is_field_name(source, owner, &access, b"score", None)?;
        if source.as_bytes().get(site_start..site_end) != Some(b"score") {
            return Err(TestError::Missing("FieldAccess site is score"));
        }
        if site_start == type_at || source.as_bytes().get(site_start..site_end) == Some(b"Point") {
            return Err(TestError::Missing(
                "FieldAccess site is the field name, not the record type path",
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
