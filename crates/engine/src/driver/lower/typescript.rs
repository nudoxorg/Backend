//! Projects source-backed OXC syntax, bindings, and spans into the canonical
//! declaration lane with recursive declared types, exact signatures, resolved
//! references, and JSDoc documentation.
//! Keeps TypeScript syntax and lexical authority in-process beside the configured checker.
//! Contains no token reconstruction, fallback collector, or declaration guessing.

use std::collections::HashMap;

use sha2::{Digest, Sha256};

use backend_frontend_typescript::legacy::{
    AstKind, AuthorityError, BoundReference, CheckerIndex, GetSpan,
    MappedModifier as CheckerMappedModifier, NodeId, Origin, OxcModule, ReferenceFlags, Semantic,
    Span, SymbolFlags, SymbolId, SyntaxMappedModifier, TemplatePart, TypeTree, Utf8Span,
    syntax_mapped_modifier, with_analysis, with_analysis_declaration,
};
use backend_frontend_typescript::{
    TszAtom, TszAuthorityError, TszBinderState, TszBoundFile, TszCheckerState, TszNodeArena,
    TszNodeIndex, TszProject, TszSymbolId, TszSyntaxKind, TszTypeDatabase,
    tsz_type_handles::{
        ConditionalType, FunctionShape, IndexSignature as TszIndexSignature, IntrinsicKind,
        LiteralValue as TszLiteral, MappedModifier as TszMappedModifier, MappedType,
        ObjectShape as TszObjectShape, ParamInfo, SymbolRef as TszSymbolRef, TemplateSpan,
        TypeData, TypeId as TszTypeId, TypeParamInfo, TypeParamOrigin as TszTypeParamOrigin,
    },
};
use backend_semantic::ir::{
    AnnotationKind, AnonRecordForm, AnonymousCallableFamilyMultiplicity, CallableAnchorStep,
    CallableChildRole, CallableParentShape, CallableTypeContainerKind, DocFragmentInput,
    DocLinkTarget, EntityId, EntityKind, ExternalEntityRef, ExternalFragmentId, ForeignKey,
    ForeignOrigin, LatticeMappedModifier, NominalRef, Occurrence, OccurrenceConfidence,
    OccurrenceTarget, PackageLineage, PrimitiveShape, ProductChildRole, ReferenceKind, RelSpan,
    SemanticProductConstructor, SemanticTypeChild, SemanticTypeRecord, SemanticTypeTag, TypeId,
    TypeParameterListId, TypeReason, TypeScriptSourceCoordinate, TypeWidth,
};
use backend_semantic::vocabulary::{
    ProjectionForeignKeyFault, ProjectionLineagePart, ProjectionPackageLineageFault,
    TypeScriptProjectionFault, TypeScriptSource,
};

use crate::driver::{
    lower::{
        COMPUTED_ROW_BASE, EmissionExtension, FactSet, FactTypeChild, LEAF_PRODUCT,
        MAX_EMISSION_FACTS, MAX_FACT_CHILDREN, MAX_TYPE_CHILDREN, SemanticFact, StagedSourceSpan,
        push_fact,
    },
    types::{FactFault, FactRejection, LoweringUnsupported},
};

/// Recursion bound for type-expression lowering; deeper expressions are
/// honestly unknown with [`TypeReason::TruncatedAtDepthLimit`].
const MAX_TYPE_DEPTH: u8 = 24;
/// Sentinel marking an unset projection-table row.
const UNSET: u32 = u32::MAX;
/// The closed foreign ecosystem every unresolved TypeScript name lives in.
const NPM_ECOSYSTEM: &str = "npm";
/// Maximum `extends` hops consulted for inherited `this` member resolution.
const MAX_INHERITANCE_DEPTH: u8 = 8;

/// Exact direct-authority rejection while borrowing OXC declaration facts.
#[derive(Debug)]
pub(crate) enum TypeScriptCollectError {
    /// The caller's bytes cannot be the UTF-8 source OXC requires.
    Utf8(std::str::Utf8Error),
    /// OXC retained syntax or lexical diagnostics for the exact source.
    Authority(AuthorityError),
    /// The canonical bounded declaration lane cannot admit every OXC symbol.
    Lowering(LoweringUnsupported),
    /// Canonical admission rejected one exact fact; operands retained.
    Rejected(FactRejection),
    /// Projection rejected one exact authority-backed cross-reference fact.
    Projection(TypeScriptProjectionFault),
    /// An OXC declaration span could not name a slice of the admitted source.
    Span { start: u32, end: u32 },
    /// Native TSZ could not provide the requested project/file authority.
    TszAuthority(TszAuthorityError),
}

/// The lane's one coarse terminal, shared by every bounded-lane rejection
/// exactly as [`push_fact`] reports them.
fn lane_rejection() -> TypeScriptCollectError {
    TypeScriptCollectError::Lowering(LoweringUnsupported::NoSupportedDeclaration)
}

/// Grows one projection sidecar only as far as admitted facts currently need.
/// The protocol maximum stays a hard ceiling, while small source files no
/// longer reserve every maximum-sized metadata lane up front.
fn grow_side_lane<T: Clone>(
    lane: &mut Vec<T>,
    required: usize,
    maximum: usize,
    fill: T,
) -> Result<(), TypeScriptCollectError> {
    if required <= lane.len() {
        return Ok(());
    }
    if required > maximum {
        return Err(lane_rejection());
    }
    let growth_target = if lane.is_empty() {
        8
    } else {
        lane.len().saturating_mul(2)
    };
    let next_len = required.max(growth_target).min(maximum);
    lane.try_reserve(next_len - lane.len())
        .map_err(|_| lane_rejection())?;
    lane.resize(next_len, fill);
    Ok(())
}

/// Maps one collector-internal lane rejection onto the coarse lane terminal.
/// The declaration-lane path now retains the full [`FactFault`] through
/// [`TypeScriptCollectError::Rejected`]; this fold remains only for the
/// pooled-lane helpers (docs, atoms, spans) and is a recorded lane criticism.
fn fault(cause: FactFault) -> TypeScriptCollectError {
    TypeScriptCollectError::Rejected(FactRejection {
        fact: 0,
        name_len: 0,
        cause,
    })
}

/// Retains one foreign-key grammar fault across the portable terminal.
fn foreign_fault(
    cause: backend_semantic::ir::ForeignKeyFault,
    span: Span,
) -> TypeScriptCollectError {
    TypeScriptCollectError::Projection(TypeScriptProjectionFault::ForeignKey {
        start: span.start,
        end: span.end,
        cause: match cause {
            backend_semantic::ir::ForeignKeyFault::EmptyPath => {
                ProjectionForeignKeyFault::EmptyPath
            }
            backend_semantic::ir::ForeignKeyFault::BackslashInPath => {
                ProjectionForeignKeyFault::BackslashInPath
            }
        },
    })
}

/// Retains the exact rejected component of an import-module package lineage.
fn lineage_fault(
    cause: backend_semantic::ir::PackageLineageFault,
    span: Span,
) -> TypeScriptCollectError {
    TypeScriptCollectError::Projection(TypeScriptProjectionFault::PackageLineage {
        start: span.start,
        end: span.end,
        cause: match cause {
            backend_semantic::ir::PackageLineageFault::EmptyEcosystem => {
                ProjectionPackageLineageFault::EmptyEcosystem
            }
            backend_semantic::ir::PackageLineageFault::EmptyName => {
                ProjectionPackageLineageFault::EmptyPackage
            }
            backend_semantic::ir::PackageLineageFault::SeparatorInEcosystem => {
                ProjectionPackageLineageFault::SeparatorInEcosystem
            }
            backend_semantic::ir::PackageLineageFault::SeparatorInName => {
                ProjectionPackageLineageFault::SeparatorInPackage
            }
            backend_semantic::ir::PackageLineageFault::Backslash { segment } => {
                ProjectionPackageLineageFault::Backslash {
                    part: match segment {
                        0 => ProjectionLineagePart::Ecosystem,
                        1 => ProjectionLineagePart::Package,
                        segment => ProjectionLineagePart::Invalid { segment },
                    },
                }
            }
        },
    })
}

/// Validates one import-module specifier as a package lineage.
///
/// The lineage grammar reserves `:` as the `ecosystem:name` render separator,
/// but Node builtins and other scheme-qualified specifiers (`node:stream`,
/// `bun:test`) declare the scheme *inside* the specifier. Splitting the scheme
/// into the ecosystem keeps the exact module spelling without erasing it, and a
/// bare specifier stays an npm package.
fn package_lineage_for_module(
    module: &str,
) -> Result<PackageLineage<'_>, backend_semantic::ir::PackageLineageFault> {
    if let Some((scheme, rest)) = module.split_once(':')
        && !scheme.is_empty()
        && !rest.is_empty()
    {
        return PackageLineage::new(scheme, rest);
    }
    PackageLineage::new(NPM_ECOSYSTEM, module)
}

/// Whether one fact kind lexically owns nested declarations in TypeScript.
/// Classes, interfaces, namespaces, enums, functions, aliases, and
/// variable/field declarations all introduce a scope a nested name can live in.
const fn ts_lexical_owner(kind: EntityKind) -> bool {
    matches!(
        kind,
        EntityKind::Function
            | EntityKind::Record
            | EntityKind::Trait
            | EntityKind::Enum
            | EntityKind::Module
            | EntityKind::Alias
            | EntityKind::Field
            | EntityKind::Constant
            | EntityKind::Static
            | EntityKind::Variant
    )
}

fn computed_fault<'source>(
    registry: &FactRegistry<'_, 'source>,
    owner: u32,
    cause: FactFault,
) -> TypeScriptCollectError {
    let index = usize::try_from(owner).ok();
    let name_len = match index {
        Some(index) => match (
            registry.name_starts.get(index).copied(),
            registry.name_ends.get(index).copied(),
        ) {
            (Some(start), Some(end)) => match (usize::try_from(start), usize::try_from(end)) {
                (Ok(start), Ok(end)) => registry.source.get(start..end).map_or(0, str::len),
                _ => 0,
            },
            _ => 0,
        },
        None => 0,
    };
    let fact = match usize::try_from(owner) {
        Ok(fact) => fact,
        Err(_) => 0,
    };
    TypeScriptCollectError::Rejected(FactRejection {
        fact,
        name_len,
        cause,
    })
}

/// Widens one lane counter to the wire's `u32` coordinate width; an overflow
/// cannot precede the bounded lane's own capacity rejection, so the same
/// coarse terminal reports it.
fn coordinate(value: usize) -> Result<u32, TypeScriptCollectError> {
    u32::try_from(value).map_err(|_| lane_rejection())
}

/// One honestly-unknown lattice record with a closed reason code.
fn unknown_record(reason: TypeReason) -> SemanticTypeRecord<'static> {
    let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Unknown);
    record.payload0 = u32::from(reason);
    record
}

/// One staged generic-parameter row: the declared name plus the borrowed
/// spans of its optional constraint and default type expressions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TypeParamRow {
    name: Span,
    constraint: Option<Span>,
    default: Option<Span>,
}

/// Staging for one declaration's generic parameters. The pooled
/// type-parameter lane is the capacity; this list is not a second cap.
#[derive(Clone, Debug, Eq, PartialEq)]
struct TypeParamRows {
    rows: Vec<TypeParamRow>,
}

impl TypeParamRows {
    fn new() -> Self {
        Self { rows: Vec::new() }
    }

    fn push(&mut self, row: TypeParamRow) -> Result<(), TypeScriptCollectError> {
        self.rows.push(row);
        Ok(())
    }

    fn iter(&self) -> impl Iterator<Item = &TypeParamRow> {
        self.rows.iter()
    }
}

/// One staged parameter: its declared name, optional annotation, and the
/// source-owned optional/rest modifier bits that are later written into the
/// canonical function type row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ParamRow {
    name: Span,
    annotation: Option<Span>,
    default: Option<Span>,
    flags: u8,
}

/// Bounded staging for one signature's parameters. One slot beyond the fact
/// lane's child bound lets an over-wide signature reach the lane's own typed
/// child-capacity rejection instead of a staging one.
///
/// The rows live on the heap. An inline `[ParamRow; MAX_FACT_CHILDREN + 1]`
/// is about 12 KiB, and signature lowering keeps one of those arrays in every
/// recursive object-literal frame, which overflowed the 2 MiB owner stack at
/// twelve levels.
#[derive(Clone, Debug, Eq, PartialEq)]
struct ParamRows {
    rows: Vec<ParamRow>,
}

impl ParamRows {
    fn new() -> Self {
        Self { rows: Vec::new() }
    }

    fn push(&mut self, row: ParamRow) -> Result<(), TypeScriptCollectError> {
        if self.rows.len() > MAX_FACT_CHILDREN {
            return Err(fault(FactFault::ChildCapacity));
        }
        self.rows.push(row);
        Ok(())
    }

    fn count(&self) -> usize {
        self.rows.len()
    }

    fn iter(&self) -> impl Iterator<Item = &ParamRow> {
        self.rows.iter()
    }
}

/// One object-literal member link: its fact ordinal, name bytes, and flags.
type MemberLink<'source> = (u32, &'source [u8], u8);

/// One lowered type expression: the lattice record plus its bounded,
/// strictly-backward child links into already-pushed facts.
///
/// The child lane stays bounded by the type-child law, but it lives on the
/// heap: at the 64-wide lane (d40260a45) an inline array made every
/// `TypeCells` about 2 KiB, and the recursive type lowering keeps dozens of
/// them live per frame, so `MAX_TYPE_DEPTH`-bounded recursion over real
/// declaration files overflowed a default 2 MiB worker stack.
#[derive(Clone, Debug, Eq, PartialEq)]
struct TypeCells<'source> {
    record: SemanticTypeRecord<'source>,
    children: Vec<FactTypeChild<'source>>,
    len: usize,
    truncated: bool,
}

impl<'source> TypeCells<'source> {
    /// A leaf record with no children and no cells.
    ///
    /// The child array is filled on the heap. `Box::new([T; N])` builds that
    /// array on the stack first, and this function runs once per nested type
    /// frame, so twelve object-literal levels overflowed a 2 MiB stack.
    fn leaf(tag: SemanticTypeTag) -> Self {
        Self {
            record: SemanticTypeRecord::leaf(tag),
            children: Vec::new(),
            len: 0,
            truncated: false,
        }
    }

    /// An honestly-unknown leaf with one closed reason code.
    fn unknown(reason: TypeReason) -> Self {
        let mut cells = Self::leaf(SemanticTypeTag::Unknown);
        cells.record.payload0 = u32::from(reason);
        cells
    }

    /// Appends one child link; overflow past the bounded type-child lane
    /// marks the cells for the lane's typed type-child-capacity rejection.
    fn push_child(
        &mut self,
        target: u32,
        name: Option<&'source [u8]>,
        flags: u8,
    ) -> Result<(), TypeScriptCollectError> {
        if self.len >= MAX_TYPE_CHILDREN {
            self.truncated = true;
            return Err(fault(FactFault::TypeChildCapacity));
        }
        self.children.push(FactTypeChild {
            target,
            name,
            flags,
        });
        self.len += 1;
        Ok(())
    }
}

/// The outcome of lowering one type expression: either the type is embodied
/// by an already-pushed fact (a declared entity or a type parameter), or it
/// carries cells the caller applies to its own fact or to a new synthetic
/// type-expression fact.
enum TypeOutcome<'source> {
    Existing(u32),
    Cells(TypeCells<'source>),
}

/// How many members of one expected kind live on one class owner.
enum ClassMemberMatch {
    Unique(u32),
    Ambiguous,
    Absent,
}

/// Applies lowered cells onto one fact under construction.
fn with_cells<'source>(
    mut fact: SemanticFact<'source>,
    cells: TypeCells<'source>,
) -> SemanticFact<'source> {
    fact = fact.typed(cells.record);
    for child in cells.children.iter().take(cells.len) {
        fact = fact.type_child(child.target, child.name, child.flags);
    }
    if cells.truncated {
        // One dummy slot past the bounded lane marks the fact for the lane's
        // own typed type-child-capacity rejection.
        fact = fact.type_child(u32::MAX, None, 0);
    }
    fact
}

/// One import binding's foreign-origin spans.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ImportModule {
    fact: u32,
    module_start: u32,
    module_end: u32,
    display_start: u32,
    display_end: u32,
}

impl ImportModule {
    const fn unset() -> Self {
        Self {
            fact: UNSET,
            module_start: 0,
            module_end: 0,
            display_start: 0,
            display_end: 0,
        }
    }
}

/// The source-backed OXC projection bound to one canonical emission lane.
///
/// Every declared fact's name span, declaring span, and kind are retained so
/// type references, occurrence owners, and doc links resolve by exact source
/// coordinates.
struct Projector<'x, 'report, 'source, 'tsz> {
    semantic: &'x Semantic<'x>,
    /// Span index built once for this projection; type probes use binary
    /// search instead of rescanning the complete syntax arena.
    node_index: Vec<(Span, NodeId)>,
    source: &'source str,
    facts: &'x mut FactSet<'source>,
    /// Binding-name span start per pushed fact (`UNSET` when unregistered).
    name_starts: Vec<u32>,
    name_ends: Vec<u32>,
    /// Declaring-node span start per pushed fact.
    decl_starts: Vec<u32>,
    decl_ends: Vec<u32>,
    /// Exact parsed parts for merged declaration families. The first parsed
    /// declaration remains the coordinate origin and public excerpt span.
    merged_declaration_parts: HashMap<u32, Vec<Span>>,
    fact_kinds: Vec<EntityKind>,
    /// One row per import-binding fact: the module and imported-name spans
    /// its foreign keys are built from.
    import_modules: Vec<ImportModule>,
    import_module_len: usize,
    /// The span-bound checker report, when the authority ran.
    checker: Option<CheckerIndex<'report>>,
    /// Exact native TypeScript parser/binder witnesses for authored
    /// `typeof` syntax. They are absent on the legacy report-only path.
    tsz_project: Option<&'source TszProject>,
    tsz_bound_file: Option<&'tsz TszBoundFile>,
    tsz_binder: Option<&'tsz TszBinderState>,
    tsz_file_index: Option<usize>,
    /// The pooled type-parameter start of the fact about to be pushed; every
    /// push path builds its extension immediately before pushing.
    pending_type_parameters: u32,
    /// Pooled type-parameter start per pushed fact, retained so the checker
    /// pass can re-attach a completed extension with the computed cell.
    extension_type_parameters: Vec<u32>,
    /// Every object-literal member fact in push order. Lowering stages each
    /// member before its embodying fact exists, so claim sites bind staged
    /// suffixes to the embodiment they just pushed.
    staged_members: Vec<u32>,
    /// Claimed embodiment per member fact (`UNSET` when the span-containment
    /// parent stands). Indexed by fact ordinal.
    member_parents: Vec<u32>,
    /// Exact native owner override for roles whose source span nests inside
    /// another declaration (notably a constructor parameter-property field).
    /// Sparse, strictly emission-ordered ordinals; ordinary facts allocate no
    /// entries and both parentage sweeps consume this list linearly.
    native_member_parents: Vec<(u32, TypeScriptLexicalOwner)>,
    /// Native class symbol/site proofs and lexical identities, computed once
    /// before declaration lowering and sorted by exact source range.
    native_class_owners: Vec<(u32, u32, TypeScriptLexicalOwner)>,
    /// Constructor parameter-property field facts whose declaration span sits
    /// inside the constructor function rather than the class body.
    parameter_properties: Vec<u32>,
    /// Function ordinals lowered from class setter method definitions.
    setters: Vec<u32>,
    /// Source span of every synthetic type-expression fact (`UNSET` otherwise).
    /// Synthetic facts register no declaration span, but span containment still
    /// binds them to the innermost enclosing declaration: identical anonymous
    /// spellings under distinct owners must not share a parentless family.
    synthetic_starts: Vec<u32>,
    synthetic_ends: Vec<u32>,
    /// Registered fact ordinals per exact binding-name bytes. Declaration
    /// merge checks consult only same-name facts instead of rescanning the
    /// whole lane, keeping the reducer linear in duplicate density.
    facts_by_name: HashMap<&'source [u8], Vec<u32>>,
    /// First registered fact ordinal per binding-name span start.
    fact_at_name: HashMap<u32, u32>,
    /// Variable-binding initializer span per pushed constant or static fact.
    binding_init_spans: HashMap<u32, Option<Span>>,
    /// Synthetic (unregistered) type-expression facts per exact spelling,
    /// consulted only for hash-consing an identical anonymous embodiment.
    synthetic_by_name: HashMap<&'source [u8], Vec<u32>>,
    /// Registered declaring facts sorted by `(start asc, end desc)` with each
    /// entry's nearest enclosing entry, built once facts are stable so
    /// position queries are logarithmic instead of a full rescan.
    owner_index: Vec<(u32, u32, u32)>,
    owner_ancestor: Vec<Option<usize>>,
}

/// The immutable registration view one computed lowering needs: the source
/// itself plus the pushed facts' exact source coordinates. Split borrows of
/// the projector's fields keep the mutable fact lane free during recursion.
/// The definition lives beside the computed lowering functions below.
struct FactRegistry<'a, 'source> {
    source: &'source str,
    /// Exact retained project-source capability for native TSZ declarations.
    tsz_project: Option<&'source TszProject>,
    /// Exact virtual path of the source file being projected, required when
    /// a native anonymous property has no TSZ declaration symbol.
    source_path: Option<String>,
    decl_starts: &'a [u32],
    decl_ends: &'a [u32],
    declaration_parts: &'a HashMap<u32, Vec<Span>>,
    name_starts: &'a [u32],
    /// First current fact ordinal for each exact start. Built from this view's
    /// immutable bounded slice, never a historical projector registration map.
    name_start_index: HashMap<u32, u32>,
    name_ends: &'a [u32],
    fact_kinds: &'a [EntityKind],
    /// Exact TypeScript-symbol to same-file emitted-entity coordinate joins.
    /// This is present only while projecting one native checker observation.
    native_tsz_entities: Option<&'a HashMap<TszSymbolId, u32>>,
    fact_len: u32,
}

impl<'x, 'report, 'source, 'tsz> Projector<'x, 'report, 'source, 'tsz> {
    /// Borrows the exact source bytes one OXC span names.
    fn slice_span(&self, span: Span) -> Option<&'source [u8]> {
        let start = usize::try_from(span.start).ok()?;
        let end = usize::try_from(span.end).ok()?;
        self.source.as_bytes().get(start..end)
    }

    /// Borrows the exact source text one OXC span names.
    fn text_span(&self, span: Span) -> Option<&'source str> {
        let start = usize::try_from(span.start).ok()?;
        let end = usize::try_from(span.end).ok()?;
        self.source.get(start..end)
    }

    /// Admits one fact into the lane, mapping every typed rejection onto the
    /// mandated coarse terminal, and records the extension's pooled
    /// type-parameter start for the checker pass.
    fn push(&mut self, fact: SemanticFact<'source>) -> Result<u32, TypeScriptCollectError> {
        let pending = self.pending_type_parameters;
        let ordinal = push_fact(self.facts, fact).map_err(TypeScriptCollectError::Rejected)?;
        let ordinal = coordinate(ordinal)?;
        let index = usize::try_from(ordinal).map_err(|_| lane_rejection())?;
        let required = index.checked_add(1).ok_or_else(lane_rejection)?;
        self.ensure_fact_sidecars(required)?;
        // Type-expression carriers remain canonical support. Only explicit
        // source declaration capture below grants product-row ownership.
        *self.facts.publication_roles.get_mut(index).ok_or_else(lane_rejection)? =
            backend_semantic::ir::DeclarationPublicationRole::ReferenceOnly;
        *self
            .extension_type_parameters
            .get_mut(index)
            .ok_or_else(lane_rejection)? = pending;
        Ok(ordinal)
    }

    fn ensure_fact_sidecars(&mut self, required: usize) -> Result<(), TypeScriptCollectError> {
        grow_side_lane(&mut self.name_starts, required, MAX_EMISSION_FACTS, UNSET)?;
        grow_side_lane(&mut self.name_ends, required, MAX_EMISSION_FACTS, UNSET)?;
        grow_side_lane(&mut self.decl_starts, required, MAX_EMISSION_FACTS, UNSET)?;
        grow_side_lane(&mut self.decl_ends, required, MAX_EMISSION_FACTS, UNSET)?;
        grow_side_lane(
            &mut self.fact_kinds,
            required,
            MAX_EMISSION_FACTS,
            EntityKind::Function,
        )?;
        grow_side_lane(
            &mut self.extension_type_parameters,
            required,
            MAX_EMISSION_FACTS,
            0,
        )?;
        grow_side_lane(
            &mut self.member_parents,
            required,
            MAX_EMISSION_FACTS,
            UNSET,
        )?;
        grow_side_lane(
            &mut self.synthetic_starts,
            required,
            MAX_EMISSION_FACTS,
            UNSET,
        )?;
        grow_side_lane(
            &mut self.synthetic_ends,
            required,
            MAX_EMISSION_FACTS,
            UNSET,
        )?;
        Ok(())
    }

    fn push_import_module(&mut self, module: ImportModule) -> Result<(), TypeScriptCollectError> {
        let required = self
            .import_module_len
            .checked_add(1)
            .ok_or_else(lane_rejection)?;
        grow_side_lane(
            &mut self.import_modules,
            required,
            MAX_EMISSION_FACTS,
            ImportModule::unset(),
        )?;
        *self
            .import_modules
            .get_mut(self.import_module_len)
            .ok_or_else(lane_rejection)? = module;
        self.import_module_len = required;
        Ok(())
    }

    /// Builds the TypeScript extension fact for the fact about to be pushed:
    /// the would-be own ordinal as the declared-type coordinate and the given
    /// pooled type-parameter start. The computed cell is attached later, in
    /// the checker pass, exactly for the facts the checker typed.
    fn extension(
        &mut self,
        type_parameter_start: u32,
    ) -> Result<EmissionExtension, TypeScriptCollectError> {
        self.pending_type_parameters = type_parameter_start;
        let declared = coordinate(self.facts.len())?;
        Ok(EmissionExtension::TypeScript(
            backend_semantic::ir::TypeScriptFacts {
                type_parameters: TypeParameterListId::new(type_parameter_start),
                declared: Some(TypeId::new(declared)),
                observed: None,
            },
        ))
    }

    /// The extension coordinate for a fact with no rows of its own: the
    /// current pooled length, which is the start of an empty list.
    fn extension_without_type_parameters(
        &mut self,
    ) -> Result<EmissionExtension, TypeScriptCollectError> {
        let start = coordinate(self.facts.type_parameter_len)?;
        self.extension(start)
    }

    /// Registers one pushed fact's declaring span, binding-name span, and
    /// kind so references and links can resolve to its ordinal, and binds
    /// one exact declaration range as immutable primary provenance. Legal
    /// merged parts retain that origin and supply separate exact containment
    /// proofs; the public excerpt never widens across intervening source.
    fn register(
        &mut self,
        fact: u32,
        declaration: Span,
        name: Span,
        kind: EntityKind,
    ) -> Result<(), TypeScriptCollectError> {
        let Some(index) = usize::try_from(fact).ok() else {
            return Ok(());
        };
        let staged = StagedSourceSpan::new(declaration.start, declaration.end).ok_or(
            TypeScriptCollectError::Span {
                start: declaration.start,
                end: declaration.end,
            },
        )?;
        // Keep the original exact primary source observation immutable.
        // Additional legal merge parts receive a separate containment proof.
        self.facts.attach_source_span(fact, staged).map_err(fault)?;
        *self.facts.publication_roles.get_mut(index).ok_or_else(lane_rejection)? =
            backend_semantic::ir::DeclarationPublicationRole::PrimaryOwned;
        *self.name_starts.get_mut(index).ok_or_else(lane_rejection)? = name.start;
        *self.name_ends.get_mut(index).ok_or_else(lane_rejection)? = name.end;
        *self.decl_starts.get_mut(index).ok_or_else(lane_rejection)? = declaration.start;
        *self.decl_ends.get_mut(index).ok_or_else(lane_rejection)? = declaration.end;
        *self.fact_kinds.get_mut(index).ok_or_else(lane_rejection)? = kind;
        if let Some(bytes) = self.slice_span(name) {
            self.facts_by_name.entry(bytes).or_default().push(fact);
        }
        self.fact_at_name.entry(name.start).or_insert(fact);
        Ok(())
    }

    /// Commits the producer's complete merge-part proof after all parsed
    /// declarations have arrived. Primary source observations stay unchanged.
    fn commit_merged_declaration_parts(&mut self) -> Result<(), TypeScriptCollectError> {
        let source_len = coordinate(self.source.len())?;
        for (&owner, parts) in &self.merged_declaration_parts {
            let parts: Box<[_]> = parts
                .iter()
                .map(|part| StagedSourceSpan {
                    start: part.start,
                    end: part.end,
                })
                .collect();
            self.facts
                .capture_declaration_parts(owner, source_len, parts)
                .map_err(fault)?;
        }
        Ok(())
    }

    /// Only exact parsed declaration parts prove lexical containment.
    fn containing_declaration_part(&self, ordinal: u32, span: Span) -> Option<Span> {
        if let Some(parts) = self.merged_declaration_parts.get(&ordinal) {
            return parts
                .iter()
                .copied()
                .find(|part| part.start <= span.start && span.end <= part.end);
        }
        let index = ordinal as usize;
        let (&start, &end) = (self.decl_starts.get(index)?, self.decl_ends.get(index)?);
        (start != UNSET && end != UNSET && start <= span.start && span.end <= end)
            .then_some(Span::new(start, end))
    }

    fn declaration_contains(&self, ordinal: u32, span: Span) -> bool {
        self.containing_declaration_part(ordinal, span).is_some()
    }

    /// Resolves one source position to the fact whose binding name starts
    /// exactly there.
    fn fact_at_name_start(&self, start: u32) -> Option<u32> {
        self.fact_at_name.get(&start).copied()
    }

    /// Resolves one source position to the innermost enclosing function
    /// whose declaring span still contains it.
    fn enclosing_function_owner(&self, position: u32) -> Option<u32> {
        let length = coordinate(self.facts.len()).ok()?;
        let mut best: Option<(u32, u32)> = None;
        for ordinal in 0..length {
            let index = usize::try_from(ordinal).ok()?;
            if self.fact_kinds.get(index).copied() != Some(EntityKind::Function) {
                continue;
            }
            if let Some(part) = self
                .containing_declaration_part(ordinal, Span::new(position, position.checked_add(1)?))
                && best.is_none_or(|(known, _)| part.start >= known)
            {
                best = Some((part.start, ordinal));
            }
        }
        best.map(|(_, ordinal)| ordinal)
    }

    /// Resolves one source position to the innermost pushed fact whose
    /// declaring span contains it.
    fn owning_fact(&self, position: u32) -> Option<u32> {
        if self.owner_index.is_empty() {
            let length = coordinate(self.facts.len()).ok()?;
            let mut best: Option<(u32, u32)> = None;
            for ordinal in 0..length {
                if let Some(part) = self.containing_declaration_part(
                    ordinal, Span::new(position, position.checked_add(1)?),
                ) && best.is_none_or(|(known, _)| part.start >= known) {
                    best = Some((part.start, ordinal));
                }
            }
            return best.map(|(_, ordinal)| ordinal);
        }
        let idx = self
            .owner_index
            .partition_point(|(start, _, _)| *start <= position);
        let mut cursor = idx.checked_sub(1)?;
        loop {
            let (start, end, ordinal) = *self.owner_index.get(cursor)?;
            if start <= position && position < end {
                return Some(ordinal);
            }
            match self.owner_ancestor.get(cursor).copied().flatten() {
                Some(parent) if parent < cursor => cursor = parent,
                _ => return None,
            }
        }
    }

    /// Resolves the first pushed fact whose declaring span starts at or after
    /// `position` — the natural owner of a JSDoc block ending there.
    fn next_fact_after(&self, position: u32) -> Option<u32> {
        if !self.owner_index.is_empty() {
            let idx = self
                .owner_index
                .partition_point(|(start, _, _)| *start < position);
            return self.owner_index.get(idx).map(|(_, _, ordinal)| *ordinal);
        }
        let length = coordinate(self.facts.len()).ok()?;
        let mut best: Option<(u32, u32)> = None;
        for ordinal in 0..length {
            let index = usize::try_from(ordinal).ok()?;
            let start = *self.decl_starts.get(index)?;
            if start != UNSET && start >= position && best.is_none_or(|(known, _)| start < known) {
                best = Some((start, ordinal));
            }
        }
        best.map(|(_, ordinal)| ordinal)
    }

    /// Builds the declaring-fact interval index once every declaration row is
    /// final. Entries nest or stay disjoint, so a nearest-enclosing stack
    /// gives each entry's ancestor and makes position queries logarithmic.
    fn build_owner_index(&mut self) {
        let length = self.facts.len;
        let mut entries: Vec<(u32, u32, u32)> = Vec::with_capacity(length);
        for index in 0..length {
            let start = self.decl_starts.get(index).copied().unwrap_or(UNSET);
            let end = self.decl_ends.get(index).copied().unwrap_or(UNSET);
            if start != UNSET && end != UNSET {
                let ordinal = coordinate(index).unwrap_or(UNSET);
                if let Some(parts) = self.merged_declaration_parts.get(&ordinal) {
                    entries.extend(parts.iter().map(|part| (part.start, part.end, ordinal)));
                } else {
                    entries.push((start, end, ordinal));
                }
            }
        }
        entries.sort_unstable_by(|left, right| left.0.cmp(&right.0).then(right.1.cmp(&left.1)));
        let mut ancestor = vec![None; entries.len()];
        let mut stack: Vec<usize> = Vec::new();
        for index in 0..entries.len() {
            let start = entries[index].0;
            while let Some(&top) = stack.last() {
                if entries[top].1 <= start {
                    stack.pop();
                } else {
                    break;
                }
            }
            ancestor[index] = stack.last().copied();
            stack.push(index);
        }
        self.owner_index = entries;
        self.owner_ancestor = ancestor;
    }

    /// Binds every still-unclaimed member staged since `base` to the
    /// just-pushed embodiment `embodiment`. Members of nested literals were
    /// already claimed to their own embodiments while their literal lowered,
    /// so the skip keeps each member with its innermost embodiment and the
    /// outer claim binds only the members this embodiment directly owns.
    fn claim_staged_members(&mut self, base: usize, embodiment: u32) {
        let staged = self.staged_members.len();
        for position in base..staged {
            let Some(member) = self.staged_members.get(position).copied() else {
                continue;
            };
            if member == embodiment {
                continue;
            }
            let Some(index) = usize::try_from(member).ok() else {
                continue;
            };
            if let Some(slot) = self.member_parents.get_mut(index) {
                if *slot == UNSET {
                    *slot = embodiment;
                }
            }
        }
    }

    /// Resolves the first pushed fact whose exact binding-name bytes equal
    /// `name`.
    fn fact_by_name_bytes(&self, name: &[u8]) -> Option<u32> {
        self.facts_by_name
            .get(name)
            .and_then(|facts| facts.first().copied())
    }

    /// Widens the leading identifier of the name starting at `start` and
    /// resolves it to a fact with identical binding-name bytes.
    fn fact_by_name_prefix(&self, start: u32) -> Option<u32> {
        let bytes = self.source.as_bytes();
        let from = usize::try_from(start).ok()?;
        let mut end = from;
        while let Some(byte) = bytes.get(end) {
            let is_ident = byte.is_ascii_alphanumeric() || *byte == b'_' || *byte == b'$';
            if !is_ident {
                break;
            }
            end += 1;
        }
        let name = bytes.get(from..end)?;
        self.fact_by_name_bytes(name)
    }

    /// Resolves the identifier starting at `start` through OXC's binding
    /// authority: the reference at that position names a symbol whose binding
    /// span is a registered fact. Falls back to an exact in-file name match.
    fn local_fact_at(&self, start: u32) -> Option<u32> {
        let scoping = self.semantic.scoping();
        let nodes = self.semantic.nodes();
        let first = self
            .node_index
            .partition_point(|(span, _)| span.start < start);
        for (span, node_id) in self.node_index.get(first..).unwrap_or(&[]) {
            if span.start != start {
                break;
            }
            let Some(identifier) = nodes.get_node(*node_id).kind().as_identifier_reference() else {
                continue;
            };
            let Some(reference_id) = identifier.reference_id.get() else {
                continue;
            };
            let Some(symbol) = scoping.get_reference(reference_id).symbol_id() else {
                continue;
            };
            if let Some(fact) = self.fact_at_name_start(scoping.symbol_span(symbol).start) {
                return Some(fact);
            }
        }
        self.fact_by_name_prefix(start)
    }

    /// Resolves one authored TypeQuery only through exact bound identifiers.
    /// The OXC source node supplies the declared syntax; the native TSZ AST
    /// and binder must independently confirm that the query and its unique
    /// same-file entity declaration name the same compiler symbol. This is
    /// intentionally stricter than `local_fact_at`, whose name fallback is
    /// useful for reference recovery but cannot authorize a TypeQuery target.
    fn type_query_entity_at(&self, query_span: Span, name_span: Span) -> Option<u32> {
        let semantic = self.semantic;
        let nodes = semantic.nodes();
        let first = self.node_index.partition_point(|(known, _)| {
            (known.start, known.end) < (name_span.start, name_span.end)
        });
        let mut oxc_symbol = None;
        for (known, node_id) in self.node_index.get(first..).unwrap_or(&[]) {
            if (known.start, known.end) != (name_span.start, name_span.end) {
                break;
            }
            let Some(identifier) = nodes.get_node(*node_id).kind().as_identifier_reference() else {
                continue;
            };
            let Some(reference) = identifier.reference_id.get() else {
                continue;
            };
            let Some(symbol) = semantic.scoping().get_reference(reference).symbol_id() else {
                continue;
            };
            if oxc_symbol.is_some_and(|existing| existing != symbol) {
                return None;
            }
            oxc_symbol = Some(symbol);
        }
        let Some(oxc_symbol) = oxc_symbol else {
            #[cfg(test)]
            eprintln!(
                "TYPE_QUERY_BINDING_TRACE no_oxc_reference query={query_span:?} name={name_span:?} fallback={:?}",
                self.local_fact_at(name_span.start)
            );
            return None;
        };
        let target_start = semantic.scoping().symbol_span(oxc_symbol).start;
        let target = self.fact_at_name_start(target_start)?;
        let target_index = usize::try_from(target).ok()?;
        let target_name = Span::new(
            *self.name_starts.get(target_index)?,
            *self.name_ends.get(target_index)?,
        );
        if target_name.start == UNSET || target_name.end == UNSET {
            return None;
        }

        // A legacy caller has only the exact OXC reference binding. Native
        // collection additionally requires a matching TypeQuery AST node,
        // matching query/declaration SymbolIds, and a single admitted
        // declaration in this exact project file. Overloads and forwarded
        // declarations therefore remain explicit unresolved source types.
        let (Some(project), Some(bound_file), Some(binder), Some(file_index)) = (
            self.tsz_project,
            self.tsz_bound_file,
            self.tsz_binder,
            self.tsz_file_index,
        ) else {
            return Some(target);
        };
        let query_symbol = native_tsz_type_query_symbol(bound_file, binder, query_span, name_span)?;
        let mut native_target = None;
        for fact_index in 0..self.facts.len() {
            let Some((&name_start, &name_end)) = self
                .name_starts
                .get(fact_index)
                .zip(self.name_ends.get(fact_index))
            else {
                continue;
            };
            if name_start == UNSET || name_end == UNSET {
                continue;
            }
            let candidate = usize::try_from(fact_index).ok()?;
            let candidate_name = Span::new(name_start, name_end);
            if native_tsz_symbol_at_identifier_span(bound_file, candidate_name)
                == Some(query_symbol)
            {
                if native_target.is_some() {
                    return None;
                }
                native_target = Some(u32::try_from(candidate).ok()?);
            }
        }
        let Some(native_target) = native_target else {
            return None;
        };
        if native_target != target {
            return None;
        }
        #[cfg(test)]
        eprintln!(
            "TYPE_QUERY_BINDING_TRACE query={query_span:?} name={name_span:?} target={target} target_name={target_name:?} tsz_query={query_symbol:?}",
        );
        let symbol = project.program().symbols.get(query_symbol)?;
        if symbol.stable_declarations.len() != 1 {
            return None;
        }
        let declaration = symbol.stable_declarations.first()?;
        if usize::try_from(declaration.file_idx).ok()? != file_index
            || declaration.pos > target_name.start
            || declaration.end < target_name.end
        {
            return None;
        }
        Some(target)
    }

    /// Reports whether the identifier at `span` sits in callee position of an
    /// enclosing call or construction expression.
    fn is_call_position(&self, span: Span) -> bool {
        let nodes = self.semantic.nodes();
        let first = self
            .node_index
            .partition_point(|(known, _)| (known.start, known.end) < (span.start, span.end));
        for (known, node_id) in self.node_index.get(first..).unwrap_or(&[]) {
            if (known.start, known.end) != (span.start, span.end) {
                break;
            }
            let node = nodes.get_node(*node_id);
            if node.kind().as_identifier_reference().is_none() {
                continue;
            }
            let parent = nodes.get_node(nodes.parent_id(node.id())).kind();
            if let Some(call) = parent.as_call_expression()
                && call.callee.span() == span
            {
                return true;
            }
            if let Some(construction) = parent.as_new_expression()
                && construction.callee.span() == span
            {
                return true;
            }
        }
        false
    }

    /// Reports whether one resolved symbol is a `const` binding initialized
    /// by a function or arrow expression, not an alias or destructuring
    /// pattern.
    fn is_const_callable_value(&self, symbol: SymbolId, symbol_flags: SymbolFlags) -> bool {
        if !symbol_flags.contains(SymbolFlags::ConstVariable) {
            return false;
        }
        let nodes = self.semantic.nodes();
        let scoping = self.semantic.scoping();
        let declared = nodes.get_node(scoping.symbol_declaration(symbol));
        let declarator = match declared.kind() {
            AstKind::VariableDeclarator(declarator) => declarator,
            AstKind::BindingIdentifier(_) => {
                let parent = nodes.get_node(nodes.parent_id(declared.id()));
                match parent.kind() {
                    AstKind::VariableDeclarator(declarator) => declarator,
                    _ => return false,
                }
            }
            _ => return false,
        };
        if !declarator.id.is_binding_identifier() {
            return false;
        }
        let Some(init) = declarator.init.as_ref() else {
            return false;
        };
        let init_span = init.span();
        self.initializer_is_callable(init_span.start, init_span.end, 0)
    }

    /// Peels only parenthesized and TypeScript assertion wrappers, then
    /// reports whether the initializer is a function or arrow expression.
    fn initializer_is_callable(&self, start: u32, end: u32, depth: u8) -> bool {
        if depth > 8 {
            return false;
        }
        let Some(kind) = self.ast_kind_at_exact_span(start, end) else {
            return false;
        };
        if let Some(parenthesized) = kind.as_parenthesized_expression() {
            let inner = parenthesized.expression.span();
            return self.initializer_is_callable(inner.start, inner.end, depth.saturating_add(1));
        }
        if let Some(cast) = kind.as_ts_as_expression() {
            let inner = cast.expression.span();
            return self.initializer_is_callable(inner.start, inner.end, depth.saturating_add(1));
        }
        if let Some(satisfied) = kind.as_ts_satisfies_expression() {
            let inner = satisfied.expression.span();
            return self.initializer_is_callable(inner.start, inner.end, depth.saturating_add(1));
        }
        if let Some(non_null) = kind.as_ts_non_null_expression() {
            let inner = non_null.expression.span();
            return self.initializer_is_callable(inner.start, inner.end, depth.saturating_add(1));
        }
        kind.as_arrow_function_expression().is_some() || kind.as_function().is_some()
    }

    /// Reports whether `span` names the property of a static (non-computed,
    /// non-private) member expression in callee position of a call or `new`.
    fn is_member_call_position(&self, span: Span) -> bool {
        let nodes = self.semantic.nodes();
        let first = self
            .node_index
            .partition_point(|(known, _)| (known.start, known.end) < (span.start, span.end));
        for (known, node_id) in self.node_index.get(first..).unwrap_or(&[]) {
            if (known.start, known.end) != (span.start, span.end) {
                break;
            }
            let member_id = match self.static_member_for_property(*node_id, span) {
                Some(member_id) => member_id,
                None => continue,
            };
            let member_span = nodes.get_node(member_id).kind().span();
            let parent = nodes.get_node(nodes.parent_id(member_id)).kind();
            if let Some(call) = parent.as_call_expression()
                && call.callee.span() == member_span
            {
                return true;
            }
            if let Some(construction) = parent.as_new_expression()
                && construction.callee.span() == member_span
            {
                return true;
            }
        }
        false
    }

    /// Returns the static member node when `span` names its property.
    fn static_member_for_property(&self, node_id: NodeId, span: Span) -> Option<NodeId> {
        let nodes = self.semantic.nodes();
        let kind = nodes.get_node(node_id).kind();
        if let Some(member) = kind.as_static_member_expression()
            && member.property.span() == span
        {
            return Some(node_id);
        }
        if kind.as_identifier_name().is_some() {
            let parent_id = nodes.parent_id(node_id);
            let parent = nodes.get_node(parent_id).kind();
            if let Some(member) = parent.as_static_member_expression()
                && member.property.span() == span
            {
                return Some(parent_id);
            }
        }
        None
    }

    /// Pushes one fact per staged generic parameter so uses of the parameter
    /// inside the declaration resolve to a `TypeVar` fact.
    fn push_type_parameter_facts(
        &mut self,
        rows: &TypeParamRows,
    ) -> Result<(), TypeScriptCollectError> {
        for row in rows.iter() {
            if self.fact_at_name_start(row.name.start).is_some() {
                continue;
            }
            let name = self
                .slice_span(row.name)
                .ok_or(TypeScriptCollectError::Span {
                    start: row.name.start,
                    end: row.name.end,
                })?;
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::TypeVar);
            record.text = Some(name);
            let extension = self.extension_without_type_parameters()?;
            let fact = SemanticFact::new(EntityKind::Parameter, name, LEAF_PRODUCT)
                .typed(record)
                .with_extension(extension);
            let ordinal = self.push(fact)?;
            self.register(ordinal, row.name, row.name, EntityKind::Parameter)?;
        }
        Ok(())
    }

    /// Pushes the pooled generic-parameter rows for one declaration: the
    /// constraint and default facts first, then the rows. Returns the pooled
    /// start coordinate — an empty list when the declaration has no rows.
    fn push_type_params(
        &mut self,
        rows: &TypeParamRows,
        depth: u8,
    ) -> Result<u32, TypeScriptCollectError> {
        let start = coordinate(self.facts.type_parameter_len)?;
        for row in rows.iter() {
            let constraint = match row.constraint {
                Some(span) => Some(self.child_target(span.start, span.end, depth)?),
                None => None,
            };
            let default = match row.default {
                Some(span) => Some(self.child_target(span.start, span.end, depth)?),
                None => None,
            };
            let name = self
                .slice_span(row.name)
                .ok_or(TypeScriptCollectError::Span {
                    start: row.name.start,
                    end: row.name.end,
                })?;
            self.facts
                .push_type_parameter(name, constraint, default)
                .map_err(fault)?;
        }
        Ok(start)
    }

    /// Pushes one fact per staged parameter and returns their ordinals in
    /// declared order.
    fn push_parameter_facts(
        &mut self,
        params: &ParamRows,
    ) -> Result<[u32; MAX_FACT_CHILDREN + 1], TypeScriptCollectError> {
        let mut ordinals = [0_u32; MAX_FACT_CHILDREN + 1];
        for (index, row) in params.iter().enumerate() {
            let bindings = self.destructured_bindings(row.name);
            let name = self
                .slice_span(row.name)
                .ok_or(TypeScriptCollectError::Span {
                    start: row.name.start,
                    end: row.name.end,
                })?;
            let type_parameter_start = coordinate(self.facts.type_parameter_len)?;
            let member_base = self.staged_members.len();
            let cells = match row.annotation {
                Some(span) => self.owner_cells(span.start, span.end, 0)?,
                None => TypeCells::unknown(TypeReason::Unannotated),
            };
            let extension = self.extension(type_parameter_start)?;
            let fact = with_cells(
                SemanticFact::new(EntityKind::Parameter, name, LEAF_PRODUCT)
                    .with_extension(extension),
                cells,
            );
            let ordinal = self.push(fact)?;
            self.register(ordinal, row.name, row.name, EntityKind::Parameter)?;
            self.claim_staged_members(member_base, ordinal);
            if let Some(bindings) = bindings {
                // The pattern occupies one argument position and retains its
                // complete declared type/source. It is not an authored name.
                self.facts.publication_roles[ordinal as usize] =
                    backend_semantic::ir::DeclarationPublicationRole::ReferenceOnly;
                for binding in bindings {
                    if self.fact_at_name_start(binding.start).is_some() { continue; }
                    let name = self.slice_span(binding).ok_or(TypeScriptCollectError::Span {
                        start: binding.start, end: binding.end,
                    })?;
                    let extension = self.extension_without_type_parameters()?;
                    // An aggregate annotation is not the type of each leaf.
                    // The native checker supplies the leaf's computed type.
                    let fact = SemanticFact::new(EntityKind::Parameter, name, LEAF_PRODUCT)
                        .typed(unknown_record(TypeReason::Unannotated))
                        .with_extension(extension);
                    let leaf = self.push(fact)?;
                    self.register(leaf, binding, binding, EntityKind::Parameter)?;
                }
            }
            if let Some(default) = row.default {
                self.declare_expression_bindings(default.start, default.end, 0)?;
            }
            if let Some(slot) = ordinals.get_mut(index) {
                *slot = ordinal;
            }
        }
        Ok(ordinals)
    }

    /// The exact parsed object/array binding pattern proves which identifiers
    /// are declarations. Property labels, computed keys and default-expression
    /// references are deliberately not bindings; OXC's pattern walker follows
    /// only binding values, nested patterns and rest arguments.
    fn destructured_bindings(&self, span: Span) -> Option<Vec<Span>> {
        let first = self.node_index.partition_point(|(known, _)|
            (known.start, known.end) < (span.start, span.end));
        for (known, node) in self.node_index.get(first..).unwrap_or(&[]) {
            if *known != span { break; }
            let kind = self.semantic.nodes().get_node(*node).kind();
            let mut bindings = Vec::new();
            if let Some(pattern) = kind.as_object_pattern() {
                for property in &pattern.properties {
                    bindings.extend(property.value.get_binding_identifiers().into_iter().map(|binding| binding.span));
                }
                if let Some(rest) = &pattern.rest {
                    bindings.extend(rest.argument.get_binding_identifiers().into_iter().map(|binding| binding.span));
                }
            } else if let Some(pattern) = kind.as_array_pattern() {
                for element in pattern.elements.iter().flatten() {
                    bindings.extend(element.get_binding_identifiers().into_iter().map(|binding| binding.span));
                }
                if let Some(rest) = &pattern.rest {
                    bindings.extend(rest.argument.get_binding_identifiers().into_iter().map(|binding| binding.span));
                }
            } else {
                continue;
            }
            return Some(bindings);
        }
        None
    }

    /// Pushes the dedicated result carrier for one annotated signature: a
    /// parameter-kind fact named after the callable and typed exactly as its
    /// return annotation. Signature carrier bindings admit only parameter
    /// carriers, so the result never points at the annotation's type fact.
    fn push_result_carrier(
        &mut self,
        name: Span,
        annotation: Span,
    ) -> Result<u32, TypeScriptCollectError> {
        let name_bytes = self.slice_span(name).ok_or(TypeScriptCollectError::Span {
            start: name.start,
            end: name.end,
        })?;
        let type_parameter_start = coordinate(self.facts.type_parameter_len)?;
        let member_base = self.staged_members.len();
        let cells = self.owner_cells(annotation.start, annotation.end, 0)?;
        let extension = self.extension(type_parameter_start)?;
        // A result may share its callable's name and type with an authored
        // input (`stmt(stmt: Stmt): Stmt`). Both are Parameter facts under
        // that callable, but their signature roles are distinct authority.
        // Frame that role, never a source coordinate or invented binding.
        let role = Sha256::digest(b"compiler.typescript.signature-result-carrier.v1\0");
        let mut discriminator = [0_u8; 16];
        discriminator.copy_from_slice(&role[..16]);
        let fact = with_cells(
            SemanticFact::new(EntityKind::Parameter, name_bytes, LEAF_PRODUCT)
                .with_extension(extension)
                .with_identity_discriminator(discriminator),
            cells,
        );
        let ordinal = self.push(fact)?;
        self.claim_staged_members(member_base, ordinal);
        // The carrier names no binding, so it stays out of the name tables;
        // its annotation span still places it under the enclosing callable
        // in the parentage pass, keeping same-name results of different
        // owners (an interface method and its class implementation) distinct.
        if let Some(index) = usize::try_from(ordinal).ok() {
            if let Some(slot) = self.synthetic_starts.get_mut(index) {
                *slot = annotation.start;
            }
            if let Some(slot) = self.synthetic_ends.get_mut(index) {
                *slot = annotation.end;
            }
        }
        Ok(ordinal)
    }

    /// Pushes one callable signature with its exact facts: generic-parameter
    /// facts and pooled rows, one parameter fact per declared parameter, the
    /// optional result fact, the function product constructor, and the
    /// function-pointer record whose children carry the same parameter and
    /// result facts. Overload signatures therefore differ in constructor or
    /// child cells and keep distinct identities with no ordinal
    /// disambiguation. Returns the signature fact's ordinal.
    fn push_signature(
        &mut self,
        name: Span,
        declaration: Span,
        rows: &TypeParamRows,
        params: &ParamRows,
        result: Option<Span>,
        static_member: bool,
    ) -> Result<u32, TypeScriptCollectError> {
        self.push_type_parameter_facts(rows)?;
        let type_parameter_start = self.push_type_params(rows, 0)?;
        let param_ordinals = self.push_parameter_facts(params)?;
        let result_target = match result {
            Some(span) => Some(self.push_result_carrier(name, span)?),
            None => None,
        };
        let name_bytes = self.slice_span(name).ok_or(TypeScriptCollectError::Span {
            start: name.start,
            end: name.end,
        })?;
        let param_count = coordinate(params.count())?;
        let result_count = u32::from(result_target.is_some());
        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::FunctionPointer);
        if result_target.is_some() {
            record.payload1 = SemanticTypeRecord::FUNCTION_RESULT_COUNT_ONE;
        }
        // A trailing `...args` parameter is a typed variadic tail, never a
        // fixed parameter. The lane's child grammar admits the rest marker
        // only under the typed-last variadic form, so the record must declare
        // it exactly when the final parameter carries it.
        if params
            .iter()
            .last()
            .is_some_and(|parameter| parameter.flags & SemanticTypeChild::FLAG_REST != 0)
        {
            record.payload0 = SemanticTypeRecord::FUNCTION_TYPED_VARIADIC_FLAG;
        }
        let extension = self.extension(type_parameter_start)?;
        let mut fact = SemanticFact::new(
            EntityKind::Function,
            name_bytes,
            SemanticProductConstructor::function(param_count, result_count),
        )
        .typed(record)
        .with_extension(extension);
        if static_member {
            fact = fact.static_member();
        }
        for (ordinal, parameter) in param_ordinals.iter().zip(params.iter()) {
            fact = fact.child(ProductChildRole::FunctionParameter, *ordinal);
            fact = fact.type_child(*ordinal, None, parameter.flags);
        }
        if let Some(target) = result_target {
            fact = fact.child(ProductChildRole::FunctionResult, target);
            fact = fact.type_child(target, None, 0);
        }
        // Real declaration files legally repeat one overload signature word
        // for word (`es-toolkit`'s `partialRight.d.ts` does), and the checker
        // reports every spelled declaration. Such twins share name, owner,
        // record, and every child shape, so the flattened identity build would
        // raise its duplicate terminal. The count of already-committed
        // structurally identical siblings is authority-proven source order,
        // so the twin count enters as the identity discriminator exactly when
        // it is nonzero, and every non-twin overload frames no new bytes.
        let twins = self.indistinguishable_signature_twins(declaration, &fact);
        if twins > 0 {
            let mut hash = Sha256::new();
            hash.update(b"compiler.typescript.signature-twin.v1\0");
            hash.update(twins.to_le_bytes());
            let mut discriminator = [0_u8; 16];
            discriminator.copy_from_slice(&hash.finalize()[..16]);
            fact = fact.with_identity_discriminator(discriminator);
        }
        let ordinal = self.push(fact)?;
        self.register(ordinal, declaration, name, EntityKind::Function)?;
        // FunctionParameter product children prove their callable owner.
        // A parameter-property Field mirrors the same syntax, but its wider
        // modifier/type span must not capture the Parameter in the later
        // lexical parentage sweep.
        for (&parameter, _) in param_ordinals.iter().zip(params.iter()) {
            let parent = self.member_parents.get_mut(parameter as usize)
                .ok_or_else(lane_rejection)?;
            if *parent != UNSET {
                return Err(lane_rejection());
            }
            *parent = ordinal;
        }
        Ok(ordinal)
    }

    /// Pushes one class field fact for each constructor parameter property.
    /// Parameter facts are already registered by [`push_signature`]; each
    /// parameter property also becomes a [`EntityKind::Field`] on the
    /// enclosing class so `this.member` resolution can see it.
    fn push_parameter_properties(
        &mut self,
        params_span: Span,
        declaration: Span,
    ) -> Result<(), TypeScriptCollectError> {
        let Some(params_kind) = self.ast_kind_at_exact_span(params_span.start, params_span.end)
        else {
            return Ok(());
        };
        let Some(params) = params_kind.as_formal_parameters() else {
            return Ok(());
        };
        if !params
            .items
            .iter()
            .any(|parameter| parameter.accessibility.is_some() || parameter.readonly)
        {
            return Ok(());
        }
        // The nearest syntax class wins even when an outer named Record
        // exists. Its exact native owner was admitted once at file entry.
        let anonymous_owner = if let Some(span) = self.anonymous_class_scope(declaration) {
            let index = self
                .native_class_owners
                .binary_search_by_key(&(span.start, span.end), |(start, end, _)| (*start, *end))
                .map_err(|_| lane_rejection())?;
            if !(span.start <= declaration.start
                && declaration.start <= params_span.start
                && params_span.end <= declaration.end
                && declaration.end <= span.end)
            {
                return Err(lane_rejection());
            }
            Some(self.native_class_owners[index].2)
        } else {
            None
        };
        for parameter in params.items.iter() {
            if !parameter.accessibility.is_some() && !parameter.readonly {
                continue;
            }
            let Some(identifier) = parameter.pattern.get_binding_identifier() else {
                continue;
            };
            let name_bytes =
                self.slice_span(identifier.span)
                    .ok_or(TypeScriptCollectError::Span {
                        start: identifier.span.start,
                        end: identifier.span.end,
                    })?;
            if let Some(existing) = self.fact_at_name_start(identifier.span.start) {
                let Some(index) = usize::try_from(existing).ok() else {
                    continue;
                };
                if self.fact_kinds.get(index) == Some(&EntityKind::Field) {
                    continue;
                }
            }
            let class = if anonymous_owner.is_none() {
                self.enclosing_record(identifier.span.start)
            } else {
                None
            };
            if anonymous_owner.is_none() && class.is_none() {
                continue;
            }
            let type_parameter_start = coordinate(self.facts.type_parameter_len)?;
            let cells = match parameter.type_annotation.as_ref() {
                Some(annotation) => {
                    let inner = annotation.type_annotation.span();
                    self.owner_cells(inner.start, inner.end, 0)?
                }
                None => TypeCells::unknown(TypeReason::Unannotated),
            };
            let extension = self.extension(type_parameter_start)?;
            let fact = with_cells(
                SemanticFact::new(EntityKind::Field, name_bytes, LEAF_PRODUCT)
                    .with_extension(extension),
                cells,
            );
            let ordinal = self.push(fact)?;
            self.register(ordinal, parameter.span, identifier.span, EntityKind::Field)?;
            let index = usize::try_from(ordinal).map_err(|_| lane_rejection())?;
            if let Some(owner) = anonymous_owner {
                self.native_member_parents.push((ordinal, owner));
            } else if let Some(class) = class {
                self.member_parents[index] = class;
            }
            self.parameter_properties.push(ordinal);
        }
        Ok(())
    }

    /// Finds the nearest unnamed class syntax scope for a native declaration
    /// or occurrence. This exact-span comparison never enters an identity;
    /// emitted native ownership requires the separately admitted class proof.
    fn anonymous_class_scope(&self, declaration: Span) -> Option<Span> {
        self.tsz_bound_file?;
        let key = (declaration.start, declaration.end);
        let first = self
            .node_index
            .partition_point(|(span, _)| (span.start, span.end) < key);
        let (span, mut current) = *self.node_index.get(first)?;
        if (span.start, span.end) != key {
            return None;
        }
        let nodes = self.semantic.nodes();
        loop {
            let kind = nodes.get_node(current).kind();
            if matches!(kind, AstKind::Program(_)) {
                return None;
            }
            if let Some(class) = kind.as_class() {
                return class.id.is_none().then_some(class.span);
            }
            let parent = nodes.parent_id(current);
            if parent == current {
                return None;
            }
            current = parent;
        }
    }

    /// Counts already-committed same-kind, same-name, same-owner signatures
    /// whose full structural frame is byte-identical to `fact`. Two overloads
    /// that differ in any committed shape never match; two word-for-word
    /// repeated overload declarations always do.
    fn indistinguishable_signature_twins(
        &self,
        declaration: Span,
        fact: &SemanticFact<'source>,
    ) -> u32 {
        let candidates = self
            .facts_by_name
            .get(fact.name)
            .cloned()
            .unwrap_or_default();
        if candidates.is_empty() {
            return 0;
        }
        let owner = self.enclosing_registered_owner(declaration.start, None);
        let class_scope = self.anonymous_class_scope(declaration);
        let mut twins = 0_u32;
        for ordinal in candidates {
            let Some(index) = usize::try_from(ordinal).ok() else {
                continue;
            };
            if self.fact_kinds.get(index).copied() != Some(fact.kind) {
                continue;
            }
            if self.facts.static_members.get(index).copied() != Some(fact.static_member) {
                continue;
            }
            let decl_start = self.decl_starts.get(index).copied().unwrap_or(UNSET);
            if decl_start == UNSET {
                continue;
            }
            if self.enclosing_registered_owner(decl_start, Some(ordinal)) != owner {
                continue;
            }
            let decl_end = self.decl_ends.get(index).copied().unwrap_or(UNSET);
            if self.anonymous_class_scope(Span::new(decl_start, decl_end)) != class_scope {
                continue;
            }
            if self.committed_frame_matches(index, fact, MAX_TYPE_DEPTH) {
                twins += 1;
            }
        }
        twins
    }

    /// Compares one committed row against a staged fact frame: the declared
    /// record with its nominal cell compared by target, the ordered type
    /// children, and the ordered role-bearing product children.
    fn committed_frame_matches(
        &self,
        index: usize,
        fact: &SemanticFact<'source>,
        depth: u8,
    ) -> bool {
        if depth == 0 {
            return false;
        }
        if self.facts.constructors.get(index).copied() != Some(fact.constructor) {
            return false;
        }
        let Some(committed) = self.facts.type_records.get(index) else {
            return false;
        };
        if !self.record_cells_match(committed, &fact.type_record) {
            return false;
        }
        let nominal_match = match (committed.nominal, fact.type_record.nominal) {
            (None, None) => true,
            (Some(NominalRef::Local(left)), Some(NominalRef::Local(right))) => {
                self.targets_match(left.raw, right.raw, depth - 1)
            }
            (Some(left), Some(right)) => left == right,
            _ => false,
        };
        if !nominal_match {
            return false;
        }
        let committed_count = usize::from(
            self.facts
                .type_child_counts
                .get(index)
                .copied()
                .unwrap_or(0),
        );
        if committed_count != usize::from(fact.type_child_count) {
            return false;
        }
        let committed_start = self
            .facts
            .type_child_starts
            .get(index)
            .copied()
            .unwrap_or(0) as usize;
        for position in 0..committed_count {
            let at = committed_start + position;
            if self.facts.type_child_names.get(at) != Some(&fact.type_children[position].name)
                || self.facts.type_child_flags.get(at) != Some(&fact.type_children[position].flags)
                || !self.targets_match(
                    self.facts
                        .type_child_targets
                        .get(at)
                        .copied()
                        .unwrap_or(u32::MAX),
                    fact.type_children[position].target,
                    depth - 1,
                )
            {
                return false;
            }
        }
        let committed_children =
            usize::from(self.facts.child_counts.get(index).copied().unwrap_or(0));
        if committed_children != usize::from(fact.child_count) {
            return false;
        }
        let child_start = self.facts.child_starts.get(index).copied().unwrap_or(0) as usize;
        for position in 0..committed_children {
            let at = child_start + position;
            if self.facts.child_roles.get(at).copied() != Some(fact.children[position].role)
                || !self.targets_match(
                    self.facts
                        .child_targets
                        .get(at)
                        .copied()
                        .unwrap_or(u32::MAX),
                    fact.children[position].target,
                    depth - 1,
                )
            {
                return false;
            }
        }
        true
    }

    /// Compares two committed rows structurally: the record cells, then the
    /// recursive child frames. Distinct ordinals for genuinely identical
    /// shapes (staging artifacts of repeated lowering) still compare equal.
    fn committed_rows_match(&self, left: u32, right: u32, depth: u8) -> bool {
        if left == right {
            return true;
        }
        if depth == 0 {
            return false;
        }
        let (left, right) = (left as usize, right as usize);
        if self.facts.kinds.get(left) != self.facts.kinds.get(right)
            || self.facts.names.get(left) != self.facts.names.get(right)
            || self.facts.constructors.get(left) != self.facts.constructors.get(right)
        {
            return false;
        }
        let (Some(left_record), Some(right_record)) = (
            self.facts.type_records.get(left),
            self.facts.type_records.get(right),
        ) else {
            return false;
        };
        if !self.record_cells_match(left_record, right_record) {
            return false;
        }
        let nominal_match = match (left_record.nominal, right_record.nominal) {
            (None, None) => true,
            (Some(NominalRef::Local(left)), Some(NominalRef::Local(right))) => {
                self.targets_match(left.raw, right.raw, depth - 1)
            }
            (Some(left), Some(right)) => left == right,
            _ => false,
        };
        if !nominal_match {
            return false;
        }
        let left_count = usize::from(self.facts.type_child_counts.get(left).copied().unwrap_or(0));
        if left_count
            != usize::from(
                self.facts
                    .type_child_counts
                    .get(right)
                    .copied()
                    .unwrap_or(0),
            )
        {
            return false;
        }
        let left_start = self.facts.type_child_starts.get(left).copied().unwrap_or(0) as usize;
        let right_start = self
            .facts
            .type_child_starts
            .get(right)
            .copied()
            .unwrap_or(0) as usize;
        for position in 0..left_count {
            if self.facts.type_child_names.get(left_start + position)
                != self.facts.type_child_names.get(right_start + position)
                || self.facts.type_child_flags.get(left_start + position)
                    != self.facts.type_child_flags.get(right_start + position)
                || !self.targets_match(
                    self.facts
                        .type_child_targets
                        .get(left_start + position)
                        .copied()
                        .unwrap_or(u32::MAX),
                    self.facts
                        .type_child_targets
                        .get(right_start + position)
                        .copied()
                        .unwrap_or(u32::MAX),
                    depth - 1,
                )
            {
                return false;
            }
        }
        let left_children = usize::from(self.facts.child_counts.get(left).copied().unwrap_or(0));
        if left_children != usize::from(self.facts.child_counts.get(right).copied().unwrap_or(0)) {
            return false;
        }
        let left_child_start = self.facts.child_starts.get(left).copied().unwrap_or(0) as usize;
        let right_child_start = self.facts.child_starts.get(right).copied().unwrap_or(0) as usize;
        for position in 0..left_children {
            if self.facts.child_roles.get(left_child_start + position)
                != self.facts.child_roles.get(right_child_start + position)
                || !self.targets_match(
                    self.facts
                        .child_targets
                        .get(left_child_start + position)
                        .copied()
                        .unwrap_or(u32::MAX),
                    self.facts
                        .child_targets
                        .get(right_child_start + position)
                        .copied()
                        .unwrap_or(u32::MAX),
                    depth - 1,
                )
            {
                return false;
            }
        }
        true
    }

    /// Records compare by their closed cells; the pooled child range is
    /// staging geometry compared through the targets instead.
    fn record_cells_match(
        &self,
        left: &SemanticTypeRecord<'_>,
        right: &SemanticTypeRecord<'_>,
    ) -> bool {
        left.tag == right.tag
            && left.payload0 == right.payload0
            && left.payload1 == right.payload1
            && left.text == right.text
            && left.text2 == right.text2
    }

    /// One target coordinate: out-of-range targets (foreign or anonymous
    /// leaves that never became facts) compare by raw coordinate, committed
    /// ordinals compare structurally.
    fn targets_match(&self, left: u32, right: u32, depth: u8) -> bool {
        let committed = u32::try_from(self.facts.len).unwrap_or(u32::MAX);
        if left < committed && right < committed {
            self.committed_rows_match(left, right, depth)
        } else {
            left == right
        }
    }

    /// Pushes one self-nominal declaration fact: the diagonal reference to
    /// its own ordinal is the terminal recursive case that lets mutually
    /// recursive declaration sets stay fully linked through their members.
    fn push_self_nominal(
        &mut self,
        kind: EntityKind,
        constructor: SemanticProductConstructor,
        declaration: Span,
        name: Span,
        rows: &TypeParamRows,
    ) -> Result<(), TypeScriptCollectError> {
        let name_bytes = self.slice_span(name).ok_or(TypeScriptCollectError::Span {
            start: name.start,
            end: name.end,
        })?;
        // A legal TypeScript declaration merge (two `interface`s, `namespace`s,
        // or `enum`s with one name in one scope) is one declaration instance,
        // not two. Reuse the first fact and widen its declaration span so the
        // merge part's members still resolve to that one owner.
        if matches!(
            kind,
            EntityKind::Trait | EntityKind::Module | EntityKind::Enum
        ) && let Some(ordinal) = self.merged_self_nominal(kind, name_bytes, declaration)
        {
            self.fact_at_name.entry(name.start).or_insert(ordinal);
            return Ok(());
        }
        // Generic parameters own resolvable facts exactly as aliases and
        // signatures stage them: without these, every use of an interface or
        // class parameter (defaults, constraints, member annotations) misses
        // its binding and mints a colliding honestly-external twin per site.
        self.push_type_parameter_facts(rows)?;
        let type_parameter_start = self.push_type_params(rows, 0)?;
        let own = coordinate(self.facts.len())?;
        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Nominal);
        record.nominal = Some(NominalRef::Local(EntityId::new(own)));
        let extension = self.extension(type_parameter_start)?;
        let fact = SemanticFact::new(kind, name_bytes, constructor)
            .typed(record)
            .with_extension(extension);
        let ordinal = self.push(fact)?;
        self.register(ordinal, declaration, name, kind)?;
        Ok(())
    }

    /// Finds one registered fact of `kind` and exact `name` bytes already
    /// declared in the same lexical scope as `declaration`, retaining the
    /// exact merge part separately from its primary source span. `None` proves
    /// this declaration opens a new family and must be emitted.
    fn merged_self_nominal(
        &mut self,
        kind: EntityKind,
        name_bytes: &[u8],
        declaration: Span,
    ) -> Option<u32> {
        let candidates = self
            .facts_by_name
            .get(name_bytes)
            .cloned()
            .unwrap_or_default();
        if candidates.is_empty() {
            return None;
        }
        let owner = self.enclosing_registered_owner(declaration.start, None);
        for ordinal in candidates {
            let Some(index) = usize::try_from(ordinal).ok() else {
                continue;
            };
            if self.fact_kinds.get(index).copied() != Some(kind) {
                continue;
            }
            let (decl_start, decl_end) = (
                self.decl_starts.get(index).copied().unwrap_or(UNSET),
                self.decl_ends.get(index).copied().unwrap_or(UNSET),
            );
            if decl_start == UNSET || decl_end == UNSET {
                continue;
            }
            let candidate_owner = self.enclosing_registered_owner(decl_start, Some(ordinal));
            if candidate_owner != owner {
                continue;
            }
            self.merged_declaration_parts
                .entry(ordinal)
                .or_insert_with(|| vec![Span::new(decl_start, decl_end)])
                .push(declaration);
            return Some(ordinal);
        }
        None
    }

    /// Resolves the innermost registered declaring fact containing
    /// `position`, optionally excluding one candidate. Synthetic rows carry
    /// no declaration span and are never owners here.
    fn enclosing_registered_owner(&self, position: u32, exclude: Option<u32>) -> Option<u32> {
        let length = self.facts.len;
        let mut best: Option<(u32, u32)> = None;
        for index in 0..length {
            let ordinal = coordinate(index).ok()?;
            if exclude == Some(ordinal) {
                continue;
            }
            if let Some(part) = self
                .containing_declaration_part(ordinal, Span::new(position, position.checked_add(1)?))
                && best.is_none_or(|(known, _)| part.start >= known)
            {
                best = Some((part.start, ordinal));
            }
        }
        best.map(|(_, ordinal)| ordinal)
    }

    /// True when two variable-binding initializer spans are the same proven
    /// source extent, including both being absent.
    fn binding_init_spans_match(left: Option<Span>, right: Option<Span>) -> bool {
        match (left, right) {
            (None, None) => true,
            (Some(left), Some(right)) => left.start == right.start && left.end == right.end,
            _ => false,
        }
    }

    /// Finds an earlier registered fact of `kind` and exact `name` bytes in
    /// the same lexical scope whose row is byte-identical this bare row (same
    /// record, no children) and whose initializer matches when the kind is a
    /// variable binding. Such rows are indistinguishable in the flattened
    /// lane, so the first is reused instead of minting a rejected twin.
    /// Structurally distinct same-name declarations (overloads, differently
    /// typed block variables, or bindings with different initializers) never
    /// match and stay distinct.
    fn merged_simple_declaration(
        &self,
        kind: EntityKind,
        name_bytes: &[u8],
        declaration: Span,
        record: SemanticTypeRecord<'source>,
        _child_count: u8,
        init_span: Option<Span>,
    ) -> Option<u32> {
        let candidates = self
            .facts_by_name
            .get(name_bytes)
            .cloned()
            .unwrap_or_default();
        if candidates.is_empty() {
            return None;
        }
        let owner = self.enclosing_registered_owner(declaration.start, None);
        for ordinal in candidates {
            let Some(index) = usize::try_from(ordinal).ok() else {
                continue;
            };
            if self.fact_kinds.get(index).copied() != Some(kind) {
                continue;
            }
            if self.facts.type_records.get(index) != Some(&record) {
                continue;
            }
            if self.facts.type_child_counts.get(index).copied() != Some(0) {
                continue;
            }
            let decl_start = self.decl_starts.get(index).copied().unwrap_or(UNSET);
            if decl_start == UNSET {
                continue;
            }
            if self.enclosing_registered_owner(decl_start, Some(ordinal)) != owner {
                continue;
            }
            if matches!(kind, EntityKind::Constant | EntityKind::Static)
                && !Self::binding_init_spans_match(
                    self.binding_init_spans.get(&ordinal).copied().flatten(),
                    init_span,
                )
            {
                continue;
            }
            return Some(ordinal);
        }
        None
    }

    /// Finds an earlier registered fact in the same lexical scope whose kind,
    /// name, declared-type record, type children, and product children are
    /// byte-identical to `fact`. Such rows are indistinguishable in the
    /// flattened lane, so the first is reused instead of minting a rejected
    /// twin; structurally distinct overloads and differently typed members
    /// never match.
    fn merged_fact(&self, declaration: Span, fact: &SemanticFact<'source>) -> Option<u32> {
        let type_child_count = usize::from(fact.type_child_count);
        let child_count = usize::from(fact.child_count);
        let candidates = self
            .facts_by_name
            .get(fact.name)
            .cloned()
            .unwrap_or_default();
        if candidates.is_empty() {
            return None;
        }
        let owner = self.enclosing_registered_owner(declaration.start, None);
        for ordinal in candidates {
            let Some(index) = usize::try_from(ordinal).ok() else {
                continue;
            };
            if self.fact_kinds.get(index).copied() != Some(fact.kind) {
                continue;
            }
            if self.facts.type_records.get(index) != Some(&fact.type_record) {
                continue;
            }
            if usize::from(
                self.facts
                    .type_child_counts
                    .get(index)
                    .copied()
                    .unwrap_or(0),
            ) != type_child_count
            {
                continue;
            }
            let child_start = self
                .facts
                .type_child_starts
                .get(index)
                .copied()
                .unwrap_or(0) as usize;
            let mut same = true;
            for position in 0..type_child_count {
                let candidate = child_start + position;
                let known = (
                    self.facts.type_child_targets.get(candidate).copied(),
                    self.facts
                        .type_child_names
                        .get(candidate)
                        .copied()
                        .flatten(),
                    self.facts.type_child_flags.get(candidate).copied(),
                );
                let asked = fact.type_children.get(position);
                if known.0 != asked.map(|child| child.target)
                    || known.1 != asked.and_then(|child| child.name)
                    || known.2 != asked.map(|child| child.flags)
                {
                    same = false;
                    break;
                }
            }
            if !same {
                continue;
            }
            if usize::from(self.facts.child_counts.get(index).copied().unwrap_or(0)) != child_count
            {
                continue;
            }
            let product_start = self.facts.child_starts.get(index).copied().unwrap_or(0) as usize;
            for position in 0..child_count {
                let product = product_start + position;
                let known = self.facts.child_targets.get(product).copied();
                let role = self.facts.child_roles.get(product).copied();
                let asked = fact.children.get(position);
                if known != asked.map(|child| child.target) || role != asked.map(|child| child.role)
                {
                    same = false;
                    break;
                }
            }
            if !same {
                continue;
            }
            let decl_start = self.decl_starts.get(index).copied().unwrap_or(UNSET);
            if decl_start == UNSET {
                continue;
            }
            if self.enclosing_registered_owner(decl_start, Some(ordinal)) != owner {
                continue;
            }
            return Some(ordinal);
        }
        None
    }

    /// Pushes one import-binding fact with its module-origin foreign key and
    /// the import-site occurrence.
    fn push_import_binding(
        &mut self,
        declaration: Span,
        module: Span,
        local: Span,
        display: Span,
    ) -> Result<(), TypeScriptCollectError> {
        let name_bytes = self.slice_span(local).ok_or(TypeScriptCollectError::Span {
            start: local.start,
            end: local.end,
        })?;
        let display_bytes = self
            .slice_span(display)
            .ok_or(TypeScriptCollectError::Span {
                start: display.start,
                end: display.end,
            })?;
        let mut record = unknown_record(TypeReason::UnresolvedExternal);
        record.text = Some(display_bytes);
        let extension = self.extension_without_type_parameters()?;
        let fact = SemanticFact::new(EntityKind::Reexport, name_bytes, LEAF_PRODUCT)
            .typed(record)
            .with_extension(extension);
        let ordinal = self.push(fact)?;
        self.register(ordinal, declaration, local, EntityKind::Reexport)?;
        self.push_import_module(ImportModule {
            fact: ordinal,
            module_start: module.start,
            module_end: module.end,
            display_start: display.start,
            display_end: display.end,
        })?;
        let relative_start = local.start.checked_sub(declaration.start);
        let relative_end = local.end.checked_sub(declaration.start);
        if let (Some(relative_start), Some(relative_end)) = (relative_start, relative_end) {
            let span = RelSpan::new(relative_start, relative_end).map_err(|_| {
                TypeScriptCollectError::Span {
                    start: relative_start,
                    end: relative_end,
                }
            })?;
            let target = self.import_target(ordinal)?;
            self.facts
                .push_occurrence(
                    ordinal,
                    Occurrence {
                        target,
                        kind: ReferenceKind::Import,
                        confidence: OccurrenceConfidence::Import,
                        span,
                    },
                )
                .map_err(fault)?;
        }
        Ok(())
    }

    /// Builds the foreign occurrence target of one import binding from its
    /// recorded module and imported-name spans.
    fn import_target(
        &self,
        fact: u32,
    ) -> Result<OccurrenceTarget<'source>, TypeScriptCollectError> {
        let rows = self
            .import_modules
            .get(..self.import_module_len)
            .unwrap_or(&[]);
        for row in rows.iter() {
            if row.fact != fact {
                continue;
            }
            let module = self
                .text_span(Span::new(row.module_start, row.module_end))
                .ok_or(TypeScriptCollectError::Span {
                    start: row.module_start,
                    end: row.module_end,
                })?;
            let display = self
                .text_span(Span::new(row.display_start, row.display_end))
                .ok_or(TypeScriptCollectError::Span {
                    start: row.display_start,
                    end: row.display_end,
                })?;
            let origin =
                ForeignOrigin::Package(package_lineage_for_module(module).map_err(|cause| {
                    lineage_fault(cause, Span::new(row.module_start, row.module_end))
                })?);
            let key = ForeignKey::new(origin, display, display, Some(EntityKind::Reexport))
                .map_err(|cause| {
                    foreign_fault(cause, Span::new(row.display_start, row.display_end))
                })?;
            return Ok(OccurrenceTarget::Foreign(key));
        }
        Err(TypeScriptCollectError::Projection(
            TypeScriptProjectionFault::MissingImportBinding { fact },
        ))
    }

    /// Pushes one synthetic fact embodying an anonymous type expression,
    /// named by its exact source spelling (kind [`EntityKind::Alias`]).
    /// A byte-identical embodiment already pushed is reused: the image
    /// identifies declarations by scope, kind, name, parentage, and
    /// structure, so two separate rows for one structural spelling would
    /// collide as twins. Reuse is hash-consing over content the lane
    /// already staged, never a fabricated declaration.
    fn synthetic_cells_fact(
        &mut self,
        span: Span,
        cells: TypeCells<'source>,
    ) -> Result<u32, TypeScriptCollectError> {
        let name = self.slice_span(span).ok_or(TypeScriptCollectError::Span {
            start: span.start,
            end: span.end,
        })?;
        if !cells.truncated
            && let Some(twin) = self.synthetic_twin(name, &cells)
        {
            return Ok(twin);
        }
        let type_parameter_start = coordinate(self.facts.type_parameter_len)?;
        let extension = self.extension(type_parameter_start)?;
        let fact = with_cells(
            SemanticFact::new(EntityKind::Alias, name, LEAF_PRODUCT).with_extension(extension),
            cells,
        );
        let ordinal = self.push(fact)?;
        if let Some(index) = usize::try_from(ordinal).ok() {
            if let Some(slot) = self.synthetic_starts.get_mut(index) {
                *slot = span.start;
            }
            if let Some(slot) = self.synthetic_ends.get_mut(index) {
                *slot = span.end;
            }
        }
        self.synthetic_by_name
            .entry(name)
            .or_default()
            .push(ordinal);
        Ok(ordinal)
    }

    /// Finds an already-pushed synthetic embodiment with byte-identical
    /// name, lattice record, and child links. Only unregistered facts are
    /// candidates: every declared fact registers its spans, so an
    /// unregistered row is exactly one anonymous embodiment. (The kind lane
    /// cannot filter here: it retains its default until registration, so an
    /// unregistered row never carries its true kind.) Child targets compare
    /// by ordinal because every child is either an earlier declared fact or
    /// an already-consed embodiment, both canonical by induction.
    fn synthetic_twin(&self, name: &[u8], cells: &TypeCells<'source>) -> Option<u32> {
        let candidates = self.synthetic_by_name.get(name)?;
        for &ordinal in candidates {
            let index = usize::try_from(ordinal).ok()?;
            if self.decl_starts.get(index) != Some(&UNSET) {
                continue;
            }
            if self.facts.names.get(index) != Some(&name) {
                continue;
            }
            if self.facts.type_records.get(index) != Some(&cells.record) {
                continue;
            }
            let count = usize::from(*self.facts.type_child_counts.get(index)?);
            if count != cells.len {
                continue;
            }
            let start = usize::try_from(*self.facts.type_child_starts.get(index)?).ok()?;
            let end = start.checked_add(count)?;
            let targets = self.facts.type_child_targets.get(start..end)?;
            let names = self.facts.type_child_names.get(start..end)?;
            let flags = self.facts.type_child_flags.get(start..end)?;
            let mut same = true;
            for (position, child) in cells.children.iter().take(cells.len).enumerate() {
                if targets.get(position) != Some(&child.target)
                    || names.get(position) != Some(&child.name)
                    || flags.get(position) != Some(&child.flags)
                {
                    same = false;
                    break;
                }
            }
            if same {
                return Some(ordinal);
            }
        }
        None
    }

    /// Pushes one synthetic honestly-unknown fact used where a structural
    /// position demands a fact ordinal and the source wrote no type.
    fn unannotated_fact(&mut self, fallback_name: Span) -> Result<u32, TypeScriptCollectError> {
        self.synthetic_cells_fact(fallback_name, TypeCells::unknown(TypeReason::Unannotated))
    }

    /// Declares one formal parameter as a `Parameter` fact when the binding
    /// site is not already registered, then walks its annotation for nested
    /// function bindings.
    ///
    /// The annotation is not lowered onto this fact. These parameters belong
    /// to a function type, and the type-literal walk already owns every
    /// member of that annotation. Lowering it here would claim a second
    /// index-signature field to the parameter, and a same-named parameter of
    /// the enclosing signature would then share that field's identity.
    fn declare_formal_parameter_binding(
        &mut self,
        name_span: Span,
        annotation: Option<Span>,
        flags: u8,
        depth: u8,
    ) -> Result<(), TypeScriptCollectError> {
        if self.text_span(name_span).is_some_and(|name| name == "this") {
            return Ok(());
        }
        if self.fact_at_name_start(name_span.start).is_none() {
            let mut params = ParamRows::new();
            params.push(ParamRow {
                name: name_span,
                annotation: None,
                default: None,
                flags,
            })?;
            self.push_parameter_facts(&params)?;
        }
        if let Some(span) = annotation {
            self.declare_bindings_in_span(span.start, span.end, depth.saturating_add(1))?;
        }
        Ok(())
    }

    /// Walks one expression span and declares bindings reachable through
    /// `satisfies`, `as`, and non-null assertions on the initializer plane.
    fn declare_expression_bindings(
        &mut self,
        start: u32,
        end: u32,
        depth: u8,
    ) -> Result<(), TypeScriptCollectError> {
        if depth > MAX_TYPE_DEPTH {
            return Ok(());
        }
        let span = Span::new(start, end);
        let first = self
            .node_index
            .partition_point(|(known, _)| (known.start, known.end) < (span.start, span.end));
        let last = self
            .node_index
            .partition_point(|(known, _)| (known.start, known.end) <= (span.start, span.end));
        for (_, node_id) in self.node_index[first..last].iter() {
            let kind = self.semantic.nodes().get_node(*node_id).kind();
            if let Some(parenthesized) = kind.as_parenthesized_expression() {
                let inner = parenthesized.expression.span();
                return self.declare_expression_bindings(inner.start, inner.end, depth);
            }
            if let Some(non_null) = kind.as_ts_non_null_expression() {
                let inner = non_null.expression.span();
                self.declare_expression_bindings(inner.start, inner.end, depth)?;
                return Ok(());
            }
            if let Some(satisfied) = kind.as_ts_satisfies_expression() {
                let inner = satisfied.expression.span();
                self.declare_expression_bindings(inner.start, inner.end, depth)?;
                let ty = satisfied.type_annotation.span();
                return self.declare_bindings_in_span(ty.start, ty.end, depth.saturating_add(1));
            }
            if let Some(cast) = kind.as_ts_as_expression() {
                let ty = cast.type_annotation.span();
                self.declare_bindings_in_span(ty.start, ty.end, depth.saturating_add(1))?;
                let inner = cast.expression.span();
                return self.declare_expression_bindings(inner.start, inner.end, depth);
            }
            if let Some(cast) = kind.as_ts_type_assertion() {
                let ty = cast.type_annotation.span();
                self.declare_bindings_in_span(ty.start, ty.end, depth.saturating_add(1))?;
                let inner = cast.expression.span();
                return self.declare_expression_bindings(inner.start, inner.end, depth);
            }
        }
        Ok(())
    }

    fn ast_kind_at_exact_span(&self, start: u32, end: u32) -> Option<AstKind<'x>> {
        let first = self
            .node_index
            .partition_point(|(known, _)| known.end <= start);
        for (known, node_id) in self.node_index.get(first..).unwrap_or(&[]) {
            if known.start > start {
                break;
            }
            if known.start == start && known.end == end {
                return Some(self.semantic.nodes().get_node(*node_id).kind());
            }
        }
        None
    }

    /// Walks one assignment-expression span, peeling parenthesized wrappers,
    /// and declares nested function bindings on the right-hand side.
    fn declare_assignment_bindings_in_assignment_expression_span(
        &mut self,
        start: u32,
        end: u32,
    ) -> Result<(), TypeScriptCollectError> {
        let span = Span::new(start, end);
        let first = self
            .node_index
            .partition_point(|(known, _)| (known.start, known.end) < (span.start, span.end));
        let last = self
            .node_index
            .partition_point(|(known, _)| (known.start, known.end) <= (span.start, span.end));
        for (_, node_id) in self.node_index[first..last].iter() {
            let kind = self.semantic.nodes().get_node(*node_id).kind();
            if let Some(parenthesized) = kind.as_parenthesized_expression() {
                let inner = parenthesized.expression.span();
                return self.declare_assignment_bindings_in_assignment_expression_span(
                    inner.start,
                    inner.end,
                );
            }
            if let Some(assignment) = kind.as_assignment_expression() {
                let right = assignment.right.span();
                return self.declare_expression_bindings(right.start, right.end, 0);
            }
        }
        Ok(())
    }

    /// Returns nested statement spans reached from one statement during
    /// assignment-binding walks.
    fn assignment_binding_nested_statement_spans(kind: AstKind<'_>) -> Vec<Span> {
        match kind {
            AstKind::BlockStatement(block) => block
                .body
                .iter()
                .map(|statement| statement.span())
                .collect(),
            AstKind::IfStatement(branch) => {
                let mut spans = vec![branch.consequent.span()];
                if let Some(alternate) = branch.alternate.as_ref() {
                    spans.push(alternate.span());
                }
                spans
            }
            AstKind::WhileStatement(statement) => vec![statement.body.span()],
            AstKind::DoWhileStatement(statement) => vec![statement.body.span()],
            AstKind::ForStatement(statement) => vec![statement.body.span()],
            AstKind::ForInStatement(statement) => vec![statement.body.span()],
            AstKind::ForOfStatement(statement) => vec![statement.body.span()],
            AstKind::LabeledStatement(statement) => vec![statement.body.span()],
            AstKind::WithStatement(statement) => vec![statement.body.span()],
            AstKind::TryStatement(try_statement) => {
                let mut spans = vec![try_statement.block.span()];
                if let Some(handler) = try_statement.handler.as_ref() {
                    spans.push(handler.body.span());
                }
                if let Some(finalizer) = try_statement.finalizer.as_ref() {
                    spans.push(finalizer.span());
                }
                spans
            }
            AstKind::SwitchStatement(switch_statement) => switch_statement
                .cases
                .iter()
                .flat_map(|case| case.consequent.iter())
                .map(|statement| statement.span())
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Walks one statement span for assignment right-hand sides that carry
    /// nested function bindings, including nested blocks.
    fn declare_assignment_bindings_in_statement_span(
        &mut self,
        start: u32,
        end: u32,
    ) -> Result<(), TypeScriptCollectError> {
        let Some(kind) = self.ast_kind_at_exact_span(start, end) else {
            return Ok(());
        };
        if let AstKind::ExpressionStatement(expression) = kind {
            let expression_span = expression.expression.span();
            return self.declare_assignment_bindings_in_assignment_expression_span(
                expression_span.start,
                expression_span.end,
            );
        }
        for span in Self::assignment_binding_nested_statement_spans(kind) {
            self.declare_assignment_bindings_in_statement_span(span.start, span.end)?;
        }
        Ok(())
    }

    /// Declares every function-binding name reachable inside one type span.
    fn declare_bindings_in_span(
        &mut self,
        start: u32,
        end: u32,
        depth: u8,
    ) -> Result<(), TypeScriptCollectError> {
        if depth > MAX_TYPE_DEPTH {
            return Ok(());
        }
        let span = Span::new(start, end);
        let next_depth = depth.saturating_add(1);
        let first = self
            .node_index
            .partition_point(|(known, _)| (known.start, known.end) < (span.start, span.end));
        let last = self
            .node_index
            .partition_point(|(known, _)| (known.start, known.end) <= (span.start, span.end));
        for (_, node_id) in self.node_index[first..last].iter() {
            let kind = self.semantic.nodes().get_node(*node_id).kind();
            if let Some(parenthesized) = kind.as_ts_parenthesized_type() {
                let inner = parenthesized.type_annotation.span();
                return self.declare_bindings_in_span(inner.start, inner.end, next_depth);
            }
            if let Some(optional) = kind.as_ts_optional_type() {
                let inner = optional.type_annotation.span();
                return self.declare_bindings_in_span(inner.start, inner.end, next_depth);
            }
            if let Some(rest) = kind.as_ts_rest_type() {
                let inner = rest.type_annotation.span();
                return self.declare_bindings_in_span(inner.start, inner.end, next_depth);
            }
            if let Some(union) = kind.as_ts_union_type() {
                for member in union.types.iter() {
                    let member_span = member.span();
                    self.declare_bindings_in_span(member_span.start, member_span.end, next_depth)?;
                }
                return Ok(());
            }
            if let Some(intersection) = kind.as_ts_intersection_type() {
                for member in intersection.types.iter() {
                    let member_span = member.span();
                    self.declare_bindings_in_span(member_span.start, member_span.end, next_depth)?;
                }
                return Ok(());
            }
            if let Some(tuple) = kind.as_ts_tuple_type() {
                for element in tuple.element_types.iter() {
                    let element_span = element.span();
                    self.declare_bindings_in_span(
                        element_span.start,
                        element_span.end,
                        next_depth,
                    )?;
                }
                return Ok(());
            }
            if let Some(literal) = kind.as_ts_type_literal() {
                for member in literal.members.iter() {
                    let member_span = member.span();
                    self.declare_bindings_in_span(member_span.start, member_span.end, next_depth)?;
                }
                return Ok(());
            }
            if let Some(function_type) = kind.as_ts_function_type() {
                for parameter in function_type.params.items.iter() {
                    let annotation = parameter
                        .type_annotation
                        .as_ref()
                        .map(|annotation| annotation.type_annotation.span());
                    let flags = if parameter.optional {
                        SemanticTypeChild::FLAG_OPTIONAL
                    } else {
                        0
                    };
                    self.declare_formal_parameter_binding(
                        parameter.pattern.span(),
                        annotation,
                        flags,
                        next_depth,
                    )?;
                    if let Some(init) = parameter.initializer.as_ref() {
                        let init_span = init.span();
                        self.declare_expression_bindings(
                            init_span.start,
                            init_span.end,
                            next_depth,
                        )?;
                    }
                }
                if let Some(rest) = function_type.params.rest.as_ref() {
                    let annotation = rest
                        .type_annotation
                        .as_ref()
                        .map(|annotation| annotation.type_annotation.span());
                    self.declare_formal_parameter_binding(
                        rest.rest.argument.span(),
                        annotation,
                        SemanticTypeChild::FLAG_REST,
                        next_depth,
                    )?;
                }
                let returned = function_type.return_type.type_annotation.span();
                self.declare_bindings_in_span(returned.start, returned.end, next_depth)?;
                return Ok(());
            }
            if let Some(reference) = kind.as_ts_type_reference() {
                if let Some(arguments) = reference.type_arguments.as_ref() {
                    for argument in arguments.params.iter() {
                        let argument_span = argument.span();
                        self.declare_bindings_in_span(
                            argument_span.start,
                            argument_span.end,
                            next_depth,
                        )?;
                    }
                }
                return Ok(());
            }
            if let Some(mapped) = kind.as_ts_mapped_type() {
                let constraint = mapped.constraint.span();
                self.declare_bindings_in_span(constraint.start, constraint.end, next_depth)?;
                if let Some(name_type) = mapped.name_type.as_ref() {
                    let name_span = name_type.span();
                    self.declare_bindings_in_span(name_span.start, name_span.end, next_depth)?;
                }
                if let Some(value) = mapped.type_annotation.as_ref() {
                    let value_span = value.span();
                    self.declare_bindings_in_span(value_span.start, value_span.end, next_depth)?;
                }
                return Ok(());
            }
            if let Some(conditional) = kind.as_ts_conditional_type() {
                let check = conditional.check_type.span();
                let extends = conditional.extends_type.span();
                let true_branch = conditional.true_type.span();
                let false_branch = conditional.false_type.span();
                self.declare_bindings_in_span(check.start, check.end, next_depth)?;
                self.declare_bindings_in_span(extends.start, extends.end, next_depth)?;
                self.declare_bindings_in_span(true_branch.start, true_branch.end, next_depth)?;
                self.declare_bindings_in_span(false_branch.start, false_branch.end, next_depth)?;
                return Ok(());
            }
            if let Some(array) = kind.as_ts_array_type() {
                let element = array.element_type.span();
                return self.declare_bindings_in_span(element.start, element.end, next_depth);
            }
            if let Some(operator) = kind.as_ts_type_operator() {
                let inner = operator.type_annotation.span();
                return self.declare_bindings_in_span(inner.start, inner.end, next_depth);
            }
            if let Some(template) = kind.as_ts_template_literal_type() {
                for substitution in template.types.iter() {
                    let substitution_span = substitution.span();
                    self.declare_bindings_in_span(
                        substitution_span.start,
                        substitution_span.end,
                        next_depth,
                    )?;
                }
                return Ok(());
            }
            if let Some(infer) = kind.as_ts_infer_type() {
                if let Some(constraint) = infer.type_parameter.constraint.as_ref() {
                    let constraint_span = constraint.span();
                    self.declare_bindings_in_span(
                        constraint_span.start,
                        constraint_span.end,
                        next_depth,
                    )?;
                }
                if let Some(default) = infer.type_parameter.default.as_ref() {
                    let default_span = default.span();
                    self.declare_bindings_in_span(
                        default_span.start,
                        default_span.end,
                        next_depth,
                    )?;
                }
                return Ok(());
            }
            if let Some(signature) = kind.as_ts_call_signature_declaration() {
                for parameter in signature.params.items.iter() {
                    if let Some(annotation) = parameter.type_annotation.as_ref() {
                        let inner = annotation.type_annotation.span();
                        self.declare_bindings_in_span(inner.start, inner.end, next_depth)?;
                    }
                }
                if let Some(returned) = signature.return_type.as_ref() {
                    let inner = returned.type_annotation.span();
                    self.declare_bindings_in_span(inner.start, inner.end, next_depth)?;
                }
                return Ok(());
            }
            if let Some(property) = kind.as_ts_property_signature() {
                if let Some(annotation) = property.type_annotation.as_ref() {
                    let inner = annotation.type_annotation.span();
                    return self.declare_bindings_in_span(inner.start, inner.end, next_depth);
                }
                return Ok(());
            }
            if let Some(method) = kind.as_ts_method_signature() {
                for parameter in method.params.items.iter() {
                    if let Some(annotation) = parameter.type_annotation.as_ref() {
                        let inner = annotation.type_annotation.span();
                        self.declare_bindings_in_span(inner.start, inner.end, next_depth)?;
                    }
                }
                if let Some(returned) = method.return_type.as_ref() {
                    let inner = returned.type_annotation.span();
                    self.declare_bindings_in_span(inner.start, inner.end, next_depth)?;
                }
                return Ok(());
            }
            if let Some(index_signature) = kind.as_ts_index_signature() {
                for parameter in index_signature.parameters.iter() {
                    let inner = parameter.type_annotation.type_annotation.span();
                    self.declare_bindings_in_span(inner.start, inner.end, next_depth)?;
                }
                let inner = index_signature.type_annotation.type_annotation.span();
                return self.declare_bindings_in_span(inner.start, inner.end, next_depth);
            }
        }
        Ok(())
    }

    /// Lowers one type expression for direct application to the fact being
    /// declared (an annotation position, not a child position). A resolved
    /// type parameter stays a `TypeVar` naming it; any other resolved fact
    /// becomes a local nominal.
    fn owner_cells(
        &mut self,
        start: u32,
        end: u32,
        depth: u8,
    ) -> Result<TypeCells<'source>, TypeScriptCollectError> {
        self.declare_bindings_in_span(start, end, depth)?;
        match self.lower_type(start, end, depth)? {
            TypeOutcome::Existing(fact) => {
                let index = usize::try_from(fact).map_err(|_| lane_rejection())?;
                if self.fact_kinds.get(index) == Some(&EntityKind::Parameter) {
                    let mut cells = TypeCells::leaf(SemanticTypeTag::TypeVar);
                    cells.record.text = self.slice_span(Span::new(start, end));
                    return Ok(cells);
                }
                let mut cells = TypeCells::leaf(SemanticTypeTag::Nominal);
                cells.record.nominal = Some(NominalRef::Local(EntityId::new(fact)));
                Ok(cells)
            }
            TypeOutcome::Cells(cells) => Ok(cells),
        }
    }

    /// Lowers one type expression and guarantees a fact ordinal embodying it:
    /// declared entities and type parameters resolve to their existing
    /// ordinal; every anonymous expression is pushed as a synthetic
    /// type-expression fact. Members staged while the expression lowered
    /// belong to that embodiment and are claimed to it, including when an
    /// identical embodiment already exists and is reused.
    fn child_target(
        &mut self,
        start: u32,
        end: u32,
        depth: u8,
    ) -> Result<u32, TypeScriptCollectError> {
        self.declare_bindings_in_span(start, end, depth)?;
        let member_base = self.staged_members.len();
        match self.lower_type(start, end, depth)? {
            TypeOutcome::Existing(fact) => Ok(fact),
            TypeOutcome::Cells(cells) => {
                let embodiment = self.synthetic_cells_fact(Span::new(start, end), cells)?;
                self.claim_staged_members(member_base, embodiment);
                Ok(embodiment)
            }
        }
    }

    /// The honest `TypeReason` record for a proven-but-unrepresented construct:
    /// unknown with [`TypeReason::NoIrRepresentation`] and the exact spelling.
    fn unrepresented(&self, span: Span) -> TypeCells<'source> {
        let mut cells = TypeCells::unknown(TypeReason::NoIrRepresentation);
        cells.record.text = self.slice_span(span);
        cells
    }

    /// Lowers the type expression at `[start, end)` by probing OXC's
    /// span-pinned syntax nodes. Every reachable construct maps onto the
    /// closed lattice; an unrepresented construct is honestly unknown with
    /// [`TypeReason::NoIrRepresentation`] and its exact spelling.
    ///
    /// Because every visited OXC syntax node owns a distinct span, the probe
    /// dispatch never misattributes a wrapper's span to an inner expression,
    /// and an unmatched node simply falls through to the next candidate.
    ///
    /// Each syntax arm is its own function. At opt-level 0 every
    /// local in a function is reserved at once, and this walk
    /// recurses once per nested object-literal level. One combined
    /// frame overflowed a 2 MiB stack at twelve levels.
    fn lower_type(
        &mut self,
        start: u32,
        end: u32,
        depth: u8,
    ) -> Result<TypeOutcome<'source>, TypeScriptCollectError> {
        if depth > MAX_TYPE_DEPTH {
            return Ok(TypeOutcome::Cells(TypeCells::unknown(
                TypeReason::TruncatedAtDepthLimit,
            )));
        }
        let span = Span::new(start, end);
        let next_depth = depth.saturating_add(1);
        if let Some(cells) = self.literal_cells(span) {
            return Ok(TypeOutcome::Cells(cells));
        }
        let first = self
            .node_index
            .partition_point(|(known, _)| (known.start, known.end) < (span.start, span.end));
        let last = self
            .node_index
            .partition_point(|(known, _)| (known.start, known.end) <= (span.start, span.end));
        for (_, node_id) in self.node_index[first..last].iter() {
            let node = self.semantic.nodes().get_node(*node_id);
            let kind = node.kind();

            // Transparent wrappers descend into their inner expression span.
            if kind.as_ts_parenthesized_type().is_some() {
                return self.lower_ts_parenthesized_type(kind, span, next_depth);
            }
            if kind.as_ts_named_tuple_member().is_some() {
                return self.lower_ts_named_tuple_member(kind, span, next_depth);
            }
            if kind.as_ts_optional_type().is_some() {
                return self.lower_ts_optional_type(kind, span, next_depth);
            }
            if kind.as_ts_rest_type().is_some() {
                return self.lower_ts_rest_type(kind, span, next_depth);
            }
            if kind.as_ts_union_type().is_some() {
                return self.lower_ts_union_type(kind, span, next_depth);
            }
            if kind.as_ts_template_literal_type().is_some() {
                return self.lower_ts_template_literal_type(kind, span, next_depth);
            }
            if kind.as_ts_intersection_type().is_some() {
                return self.lower_ts_intersection_type(kind, span, next_depth);
            }
            if kind.as_ts_tuple_type().is_some() {
                return self.lower_ts_tuple_type(kind, span, next_depth);
            }
            if kind.as_ts_type_literal().is_some() {
                return self.lower_ts_type_literal(kind, span, next_depth);
            }
            if kind.as_ts_function_type().is_some() {
                return self.lower_ts_function_type(kind, span, next_depth);
            }
            if kind.as_ts_type_reference().is_some() {
                return self.lower_ts_type_reference(kind, span, next_depth);
            }
            if kind.as_ts_type_query().is_some() {
                return self.lower_ts_type_query(kind, span);
            }
            if kind.as_ts_mapped_type().is_some() {
                return self.lower_ts_mapped_type(kind, span, next_depth);
            }
            if kind.as_ts_conditional_type().is_some() {
                return self.lower_ts_conditional_type(kind, span, next_depth);
            }
            if kind.as_ts_array_type().is_some() {
                return self.lower_ts_array_type(kind, span, next_depth);
            }
            if kind.as_ts_type_operator().is_some() {
                return self.lower_ts_type_operator(kind, span, next_depth);
            }
            if kind.as_ts_this_type().is_some() {
                return self.lower_ts_this_type(kind);
            }
            if kind.as_ts_number_keyword().is_some() {
                return self.lower_ts_number_keyword(kind);
            }
            if kind.as_ts_string_keyword().is_some() {
                return self.lower_ts_string_keyword(kind);
            }
            if kind.as_ts_boolean_keyword().is_some() {
                return self.lower_ts_boolean_keyword(kind);
            }
            if kind.as_ts_big_int_keyword().is_some() {
                return self.lower_ts_big_int_keyword(kind);
            }
            if kind.as_ts_void_keyword().is_some() {
                return self.lower_ts_void_keyword(kind);
            }
            if kind.as_ts_null_keyword().is_some() {
                return self.lower_ts_null_keyword(kind);
            }
            if kind.as_ts_undefined_keyword().is_some() {
                return self.lower_ts_undefined_keyword(kind);
            }
            if kind.as_ts_any_keyword().is_some() {
                return self.lower_ts_any_keyword(kind);
            }
            if kind.as_ts_unknown_keyword().is_some() {
                return self.lower_ts_unknown_keyword(kind);
            }
            if kind.as_ts_never_keyword().is_some() {
                return self.lower_ts_never_keyword(kind);
            }
        }
        Ok(TypeOutcome::Cells(self.unrepresented(span)))
    }

    /// One syntax arm of [`Self::lower_type`].
    ///
    /// Split out so opt-level 0 does not reserve every other arm's
    /// locals on this recursive frame.
    #[inline(never)]
    fn lower_ts_parenthesized_type(
        &mut self,
        kind: AstKind<'x>,
        span: Span,
        next_depth: u8,
    ) -> Result<TypeOutcome<'source>, TypeScriptCollectError> {
        if let Some(parenthesized) = kind.as_ts_parenthesized_type() {
            let inner = parenthesized.type_annotation.span();
            return self.lower_type(inner.start, inner.end, next_depth);
        }
        Ok(TypeOutcome::Cells(self.unrepresented(span)))
    }

    /// One syntax arm of [`Self::lower_type`].
    ///
    /// Split out so opt-level 0 does not reserve every other arm's
    /// locals on this recursive frame.
    #[inline(never)]
    fn lower_ts_named_tuple_member(
        &mut self,
        kind: AstKind<'x>,
        span: Span,
        next_depth: u8,
    ) -> Result<TypeOutcome<'source>, TypeScriptCollectError> {
        if let Some(named) = kind.as_ts_named_tuple_member() {
            let inner = named.element_type.span();
            return self.lower_type(inner.start, inner.end, next_depth);
        }
        Ok(TypeOutcome::Cells(self.unrepresented(span)))
    }

    /// One syntax arm of [`Self::lower_type`].
    ///
    /// Split out so opt-level 0 does not reserve every other arm's
    /// locals on this recursive frame.
    #[inline(never)]
    fn lower_ts_optional_type(
        &mut self,
        kind: AstKind<'x>,
        span: Span,
        next_depth: u8,
    ) -> Result<TypeOutcome<'source>, TypeScriptCollectError> {
        if let Some(optional) = kind.as_ts_optional_type() {
            let inner = optional.type_annotation.span();
            return self.lower_type(inner.start, inner.end, next_depth);
        }
        Ok(TypeOutcome::Cells(self.unrepresented(span)))
    }

    /// One syntax arm of [`Self::lower_type`].
    ///
    /// Split out so opt-level 0 does not reserve every other arm's
    /// locals on this recursive frame.
    #[inline(never)]
    fn lower_ts_rest_type(
        &mut self,
        kind: AstKind<'x>,
        span: Span,
        next_depth: u8,
    ) -> Result<TypeOutcome<'source>, TypeScriptCollectError> {
        if let Some(rest) = kind.as_ts_rest_type() {
            let inner = rest.type_annotation.span();
            return self.lower_type(inner.start, inner.end, next_depth);
        }
        Ok(TypeOutcome::Cells(self.unrepresented(span)))
    }

    /// One syntax arm of [`Self::lower_type`].
    ///
    /// Split out so opt-level 0 does not reserve every other arm's
    /// locals on this recursive frame.
    #[inline(never)]
    fn lower_ts_union_type(
        &mut self,
        kind: AstKind<'x>,
        span: Span,
        next_depth: u8,
    ) -> Result<TypeOutcome<'source>, TypeScriptCollectError> {
        if let Some(union) = kind.as_ts_union_type() {
            if union.types.len() > MAX_TYPE_CHILDREN {
                let spans: Vec<_> = union.types.iter().map(GetSpan::span).collect();
                return self.associative_cells(&spans, next_depth, SemanticTypeTag::Union);
            }
            let mut cells = TypeCells::leaf(SemanticTypeTag::Union);
            for member in union.types.iter() {
                let member_span = member.span();
                let target = self.child_target(member_span.start, member_span.end, next_depth)?;
                cells.push_child(target, None, 0)?;
            }
            return Ok(TypeOutcome::Cells(cells));
        }
        Ok(TypeOutcome::Cells(self.unrepresented(span)))
    }

    /// One syntax arm of [`Self::lower_type`].
    ///
    /// Split out so opt-level 0 does not reserve every other arm's
    /// locals on this recursive frame.
    #[inline(never)]
    fn lower_ts_template_literal_type(
        &mut self,
        kind: AstKind<'x>,
        span: Span,
        next_depth: u8,
    ) -> Result<TypeOutcome<'source>, TypeScriptCollectError> {
        if let Some(template) = kind.as_ts_template_literal_type() {
            let width = template.quasis.len().saturating_add(template.types.len());
            if width > MAX_TYPE_CHILDREN {
                return Err(fault(FactFault::TypeProjectionWidth {
                    actual: width,
                    maximum: MAX_TYPE_CHILDREN,
                }));
            }
            let mut cells = TypeCells::leaf(SemanticTypeTag::TemplateLiteral);
            // OXC preserves one quasi before, between, and after every
            // substitution. Keep that ordered alternating sequence in the
            // canonical child lane; `record.text` cannot represent it.
            for (position, quasi) in template.quasis.iter().enumerate() {
                let text = self
                    .slice_span(quasi.span)
                    .ok_or(TypeScriptCollectError::Span {
                        start: quasi.span.start,
                        end: quasi.span.end,
                    })?;
                cells.push_child(u32::MAX, Some(text), 0)?;
                if let Some(substitution) = template.types.get(position) {
                    let substitution_span = substitution.span();
                    let target = self.child_target(
                        substitution_span.start,
                        substitution_span.end,
                        next_depth,
                    )?;
                    cells.push_child(target, None, 0)?;
                }
            }
            return Ok(TypeOutcome::Cells(cells));
        }
        Ok(TypeOutcome::Cells(self.unrepresented(span)))
    }

    /// One syntax arm of [`Self::lower_type`].
    ///
    /// Split out so opt-level 0 does not reserve every other arm's
    /// locals on this recursive frame.
    #[inline(never)]
    fn lower_ts_intersection_type(
        &mut self,
        kind: AstKind<'x>,
        span: Span,
        next_depth: u8,
    ) -> Result<TypeOutcome<'source>, TypeScriptCollectError> {
        if let Some(intersection) = kind.as_ts_intersection_type() {
            if intersection.types.len() > MAX_TYPE_CHILDREN {
                let spans: Vec<_> = intersection.types.iter().map(GetSpan::span).collect();
                return self.associative_cells(&spans, next_depth, SemanticTypeTag::Intersection);
            }
            let mut cells = TypeCells::leaf(SemanticTypeTag::Intersection);
            for member in intersection.types.iter() {
                let member_span = member.span();
                let target = self.child_target(member_span.start, member_span.end, next_depth)?;
                cells.push_child(target, None, 0)?;
            }
            return Ok(TypeOutcome::Cells(cells));
        }
        Ok(TypeOutcome::Cells(self.unrepresented(span)))
    }

    /// One syntax arm of [`Self::lower_type`].
    ///
    /// Split out so opt-level 0 does not reserve every other arm's
    /// locals on this recursive frame.
    #[inline(never)]
    fn lower_ts_tuple_type(
        &mut self,
        kind: AstKind<'x>,
        span: Span,
        next_depth: u8,
    ) -> Result<TypeOutcome<'source>, TypeScriptCollectError> {
        if let Some(tuple) = kind.as_ts_tuple_type() {
            if tuple.element_types.len() > MAX_TYPE_CHILDREN {
                return Err(fault(FactFault::TypeProjectionWidth {
                    actual: tuple.element_types.len(),
                    maximum: MAX_TYPE_CHILDREN,
                }));
            }
            let mut cells = TypeCells::leaf(SemanticTypeTag::Tuple);
            for element in tuple.element_types.iter() {
                let element_span = element.span();
                let mut label: Option<Span> = None;
                let mut inner = element_span;
                let mut flags = 0_u8;
                if let Some(position) = self
                    .node_index
                    .binary_search_by_key(&(element_span.start, element_span.end), |(known, _)| {
                        (known.start, known.end)
                    })
                    .ok()
                {
                    let Some((_, node_id)) = self.node_index.get(position) else {
                        continue;
                    };
                    let wrapped = self.semantic.nodes().get_node(*node_id);
                    let wrapped_kind = wrapped.kind();
                    if let Some(named) = wrapped_kind.as_ts_named_tuple_member() {
                        label = Some(named.label.span);
                        inner = named.element_type.span();
                        if named.optional {
                            flags |= SemanticTypeChild::FLAG_OPTIONAL;
                        }
                    } else if let Some(optional) = wrapped_kind.as_ts_optional_type() {
                        inner = optional.type_annotation.span();
                        flags |= SemanticTypeChild::FLAG_OPTIONAL;
                    } else if let Some(rest) = wrapped_kind.as_ts_rest_type() {
                        inner = rest.type_annotation.span();
                        flags |= SemanticTypeChild::FLAG_REST;
                    }
                }
                let target = self.child_target(inner.start, inner.end, next_depth)?;
                let name = match label {
                    Some(label_span) => Some(self.slice_span(label_span).ok_or(
                        TypeScriptCollectError::Span {
                            start: label_span.start,
                            end: label_span.end,
                        },
                    )?),
                    None => None,
                };
                cells.push_child(target, name, flags)?;
            }
            return Ok(TypeOutcome::Cells(cells));
        }
        Ok(TypeOutcome::Cells(self.unrepresented(span)))
    }

    /// One syntax arm of [`Self::lower_type`].
    ///
    /// Split out so opt-level 0 does not reserve every other arm's
    /// locals on this recursive frame.
    #[inline(never)]
    fn lower_ts_type_literal(
        &mut self,
        kind: AstKind<'x>,
        span: Span,
        next_depth: u8,
    ) -> Result<TypeOutcome<'source>, TypeScriptCollectError> {
        if let Some(literal) = kind.as_ts_type_literal() {
            if literal.members.len() > MAX_TYPE_CHILDREN {
                return Err(fault(FactFault::TypeProjectionWidth {
                    actual: literal.members.len(),
                    maximum: MAX_TYPE_CHILDREN,
                }));
            }
            let mut cells = TypeCells::leaf(SemanticTypeTag::AnonymousRecord);
            cells.record.payload0 = u32::from(AnonRecordForm::Interface);
            for member in literal.members.iter() {
                let member_span = member.span();
                let member_fact = self.push_type_literal_member(member_span, next_depth)?;
                if let Some((member_fact, name, flags)) = member_fact {
                    cells.push_child(member_fact, Some(name), flags)?;
                }
            }
            return Ok(TypeOutcome::Cells(cells));
        }
        Ok(TypeOutcome::Cells(self.unrepresented(span)))
    }

    /// One syntax arm of [`Self::lower_type`].
    ///
    /// Split out so opt-level 0 does not reserve every other arm's
    /// locals on this recursive frame.
    #[inline(never)]
    fn lower_ts_function_type(
        &mut self,
        kind: AstKind<'x>,
        span: Span,
        next_depth: u8,
    ) -> Result<TypeOutcome<'source>, TypeScriptCollectError> {
        if let Some(function_type) = kind.as_ts_function_type() {
            let width = function_type
                .params
                .items
                .len()
                .saturating_add(usize::from(function_type.params.rest.is_some()))
                .saturating_add(1);
            if width > MAX_TYPE_CHILDREN {
                return Err(fault(FactFault::TypeProjectionWidth {
                    actual: width,
                    maximum: MAX_TYPE_CHILDREN,
                }));
            }
            let mut cells = TypeCells::leaf(SemanticTypeTag::FunctionPointer);
            for parameter in function_type.params.items.iter() {
                let target = match parameter.type_annotation.as_ref() {
                    Some(annotation) => {
                        let inner = annotation.type_annotation.span();
                        self.child_target(inner.start, inner.end, next_depth)?
                    }
                    None => self.unannotated_fact(parameter.span())?,
                };
                let flags = if parameter.optional {
                    SemanticTypeChild::FLAG_OPTIONAL
                } else {
                    0
                };
                // A standalone function type mints no `Parameter` fact
                // for its parameters, so the written pattern is the only
                // source of a name; pass it explicitly rather than
                // leaving the child unnamed.
                let name = self.slice_span(parameter.pattern.span());
                cells.push_child(target, name, flags)?;
            }
            if let Some(rest) = function_type.params.rest.as_ref() {
                let target = match rest.type_annotation.as_ref() {
                    Some(annotation) => {
                        let inner = annotation.type_annotation.span();
                        self.child_target(inner.start, inner.end, next_depth)?
                    }
                    None => self.unannotated_fact(rest.span())?,
                };
                let name = self.slice_span(rest.rest.argument.span());
                cells.push_child(target, name, SemanticTypeChild::FLAG_REST)?;
            }
            let returned = function_type.return_type.type_annotation.span();
            let target = self.child_target(returned.start, returned.end, next_depth)?;
            cells.record.payload1 = SemanticTypeRecord::FUNCTION_RESULT_COUNT_ONE;
            cells.push_child(target, None, 0)?;
            // The rest element is pushed after every fixed parameter, so
            // it is the final parameter exactly when present, and the
            // record must declare the typed-last variadic form for it.
            if function_type.params.rest.is_some() {
                cells.record.payload0 = SemanticTypeRecord::FUNCTION_TYPED_VARIADIC_FLAG;
            }
            return Ok(TypeOutcome::Cells(cells));
        }
        Ok(TypeOutcome::Cells(self.unrepresented(span)))
    }

    /// One syntax arm of [`Self::lower_type`].
    ///
    /// Split out so opt-level 0 does not reserve every other arm's
    /// locals on this recursive frame.
    #[inline(never)]
    fn lower_ts_type_reference(
        &mut self,
        kind: AstKind<'x>,
        span: Span,
        next_depth: u8,
    ) -> Result<TypeOutcome<'source>, TypeScriptCollectError> {
        if let Some(reference) = kind.as_ts_type_reference() {
            let name_span = reference.type_name.span();
            let mut base = self.local_fact_at(name_span.start);
            if base.is_none() {
                // OXC missed the binding; the checker may still have
                // resolved it, either to a same-file declaration or to a
                // foreign module origin.
                base = self.checker_local_target(name_span);
            }
            if let Some(fact) = base {
                // A use of a generic parameter names its declared
                // parameter fact. Owner positions inline it as a `TypeVar`
                // in [`Projector::owner_cells`]; child positions link at
                // the fact itself, so one argument site never mints a
                // redundant per-use embodiment that collides with the next
                // spelled use.
                if self
                    .fact_kinds
                    .get(usize::try_from(fact).map_err(|_| lane_rejection())?)
                    == Some(&EntityKind::Parameter)
                {
                    return Ok(TypeOutcome::Existing(fact));
                }
                if reference.type_arguments.is_some() {
                    let mut cells = TypeCells::leaf(SemanticTypeTag::Apply);
                    cells.push_child(fact, None, 0)?;
                    if let Some(arguments) = reference.type_arguments.as_ref() {
                        for argument in arguments.params.iter() {
                            let argument_span = argument.span();
                            let target = self.child_target(
                                argument_span.start,
                                argument_span.end,
                                next_depth,
                            )?;
                            cells.push_child(target, None, 0)?;
                        }
                    }
                    return Ok(TypeOutcome::Cells(cells));
                }
                return Ok(TypeOutcome::Existing(fact));
            }
            if let Some(module) = self.checker_foreign_module(name_span) {
                let fragment = ExternalFragmentId::from_canonical_bytes(module.as_bytes());
                let mut constructor = TypeCells::leaf(SemanticTypeTag::Nominal);
                constructor.record.nominal =
                    Some(NominalRef::External(ExternalEntityRef::bind(fragment, 0)));
                constructor.record.text = self.slice_span(name_span);
                if reference.type_arguments.is_some() {
                    let constructor = self.synthetic_cells_fact(name_span, constructor)?;
                    let mut cells = TypeCells::leaf(SemanticTypeTag::Apply);
                    cells.push_child(constructor, None, 0)?;
                    if let Some(arguments) = reference.type_arguments.as_ref() {
                        for argument in arguments.params.iter() {
                            let argument_span = argument.span();
                            let target = self.child_target(
                                argument_span.start,
                                argument_span.end,
                                next_depth,
                            )?;
                            cells.push_child(target, None, 0)?;
                        }
                    }
                    return Ok(TypeOutcome::Cells(cells));
                }
                return Ok(TypeOutcome::Cells(constructor));
            }
            // Genuinely unresolvable names stay honestly external; a
            // checker-resolved foreign module type is known and named
            // but has no foreign-nominal row form in the closed lattice,
            // so its exact spelling backs the `TypeReason` instead.
            let reason = if self.checker_foreign_resolved(name_span) {
                TypeReason::NoIrRepresentation
            } else {
                TypeReason::UnresolvedExternal
            };
            let mut cells = TypeCells::unknown(reason);
            cells.record.text = self.slice_span(name_span);
            return Ok(TypeOutcome::Cells(cells));
        }
        Ok(TypeOutcome::Cells(self.unrepresented(span)))
    }

    /// One syntax arm of [`Self::lower_type`].
    ///
    /// Split out so opt-level 0 does not reserve every other arm's
    /// locals on this recursive frame.
    #[inline(never)]
    fn lower_ts_type_query(
        &mut self,
        kind: AstKind<'x>,
        span: Span,
    ) -> Result<TypeOutcome<'source>, TypeScriptCollectError> {
        if let Some(query) = kind.as_ts_type_query() {
            let inner = query.expr_name.span();
            if let Some(target) = self.type_query_entity_at(span, inner) {
                let mut cells = TypeCells::leaf(SemanticTypeTag::TypeOf);
                cells.record.payload0 = target;
                return Ok(TypeOutcome::Cells(cells));
            }
            let mut cells = TypeCells::unknown(TypeReason::UnresolvedExternal);
            cells.record.text = self.slice_span(span);
            return Ok(TypeOutcome::Cells(cells));
        }
        Ok(TypeOutcome::Cells(self.unrepresented(span)))
    }

    /// One syntax arm of [`Self::lower_type`].
    ///
    /// Split out so opt-level 0 does not reserve every other arm's
    /// locals on this recursive frame.
    #[inline(never)]
    fn lower_ts_mapped_type(
        &mut self,
        kind: AstKind<'x>,
        span: Span,
        next_depth: u8,
    ) -> Result<TypeOutcome<'source>, TypeScriptCollectError> {
        if let Some(mapped) = kind.as_ts_mapped_type() {
            let mut cells = TypeCells::leaf(SemanticTypeTag::Mapped);
            cells.record.payload0 =
                syntax_mapped_modifier_cell(syntax_mapped_modifier(mapped.readonly));
            cells.record.payload1 =
                syntax_mapped_modifier_cell(syntax_mapped_modifier(mapped.optional));
            cells.record.text = self.slice_span(mapped.key.span);
            let constraint = mapped.constraint.span();
            let constraint_target =
                self.child_target(constraint.start, constraint.end, next_depth)?;
            let name_as_target = mapped
                .name_type
                .as_ref()
                .map(|name_type| {
                    let span = name_type.span();
                    self.child_target(span.start, span.end, next_depth)
                })
                .transpose()?;
            let value_target = match mapped.type_annotation.as_ref() {
                Some(value) => {
                    let value_span = value.span();
                    self.child_target(value_span.start, value_span.end, next_depth)?
                }
                None => self.unannotated_fact(span)?,
            };
            cells.push_child(constraint_target, None, 0)?;
            if let Some(name_as_target) = name_as_target {
                cells.push_child(name_as_target, None, 0)?;
            }
            cells.push_child(value_target, None, 0)?;
            return Ok(TypeOutcome::Cells(cells));
        }
        Ok(TypeOutcome::Cells(self.unrepresented(span)))
    }

    /// One syntax arm of [`Self::lower_type`].
    ///
    /// Split out so opt-level 0 does not reserve every other arm's
    /// locals on this recursive frame.
    #[inline(never)]
    fn lower_ts_conditional_type(
        &mut self,
        kind: AstKind<'x>,
        span: Span,
        next_depth: u8,
    ) -> Result<TypeOutcome<'source>, TypeScriptCollectError> {
        if let Some(conditional) = kind.as_ts_conditional_type() {
            let mut cells = TypeCells::leaf(SemanticTypeTag::Conditional);
            let check = conditional.check_type.span();
            let extends = conditional.extends_type.span();
            let true_branch = conditional.true_type.span();
            let false_branch = conditional.false_type.span();
            let check_target = self.child_target(check.start, check.end, next_depth)?;
            let extends_target = self.child_target(extends.start, extends.end, next_depth)?;
            let true_target = self.child_target(true_branch.start, true_branch.end, next_depth)?;
            let false_target =
                self.child_target(false_branch.start, false_branch.end, next_depth)?;
            cells.push_child(check_target, None, 0)?;
            cells.push_child(extends_target, None, 0)?;
            cells.push_child(true_target, None, 0)?;
            cells.push_child(false_target, None, 0)?;
            return Ok(TypeOutcome::Cells(cells));
        }
        Ok(TypeOutcome::Cells(self.unrepresented(span)))
    }

    /// One syntax arm of [`Self::lower_type`].
    ///
    /// Split out so opt-level 0 does not reserve every other arm's
    /// locals on this recursive frame.
    #[inline(never)]
    fn lower_ts_array_type(
        &mut self,
        kind: AstKind<'x>,
        span: Span,
        next_depth: u8,
    ) -> Result<TypeOutcome<'source>, TypeScriptCollectError> {
        if let Some(array) = kind.as_ts_array_type() {
            let element = array.element_type.span();
            let element_target = self.child_target(element.start, element.end, next_depth)?;
            let mut cells = TypeCells::leaf(SemanticTypeTag::ArraySequence);
            cells.push_child(element_target, None, 0)?;
            return Ok(TypeOutcome::Cells(cells));
        }
        Ok(TypeOutcome::Cells(self.unrepresented(span)))
    }

    /// One syntax arm of [`Self::lower_type`].
    ///
    /// Split out so opt-level 0 does not reserve every other arm's
    /// locals on this recursive frame.
    #[inline(never)]
    fn lower_ts_type_operator(
        &mut self,
        kind: AstKind<'x>,
        span: Span,
        next_depth: u8,
    ) -> Result<TypeOutcome<'source>, TypeScriptCollectError> {
        if let Some(operator) = kind.as_ts_type_operator() {
            let full = self.slice_span(span).unwrap_or(&[]);
            if full.starts_with(b"readonly") {
                let inner = operator.type_annotation.span();
                let inner_target = self.child_target(inner.start, inner.end, next_depth)?;
                let mut cells = TypeCells::leaf(SemanticTypeTag::Annotated);
                cells.record.payload0 = AnnotationKind::Readonly as u32;
                cells.push_child(inner_target, None, 0)?;
                return Ok(TypeOutcome::Cells(cells));
            }
            return Ok(TypeOutcome::Cells(self.unrepresented(span)));
        }
        Ok(TypeOutcome::Cells(self.unrepresented(span)))
    }

    /// One syntax arm of [`Self::lower_type`].
    ///
    /// Split out so opt-level 0 does not reserve every other arm's
    /// locals on this recursive frame.
    #[inline(never)]
    fn lower_ts_this_type(
        &mut self,
        kind: AstKind<'x>,
    ) -> Result<TypeOutcome<'source>, TypeScriptCollectError> {
        if kind.as_ts_this_type().is_some() {
            let mut cells = TypeCells::leaf(SemanticTypeTag::SelfType);
            cells.record.text = Some(&b"this"[..]);
            return Ok(TypeOutcome::Cells(cells));
        }
        Ok(TypeOutcome::Cells(TypeCells::unknown(
            TypeReason::NoIrRepresentation,
        )))
    }

    /// One syntax arm of [`Self::lower_type`].
    ///
    /// Split out so opt-level 0 does not reserve every other arm's
    /// locals on this recursive frame.
    #[inline(never)]
    fn lower_ts_number_keyword(
        &mut self,
        kind: AstKind<'x>,
    ) -> Result<TypeOutcome<'source>, TypeScriptCollectError> {
        if let Some(number) = kind.as_ts_number_keyword() {
            let _ = number;
            let mut cells = TypeCells::leaf(SemanticTypeTag::Primitive);
            cells.record.payload0 = u32::from(PrimitiveShape::Float);
            cells.record.payload1 = TypeWidth::Fixed(64).to_cell();
            return Ok(TypeOutcome::Cells(cells));
        }
        Ok(TypeOutcome::Cells(TypeCells::unknown(
            TypeReason::NoIrRepresentation,
        )))
    }

    /// One syntax arm of [`Self::lower_type`].
    ///
    /// Split out so opt-level 0 does not reserve every other arm's
    /// locals on this recursive frame.
    #[inline(never)]
    fn lower_ts_string_keyword(
        &mut self,
        kind: AstKind<'x>,
    ) -> Result<TypeOutcome<'source>, TypeScriptCollectError> {
        if kind.as_ts_string_keyword().is_some() {
            let mut cells = TypeCells::leaf(SemanticTypeTag::Primitive);
            cells.record.payload0 = u32::from(PrimitiveShape::Str);
            return Ok(TypeOutcome::Cells(cells));
        }
        Ok(TypeOutcome::Cells(TypeCells::unknown(
            TypeReason::NoIrRepresentation,
        )))
    }

    /// One syntax arm of [`Self::lower_type`].
    ///
    /// Split out so opt-level 0 does not reserve every other arm's
    /// locals on this recursive frame.
    #[inline(never)]
    fn lower_ts_boolean_keyword(
        &mut self,
        kind: AstKind<'x>,
    ) -> Result<TypeOutcome<'source>, TypeScriptCollectError> {
        if kind.as_ts_boolean_keyword().is_some() {
            let mut cells = TypeCells::leaf(SemanticTypeTag::Primitive);
            cells.record.payload0 = u32::from(PrimitiveShape::Bool);
            return Ok(TypeOutcome::Cells(cells));
        }
        Ok(TypeOutcome::Cells(TypeCells::unknown(
            TypeReason::NoIrRepresentation,
        )))
    }

    /// One syntax arm of [`Self::lower_type`].
    ///
    /// Split out so opt-level 0 does not reserve every other arm's
    /// locals on this recursive frame.
    #[inline(never)]
    fn lower_ts_big_int_keyword(
        &mut self,
        kind: AstKind<'x>,
    ) -> Result<TypeOutcome<'source>, TypeScriptCollectError> {
        if kind.as_ts_big_int_keyword().is_some() {
            let mut cells = TypeCells::leaf(SemanticTypeTag::Primitive);
            cells.record.payload0 = u32::from(PrimitiveShape::Builtin);
            cells.record.text = Some(&b"bigint"[..]);
            return Ok(TypeOutcome::Cells(cells));
        }
        Ok(TypeOutcome::Cells(TypeCells::unknown(
            TypeReason::NoIrRepresentation,
        )))
    }

    /// One syntax arm of [`Self::lower_type`].
    ///
    /// Split out so opt-level 0 does not reserve every other arm's
    /// locals on this recursive frame.
    #[inline(never)]
    fn lower_ts_void_keyword(
        &mut self,
        kind: AstKind<'x>,
    ) -> Result<TypeOutcome<'source>, TypeScriptCollectError> {
        if kind.as_ts_void_keyword().is_some() {
            let mut cells = TypeCells::leaf(SemanticTypeTag::Primitive);
            cells.record.payload0 = u32::from(PrimitiveShape::Builtin);
            cells.record.text = Some(&b"void"[..]);
            return Ok(TypeOutcome::Cells(cells));
        }
        Ok(TypeOutcome::Cells(TypeCells::unknown(
            TypeReason::NoIrRepresentation,
        )))
    }

    /// One syntax arm of [`Self::lower_type`].
    ///
    /// Split out so opt-level 0 does not reserve every other arm's
    /// locals on this recursive frame.
    #[inline(never)]
    fn lower_ts_null_keyword(
        &mut self,
        kind: AstKind<'x>,
    ) -> Result<TypeOutcome<'source>, TypeScriptCollectError> {
        if kind.as_ts_null_keyword().is_some() {
            let mut cells = TypeCells::leaf(SemanticTypeTag::Primitive);
            cells.record.payload0 = u32::from(PrimitiveShape::Builtin);
            cells.record.text = Some(&b"null"[..]);
            return Ok(TypeOutcome::Cells(cells));
        }
        Ok(TypeOutcome::Cells(TypeCells::unknown(
            TypeReason::NoIrRepresentation,
        )))
    }

    /// One syntax arm of [`Self::lower_type`].
    ///
    /// Split out so opt-level 0 does not reserve every other arm's
    /// locals on this recursive frame.
    #[inline(never)]
    fn lower_ts_undefined_keyword(
        &mut self,
        kind: AstKind<'x>,
    ) -> Result<TypeOutcome<'source>, TypeScriptCollectError> {
        if kind.as_ts_undefined_keyword().is_some() {
            let mut cells = TypeCells::leaf(SemanticTypeTag::Primitive);
            cells.record.payload0 = u32::from(PrimitiveShape::Builtin);
            cells.record.text = Some(&b"undefined"[..]);
            return Ok(TypeOutcome::Cells(cells));
        }
        Ok(TypeOutcome::Cells(TypeCells::unknown(
            TypeReason::NoIrRepresentation,
        )))
    }

    /// One syntax arm of [`Self::lower_type`].
    ///
    /// Split out so opt-level 0 does not reserve every other arm's
    /// locals on this recursive frame.
    #[inline(never)]
    fn lower_ts_any_keyword(
        &mut self,
        kind: AstKind<'x>,
    ) -> Result<TypeOutcome<'source>, TypeScriptCollectError> {
        if kind.as_ts_any_keyword().is_some() {
            return Ok(TypeOutcome::Cells(TypeCells::unknown(
                TypeReason::DynamicallyTyped,
            )));
        }
        Ok(TypeOutcome::Cells(TypeCells::unknown(
            TypeReason::NoIrRepresentation,
        )))
    }

    /// One syntax arm of [`Self::lower_type`].
    ///
    /// Split out so opt-level 0 does not reserve every other arm's
    /// locals on this recursive frame.
    #[inline(never)]
    fn lower_ts_unknown_keyword(
        &mut self,
        kind: AstKind<'x>,
    ) -> Result<TypeOutcome<'source>, TypeScriptCollectError> {
        if kind.as_ts_unknown_keyword().is_some() {
            return Ok(TypeOutcome::Cells(TypeCells::leaf(SemanticTypeTag::Any)));
        }
        Ok(TypeOutcome::Cells(TypeCells::unknown(
            TypeReason::NoIrRepresentation,
        )))
    }

    /// One syntax arm of [`Self::lower_type`].
    ///
    /// Split out so opt-level 0 does not reserve every other arm's
    /// locals on this recursive frame.
    #[inline(never)]
    fn lower_ts_never_keyword(
        &mut self,
        kind: AstKind<'x>,
    ) -> Result<TypeOutcome<'source>, TypeScriptCollectError> {
        if kind.as_ts_never_keyword().is_some() {
            return Ok(TypeOutcome::Cells(TypeCells::leaf(SemanticTypeTag::Never)));
        }
        Ok(TypeOutcome::Cells(TypeCells::unknown(
            TypeReason::NoIrRepresentation,
        )))
    }

    /// Recognizes the closed literal spellings on the source plane. The
    /// checker reports their bases, while the source owns the exact value.
    fn literal_cells(&self, span: Span) -> Option<TypeCells<'source>> {
        let text = self.slice_span(span)?;
        let text = trim_bytes(text, b" \t\r\n");
        let base = if (text.starts_with(b"\"") && text.ends_with(b"\""))
            || (text.starts_with(b"'") && text.ends_with(b"'"))
        {
            Some(backend_frontend_typescript::legacy::LiteralBase::String)
        } else if text == b"true" || text == b"false" {
            Some(backend_frontend_typescript::legacy::LiteralBase::Boolean)
        } else if text.ends_with(b"n")
            && text[..text.len().saturating_sub(1)]
                .iter()
                .all(u8::is_ascii_digit)
        {
            Some(backend_frontend_typescript::legacy::LiteralBase::Bigint)
        } else if !text.is_empty()
            && text
                .iter()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'.' | b'-' | b'+'))
        {
            Some(backend_frontend_typescript::legacy::LiteralBase::Number)
        } else {
            None
        }?;
        let mut cells = TypeCells::leaf(SemanticTypeTag::Primitive);
        cells.record.payload0 = u32::from(PrimitiveShape::Builtin);
        cells.record.text = Some(text);
        // The projected literal lane keys on the canonical base code, not the
        // frontend's declaration order: string `0`, number `1`, bigint `2`,
        // boolean `3`. Reusing `LiteralBase as u8` (number-first) silently
        // widened every syntax-path literal to an unresolved unknown.
        cells.record.payload1 = match base {
            backend_frontend_typescript::legacy::LiteralBase::String => 0,
            backend_frontend_typescript::legacy::LiteralBase::Number => 1,
            backend_frontend_typescript::legacy::LiteralBase::Bigint => 2,
            backend_frontend_typescript::legacy::LiteralBase::Boolean => 3,
        };
        Some(cells)
    }

    /// Folds an arbitrarily wide union or intersection into
    /// [`MAX_TYPE_CHILDREN`]-wide rows. Every source member stays reachable in
    /// source order, while nesting depth stays logarithmic in the member
    /// count. The previous left-leaning pair fold grew depth linearly, so a
    /// multi-thousand-member union produced a multi-thousand-deep type graph
    /// that tripped every depth-bounded consumer.
    fn associative_cells(
        &mut self,
        spans: &[Span],
        depth: u8,
        tag: SemanticTypeTag,
    ) -> Result<TypeOutcome<'source>, TypeScriptCollectError> {
        let mut rows: Vec<(u32, Span)> = Vec::with_capacity(spans.len());
        for span in spans {
            let target = self.child_target(span.start, span.end, depth)?;
            rows.push((target, *span));
        }
        while rows.len() > MAX_TYPE_CHILDREN {
            let mut next: Vec<(u32, Span)> =
                Vec::with_capacity(rows.len().div_ceil(MAX_TYPE_CHILDREN));
            for chunk in rows.chunks(MAX_TYPE_CHILDREN) {
                let mut cells = TypeCells::leaf(tag);
                for (target, _) in chunk {
                    cells.push_child(*target, None, 0)?;
                }
                // Each folded row is named by the last member of its chunk,
                // the rightmost-member naming the pair fold used.
                let name = chunk
                    .last()
                    .map(|(_, span)| *span)
                    .ok_or_else(lane_rejection)?;
                next.push((self.synthetic_cells_fact(name, cells)?, name));
            }
            rows = next;
        }
        let mut root = TypeCells::leaf(tag);
        for (target, _) in &rows {
            root.push_child(*target, None, 0)?;
        }
        Ok(TypeOutcome::Cells(root))
    }

    /// Pushes one member fact of a type-position object literal and returns
    /// its fact ordinal, member name bytes, and member flags. Anonymous call
    /// and construct signatures embody as Function facts named by their full
    /// source spelling, so the anonymous-record child keeps its required
    /// name. The member ordinal is staged so the literal's
    /// embodiment claims it: members of one anonymous object must share the
    /// parentage of that object, not the nearest declared owner, or two
    /// same-named members of sibling objects collide as twins.
    fn push_type_literal_member(
        &mut self,
        member_span: Span,
        depth: u8,
    ) -> Result<Option<MemberLink<'source>>, TypeScriptCollectError> {
        let member_base = self.staged_members.len();
        let node_position = self
            .node_index
            .binary_search_by_key(&(member_span.start, member_span.end), |(known, _)| {
                (known.start, known.end)
            })
            .ok();
        let Some(node_position) = node_position else {
            return Ok(None);
        };
        let Some((_, node_id)) = self.node_index.get(node_position) else {
            return Ok(None);
        };
        let probed = self.semantic.nodes().get_node(*node_id);
        {
            let member_kind = probed.kind();
            if member_kind.as_ts_property_signature().is_some() {
                return self.push_ts_property_signature(
                    member_kind,
                    member_span,
                    member_base,
                    depth,
                );
            }
            if member_kind.as_ts_index_signature().is_some() {
                return self.push_ts_index_signature(member_kind, member_span, member_base, depth);
            }
            if member_kind.as_ts_method_signature().is_some() {
                return self.push_ts_method_signature(member_kind, member_span, member_base);
            }
            if member_kind.as_ts_call_signature_declaration().is_some() {
                return self.push_ts_call_signature_declaration(
                    member_kind,
                    member_span,
                    member_base,
                );
            }
            if member_kind
                .as_ts_construct_signature_declaration()
                .is_some()
            {
                return self.push_ts_construct_signature_declaration(
                    member_kind,
                    member_span,
                    member_base,
                );
            }
        }
        Ok(None)
    }

    /// One member arm of [`Self::push_type_literal_member`].
    ///
    /// Split out so a nested object literal does not keep every
    /// other member arm's locals on the 2 MiB owner stack.
    #[inline(never)]
    fn push_ts_property_signature(
        &mut self,
        member_kind: AstKind<'x>,
        member_span: Span,
        member_base: usize,
        depth: u8,
    ) -> Result<Option<MemberLink<'source>>, TypeScriptCollectError> {
        if let Some(property) = member_kind.as_ts_property_signature() {
            let key_span = property.key.span();
            let name = self
                .slice_span(key_span)
                .ok_or(TypeScriptCollectError::Span {
                    start: key_span.start,
                    end: key_span.end,
                })?;
            let mut flags = 0_u8;
            if property.optional {
                flags |= SemanticTypeChild::FLAG_OPTIONAL;
            }
            if property.readonly {
                flags |= SemanticTypeChild::FLAG_READONLY;
            }
            let type_parameter_start = coordinate(self.facts.type_parameter_len)?;
            let cells = match property.type_annotation.as_ref() {
                Some(annotation) => {
                    let inner = annotation.type_annotation.span();
                    self.owner_cells(inner.start, inner.end, depth)?
                }
                None => TypeCells::unknown(TypeReason::Unannotated),
            };
            let extension = self.extension(type_parameter_start)?;
            let fact = with_cells(
                SemanticFact::new(EntityKind::Field, name, LEAF_PRODUCT).with_extension(extension),
                cells,
            );
            if self.staged_members.len() == member_base
                && let Some(existing) = self.merged_fact(member_span, &fact)
            {
                return Ok(Some((existing, name, flags)));
            }
            let ordinal = self.push(fact)?;
            self.register(ordinal, member_span, key_span, EntityKind::Field)?;
            self.staged_members.push(ordinal);
            self.claim_staged_members(member_base, ordinal);
            return Ok(Some((ordinal, name, flags)));
        }
        Ok(None)
    }

    /// One member arm of [`Self::push_type_literal_member`].
    ///
    /// Split out so a nested object literal does not keep every
    /// other member arm's locals on the 2 MiB owner stack.
    #[inline(never)]
    fn push_ts_index_signature(
        &mut self,
        member_kind: AstKind<'x>,
        member_span: Span,
        member_base: usize,
        depth: u8,
    ) -> Result<Option<MemberLink<'source>>, TypeScriptCollectError> {
        if let Some(signature) = member_kind.as_ts_index_signature() {
            let inner = signature.type_annotation.type_annotation.span();
            let name_span = Span::new(member_span.start, inner.start);
            let name = self
                .slice_span(name_span)
                .ok_or(TypeScriptCollectError::Span {
                    start: name_span.start,
                    end: name_span.end,
                })?;
            let type_parameter_start = coordinate(self.facts.type_parameter_len)?;
            let cells = self.owner_cells(inner.start, inner.end, depth)?;
            let extension = self.extension(type_parameter_start)?;
            let fact = with_cells(
                SemanticFact::new(EntityKind::Field, name, LEAF_PRODUCT).with_extension(extension),
                cells,
            );
            let ordinal = self.push(fact)?;
            self.register(ordinal, member_span, name_span, EntityKind::Field)?;
            self.staged_members.push(ordinal);
            self.claim_staged_members(member_base, ordinal);
            let flags = if signature.readonly {
                SemanticTypeChild::FLAG_READONLY
            } else {
                0
            };
            return Ok(Some((ordinal, name, flags)));
        }
        Ok(None)
    }

    /// One member arm of [`Self::push_type_literal_member`].
    ///
    /// Split out so a nested object literal does not keep every
    /// other member arm's locals on the 2 MiB owner stack.
    #[inline(never)]
    fn push_ts_method_signature(
        &mut self,
        member_kind: AstKind<'x>,
        member_span: Span,
        member_base: usize,
    ) -> Result<Option<MemberLink<'source>>, TypeScriptCollectError> {
        if let Some(method) = member_kind.as_ts_method_signature() {
            let key_span = method.key.span();
            let name = self
                .slice_span(key_span)
                .ok_or(TypeScriptCollectError::Span {
                    start: key_span.start,
                    end: key_span.end,
                })?;
            let mut params = ParamRows::new();
            for parameter in method.params.items.iter() {
                params.push(ParamRow {
                    name: parameter.pattern.span(),
                    annotation: parameter
                        .type_annotation
                        .as_ref()
                        .map(|annotation| annotation.type_annotation.span()),
                    default: parameter.initializer.as_ref().map(|init| init.span()),
                    flags: if parameter.optional {
                        SemanticTypeChild::FLAG_OPTIONAL
                    } else {
                        0
                    },
                })?;
            }
            let result = method
                .return_type
                .as_ref()
                .map(|returned| returned.type_annotation.span());
            let ordinal = self.push_signature(
                key_span,
                member_span,
                &TypeParamRows::new(),
                &params,
                result,
                false,
            )?;
            let flags = if method.optional {
                SemanticTypeChild::FLAG_OPTIONAL
            } else {
                0
            };
            self.staged_members.push(ordinal);
            self.claim_staged_members(member_base, ordinal);
            return Ok(Some((ordinal, name, flags)));
        }
        Ok(None)
    }

    /// One member arm of [`Self::push_type_literal_member`].
    ///
    /// Split out so a nested object literal does not keep every
    /// other member arm's locals on the 2 MiB owner stack.
    #[inline(never)]
    fn push_ts_call_signature_declaration(
        &mut self,
        member_kind: AstKind<'x>,
        member_span: Span,
        member_base: usize,
    ) -> Result<Option<MemberLink<'source>>, TypeScriptCollectError> {
        if let Some(signature) = member_kind.as_ts_call_signature_declaration() {
            // An anonymous call member embodies exactly as an interface
            // call signature does, named by its full source spelling so
            // the anonymous-record child keeps its required name. The
            // staged ordinal lets the enclosing literal claim it.
            let mut rows = TypeParamRows::new();
            if let Some(declared) = signature.type_parameters.as_ref() {
                for parameter in declared.params.iter() {
                    Self::stage_row(
                        &mut rows,
                        TypeParamRow {
                            name: parameter.name.span,
                            constraint: parameter.constraint.as_ref().map(|t| t.span()),
                            default: parameter.default.as_ref().map(|t| t.span()),
                        },
                    )?;
                }
            }
            let mut params = ParamRows::new();
            for parameter in signature.params.items.iter() {
                params.push(ParamRow {
                    name: parameter.pattern.span(),
                    annotation: parameter
                        .type_annotation
                        .as_ref()
                        .map(|annotation| annotation.type_annotation.span()),
                    default: parameter.initializer.as_ref().map(|init| init.span()),
                    flags: if parameter.optional {
                        SemanticTypeChild::FLAG_OPTIONAL
                    } else {
                        0
                    },
                })?;
            }
            if let Some(rest) = signature.params.rest.as_ref() {
                params.push(ParamRow {
                    name: rest.rest.argument.span(),
                    annotation: rest
                        .type_annotation
                        .as_ref()
                        .map(|annotation| annotation.type_annotation.span()),
                    default: None,
                    flags: SemanticTypeChild::FLAG_REST,
                })?;
            }
            let result = signature
                .return_type
                .as_ref()
                .map(|returned| returned.type_annotation.span());
            let ordinal =
                self.push_signature(member_span, member_span, &rows, &params, result, false)?;
            let name = self
                .slice_span(member_span)
                .ok_or(TypeScriptCollectError::Span {
                    start: member_span.start,
                    end: member_span.end,
                })?;
            self.staged_members.push(ordinal);
            self.claim_staged_members(member_base, ordinal);
            return Ok(Some((ordinal, name, 0)));
        }
        Ok(None)
    }

    /// One member arm of [`Self::push_type_literal_member`].
    ///
    /// Split out so a nested object literal does not keep every
    /// other member arm's locals on the 2 MiB owner stack.
    #[inline(never)]
    fn push_ts_construct_signature_declaration(
        &mut self,
        member_kind: AstKind<'x>,
        member_span: Span,
        member_base: usize,
    ) -> Result<Option<MemberLink<'source>>, TypeScriptCollectError> {
        if let Some(signature) = member_kind.as_ts_construct_signature_declaration() {
            // An anonymous construct member embodies exactly as a call
            // member does; only the signature node differs.
            let mut rows = TypeParamRows::new();
            if let Some(declared) = signature.type_parameters.as_ref() {
                for parameter in declared.params.iter() {
                    Self::stage_row(
                        &mut rows,
                        TypeParamRow {
                            name: parameter.name.span,
                            constraint: parameter.constraint.as_ref().map(|t| t.span()),
                            default: parameter.default.as_ref().map(|t| t.span()),
                        },
                    )?;
                }
            }
            let mut params = ParamRows::new();
            for parameter in signature.params.items.iter() {
                params.push(ParamRow {
                    name: parameter.pattern.span(),
                    annotation: parameter
                        .type_annotation
                        .as_ref()
                        .map(|annotation| annotation.type_annotation.span()),
                    default: parameter.initializer.as_ref().map(|init| init.span()),
                    flags: if parameter.optional {
                        SemanticTypeChild::FLAG_OPTIONAL
                    } else {
                        0
                    },
                })?;
            }
            if let Some(rest) = signature.params.rest.as_ref() {
                params.push(ParamRow {
                    name: rest.rest.argument.span(),
                    annotation: rest
                        .type_annotation
                        .as_ref()
                        .map(|annotation| annotation.type_annotation.span()),
                    default: None,
                    flags: SemanticTypeChild::FLAG_REST,
                })?;
            }
            let result = signature
                .return_type
                .as_ref()
                .map(|returned| returned.type_annotation.span());
            let ordinal =
                self.push_signature(member_span, member_span, &rows, &params, result, false)?;
            let name = self
                .slice_span(member_span)
                .ok_or(TypeScriptCollectError::Span {
                    start: member_span.start,
                    end: member_span.end,
                })?;
            self.staged_members.push(ordinal);
            self.claim_staged_members(member_base, ordinal);
            return Ok(Some((ordinal, name, 0)));
        }
        Ok(None)
    }
}

/// Streams the OXC projection with one caller-supplied checker report.
///
/// Passing `None` lowers at pure OXC fidelity: the declared syntax plane
/// only. Passing a validated report fills the extension computed cells,
/// applies exact overload signatures to resolved call sites, and upgrades
/// checker-resolved same-file references to oracle confidence.
pub(crate) fn collect_with_checker<'source, 'report>(
    profile: TypeScriptSource,
    source: &'source [u8],
    checker: Option<&'report backend_frontend_typescript::legacy::Report>,
    facts: &mut FactSet<'source>,
) -> Result<(), TypeScriptCollectError> {
    let source = std::str::from_utf8(source).map_err(TypeScriptCollectError::Utf8)?;
    let declaration_file = checker.is_some_and(|report| report.declaration_file);
    let index = match checker {
        Some(report) => Some(CheckerIndex::bind(report, source).map_err(|cause| {
            TypeScriptCollectError::Authority(AuthorityError::Checker { cause })
        })?),
        None => None,
    };
    let build = |module: OxcModule<'_>| {
        let mut projector = Projector {
            semantic: &module.semantic,
            node_index: {
                let mut index: Vec<_> = module
                    .semantic
                    .nodes()
                    .iter_enumerated()
                    .map(|(node_id, node)| (node.kind().span(), node_id))
                    .collect();
                index.sort_unstable_by_key(|(span, _)| (span.start, span.end));
                index
            },
            source,
            facts,
            name_starts: Vec::new(),
            name_ends: Vec::new(),
            decl_starts: Vec::new(),
            decl_ends: Vec::new(),
            merged_declaration_parts: HashMap::new(),
            fact_kinds: Vec::new(),
            import_modules: Vec::new(),
            import_module_len: 0,
            checker: index,
            tsz_project: None,
            tsz_bound_file: None,
            tsz_binder: None,
            tsz_file_index: None,
            pending_type_parameters: 0,
            extension_type_parameters: Vec::new(),
            staged_members: Vec::new(),
            member_parents: Vec::new(),
            native_member_parents: Vec::new(),
            native_class_owners: Vec::new(),
            parameter_properties: Vec::new(),
            setters: Vec::new(),
            synthetic_starts: Vec::new(),
            synthetic_ends: Vec::new(),
            facts_by_name: HashMap::new(),
            fact_at_name: HashMap::new(),
            binding_init_spans: HashMap::new(),
            synthetic_by_name: HashMap::new(),
            owner_index: Vec::new(),
            owner_ancestor: Vec::new(),
        };
        projector.run()
    };
    // An ambient declaration file is parsed under the TypeScript definition
    // grammar so OXC's implementation-presence checks do not fire on members
    // that legitimately have no body. The checker report owns that
    // classification; a report that never classified the source keeps the
    // ordinary value grammar.
    if declaration_file {
        with_analysis_declaration(profile, source, true, build)
    } else {
        with_analysis(profile, source, build)
    }
    .map_err(TypeScriptCollectError::Authority)?
}

/// Returns the one TSZ binder symbol attached to an exact identifier source
/// span. The parser node and byte range are checked together; matching names
/// elsewhere in the file cannot lend an identity.
fn native_tsz_symbol_at_identifier_span(
    bound_file: &TszBoundFile,
    span: Span,
) -> Option<TszSymbolId> {
    let mut found = None;
    for (&raw_node, &symbol) in bound_file.node_symbols.iter() {
        let node_index = TszNodeIndex(raw_node);
        if bound_file.arena.pos_end_at(node_index) != Some((span.start, span.end)) {
            continue;
        }
        let node = bound_file.arena.get(node_index)?;
        if bound_file.arena.get_identifier(node).is_none() {
            continue;
        }
        if found.is_some_and(|existing| existing != symbol) {
            return None;
        }
        found = Some(symbol);
    }
    found
}

/// Returns the symbol attached to one exact class or interface declaration.
/// The TSZ binder records declaration identity on the declaration node, while
/// `node_symbols` need not also contain its identifier child. Matching the
/// full source span prevents an unrelated same-named or nested declaration
/// from supplying merge evidence.
fn native_tsz_symbol_at_declaration_span(
    bound_file: &TszBoundFile,
    span: Span,
) -> Option<TszSymbolId> {
    let mut found = None;
    for (&raw_node, &symbol) in bound_file.node_symbols.iter() {
        let node_index = TszNodeIndex(raw_node);
        if bound_file.arena.pos_end_at(node_index) != Some((span.start, span.end)) {
            continue;
        }
        let node = bound_file.arena.get(node_index)?;
        if !matches!(
            node.kind,
            TszSyntaxKind::CLASS_DECLARATION | TszSyntaxKind::INTERFACE_DECLARATION
        ) {
            continue;
        }
        if found.is_some_and(|existing| existing != symbol) {
            return None;
        }
        found = Some(symbol);
    }
    found
}

/// Matches an OXC TypeQuery node to the exact TSZ TypeQuery AST node and
/// returns the symbol bound to its simple identifier expression. Qualified
/// queries are intentionally left unresolved until the IR has an exact path
/// target form for source-owned declarations.
fn native_tsz_type_query_symbol(
    bound_file: &TszBoundFile,
    binder: &TszBinderState,
    query_span: Span,
    name_span: Span,
) -> Option<TszSymbolId> {
    let mut found = None;
    for (raw_node, node) in bound_file.arena.nodes.iter().enumerate() {
        if node.kind != TszSyntaxKind::TYPE_QUERY {
            continue;
        }
        let node_index = TszNodeIndex(u32::try_from(raw_node).ok()?);
        let candidate_query_span = bound_file.arena.pos_end_at(node_index);
        let node = bound_file.arena.get(node_index)?;
        let Some(query) = bound_file.arena.get_type_query(node) else {
            #[cfg(test)]
            eprintln!(
                "TSZ_TYPEQUERY_NODE_TRACE missing_type_data node={node_index:?} span={candidate_query_span:?}"
            );
            continue;
        };
        let candidate_name_span = bound_file.arena.pos_end_at(query.expr_name);
        let name = bound_file.arena.get(query.expr_name)?;
        let identifier = bound_file.arena.get_identifier(name).is_some();
        let symbol = binder.resolve_identifier(&bound_file.arena, query.expr_name);
        #[cfg(test)]
        eprintln!(
            "TSZ_TYPEQUERY_NODE_TRACE node={node_index:?} span={candidate_query_span:?} expr={candidate_name_span:?} identifier={identifier} node_symbol={:?} binder_symbol={symbol:?} expected_query={query_span:?} expected_name={name_span:?}",
            bound_file.node_symbols.get(&query.expr_name.0),
        );
        if candidate_query_span != Some((query_span.start, query_span.end))
            || candidate_name_span != Some((name_span.start, name_span.end))
        {
            continue;
        }
        if !identifier {
            return None;
        }
        let symbol = symbol?;
        if found.is_some_and(|existing| existing != symbol) {
            return None;
        }
        found = Some(symbol);
    }
    found
}

#[derive(Clone, Debug)]
struct NativeTszDeclarationCoordinate {
    file_index: u32,
    path: String,
    source_identity: [u8; 32],
    declaration_start: u32,
    declaration_end: u32,
    name_start: u32,
    kind: EntityKind,
}

struct NativeAnonymousCallable<'source> {
    span: Span,
    step: CallableAnchorStep<'source>,
    scope: Option<[u8; 32]>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum TypeScriptLexicalOwner {
    Declaration(u32),
    UnrepresentedNativeClass {
        family: [u8; 16],
        instance: u32,
        multiplicity: u32,
    },
}

enum NativeClassRouteStep<'source> {
    Named { kind: u16, name: &'source [u8] },
    Expression(CallableAnchorStep<'source>),
}

#[derive(Clone, Copy)]
enum NativeCallableScopeEvent {
    Named {
        start: u32,
        end: u32,
        ordinal: usize,
    },
    Anonymous {
        start: u32,
        end: u32,
        index: usize,
    },
}

/// Resolves a property name through an exact namespace import or the receiver's
/// checker type and member table. Namespace exports retain their terminal file
/// owner because raw symbol IDs are binder-relative. No project-wide name scan
/// participates, so same-spelled members on unrelated receivers stay apart.
fn native_tsz_member_symbol(
    checker: &mut TszCheckerState<'_>,
    binder: &backend_frontend_typescript::TszBinderState,
    project: &TszProject,
    arena: &TszNodeArena,
    receiver: TszNodeIndex,
    member_name: TszNodeIndex,
) -> Option<(TszSymbolId, Option<usize>)> {
    let member_name = arena.get_identifier_at(member_name)?.escaped_text.as_str();
    if let Some(receiver_symbol) = binder.resolve_identifier(arena, receiver)
        && let Some(receiver_symbol) = binder.symbols.get(receiver_symbol)
        && receiver_symbol.import_name() == Some("*")
    {
        // A namespace import's raw type symbol cannot be read from the
        // caller's binder. Even a refused namespace resolution must not fall
        // through to a same-numbered local symbol.
        let target = checker.resolve_project_namespace_member_with_owner(receiver, member_name)?;
        return Some((target.symbol_id, Some(target.owner_file_idx)));
    }
    let receiver_type = checker.get_type_of_node(receiver);
    let owner_symbol = checker.ctx.resolve_type_to_symbol_id(receiver_type)?;
    // A receiver's class symbol may belong to a different project file than
    // the call site (for example, a field initialized with Angular's
    // `inject(Service)`). Symbol ids are binder-relative, so looking the id
    // up in the caller's binder can miss the real member or select an
    // unrelated same-numbered symbol. Carry the checker's exact file owner
    // through both the member lookup and the later declaration-coordinate
    // join.
    let checker_file_idx = checker.ctx.resolve_symbol_file_index(owner_symbol);
    let program_file_idx = native_tsz_symbol_file_index(project, owner_symbol);
    // The live checker owner is tied to the resolved receiver type. The
    // program fallback is useful only when that owner is absent: symbol IDs
    // read from a checker binder can numerically collide with a different
    // project-table symbol, so a disagreement is not evidence against the
    // checker's exact file.
    let owner_file_idx = checker_file_idx.or(program_file_idx);
    let owner_binder = match owner_file_idx {
        Some(owner_file_idx) => checker.ctx.get_binder_for_file(owner_file_idx)?,
        // Same-file symbols do not always need a cross-file owner entry. Keep
        // the caller-binder route only when the symbol itself proves local
        // ownership; a same-numbered local symbol is not a fallback witness
        // for an unresolved cross-file receiver.
        None => {
            let local = binder.symbols.get(owner_symbol)?;
            if local.decl_file_idx != checker.ctx.current_file_idx as u32 {
                return None;
            }
            binder
        }
    };
    let owner_symbol = owner_binder
        .resolve_import_symbol(owner_symbol)
        .unwrap_or(owner_symbol);
    let owner = owner_binder.symbols.get(owner_symbol)?;
    if owner_file_idx.is_some_and(|file_idx| owner.decl_file_idx != file_idx as u32) {
        return None;
    }
    let member = owner
        .members
        .as_ref()
        .and_then(|members| members.get(member_name))
        .or_else(|| {
            owner
                .exports
                .as_ref()
                .and_then(|exports| exports.get(member_name))
        });
    member.map(|symbol| (symbol, owner_file_idx))
}

/// Finds a symbol's file only when its exact retained program declarations
/// all belong to one bound source file and that file independently maps every
/// declaration node back to this symbol. This supplies an immutable project
/// witness when the checker has not populated its dynamic symbol-owner map.
fn native_tsz_symbol_file_index(project: &TszProject, symbol: TszSymbolId) -> Option<usize> {
    let program = project.program();
    let symbol_data = program.symbols.get(symbol)?;
    if symbol_data.declarations.is_empty()
        || symbol_data.declarations.len() != symbol_data.stable_declarations.len()
    {
        return None;
    }
    let file_index = usize::try_from(symbol_data.stable_declarations.first()?.file_idx).ok()?;
    let file = program.files.get(file_index)?;
    for (declaration, stable) in symbol_data
        .declarations
        .iter()
        .zip(&symbol_data.stable_declarations)
    {
        if usize::try_from(stable.file_idx).ok()? != file_index
            || stable.pos >= stable.end
            || file.arena.pos_end_at(*declaration) != Some((stable.pos, stable.end))
            || file.node_symbols.get(&declaration.0) != Some(&symbol)
        {
            return None;
        }
    }
    Some(file_index)
}

/// An object method is a source-owned callable even though it has no
/// function-expression node. Reuse the existing object-member anchor so its
/// inline parameter types and nested declarations have that exact method as
/// owner. Class methods already have named signature facts and are excluded.
fn native_tsz_object_method_step<'source>(
    source: &'source str,
    arena: &TszNodeArena,
    method: TszNodeIndex,
) -> Option<CallableAnchorStep<'source>> {
    let declaration = arena.get_method_decl_at(method)?;
    let parent = arena.parent_of(method)?;
    let object = arena.get(parent)?;
    if object.kind != TszSyntaxKind::OBJECT_LITERAL_EXPRESSION
        || declaration.body.is_none()
        || arena.get_identifier_at(declaration.name).is_none()
        || !arena
            .get_literal_expr_at(parent)?
            .elements
            .nodes
            .contains(&method)
    {
        return None;
    }
    Some(CallableAnchorStep {
        child_role: CallableChildRole::ObjectMemberValue,
        parent: CallableParentShape::ObjectMember(source_node_bytes(
            source,
            arena,
            declaration.name,
        )?),
    })
}

fn native_tsz_expression_parent_step<'source>(
    source: &'source str,
    arena: &TszNodeArena,
    callable: TszNodeIndex,
) -> Option<CallableAnchorStep<'source>> {
    let mut child = callable;
    for _ in 0..8 {
        let parent = arena.parent_of(child)?;
        let node = arena.get(parent)?;
        if node.kind == TszSyntaxKind::PARENTHESIZED_EXPRESSION
            && arena
                .get_parenthesized_at(parent)
                .is_some_and(|wrapped| wrapped.expression == child)
        {
            child = parent;
            continue;
        }
        let step = if node.kind == TszSyntaxKind::CALL_EXPRESSION {
            let call = arena.get_call_expr_at(parent)?;
            call.arguments
                .as_ref()
                .is_some_and(|arguments| arguments.nodes.contains(&child))
                .then_some(CallableAnchorStep {
                    child_role: CallableChildRole::CallArgument,
                    parent: CallableParentShape::Call,
                })?
        } else if node.kind == TszSyntaxKind::VARIABLE_DECLARATION {
            let declaration = arena.get_variable_declaration_at(parent)?;
            if declaration.initializer != child
                || arena.get_identifier_at(declaration.name).is_none()
            {
                return None;
            }
            CallableAnchorStep {
                child_role: CallableChildRole::VariableInitializer,
                parent: CallableParentShape::VariableBinding(source_node_bytes(
                    source,
                    arena,
                    declaration.name,
                )?),
            }
        } else if node.kind == TszSyntaxKind::CONDITIONAL_EXPRESSION {
            let conditional = arena.get_conditional_expr_at(parent)?;
            let child_role = if conditional.when_true == child {
                CallableChildRole::ConditionalConsequent
            } else if conditional.when_false == child {
                CallableChildRole::ConditionalAlternate
            } else {
                return None;
            };
            CallableAnchorStep {
                child_role,
                parent: CallableParentShape::Conditional,
            }
        } else if node.kind == TszSyntaxKind::ARRAY_LITERAL_EXPRESSION {
            let array = arena.get_literal_expr_at(parent)?;
            if !array.elements.nodes.contains(&child) {
                return None;
            }
            CallableAnchorStep {
                child_role: CallableChildRole::ArrayElement,
                parent: CallableParentShape::ArrayLiteral,
            }
        } else if node.kind == TszSyntaxKind::PROPERTY_ASSIGNMENT {
            let property = arena.get_property_assignment_at(parent)?;
            let object_literal = arena
                .parent_of(parent)
                .and_then(|object| arena.get(object))
                .is_some_and(|object| object.kind == TszSyntaxKind::OBJECT_LITERAL_EXPRESSION);
            if property.initializer != child
                || !object_literal
                || arena.get_identifier_at(property.name).is_none()
            {
                return None;
            }
            CallableAnchorStep {
                child_role: CallableChildRole::ObjectMemberValue,
                parent: CallableParentShape::ObjectMember(source_node_bytes(
                    source,
                    arena,
                    property.name,
                )?),
            }
        } else {
            return None;
        };
        return Some(step);
    }
    None
}

/// Native lexical roles and named enclosing declarations form the class
/// family route. Exact source coordinates are deliberately not route steps.
fn native_tsz_anonymous_class_route(
    source: &str,
    file: &TszBoundFile,
    declaration: TszNodeIndex,
) -> Option<[u8; 16]> {
    let arena = &file.arena;
    let mut steps = Vec::new();
    let mut current = declaration;
    let mut reached_source_file = false;
    for _ in 0..arena.nodes.len() {
        let node = arena.get(current)?;
        let name = arena
            .get_class(node)
            .map(|class| class.name)
            .or_else(|| arena.get_function(node).map(|function| function.name))
            .or_else(|| arena.get_method_decl(node).map(|method| method.name))
            .or_else(|| arena.get_accessor(node).map(|accessor| accessor.name))
            .or_else(|| arena.get_property_decl(node).map(|property| property.name))
            .or_else(|| arena.get_module(node).map(|module| module.name));
        if node.kind == TszSyntaxKind::CONSTRUCTOR {
            steps.push(NativeClassRouteStep::Named {
                kind: node.kind,
                name: &[],
            });
        }
        if let Some(name) = name.filter(|name| !name.is_none()) {
            steps.push(NativeClassRouteStep::Named {
                kind: node.kind,
                name: source_node_bytes(source, arena, name)?,
            });
        }
        if let Some(step) = native_tsz_expression_parent_step(source, arena, current) {
            steps.push(NativeClassRouteStep::Expression(step));
        }
        let parent = arena.parent_of(current)?;
        if current == file.source_file {
            // The native root has a present parent cell containing NONE;
            // a missing cell is not proof of a source-file root.
            if node.kind != TszSyntaxKind::SOURCE_FILE || !parent.is_none() {
                return None;
            }
            reached_source_file = true;
            break;
        }
        if parent.is_none() {
            return None;
        }
        current = parent;
    }
    if !reached_source_file {
        return None;
    }
    let mut hash = Sha256::new();
    hash.update(b"compiler.typescript.native-anonymous-class-family.v1\0");
    hash.update((file.file_name.len() as u64).to_le_bytes());
    hash.update(file.file_name.as_bytes());
    for step in steps.into_iter().rev() {
        match step {
            NativeClassRouteStep::Named { kind, name } => {
                hash.update([0]);
                hash.update(kind.to_le_bytes());
                hash.update((name.len() as u64).to_le_bytes());
                hash.update(name);
            }
            NativeClassRouteStep::Expression(step) => {
                hash.update([1]);
                hash_callable_anchor_step(&mut hash, step);
            }
        }
    }
    let mut family = [0_u8; 16];
    family.copy_from_slice(&hash.finalize()[..16]);
    Some(family)
}

/// Gives a source function type its owner-local signature role. The owning
/// parameter name is exact syntax evidence; overload positions and byte spans
/// are not used as durable family identity.
fn native_tsz_function_type_parent_step<'source>(
    source: &'source str,
    arena: &TszNodeArena,
    function_type: TszNodeIndex,
) -> Option<CallableAnchorStep<'source>> {
    let mut child = function_type;
    for _ in 0..8 {
        let parent = arena.parent_of(child)?;
        let node = arena.get(parent)?;
        if matches!(
            node.kind,
            TszSyntaxKind::PARENTHESIZED_TYPE
                | TszSyntaxKind::OPTIONAL_TYPE
                | TszSyntaxKind::REST_TYPE
        ) && arena
            .get_wrapped_type_at(parent)
            .is_some_and(|wrapped| wrapped.type_node == child)
        {
            child = parent;
            continue;
        }
        if node.kind == TszSyntaxKind::PARAMETER {
            let parameter = arena.get_parameter_at(parent)?;
            if parameter.type_annotation == child
                && arena.get_identifier_at(parameter.name).is_some()
            {
                return Some(CallableAnchorStep {
                    child_role: CallableChildRole::SignatureParameterType,
                    parent: CallableParentShape::SignatureParameter(source_node_bytes(
                        source,
                        arena,
                        parameter.name,
                    )?),
                });
            }
            return None;
        }
        if node.kind == TszSyntaxKind::TYPE_ALIAS_DECLARATION {
            let alias = arena.get_type_alias(node)?;
            if alias.type_node != child || arena.get_identifier_at(alias.name).is_none() {
                return None;
            }
            return Some(CallableAnchorStep {
                child_role: CallableChildRole::TypeAliasValue,
                parent: CallableParentShape::TypeAliasName(source_node_bytes(
                    source, arena, alias.name,
                )?),
            });
        }
        return native_tsz_type_container_for_child(arena, parent, child).map(|kind| {
            CallableAnchorStep {
                child_role: CallableChildRole::TypeExpression,
                parent: CallableParentShape::TypeContainer(kind),
            }
        });
    }
    None
}

fn native_tsz_type_container_for_child(
    arena: &TszNodeArena,
    parent: TszNodeIndex,
    child: TszNodeIndex,
) -> Option<CallableTypeContainerKind> {
    let node = arena.get(parent)?;
    let kind = match node.kind {
        TszSyntaxKind::ARRAY_TYPE
            if arena
                .get_array_type_at(parent)
                .is_some_and(|array| array.element_type == child) =>
        {
            CallableTypeContainerKind::Array
        }
        TszSyntaxKind::TUPLE_TYPE
            if arena
                .get_tuple_type_at(parent)
                .is_some_and(|tuple| tuple.elements.nodes.contains(&child)) =>
        {
            CallableTypeContainerKind::Tuple
        }
        TszSyntaxKind::UNION_TYPE | TszSyntaxKind::INTERSECTION_TYPE
            if arena
                .get_composite_type_at(parent)
                .is_some_and(|composite| composite.types.nodes.contains(&child)) =>
        {
            if node.kind == TszSyntaxKind::UNION_TYPE {
                CallableTypeContainerKind::Union
            } else {
                CallableTypeContainerKind::Intersection
            }
        }
        TszSyntaxKind::FUNCTION_TYPE | TszSyntaxKind::CONSTRUCTOR_TYPE
            if arena
                .get_function_type_at(parent)
                .is_some_and(|signature| signature.type_annotation == child) =>
        {
            if node.kind == TszSyntaxKind::FUNCTION_TYPE {
                CallableTypeContainerKind::Function
            } else {
                CallableTypeContainerKind::Constructor
            }
        }
        TszSyntaxKind::TYPE_REFERENCE
            if arena
                .get_type_ref_at(parent)
                .and_then(|type_ref| type_ref.type_arguments.as_ref())
                .is_some_and(|arguments| arguments.nodes.contains(&child)) =>
        {
            CallableTypeContainerKind::TypeReference
        }
        _ => return None,
    };
    Some(kind)
}

fn native_tsz_call_signature_parent_step(
    arena: &TszNodeArena,
    signature: TszNodeIndex,
) -> Option<CallableAnchorStep<'static>> {
    let signature_kind = arena.get(signature)?.kind;
    let child_role = match signature_kind {
        TszSyntaxKind::CALL_SIGNATURE => CallableChildRole::CallSignatureMember,
        TszSyntaxKind::CONSTRUCT_SIGNATURE => CallableChildRole::ConstructSignatureMember,
        _ => return None,
    };
    let parent = arena.parent_of(signature)?;
    let parent_node = arena.get(parent)?;
    let kind = if parent_node.kind == TszSyntaxKind::TYPE_LITERAL
        && arena
            .get_type_literal_at(parent)
            .is_some_and(|literal| literal.members.nodes.contains(&signature))
    {
        CallableTypeContainerKind::TypeLiteral
    } else if parent_node.kind == TszSyntaxKind::INTERFACE_DECLARATION
        && arena
            .get_interface_at(parent)
            .is_some_and(|interface| interface.members.nodes.contains(&signature))
    {
        CallableTypeContainerKind::Interface
    } else {
        return None;
    };
    Some(CallableAnchorStep {
        child_role,
        parent: CallableParentShape::TypeContainer(kind),
    })
}

fn source_node_bytes<'source>(
    source: &'source str,
    arena: &TszNodeArena,
    node: TszNodeIndex,
) -> Option<&'source [u8]> {
    let (start, end) = arena.pos_end_at(node)?;
    source
        .as_bytes()
        .get(usize::try_from(start).ok()?..usize::try_from(end).ok()?)
}

fn hash_callable_anchor_step(hash: &mut Sha256, step: CallableAnchorStep<'_>) {
    let role = match step.child_role {
        CallableChildRole::CallArgument => 0,
        CallableChildRole::VariableInitializer => 1,
        CallableChildRole::PropertyValue => 2,
        CallableChildRole::ConditionalConsequent => 3,
        CallableChildRole::ConditionalAlternate => 4,
        CallableChildRole::ArrayElement => 5,
        CallableChildRole::ObjectMemberValue => 6,
        CallableChildRole::SignatureParameterType => 7,
        CallableChildRole::TypeExpression => 8,
        CallableChildRole::TypeAliasValue => 9,
        CallableChildRole::CallSignatureMember => 10,
        CallableChildRole::ConstructSignatureMember => 11,
    };
    hash.update([role]);
    match step.parent {
        CallableParentShape::Call => hash.update([0]),
        CallableParentShape::VariableBinding(name) => {
            hash.update([1]);
            hash.update(u32::try_from(name.len()).unwrap_or(u32::MAX).to_le_bytes());
            hash.update(name);
        }
        CallableParentShape::PropertyName(name) => {
            hash.update([2]);
            hash.update(u32::try_from(name.len()).unwrap_or(u32::MAX).to_le_bytes());
            hash.update(name);
        }
        CallableParentShape::Conditional => hash.update([3]),
        CallableParentShape::ArrayLiteral => hash.update([4]),
        CallableParentShape::ObjectMember(name) => {
            hash.update([5]);
            hash.update(u32::try_from(name.len()).unwrap_or(u32::MAX).to_le_bytes());
            hash.update(name);
        }
        CallableParentShape::SignatureParameter(name) => {
            hash.update([6]);
            hash.update(u32::try_from(name.len()).unwrap_or(u32::MAX).to_le_bytes());
            hash.update(name);
        }
        CallableParentShape::TypeContainer(kind) => hash.update([7, kind as u8]),
        CallableParentShape::TypeAliasName(name) => {
            hash.update([8]);
            hash.update(u32::try_from(name.len()).unwrap_or(u32::MAX).to_le_bytes());
            hash.update(name);
        }
    }
}

/// Uses TSZ's exact bound symbol and stable declaration file/span to identify
/// one declaration that the compiler actually emits. Overload groups prefer
/// their unique implementation; signature-only groups must contain exactly
/// one declaration. Unassigned, external, stale, or ambiguous declarations
/// produce no coordinate.
fn native_tsz_declaration_coordinate(
    binder: &backend_frontend_typescript::TszBinderState,
    symbol_id: TszSymbolId,
    session: &backend_frontend_typescript::TszProjectQuerySession<'_>,
    callable: bool,
) -> Option<NativeTszDeclarationCoordinate> {
    let program_files = &session.project().program().files;
    let symbol_id = binder.resolve_import_symbol(symbol_id).unwrap_or(symbol_id);
    let symbol = binder.symbols.get(symbol_id)?;
    if symbol.declarations.is_empty()
        || symbol.declarations.len() != symbol.stable_declarations.len()
    {
        return None;
    }
    let mut functions = Vec::<(NativeTszDeclarationCoordinate, bool)>::new();
    let mut fields = Vec::<NativeTszDeclarationCoordinate>::new();
    for (declaration, stable) in symbol
        .declarations
        .iter()
        .zip(symbol.stable_declarations.iter())
    {
        if stable.file_idx == u32::MAX || stable.pos >= stable.end {
            continue;
        }
        let file = program_files.get(usize::try_from(stable.file_idx).ok()?)?;
        let arena = &file.arena;
        if arena.pos_end_at(*declaration) != Some((stable.pos, stable.end)) {
            continue;
        }
        let Some(node) = arena.get(*declaration) else {
            continue;
        };
        let (name_node, kind, has_body) = if let Some(method) = arena.get_method_decl(node) {
            (method.name, EntityKind::Function, !method.body.is_none())
        } else if let Some(function) = arena.get_function(node) {
            (
                function.name,
                EntityKind::Function,
                !function.body.is_none(),
            )
        } else if let Some(property) = arena.get_property_decl(node) {
            (property.name, EntityKind::Field, false)
        } else if let Some(accessor) = arena.get_accessor(node) {
            (accessor.name, EntityKind::Field, !accessor.body.is_none())
        } else {
            continue;
        };
        if callable && kind != EntityKind::Function {
            continue;
        }
        let Some((name_start, name_end)) = arena.pos_end_at(name_node) else {
            continue;
        };
        if name_start < stable.pos || name_end > stable.end || name_start >= name_end {
            continue;
        }
        let Some(identifier) = arena.get_identifier_at(name_node) else {
            continue;
        };
        if identifier.escaped_text.is_empty() {
            continue;
        }
        let Some(source) = arena.get_source_file_at(file.source_file) else {
            continue;
        };
        if source
            .text
            .get(name_start as usize..name_end as usize)
            .is_none()
        {
            continue;
        }
        let identity = session
            .native_source_identity_for_bound_file(stable.file_idx as usize, file)?;
        let coordinate = NativeTszDeclarationCoordinate {
            file_index: stable.file_idx,
            path: file.file_name.clone(),
            source_identity: identity,
            declaration_start: stable.pos,
            declaration_end: stable.end,
            name_start,
            kind,
        };
        if kind == EntityKind::Function {
            functions.push((coordinate, has_body));
        } else {
            fields.push(coordinate);
        }
    }
    if !functions.is_empty() {
        let implementations = functions
            .iter()
            .filter(|(_, has_body)| *has_body)
            .map(|(coordinate, _)| coordinate.clone())
            .collect::<Vec<_>>();
        if implementations.len() == 1 {
            return implementations.into_iter().next();
        }
        if implementations.is_empty() && functions.len() == 1 {
            return functions
                .into_iter()
                .next()
                .map(|(coordinate, _)| coordinate);
        }
        return None;
    }
    if callable || fields.len() != 1 {
        return None;
    }
    fields.into_iter().next()
}

/// The binder records a destructured variable symbol at its exact identifier
/// leaf. Follow only native binding-name edges to its variable declaration;
/// property labels, default initializers and parameter patterns are not owners.
fn native_tsz_destructured_variable_kind(
    arena: &TszNodeArena,
    identifier: TszNodeIndex,
) -> Option<EntityKind> {
    arena.get_identifier_at(identifier)?;
    let mut current = identifier;
    let mut in_pattern = false;
    // Every admitted edge must advance in the finite native node graph. A
    // malformed parent cycle cannot manufacture declaration authority.
    for _ in 0..arena.nodes.len() {
        let parent = arena.get_extended(current)?.parent;
        let node = arena.get(parent)?;
        if let Some(binding) = arena.get_binding_element(node) {
            if binding.name != current {
                return None;
            }
        } else if let Some(pattern) = arena.get_binding_pattern(node) {
            if !pattern.elements.nodes.contains(&current) {
                return None;
            }
            in_pattern = true;
        } else if let Some(variable) = arena.get_variable_declaration(node) {
            if !in_pattern || variable.name != current {
                return None;
            }
            return Some(if arena.is_const_variable_declaration(parent) {
                EntityKind::Constant
            } else {
                EntityKind::Static
            });
        } else {
            return None;
        }
        current = parent;
    }
    None
}

/// Import aliases may denote a value or a type. The importing spelling never
/// chooses that role: only the terminal TSZ symbol's unique stable declaration
/// in this admitted program can supply it. Callable overload selection keeps
/// the existing implementation rule above; merged non-callable declarations
/// remain unresolved rather than borrowing one same-named declaration.
fn native_tsz_import_declaration_coordinate(
    binder: &TszBinderState,
    symbol: TszSymbolId,
    session: &backend_frontend_typescript::TszProjectQuerySession<'_>,
) -> Option<NativeTszDeclarationCoordinate> {
    let program_files = &session.project().program().files;
    let declaration_symbol = binder.symbols.get(symbol)?;
    if declaration_symbol.declarations.len() != declaration_symbol.stable_declarations.len() {
        return None;
    }
    // A bare import has not selected a callable/type/namespace role. Only
    // homogeneous callable overloads may use the implementation selector;
    // a function merged with a namespace must not swallow that second role.
    if declaration_symbol.declarations.len() > 1
        && !declaration_symbol.declarations.iter().zip(&declaration_symbol.stable_declarations)
            .all(|(declaration, stable)| {
                let Some(file) = program_files.get(stable.file_idx as usize) else { return false; };
                let arena = &file.arena;
                if stable.pos >= stable.end || arena.pos_end_at(*declaration) != Some((stable.pos, stable.end)) {
                    return false;
                }
                arena.get(*declaration).is_some_and(|node|
                    arena.get_function(node).is_some() || arena.get_method_decl(node).is_some())
            })
    {
        return None;
    }
    if let Some(target) = native_tsz_declaration_coordinate(binder, symbol, session, false) {
        return Some(target);
    }
    let symbol = binder.symbols.get(symbol)?;
    let ([declaration], [stable]) = (
        symbol.declarations.as_slice(),
        symbol.stable_declarations.as_slice(),
    ) else {
        return None;
    };
    let file = program_files.get(usize::try_from(stable.file_idx).ok()?)?;
    let arena = &file.arena;
    if stable.pos >= stable.end || arena.pos_end_at(*declaration) != Some((stable.pos, stable.end)) {
        return None;
    }
    let node = arena.get(*declaration)?;
    let (name, kind) = if let Some(class) = arena.get_class(node) {
        (class.name, EntityKind::Record)
    } else if let Some(interface) = arena.get_interface(node) {
        (interface.name, EntityKind::Trait)
    } else if let Some(alias) = arena.get_type_alias(node) {
        (alias.name, EntityKind::Alias)
    } else if let Some(enumeration) = arena.get_enum(node) {
        (enumeration.name, EntityKind::Enum)
    } else if let Some(module) = arena.get_module(node) {
        (module.name, EntityKind::Module)
    } else if let Some(variable) = arena.get_variable_declaration(node) {
        let kind = if arena.is_const_variable_declaration(*declaration) {
            EntityKind::Constant
        } else {
            EntityKind::Static
        };
        (variable.name, kind)
    } else if arena.get_identifier_at(*declaration).is_some() {
        (
            *declaration,
            native_tsz_destructured_variable_kind(arena, *declaration)?,
        )
    } else {
        return None;
    };
    // Aggregate patterns/export assignments do not prove a named target.
    // A destructured leaf above retains its exact identifier primary extent,
    // matching the already published authored leaf rather than the aggregate.
    let identifier = arena.get_identifier_at(name)?;
    let (name_start, name_end) = arena.pos_end_at(name)?;
    if identifier.escaped_text.is_empty()
        || name_start < stable.pos
        || name_start >= name_end
        || name_end > stable.end
    {
        return None;
    }
    let source = arena.get_source_file_at(file.source_file)?;
    source.text.get(name_start as usize..name_end as usize)?;
    Some(NativeTszDeclarationCoordinate {
        file_index: stable.file_idx,
        path: file.file_name.clone(),
        source_identity: session
            .native_source_identity_for_bound_file(stable.file_idx as usize, file)?,
        declaration_start: stable.pos,
        declaration_end: stable.end,
        name_start,
        kind,
    })
}

/// A namespace call to a function-valued variable names that authored
/// binding, not the anonymous function's separate declaration. Require the
/// terminal binder's one stable variable declaration and actual native
/// function initializer; a scalar, class, merged symbol or unresolved alias
/// cannot acquire callable authority merely by appearing in call syntax.
fn native_tsz_callable_binding_coordinate(
    binder: &TszBinderState,
    symbol: TszSymbolId,
    session: &backend_frontend_typescript::TszProjectQuerySession<'_>,
) -> Option<NativeTszDeclarationCoordinate> {
    let declaration = binder.symbols.get(symbol)?;
    let ([node], [stable]) = (
        declaration.declarations.as_slice(),
        declaration.stable_declarations.as_slice(),
    ) else {
        return None;
    };
    let file = session
        .project()
        .program()
        .files
        .get(usize::try_from(stable.file_idx).ok()?)?;
    let arena = &file.arena;
    let variable = arena.get_variable_declaration(arena.get(*node)?)?;
    let initializer = arena.get(variable.initializer)?;
    if !matches!(
        initializer.kind,
        TszSyntaxKind::ARROW_FUNCTION | TszSyntaxKind::FUNCTION_EXPRESSION
    ) {
        return None;
    }
    arena.get_function(initializer)?;
    native_tsz_import_declaration_coordinate(binder, symbol, session)
}

pub(crate) fn collect_with_tsz<'source, 'session>(
    profile: TypeScriptSource,
    source: &'source [u8],
    project: &'source TszProject,
    source_path: &str,
    session: &'session backend_frontend_typescript::TszProjectQuerySession<'source>,
    facts: &mut FactSet<'source>,
) -> Result<(), TypeScriptCollectError> {
    let source = std::str::from_utf8(source).map_err(TypeScriptCollectError::Utf8)?;
    if project.source_text(source_path) != Some(source) {
        return Err(TypeScriptCollectError::TszAuthority(
            TszAuthorityError::SourceMismatch {
                path: source_path.to_owned(),
            },
        ));
    }
    if !std::ptr::eq(session.project(), project) {
        return Err(TypeScriptCollectError::TszAuthority(
            TszAuthorityError::ProjectSessionMismatch,
        ));
    }
    let file_index = session
        .file_index(source_path)
        .map_err(TypeScriptCollectError::TszAuthority)?;
    let declaration_file = source_path.ends_with(".d.ts")
        || source_path.ends_with(".d.mts")
        || source_path.ends_with(".d.cts");
    let program_identity = session.native_program_identity();
    let source_identity = session
        .native_source_identity(file_index, source.as_bytes())
        .ok_or_else(|| {
            TypeScriptCollectError::TszAuthority(TszAuthorityError::SourceMismatch {
                path: source_path.to_owned(),
            })
        })?;
    facts.set_native_tsz_identity(program_identity, source_identity);
    macro_rules! lower_with_tsz_checker {
        ($checker:ident, $binder:ident, $bound_file:ident, $database:ident) => {{
            let observe = |event: backend_frontend_typescript::TszProjectExecutionEvent<'_>| session.observe_projection(event);
            // The enclosing native FileQuery already carries the exact file.
            // Subpasses inherit that worker-local context without copying paths.
            let analysis = backend_frontend_typescript::TszProjectExecutionSpan::with_observer(
                &observe, backend_frontend_typescript::TszProjectExecutionPhase::FileQueryAnalysis, None,
            );
            let build = |module: OxcModule<'_>| {
                analysis.returned();
                let structure = backend_frontend_typescript::TszProjectExecutionSpan::with_observer(
                    &observe, backend_frontend_typescript::TszProjectExecutionPhase::FileQueryStructure, None,
                );
                let mut projector = Projector {
                    semantic: &module.semantic,
                    node_index: {
                        let mut index: Vec<_> = module
                            .semantic
                            .nodes()
                            .iter_enumerated()
                            .map(|(node_id, node)| (node.kind().span(), node_id))
                            .collect();
                        index.sort_unstable_by_key(|(span, _)| (span.start, span.end));
                        index
                    },
                    source,
                    facts,
                    name_starts: Vec::new(),
                    name_ends: Vec::new(),
                    decl_starts: Vec::new(),
                    decl_ends: Vec::new(),
                    merged_declaration_parts: HashMap::new(),
                    fact_kinds: Vec::new(),
                    import_modules: Vec::new(),
                    import_module_len: 0,
                    checker: None,
                    tsz_project: Some(project),
                    tsz_bound_file: Some($bound_file),
                    tsz_binder: Some($binder),
                    tsz_file_index: Some(file_index),
                    pending_type_parameters: 0,
                    extension_type_parameters: Vec::new(),
                    staged_members: Vec::new(),
                    member_parents: Vec::new(),
                    native_member_parents: Vec::new(),
                    native_class_owners: Vec::new(),
                    parameter_properties: Vec::new(),
                    setters: Vec::new(),
                    synthetic_starts: Vec::new(),
                    synthetic_ends: Vec::new(),
                    facts_by_name: HashMap::new(),
                    fact_at_name: HashMap::new(),
                    binding_init_spans: HashMap::new(),
                    synthetic_by_name: HashMap::new(),
                    owner_index: Vec::new(),
                    owner_ancestor: Vec::new(),
                };
                projector.run()?;
                structure.returned();
                let occurrences = backend_frontend_typescript::TszProjectExecutionSpan::with_observer(
                    &observe, backend_frontend_typescript::TszProjectExecutionPhase::FileQueryOccurrences, None,
                );
                projector.pass_native_tsz_occurrences(
                    &mut *$checker,
                    $binder,
                    $bound_file,
                    session,
                    file_index,
                    program_identity,
                )?;
                occurrences.returned();
                let types = backend_frontend_typescript::TszProjectExecutionSpan::with_observer(
                    &observe, backend_frontend_typescript::TszProjectExecutionPhase::FileQueryTypes, None,
                );
                projector.pass_native_tsz(&mut *$checker, $bound_file, $database, project)?;
                types.returned();
                Ok::<(), TypeScriptCollectError>(())
            };
            if declaration_file {
                with_analysis_declaration(profile, source, true, build)
            } else {
                with_analysis(profile, source, build)
            }
            .map_err(TypeScriptCollectError::Authority)?
        }};
    }
    let lowered = session
        .with_file_checker_and_types(file_index, |checker, binder, bound_file, database| {
            lower_with_tsz_checker!(checker, binder, bound_file, database)
        })
        .map_err(|error| {
            TypeScriptCollectError::TszAuthority(TszAuthorityError::ProjectCheckerSession(error))
        })?;
    lowered
}

#[cfg(test)]
fn collect_with_tsz_unmetered_for_test<'source>(
    profile: TypeScriptSource,
    source: &'source [u8],
    project: &'source TszProject,
    source_path: &str,
    facts: &mut FactSet<'source>,
) -> Result<(), TypeScriptCollectError> {
    let session = project
        .checked_query_session_unmetered_for_test()
        .map_err(|error| {
            TypeScriptCollectError::TszAuthority(TszAuthorityError::ProjectCheckerSession(error))
        })?;
    collect_with_tsz(profile, source, project, source_path, &session, facts)
}

impl<'x, 'report, 'source, 'tsz> Projector<'x, 'report, 'source, 'tsz> {
    /// Runs the ordered projection: the self-nominal declaration pass, the
    /// alias/member/signature/variable pass, the checker computed pass, the
    /// narrowing pass, the reference pass, the checker-only reference pass,
    /// the enum-member pass, the static property-access pass, then the
    /// documentation pass.
    fn run(&mut self) -> Result<(), TypeScriptCollectError> {
        self.native_class_owners = self.native_anonymous_class_owners()?;
        self.pass_declarations()?;
        self.pass_members()?;
        self.pass_checker()?;
        self.pass_narrowings()?;
        if let Some(bound_file) = self.tsz_bound_file {
            self.pass_native_tsz_anonymous_callables(bound_file)?;
        }
        self.pass_references()?;
        self.pass_checker_references()?;
        self.pass_enum_member_accesses()?;
        self.pass_property_accesses()?;
        self.pass_docs()?;
        self.pass_declared_member_ownership(false)?;
        self.pass_parentage()?;
        self.pass_declared_member_ownership(true)?;
        self.commit_merged_declaration_parts()?;
        Ok(())
    }

    /// OXC body lists prove direct declared membership, independently of the
    /// checker object's effective structural shape. Every body element must
    /// map to one emitted declaration. Merged declarations, unsupported body
    /// elements stay unavailable. Constructor parameter properties additionally
    /// require the exact parsed binding, emitted field and direct owner.
    fn pass_declared_member_ownership(
        &mut self,
        capture_inventory: bool,
    ) -> Result<(), TypeScriptCollectError> {
        let mut declarations: HashMap<(u32, u32), Option<u32>> = HashMap::new();
        for ordinal in 0..self.facts.len() {
            if matches!(
                self.fact_kinds.get(ordinal),
                Some(EntityKind::Field | EntityKind::Function | EntityKind::Variant)
            ) {
                let span = (self.decl_starts[ordinal], self.decl_ends[ordinal]);
                declarations
                    .entry(span)
                    .and_modify(|entry| *entry = None)
                    .or_insert(Some(coordinate(ordinal)?));
            }
        }
        let semantic = self.semantic;
        for node in semantic.nodes().iter() {
            let kind = node.kind();
            let (name, spans): (Span, Vec<Span>) = if let Some(class) = kind.as_class() {
                let Some(name) = class.id.as_ref().map(|id| id.span) else {
                    continue;
                };
                (
                    name,
                    class.body.body.iter().map(|member| member.span()).collect(),
                )
            } else if let Some(interface) = kind.as_ts_interface_declaration() {
                (
                    interface.id.span,
                    interface
                        .body
                        .body
                        .iter()
                        .map(|member| member.span())
                        .collect(),
                )
            } else if let Some(enumeration) = kind.as_ts_enum_declaration() {
                (
                    enumeration.id.span,
                    enumeration
                        .body
                        .members
                        .iter()
                        .map(|member| member.span())
                        .collect(),
                )
            } else {
                continue;
            };
            let Some(owner) = self.fact_at_name_start(name.start) else {
                continue;
            };
            // One exact merge part cannot prove the whole merged inventory.
            let declaration = kind.span();
            if self.merged_declaration_parts.contains_key(&owner)
                || self.decl_starts.get(owner as usize) != Some(&declaration.start)
                || self.decl_ends.get(owner as usize) != Some(&declaration.end)
            {
                continue;
            }
            // Class/interface declaration merging crosses the IR's Record and
            // Trait entity kinds, so `merged_declaration_parts` cannot group
            // those parts under one owner. The exact TSZ binding still records
            // the merged declarations; preserve that evidence by withholding
            // a complete member inventory from either source declaration.
            let checker_reports_merge = (kind.as_class().is_some()
                || kind.as_ts_interface_declaration().is_some())
                && self
                    .tsz_bound_file
                    .and_then(|file| native_tsz_symbol_at_declaration_span(file, declaration))
                    .and_then(|symbol| {
                        self.tsz_project
                            .and_then(|project| project.program().symbols.get(symbol))
                    })
                    .is_some_and(|symbol| symbol.declarations.len() > 1);
            let mut members = Vec::with_capacity(spans.len());
            let mut complete = !checker_reports_merge;
            for span in spans {
                let Some(Some(member)) = declarations.get(&(span.start, span.end)) else {
                    complete = false;
                    break;
                };
                members.push(*member);
            }
            // Parameter properties are direct class fields as well as
            // signature parameters. Prove them from this constructor's syntax;
            // an emitted field with the same spelling elsewhere is not enough.
            let mut properties = Vec::new();
            if let Some(class) = kind.as_class() {
                for element in class.body.body.iter() {
                    let span = element.span();
                    let Some(element_kind) = self.ast_kind_at_exact_span(span.start, span.end)
                    else {
                        complete = false;
                        break;
                    };
                    let Some(method) = element_kind.as_method_definition() else {
                        continue;
                    };
                    if !method.kind.is_constructor() {
                        continue;
                    }
                    for parameter in method.value.params.items.iter() {
                        if parameter.accessibility.is_none() && !parameter.readonly {
                            continue;
                        }
                        let Some(identifier) = parameter.pattern.get_binding_identifier() else {
                            complete = false;
                            break;
                        };
                        let span = parameter.span;
                        let Some(Some(member)) = declarations.get(&(span.start, span.end)) else {
                            complete = false;
                            break;
                        };
                        let index = *member as usize;
                        if self.fact_kinds.get(index) != Some(&EntityKind::Field)
                            || !self.parameter_properties.contains(member)
                            || self.member_parents.get(index) != Some(&owner)
                            || self.name_starts.get(index) != Some(&identifier.span.start)
                            || self.name_ends.get(index) != Some(&identifier.span.end)
                            || properties.contains(member)
                        {
                            complete = false;
                            break;
                        }
                        properties.push(*member);
                    }
                    if !complete {
                        break;
                    }
                }
            }
            // Both directions are required: no parsed property may be missing,
            // and no foreign/unmatched field may enter this owner's inventory.
            let owned_properties = self
                .parameter_properties
                .iter()
                .filter(|ordinal| self.member_parents.get(**ordinal as usize) == Some(&owner))
                .count();
            if properties.len() != owned_properties {
                complete = false;
            }
            members.extend(properties);
            if complete && !capture_inventory {
                // The AST body proves direct ownership before the lexical
                // span sweep. Native callable ranges can include trailing
                // trivia and overlap the compatibility signature for the
                // same member; that overlap does not create a nested owner.
                // Never overwrite a conflicting explicit embodiment.
                for &member in &members {
                    let existing = self.member_parents[member as usize];
                    if existing != UNSET && existing != owner {
                        return Err(TypeScriptCollectError::Rejected(FactRejection {
                            fact: owner as usize,
                            name_len: self.facts.names[owner as usize].len(),
                            cause: FactFault::ConflictingParentage {
                                entity: backend_semantic::ir::EntityId::new(member),
                                existing: crate::driver::types::ParentageState::Bound {
                                    parent: backend_semantic::ir::EntityId::new(existing),
                                },
                                requested: crate::driver::types::ParentageState::Bound {
                                    parent: backend_semantic::ir::EntityId::new(owner),
                                },
                            },
                        }));
                    }
                }
                for &member in &members {
                    self.member_parents[member as usize] = owner;
                }
            } else if complete {
                self.facts
                    .capture_declared_members(owner, &members)
                    .map_err(|cause| {
                        TypeScriptCollectError::Rejected(FactRejection {
                            fact: owner as usize,
                            name_len: self.facts.names[owner as usize].len(),
                            cause,
                        })
                    })?;
            }
        }
        Ok(())
    }

    /// Retains TSZ's actual bound anonymous class declarations as owners,
    /// without inventing named class rows. Exact source/site binding is
    /// admission evidence; lexical roles alone enter the owner family.
    /// Same-route siblings retain native source-order instance ordinals in
    /// variants, explicitly separating ambiguity from durable family identity.
    fn native_anonymous_class_owners(
        &self,
    ) -> Result<Vec<(u32, u32, TypeScriptLexicalOwner)>, TypeScriptCollectError> {
        let (Some(file), Some(binder), Some(file_index)) =
            (self.tsz_bound_file, self.tsz_binder, self.tsz_file_index)
        else {
            return Ok(Vec::new());
        };
        let mut owners = Vec::new();
        for (raw, node) in file.arena.nodes.iter().enumerate() {
            if node.kind != TszSyntaxKind::CLASS_EXPRESSION {
                continue;
            }
            let class = file.arena.get_class(node).ok_or_else(lane_rejection)?;
            if !class.name.is_none() {
                continue;
            }
            let raw = coordinate(raw)?;
            let declaration = TszNodeIndex(raw);
            let (start, end) = file
                .arena
                .pos_end_at(declaration)
                .ok_or_else(lane_rejection)?;
            if start >= end || self.source.get(start as usize..end as usize).is_none() {
                return Err(lane_rejection());
            }
            let Some(symbol) = file
                .node_symbols
                .get(&raw)
                .and_then(|id| binder.symbols.get(*id))
            else {
                return Err(lane_rejection());
            };
            // Only a unique native declaration with its exact stable file
            // and range proves this otherwise unrepresented owner.
            if symbol.declarations.as_slice() != [declaration]
                || symbol.stable_declarations.len() != 1
                || symbol.value_declaration != declaration
            {
                return Err(lane_rejection());
            }
            let stable = &symbol.stable_declarations[0];
            if usize::try_from(stable.file_idx).ok() != Some(file_index)
                || (stable.pos, stable.end) != (start, end)
            {
                return Err(lane_rejection());
            }
            let Some(family) = native_tsz_anonymous_class_route(self.source, file, declaration)
            else {
                return Err(lane_rejection());
            };
            owners.push((start, end, family));
        }
        owners.sort_unstable_by_key(|(start, end, _)| (*start, *end));
        let mut counts = HashMap::<[u8; 16], u32>::new();
        for (_, _, family) in &owners {
            *counts.entry(*family).or_default() += 1;
        }
        let mut instances = HashMap::<[u8; 16], u32>::new();
        Ok(owners
            .into_iter()
            .map(|(start, end, family)| {
                let next = instances.entry(family).or_default();
                let instance = *next;
                *next += 1;
                (
                    start,
                    end,
                    TypeScriptLexicalOwner::UnrepresentedNativeClass {
                        family,
                        instance,
                        multiplicity: counts[&family],
                    },
                )
            })
            .collect())
    }

    /// Pass eight: binds every pushed fact to its innermost enclosing
    /// declaration by strict span containment, exactly as the lexical scope it
    /// lives in. TypeScript reuses member names across classes, interfaces,
    /// namespaces, and nested functions, so without a bound parent two
    /// same-name rows in different owners share one `Unavailable` family and
    /// the image build rejects the honest duplicate with a
    /// `DuplicateDeclarationIdentity`. A fact with no enclosing declaration is
    /// an authority-proven root, never a fabricated parent.
    ///
    /// Members of anonymous object literals keep the embodiment their
    /// literal lowering claimed instead: their declaring spans sit inside
    /// the nearest declared owner, but they belong to distinct anonymous
    /// objects that span containment cannot tell apart.
    fn pass_parentage(&mut self) -> Result<(), TypeScriptCollectError> {
        let length = self.facts.len;
        if self
            .native_member_parents
            .windows(2)
            .any(|pair| pair[0].0 >= pair[1].0)
            || self
                .native_member_parents
                .last()
                .is_some_and(|(ordinal, _)| *ordinal as usize >= length)
        {
            return Err(lane_rejection());
        }
        // One sweep over source-ordered events: an owner opens its lexical
        // range, and every other fact asks the innermost still-open owner for
        // its parent. Declaration spans nest or stay disjoint, so a stack is
        // exact, and the pass stays O(facts log facts) instead of rescanning
        // every candidate for every fact.
        let mut events = Vec::with_capacity(length * 2);
        let mut native_parent_cursor = 0;
        for index in 0..length {
            let ordinal = coordinate(index)?;
            let native_member = self
                .native_member_parents
                .get(native_parent_cursor)
                .is_some_and(|(fact, _)| *fact == ordinal);
            if native_member {
                native_parent_cursor += 1;
            }
            let is_member =
                self.member_parents.get(index).copied().unwrap_or(UNSET) != UNSET || native_member;
            let decl_start = self.decl_starts.get(index).copied().unwrap_or(UNSET);
            let decl_end = self.decl_ends.get(index).copied().unwrap_or(UNSET);
            let synthetic = decl_start == UNSET || decl_end == UNSET;
            let (start, end) = if synthetic {
                (
                    self.synthetic_starts.get(index).copied().unwrap_or(UNSET),
                    self.synthetic_ends.get(index).copied().unwrap_or(UNSET),
                )
            } else {
                (decl_start, decl_end)
            };
            if !is_member && start != UNSET && end != UNSET {
                events.push((start, end, None, Some(ordinal)));
            }
            if !synthetic
                && ts_lexical_owner(
                    self.fact_kinds
                        .get(index)
                        .copied()
                        .unwrap_or(EntityKind::Parameter),
                )
            {
                if let Some(parts) = self.merged_declaration_parts.get(&ordinal) {
                    events.extend(parts.iter().map(|part| {
                        (
                            part.start,
                            part.end,
                            Some(TypeScriptLexicalOwner::Declaration(ordinal)),
                            None,
                        )
                    }));
                } else {
                    events.push((
                        decl_start,
                        decl_end,
                        Some(TypeScriptLexicalOwner::Declaration(ordinal)),
                        None,
                    ));
                }
            }
        }
        events.extend(
            self.native_class_owners
                .iter()
                .copied()
                .map(|(start, end, owner)| (start, end, Some(owner), None)),
        );
        events.sort_unstable_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then(right.1.cmp(&left.1))
                .then(left.2.is_some().cmp(&right.2.is_some()))
        });
        let mut stack: Vec<(u32, u32, TypeScriptLexicalOwner)> = Vec::new();
        let mut owners = vec![None; length];
        for (start, end, owner, ordinal) in events {
            while stack
                .last()
                .is_some_and(|(top_end, _, _)| *top_end <= start)
            {
                stack.pop();
            }
            if let Some(owner) = owner {
                stack.push((end, start, owner));
                continue;
            }
            let Some(ordinal) = ordinal else {
                continue;
            };
            let mut found = None;
            for &(top_end, top_start, top_ordinal) in stack.iter().rev() {
                if top_ordinal == TypeScriptLexicalOwner::Declaration(ordinal) {
                    continue;
                }
                if top_start == start && top_end == end {
                    continue;
                }
                if top_end >= end {
                    found = Some(top_ordinal);
                    break;
                }
            }
            owners[usize::try_from(ordinal).unwrap_or(0)] = found;
        }
        let mut native_parent_cursor = 0;
        for index in 0..length {
            let ordinal = coordinate(index)?;
            let native_parent = self
                .native_member_parents
                .get(native_parent_cursor)
                .copied()
                .filter(|(fact, _)| *fact == ordinal)
                .map(|(_, owner)| owner);
            if native_parent.is_some() {
                native_parent_cursor += 1;
            }
            if let Some(embodiment) = self
                .member_parents
                .get(index)
                .copied()
                .filter(|parent| *parent != UNSET)
            {
                if native_parent.is_some() {
                    return Err(lane_rejection());
                }
                self.facts
                    .attach_parent(ordinal, embodiment)
                    .map_err(fault)?;
                continue;
            }
            match native_parent.or(owners[index]) {
                Some(TypeScriptLexicalOwner::Declaration(parent)) => {
                    self.facts.attach_parent(ordinal, parent).map_err(fault)?
                }
                Some(TypeScriptLexicalOwner::UnrepresentedNativeClass {
                    family,
                    instance,
                    multiplicity,
                }) => {
                    debug_assert!(instance < multiplicity);
                    self.facts
                        .mark_unrepresented_parent(ordinal, family)
                        .map_err(fault)?;
                    // Keep the native sibling instance out of the family.
                    // Preserve any existing overload/twin discriminator.
                    let mut hash = Sha256::new();
                    hash.update(b"compiler.typescript.native-class-member-instance.v1\0");
                    hash.update(instance.to_le_bytes());
                    if let Some(existing) = self.facts.identity_discriminators[index] {
                        hash.update([1]);
                        hash.update(existing);
                    } else {
                        hash.update([0]);
                    }
                    let mut discriminator = [0_u8; 16];
                    discriminator.copy_from_slice(&hash.finalize()[..16]);
                    self.facts.identity_discriminators[index] = Some(discriminator);
                }
                None => self.facts.mark_parentage_root(ordinal).map_err(fault)?,
            }
        }
        Ok(())
    }

    /// Stages one generic-parameter row onto the bounded staging lane.
    fn stage_row(
        rows: &mut TypeParamRows,
        row: TypeParamRow,
    ) -> Result<(), TypeScriptCollectError> {
        rows.push(row)
    }

    /// Pass one: every declaration whose declared type is itself (interfaces,
    /// classes, enums, namespaces) plus import bindings. These facts never
    /// reference another declaration, so every later pass can link at them.
    fn pass_declarations(&mut self) -> Result<(), TypeScriptCollectError> {
        let semantic = self.semantic;
        for node in semantic.nodes().iter() {
            let kind = node.kind();
            let declaration_span = kind.span();
            if let Some(interface) = kind.as_ts_interface_declaration() {
                let mut rows = TypeParamRows::new();
                if let Some(declared) = interface.type_parameters.as_ref() {
                    for parameter in declared.params.iter() {
                        Self::stage_row(
                            &mut rows,
                            TypeParamRow {
                                name: parameter.name.span,
                                constraint: parameter.constraint.as_ref().map(|t| t.span()),
                                default: parameter.default.as_ref().map(|t| t.span()),
                            },
                        )?;
                    }
                }
                self.push_self_nominal(
                    EntityKind::Trait,
                    SemanticProductConstructor::INTERSECTION,
                    declaration_span,
                    interface.id.span,
                    &rows,
                )?;
            } else if let Some(class) = kind.as_class() {
                let Some(id) = class.id.as_ref() else {
                    continue;
                };
                let mut rows = TypeParamRows::new();
                if let Some(declared) = class.type_parameters.as_ref() {
                    for parameter in declared.params.iter() {
                        Self::stage_row(
                            &mut rows,
                            TypeParamRow {
                                name: parameter.name.span,
                                constraint: parameter.constraint.as_ref().map(|t| t.span()),
                                default: parameter.default.as_ref().map(|t| t.span()),
                            },
                        )?;
                    }
                }
                self.push_self_nominal(
                    EntityKind::Record,
                    SemanticProductConstructor::PRODUCT,
                    declaration_span,
                    id.span,
                    &rows,
                )?;
                for element in class.body.body.iter() {
                    let element_span = element.span();
                    let Some(element_kind) =
                        self.ast_kind_at_exact_span(element_span.start, element_span.end)
                    else {
                        continue;
                    };
                    let Some(method) = element_kind.as_method_definition() else {
                        continue;
                    };
                    if self.text_span(method.key.span()) != Some("constructor") {
                        continue;
                    }
                    if let Some(body) = method.value.body.as_ref() {
                        for statement in body.statements.iter() {
                            let statement_span = statement.span();
                            self.declare_assignment_bindings_in_statement_span(
                                statement_span.start,
                                statement_span.end,
                            )?;
                        }
                    }
                }
            } else if let Some(enumeration) = kind.as_ts_enum_declaration() {
                self.push_self_nominal(
                    EntityKind::Enum,
                    SemanticProductConstructor::UNION,
                    declaration_span,
                    enumeration.id.span,
                    &TypeParamRows::new(),
                )?;
            } else if let Some(module) = kind.as_ts_module_declaration() {
                let name_span = module.id.span();
                self.push_self_nominal(
                    EntityKind::Module,
                    LEAF_PRODUCT,
                    declaration_span,
                    name_span,
                    &TypeParamRows::new(),
                )?;
            } else if let Some(import) = kind.as_import_declaration() {
                let module = strip_quotes(import.source.span);
                let Some(specifiers) = import.specifiers.as_ref() else {
                    continue;
                };
                for specifier in specifiers.iter() {
                    let specifier_span = specifier.span();
                    let mut local: Option<Span> = None;
                    let mut display: Option<Span> = None;
                    for probed in semantic.nodes().iter() {
                        let specifier_kind = probed.kind();
                        if specifier_kind.span() != specifier_span {
                            continue;
                        }
                        if let Some(named) = specifier_kind.as_import_specifier() {
                            local = Some(named.local.span);
                            display = Some(named.imported.span());
                        } else if let Some(default) = specifier_kind.as_import_default_specifier() {
                            local = Some(default.local.span);
                            display = local;
                        } else if let Some(namespace) =
                            specifier_kind.as_import_namespace_specifier()
                        {
                            local = Some(namespace.local.span);
                            display = local;
                        }
                    }
                    let Some(local) = local else {
                        continue;
                    };
                    let display = display.unwrap_or(local);
                    self.push_import_binding(declaration_span, module, local, display)?;
                }
            }
        }
        Ok(())
    }

    /// Pass two, in source order: type aliases, generic-parameter facts,
    /// function and method signatures with their parameter and result facts,
    /// members, enum members, and variables. Every type record here may link
    /// at the pass-one ordinals and at earlier pass-two facts; a reference to
    /// a later in-file fact is honestly unknown with its spelling.
    fn pass_members(&mut self) -> Result<(), TypeScriptCollectError> {
        let semantic = self.semantic;
        for node in semantic.nodes().iter() {
            let kind = node.kind();
            let declaration_span = kind.span();
            if let Some(alias) = kind.as_ts_type_alias_declaration() {
                if self.fact_at_name_start(alias.id.span.start).is_some() {
                    continue;
                }
                let mut rows = TypeParamRows::new();
                if let Some(declared) = alias.type_parameters.as_ref() {
                    for parameter in declared.params.iter() {
                        Self::stage_row(
                            &mut rows,
                            TypeParamRow {
                                name: parameter.name.span,
                                constraint: parameter.constraint.as_ref().map(|t| t.span()),
                                default: parameter.default.as_ref().map(|t| t.span()),
                            },
                        )?;
                    }
                }
                self.push_type_parameter_facts(&rows)?;
                let type_parameter_start = self.push_type_params(&rows, 0)?;
                let annotation = alias.type_annotation.span();
                let member_base = self.staged_members.len();
                let cells = self.owner_cells(annotation.start, annotation.end, 0)?;
                let name_bytes =
                    self.slice_span(alias.id.span)
                        .ok_or(TypeScriptCollectError::Span {
                            start: alias.id.span.start,
                            end: alias.id.span.end,
                        })?;
                let extension = self.extension(type_parameter_start)?;
                let fact = with_cells(
                    SemanticFact::new(EntityKind::Alias, name_bytes, LEAF_PRODUCT)
                        .with_extension(extension),
                    cells,
                );
                let ordinal = self.push(fact)?;
                self.register(ordinal, declaration_span, alias.id.span, EntityKind::Alias)?;
                self.claim_staged_members(member_base, ordinal);
            } else if let Some(parameter) = kind.as_ts_type_parameter() {
                // Generic parameters were pushed beside their owning
                // declaration; only a parameter outside that path remains.
                if self.fact_at_name_start(parameter.name.span.start).is_some() {
                    continue;
                }
                let name =
                    self.slice_span(parameter.name.span)
                        .ok_or(TypeScriptCollectError::Span {
                            start: parameter.name.span.start,
                            end: parameter.name.span.end,
                        })?;
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::TypeVar);
                record.text = Some(name);
                // Two `infer X` parameters in mutually exclusive conditional
                // branches are name-identical lone `TypeVar` rows in one
                // owner; the image cannot tell them apart, so the first owns
                // the one fact every occurrence of that name resolves to.
                if self
                    .merged_simple_declaration(
                        EntityKind::Parameter,
                        name,
                        declaration_span,
                        record,
                        0,
                        None,
                    )
                    .is_some()
                {
                    continue;
                }
                let extension = self.extension_without_type_parameters()?;
                let fact = SemanticFact::new(EntityKind::Parameter, name, LEAF_PRODUCT)
                    .typed(record)
                    .with_extension(extension);
                let ordinal = self.push(fact)?;
                self.register(
                    ordinal,
                    declaration_span,
                    parameter.name.span,
                    EntityKind::Parameter,
                )?;
            } else if let Some(function) = kind.as_function() {
                let Some(id) = function.id.as_ref() else {
                    continue;
                };
                let mut rows = TypeParamRows::new();
                if let Some(declared) = function.type_parameters.as_ref() {
                    for parameter in declared.params.iter() {
                        Self::stage_row(
                            &mut rows,
                            TypeParamRow {
                                name: parameter.name.span,
                                constraint: parameter.constraint.as_ref().map(|t| t.span()),
                                default: parameter.default.as_ref().map(|t| t.span()),
                            },
                        )?;
                    }
                }
                let mut params = ParamRows::new();
                for parameter in function.params.items.iter() {
                    params.push(ParamRow {
                        name: parameter.pattern.span(),
                        annotation: parameter
                            .type_annotation
                            .as_ref()
                            .map(|annotation| annotation.type_annotation.span()),
                        default: parameter.initializer.as_ref().map(|init| init.span()),
                        flags: if parameter.optional {
                            SemanticTypeChild::FLAG_OPTIONAL
                        } else {
                            0
                        },
                    })?;
                }
                if let Some(rest) = function.params.rest.as_ref() {
                    params.push(ParamRow {
                        name: rest.rest.argument.span(),
                        annotation: rest
                            .type_annotation
                            .as_ref()
                            .map(|annotation| annotation.type_annotation.span()),
                        default: None,
                        flags: SemanticTypeChild::FLAG_REST,
                    })?;
                }
                let result = function
                    .return_type
                    .as_ref()
                    .map(|returned| returned.type_annotation.span());
                self.push_signature(id.span, declaration_span, &rows, &params, result, false)?;
            } else if let Some(declarator) = kind.as_variable_declarator() {
                let bindings = self.destructured_bindings(declarator.id.span());
                if self
                    .fact_at_name_start(declarator.id.span().start)
                    .is_some()
                {
                    continue;
                }
                let name_span = declarator.id.span();
                let name_bytes =
                    self.slice_span(name_span)
                        .ok_or(TypeScriptCollectError::Span {
                            start: name_span.start,
                            end: name_span.end,
                        })?;
                let entity_kind = self.declarator_kind(name_span.start);
                let type_parameter_start = coordinate(self.facts.type_parameter_len)?;
                let member_base = self.staged_members.len();
                let cells = match declarator.type_annotation.as_ref() {
                    Some(annotation) => {
                        let inner = annotation.type_annotation.span();
                        self.owner_cells(inner.start, inner.end, 0)?
                    }
                    None => TypeCells::unknown(TypeReason::Unannotated),
                };
                let extension = self.extension(type_parameter_start)?;
                let record = cells.record;
                let child_count = cells.len;
                let init_span = declarator.init.as_ref().map(|init| init.span());
                let mut fact = with_cells(
                    SemanticFact::new(entity_kind, name_bytes, LEAF_PRODUCT)
                        .with_extension(extension),
                    cells,
                );
                // A block-scope redeclaration the flattened lane cannot
                // distinguish from an earlier one (`const value = 1` in two
                // sibling blocks) is byte-identical at the row level and
                // carries the same initializer. The first row owns the
                // binding; occurrences of either spelling resolve to it.
                if child_count == 0 && bindings.is_none() {
                    if let Some(ordinal) = self.merged_simple_declaration(
                        entity_kind,
                        name_bytes,
                        declaration_span,
                        record,
                        0,
                        init_span,
                    ) {
                        self.fact_at_name.insert(name_span.start, ordinal);
                        continue;
                    }
                }
                let twins = self.indistinguishable_signature_twins(declaration_span, &fact);
                if twins > 0 {
                    let mut hash = Sha256::new();
                    hash.update(b"compiler.typescript.binding-twin.v1\0");
                    hash.update(twins.to_le_bytes());
                    let mut discriminator = [0_u8; 16];
                    discriminator.copy_from_slice(&hash.finalize()[..16]);
                    fact = fact.with_identity_discriminator(discriminator);
                }
                let ordinal = self.push(fact)?;
                self.register(ordinal, declaration_span, name_span, entity_kind)?;
                if matches!(entity_kind, EntityKind::Constant | EntityKind::Static) {
                    self.binding_init_spans.insert(ordinal, init_span);
                }
                self.claim_staged_members(member_base, ordinal);
                if let Some(bindings) = bindings {
                    // The pattern owns its aggregate annotation and initializer
                    // references, but is not an authored declaration name.
                    self.facts.publication_roles[ordinal as usize] =
                        backend_semantic::ir::DeclarationPublicationRole::ReferenceOnly;
                    for binding in bindings {
                        if self.fact_at_name_start(binding.start).is_some() { continue; }
                        let name = self.slice_span(binding).ok_or(TypeScriptCollectError::Span {
                            start: binding.start, end: binding.end,
                        })?;
                        let kind = self.declarator_kind(binding.start);
                        let extension = self.extension_without_type_parameters()?;
                        // An aggregate annotation/initializer is not the type
                        // or callable initializer of each destructured leaf.
                        let mut fact = SemanticFact::new(kind, name, LEAF_PRODUCT)
                            .typed(unknown_record(TypeReason::Unannotated))
                            .with_extension(extension);
                        let twins = self.indistinguishable_signature_twins(binding, &fact);
                        if twins > 0 {
                            let mut hash = Sha256::new();
                            hash.update(b"compiler.typescript.binding-twin.v1\0");
                            hash.update(twins.to_le_bytes());
                            let mut discriminator = [0_u8; 16];
                            discriminator.copy_from_slice(&hash.finalize()[..16]);
                            fact = fact.with_identity_discriminator(discriminator);
                        }
                        let leaf = self.push(fact)?;
                        self.register(leaf, binding, binding, kind)?;
                    }
                }
                if let Some(init) = declarator.init.as_ref() {
                    let init_span = init.span();
                    self.declare_expression_bindings(init_span.start, init_span.end, 0)?;
                }
            } else if let Some(parameter) = kind.as_catch_parameter() {
                if !parameter.pattern.is_binding_identifier() {
                    continue;
                }
                let name_span = parameter.pattern.span();
                if self.fact_at_name_start(name_span.start).is_some() {
                    continue;
                }
                let name_bytes =
                    self.slice_span(name_span)
                        .ok_or(TypeScriptCollectError::Span {
                            start: name_span.start,
                            end: name_span.end,
                        })?;
                let entity_kind = self.declarator_kind(name_span.start);
                let type_parameter_start = coordinate(self.facts.type_parameter_len)?;
                let member_base = self.staged_members.len();
                let cells = match parameter.type_annotation.as_ref() {
                    Some(annotation) => {
                        let inner = annotation.type_annotation.span();
                        self.owner_cells(inner.start, inner.end, 0)?
                    }
                    None => TypeCells::unknown(TypeReason::Unannotated),
                };
                let extension = self.extension(type_parameter_start)?;
                let record = cells.record;
                let child_count = cells.len;
                let fact = with_cells(
                    SemanticFact::new(entity_kind, name_bytes, LEAF_PRODUCT)
                        .with_extension(extension),
                    cells,
                );
                if child_count == 0 {
                    if let Some(ordinal) = self.merged_simple_declaration(
                        entity_kind,
                        name_bytes,
                        declaration_span,
                        record,
                        0,
                        None,
                    ) {
                        self.fact_at_name.insert(name_span.start, ordinal);
                        continue;
                    }
                }
                let ordinal = self.push(fact)?;
                self.register(ordinal, name_span, name_span, entity_kind)?;
                self.claim_staged_members(member_base, ordinal);
            } else if let Some(property) = kind.as_ts_property_signature() {
                if self.fact_at_name_start(property.key.span().start).is_some() {
                    continue;
                }
                let key_span = property.key.span();
                let name_bytes = self
                    .slice_span(key_span)
                    .ok_or(TypeScriptCollectError::Span {
                        start: key_span.start,
                        end: key_span.end,
                    })?;
                let type_parameter_start = coordinate(self.facts.type_parameter_len)?;
                let member_base = self.staged_members.len();
                let cells = match property.type_annotation.as_ref() {
                    Some(annotation) => {
                        let inner = annotation.type_annotation.span();
                        self.owner_cells(inner.start, inner.end, 0)?
                    }
                    None => TypeCells::unknown(TypeReason::Unannotated),
                };
                let extension = self.extension(type_parameter_start)?;
                let fact = with_cells(
                    SemanticFact::new(EntityKind::Field, name_bytes, LEAF_PRODUCT)
                        .with_extension(extension),
                    cells,
                );
                // A member the type-lowering path could not reach (a truncated
                // deeply nested conditional, say) still reaches this syntax
                // pass. A structurally identical same-name member in the same
                // owner is the same row, so the first is reused.
                if self.staged_members.len() == member_base
                    && self.merged_fact(declaration_span, &fact).is_some()
                {
                    continue;
                }
                let ordinal = self.push(fact)?;
                self.register(ordinal, declaration_span, key_span, EntityKind::Field)?;
                self.claim_staged_members(member_base, ordinal);
            } else if let Some(method) = kind.as_ts_method_signature() {
                let key_span = method.key.span();
                if self.fact_at_name_start(key_span.start).is_some() {
                    continue;
                }
                let mut rows = TypeParamRows::new();
                if let Some(declared) = method.type_parameters.as_ref() {
                    for parameter in declared.params.iter() {
                        Self::stage_row(
                            &mut rows,
                            TypeParamRow {
                                name: parameter.name.span,
                                constraint: parameter.constraint.as_ref().map(|t| t.span()),
                                default: parameter.default.as_ref().map(|t| t.span()),
                            },
                        )?;
                    }
                }
                let mut params = ParamRows::new();
                for parameter in method.params.items.iter() {
                    params.push(ParamRow {
                        name: parameter.pattern.span(),
                        annotation: parameter
                            .type_annotation
                            .as_ref()
                            .map(|annotation| annotation.type_annotation.span()),
                        default: parameter.initializer.as_ref().map(|init| init.span()),
                        flags: if parameter.optional {
                            SemanticTypeChild::FLAG_OPTIONAL
                        } else {
                            0
                        },
                    })?;
                }
                let result = method
                    .return_type
                    .as_ref()
                    .map(|returned| returned.type_annotation.span());
                self.push_signature(key_span, declaration_span, &rows, &params, result, false)?;
            } else if let Some(index_signature) = kind.as_ts_index_signature() {
                if self.fact_at_name_start(declaration_span.start).is_some() {
                    continue;
                }
                let inner = index_signature.type_annotation.type_annotation.span();
                let name_span = Span::new(declaration_span.start, inner.start);
                let name_bytes =
                    self.slice_span(name_span)
                        .ok_or(TypeScriptCollectError::Span {
                            start: name_span.start,
                            end: name_span.end,
                        })?;
                let type_parameter_start = coordinate(self.facts.type_parameter_len)?;
                let member_base = self.staged_members.len();
                let cells = self.owner_cells(inner.start, inner.end, 0)?;
                let extension = self.extension(type_parameter_start)?;
                let fact = with_cells(
                    SemanticFact::new(EntityKind::Field, name_bytes, LEAF_PRODUCT)
                        .with_extension(extension),
                    cells,
                );
                let ordinal = self.push(fact)?;
                self.register(ordinal, declaration_span, name_span, EntityKind::Field)?;
                self.claim_staged_members(member_base, ordinal);
            } else if let Some(definition) = kind.as_property_definition() {
                let key_span = definition.key.span();
                if self.fact_at_name_start(key_span.start).is_some() {
                    continue;
                }
                let name_bytes = self
                    .slice_span(key_span)
                    .ok_or(TypeScriptCollectError::Span {
                        start: key_span.start,
                        end: key_span.end,
                    })?;
                let type_parameter_start = coordinate(self.facts.type_parameter_len)?;
                let member_base = self.staged_members.len();
                let cells = match definition.type_annotation.as_ref() {
                    Some(annotation) => {
                        let inner = annotation.type_annotation.span();
                        self.owner_cells(inner.start, inner.end, 0)?
                    }
                    None => TypeCells::unknown(TypeReason::Unannotated),
                };
                let extension = self.extension(type_parameter_start)?;
                let mut base = SemanticFact::new(EntityKind::Field, name_bytes, LEAF_PRODUCT)
                    .with_extension(extension);
                if definition.r#static {
                    base = base.static_member();
                }
                let fact = with_cells(base, cells);
                let ordinal = self.push(fact)?;
                self.register(ordinal, declaration_span, key_span, EntityKind::Field)?;
                self.claim_staged_members(member_base, ordinal);
                if let Some(value) = definition.value.as_ref() {
                    let value_span = value.span();
                    self.declare_expression_bindings(value_span.start, value_span.end, 0)?;
                }
            } else if let Some(definition) = kind.as_accessor_property() {
                let key_span = definition.key.span();
                if self.fact_at_name_start(key_span.start).is_some() {
                    continue;
                }
                let name_bytes = self
                    .slice_span(key_span)
                    .ok_or(TypeScriptCollectError::Span {
                        start: key_span.start,
                        end: key_span.end,
                    })?;
                let type_parameter_start = coordinate(self.facts.type_parameter_len)?;
                let member_base = self.staged_members.len();
                let cells = match definition.type_annotation.as_ref() {
                    Some(annotation) => {
                        let inner = annotation.type_annotation.span();
                        self.owner_cells(inner.start, inner.end, 0)?
                    }
                    None => TypeCells::unknown(TypeReason::Unannotated),
                };
                let extension = self.extension(type_parameter_start)?;
                let mut base = SemanticFact::new(EntityKind::Field, name_bytes, LEAF_PRODUCT)
                    .with_extension(extension);
                if definition.r#static {
                    base = base.static_member();
                }
                let fact = with_cells(base, cells);
                let ordinal = self.push(fact)?;
                self.register(ordinal, declaration_span, key_span, EntityKind::Field)?;
                self.claim_staged_members(member_base, ordinal);
                if let Some(value) = definition.value.as_ref() {
                    let value_span = value.span();
                    self.declare_expression_bindings(value_span.start, value_span.end, 0)?;
                }
            } else if let Some(definition) = kind.as_method_definition() {
                let key_span = definition.key.span();
                if self.fact_at_name_start(key_span.start).is_some() {
                    continue;
                }
                let value = &definition.value;
                let mut rows = TypeParamRows::new();
                if let Some(declared) = value.type_parameters.as_ref() {
                    for parameter in declared.params.iter() {
                        Self::stage_row(
                            &mut rows,
                            TypeParamRow {
                                name: parameter.name.span,
                                constraint: parameter.constraint.as_ref().map(|t| t.span()),
                                default: parameter.default.as_ref().map(|t| t.span()),
                            },
                        )?;
                    }
                }
                let mut params = ParamRows::new();
                for parameter in value.params.items.iter() {
                    params.push(ParamRow {
                        name: parameter.pattern.span(),
                        annotation: parameter
                            .type_annotation
                            .as_ref()
                            .map(|annotation| annotation.type_annotation.span()),
                        default: parameter.initializer.as_ref().map(|init| init.span()),
                        flags: if parameter.optional {
                            SemanticTypeChild::FLAG_OPTIONAL
                        } else {
                            0
                        },
                    })?;
                }
                let result = value
                    .return_type
                    .as_ref()
                    .map(|returned| returned.type_annotation.span());
                let ordinal = self.push_signature(
                    key_span,
                    declaration_span,
                    &rows,
                    &params,
                    result,
                    definition.r#static,
                )?;
                if definition.kind.is_set() {
                    self.setters.push(ordinal);
                }
                if definition.kind.is_constructor() {
                    self.push_parameter_properties(value.params.span(), definition.span)?;
                }
                if let Some(body) = value.body.as_ref() {
                    for statement in body.statements.iter() {
                        let statement_span = statement.span();
                        self.declare_assignment_bindings_in_statement_span(
                            statement_span.start,
                            statement_span.end,
                        )?;
                    }
                }
            } else if let Some(signature) = kind.as_ts_call_signature_declaration() {
                // A call signature inside an anonymous object literal was
                // already embodied while that literal lowered (its member
                // carries the record's required name). Re-lowering the same
                // node here would mint a byte-identical twin and duplicate
                // every parameter fact, so it is skipped exactly as the
                // named member branches above skip their own claimed names.
                if self.fact_at_name_start(declaration_span.start).is_some() {
                    continue;
                }
                // Anonymous call overloads own their generic parameters
                // exactly as named signatures do. Without an embodiment the
                // parameters become orphaned lone facts that collide across
                // overloads, so each signature is pushed as a Function fact
                // named by its exact source spelling.
                let mut rows = TypeParamRows::new();
                if let Some(declared) = signature.type_parameters.as_ref() {
                    for parameter in declared.params.iter() {
                        Self::stage_row(
                            &mut rows,
                            TypeParamRow {
                                name: parameter.name.span,
                                constraint: parameter.constraint.as_ref().map(|t| t.span()),
                                default: parameter.default.as_ref().map(|t| t.span()),
                            },
                        )?;
                    }
                }
                let mut params = ParamRows::new();
                for parameter in signature.params.items.iter() {
                    params.push(ParamRow {
                        name: parameter.pattern.span(),
                        annotation: parameter
                            .type_annotation
                            .as_ref()
                            .map(|annotation| annotation.type_annotation.span()),
                        default: parameter.initializer.as_ref().map(|init| init.span()),
                        flags: if parameter.optional {
                            SemanticTypeChild::FLAG_OPTIONAL
                        } else {
                            0
                        },
                    })?;
                }
                if let Some(rest) = signature.params.rest.as_ref() {
                    params.push(ParamRow {
                        name: rest.rest.argument.span(),
                        annotation: rest
                            .type_annotation
                            .as_ref()
                            .map(|annotation| annotation.type_annotation.span()),
                        default: None,
                        flags: SemanticTypeChild::FLAG_REST,
                    })?;
                }
                let result = signature
                    .return_type
                    .as_ref()
                    .map(|returned| returned.type_annotation.span());
                self.push_signature(
                    declaration_span,
                    declaration_span,
                    &rows,
                    &params,
                    result,
                    false,
                )?;
            } else if let Some(signature) = kind.as_ts_construct_signature_declaration() {
                // Anonymous construct overloads embody exactly as call
                // overloads do; only the signature node differs. A literal
                // member was already embodied by its enclosing literal.
                if self.fact_at_name_start(declaration_span.start).is_some() {
                    continue;
                }
                let mut rows = TypeParamRows::new();
                if let Some(declared) = signature.type_parameters.as_ref() {
                    for parameter in declared.params.iter() {
                        Self::stage_row(
                            &mut rows,
                            TypeParamRow {
                                name: parameter.name.span,
                                constraint: parameter.constraint.as_ref().map(|t| t.span()),
                                default: parameter.default.as_ref().map(|t| t.span()),
                            },
                        )?;
                    }
                }
                let mut params = ParamRows::new();
                for parameter in signature.params.items.iter() {
                    params.push(ParamRow {
                        name: parameter.pattern.span(),
                        annotation: parameter
                            .type_annotation
                            .as_ref()
                            .map(|annotation| annotation.type_annotation.span()),
                        default: parameter.initializer.as_ref().map(|init| init.span()),
                        flags: if parameter.optional {
                            SemanticTypeChild::FLAG_OPTIONAL
                        } else {
                            0
                        },
                    })?;
                }
                if let Some(rest) = signature.params.rest.as_ref() {
                    params.push(ParamRow {
                        name: rest.rest.argument.span(),
                        annotation: rest
                            .type_annotation
                            .as_ref()
                            .map(|annotation| annotation.type_annotation.span()),
                        default: None,
                        flags: SemanticTypeChild::FLAG_REST,
                    })?;
                }
                let result = signature
                    .return_type
                    .as_ref()
                    .map(|returned| returned.type_annotation.span());
                self.push_signature(
                    declaration_span,
                    declaration_span,
                    &rows,
                    &params,
                    result,
                    false,
                )?;
            } else if let Some(member) = kind.as_ts_enum_member() {
                let name_span = member.id.span();
                if self.fact_at_name_start(name_span.start).is_some() {
                    continue;
                }
                let name_bytes =
                    self.slice_span(name_span)
                        .ok_or(TypeScriptCollectError::Span {
                            start: name_span.start,
                            end: name_span.end,
                        })?;
                let enum_fact = self
                    .owning_fact(declaration_span.start)
                    .ok_or_else(lane_rejection)?;
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Nominal);
                record.nominal = Some(NominalRef::Local(EntityId::new(enum_fact)));
                let extension = self.extension_without_type_parameters()?;
                let fact = SemanticFact::new(EntityKind::Variant, name_bytes, LEAF_PRODUCT)
                    .typed(record)
                    .with_extension(extension);
                let ordinal = self.push(fact)?;
                self.register(ordinal, declaration_span, name_span, EntityKind::Variant)?;
            }
        }
        Ok(())
    }

    /// Resolves the binding class of one variable declarator through OXC's
    /// symbol table: a `const` binding is a constant, everything else is a
    /// static.
    fn declarator_kind(&self, name_start: u32) -> EntityKind {
        let scoping = self.semantic.scoping();
        for symbol in scoping.symbol_ids() {
            if scoping.symbol_span(symbol).start == name_start {
                if scoping
                    .symbol_flags(symbol)
                    .contains(SymbolFlags::ConstVariable)
                {
                    return EntityKind::Constant;
                }
                return EntityKind::Static;
            }
        }
        EntityKind::Static
    }

    /// Pass three: the checker's computed type plane. Every checker-computed
    /// declaration whose name binds to a published fact receives one observed
    /// type row — compound shapes intern their children first so the pool
    /// stays topologically backward — and a completed extension whose
    /// observation names that exact staged row. No compatibility node is
    /// minted: concrete checker observations stay concrete.
    fn pass_checker(&mut self) -> Result<(), TypeScriptCollectError> {
        let Some(checker) = self.checker.as_ref() else {
            return Ok(());
        };
        // Bound declaration rows are `Copy`; the loop body mutates the lane,
        // so the iteration set is copied out of the borrowed report first.
        let declarations: Vec<_> = checker.declarations().copied().collect();
        let fact_len = u32::try_from(self.facts.len()).map_err(|_| lane_rejection())?;
        let registry = FactRegistry {
            source: self.source,
            tsz_project: None,
            source_path: None,
            decl_starts: &self.decl_starts,
            decl_ends: &self.decl_ends,
            declaration_parts: &self.merged_declaration_parts,
            name_starts: &self.name_starts,
            name_start_index: index_name_starts(&self.name_starts, fact_len),
            name_ends: &self.name_ends,
            fact_kinds: &self.fact_kinds,
            native_tsz_entities: None,
            fact_len,
        };
        for declaration in declarations {
            if declaration.origin != Origin::Computed {
                continue;
            }
            let Some(tree) = declaration.r#type else {
                continue;
            };
            let Some(owner) = registry.fact_at_name_start(declaration.name.start) else {
                // The checker typed a declaration the lane does not publish;
                // there is no honest owner for a computed row.
                continue;
            };
            let owner_index = usize::try_from(owner).map_err(|_| lane_rejection())?;
            if registry.fact_kinds.get(owner_index) == Some(&EntityKind::Reexport) {
                // Import bindings already carry their npm foreign origin.
                continue;
            }
            // The computed type lane is a fixed bound. Once it is full, no
            // later checker type has a representable row. The declaration
            // keeps its declared type and stays published; only the observed
            // cell is left absent rather than failing the whole projection or
            // fabricating an unknown row the capacity cannot hold.
            let row = match intern_computed_tree(
                &registry,
                self.facts,
                tree,
                owner,
                0,
                SpellDomain::Owner,
            ) {
                Ok(row) => row,
                Err(TypeScriptCollectError::Rejected(FactRejection {
                    cause: FactFault::ComputedRowCapacity,
                    ..
                })) => break,
                Err(cause) => return Err(cause),
            };
            let _ordinal = row
                .checked_sub(COMPUTED_ROW_BASE)
                .ok_or_else(lane_rejection)?;
            let type_parameters = self
                .extension_type_parameters
                .get(owner_index)
                .copied()
                .ok_or_else(lane_rejection)?;
            let extension = EmissionExtension::TypeScript(backend_semantic::ir::TypeScriptFacts {
                type_parameters: TypeParameterListId::new(type_parameters),
                declared: Some(TypeId::new(owner)),
                observed: Some(TypeId::new(row)),
            });
            self.facts
                .attach_extension_with_type_parameters(
                    owner_index,
                    extension,
                    self.facts
                        .captured_type_parameter_range(owner_index)
                        .map_err(fault)?,
                )
                .map_err(fault)?;
        }
        Ok(())
    }

    /// Adds occurrence facts resolved by the exact borrowed TSZ program.
    /// OXC's source occurrences remain the fallback for unsupported syntax;
    /// only a binder/checker-resolved member or identifier with a unique
    /// stable declaration coordinate upgrades that existing site.
    fn pass_native_tsz_anonymous_callables(
        &mut self,
        bound_file: &TszBoundFile,
    ) -> Result<(), TypeScriptCollectError> {
        let arena = &bound_file.arena;
        let mut callables = Vec::new();
        for (raw_node, node) in arena.nodes.iter().enumerate() {
            let expression_callable = node.kind == TszSyntaxKind::ARROW_FUNCTION
                || node.kind == TszSyntaxKind::FUNCTION_EXPRESSION;
            let function_type = node.kind == TszSyntaxKind::FUNCTION_TYPE
                || node.kind == TszSyntaxKind::CONSTRUCTOR_TYPE;
            let call_signature = node.kind == TszSyntaxKind::CALL_SIGNATURE
                || node.kind == TszSyntaxKind::CONSTRUCT_SIGNATURE;
            let object_method = node.kind == TszSyntaxKind::METHOD_DECLARATION;
            if !expression_callable && !function_type && !call_signature && !object_method {
                continue;
            }
            let Ok(raw_node) = u32::try_from(raw_node) else {
                continue;
            };
            let callable = TszNodeIndex(raw_node);
            let step = if object_method {
                native_tsz_object_method_step(self.source, arena, callable)
            } else if expression_callable {
                let Some(function) = arena.get_function_at(callable) else {
                    continue;
                };
                if !function.name.is_none() {
                    continue;
                }
                native_tsz_expression_parent_step(self.source, arena, callable)
            } else {
                if function_type {
                    if arena.get_function_type_at(callable).is_none() {
                        continue;
                    }
                    native_tsz_function_type_parent_step(self.source, arena, callable)
                } else {
                    if arena.get_signature_at(callable).is_none() {
                        continue;
                    }
                    native_tsz_call_signature_parent_step(arena, callable)
                }
            };
            let Some((start, end)) = arena.pos_end_at(callable) else {
                continue;
            };
            if start >= end || self.source.get(start as usize..end as usize).is_none() {
                continue;
            }
            let Some(step) = step else {
                continue;
            };
            callables.push(NativeAnonymousCallable {
                span: Span::new(start, end),
                step,
                scope: None,
            });
        }
        if callables.is_empty() {
            return Ok(());
        }

        let mut events = Vec::with_capacity(self.facts.len + callables.len());
        for ordinal in 0..self.facts.len {
            if !ts_lexical_owner(
                self.fact_kinds
                    .get(ordinal)
                    .copied()
                    .unwrap_or(EntityKind::Parameter),
            ) {
                continue;
            }
            let (Some(&start), Some(&end)) =
                (self.decl_starts.get(ordinal), self.decl_ends.get(ordinal))
            else {
                continue;
            };
            if let Some(parts) = self.merged_declaration_parts.get(&coordinate(ordinal)?) {
                events.extend(parts.iter().map(|part| NativeCallableScopeEvent::Named {
                    start: part.start, end: part.end, ordinal,
                }));
            } else if start != UNSET && start < end {
                events.push(NativeCallableScopeEvent::Named { start, end, ordinal });
            }
        }
        for (index, callable) in callables.iter().enumerate() {
            events.push(NativeCallableScopeEvent::Anonymous {
                start: callable.span.start,
                end: callable.span.end,
                index,
            });
        }
        events.sort_unstable_by_key(|event| match event {
            NativeCallableScopeEvent::Named { start, end, .. } => {
                (*start, std::cmp::Reverse(*end), 0_u8)
            }
            NativeCallableScopeEvent::Anonymous { start, end, .. } => {
                (*start, std::cmp::Reverse(*end), 1_u8)
            }
        });

        let mut root_scope = Sha256::new();
        root_scope.update(b"compiler.typescript.anonymous-scope.v1\0");
        let mut scope_stack = Vec::<(u32, Sha256, bool)>::new();
        let mut family_counts = HashMap::<(CallableAnchorStep<'source>, [u8; 32]), u32>::new();
        for event in events {
            let (start, end) = match event {
                NativeCallableScopeEvent::Named { start, end, .. }
                | NativeCallableScopeEvent::Anonymous { start, end, .. } => (start, end),
            };
            while scope_stack
                .last()
                .is_some_and(|(outer_end, _, _)| *outer_end <= start)
            {
                scope_stack.pop();
            }
            let (mut scope, stable_scope) = scope_stack.last().map_or_else(
                || (root_scope.clone(), true),
                |(_, hash, stable)| (hash.clone(), *stable),
            );
            match event {
                NativeCallableScopeEvent::Named { ordinal, .. } => {
                    scope.update(b"\0named\0");
                    let kind = self
                        .fact_kinds
                        .get(ordinal)
                        .copied()
                        .unwrap_or(EntityKind::Parameter);
                    scope.update(u16::from(kind).to_le_bytes());
                    let name_start = self.name_starts.get(ordinal).copied().unwrap_or(UNSET);
                    let name_end = self.name_ends.get(ordinal).copied().unwrap_or(UNSET);
                    let has_name = name_start != UNSET
                        && name_start < name_end
                        && self
                            .source
                            .get(name_start as usize..name_end as usize)
                            .is_some_and(|name| !name.is_empty());
                    if has_name {
                        let Some(name) = self.source.get(name_start as usize..name_end as usize)
                        else {
                            continue;
                        };
                        scope.update(u32::try_from(name.len()).unwrap_or(u32::MAX).to_le_bytes());
                        scope.update(name.as_bytes());
                    } else {
                        scope.update(b"\0unnameable-owner\0");
                    }
                    scope_stack.push((end, scope, stable_scope && has_name));
                }
                NativeCallableScopeEvent::Anonymous { index, .. } => {
                    let Some(callable) = callables.get_mut(index) else {
                        continue;
                    };
                    if stable_scope {
                        let scope_id: [u8; 32] = scope.clone().finalize().into();
                        callable.scope = Some(scope_id);
                        let count = family_counts.entry((callable.step, scope_id)).or_insert(0);
                        *count = count.saturating_add(1);
                    }
                    // Descendants inherit the parent family route, never its
                    // body span. Exact site coordinates are attached only to
                    // the current instance after its durable family is built.
                    scope.update(b"\0anonymous-family\0");
                    hash_callable_anchor_step(&mut scope, callable.step);
                    scope_stack.push((end, scope, stable_scope));
                }
            }
        }

        for callable in callables {
            let Some(scope) = callable.scope else {
                continue;
            };
            let count = family_counts
                .get(&(callable.step, scope))
                .copied()
                .unwrap_or(1);
            let multiplicity =
                AnonymousCallableFamilyMultiplicity::new(count).ok_or_else(lane_rejection)?;
            self.pending_type_parameters = coordinate(self.facts.type_parameter_len)?;
            let fact = SemanticFact::new(EntityKind::Function, &[], LEAF_PRODUCT)
                .anonymous_callable_step(callable.step, multiplicity);
            let ordinal = self.push(fact)?;
            let index = usize::try_from(ordinal).map_err(|_| lane_rejection())?;
            let staged = StagedSourceSpan::new(callable.span.start, callable.span.end).ok_or(
                TypeScriptCollectError::Span {
                    start: callable.span.start,
                    end: callable.span.end,
                },
            )?;
            self.facts
                .attach_source_span(ordinal, staged)
                .map_err(fault)?;
            *self.facts.publication_roles.get_mut(index).ok_or_else(lane_rejection)? =
                backend_semantic::ir::DeclarationPublicationRole::PrimaryOwned;
            *self.decl_starts.get_mut(index).ok_or_else(lane_rejection)? = callable.span.start;
            *self.decl_ends.get_mut(index).ok_or_else(lane_rejection)? = callable.span.end;
            *self.fact_kinds.get_mut(index).ok_or_else(lane_rejection)? = EntityKind::Function;
        }
        Ok(())
    }

    /// Adds occurrence facts resolved by the exact borrowed TSZ program.
    /// OXC's source occurrences remain the fallback for unsupported syntax;
    /// only a binder/checker-resolved member or identifier with a unique
    /// stable declaration coordinate upgrades that existing site.
    fn pass_native_tsz_occurrences(
        &mut self,
        checker: &mut TszCheckerState<'_>,
        binder: &backend_frontend_typescript::TszBinderState,
        bound_file: &TszBoundFile,
        session: &backend_frontend_typescript::TszProjectQuerySession<'_>,
        current_file_index: usize,
        program_identity: Option<[u8; 32]>,
    ) -> Result<(), TypeScriptCollectError> {
        let program_files = &session.project().program().files;
        let current_file_index = u32::try_from(current_file_index).map_err(|_| lane_rejection())?;
        if program_files
            .get(current_file_index as usize)
            .map(|file| file.file_name.as_str())
            != Some(bound_file.file_name.as_str())
        {
            return Ok(());
        }
        let arena = &bound_file.arena;
        // The syntax pass retains import placeholders until the admitted
        // project can resolve them. Upgrade both the written alias site and
        // its bare references, including type/value reads outside a call.
        // An exact existing import occurrence is required; unrelated same-name
        // identifiers cannot acquire another module's declaration authority.
        let mut wanted = std::collections::HashSet::new();
        for index in 0..self.facts.occurrence_len {
            let occurrence = self.facts.occurrences[index];
            if occurrence.confidence != OccurrenceConfidence::Import { continue; }
            let owner = self.facts.occurrence_owners[index];
            let Some(base) = self.decl_starts.get(owner as usize).copied() else { continue; };
            if let (Some(start), Some(end)) = (
                base.checked_add(occurrence.span.start), base.checked_add(occurrence.span.end),
            ) { wanted.insert((start, end)); }
        }
        let mut identifiers = HashMap::<(u32, u32), Option<TszNodeIndex>>::new();
        for (raw_node, _) in arena.nodes.iter().enumerate().take_while(|_| !wanted.is_empty()) {
            let Ok(raw_node) = u32::try_from(raw_node) else { continue; };
            let identifier = TszNodeIndex(raw_node);
            if arena.get_identifier_at(identifier).is_none() { continue; }
            let Some((start, end)) = arena.pos_end_at(identifier) else { continue; };
            if !wanted.contains(&(start, end)) { continue; }
            identifiers.entry((start, end)).and_modify(|previous| {
                if *previous != Some(identifier) { *previous = None; }
            }).or_insert(Some(identifier));
        }
        for index in 0..self.facts.occurrence_len {
            let occurrence = self.facts.occurrences[index];
            if occurrence.confidence != OccurrenceConfidence::Import { continue; }
            let owner = self.facts.occurrence_owners[index];
            let Some(base) = self.decl_starts.get(owner as usize).copied() else { continue; };
            let (Some(start), Some(end)) = (
                base.checked_add(occurrence.span.start), base.checked_add(occurrence.span.end),
            ) else { continue; };
            let Some(&Some(identifier)) = identifiers.get(&(start, end)) else { continue; };
            let Some(resolved) = checker.resolve_project_identifier_with_owner(identifier) else {
                continue;
            };
            let Some(target_binder) = checker.ctx.get_binder_for_file(resolved.owner_file_idx) else {
                continue;
            };
            let Some(target) = native_tsz_import_declaration_coordinate(
                target_binder, resolved.symbol_id, session,
            ) else { continue; };
            self.commit_native_tsz_occurrence(
                owner, Span::new(start, end), occurrence.kind, target,
                current_file_index, program_identity,
            )?;
        }
        let mut called_member_accesses = std::collections::HashSet::<u32>::new();
        for (raw_node, node) in arena.nodes.iter().enumerate() {
            if node.kind != TszSyntaxKind::CALL_EXPRESSION {
                continue;
            }
            let Ok(raw_node) = u32::try_from(raw_node) else {
                continue;
            };
            let call_node = TszNodeIndex(raw_node);
            let Some(call) = arena.get_call_expr_at(call_node) else {
                continue;
            };
            let Some(callee_node) = arena.get(call.expression) else {
                continue;
            };
            let (site_node, target_symbol, target_owner, reference_kind) = if callee_node.kind
                == TszSyntaxKind::PROPERTY_ACCESS_EXPRESSION
            {
                let Some(access) = arena.get_access_expr_at(call.expression) else {
                    continue;
                };
                if arena.get_identifier_at(access.name_or_argument).is_none() {
                    continue;
                }
                called_member_accesses.insert(call.expression.0);
                let Some((target, target_owner)) = native_tsz_member_symbol(
                    checker,
                    binder,
                    session.project(),
                    arena,
                    access.expression,
                    access.name_or_argument,
                ) else {
                    continue;
                };
                (
                    access.name_or_argument,
                    target,
                    target_owner,
                    ReferenceKind::MethodCall,
                )
            } else if arena.get_identifier_at(call.expression).is_some() {
                let Some(target) = checker.resolve_project_identifier_with_owner(call.expression)
                else {
                    continue;
                };
                (
                    call.expression,
                    target.symbol_id,
                    Some(target.owner_file_idx),
                    ReferenceKind::FunctionCall,
                )
            } else {
                continue;
            };
            let Some((site_start, site_end)) = arena.pos_end_at(site_node) else {
                continue;
            };
            let Some(owner) = self.owning_fact(site_start) else {
                continue;
            };
            let target_binder = match target_owner {
                Some(owner) => checker.ctx.get_binder_for_file(owner),
                None => Some(binder),
            };
            let Some(target_binder) = target_binder else {
                continue;
            };
            // A called namespace export can be an authored const/let value,
            // not a function declaration. Keep callable/overload selection
            // first; only an exact project owner may supply the unique
            // binding coordinate when that callable-only selector has none.
            let target =
                native_tsz_declaration_coordinate(target_binder, target_symbol, session, true)
                    .or_else(|| {
                        target_owner.and_then(|_| {
                            native_tsz_callable_binding_coordinate(
                                target_binder,
                                target_symbol,
                                session,
                            )
                        })
                    });
            let Some(target) = target else {
                continue;
            };
            self.commit_native_tsz_occurrence(
                owner,
                Span::new(site_start, site_end),
                reference_kind,
                target,
                current_file_index,
                program_identity,
            )?;
        }

        // Property-name occurrences are emitted independently of calls, so
        // method and field reads share the exact same receiver-owned lookup.
        for (raw_node, node) in arena.nodes.iter().enumerate() {
            if node.kind != TszSyntaxKind::PROPERTY_ACCESS_EXPRESSION {
                continue;
            }
            let Ok(raw_node) = u32::try_from(raw_node) else {
                continue;
            };
            if called_member_accesses.contains(&raw_node) {
                continue;
            }
            let access_node = TszNodeIndex(raw_node);
            let Some(access) = arena.get_access_expr_at(access_node) else {
                continue;
            };
            if arena.get_identifier_at(access.name_or_argument).is_none() {
                continue;
            }
            let Some((target_symbol, target_owner)) = native_tsz_member_symbol(
                checker,
                binder,
                session.project(),
                arena,
                access.expression,
                access.name_or_argument,
            ) else {
                continue;
            };
            let Some((site_start, site_end)) = arena.pos_end_at(access.name_or_argument) else {
                continue;
            };
            let Some(owner) = self.owning_fact(site_start) else {
                continue;
            };
            let target_binder = match target_owner {
                Some(owner) => checker.ctx.get_binder_for_file(owner),
                None => Some(binder),
            };
            let Some(target_binder) = target_binder else { continue; };
            let target = if target_owner.is_some() {
                native_tsz_import_declaration_coordinate(target_binder, target_symbol, session)
            } else {
                native_tsz_declaration_coordinate(target_binder, target_symbol, session, false)
            };
            let Some(target) = target
            else {
                continue;
            };
            self.commit_native_tsz_occurrence(
                owner,
                Span::new(site_start, site_end),
                ReferenceKind::FieldAccess,
                target,
                current_file_index,
                program_identity,
            )?;
        }
        Ok(())
    }

    /// Replaces an OXC name/import placeholder with one exact TSZ target.
    /// Same-file declarations bind directly to their registered fact;
    /// cross-file targets retain a typed source/program coordinate in the
    /// existing foreign-key operand until the common query join admits it.
    fn commit_native_tsz_occurrence(
        &mut self,
        owner: u32,
        span: Span,
        reference_kind: ReferenceKind,
        target: NativeTszDeclarationCoordinate,
        current_file_index: u32,
        program_identity: Option<[u8; 32]>,
    ) -> Result<(), TypeScriptCollectError> {
        if span.start >= span.end
            || self
                .source
                .get(span.start as usize..span.end as usize)
                .is_none()
        {
            return Ok(());
        }
        let occurrence_index = self.occurrence_index(Utf8Span {
            start: span.start,
            end: span.end,
        })?;
        if !self.declaration_contains(owner, span) {
            return Ok(());
        }
        let owner_index = usize::try_from(owner).map_err(|_| lane_rejection())?;
        let Some(owner_start) = self.decl_starts.get(owner_index).copied() else {
            return Ok(());
        };
        let (Some(relative_start), Some(relative_end)) = (
            span.start.checked_sub(owner_start),
            span.end.checked_sub(owner_start),
        ) else {
            return Ok(());
        };
        let relative = RelSpan::new(relative_start, relative_end).map_err(|_| {
            TypeScriptCollectError::Span {
                start: relative_start,
                end: relative_end,
            }
        })?;
        let confidence = OccurrenceConfidence::Oracle;
        if target.file_index == current_file_index {
            let Some(local) = self.fact_at_name_start(target.name_start) else {
                return Ok(());
            };
            let local_index = usize::try_from(local).map_err(|_| lane_rejection())?;
            if self.fact_kinds.get(local_index) != Some(&target.kind) {
                return Ok(());
            }
            let occurrence = Occurrence {
                target: OccurrenceTarget::Local(EntityId::new(local)),
                kind: reference_kind,
                confidence,
                span: relative,
            };
            if let Some(index) = occurrence_index {
                self.facts
                    .replace_occurrence(index, owner, occurrence)
                    .map_err(fault)?;
            } else {
                self.facts
                    .push_occurrence(owner, occurrence)
                    .map_err(fault)?;
            }
            return Ok(());
        }
        let Some(program) = program_identity else {
            return Ok(());
        };
        let Some(encoded) = (TypeScriptSourceCoordinate {
            program,
            source: target.source_identity,
            path: &target.path,
            declaration_start: target.declaration_start,
            declaration_end: target.declaration_end,
            name_start: target.name_start,
        })
        .encode() else {
            return Ok(());
        };
        let Some(display) = self.text_span(span) else {
            return Ok(());
        };
        if let Some(index) = occurrence_index {
            self.facts
                .replace_with_owned_tsz_source_occurrence(
                    index,
                    owner,
                    encoded,
                    display,
                    target.kind,
                    reference_kind,
                    confidence,
                    relative,
                )
                .map_err(fault)?;
        } else {
            self.facts
                .push_owned_tsz_source_occurrence(
                    owner,
                    encoded,
                    display,
                    target.kind,
                    reference_kind,
                    confidence,
                    relative,
                )
                .map_err(fault)?;
        }
        Ok(())
    }

    /// Pass three for the native authority: resolve each exact source name
    /// span to the TSZ binder symbol, ask the in-process checker for its
    /// `TypeId`, and map that live structural type directly into the existing
    /// schema-2 computed lane.
    fn pass_native_tsz(
        &mut self,
        checker: &mut TszCheckerState<'_>,
        bound_file: &TszBoundFile,
        database: &dyn TszTypeDatabase,
        project: &'source TszProject,
    ) -> Result<(), TypeScriptCollectError> {
        let mut symbols_by_span = HashMap::<(u32, u32), TszSymbolId>::new();
        let mut ambiguous_spans = std::collections::HashSet::new();
        for (&raw_node, &symbol) in bound_file.node_symbols.iter() {
            let declaration = TszNodeIndex(raw_node);
            // TSZ binds type-alias symbols to the whole TypeAliasDeclaration
            // node, while the existing IR owns the identifier span. Project
            // that binder identity through the alias's native name node so
            // checker facts join by the exact declaration name, just like
            // variable/function bindings. The outer declaration span is not
            // an acceptable substitute for an owner-name match.
            let symbol_node = bound_file
                .arena
                .get(declaration)
                .and_then(|node| {
                    bound_file.arena.get_type_alias(node).map(|alias| alias.name)
                        .or_else(|| bound_file.arena.get_binding_element(node).map(|binding| binding.name))
                })
                .unwrap_or(declaration);
            let Some((start, end)) = bound_file.arena.pos_end_at(symbol_node) else {
                continue;
            };
            let (Ok(start_index), Ok(end_index)) = (usize::try_from(start), usize::try_from(end))
            else {
                continue;
            };
            if start >= end || self.source.get(start_index..end_index).is_none() {
                // TSZ source offsets are admitted only when they project to
                // this exact UTF-8 buffer; no UTF-16 or lossy coordinate
                // conversion is allowed in the checker-to-owner join.
                continue;
            }
            if ambiguous_spans.contains(&(start, end)) {
                continue;
            }
            match symbols_by_span.entry((start, end)) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(symbol);
                }
                std::collections::hash_map::Entry::Occupied(entry) if *entry.get() != symbol => {
                    symbols_by_span.remove(&(start, end));
                    ambiguous_spans.insert((start, end));
                }
                std::collections::hash_map::Entry::Occupied(_) => {}
            }
        }

        // `TypeQuery(SymbolRef)` can be represented only when the exact TSZ
        // symbol binds to one entity row emitted from this source. Symbols
        // with multiple local declaration facts (for example overloads) are
        // deliberately removed from this inverse map instead of choosing an
        // arbitrary row.
        let fact_len = u32::try_from(self.facts.len()).map_err(|_| lane_rejection())?;
        let mut native_tsz_entities = HashMap::<TszSymbolId, u32>::new();
        let mut ambiguous_symbols = std::collections::HashSet::new();
        for fact_index in 0..self.facts.len() {
            if self.fact_kinds.get(fact_index) == Some(&EntityKind::Reexport) {
                continue;
            }
            let (Some(&name_start), Some(&name_end)) = (
                self.name_starts.get(fact_index),
                self.name_ends.get(fact_index),
            ) else {
                continue;
            };
            if name_start == UNSET || name_end == UNSET {
                continue;
            }
            let Some(&symbol) = symbols_by_span.get(&(name_start, name_end)) else {
                continue;
            };
            if ambiguous_symbols.contains(&symbol) {
                continue;
            }
            let Ok(fact) = u32::try_from(fact_index) else {
                return Err(lane_rejection());
            };
            match native_tsz_entities.entry(symbol) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(fact);
                }
                std::collections::hash_map::Entry::Occupied(entry) if *entry.get() != fact => {
                    native_tsz_entities.remove(&symbol);
                    ambiguous_symbols.insert(symbol);
                }
                std::collections::hash_map::Entry::Occupied(_) => {}
            }
        }
        let registry = FactRegistry {
            source: self.source,
            tsz_project: Some(project),
            source_path: Some(bound_file.file_name.clone()),
            decl_starts: &self.decl_starts,
            decl_ends: &self.decl_ends,
            declaration_parts: &self.merged_declaration_parts,
            name_starts: &self.name_starts,
            name_start_index: index_name_starts(&self.name_starts, fact_len),
            name_ends: &self.name_ends,
            fact_kinds: &self.fact_kinds,
            native_tsz_entities: Some(&native_tsz_entities),
            fact_len,
        };

        let fact_count = self.facts.len();
        for fact_index in 0..fact_count {
            let Some(&name_start) = self.name_starts.get(fact_index) else {
                continue;
            };
            let Some(&name_end) = self.name_ends.get(fact_index) else {
                continue;
            };
            if name_start == UNSET || name_end == UNSET {
                continue;
            }
            let Some(owner) = registry.fact_at_name_start(name_start) else {
                continue;
            };
            if owner != fact_index as u32
                || registry.fact_kinds.get(fact_index) == Some(&EntityKind::Reexport)
            {
                continue;
            }
            let Some(symbol) = symbols_by_span.get(&(name_start, name_end)).copied() else {
                continue;
            };
            let native_type = checker.get_type_of_symbol(symbol);
            let mut active = std::collections::HashSet::new();
            let row = intern_native_tsz_type(
                &registry,
                self.facts,
                checker,
                database,
                native_type,
                owner,
                0,
                &mut active,
            )?;
            let owner_index = usize::try_from(owner).map_err(|_| lane_rejection())?;
            let type_parameters = self
                .extension_type_parameters
                .get(owner_index)
                .copied()
                .ok_or_else(lane_rejection)?;
            let extension = EmissionExtension::TypeScript(backend_semantic::ir::TypeScriptFacts {
                type_parameters: TypeParameterListId::new(type_parameters),
                declared: Some(TypeId::new(owner)),
                observed: Some(TypeId::new(row)),
            });
            self.facts
                .attach_extension_with_type_parameters(
                    owner_index,
                    extension,
                    self.facts
                        .captured_type_parameter_range(owner_index)
                        .map_err(fault)?,
                )
                .map_err(fault)?;
        }
        Ok(())
    }

    /// Pass four: the checker's assignment narrowings. Every reported
    /// narrowing whose declaration name binds to a published fact receives
    /// one extra owned computed row in the anonymous pool — the
    /// control-flow-sensitive type at that exact assignment site, a fact
    /// the syntax plane cannot see. The extension's observed cell keeps
    /// the declaration-time type; narrowing rows carry no computed-cell
    /// proof because they extend rather than replace it.
    fn pass_narrowings(&mut self) -> Result<(), TypeScriptCollectError> {
        let Some(checker) = self.checker.as_ref() else {
            return Ok(());
        };
        let narrowings: Vec<_> = checker.narrowings().copied().collect();
        let fact_len = u32::try_from(self.facts.len()).map_err(|_| lane_rejection())?;
        let registry = FactRegistry {
            source: self.source,
            tsz_project: None,
            source_path: None,
            decl_starts: &self.decl_starts,
            decl_ends: &self.decl_ends,
            declaration_parts: &self.merged_declaration_parts,
            name_starts: &self.name_starts,
            name_start_index: index_name_starts(&self.name_starts, fact_len),
            name_ends: &self.name_ends,
            fact_kinds: &self.fact_kinds,
            native_tsz_entities: None,
            fact_len,
        };
        for narrowing in narrowings {
            let Some(tree) = narrowing.r#type else {
                continue;
            };
            let Some(owner) = registry.fact_at_name_start(narrowing.name.start) else {
                // The checker narrowed a declaration the lane does not
                // publish; there is no honest owner for the row.
                continue;
            };
            let owner_index = usize::try_from(owner).map_err(|_| lane_rejection())?;
            if registry.fact_kinds.get(owner_index) == Some(&EntityKind::Reexport) {
                continue;
            }
            let site = SpellDomain::Range(narrowing.site.start, narrowing.site.end);
            match intern_computed_tree(&registry, self.facts, tree, owner, 0, site) {
                Ok(_) => {}
                Err(TypeScriptCollectError::Rejected(FactRejection {
                    cause: FactFault::ComputedRowCapacity,
                    ..
                })) => break,
                Err(cause) => return Err(cause),
            }
        }
        Ok(())
    }

    /// Pass five: every OXC reference becomes one occurrence fact owned by
    /// the innermost pushed declaration containing it. Resolved in-file
    /// targets carry [`OccurrenceConfidence::Index`]; import-resolved targets
    /// carry their npm foreign key at [`OccurrenceConfidence::Import`];
    /// unresolved names stay [`OccurrenceConfidence::Syntactic`] against the
    /// npm universe. When the checker authority ran, a checker-resolved
    /// same-file reference upgrades to [`OccurrenceConfidence::Oracle`] and
    /// targets the exact overload member the checker picked at a call site,
    /// and a checker-resolved foreign base upgrades to an oracle-confident
    /// foreign key naming the resolved package when this source spells it.
    fn pass_references(&mut self) -> Result<(), TypeScriptCollectError> {
        self.build_owner_index();
        let scoping = self.semantic.scoping();
        let nodes = self.semantic.nodes();
        for symbol in scoping.symbol_ids() {
            let symbol_flags = scoping.symbol_flags(symbol);
            for reference_id in scoping.get_resolved_reference_ids(symbol) {
                let reference = scoping.get_reference(*reference_id);
                let span = nodes.get_node(reference.node_id()).kind().span();
                let Some(owner) = self.owning_fact(span.start) else {
                    continue;
                };
                self.push_reference(owner, span, reference.flags(), Some((symbol, symbol_flags)))?;
            }
        }
        for group in scoping.root_unresolved_references_ids() {
            for reference_id in group {
                let reference = scoping.get_reference(reference_id);
                let span = nodes.get_node(reference.node_id()).kind().span();
                let Some(owner) = self.owning_fact(span.start) else {
                    continue;
                };
                self.push_reference(owner, span, reference.flags(), None)?;
            }
        }
        Ok(())
    }

    /// Commits one reference fact, resolving its target through OXC's symbol
    /// when one exists and its category from the reference flags, call
    /// position, and—when the resolved symbol is a function declaration or
    /// expression—value use that is neither a type query nor write-only.
    fn push_reference(
        &mut self,
        owner: u32,
        span: Span,
        flags: ReferenceFlags,
        symbol: Option<(SymbolId, SymbolFlags)>,
    ) -> Result<(), TypeScriptCollectError> {
        let kind = if flags.is_type() {
            ReferenceKind::TypeReference
        } else if self.is_call_position(span) {
            ReferenceKind::FunctionCall
        } else if let Some((symbol, symbol_flags)) = symbol
            && !flags.is_value_as_type()
            && !flags.is_write_only()
            && (symbol_flags.is_function() || self.is_const_callable_value(symbol, symbol_flags))
        {
            ReferenceKind::FunctionCall
        } else {
            ReferenceKind::VariableUse
        };
        let (target, confidence) = match symbol {
            Some((symbol, symbol_flags)) => {
                let scoping = self.semantic.scoping();
                let binding_start = scoping.symbol_span(symbol).start;
                match self.fact_at_name_start(binding_start) {
                    Some(fact) => {
                        if symbol_flags.intersects(SymbolFlags::Import | SymbolFlags::TypeImport) {
                            (self.import_target(fact)?, OccurrenceConfidence::Import)
                        } else if let Some(upgraded) =
                            self.oracle_upgrade(fact, binding_start, span)?
                        {
                            upgraded
                        } else {
                            (
                                OccurrenceTarget::Local(EntityId::new(fact)),
                                OccurrenceConfidence::Index,
                            )
                        }
                    }
                    // Resolved in-file but outside the published set: there
                    // is no honest target to name.
                    None => return Ok(()),
                }
            }
            None => {
                if let Some(upgraded) = self.checker_resolved_unresolved(span)? {
                    upgraded
                } else {
                    // A syntactic foreign target remains authority-backed by
                    // its exact source span.  Do not turn a failed source
                    // projection into an empty key: that would certify a
                    // different external declaration.
                    let name = self.text_span(span).ok_or(TypeScriptCollectError::Span {
                        start: span.start,
                        end: span.end,
                    })?;
                    let key = ForeignKey::new(
                        ForeignOrigin::Universe {
                            ecosystem: NPM_ECOSYSTEM,
                        },
                        name,
                        name,
                        None,
                    )
                    .map_err(|cause| foreign_fault(cause, span))?;
                    (
                        OccurrenceTarget::Foreign(key),
                        OccurrenceConfidence::Syntactic,
                    )
                }
            }
        };
        self.commit_occurrence(owner, span, kind, target, confidence)
    }

    /// Commits one occurrence fact owned by `owner`, projecting the
    /// absolute source span onto the owner-relative wire span. A span that
    /// does not sit inside its owner is silently absent rather than
    /// misattributed: image construction rechecks that the absolute range
    /// fits one exact producer-proven declaration part, never a family gap.
    fn commit_occurrence(
        &mut self,
        owner: u32,
        span: Span,
        kind: ReferenceKind,
        target: OccurrenceTarget<'source>,
        confidence: OccurrenceConfidence,
    ) -> Result<(), TypeScriptCollectError> {
        if !self.declaration_contains(owner, span) {
            return Ok(());
        }
        let owner_index = usize::try_from(owner).map_err(|_| lane_rejection())?;
        let Some(owner_start) = self.decl_starts.get(owner_index).copied() else {
            return Ok(());
        };
        let (Some(relative_start), Some(relative_end)) = (
            span.start.checked_sub(owner_start),
            span.end.checked_sub(owner_start),
        ) else {
            return Ok(());
        };
        // The exact declaration-part containment above is rechecked against
        // producer provenance during image construction.
        let relative = RelSpan::new(relative_start, relative_end).map_err(|_| {
            TypeScriptCollectError::Span {
                start: relative_start,
                end: relative_end,
            }
        })?;
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
            .map_err(fault)?;
        Ok(())
    }

    /// Upgrades one OXC-unresolved reference when the checker authority
    /// resolved it: a same-file target becomes a [`Local`] occurrence at
    /// oracle confidence, and a checker-resolved foreign module origin
    /// becomes an oracle-confident foreign key naming the exact package and
    /// display symbol. Every wire cell stays source-backed: the package
    /// lineage and the display spelling are borrowed from this source's own
    /// bytes (the module specifier and the checker-reported name must be
    /// spelled here); a module this source never spells keeps the honest
    /// npm-universe key, still at oracle confidence because the checker
    /// proved the resolution.
    fn checker_resolved_unresolved(
        &self,
        span: Span,
    ) -> Result<Option<(OccurrenceTarget<'source>, OccurrenceConfidence)>, TypeScriptCollectError>
    {
        let Some(checker) = self.checker.as_ref() else {
            return Ok(None);
        };
        let resolved = checker.reference_at(Utf8Span {
            start: span.start,
            end: span.end,
        });
        let Some(resolved) = resolved else {
            return Ok(None);
        };
        if let Some(target) = resolved.target {
            if let Some(fact) = self.fact_at_name_start(target.start) {
                return Ok(Some((
                    OccurrenceTarget::Local(EntityId::new(fact)),
                    OccurrenceConfidence::Oracle,
                )));
            }
            return Ok(None);
        }
        let Some(module) = resolved.module else {
            return Ok(None);
        };
        if let Some(key) = self.checker_foreign_key(span, module, resolved.name, None)? {
            return Ok(Some((
                OccurrenceTarget::Foreign(key),
                OccurrenceConfidence::Oracle,
            )));
        }
        Ok(None)
    }

    /// Borrows the exact source bytes one checker-reported spelling names in
    /// this source, when the source spells that text anywhere. A
    /// checker-reported string can never enter a wire cell directly — every
    /// borrowed cell stays source-backed — so this is the only lawful bridge.
    fn spelled_in_source(&self, name: &[u8]) -> Option<&'source str> {
        let bytes = self.source.as_bytes();
        let at = find_sub(bytes, name, 0)?;
        let end = at.checked_add(name.len())?;
        core::str::from_utf8(bytes.get(at..end)?).ok()
    }

    /// Builds one source-backed foreign key for a checker-resolved foreign
    /// reference: the module names the package lineage (through the shared
    /// import-specifier grammar) and the use-site spelling names the path,
    /// with the checker-reported foreign declaration name as the display
    /// when this source spells it. `None` keeps the honest npm-universe key
    /// when the module is not spelled here; the caller's fallback owns it.
    fn checker_foreign_key(
        &self,
        span: Span,
        module: &str,
        name: Option<&str>,
        member_kind: Option<EntityKind>,
    ) -> Result<Option<ForeignKey<'source>>, TypeScriptCollectError> {
        let spelled_module = self.spelled_in_source(module.as_bytes());
        let site = self.text_span(span).ok_or(TypeScriptCollectError::Span {
            start: span.start,
            end: span.end,
        })?;
        if site.is_empty() {
            return Ok(None);
        }
        let (path, display, kind) = match member_kind {
            Some(member_kind) => {
                let Some(name) = name else {
                    return Ok(None);
                };
                let Some(path) = self
                    .spelled_in_source(name.as_bytes())
                    .filter(|path| !path.is_empty())
                else {
                    return Ok(None);
                };
                (path, path, Some(member_kind))
            }
            None => {
                let display = name
                    .and_then(|name| self.spelled_in_source(name.as_bytes()))
                    .unwrap_or(site);
                (site, display, None)
            }
        };
        let origin = match spelled_module {
            Some(module) => ForeignOrigin::Package(
                package_lineage_for_module(module).map_err(|cause| lineage_fault(cause, span))?,
            ),
            None => ForeignOrigin::Universe {
                ecosystem: NPM_ECOSYSTEM,
            },
        };
        ForeignKey::new(origin, path, display, kind)
            .map(Some)
            .map_err(|cause| foreign_fault(cause, span))
    }

    /// Pass six: the checker's references that OXC never visits — property
    /// accesses and other member sites OXC does not bind. Every published
    /// same-file target commits a Local occurrence at oracle confidence
    /// (a call site targets the exact overload member the checker picked);
    /// a checker-resolved foreign base commits its oracle-confident foreign
    /// key naming the resolved package. Every other site stays absent rather
    /// than guessed, and a span already covered by an OXC reference is never
    /// committed twice.
    fn pass_checker_references(&mut self) -> Result<(), TypeScriptCollectError> {
        let Some(checker) = self.checker.as_ref() else {
            return Ok(());
        };
        // Bound reference rows are `Copy`; the loop body mutates the lane,
        // so the iteration set is copied out of the borrowed report first.
        let references: Vec<_> = checker.references().copied().collect();
        for reference in references {
            if self.occurrence_covers(reference.span)? {
                continue;
            }
            let span = Span::new(reference.span.start, reference.span.end);
            if reference.is_enum_member
                && reference.overload_index.is_none()
                && !self.is_member_call_position(span)
            {
                continue;
            }
            let Some(mut owner) = self.owning_fact(reference.span.start) else {
                continue;
            };
            let Some((target, confidence, kind)) = self.checker_only_target(reference)? else {
                continue;
            };
            let retarget_to_enclosing_function = match (&kind, &target) {
                (
                    ReferenceKind::FieldAccess,
                    OccurrenceTarget::Foreign(ForeignKey {
                        kind: Some(EntityKind::Field),
                        ..
                    }),
                ) => true,
                (
                    ReferenceKind::VariableUse,
                    OccurrenceTarget::Foreign(ForeignKey {
                        kind: Some(EntityKind::Constant | EntityKind::Static | EntityKind::Function),
                        ..
                    }),
                ) => true,
                (
                    ReferenceKind::FunctionCall,
                    OccurrenceTarget::Foreign(ForeignKey { kind: None, .. }),
                ) => true,
                _ => false,
            };
            if retarget_to_enclosing_function
                && let Some(function) = self.enclosing_function_owner(reference.span.start)
            {
                owner = function;
            }
            self.commit_occurrence(owner, span, kind, target, confidence)?;
        }
        Ok(())
    }

    /// Pass six-and-a-half: every `Enum.Member` value use whose object is a
    /// resolved enum binding becomes one `FieldAccess` occurrence on the
    /// member token, targeting the variant declared inside that enum.
    /// Checker-only and OXC reference rows that already name the member span
    /// are left alone; every other static member site stays absent.
    fn pass_enum_member_accesses(&mut self) -> Result<(), TypeScriptCollectError> {
        let nodes = self.semantic.nodes();
        for node in nodes.iter() {
            let kind = node.kind();
            let Some(member) = kind.as_static_member_expression() else {
                continue;
            };
            let property_span = member.property.span();
            if self.occurrence_covers(Utf8Span {
                start: property_span.start,
                end: property_span.end,
            })? {
                continue;
            }
            let Some(owner) = self.owning_fact(property_span.start) else {
                continue;
            };
            let object_span = member.object.span();
            let property_bytes =
                self.slice_span(property_span)
                    .ok_or(TypeScriptCollectError::Span {
                        start: property_span.start,
                        end: property_span.end,
                    })?;
            if let Some(identifier_span) =
                self.peel_object_identifier_span(object_span.start, object_span.end)
            {
                if let Some(enum_fact) = self.enum_fact_for_identifier_span(identifier_span) {
                    let Some(variant) = self.variant_in_enum(enum_fact, property_bytes) else {
                        continue;
                    };
                    self.commit_occurrence(
                        owner,
                        property_span,
                        ReferenceKind::FieldAccess,
                        OccurrenceTarget::Local(EntityId::new(variant)),
                        OccurrenceConfidence::Index,
                    )?;
                    continue;
                }
                if let Some(target) = self.cross_file_enum_member_target(
                    identifier_span,
                    property_span,
                    property_bytes,
                )? {
                    self.commit_occurrence(
                        owner,
                        property_span,
                        ReferenceKind::FieldAccess,
                        target,
                        OccurrenceConfidence::Oracle,
                    )?;
                }
            } else if let Some(container) =
                self.namespace_qualified_fact(object_span.start, object_span.end, 0)
            {
                let container_index = match usize::try_from(container) {
                    Ok(index) => index,
                    Err(_) => continue,
                };
                if self.fact_kinds.get(container_index) == Some(&EntityKind::Enum)
                    && let Some(variant) = self.variant_in_enum(container, property_bytes)
                {
                    self.commit_occurrence(
                        owner,
                        property_span,
                        ReferenceKind::FieldAccess,
                        OccurrenceTarget::Local(EntityId::new(variant)),
                        OccurrenceConfidence::Index,
                    )?;
                }
            }
        }
        Ok(())
    }

    /// Resolves one imported-enum `Enum.Member` site to an oracle package key
    /// naming the member declaration in the spelled import module.
    fn cross_file_enum_member_target(
        &self,
        object_identifier_span: Span,
        property_span: Span,
        property_bytes: &[u8],
    ) -> Result<Option<OccurrenceTarget<'source>>, TypeScriptCollectError> {
        if !self.is_import_binding_span(object_identifier_span) {
            return Ok(None);
        }
        let Some(checker) = self.checker.as_ref() else {
            return Ok(None);
        };
        let resolved = checker.reference_at(Utf8Span {
            start: property_span.start,
            end: property_span.end,
        });
        let Some(resolved) = resolved else {
            return Ok(None);
        };
        if !resolved.is_enum_member || resolved.target.is_some() {
            return Ok(None);
        }
        let Some(module) = resolved.module else {
            return Ok(None);
        };
        let Some(name) = resolved.name else {
            return Ok(None);
        };
        if name.as_bytes() != property_bytes {
            return Ok(None);
        }
        let object_resolved = checker.reference_at(Utf8Span {
            start: object_identifier_span.start,
            end: object_identifier_span.end,
        });
        if !object_resolved.is_some_and(|object| object.module == Some(module)) {
            return Ok(None);
        }
        self.checker_foreign_key(property_span, module, Some(name), Some(EntityKind::Variant))
            .map(|key| key.map(OccurrenceTarget::Foreign))
    }

    /// Reports whether one identifier use resolves to an import binding.
    fn is_import_binding_span(&self, identifier_span: Span) -> bool {
        let scoping = self.semantic.scoping();
        let nodes = self.semantic.nodes();
        for symbol in scoping.symbol_ids() {
            if !scoping
                .symbol_flags(symbol)
                .intersects(SymbolFlags::Import | SymbolFlags::TypeImport)
            {
                continue;
            }
            for reference_id in scoping.get_resolved_reference_ids(symbol) {
                let reference = scoping.get_reference(*reference_id);
                let span = nodes.get_node(reference.node_id()).kind().span();
                if span.start == identifier_span.start && span.end == identifier_span.end {
                    return true;
                }
            }
        }
        false
    }

    /// Returns whether an exact span names a `TSQualifiedName` node.
    fn span_is_ts_qualified_name(&self, start: u32, end: u32) -> bool {
        let first = self
            .node_index
            .partition_point(|(known, _)| known.end <= start);
        for (known, node_id) in self.node_index.get(first..).unwrap_or(&[]) {
            if known.start > start {
                break;
            }
            if known.start == start
                && known.end == end
                && self
                    .semantic
                    .nodes()
                    .get_node(*node_id)
                    .kind()
                    .as_ts_qualified_name()
                    .is_some()
            {
                return true;
            }
        }
        false
    }

    /// Returns an exact-span `IdentifierReference` when one is indexed at
    /// `start`/`end`, even if a wrapper node shares the same span.
    fn identifier_reference_span_at(&self, start: u32, end: u32) -> Option<Span> {
        let first = self
            .node_index
            .partition_point(|(known, _)| known.end <= start);
        for (known, node_id) in self.node_index.get(first..).unwrap_or(&[]) {
            if known.start > start {
                break;
            }
            if known.start == start
                && known.end == end
                && self
                    .semantic
                    .nodes()
                    .get_node(*node_id)
                    .kind()
                    .as_identifier_reference()
                    .is_some()
            {
                return Some(Span::new(start, end));
            }
        }
        None
    }

    /// Returns an exact-span `TSTypeReference` when one is indexed at
    /// `start`/`end`, even if a wrapper node shares the same span.
    fn ts_type_reference_at_span(&self, start: u32, end: u32) -> Option<Span> {
        let first = self
            .node_index
            .partition_point(|(known, _)| known.end <= start);
        for (known, node_id) in self.node_index.get(first..).unwrap_or(&[]) {
            if known.start > start {
                break;
            }
            if known.start == start
                && known.end == end
                && self
                    .semantic
                    .nodes()
                    .get_node(*node_id)
                    .kind()
                    .as_ts_type_reference()
                    .is_some()
            {
                return Some(Span::new(start, end));
            }
        }
        None
    }

    /// Peels one or more parenthesized wrappers and returns the span of the
    /// innermost identifier reference, if any.
    fn peel_object_identifier_span(&self, start: u32, end: u32) -> Option<Span> {
        let mut span = Span::new(start, end);
        loop {
            let Some(kind) = self.ast_kind_at_exact_span(span.start, span.end) else {
                return None;
            };
            if let Some(parenthesized) = kind.as_parenthesized_expression() {
                span = parenthesized.expression.span();
                continue;
            }
            if let Some(non_null) = kind.as_ts_non_null_expression() {
                span = non_null.expression.span();
                continue;
            }
            if kind.as_identifier_reference().is_some() {
                return Some(span);
            }
            return None;
        }
    }

    /// Resolves one identifier use through OXC's resolved references to the
    /// published enum fact of its binding, when that binding is an enum.
    fn enum_fact_for_identifier_span(&self, identifier_span: Span) -> Option<u32> {
        let scoping = self.semantic.scoping();
        let nodes = self.semantic.nodes();
        for symbol in scoping.symbol_ids() {
            for reference_id in scoping.get_resolved_reference_ids(symbol) {
                let reference = scoping.get_reference(*reference_id);
                let span = nodes.get_node(reference.node_id()).kind().span();
                if span.start != identifier_span.start || span.end != identifier_span.end {
                    continue;
                }
                let binding_start = scoping.symbol_span(symbol).start;
                let Some(fact) = self.fact_at_name_start(binding_start) else {
                    return None;
                };
                let index = usize::try_from(fact).ok()?;
                if self.fact_kinds.get(index) == Some(&EntityKind::Enum) {
                    return Some(fact);
                }
                return None;
            }
        }
        None
    }

    /// Resolves the single variant with `name` declared inside `enum_fact`,
    /// or `None` when zero or more than one such variant is published.
    fn variant_in_enum(&self, enum_fact: u32, name: &[u8]) -> Option<u32> {
        let enum_index = usize::try_from(enum_fact).ok()?;
        let enum_start = *self.decl_starts.get(enum_index)?;
        let enum_end = *self.decl_ends.get(enum_index)?;
        if enum_start == UNSET || enum_end == UNSET {
            return None;
        }
        let Some(candidates) = self.facts_by_name.get(name) else {
            return None;
        };
        let mut matched: Option<u32> = None;
        for &candidate in candidates {
            let index = usize::try_from(candidate).ok()?;
            if self.fact_kinds.get(index) != Some(&EntityKind::Variant) {
                continue;
            }
            let decl_start = *self.decl_starts.get(index)?;
            let decl_end = *self.decl_ends.get(index)?;
            if decl_start == UNSET || decl_end == UNSET {
                continue;
            }
            if !self.declaration_contains(enum_fact, Span::new(decl_start, decl_end))
                || self.enclosing_registered_owner(decl_start, Some(candidate)) != Some(enum_fact)
            {
                continue;
            }
            if matched.is_some() {
                return None;
            }
            matched = Some(candidate);
        }
        matched
    }

    /// Reports whether a committed occurrence already names the exact
    /// absolute source span.
    fn occurrence_covers(&self, span: Utf8Span) -> Result<bool, TypeScriptCollectError> {
        Ok(self.occurrence_index(span)?.is_some())
    }

    /// Returns the first occurrence row at one exact absolute source span.
    /// Native TSZ uses this to upgrade, rather than duplicate, the syntax-only
    /// row that the ordinary OXC pass already emitted.
    fn occurrence_index(&self, span: Utf8Span) -> Result<Option<usize>, TypeScriptCollectError> {
        let count = self.facts.occurrence_len;
        for index in 0..count {
            let owner = *self
                .facts
                .occurrence_owners
                .get(index)
                .ok_or_else(lane_rejection)?;
            let occurrence = self
                .facts
                .occurrences
                .get(index)
                .ok_or_else(lane_rejection)?;
            let owner_index = usize::try_from(owner).map_err(|_| lane_rejection())?;
            let Some(base) = self.decl_starts.get(owner_index).copied() else {
                continue;
            };
            if base.checked_add(occurrence.span.start) == Some(span.start)
                && base.checked_add(occurrence.span.end) == Some(span.end)
            {
                return Ok(Some(index));
            }
        }
        Ok(None)
    }

    /// Resolves the target, confidence, and kind of one checker-only
    /// reference site. Same-file targets resolve through the published fact
    /// of the target's binding name; a checker-resolved foreign module
    /// origin resolves through the shared source-backed package key; every
    /// other base is left absent instead of guessed.
    fn checker_only_target(
        &self,
        reference: BoundReference<'report>,
    ) -> Result<
        Option<(
            OccurrenceTarget<'source>,
            OccurrenceConfidence,
            ReferenceKind,
        )>,
        TypeScriptCollectError,
    > {
        let span = Span::new(reference.span.start, reference.span.end);
        let member_call = self.is_member_call_position(span);
        let call = reference.overload_index.is_some() || member_call;
        if let Some(target) = reference.target {
            let Some(fact) = self.fact_at_name_start(target.start) else {
                return Ok(None);
            };
            let kind = if call {
                ReferenceKind::FunctionCall
            } else {
                ReferenceKind::FieldAccess
            };
            return Ok(Some((
                OccurrenceTarget::Local(EntityId::new(fact)),
                OccurrenceConfidence::Oracle,
                kind,
            )));
        }
        if let Some(module) = reference.module {
            let member_kind = if reference.is_field && !call {
                Some(EntityKind::Field)
            } else {
                None
            };
            if let Some(mut key) =
                self.checker_foreign_key(span, module, reference.name, member_kind)?
            {
                if !call && !reference.is_field && !reference.is_enum_member {
                    key.kind = Some(if reference.is_const {
                        EntityKind::Constant
                    } else if reference.is_variable {
                        EntityKind::Static
                    } else {
                        EntityKind::Function
                    });
                }
                let kind = if call {
                    ReferenceKind::FunctionCall
                } else if member_kind == Some(EntityKind::Field) {
                    ReferenceKind::FieldAccess
                } else {
                    ReferenceKind::VariableUse
                };
                return Ok(Some((
                    OccurrenceTarget::Foreign(key),
                    OccurrenceConfidence::Oracle,
                    kind,
                )));
            }
        }
        Ok(None)
    }

    /// Pass seven: static and string-literal computed member property tokens
    /// OXC never binds. Static sites name the property identifier span;
    /// computed sites name the inner span of a string-literal key whose source
    /// bytes strictly equal its unescaped value. Checker-resolved sites are
    /// left untouched when [`occurrence_covers`] already owns that span.
    fn pass_property_accesses(&mut self) -> Result<(), TypeScriptCollectError> {
        let nodes = self.semantic.nodes();
        for node in nodes.iter() {
            let Some(member) = node.kind().as_static_member_expression() else {
                continue;
            };
            let property_span = member.property.span;
            if self.occurrence_covers(Utf8Span {
                start: property_span.start,
                end: property_span.end,
            })? {
                continue;
            }
            let Some(owner) = self.owning_fact(property_span.start) else {
                continue;
            };
            let parent = nodes.get_node(nodes.parent_id(node.id())).kind();
            let kind = if parent
                .as_call_expression()
                .is_some_and(|call| call.callee.span() == member.span)
            {
                ReferenceKind::FunctionCall
            } else {
                ReferenceKind::FieldAccess
            };
            let object_kind = AstKind::from_expression(&member.object);
            let object_span = member.object.span();
            let (target, confidence) = if Self::is_this_receiver(object_kind) {
                self.this_property_target(property_span, kind)?
            } else if Self::is_super_receiver(object_kind) {
                self.super_property_target(property_span, kind)?
            } else {
                match self.namespace_property_target(object_span, property_span, kind)? {
                    Some(pair) => pair,
                    None => match self.class_qualified_property_target(
                        object_span,
                        property_span,
                        kind,
                    )? {
                        Some(pair) => pair,
                        None => match self.namespace_qualified_property_target(
                            object_span,
                            property_span,
                            kind,
                        )? {
                            Some(pair) => pair,
                            None => match self.constructed_property_target(
                                object_span,
                                property_span,
                                kind,
                            )? {
                                Some(pair) => pair,
                                None => match self.asserted_property_target(
                                    object_span,
                                    property_span,
                                    kind,
                                )? {
                                    Some(pair) => pair,
                                    None => self.syntactic_property_target(property_span)?,
                                },
                            },
                        },
                    },
                }
            };
            self.commit_occurrence(owner, property_span, kind, target, confidence)?;
        }
        for node in nodes.iter() {
            let Some(member) = node.kind().as_computed_member_expression() else {
                continue;
            };
            let key_kind = AstKind::from_expression(&member.expression);
            let Some(literal) = key_kind.as_string_literal() else {
                continue;
            };
            let Some(property_span) = self.string_literal_inner_property_span(
                literal.span,
                literal.value.as_str(),
                literal.lone_surrogates,
            ) else {
                continue;
            };
            if self.occurrence_covers(Utf8Span {
                start: property_span.start,
                end: property_span.end,
            })? {
                continue;
            }
            let Some(owner) = self.owning_fact(property_span.start) else {
                continue;
            };
            let parent = nodes.get_node(nodes.parent_id(node.id())).kind();
            let kind = if parent
                .as_call_expression()
                .is_some_and(|call| call.callee.span() == member.span)
            {
                ReferenceKind::FunctionCall
            } else {
                ReferenceKind::FieldAccess
            };
            let object_kind = AstKind::from_expression(&member.object);
            let object_span = member.object.span();
            let (target, confidence) = if Self::is_this_receiver(object_kind) {
                self.this_property_target(property_span, kind)?
            } else if Self::is_super_receiver(object_kind) {
                self.super_property_target(property_span, kind)?
            } else {
                match self.namespace_property_target(object_span, property_span, kind)? {
                    Some(pair) => pair,
                    None => match self.class_qualified_property_target(
                        object_span,
                        property_span,
                        kind,
                    )? {
                        Some(pair) => pair,
                        None => match self.namespace_qualified_property_target(
                            object_span,
                            property_span,
                            kind,
                        )? {
                            Some(pair) => pair,
                            None => match self.constructed_property_target(
                                object_span,
                                property_span,
                                kind,
                            )? {
                                Some(pair) => pair,
                                None => match self.asserted_property_target(
                                    object_span,
                                    property_span,
                                    kind,
                                )? {
                                    Some(pair) => pair,
                                    None => self.syntactic_property_target(property_span)?,
                                },
                            },
                        },
                    },
                }
            };
            self.commit_occurrence(owner, property_span, kind, target, confidence)?;
        }
        for node in nodes.iter() {
            let Some(member) = node.kind().as_private_field_expression() else {
                continue;
            };
            let property_span = member.field.span;
            if self.occurrence_covers(Utf8Span {
                start: property_span.start,
                end: property_span.end,
            })? {
                continue;
            }
            let Some(owner) = self.owning_fact(property_span.start) else {
                continue;
            };
            let parent = nodes.get_node(nodes.parent_id(node.id())).kind();
            let kind = if parent
                .as_call_expression()
                .is_some_and(|call| call.callee.span() == member.span)
            {
                ReferenceKind::FunctionCall
            } else {
                ReferenceKind::FieldAccess
            };
            let object_kind = AstKind::from_expression(&member.object);
            let (target, confidence) = if Self::is_this_receiver(object_kind) {
                self.this_private_property_target(property_span, kind)?
            } else {
                self.syntactic_property_target(property_span)?
            };
            self.commit_occurrence(owner, property_span, kind, target, confidence)?;
        }
        Ok(())
    }

    /// Returns the inner span of one string-literal computed-member key when
    /// the source bytes strictly inside the quotes equal the unescaped value.
    fn string_literal_inner_property_span(
        &self,
        full: Span,
        value: &str,
        lone_surrogates: bool,
    ) -> Option<Span> {
        if lone_surrogates {
            return None;
        }
        if value.is_empty() {
            return None;
        }
        let inner_start = full.start.checked_add(1)?;
        let inner_end = full.end.checked_sub(1)?;
        if inner_end <= inner_start {
            return None;
        }
        let bytes = self.source.as_bytes();
        let open = *bytes.get(full.start as usize)?;
        let close = *bytes.get((full.end - 1) as usize)?;
        if (open != b'"' && open != b'\'') || open != close {
            return None;
        }
        let inner = bytes.get(inner_start as usize..inner_end as usize)?;
        if inner != value.as_bytes() {
            return None;
        }
        Some(Span::new(inner_start, inner_end))
    }

    /// Walks past any number of parenthesized and non-null wrappers.
    fn peeled_receiver(mut kind: AstKind<'_>) -> AstKind<'_> {
        loop {
            if let Some(wrapped) = kind.as_parenthesized_expression() {
                kind = AstKind::from_expression(&wrapped.expression);
                continue;
            }
            if let Some(non_null) = kind.as_ts_non_null_expression() {
                kind = AstKind::from_expression(&non_null.expression);
                continue;
            }
            return kind;
        }
    }

    /// Reports whether one expression is `this` after peeling parenthesized
    /// and non-null wrappers.
    fn is_this_receiver(kind: AstKind<'_>) -> bool {
        Self::peeled_receiver(kind).as_this_expression().is_some()
    }

    /// Reports whether one expression is `super` after peeling parenthesized
    /// and non-null wrappers.
    fn is_super_receiver(kind: AstKind<'_>) -> bool {
        Self::peeled_receiver(kind).as_super().is_some()
    }

    /// Resolves the innermost pushed class record whose declaring span
    /// contains `position`.
    fn enclosing_record(&self, position: u32) -> Option<u32> {
        let length = self.facts.len;
        let mut best: Option<(u32, u32)> = None;
        for index in 0..length {
            let ordinal = coordinate(index).ok()?;
            if self.fact_kinds.get(index).copied() != Some(EntityKind::Record) {
                continue;
            }
            if let Some(part) = self
                .containing_declaration_part(ordinal, Span::new(position, position.checked_add(1)?))
                && best.is_none_or(|(known, _)| part.start >= known)
            {
                best = Some((part.start, ordinal));
            }
        }
        best.map(|(_, ordinal)| ordinal)
    }

    /// Resolves one `this.property` site through the enclosing class when
    /// exactly one same-name member of the expected kind lives there, then
    /// through at most [`MAX_INHERITANCE_DEPTH`] `extends` hops when the
    /// enclosing class declares no such member. Field reads consult fields
    /// first and only walk methods when local, inherited, and implemented
    /// fields are absent; ambiguous fields never fall through to methods.
    fn this_property_target(
        &self,
        property_span: Span,
        kind: ReferenceKind,
    ) -> Result<(OccurrenceTarget<'source>, OccurrenceConfidence), TypeScriptCollectError> {
        // This compatibility lookup requires an emitted Record. An unnamed
        // class must not borrow an outer named class's `this` members. The
        // native TSZ occurrence pass may still prove its own exact target.
        if self.anonymous_class_scope(property_span).is_some() {
            return self.syntactic_property_target(property_span);
        }
        let Some(class) = self.enclosing_record(property_span.start) else {
            return self.syntactic_property_target(property_span);
        };
        let name = self
            .slice_span(property_span)
            .ok_or(TypeScriptCollectError::Span {
                start: property_span.start,
                end: property_span.end,
            })?;
        match kind {
            ReferenceKind::FunctionCall => {
                self.this_member_target(class, name, EntityKind::Function, property_span)
            }
            ReferenceKind::FieldAccess => {
                match self.class_member_of_owner(class, name, EntityKind::Field) {
                    ClassMemberMatch::Unique(fact) => Ok((
                        OccurrenceTarget::Local(EntityId::new(fact)),
                        OccurrenceConfidence::Index,
                    )),
                    ClassMemberMatch::Ambiguous => self.syntactic_property_target(property_span),
                    ClassMemberMatch::Absent => {
                        match self.inherited_class_member(class, name, EntityKind::Field) {
                            ClassMemberMatch::Unique(fact) => Ok((
                                OccurrenceTarget::Local(EntityId::new(fact)),
                                OccurrenceConfidence::Index,
                            )),
                            ClassMemberMatch::Ambiguous => {
                                self.syntactic_property_target(property_span)
                            }
                            ClassMemberMatch::Absent => {
                                match self.implemented_class_member(class, name, EntityKind::Field)
                                {
                                    ClassMemberMatch::Unique(fact) => Ok((
                                        OccurrenceTarget::Local(EntityId::new(fact)),
                                        OccurrenceConfidence::Index,
                                    )),
                                    ClassMemberMatch::Ambiguous => {
                                        self.syntactic_property_target(property_span)
                                    }
                                    ClassMemberMatch::Absent => {
                                        match self.inherited_implemented_member(
                                            class,
                                            name,
                                            EntityKind::Field,
                                        ) {
                                            ClassMemberMatch::Unique(fact) => Ok((
                                                OccurrenceTarget::Local(EntityId::new(fact)),
                                                OccurrenceConfidence::Index,
                                            )),
                                            ClassMemberMatch::Ambiguous => {
                                                self.syntactic_property_target(property_span)
                                            }
                                            ClassMemberMatch::Absent => self.this_member_target(
                                                class,
                                                name,
                                                EntityKind::Function,
                                                property_span,
                                            ),
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            _ => self.syntactic_property_target(property_span),
        }
    }

    /// Resolves one `this.#name` site through the enclosing class only.
    /// Private names are class-branded and never walk `extends`.
    fn this_private_property_target(
        &self,
        property_span: Span,
        kind: ReferenceKind,
    ) -> Result<(OccurrenceTarget<'source>, OccurrenceConfidence), TypeScriptCollectError> {
        let Some(class) = self.enclosing_record(property_span.start) else {
            return self.syntactic_property_target(property_span);
        };
        let name = self
            .slice_span(property_span)
            .ok_or(TypeScriptCollectError::Span {
                start: property_span.start,
                end: property_span.end,
            })?;
        match kind {
            ReferenceKind::FunctionCall => {
                match self.class_member_of_owner(class, name, EntityKind::Function) {
                    ClassMemberMatch::Unique(fact) => Ok((
                        OccurrenceTarget::Local(EntityId::new(fact)),
                        OccurrenceConfidence::Index,
                    )),
                    ClassMemberMatch::Ambiguous | ClassMemberMatch::Absent => {
                        self.syntactic_property_target(property_span)
                    }
                }
            }
            ReferenceKind::FieldAccess => {
                match self.class_member_of_owner(class, name, EntityKind::Field) {
                    ClassMemberMatch::Unique(fact) => Ok((
                        OccurrenceTarget::Local(EntityId::new(fact)),
                        OccurrenceConfidence::Index,
                    )),
                    ClassMemberMatch::Ambiguous => self.syntactic_property_target(property_span),
                    ClassMemberMatch::Absent => {
                        match self.class_member_of_owner(class, name, EntityKind::Function) {
                            ClassMemberMatch::Unique(fact) => Ok((
                                OccurrenceTarget::Local(EntityId::new(fact)),
                                OccurrenceConfidence::Index,
                            )),
                            ClassMemberMatch::Ambiguous | ClassMemberMatch::Absent => {
                                self.syntactic_property_target(property_span)
                            }
                        }
                    }
                }
            }
            _ => self.syntactic_property_target(property_span),
        }
    }

    /// Combines member lookups across every merged namespace module. Any
    /// ambiguous module result or two different unique facts stay ambiguous.
    fn combine_namespace_member_matches(matches: &[ClassMemberMatch]) -> ClassMemberMatch {
        let mut unique = None;
        for matched in matches {
            match *matched {
                ClassMemberMatch::Ambiguous => return ClassMemberMatch::Ambiguous,
                ClassMemberMatch::Absent => {}
                ClassMemberMatch::Unique(fact) => {
                    if unique.is_some_and(|known| known != fact) {
                        return ClassMemberMatch::Ambiguous;
                    }
                    unique = Some(fact);
                }
            }
        }
        match unique {
            Some(fact) => ClassMemberMatch::Unique(fact),
            None => ClassMemberMatch::Absent,
        }
    }

    /// Returns every published namespace module whose binding name equals
    /// `name`, including every block of one merged namespace.
    fn namespace_modules_for_name(&self, name: &[u8]) -> Vec<u32> {
        self.facts_by_name
            .get(name)
            .map(|candidates| {
                candidates
                    .iter()
                    .filter(|ordinal| {
                        usize::try_from(**ordinal)
                            .ok()
                            .and_then(|index| self.fact_kinds.get(index).copied())
                            == Some(EntityKind::Module)
                    })
                    .copied()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Resolves one namespace member through every merged module block named
    /// by the receiver identifier.
    fn namespace_member_match(
        &self,
        modules: &[u32],
        property_name: &[u8],
        kind: ReferenceKind,
    ) -> ClassMemberMatch {
        match kind {
            ReferenceKind::FunctionCall => {
                let function_matches = modules
                    .iter()
                    .map(|module| {
                        self.class_member_of_owner(*module, property_name, EntityKind::Function)
                    })
                    .collect::<Vec<_>>();
                match Self::combine_namespace_member_matches(&function_matches) {
                    ClassMemberMatch::Unique(fact) => ClassMemberMatch::Unique(fact),
                    ClassMemberMatch::Ambiguous => ClassMemberMatch::Ambiguous,
                    ClassMemberMatch::Absent => {
                        let record_matches = modules
                            .iter()
                            .map(|module| {
                                self.class_member_of_owner(
                                    *module,
                                    property_name,
                                    EntityKind::Record,
                                )
                            })
                            .collect::<Vec<_>>();
                        Self::combine_namespace_member_matches(&record_matches)
                    }
                }
            }
            ReferenceKind::FieldAccess => {
                let field_matches = modules
                    .iter()
                    .map(|module| {
                        self.class_member_of_owner(*module, property_name, EntityKind::Field)
                    })
                    .collect::<Vec<_>>();
                match Self::combine_namespace_member_matches(&field_matches) {
                    ClassMemberMatch::Unique(fact) => ClassMemberMatch::Unique(fact),
                    ClassMemberMatch::Ambiguous => ClassMemberMatch::Ambiguous,
                    ClassMemberMatch::Absent => {
                        let mut unique = None;
                        for expected_kind in [
                            EntityKind::Constant,
                            EntityKind::Static,
                            EntityKind::Function,
                            EntityKind::Record,
                            EntityKind::Enum,
                            EntityKind::Module,
                        ] {
                            let matches = modules
                                .iter()
                                .map(|module| {
                                    self.class_member_of_owner(
                                        *module,
                                        property_name,
                                        expected_kind,
                                    )
                                })
                                .collect::<Vec<_>>();
                            match Self::combine_namespace_member_matches(&matches) {
                                ClassMemberMatch::Ambiguous => {
                                    return ClassMemberMatch::Ambiguous;
                                }
                                ClassMemberMatch::Unique(fact) => {
                                    if unique.is_some_and(|known| known != fact) {
                                        return ClassMemberMatch::Ambiguous;
                                    }
                                    unique = Some(fact);
                                }
                                ClassMemberMatch::Absent => {}
                            }
                        }
                        match unique {
                            Some(fact) => ClassMemberMatch::Unique(fact),
                            None => ClassMemberMatch::Absent,
                        }
                    }
                }
            }
            _ => ClassMemberMatch::Absent,
        }
    }

    /// Resolves one member on a published class `record` using the same local
    /// and inherited class-body rules as a class-qualified receiver.
    fn record_qualified_property_target(
        &self,
        record: u32,
        property_name: &[u8],
        kind: ReferenceKind,
    ) -> Result<Option<(OccurrenceTarget<'source>, OccurrenceConfidence)>, TypeScriptCollectError>
    {
        match kind {
            ReferenceKind::FunctionCall => {
                match self.class_member_of_owner(record, property_name, EntityKind::Function) {
                    ClassMemberMatch::Unique(fact) => Ok(Some((
                        OccurrenceTarget::Local(EntityId::new(fact)),
                        OccurrenceConfidence::Index,
                    ))),
                    ClassMemberMatch::Ambiguous => Ok(None),
                    ClassMemberMatch::Absent => {
                        match self.inherited_class_member(
                            record,
                            property_name,
                            EntityKind::Function,
                        ) {
                            ClassMemberMatch::Unique(fact) => Ok(Some((
                                OccurrenceTarget::Local(EntityId::new(fact)),
                                OccurrenceConfidence::Index,
                            ))),
                            ClassMemberMatch::Ambiguous | ClassMemberMatch::Absent => Ok(None),
                        }
                    }
                }
            }
            ReferenceKind::FieldAccess => {
                match self.class_member_of_owner(record, property_name, EntityKind::Field) {
                    ClassMemberMatch::Unique(fact) => Ok(Some((
                        OccurrenceTarget::Local(EntityId::new(fact)),
                        OccurrenceConfidence::Index,
                    ))),
                    ClassMemberMatch::Ambiguous => Ok(None),
                    ClassMemberMatch::Absent => {
                        match self.inherited_class_member(record, property_name, EntityKind::Field)
                        {
                            ClassMemberMatch::Unique(fact) => Ok(Some((
                                OccurrenceTarget::Local(EntityId::new(fact)),
                                OccurrenceConfidence::Index,
                            ))),
                            ClassMemberMatch::Ambiguous => Ok(None),
                            ClassMemberMatch::Absent => {
                                match self.class_member_of_owner(
                                    record,
                                    property_name,
                                    EntityKind::Function,
                                ) {
                                    ClassMemberMatch::Unique(fact) => Ok(Some((
                                        OccurrenceTarget::Local(EntityId::new(fact)),
                                        OccurrenceConfidence::Index,
                                    ))),
                                    ClassMemberMatch::Ambiguous => Ok(None),
                                    ClassMemberMatch::Absent => match self.inherited_class_member(
                                        record,
                                        property_name,
                                        EntityKind::Function,
                                    ) {
                                        ClassMemberMatch::Unique(fact) => Ok(Some((
                                            OccurrenceTarget::Local(EntityId::new(fact)),
                                            OccurrenceConfidence::Index,
                                        ))),
                                        ClassMemberMatch::Ambiguous | ClassMemberMatch::Absent => {
                                            Ok(None)
                                        }
                                    },
                                }
                            }
                        }
                    }
                }
            }
            _ => Ok(None),
        }
    }

    /// Resolves one `Class.member` site when the receiver peels to a
    /// file-unique class identifier. Returns `None` when the receiver is not
    /// a class or no unique member binds.
    fn class_qualified_property_target(
        &self,
        object_span: Span,
        property_span: Span,
        kind: ReferenceKind,
    ) -> Result<Option<(OccurrenceTarget<'source>, OccurrenceConfidence)>, TypeScriptCollectError>
    {
        let Some(identifier_span) =
            self.peel_object_identifier_span(object_span.start, object_span.end)
        else {
            return Ok(None);
        };
        let class_name = self
            .slice_span(identifier_span)
            .ok_or(TypeScriptCollectError::Span {
                start: identifier_span.start,
                end: identifier_span.end,
            })?;
        let Some(record) = self.unique_file_record(class_name) else {
            return Ok(None);
        };
        let property_name = self
            .slice_span(property_span)
            .ok_or(TypeScriptCollectError::Span {
                start: property_span.start,
                end: property_span.end,
            })?;
        self.record_qualified_property_target(record, property_name, kind)
    }

    /// Resolves one namespace-qualified receiver such as `Box.Child` or
    /// `Box.Inner.Deep` to the published fact named by the full prefix.
    fn namespace_qualified_fact(&self, start: u32, end: u32, depth: u8) -> Option<u32> {
        if depth >= MAX_INHERITANCE_DEPTH {
            return None;
        }
        let mut span = Span::new(start, end);
        loop {
            let Some(kind) = self.ast_kind_at_exact_span(span.start, span.end) else {
                return None;
            };
            if let Some(parenthesized) = kind.as_parenthesized_expression() {
                span = parenthesized.expression.span();
                continue;
            }
            if let Some(non_null) = kind.as_ts_non_null_expression() {
                span = non_null.expression.span();
                continue;
            }
            break;
        }
        let kind = self.ast_kind_at_exact_span(span.start, span.end)?;
        let member = kind.as_static_member_expression()?;
        let property_name = self.slice_span(member.property.span)?;
        let object_span = member.object.span();
        let modules = if let Some(identifier_span) =
            self.peel_object_identifier_span(object_span.start, object_span.end)
        {
            let name = self.slice_span(identifier_span)?;
            let modules = self.namespace_modules_for_name(name);
            if modules.is_empty() {
                return None;
            }
            modules
        } else {
            let container = self.namespace_qualified_fact(
                object_span.start,
                object_span.end,
                depth.saturating_add(1),
            )?;
            let container_index = usize::try_from(container).ok()?;
            if self.fact_kinds.get(container_index) != Some(&EntityKind::Module) {
                return None;
            }
            vec![container]
        };
        match self.namespace_member_match(&modules, property_name, ReferenceKind::FieldAccess) {
            ClassMemberMatch::Unique(fact) => Some(fact),
            ClassMemberMatch::Ambiguous | ClassMemberMatch::Absent => None,
        }
    }

    /// Resolves one `new Receiver().member` site when the receiver peels to a
    /// `new` expression whose callee names a unique file-local class or a
    /// namespace-qualified record prefix.
    fn constructed_property_target(
        &self,
        object_span: Span,
        property_span: Span,
        kind: ReferenceKind,
    ) -> Result<Option<(OccurrenceTarget<'source>, OccurrenceConfidence)>, TypeScriptCollectError>
    {
        let mut span = object_span;
        loop {
            let Some(ast_kind) = self.ast_kind_at_exact_span(span.start, span.end) else {
                return Ok(None);
            };
            if let Some(parenthesized) = ast_kind.as_parenthesized_expression() {
                span = parenthesized.expression.span();
                continue;
            }
            if let Some(non_null) = ast_kind.as_ts_non_null_expression() {
                span = non_null.expression.span();
                continue;
            }
            break;
        }
        let Some(ast_kind) = self.ast_kind_at_exact_span(span.start, span.end) else {
            return Ok(None);
        };
        let Some(new_expression) = ast_kind.as_new_expression() else {
            return Ok(None);
        };
        let mut callee_span = new_expression.callee.span();
        loop {
            let Some(callee_kind) = self.ast_kind_at_exact_span(callee_span.start, callee_span.end)
            else {
                return Ok(None);
            };
            if let Some(parenthesized) = callee_kind.as_parenthesized_expression() {
                callee_span = parenthesized.expression.span();
                continue;
            }
            if let Some(non_null) = callee_kind.as_ts_non_null_expression() {
                callee_span = non_null.expression.span();
                continue;
            }
            break;
        }
        let record = if let Some(identifier_span) =
            self.peel_object_identifier_span(callee_span.start, callee_span.end)
        {
            let class_name =
                self.slice_span(identifier_span)
                    .ok_or(TypeScriptCollectError::Span {
                        start: identifier_span.start,
                        end: identifier_span.end,
                    })?;
            match self.unique_file_record(class_name) {
                Some(record) => record,
                None => return Ok(None),
            }
        } else {
            let Some(container) =
                self.namespace_qualified_fact(callee_span.start, callee_span.end, 0)
            else {
                return Ok(None);
            };
            let container_index = usize::try_from(container).map_err(|_| lane_rejection())?;
            let container_kind = self
                .fact_kinds
                .get(container_index)
                .copied()
                .ok_or(lane_rejection())?;
            if container_kind != EntityKind::Record {
                return Ok(None);
            }
            container
        };
        let property_name = self
            .slice_span(property_span)
            .ok_or(TypeScriptCollectError::Span {
                start: property_span.start,
                end: property_span.end,
            })?;
        self.record_qualified_property_target(record, property_name, kind)
    }

    /// Resolves one `(value as Type).member` or `(<Type>value).member` site
    /// when the assertion names a unique file-local class, interface, or
    /// namespace-qualified record/trait type.
    fn asserted_property_target(
        &self,
        object_span: Span,
        property_span: Span,
        kind: ReferenceKind,
    ) -> Result<Option<(OccurrenceTarget<'source>, OccurrenceConfidence)>, TypeScriptCollectError>
    {
        let Some(reference_span) = self.assertion_type_reference_span(object_span) else {
            return Ok(None);
        };
        let Some((owner, owner_kind)) = self.assertion_type_owner(reference_span) else {
            return Ok(None);
        };
        let property_name = self
            .slice_span(property_span)
            .ok_or(TypeScriptCollectError::Span {
                start: property_span.start,
                end: property_span.end,
            })?;
        match owner_kind {
            EntityKind::Record => self.record_qualified_property_target(owner, property_name, kind),
            EntityKind::Trait => self.trait_asserted_property_target(owner, property_name, kind),
            _ => Ok(None),
        }
    }

    /// Returns the span of one assertion's `TSTypeReference` annotation when
    /// `object_span` peels to `as` or angle-bracket type assertion syntax.
    fn assertion_type_reference_span(&self, object_span: Span) -> Option<Span> {
        let mut span = object_span;
        loop {
            let kind = self.ast_kind_at_exact_span(span.start, span.end)?;
            if let Some(parenthesized) = kind.as_parenthesized_expression() {
                span = parenthesized.expression.span();
                continue;
            }
            if let Some(non_null) = kind.as_ts_non_null_expression() {
                span = non_null.expression.span();
                continue;
            }
            let annotation_span = if let Some(cast) = kind.as_ts_as_expression() {
                cast.type_annotation.span()
            } else if let Some(cast) = kind.as_ts_type_assertion() {
                cast.type_annotation.span()
            } else {
                return None;
            };
            if self
                .ts_type_reference_at_span(annotation_span.start, annotation_span.end)
                .is_some()
            {
                return Some(annotation_span);
            }
            return None;
        }
    }

    /// Resolves one assertion type reference to the published class or
    /// interface fact named by its `TSTypeReference` type name.
    fn assertion_type_owner(&self, reference_span: Span) -> Option<(u32, EntityKind)> {
        let first = self
            .node_index
            .partition_point(|(known, _)| known.end <= reference_span.start);
        let mut reference = None;
        for (known, node_id) in self.node_index.get(first..).unwrap_or(&[]) {
            if known.start > reference_span.start {
                break;
            }
            if known.start == reference_span.start && known.end == reference_span.end {
                if let Some(found) = self
                    .semantic
                    .nodes()
                    .get_node(*node_id)
                    .kind()
                    .as_ts_type_reference()
                {
                    reference = Some(found);
                    break;
                }
            }
        }
        let reference = reference?;
        if reference.type_name.is_identifier() {
            let ident = reference.type_name.get_identifier_reference()?;
            return self.assertion_identifier_type_owner(ident.span);
        }
        if reference.type_name.is_qualified_name() {
            return self.assertion_qualified_type_owner(reference.type_name.span());
        }
        None
    }

    /// Resolves one identifier type name to a unique file-local record, or to
    /// a unique trait when no such record is published.
    fn assertion_identifier_type_owner(&self, ident_span: Span) -> Option<(u32, EntityKind)> {
        let name = self.slice_span(ident_span)?;
        let record_count = self
            .facts_by_name
            .get(name)
            .map(|candidates| {
                candidates
                    .iter()
                    .filter(|ordinal| {
                        usize::try_from(**ordinal)
                            .ok()
                            .and_then(|index| self.fact_kinds.get(index).copied())
                            == Some(EntityKind::Record)
                    })
                    .count()
            })
            .unwrap_or(0);
        if record_count > 1 {
            return None;
        }
        if record_count == 1 {
            let record = self.unique_file_record(name)?;
            return Some((record, EntityKind::Record));
        }
        let trait_ordinal = self.unique_file_trait(name)?;
        Some((trait_ordinal, EntityKind::Trait))
    }

    /// Resolves one namespace-qualified type name such as `Box.Child` to the
    /// published record or trait named by the full prefix.
    fn assertion_qualified_type_owner(&self, qualified_span: Span) -> Option<(u32, EntityKind)> {
        let Some(container) = self.assertion_namespace_qualified_type_fact(
            qualified_span.start,
            qualified_span.end,
            0,
        ) else {
            return None;
        };
        let index = usize::try_from(container).ok()?;
        match self.fact_kinds.get(index).copied() {
            Some(EntityKind::Record) | Some(EntityKind::Trait) => {
                Some((container, self.fact_kinds.get(index).copied()?))
            }
            _ => None,
        }
    }

    /// Resolves one namespace-qualified type prefix such as `Box.Child` or
    /// `Box.Inner.Deep` to the published fact named by the full prefix.
    fn assertion_namespace_qualified_type_fact(
        &self,
        start: u32,
        end: u32,
        depth: u8,
    ) -> Option<u32> {
        if depth >= MAX_INHERITANCE_DEPTH {
            return None;
        }
        let first = self
            .node_index
            .partition_point(|(known, _)| known.end <= start);
        let mut qualified = None;
        for (known, node_id) in self.node_index.get(first..).unwrap_or(&[]) {
            if known.start > start {
                break;
            }
            if known.start == start && known.end == end {
                if let Some(found) = self
                    .semantic
                    .nodes()
                    .get_node(*node_id)
                    .kind()
                    .as_ts_qualified_name()
                {
                    qualified = Some(found);
                    break;
                }
            }
        }
        let qualified = qualified?;
        let modules = if qualified.left.is_identifier() {
            let ident = qualified.left.get_identifier_reference()?;
            let name = self.slice_span(ident.span)?;
            let modules = self.namespace_modules_for_name(name);
            if modules.is_empty() {
                return None;
            }
            modules
        } else if qualified.left.is_qualified_name() {
            let container = self.assertion_namespace_qualified_type_fact(
                qualified.left.span().start,
                qualified.left.span().end,
                depth.saturating_add(1),
            )?;
            let container_index = usize::try_from(container).ok()?;
            if self.fact_kinds.get(container_index) != Some(&EntityKind::Module) {
                return None;
            }
            vec![container]
        } else {
            return None;
        };
        let property_name = self.slice_span(qualified.right.span())?;
        match self.namespace_member_match(&modules, property_name, ReferenceKind::FieldAccess) {
            ClassMemberMatch::Unique(fact) => Some(fact),
            ClassMemberMatch::Ambiguous | ClassMemberMatch::Absent => None,
        }
    }

    /// Resolves one asserted interface member through declared and inherited
    /// interface bodies only.
    fn trait_asserted_property_target(
        &self,
        trait_ordinal: u32,
        property_name: &[u8],
        kind: ReferenceKind,
    ) -> Result<Option<(OccurrenceTarget<'source>, OccurrenceConfidence)>, TypeScriptCollectError>
    {
        match kind {
            ReferenceKind::FunctionCall => {
                let mut visited_ordinals = Vec::new();
                let mut visited_spans = Vec::new();
                match self.trait_member_in_hierarchy(
                    trait_ordinal,
                    property_name,
                    EntityKind::Function,
                    0,
                    &mut visited_ordinals,
                    &mut visited_spans,
                ) {
                    ClassMemberMatch::Unique(fact) => Ok(Some((
                        OccurrenceTarget::Local(EntityId::new(fact)),
                        OccurrenceConfidence::Index,
                    ))),
                    ClassMemberMatch::Ambiguous | ClassMemberMatch::Absent => Ok(None),
                }
            }
            ReferenceKind::FieldAccess => {
                let mut visited_ordinals = Vec::new();
                let mut visited_spans = Vec::new();
                match self.trait_member_in_hierarchy(
                    trait_ordinal,
                    property_name,
                    EntityKind::Field,
                    0,
                    &mut visited_ordinals,
                    &mut visited_spans,
                ) {
                    ClassMemberMatch::Unique(fact) => Ok(Some((
                        OccurrenceTarget::Local(EntityId::new(fact)),
                        OccurrenceConfidence::Index,
                    ))),
                    ClassMemberMatch::Ambiguous => Ok(None),
                    ClassMemberMatch::Absent => {
                        let mut visited_ordinals = Vec::new();
                        let mut visited_spans = Vec::new();
                        match self.trait_member_in_hierarchy(
                            trait_ordinal,
                            property_name,
                            EntityKind::Function,
                            0,
                            &mut visited_ordinals,
                            &mut visited_spans,
                        ) {
                            ClassMemberMatch::Unique(fact) => Ok(Some((
                                OccurrenceTarget::Local(EntityId::new(fact)),
                                OccurrenceConfidence::Index,
                            ))),
                            ClassMemberMatch::Ambiguous | ClassMemberMatch::Absent => Ok(None),
                        }
                    }
                }
            }
            _ => Ok(None),
        }
    }

    /// Resolves one `Namespace.Prefix.member` site when the receiver is a
    /// namespace-qualified prefix that names a published module, record, or
    /// enum fact.
    fn namespace_qualified_property_target(
        &self,
        object_span: Span,
        property_span: Span,
        kind: ReferenceKind,
    ) -> Result<Option<(OccurrenceTarget<'source>, OccurrenceConfidence)>, TypeScriptCollectError>
    {
        let Some(container) = self.namespace_qualified_fact(object_span.start, object_span.end, 0)
        else {
            return Ok(None);
        };
        let container_index = usize::try_from(container).map_err(|_| lane_rejection())?;
        let container_kind = self
            .fact_kinds
            .get(container_index)
            .copied()
            .ok_or(lane_rejection())?;
        let property_name = self
            .slice_span(property_span)
            .ok_or(TypeScriptCollectError::Span {
                start: property_span.start,
                end: property_span.end,
            })?;
        match container_kind {
            EntityKind::Module => {
                match self.namespace_member_match(&[container], property_name, kind) {
                    ClassMemberMatch::Unique(fact) => Ok(Some((
                        OccurrenceTarget::Local(EntityId::new(fact)),
                        OccurrenceConfidence::Index,
                    ))),
                    ClassMemberMatch::Ambiguous | ClassMemberMatch::Absent => Ok(None),
                }
            }
            EntityKind::Record => {
                self.record_qualified_property_target(container, property_name, kind)
            }
            EntityKind::Enum if kind == ReferenceKind::FieldAccess => {
                match self.variant_in_enum(container, property_name) {
                    Some(variant) => Ok(Some((
                        OccurrenceTarget::Local(EntityId::new(variant)),
                        OccurrenceConfidence::Index,
                    ))),
                    None => Ok(None),
                }
            }
            _ => Ok(None),
        }
    }

    /// Resolves one `Namespace.member` site when the receiver peels to a
    /// namespace identifier. Returns `None` when the receiver is not a
    /// namespace or no unique member binds.
    fn namespace_property_target(
        &self,
        object_span: Span,
        property_span: Span,
        kind: ReferenceKind,
    ) -> Result<Option<(OccurrenceTarget<'source>, OccurrenceConfidence)>, TypeScriptCollectError>
    {
        let Some(identifier_span) =
            self.peel_object_identifier_span(object_span.start, object_span.end)
        else {
            return Ok(None);
        };
        let namespace_name =
            self.slice_span(identifier_span)
                .ok_or(TypeScriptCollectError::Span {
                    start: identifier_span.start,
                    end: identifier_span.end,
                })?;
        let modules = self.namespace_modules_for_name(namespace_name);
        if modules.is_empty() {
            return Ok(None);
        }
        let property_name = self
            .slice_span(property_span)
            .ok_or(TypeScriptCollectError::Span {
                start: property_span.start,
                end: property_span.end,
            })?;
        match self.namespace_member_match(&modules, property_name, kind) {
            ClassMemberMatch::Unique(fact) => Ok(Some((
                OccurrenceTarget::Local(EntityId::new(fact)),
                OccurrenceConfidence::Index,
            ))),
            ClassMemberMatch::Ambiguous | ClassMemberMatch::Absent => Ok(None),
        }
    }

    /// Resolves one `super.property` site through inherited bases only. The
    /// enclosing class is skipped, so a child override never wins.
    fn super_property_target(
        &self,
        property_span: Span,
        kind: ReferenceKind,
    ) -> Result<(OccurrenceTarget<'source>, OccurrenceConfidence), TypeScriptCollectError> {
        let Some(class) = self.enclosing_record(property_span.start) else {
            return self.syntactic_property_target(property_span);
        };
        let name = self
            .slice_span(property_span)
            .ok_or(TypeScriptCollectError::Span {
                start: property_span.start,
                end: property_span.end,
            })?;
        match kind {
            ReferenceKind::FunctionCall => {
                match self.inherited_class_member(class, name, EntityKind::Function) {
                    ClassMemberMatch::Unique(fact) => Ok((
                        OccurrenceTarget::Local(EntityId::new(fact)),
                        OccurrenceConfidence::Index,
                    )),
                    ClassMemberMatch::Ambiguous | ClassMemberMatch::Absent => {
                        self.syntactic_property_target(property_span)
                    }
                }
            }
            ReferenceKind::FieldAccess => {
                match self.inherited_class_member(class, name, EntityKind::Field) {
                    ClassMemberMatch::Unique(fact) => Ok((
                        OccurrenceTarget::Local(EntityId::new(fact)),
                        OccurrenceConfidence::Index,
                    )),
                    ClassMemberMatch::Ambiguous => self.syntactic_property_target(property_span),
                    ClassMemberMatch::Absent => {
                        match self.inherited_class_member(class, name, EntityKind::Function) {
                            ClassMemberMatch::Unique(fact) => Ok((
                                OccurrenceTarget::Local(EntityId::new(fact)),
                                OccurrenceConfidence::Index,
                            )),
                            ClassMemberMatch::Ambiguous | ClassMemberMatch::Absent => {
                                self.syntactic_property_target(property_span)
                            }
                        }
                    }
                }
            }
            _ => self.syntactic_property_target(property_span),
        }
    }

    /// Resolves one `this.property` member through the enclosing class, then
    /// through inherited bases, then through `implements` interfaces, then
    /// through `implements` on inherited bases, using
    /// [`class_member_of_owner`], [`inherited_class_member`],
    /// [`implemented_class_member`], and [`inherited_implemented_member`].
    fn this_member_target(
        &self,
        class: u32,
        name: &[u8],
        expected_kind: EntityKind,
        property_span: Span,
    ) -> Result<(OccurrenceTarget<'source>, OccurrenceConfidence), TypeScriptCollectError> {
        match self.class_member_of_owner(class, name, expected_kind) {
            ClassMemberMatch::Unique(fact) => Ok((
                OccurrenceTarget::Local(EntityId::new(fact)),
                OccurrenceConfidence::Index,
            )),
            ClassMemberMatch::Ambiguous => self.syntactic_property_target(property_span),
            ClassMemberMatch::Absent => {
                match self.inherited_class_member(class, name, expected_kind) {
                    ClassMemberMatch::Unique(fact) => Ok((
                        OccurrenceTarget::Local(EntityId::new(fact)),
                        OccurrenceConfidence::Index,
                    )),
                    ClassMemberMatch::Ambiguous => self.syntactic_property_target(property_span),
                    ClassMemberMatch::Absent => {
                        match self.implemented_class_member(class, name, expected_kind) {
                            ClassMemberMatch::Unique(fact) => Ok((
                                OccurrenceTarget::Local(EntityId::new(fact)),
                                OccurrenceConfidence::Index,
                            )),
                            ClassMemberMatch::Ambiguous => {
                                self.syntactic_property_target(property_span)
                            }
                            ClassMemberMatch::Absent => {
                                match self.inherited_implemented_member(class, name, expected_kind)
                                {
                                    ClassMemberMatch::Unique(fact) => Ok((
                                        OccurrenceTarget::Local(EntityId::new(fact)),
                                        OccurrenceConfidence::Index,
                                    )),
                                    ClassMemberMatch::Ambiguous | ClassMemberMatch::Absent => {
                                        self.syntactic_property_target(property_span)
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// Counts members of `expected_kind` named `name` whose enclosing owner
    /// is exactly `owner`.
    fn class_member_of_owner(
        &self,
        owner: u32,
        name: &[u8],
        expected_kind: EntityKind,
    ) -> ClassMemberMatch {
        let candidates = self.facts_by_name.get(name).cloned().unwrap_or_default();
        let mut primary = None;
        let mut setter_count: u32 = 0;
        let mut setter = None;
        for ordinal in candidates {
            let Some(index) = usize::try_from(ordinal).ok() else {
                continue;
            };
            if self.fact_kinds.get(index).copied() != Some(expected_kind) {
                continue;
            }
            let decl_start = self.decl_starts.get(index).copied().unwrap_or(UNSET);
            if decl_start == UNSET {
                continue;
            }
            let owner_match =
                self.enclosing_registered_owner(decl_start, Some(ordinal)) == Some(owner);
            let parameter_property_match = self.parameter_properties.contains(&ordinal)
                && self.enclosing_record(decl_start) == Some(owner);
            if !owner_match && !parameter_property_match {
                continue;
            }
            if expected_kind == EntityKind::Function && self.setters.contains(&ordinal) {
                setter_count = setter_count.saturating_add(1);
                setter = Some(ordinal);
                continue;
            }
            if primary.is_some() {
                return ClassMemberMatch::Ambiguous;
            }
            primary = Some(ordinal);
        }
        match primary {
            Some(fact) => ClassMemberMatch::Unique(fact),
            None if setter_count == 1 => match setter {
                Some(fact) => ClassMemberMatch::Unique(fact),
                None => ClassMemberMatch::Absent,
            },
            None if setter_count > 1 => ClassMemberMatch::Ambiguous,
            None => ClassMemberMatch::Absent,
        }
    }

    /// Walks `extends` from `start_class`, resolving inherited members with
    /// the same count rules as the enclosing class.
    fn inherited_class_member(
        &self,
        start_class: u32,
        name: &[u8],
        expected_kind: EntityKind,
    ) -> ClassMemberMatch {
        let mut visited = Vec::new();
        let mut current = start_class;
        for _ in 0..MAX_INHERITANCE_DEPTH {
            if visited.contains(&current) {
                return ClassMemberMatch::Absent;
            }
            visited.push(current);
            let super_span = match self.record_super_class_name_span(current) {
                Some(span) => span,
                None => return ClassMemberMatch::Absent,
            };
            let super_name = match self.slice_span(super_span) {
                Some(name) => name,
                None => return ClassMemberMatch::Absent,
            };
            let base = match self.unique_file_record(super_name) {
                Some(record) => record,
                None => return ClassMemberMatch::Absent,
            };
            match self.class_member_of_owner(base, name, expected_kind) {
                ClassMemberMatch::Unique(fact) => return ClassMemberMatch::Unique(fact),
                ClassMemberMatch::Ambiguous => return ClassMemberMatch::Ambiguous,
                ClassMemberMatch::Absent => current = base,
            }
        }
        ClassMemberMatch::Absent
    }

    /// Walks `extends` from `start_class`, resolving members through each
    /// base's `implements` clauses when the enclosing class and inherited
    /// class-body members are absent.
    fn inherited_implemented_member(
        &self,
        start_class: u32,
        name: &[u8],
        expected_kind: EntityKind,
    ) -> ClassMemberMatch {
        let mut visited = Vec::new();
        let mut current = start_class;
        for _ in 0..MAX_INHERITANCE_DEPTH {
            if visited.contains(&current) {
                return ClassMemberMatch::Absent;
            }
            visited.push(current);
            let super_span = match self.record_super_class_name_span(current) {
                Some(span) => span,
                None => return ClassMemberMatch::Absent,
            };
            let super_name = match self.slice_span(super_span) {
                Some(name) => name,
                None => return ClassMemberMatch::Absent,
            };
            let base = match self.unique_file_record(super_name) {
                Some(record) => record,
                None => return ClassMemberMatch::Absent,
            };
            match self.implemented_class_member(base, name, expected_kind) {
                ClassMemberMatch::Unique(fact) => return ClassMemberMatch::Unique(fact),
                ClassMemberMatch::Ambiguous => return ClassMemberMatch::Ambiguous,
                ClassMemberMatch::Absent => current = base,
            }
        }
        ClassMemberMatch::Absent
    }

    /// Resolves the single file-local `Record` with `name`, or `None` when
    /// zero or more than one such record is published.
    fn unique_file_record(&self, name: &[u8]) -> Option<u32> {
        let candidates = self.facts_by_name.get(name)?;
        let mut matched = None;
        for &ordinal in candidates {
            let index = usize::try_from(ordinal).ok()?;
            if self.fact_kinds.get(index) != Some(&EntityKind::Record) {
                continue;
            }
            if matched.is_some() {
                return None;
            }
            matched = Some(ordinal);
        }
        matched
    }

    /// Resolves the single file-local `Trait` with `name`, or `None` when
    /// zero or more than one such trait is published.
    fn unique_file_trait(&self, name: &[u8]) -> Option<u32> {
        let candidates = self.facts_by_name.get(name)?;
        let mut matched = None;
        for &ordinal in candidates {
            let index = usize::try_from(ordinal).ok()?;
            if self.fact_kinds.get(index) != Some(&EntityKind::Trait) {
                continue;
            }
            if matched.is_some() {
                return None;
            }
            matched = Some(ordinal);
        }
        matched
    }

    /// Resolves one `this.property` member through every `implements` clause
    /// on `class`, walking each interface's `extends` heritages when the
    /// direct member is absent.
    fn implemented_class_member(
        &self,
        class: u32,
        name: &[u8],
        expected_kind: EntityKind,
    ) -> ClassMemberMatch {
        let index = match usize::try_from(class) {
            Ok(index) => index,
            Err(_) => return ClassMemberMatch::Absent,
        };
        let start = self.decl_starts.get(index).copied().unwrap_or(UNSET);
        let end = self.decl_ends.get(index).copied().unwrap_or(UNSET);
        if start == UNSET || end == UNSET {
            return ClassMemberMatch::Absent;
        }
        let kind = match self.ast_kind_at_exact_span(start, end) {
            Some(kind) => kind,
            None => return ClassMemberMatch::Absent,
        };
        let class_ast = match kind.as_class() {
            Some(class_ast) => class_ast,
            None => return ClassMemberMatch::Absent,
        };
        let mut matches = Vec::new();
        for implements_clause in class_ast.implements.iter() {
            let expression_span = implements_clause.expression.span();
            if self.span_is_ts_qualified_name(expression_span.start, expression_span.end) {
                continue;
            }
            let identifier_span = match self
                .identifier_reference_span_at(expression_span.start, expression_span.end)
            {
                Some(span) => span,
                None => continue,
            };
            let trait_name = match self.slice_span(identifier_span) {
                Some(name) => name,
                None => continue,
            };
            let trait_ordinal = match self.unique_file_trait(trait_name) {
                Some(trait_ordinal) => trait_ordinal,
                None => continue,
            };
            let mut visited_ordinals = Vec::new();
            let mut visited_spans = Vec::new();
            matches.push(self.trait_member_in_hierarchy(
                trait_ordinal,
                name,
                expected_kind,
                0,
                &mut visited_ordinals,
                &mut visited_spans,
            ));
        }
        Self::combine_namespace_member_matches(&matches)
    }

    /// Resolves one member on `trait_ordinal` and, when absent, walks that
    /// interface's `extends` heritages up to [`MAX_INHERITANCE_DEPTH`].
    fn trait_member_in_hierarchy(
        &self,
        trait_ordinal: u32,
        name: &[u8],
        expected_kind: EntityKind,
        depth: u8,
        visited_ordinals: &mut Vec<u32>,
        visited_spans: &mut Vec<u32>,
    ) -> ClassMemberMatch {
        if depth >= MAX_INHERITANCE_DEPTH {
            return ClassMemberMatch::Absent;
        }
        if visited_ordinals.contains(&trait_ordinal) {
            return ClassMemberMatch::Absent;
        }
        let index = match usize::try_from(trait_ordinal) {
            Ok(index) => index,
            Err(_) => return ClassMemberMatch::Absent,
        };
        let decl_start = self.decl_starts.get(index).copied().unwrap_or(UNSET);
        if decl_start != UNSET && visited_spans.contains(&decl_start) {
            return ClassMemberMatch::Absent;
        }
        visited_ordinals.push(trait_ordinal);
        if decl_start != UNSET {
            visited_spans.push(decl_start);
        }
        match self.class_member_of_owner(trait_ordinal, name, expected_kind) {
            ClassMemberMatch::Unique(fact) => ClassMemberMatch::Unique(fact),
            ClassMemberMatch::Ambiguous => ClassMemberMatch::Ambiguous,
            ClassMemberMatch::Absent => {
                let start = self.decl_starts.get(index).copied().unwrap_or(UNSET);
                let end = self.decl_ends.get(index).copied().unwrap_or(UNSET);
                if start == UNSET || end == UNSET {
                    return ClassMemberMatch::Absent;
                }
                let kind = match self.ast_kind_at_exact_span(start, end) {
                    Some(kind) => kind,
                    None => return ClassMemberMatch::Absent,
                };
                let interface = match kind.as_ts_interface_declaration() {
                    Some(interface) => interface,
                    None => return ClassMemberMatch::Absent,
                };
                let mut combined = ClassMemberMatch::Absent;
                for heritage in interface.extends.iter() {
                    let expression_span = heritage.expression.span();
                    if self.span_is_ts_qualified_name(expression_span.start, expression_span.end) {
                        continue;
                    }
                    let identifier_span = match self
                        .identifier_reference_span_at(expression_span.start, expression_span.end)
                        .or_else(|| {
                            self.peel_object_identifier_span(
                                expression_span.start,
                                expression_span.end,
                            )
                        }) {
                        Some(span) => span,
                        None => continue,
                    };
                    let super_name = match self.slice_span(identifier_span) {
                        Some(name) => name,
                        None => continue,
                    };
                    let super_trait = match self.unique_file_trait(super_name) {
                        Some(super_trait) => super_trait,
                        None => continue,
                    };
                    let branch = self.trait_member_in_hierarchy(
                        super_trait,
                        name,
                        expected_kind,
                        depth.saturating_add(1),
                        visited_ordinals,
                        visited_spans,
                    );
                    combined = Self::combine_namespace_member_matches(&[combined, branch]);
                    if matches!(combined, ClassMemberMatch::Ambiguous) {
                        return ClassMemberMatch::Ambiguous;
                    }
                }
                combined
            }
        }
    }

    /// Returns the identifier span of one class record's `extends` clause,
    /// peeling parenthesized wrappers and requiring an identifier reference.
    fn record_super_class_name_span(&self, record: u32) -> Option<Span> {
        let index = usize::try_from(record).ok()?;
        let start = self.decl_starts.get(index).copied().unwrap_or(UNSET);
        let end = self.decl_ends.get(index).copied().unwrap_or(UNSET);
        if start == UNSET || end == UNSET {
            return None;
        }
        let kind = self.ast_kind_at_exact_span(start, end)?;
        let class = kind.as_class()?;
        let super_expr = class.super_class.as_ref()?;
        let span = super_expr.span();
        self.peel_object_identifier_span(span.start, span.end)
    }

    /// Builds the honest npm-universe foreign key for one unresolved property
    /// token, borrowing the exact source spelling as path and display.
    fn syntactic_property_target(
        &self,
        span: Span,
    ) -> Result<(OccurrenceTarget<'source>, OccurrenceConfidence), TypeScriptCollectError> {
        let name = self.text_span(span).ok_or(TypeScriptCollectError::Span {
            start: span.start,
            end: span.end,
        })?;
        let key = ForeignKey::new(
            ForeignOrigin::Universe {
                ecosystem: NPM_ECOSYSTEM,
            },
            name,
            name,
            None,
        )
        .map_err(|cause| foreign_fault(cause, span))?;
        Ok((
            OccurrenceTarget::Foreign(key),
            OccurrenceConfidence::Syntactic,
        ))
    }

    /// Resolves one type-reference name through the checker's report when
    /// OXC missed it: a checker-resolved same-file declaration becomes the
    /// published fact of its binding name.
    fn checker_local_target(&self, name_span: Span) -> Option<u32> {
        let checker = self.checker.as_ref()?;
        let resolved = checker.reference_at(Utf8Span {
            start: name_span.start,
            end: name_span.end,
        })?;
        let target = resolved.target?;
        self.fact_at_name_start(target.start)
    }

    /// Reports whether the checker resolved this name to a foreign module
    /// origin, so the declared record can distinguish a known-but-
    /// unrepresentable base from a genuinely unresolvable one.
    fn checker_foreign_resolved(&self, name_span: Span) -> bool {
        let Some(checker) = self.checker.as_ref() else {
            return false;
        };
        checker
            .reference_at(Utf8Span {
                start: name_span.start,
                end: name_span.end,
            })
            .is_some_and(|reference| reference.module.is_some())
    }

    fn checker_foreign_module(&self, name_span: Span) -> Option<&'report str> {
        let checker = self.checker.as_ref()?;
        checker
            .reference_at(Utf8Span {
                start: name_span.start,
                end: name_span.end,
            })
            .and_then(|reference| reference.module)
    }

    /// Upgrades one same-file reference to oracle confidence when the
    /// checker authority resolved it: the target becomes the exact overload
    /// member the checker picked (call sites), the first published fact of
    /// the binding otherwise. Import bindings keep their npm foreign origin
    /// so a resolution never loses its ecosystem key.
    fn oracle_upgrade(
        &self,
        fact: u32,
        binding_start: u32,
        span: Span,
    ) -> Result<Option<(OccurrenceTarget<'source>, OccurrenceConfidence)>, TypeScriptCollectError>
    {
        let Some(checker) = self.checker.as_ref() else {
            return Ok(None);
        };
        let resolved = checker.reference_at(Utf8Span {
            start: span.start,
            end: span.end,
        });
        let Some(resolved) = resolved else {
            return Ok(None);
        };
        let Some(target) = resolved.target else {
            return Ok(None);
        };
        let Some(base) = self.fact_at_name_start(target.start) else {
            return Ok(None);
        };
        if base != fact {
            // The checker and OXC disagree on the binding; the weaker but
            // proven index fact wins.
            return Ok(None);
        }
        let picked = match resolved.overload_index {
            Some(index) => self.overload_fact(binding_start, index).unwrap_or(fact),
            None => fact,
        };
        Ok(Some((
            OccurrenceTarget::Local(EntityId::new(picked)),
            OccurrenceConfidence::Oracle,
        )))
    }

    /// Resolves one binding's `index`-th overload member: the facts sharing
    /// the binding-name span, in push (source) order.
    fn overload_fact(&self, binding_start: u32, index: u32) -> Option<u32> {
        let length = coordinate(self.facts.len()).ok()?;
        let mut seen = 0_u32;
        for ordinal in 0..length {
            let index_slot = usize::try_from(ordinal).ok()?;
            if self.name_starts.get(index_slot) != Some(&binding_start) {
                continue;
            }
            if seen == index {
                return Some(ordinal);
            }
            seen = seen.checked_add(1)?;
        }
        None
    }

    /// Pass seven: every `/** */` block comment becomes the documentation of
    /// the next declared fact, split into text lines, inline code spans, and
    /// `{@link ...}` targets resolved against the published facts.
    fn pass_docs(&mut self) -> Result<(), TypeScriptCollectError> {
        // OXC provides the documentation-comment plane for registered source
        // declarations. Synthetic type-expression facts have no declaration
        // comment authority and deliberately remain unmarked.
        let fact_count = coordinate(self.facts.len())?;
        for owner in 0..fact_count {
            let index = usize::try_from(owner).map_err(|_| lane_rejection())?;
            if self.decl_starts.get(index).copied() != Some(UNSET) {
                self.facts
                    .mark_documentation_captured(owner)
                    .map_err(fault)?;
            }
        }
        let comments = self.semantic.comments();
        let bytes = self.source.as_bytes();
        for comment in comments.iter() {
            if !comment.is_block() {
                continue;
            }
            let start = usize::try_from(comment.span.start).map_err(|_| lane_rejection())?;
            let marker_end = start.checked_add(3).ok_or_else(lane_rejection)?;
            if bytes.get(start..marker_end) != Some(&b"/**"[..]) {
                continue;
            }
            let content = comment.content_span();
            let Some(owner) = self.next_fact_after(content.end) else {
                continue;
            };
            let Some(content_bytes) = self.slice_span(content) else {
                continue;
            };
            self.push_jsdoc_content(owner, content_bytes)?;
        }
        Ok(())
    }

    /// Splits one JSDoc body into per-line fragment runs separated by soft
    /// breaks between emitted lines.
    fn push_jsdoc_content(
        &mut self,
        owner: u32,
        content: &'source [u8],
    ) -> Result<(), TypeScriptCollectError> {
        let mut emitted_any = false;
        let mut lines = content.split(|byte| *byte == b'\n').peekable();
        while let Some(line) = lines.next() {
            let trimmed = trim_jsdoc_line(line);
            if trimmed.is_empty() && lines.peek().is_none() {
                break;
            }
            let emitted = self.push_jsdoc_line(owner, trimmed, emitted_any)?;
            emitted_any = emitted_any || emitted;
        }
        Ok(())
    }

    /// Stages one JSDoc line and commits it after the inter-line soft break.
    /// The doc lane's own capacity is the only bound; a line is not truncated.
    fn push_jsdoc_line(
        &mut self,
        owner: u32,
        line: &'source [u8],
        leading_break: bool,
    ) -> Result<bool, TypeScriptCollectError> {
        let mut staged = Vec::new();
        let mut cursor = 0_usize;
        loop {
            let Some((at, is_link)) = earliest_inline_tag(line, cursor) else {
                if let Some(rest) = line.get(cursor..)
                    && !rest.is_empty()
                {
                    staged.push(DocFragmentInput::Text(rest));
                }
                break;
            };
            if let Some(before) = line.get(cursor..at)
                && !before.is_empty()
            {
                staged.push(DocFragmentInput::Text(before));
            }
            let content_start = at + TAG_WIDTH;
            match find_sub(line, b"}", content_start) {
                Some(close) => {
                    let inner = trim_bytes(line.get(content_start..close).unwrap_or(&[]), b" ");
                    if !inner.is_empty() {
                        let fragment = if is_link {
                            DocFragmentInput::Link {
                                label: inner,
                                target: self.link_target(inner)?,
                            }
                        } else {
                            DocFragmentInput::Code(inner)
                        };
                        staged.push(fragment);
                    }
                    cursor = close + 1;
                }
                None => {
                    if let Some(rest) = line.get(cursor..)
                        && !rest.is_empty()
                    {
                        staged.push(DocFragmentInput::Text(rest));
                    }
                    break;
                }
            }
        }
        if staged.is_empty() {
            return Ok(false);
        }
        if leading_break {
            self.facts
                .push_doc(owner, DocFragmentInput::SoftBreak)
                .map_err(fault)?;
        }
        for fragment in staged {
            self.facts.push_doc(owner, fragment).map_err(fault)?;
        }
        Ok(true)
    }

    /// Resolves one `{@link ...}` target: a published fact name is a local
    /// link; everything else is an npm-foreign path.
    fn link_target(
        &self,
        name: &'source [u8],
    ) -> Result<DocLinkTarget<'source>, TypeScriptCollectError> {
        if let Some(fact) = self.fact_by_name_bytes(name) {
            return Ok(DocLinkTarget::Local(EntityId::new(fact)));
        }
        Ok(DocLinkTarget::Foreign {
            ecosystem: b"npm",
            path: name,
        })
    }
}

/// Captures the old scan's first-ordinal rule in one bounded pass. Include
/// UNSET exactly as before; callers decide whether that start is meaningful.
/// A short side lane has no implicit tail entries, and facts beyond fact_len
/// cannot enter this computed-lowering view.
fn index_name_starts(name_starts: &[u32], fact_len: u32) -> HashMap<u32, u32> {
    let mut index = HashMap::new();
    let bound = usize::try_from(fact_len).unwrap_or(usize::MAX);
    for (ordinal, &start) in name_starts.iter().take(bound).enumerate() {
        // The take bound proves ordinal < fact_len <= u32::MAX.
        index.entry(start).or_insert(ordinal as u32);
    }
    index
}

impl<'a, 'source> FactRegistry<'a, 'source> {
    /// Resolves one source position to the fact whose binding name starts
    /// exactly there.
    fn fact_at_name_start(&self, start: u32) -> Option<u32> {
        self.name_start_index.get(&start).copied()
    }

    /// Resolves the first pushed fact whose exact binding-name bytes equal
    /// `name`.
    fn fact_by_name_bytes(&self, name: &[u8]) -> Option<u32> {
        for ordinal in 0..self.fact_len {
            let index = usize::try_from(ordinal).ok()?;
            let start = *self.name_starts.get(index)?;
            let end = *self.name_ends.get(index)?;
            if start == UNSET || end == UNSET {
                continue;
            }
            let spelled = self
                .source
                .get(usize::try_from(start).ok()?..usize::try_from(end).ok()?)?;
            if spelled.as_bytes() == name {
                return Some(ordinal);
            }
        }
        None
    }

    /// Borrows the exact source bytes one checker spelling names inside its
    /// owning declaration's source text, so computed record text cells stay
    /// source-backed. `None` when the declaration never spells the name.
    fn source_spelling_owner(&self, name: &[u8], owner: u32) -> Option<&'source [u8]> {
        if let Some(parts) = self.declaration_parts.get(&owner) {
            return parts
                .iter()
                .find_map(|part| self.source_spelling_in(name, part.start, part.end));
        }
        let owner_index = usize::try_from(owner).ok()?;
        let start = *self.decl_starts.get(owner_index)?;
        let end = *self.decl_ends.get(owner_index)?;
        if start == UNSET || end == UNSET {
            return None;
        }
        self.source_spelling_in(name, start, end)
    }

    /// Borrows the exact source bytes one checker spelling names inside the
    /// `[start, end)` source range. `None` when the range never spells the
    /// name.
    fn source_spelling_in(&self, name: &[u8], start: u32, end: u32) -> Option<&'source [u8]> {
        let declaration = self
            .source
            .get(usize::try_from(start).ok()?..usize::try_from(end).ok()?)?;
        let at = find_sub(declaration.as_bytes(), name, 0)?;
        let end = at.checked_add(name.len())?;
        Some(declaration.as_bytes().get(at..end)?)
    }

    /// Borrows the exact source bytes one checker spelling names inside its
    /// spelling domain: the owner declaration for declaration-computed rows,
    /// the narrowing site for narrowing rows (falling back to the owner
    /// declaration when the site never spells the name).
    fn source_spelling(
        &self,
        domain: SpellDomain,
        name: &[u8],
        owner: u32,
    ) -> Option<&'source [u8]> {
        let source_end = u32::try_from(self.source.len()).ok()?;
        match domain {
            SpellDomain::Owner => self
                .source_spelling_owner(name, owner)
                .or_else(|| self.source_spelling_in(name, 0, source_end)),
            SpellDomain::Range(start, end) => self
                .source_spelling_in(name, start, end)
                .or_else(|| self.source_spelling_owner(name, owner))
                .or_else(|| self.source_spelling_in(name, 0, source_end)),
        }
    }

    /// Resolves a computed type name only from the declaration or explicit
    /// source range that owns that type. The checker may report dependency
    /// types whose binders and members do not occur in this file; an unrelated
    /// homonym elsewhere in the file is not evidence that those names were
    /// written here. TSZ dependency and library atoms are checker data, never
    /// source text merely because a same-spelled token occurs in the file.
    fn source_spelling_in_domain(
        &self,
        domain: SpellDomain,
        name: &[u8],
        owner: u32,
    ) -> Option<&'source [u8]> {
        match domain {
            SpellDomain::Owner => self.source_spelling_owner(name, owner),
            SpellDomain::Range(start, end) => self
                .source_spelling_in(name, start, end)
                .or_else(|| self.source_spelling_owner(name, owner)),
        }
    }
}

/// The source span that owns one computed tree's member spellings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SpellDomain {
    /// The owning declaration's declaring span.
    Owner,
    /// One explicit source range — a narrowing's assignment site.
    Range(u32, u32),
}

/// Interns one checker computed type as a computed type row owned by
/// `owner`, interning every child row first so the pooled lane stays
/// topologically backward. Returns the row's lane coordinate
/// (`COMPUTED_ROW_BASE` plus its pool ordinal).
///
/// `spell` names the source span that owns the tree's member spellings:
/// the owner's declaring span for declaration-computed rows, or the exact
/// assignment site for narrowing rows (whose shapes are spelled at the
/// site, not at the declaration).
fn intern_computed_tree<'source>(
    registry: &FactRegistry<'_, 'source>,
    facts: &mut FactSet<'source>,
    tree: &TypeTree,
    owner: u32,
    depth: u8,
    spell: SpellDomain,
) -> Result<u32, TypeScriptCollectError> {
    if depth > MAX_TYPE_DEPTH {
        return intern_computed_leaf(
            facts,
            unknown_record(TypeReason::TruncatedAtDepthLimit),
            owner,
        );
    }
    match tree {
        TypeTree::This => match registry.source_spelling(spell, b"this", owner) {
            Some(spelling) => {
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::SelfType);
                record.text = Some(spelling);
                intern_computed_leaf(facts, record, owner)
            }
            None => intern_computed_leaf(facts, unknown_record(TypeReason::OracleGap), owner),
        },
        TypeTree::TypeParameter { name } => {
            // A computed type parameter names the source spelling of its
            // declared generic parameter; the record text is sliced from
            // the exact source bytes, never from the report.
            match registry.source_spelling(spell, name.as_bytes(), owner) {
                Some(spelling) => {
                    let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::TypeVar);
                    record.text = Some(spelling);
                    intern_computed_leaf(facts, record, owner)
                }
                None => intern_computed_leaf(facts, unknown_record(TypeReason::OracleGap), owner),
            }
        }
        TypeTree::Other { .. } => {
            // The checker's printed spelling is not source text and the
            // record text cell borrows only source bytes, so an unknown
            // printed shape is an honest oracle gap.
            intern_computed_leaf(facts, unknown_record(TypeReason::OracleGap), owner)
        }
        TypeTree::Primitive { name } => {
            intern_computed_leaf(facts, checker_primitive(name)?, owner)
        }
        TypeTree::Literal { base, .. } => {
            intern_computed_leaf(facts, checker_literal(*base), owner)
        }
        TypeTree::Union { members } => intern_computed_associative(
            registry,
            facts,
            members,
            owner,
            depth,
            spell,
            SemanticTypeTag::Union,
        ),
        TypeTree::Intersection { members } => intern_computed_associative(
            registry,
            facts,
            members,
            owner,
            depth,
            spell,
            SemanticTypeTag::Intersection,
        ),
        TypeTree::Conditional {
            check,
            extends,
            then_type,
            else_type,
        } => {
            let children = [
                check.as_ref().clone(),
                extends.as_ref().clone(),
                then_type.as_ref().clone(),
                else_type.as_ref().clone(),
            ];
            let children =
                intern_computed_children(registry, facts, &children, owner, depth, spell)?;
            intern_computed_row(
                registry,
                facts,
                SemanticTypeRecord::leaf(SemanticTypeTag::Conditional),
                owner,
                &children[..4],
            )
        }
        TypeTree::Mapped {
            parameter,
            constraint,
            name_as,
            value,
            readonly,
            optional,
        } => {
            // A mapped binder from a dependency's computed type is not a
            // source spelling merely because the same bytes occur elsewhere
            // in the owner's file. Preserve a typed precision loss without
            // partially interning the mapped type's children.
            let Some(parameter_text) =
                registry.source_spelling_in_domain(spell, parameter.as_bytes(), owner)
            else {
                return intern_computed_leaf(facts, unknown_record(TypeReason::OracleGap), owner);
            };
            let constraint =
                intern_computed_tree(registry, facts, constraint, owner, depth, spell)?;
            let name_as = name_as
                .as_deref()
                .map(|name_as| intern_computed_tree(registry, facts, name_as, owner, depth, spell))
                .transpose()?;
            let value = intern_computed_tree(registry, facts, value, owner, depth, spell)?;
            let mut children = [constraint, value, 0];
            let len = match name_as {
                Some(name_as) => {
                    children = [constraint, name_as, value];
                    3
                }
                None => 2,
            };
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Mapped);
            record.text = Some(parameter_text);
            record.payload0 = checker_mapped_modifier(*readonly);
            record.payload1 = checker_mapped_modifier(*optional);
            intern_computed_row(registry, facts, record, owner, &children[..len])
        }
        TypeTree::TemplateLiteral { parts } => {
            // The checker report owns strings, while staging deliberately
            // borrows only the entered source lease. Resolve every literal
            // segment before appending anything so an absent source spelling
            // becomes one truthful OracleGap row rather than a half-built
            // computed-child transaction.
            let mut text_parts = [None; MAX_TYPE_CHILDREN];
            if parts.len() > MAX_TYPE_CHILDREN {
                return Err(computed_fault(
                    registry,
                    owner,
                    FactFault::TypeChildCapacity,
                ));
            }
            for (position, part) in parts.iter().enumerate() {
                if let TemplatePart::Text { text } = part {
                    let Some(source_text) = registry.source_spelling(spell, text.as_bytes(), owner)
                    else {
                        return intern_computed_leaf(
                            facts,
                            unknown_record(TypeReason::OracleGap),
                            owner,
                        );
                    };
                    text_parts[position] = Some(source_text);
                }
            }
            let mut staged = Vec::with_capacity(parts.len());
            for (position, part) in parts.iter().enumerate() {
                staged.push(match part {
                    TemplatePart::Text { .. } => {
                        ComputedTemplatePart::Text(text_parts[position].ok_or_else(|| {
                            computed_fault(registry, owner, FactFault::TypeChildCapacity)
                        })?)
                    }
                    TemplatePart::Type { r#type } => {
                        ComputedTemplatePart::Type(intern_computed_tree(
                            registry,
                            facts,
                            r#type,
                            owner,
                            depth.saturating_add(1),
                            spell,
                        )?)
                    }
                });
            }
            intern_computed_template(registry, facts, owner, &staged)
        }
        TypeTree::Tuple { elements } => {
            let children =
                intern_computed_children(registry, facts, elements, owner, depth, spell)?;
            intern_computed_row(
                registry,
                facts,
                SemanticTypeRecord::leaf(SemanticTypeTag::Tuple),
                owner,
                &children[..elements.len()],
            )
        }
        TypeTree::Array { element } => {
            let children = intern_computed_children(
                registry,
                facts,
                core::slice::from_ref(element),
                owner,
                depth,
                spell,
            )?;
            let record = SemanticTypeRecord::leaf(SemanticTypeTag::ArraySequence);
            intern_computed_row(registry, facts, record, owner, &children[..1])
        }
        TypeTree::Function { parameters, result } => {
            let mut children = [0_u32; MAX_TYPE_CHILDREN];
            let mut len = 0_usize;
            if parameters.len() >= MAX_TYPE_CHILDREN {
                return Err(computed_fault(
                    registry,
                    owner,
                    FactFault::TypeChildCapacity,
                ));
            }
            for parameter in parameters {
                let slot = children
                    .get_mut(len)
                    .ok_or_else(|| computed_fault(registry, owner, FactFault::TypeChildCapacity))?;
                *slot = intern_computed_tree(
                    registry,
                    facts,
                    parameter,
                    owner,
                    depth.saturating_add(1),
                    spell,
                )?;
                len += 1;
            }
            let result_row = intern_computed_tree(
                registry,
                facts,
                result,
                owner,
                depth.saturating_add(1),
                spell,
            )?;
            let slot = children
                .get_mut(len)
                .ok_or_else(|| computed_fault(registry, owner, FactFault::TypeChildCapacity))?;
            *slot = result_row;
            len += 1;
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::FunctionPointer);
            record.payload1 = SemanticTypeRecord::FUNCTION_RESULT_COUNT_ONE;
            intern_computed_row(registry, facts, record, owner, &children[..len])
        }
        TypeTree::Object { members } => {
            // Prove all non-synthetic member names before interning children.
            // A dependency-computed anonymous object cannot be represented
            // faithfully when even one member has no source spelling. Emit a
            // single OracleGap leaf instead of rejecting the package or
            // retaining a misleading partial record.
            let mut spellings: Vec<Option<&'source [u8]>> = Vec::with_capacity(members.len());
            for member in members {
                match registry.source_spelling_in_domain(spell, member.name.as_bytes(), owner) {
                    Some(spelling) => spellings.push(Some(spelling)),
                    None if member.name.starts_with("__@") => spellings.push(None),
                    None => {
                        return intern_computed_leaf(
                            facts,
                            unknown_record(TypeReason::OracleGap),
                            owner,
                        );
                    }
                }
            }

            // Members carry names and flags, so each member's row is
            // interned first and then linked with its proven source spelling.
            let mut rows: Vec<(u32, Option<&'source [u8]>, u8)> = Vec::with_capacity(members.len());
            for (member, spelling) in members.iter().zip(spellings.into_iter()) {
                let Some(spelling) = spelling else {
                    // Checker-internal `__@` members have no source member
                    // row and are intentionally absent from the anonymous
                    // record projection.
                    continue;
                };
                let row = intern_computed_tree(
                    registry,
                    facts,
                    &member.member_type,
                    owner,
                    depth.saturating_add(1),
                    spell,
                )?;
                let mut flags = 0_u8;
                if member.optional {
                    flags |= SemanticTypeChild::FLAG_OPTIONAL;
                }
                if member.readonly {
                    flags |= SemanticTypeChild::FLAG_READONLY;
                }
                rows.push((row, Some(spelling), flags));
            }
            // A checker-synthesized namespace type (`typeof Ns` for a module
            // with more exported members than the lane holds per row) legally
            // exceeds the per-row bound. The member run therefore folds into
            // MAX_TYPE_CHILDREN-wide anonymous-record rows exactly as wide
            // unions fold: no member is lost, none nests deeper than the fold
            // requires, and every run at or under the bound stays unchanged.
            while rows.len() > MAX_TYPE_CHILDREN {
                let mut next: Vec<(u32, Option<&'source [u8]>, u8)> =
                    Vec::with_capacity(rows.len().div_ceil(MAX_TYPE_CHILDREN));
                for chunk in rows.chunks(MAX_TYPE_CHILDREN) {
                    for (target, name, flags) in chunk {
                        facts
                            .computed_type_child(*target, *name, *flags)
                            .map_err(|cause| computed_fault(registry, owner, cause))?;
                    }
                    let mut chunk_record =
                        SemanticTypeRecord::leaf(SemanticTypeTag::AnonymousRecord);
                    chunk_record.payload0 = u32::from(AnonRecordForm::Interface);
                    let folded = facts
                        .intern_computed_type_row(owner, chunk_record)
                        .map_err(|cause| computed_fault(registry, owner, cause))?;
                    // The anonymous-record child law names every member from
                    // source, so each folded row is named by the exact member
                    // that closes its chunk — the rightmost-member naming the
                    // pair fold used elsewhere in the lane.
                    let closes = chunk.last().and_then(|(_, name, _)| *name);
                    next.push((folded, closes, 0));
                }
                rows = next;
            }
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::AnonymousRecord);
            record.payload0 = u32::from(AnonRecordForm::Interface);
            for (target, name, flags) in &rows {
                facts
                    .computed_type_child(*target, *name, *flags)
                    .map_err(|cause| computed_fault(registry, owner, cause))?;
            }
            facts
                .intern_computed_type_row(owner, record)
                .map_err(|cause| computed_fault(registry, owner, cause))
        }
        TypeTree::Reference { name, module, args } => intern_computed_reference(
            registry,
            facts,
            name,
            module.as_deref(),
            args,
            owner,
            depth,
            spell,
        ),
    }
}

/// Maps native TSZ checker types directly into the existing schema-2 computed
/// lane. Unsupported shapes remain explicit OracleGap rows; no checker type
/// string is formatted, reparsed, or used as IR text.
fn intern_native_tsz_type<'source>(
    registry: &FactRegistry<'_, 'source>,
    facts: &mut FactSet<'source>,
    checker: &mut TszCheckerState<'_>,
    database: &dyn TszTypeDatabase,
    type_id: TszTypeId,
    owner: u32,
    depth: u8,
    active: &mut std::collections::HashSet<TszTypeId>,
) -> Result<u32, TypeScriptCollectError> {
    if depth > MAX_TYPE_DEPTH {
        return Err(computed_fault(
            registry,
            owner,
            FactFault::TypeProjectionDepthLimit {
                depth,
                maximum: MAX_TYPE_DEPTH,
            },
        ));
    }
    // A declaration's checker type can be a lazy definition handle even
    // when its body is a supported mapped/operator/structural type. Resolve
    // only that exact checker-owned handle through the active project
    // checker before interpreting TypeData; guessing from source spelling
    // would lose generic arguments and could confuse unrelated declarations.
    let resolved_type_id = checker.resolve_lazy_type(type_id);
    // `resolve_lazy_type` leaves unresolved handles lazy; a result of `any`
    // therefore comes from the checker-owned body and must keep its native
    // dynamically-typed meaning instead of being confused with a failed lookup.
    if !active.insert(resolved_type_id) {
        return Err(computed_fault(
            registry,
            owner,
            FactFault::TypeProjectionCycle { type_id: type_id.0 },
        ));
    }
    let result = intern_native_tsz_type_inner(
        registry,
        facts,
        checker,
        database,
        resolved_type_id,
        owner,
        depth,
        active,
    );
    active.remove(&resolved_type_id);
    result
}

fn intern_native_tsz_type_inner<'source>(
    registry: &FactRegistry<'_, 'source>,
    facts: &mut FactSet<'source>,
    checker: &mut TszCheckerState<'_>,
    database: &dyn TszTypeDatabase,
    type_id: TszTypeId,
    owner: u32,
    depth: u8,
    active: &mut std::collections::HashSet<TszTypeId>,
) -> Result<u32, TypeScriptCollectError> {
    let Some(data) = database.lookup(type_id) else {
        return intern_computed_leaf(facts, unknown_record(TypeReason::OracleGap), owner);
    };
    let next_depth = depth.saturating_add(1);
    match data {
        TypeData::Intrinsic(kind) => {
            intern_computed_leaf(facts, native_tsz_intrinsic(registry, owner, kind), owner)
        }
        TypeData::Literal(literal) => {
            intern_computed_leaf(facts, native_tsz_literal(literal), owner)
        }
        TypeData::Array(element) => {
            let child = intern_native_tsz_type(
                registry, facts, checker, database, element, owner, next_depth, active,
            )?;
            intern_native_tsz_row(
                registry,
                facts,
                SemanticTypeRecord::leaf(SemanticTypeTag::ArraySequence),
                owner,
                &[(child, None, 0)],
            )
        }
        TypeData::ReadonlyType(inner) => {
            let child = intern_native_tsz_type(
                registry, facts, checker, database, inner, owner, next_depth, active,
            )?;
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Annotated);
            record.payload0 = AnnotationKind::Readonly as u32;
            intern_native_tsz_row(registry, facts, record, owner, &[(child, None, 0)])
        }
        TypeData::NoInfer(inner) => intern_native_tsz_type(
            registry, facts, checker, database, inner, owner, next_depth, active,
        ),
        TypeData::Union(list) | TypeData::Intersection(list) => {
            let tag = if matches!(data, TypeData::Union(_)) {
                SemanticTypeTag::Union
            } else {
                SemanticTypeTag::Intersection
            };
            let members = database.type_list(list);
            intern_native_tsz_associative(
                registry, facts, checker, database, &members, owner, next_depth, active, tag,
            )
        }
        TypeData::Tuple(list) => {
            let elements = database.tuple_list(list);
            if elements.len() > MAX_TYPE_CHILDREN {
                return Err(computed_fault(
                    registry,
                    owner,
                    FactFault::TypeProjectionWidth {
                        actual: elements.len(),
                        maximum: MAX_TYPE_CHILDREN,
                    },
                ));
            }
            let mut children = Vec::with_capacity(elements.len());
            for element in elements.iter() {
                let child = intern_native_tsz_type(
                    registry,
                    facts,
                    checker,
                    database,
                    element.type_id,
                    owner,
                    next_depth,
                    active,
                )?;
                let name = element
                    .name
                    .and_then(|atom| native_tsz_source_name(registry, database, atom, owner));
                let mut flags = 0;
                if element.optional {
                    flags |= SemanticTypeChild::FLAG_OPTIONAL;
                }
                if element.rest {
                    flags |= SemanticTypeChild::FLAG_REST;
                }
                children.push((child, name, flags));
            }
            intern_native_tsz_row(
                registry,
                facts,
                SemanticTypeRecord::leaf(SemanticTypeTag::Tuple),
                owner,
                &children,
            )
        }
        TypeData::Function(shape_id) => {
            let shape = database.function_shape(shape_id);
            intern_native_tsz_function(
                registry, facts, checker, database, &shape, owner, next_depth, active,
            )
        }
        TypeData::Object(shape_id) | TypeData::ObjectWithIndex(shape_id) => {
            let shape = database.object_shape(shape_id);
            intern_native_tsz_object(
                registry, facts, checker, database, &shape, owner, next_depth, active,
            )
        }
        TypeData::Application(application_id) => {
            let application = database.type_application(application_id);
            if application.args.len().saturating_add(1) > MAX_TYPE_CHILDREN {
                return Err(computed_fault(
                    registry,
                    owner,
                    FactFault::TypeProjectionWidth {
                        actual: application.args.len().saturating_add(1),
                        maximum: MAX_TYPE_CHILDREN,
                    },
                ));
            }
            let mut children = Vec::with_capacity(application.args.len() + 1);
            children.push((
                intern_native_tsz_type(
                    registry,
                    facts,
                    checker,
                    database,
                    application.base,
                    owner,
                    next_depth,
                    active,
                )?,
                None,
                0,
            ));
            for argument in &application.args {
                children.push((
                    intern_native_tsz_type(
                        registry, facts, checker, database, *argument, owner, next_depth, active,
                    )?,
                    None,
                    0,
                ));
            }
            intern_native_tsz_row(
                registry,
                facts,
                SemanticTypeRecord::leaf(SemanticTypeTag::Apply),
                owner,
                &children,
            )
        }
        TypeData::Conditional(conditional_id) => {
            let conditional = database.conditional_type(conditional_id);
            if conditional.is_distributive {
                return intern_computed_leaf(facts, unknown_record(TypeReason::OracleGap), owner);
            }
            intern_native_tsz_conditional(
                registry,
                facts,
                checker,
                database,
                &conditional,
                owner,
                next_depth,
                active,
            )
        }
        TypeData::Mapped(mapped_id) => {
            let mapped = database.mapped_type(mapped_id);
            intern_native_tsz_mapped(
                registry, facts, checker, database, &mapped, owner, next_depth, active,
            )
        }
        TypeData::TemplateLiteral(template_id) => {
            let parts = database.template_list(template_id);
            if parts.len() > MAX_TYPE_CHILDREN {
                return Err(computed_fault(
                    registry,
                    owner,
                    FactFault::TypeProjectionWidth {
                        actual: parts.len(),
                        maximum: MAX_TYPE_CHILDREN,
                    },
                ));
            }
            // Resolve every cooked segment to exact owner bytes first, so a
            // failed name proof cannot leave an incomplete pending child run.
            let mut text_parts = Vec::with_capacity(parts.len());
            for part in parts.iter() {
                if let TemplateSpan::Text(atom) = part {
                    let text = database.resolve_atom_ref(*atom);
                    if text.is_empty() {
                        return intern_computed_leaf(
                            facts,
                            unknown_record(TypeReason::OracleGap),
                            owner,
                        );
                    }
                    let Some(source_text) = registry.source_spelling_in_domain(
                        SpellDomain::Owner,
                        text.as_bytes(),
                        owner,
                    ) else {
                        return intern_computed_leaf(
                            facts,
                            unknown_record(TypeReason::OracleGap),
                            owner,
                        );
                    };
                    text_parts.push(Some(source_text));
                } else {
                    text_parts.push(None);
                }
            }
            let mut staged = Vec::with_capacity(parts.len());
            for (part, text) in parts.iter().zip(text_parts) {
                staged.push(match (part, text) {
                    (TemplateSpan::Text(_), Some(text)) => ComputedTemplatePart::Text(text),
                    (TemplateSpan::Type(part_type), None) => {
                        ComputedTemplatePart::Type(intern_native_tsz_type(
                            registry, facts, checker, database, *part_type, owner, next_depth,
                            active,
                        )?)
                    }
                    _ => {
                        return intern_computed_leaf(
                            facts,
                            unknown_record(TypeReason::OracleGap),
                            owner,
                        );
                    }
                });
            }
            intern_computed_template(registry, facts, owner, &staged)
        }
        TypeData::TypeParameter(parameter) | TypeData::Infer(parameter) => {
            intern_native_tsz_type_parameter(registry, facts, database, parameter, owner)
        }
        TypeData::ThisType => intern_computed_leaf(
            facts,
            SemanticTypeRecord::leaf(SemanticTypeTag::SelfType),
            owner,
        ),
        TypeData::Substitution {
            base_type,
            constraint,
        } => intern_native_tsz_associative(
            registry,
            facts,
            checker,
            database,
            &[base_type, constraint],
            owner,
            next_depth,
            active,
            SemanticTypeTag::Intersection,
        ),
        TypeData::KeyOf(inner) => {
            let child = intern_native_tsz_type(
                registry, facts, checker, database, inner, owner, next_depth, active,
            )?;
            intern_native_tsz_row(
                registry,
                facts,
                SemanticTypeRecord::leaf(SemanticTypeTag::KeyOf),
                owner,
                &[(child, None, 0)],
            )
        }
        TypeData::IndexAccess(object, index) => {
            let object = intern_native_tsz_type(
                registry, facts, checker, database, object, owner, next_depth, active,
            )?;
            let index = intern_native_tsz_type(
                registry, facts, checker, database, index, owner, next_depth, active,
            )?;
            intern_native_tsz_row(
                registry,
                facts,
                SemanticTypeRecord::leaf(SemanticTypeTag::IndexedAccess),
                owner,
                &[(object, None, 0), (index, None, 0)],
            )
        }
        TypeData::TypeQuery(TszSymbolRef(symbol)) => {
            let Some(target) = registry
                .native_tsz_entities
                .and_then(|entities| entities.get(&TszSymbolId(symbol)))
                .copied()
            else {
                // Cross-file or otherwise unowned symbols remain an explicit
                // gap. Never translate a TSZ symbol index into an entity row
                // without the exact same-file source-span join above.
                return intern_computed_leaf(facts, unknown_record(TypeReason::OracleGap), owner);
            };
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::TypeOf);
            record.payload0 = target;
            intern_native_tsz_row(registry, facts, record, owner, &[])
        }
        TypeData::Recursive(distance) => Err(computed_fault(
            registry,
            owner,
            FactFault::TypeProjectionRecursiveReference { distance },
        )),
        TypeData::Callable(_)
        | TypeData::BoundParameter(_)
        | TypeData::Lazy(_)
        | TypeData::Enum(_, _)
        | TypeData::UniqueSymbol(_)
        | TypeData::StringIntrinsic { .. }
        | TypeData::ModuleNamespace(_)
        | TypeData::Error
        | TypeData::UnresolvedTypeName(_) => {
            intern_computed_leaf(facts, unknown_record(TypeReason::OracleGap), owner)
        }
    }
}

fn native_tsz_intrinsic<'source>(
    registry: &FactRegistry<'_, 'source>,
    owner: u32,
    kind: IntrinsicKind,
) -> SemanticTypeRecord<'source> {
    match kind {
        IntrinsicKind::Any => unknown_record(TypeReason::DynamicallyTyped),
        IntrinsicKind::Unknown => SemanticTypeRecord::leaf(SemanticTypeTag::Any),
        IntrinsicKind::Never => SemanticTypeRecord::leaf(SemanticTypeTag::Never),
        IntrinsicKind::Boolean => tsz_primitive(PrimitiveShape::Bool, None),
        IntrinsicKind::Number => tsz_primitive(PrimitiveShape::Float, Some(TypeWidth::Fixed(64))),
        IntrinsicKind::String => tsz_primitive(PrimitiveShape::Str, None),
        IntrinsicKind::Bigint => tsz_primitive(PrimitiveShape::ArbitraryInteger, None),
        IntrinsicKind::Null => tsz_builtin(registry, owner, b"null"),
        IntrinsicKind::Undefined => tsz_builtin(registry, owner, b"undefined"),
        IntrinsicKind::Void => tsz_builtin(registry, owner, b"void"),
        IntrinsicKind::Symbol => tsz_builtin(registry, owner, b"symbol"),
        IntrinsicKind::Object => tsz_builtin(registry, owner, b"object"),
        IntrinsicKind::Function => tsz_builtin(registry, owner, b"Function"),
    }
}

fn native_tsz_literal(literal: TszLiteral) -> SemanticTypeRecord<'static> {
    match literal {
        TszLiteral::String(_) => tsz_primitive(PrimitiveShape::Str, None),
        TszLiteral::Number(_) => tsz_primitive(PrimitiveShape::Float, Some(TypeWidth::Fixed(64))),
        TszLiteral::Boolean(_) => tsz_primitive(PrimitiveShape::Bool, None),
        TszLiteral::BigInt(_) => tsz_primitive(PrimitiveShape::ArbitraryInteger, None),
    }
}

fn tsz_primitive<'source>(
    shape: PrimitiveShape,
    width: Option<TypeWidth>,
) -> SemanticTypeRecord<'source> {
    let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
    record.payload0 = u32::from(shape);
    if let Some(width) = width {
        record.payload1 = width.to_cell();
    }
    record
}

fn tsz_builtin<'source>(
    registry: &FactRegistry<'_, 'source>,
    owner: u32,
    spelling: &'static [u8],
) -> SemanticTypeRecord<'source> {
    let Some(source_spelling) = native_tsz_source_name_bytes(registry, spelling, owner) else {
        return unknown_record(TypeReason::OracleGap);
    };
    let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
    record.payload0 = u32::from(PrimitiveShape::Builtin);
    record.text = Some(source_spelling);
    record
}

fn native_tsz_source_name<'source>(
    registry: &FactRegistry<'_, 'source>,
    database: &dyn TszTypeDatabase,
    atom: TszAtom,
    owner: u32,
) -> Option<&'source [u8]> {
    let name = database.resolve_atom_ref(atom);
    native_tsz_source_name_bytes(registry, name.as_bytes(), owner)
}

fn native_tsz_source_name_bytes<'source>(
    registry: &FactRegistry<'_, 'source>,
    name: &[u8],
    owner: u32,
) -> Option<&'source [u8]> {
    if name.is_empty() {
        return None;
    }
    let owner_index = usize::try_from(owner).ok()?;
    let start = *registry.decl_starts.get(owner_index)?;
    let end = *registry.decl_ends.get(owner_index)?;
    if start == UNSET || end == UNSET {
        return None;
    }
    let owner_text = registry
        .source
        .get(usize::try_from(start).ok()?..usize::try_from(end).ok()?)?;
    let bytes = owner_text.as_bytes();
    let mut from = 0;
    while let Some(at) = find_sub(bytes, name, from) {
        let name_end = at.checked_add(name.len())?;
        let left_is_part = at > 0 && identifier_byte(*bytes.get(at - 1)?);
        let right_is_part = name_end < bytes.len() && identifier_byte(*bytes.get(name_end)?);
        if !left_is_part && !right_is_part {
            let absolute_start = usize::try_from(start).ok()?.checked_add(at)?;
            let absolute_end = absolute_start.checked_add(name.len())?;
            return registry.source.as_bytes().get(absolute_start..absolute_end);
        }
        from = at.checked_add(1)?;
    }
    None
}

fn native_tsz_decl_scoped_parameter_name<'source>(
    registry: &FactRegistry<'_, 'source>,
    database: &dyn TszTypeDatabase,
    parameter: TypeParamInfo,
) -> Option<&'source [u8]> {
    let TszTypeParamOrigin::DeclScoped { file, node } = parameter.origin else {
        return None;
    };
    let project = registry.tsz_project?;
    let path = database.resolve_atom_ref(file);
    let expected = database.resolve_atom_ref(parameter.name);
    project
        .declaration_name_source(&path, node, &expected)
        .map(str::as_bytes)
}

const fn identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$' || byte >= 0x80
}

fn intern_native_tsz_type_parameter<'source>(
    registry: &FactRegistry<'_, 'source>,
    facts: &mut FactSet<'source>,
    database: &dyn TszTypeDatabase,
    parameter: TypeParamInfo,
    owner: u32,
) -> Result<u32, TypeScriptCollectError> {
    let Some(name) = native_tsz_decl_scoped_parameter_name(registry, database, parameter) else {
        return intern_computed_leaf(facts, unknown_record(TypeReason::OracleGap), owner);
    };
    let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::TypeVar);
    record.text = Some(name);
    intern_computed_leaf(facts, record, owner)
}

fn intern_native_tsz_function<'source>(
    registry: &FactRegistry<'_, 'source>,
    facts: &mut FactSet<'source>,
    checker: &mut TszCheckerState<'_>,
    database: &dyn TszTypeDatabase,
    function: &FunctionShape,
    owner: u32,
    depth: u8,
    active: &mut std::collections::HashSet<TszTypeId>,
) -> Result<u32, TypeScriptCollectError> {
    let child_count = function.params.len().saturating_add(1);
    if child_count > MAX_TYPE_CHILDREN {
        return Err(computed_fault(
            registry,
            owner,
            FactFault::TypeProjectionWidth {
                actual: child_count,
                maximum: MAX_TYPE_CHILDREN,
            },
        ));
    }
    if function.this_type.is_some() || function.type_predicate.is_some() || function.is_constructor
    {
        return intern_computed_leaf(facts, unknown_record(TypeReason::OracleGap), owner);
    }
    let mut children = Vec::with_capacity(function.params.len() + 1);
    for parameter in &function.params {
        children.push(native_tsz_parameter_child(
            registry, facts, checker, database, *parameter, owner, depth, active,
        )?);
    }
    let result = intern_native_tsz_type(
        registry,
        facts,
        checker,
        database,
        function.return_type,
        owner,
        depth,
        active,
    )?;
    children.push((result, None, 0));
    let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::FunctionPointer);
    record.payload1 = SemanticTypeRecord::FUNCTION_RESULT_COUNT_ONE;
    if function
        .params
        .last()
        .is_some_and(|parameter| parameter.rest)
    {
        record.payload0 = SemanticTypeRecord::FUNCTION_TYPED_VARIADIC_FLAG;
    }
    intern_native_tsz_row(registry, facts, record, owner, &children)
}

fn native_tsz_parameter_child<'source>(
    registry: &FactRegistry<'_, 'source>,
    facts: &mut FactSet<'source>,
    checker: &mut TszCheckerState<'_>,
    database: &dyn TszTypeDatabase,
    parameter: ParamInfo,
    owner: u32,
    depth: u8,
    active: &mut std::collections::HashSet<TszTypeId>,
) -> Result<(u32, Option<&'source [u8]>, u8), TypeScriptCollectError> {
    let child = intern_native_tsz_type(
        registry,
        facts,
        checker,
        database,
        parameter.type_id,
        owner,
        depth,
        active,
    )?;
    let name = parameter
        .name
        .and_then(|atom| native_tsz_source_name(registry, database, atom, owner));
    // TSZ marks unannotated JavaScript parameters optional for call arity,
    // including a rest parameter. The canonical grammar represents the
    // latter by the function's typed variadic tail, not OPTIONAL | REST.
    // Fixed optional parameters retain their independent optional marker.
    let flags = if parameter.rest {
        SemanticTypeChild::FLAG_REST
    } else if parameter.optional {
        SemanticTypeChild::FLAG_OPTIONAL
    } else {
        0
    };
    Ok((child, name, flags))
}

fn intern_native_tsz_conditional<'source>(
    registry: &FactRegistry<'_, 'source>,
    facts: &mut FactSet<'source>,
    checker: &mut TszCheckerState<'_>,
    database: &dyn TszTypeDatabase,
    conditional: &ConditionalType,
    owner: u32,
    depth: u8,
    active: &mut std::collections::HashSet<TszTypeId>,
) -> Result<u32, TypeScriptCollectError> {
    let mut children = Vec::with_capacity(4);
    for child_id in [
        conditional.check_type,
        conditional.extends_type,
        conditional.true_type,
        conditional.false_type,
    ] {
        children.push((
            intern_native_tsz_type(
                registry, facts, checker, database, child_id, owner, depth, active,
            )?,
            None,
            0,
        ));
    }
    intern_native_tsz_row(
        registry,
        facts,
        SemanticTypeRecord::leaf(SemanticTypeTag::Conditional),
        owner,
        &children,
    )
}

fn intern_native_tsz_mapped<'source>(
    registry: &FactRegistry<'_, 'source>,
    facts: &mut FactSet<'source>,
    checker: &mut TszCheckerState<'_>,
    database: &dyn TszTypeDatabase,
    mapped: &MappedType,
    owner: u32,
    depth: u8,
    active: &mut std::collections::HashSet<TszTypeId>,
) -> Result<u32, TypeScriptCollectError> {
    let Some(name) = native_tsz_decl_scoped_parameter_name(registry, database, mapped.type_param)
    else {
        return intern_computed_leaf(facts, unknown_record(TypeReason::OracleGap), owner);
    };
    let mut children = Vec::with_capacity(3);
    for child_id in [
        Some(mapped.constraint),
        mapped.name_type,
        Some(mapped.template),
    ]
    .into_iter()
    .flatten()
    {
        children.push((
            intern_native_tsz_type(
                registry, facts, checker, database, child_id, owner, depth, active,
            )?,
            None,
            0,
        ));
    }
    let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Mapped);
    record.text = Some(name);
    record.payload0 = native_tsz_mapped_modifier(mapped.readonly_modifier);
    record.payload1 = native_tsz_mapped_modifier(mapped.optional_modifier);
    intern_native_tsz_row(registry, facts, record, owner, &children)
}

fn native_tsz_mapped_modifier(modifier: Option<TszMappedModifier>) -> u32 {
    u32::from(match modifier {
        Some(TszMappedModifier::Add) => LatticeMappedModifier::Add,
        Some(TszMappedModifier::Remove) => LatticeMappedModifier::Remove,
        None => LatticeMappedModifier::Absent,
    })
}

fn intern_native_tsz_associative<'source>(
    registry: &FactRegistry<'_, 'source>,
    facts: &mut FactSet<'source>,
    checker: &mut TszCheckerState<'_>,
    database: &dyn TszTypeDatabase,
    members: &[TszTypeId],
    owner: u32,
    depth: u8,
    active: &mut std::collections::HashSet<TszTypeId>,
    tag: SemanticTypeTag,
) -> Result<u32, TypeScriptCollectError> {
    if members.is_empty() {
        return intern_computed_leaf(facts, unknown_record(TypeReason::OracleGap), owner);
    }
    let mut rows = Vec::with_capacity(members.len());
    for member in members {
        rows.push(intern_native_tsz_type(
            registry, facts, checker, database, *member, owner, depth, active,
        )?);
    }
    while rows.len() > MAX_TYPE_CHILDREN {
        let mut next = Vec::with_capacity(rows.len().div_ceil(MAX_TYPE_CHILDREN));
        for chunk in rows.chunks(MAX_TYPE_CHILDREN) {
            let children: Vec<_> = chunk.iter().map(|row| (*row, None, 0)).collect();
            next.push(intern_native_tsz_row(
                registry,
                facts,
                SemanticTypeRecord::leaf(tag),
                owner,
                &children,
            )?);
        }
        rows = next;
    }
    let children: Vec<_> = rows.into_iter().map(|row| (row, None, 0)).collect();
    intern_native_tsz_row(
        registry,
        facts,
        SemanticTypeRecord::leaf(tag),
        owner,
        &children,
    )
}

type NativeTszOwnerNameSources<'source> =
    HashMap<TszSymbolId, HashMap<&'source str, Option<&'source str>>>;

fn native_tsz_owner_name_sources<'source>(
    project: Option<&'source TszProject>,
    database: &dyn TszTypeDatabase,
    shape: &TszObjectShape,
) -> Result<Option<NativeTszOwnerNameSources<'source>>, TypeScriptCollectError> {
    // Build a local borrowed index once for each exact declaring owner. All
    // ownership scans finish before any object children are interned; a stopped
    // scan drops its local index and propagates the shared typed stop reason.
    let mut owner_names: HashMap<TszSymbolId, HashMap<&str, Option<&str>>> = HashMap::new();
    for property in &shape.properties {
        database.execution_checkpoint(1).map_err(|stop| {
            TypeScriptCollectError::TszAuthority(TszAuthorityError::ExecutionStopped(stop))
        })?;
        if property.is_symbol_named || property.write_type != property.type_id {
            return Ok(None);
        }
        if let Some(symbol) = property.parent_id {
            if let std::collections::hash_map::Entry::Vacant(entry) = owner_names.entry(symbol) {
                let mut names = HashMap::new();
                if let Some(project) = project {
                    project
                        .with_property_name_sources_from_owner(
                            symbol,
                            |work| database.execution_checkpoint(work),
                            |name, raw| match names.entry(name) {
                                std::collections::hash_map::Entry::Vacant(entry) => {
                                    entry.insert(Some(raw));
                                }
                                std::collections::hash_map::Entry::Occupied(mut entry) => {
                                    if entry.get().is_some_and(|previous| previous != raw) {
                                        entry.insert(None);
                                    }
                                }
                            },
                        )
                        .map_err(|stop| {
                            TypeScriptCollectError::TszAuthority(
                                TszAuthorityError::ExecutionStopped(stop),
                            )
                        })?;
                }
                entry.insert(names);
            }
        }
    }

    Ok(Some(owner_names))
}

fn intern_native_tsz_object<'source>(
    registry: &FactRegistry<'_, 'source>,
    facts: &mut FactSet<'source>,
    checker: &mut TszCheckerState<'_>,
    database: &dyn TszTypeDatabase,
    shape: &TszObjectShape,
    owner: u32,
    depth: u8,
    active: &mut std::collections::HashSet<TszTypeId>,
) -> Result<u32, TypeScriptCollectError> {
    if shape.symbol.is_some() {
        return intern_computed_leaf(facts, unknown_record(TypeReason::OracleGap), owner);
    }

    let Some(owner_names) = native_tsz_owner_name_sources(registry.tsz_project, database, shape)?
    else {
        return intern_computed_leaf(facts, unknown_record(TypeReason::OracleGap), owner);
    };

    // Anonymous structural properties retain their separate parsed declaration
    // route. Owner-backed lookups use decoded names as borrowed index keys and
    // refuse conflicting raw spellings without selecting a declaration by order.
    let mut members = Vec::with_capacity(shape.properties.len());
    for property in &shape.properties {
        let name = database.resolve_atom_ref(property.name);
        database
            .execution_checkpoint(
                u64::try_from(name.len())
                    .unwrap_or(u64::MAX)
                    .saturating_add(1),
            )
            .map_err(|stop| {
                TypeScriptCollectError::TszAuthority(TszAuthorityError::ExecutionStopped(stop))
            })?;
        let spelling = match property.parent_id {
            Some(symbol) => owner_names
                .get(&symbol)
                .and_then(|names| names.get(name.as_ref()))
                .copied()
                .flatten(),
            None => usize::try_from(owner).ok().and_then(|owner_index| {
                let start = *registry.decl_starts.get(owner_index)?;
                let end = *registry.decl_ends.get(owner_index)?;
                if start == UNSET || end == UNSET {
                    return None;
                }
                registry.tsz_project?.property_name_source_in_declaration(
                    registry.source_path.as_deref()?,
                    (start, end),
                    &name,
                )
            }),
        }
        .map(str::as_bytes);
        let Some(spelling) = spelling else {
            return intern_computed_leaf(facts, unknown_record(TypeReason::OracleGap), owner);
        };
        members.push((property, spelling));
    }

    let mut parts = Vec::with_capacity(4);
    let mut rows = Vec::with_capacity(members.len());
    for (property, spelling) in members {
        let child = intern_native_tsz_type(
            registry,
            facts,
            checker,
            database,
            property.type_id,
            owner,
            depth,
            active,
        )?;
        let mut flags = 0;
        if property.optional {
            flags |= SemanticTypeChild::FLAG_OPTIONAL;
        }
        if property.readonly {
            flags |= SemanticTypeChild::FLAG_READONLY;
        }
        rows.push((child, Some(spelling), flags));
    }

    // Each anonymous-record row keeps the original member labels. If a
    // checker object exceeds the row bound, use associative intersections
    // between complete record chunks. Naming a folded record after one of its
    // fields would silently change the shape and lose top-level members.
    if !rows.is_empty() {
        let mut record_parts = Vec::with_capacity(rows.len().div_ceil(MAX_TYPE_CHILDREN));
        for chunk in rows.chunks(MAX_TYPE_CHILDREN) {
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::AnonymousRecord);
            record.payload0 = u32::from(AnonRecordForm::Interface);
            record_parts.push(intern_native_tsz_row(
                registry, facts, record, owner, chunk,
            )?);
        }
        while record_parts.len() > MAX_TYPE_CHILDREN {
            let mut next = Vec::with_capacity(record_parts.len().div_ceil(MAX_TYPE_CHILDREN));
            for chunk in record_parts.chunks(MAX_TYPE_CHILDREN) {
                let children = chunk
                    .iter()
                    .map(|part| (*part, None, 0))
                    .collect::<Vec<_>>();
                next.push(intern_native_tsz_row(
                    registry,
                    facts,
                    SemanticTypeRecord::leaf(SemanticTypeTag::Intersection),
                    owner,
                    &children,
                )?);
            }
            record_parts = next;
        }
        let named = match record_parts.as_slice() {
            [only] => *only,
            many => {
                let children = many.iter().map(|part| (*part, None, 0)).collect::<Vec<_>>();
                intern_native_tsz_row(
                    registry,
                    facts,
                    SemanticTypeRecord::leaf(SemanticTypeTag::Intersection),
                    owner,
                    &children,
                )?
            }
        };
        parts.push(named);
    } else if shape.string_index.is_none()
        && shape.number_index.is_none()
        && shape.symbol_index.is_none()
    {
        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::AnonymousRecord);
        record.payload0 = u32::from(AnonRecordForm::Interface);
        parts.push(intern_native_tsz_row(registry, facts, record, owner, &[])?);
    }

    for signature in [shape.string_index, shape.number_index, shape.symbol_index]
        .into_iter()
        .flatten()
    {
        parts.push(intern_native_tsz_index_signature(
            registry, facts, checker, database, signature, owner, depth, active,
        )?);
    }

    match parts.as_slice() {
        [only] => Ok(*only),
        [] => intern_computed_leaf(facts, unknown_record(TypeReason::OracleGap), owner),
        _ => {
            let children = parts
                .iter()
                .map(|part| (*part, None, 0))
                .collect::<Vec<_>>();
            intern_native_tsz_row(
                registry,
                facts,
                SemanticTypeRecord::leaf(SemanticTypeTag::Intersection),
                owner,
                &children,
            )
        }
    }
}

fn intern_native_tsz_index_signature<'source>(
    registry: &FactRegistry<'_, 'source>,
    facts: &mut FactSet<'source>,
    checker: &mut TszCheckerState<'_>,
    database: &dyn TszTypeDatabase,
    signature: TszIndexSignature,
    owner: u32,
    depth: u8,
    active: &mut std::collections::HashSet<TszTypeId>,
) -> Result<u32, TypeScriptCollectError> {
    let key = intern_native_tsz_type(
        registry,
        facts,
        checker,
        database,
        signature.key_type,
        owner,
        depth,
        active,
    )?;
    let value = intern_native_tsz_type(
        registry,
        facts,
        checker,
        database,
        signature.value_type,
        owner,
        depth,
        active,
    )?;
    let map = intern_native_tsz_row(
        registry,
        facts,
        SemanticTypeRecord::leaf(SemanticTypeTag::Map),
        owner,
        &[(key, None, 0), (value, None, 0)],
    )?;
    if signature.readonly {
        let mut readonly = SemanticTypeRecord::leaf(SemanticTypeTag::Annotated);
        readonly.payload0 = AnnotationKind::Readonly as u32;
        intern_native_tsz_row(registry, facts, readonly, owner, &[(map, None, 0)])
    } else {
        Ok(map)
    }
}

fn intern_native_tsz_row<'source>(
    registry: &FactRegistry<'_, 'source>,
    facts: &mut FactSet<'source>,
    record: SemanticTypeRecord<'source>,
    owner: u32,
    children: &[(u32, Option<&'source [u8]>, u8)],
) -> Result<u32, TypeScriptCollectError> {
    if children.len() > MAX_TYPE_CHILDREN {
        return Err(computed_fault(
            registry,
            owner,
            FactFault::TypeProjectionWidth {
                actual: children.len(),
                maximum: MAX_TYPE_CHILDREN,
            },
        ));
    }
    for (target, name, flags) in children {
        facts
            .computed_type_child(*target, *name, *flags)
            .map_err(|cause| computed_fault(registry, owner, cause))?;
    }
    facts
        .intern_computed_type_row(owner, record)
        .map_err(|cause| computed_fault(registry, owner, cause))
}

/// Interns one checker-computed reference. Foreign bases retain a typed external
/// nominal row, so applying one keeps the constructor rather than decaying
/// to an unknown record.
fn intern_computed_reference<'source>(
    registry: &FactRegistry<'_, 'source>,
    facts: &mut FactSet<'source>,
    name: &str,
    module: Option<&str>,
    args: &[TypeTree],
    owner: u32,
    depth: u8,
    spell: SpellDomain,
) -> Result<u32, TypeScriptCollectError> {
    // The library array shape keeps the lane's own array record.
    if module == Some("typescript")
        && (name == "Array" || name == "ReadonlyArray")
        && args.len() == 1
    {
        let element = args.first().ok_or_else(lane_rejection)?.clone();
        return intern_computed_tree(
            registry,
            facts,
            &TypeTree::Array {
                element: Box::new(element),
            },
            owner,
            depth.saturating_add(1),
            spell,
        );
    }
    let local = module
        .is_none()
        .then(|| registry.fact_by_name_bytes(name.as_bytes()))
        .flatten();
    let local_fact = local;
    let base = match local_fact {
        Some(fact) => fact,
        None => {
            // A checker name the declaration never spells (a synthesized
            // `__type`, a printed tuple, or a lib-internal spelling) cannot
            // back a spelling-bearing unknown row and cannot become a
            // nominal-external row, whose owned-IR conversion rejects an
            // absent text cell as a dangling atom. It stays an honest
            // oracle-gap unknown, whose reason owns no text cell. The
            // foreign occurrence link still retains the exact endpoint.
            let Some(text) = registry.source_spelling(spell, name.as_bytes(), owner) else {
                return intern_computed_leaf(facts, unknown_record(TypeReason::OracleGap), owner);
            };
            if module.is_none() {
                let mut record = unknown_record(TypeReason::UnresolvedExternal);
                record.text = Some(text);
                return intern_computed_leaf(facts, record, owner);
            }
            let module_bytes = module.map_or(name.as_bytes(), str::as_bytes);
            let fragment = ExternalFragmentId::from_canonical_bytes(module_bytes);
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Nominal);
            record.nominal = Some(NominalRef::External(ExternalEntityRef::bind(fragment, 0)));
            record.text = Some(text);
            intern_computed_leaf(facts, record, owner)?
        }
    };
    if args.is_empty() {
        if local.is_none() {
            // A foreign base already owns the honest unknown row above; it is
            // the computed root, not an entity ordinal for a nominal cell.
            return Ok(base);
        }
        // A bare reference is still its own computed row: every computed
        // declaration owns exactly one root row in the anonymous pool, in
        // pool order, so the computed-cell mint stays aligned.
        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Nominal);
        record.nominal = Some(NominalRef::Local(EntityId::new(base)));
        return intern_computed_leaf(facts, record, owner);
    }
    let mut children = [0_u32; MAX_TYPE_CHILDREN];
    let mut len = 0_usize;
    if let Some(slot) = children.first_mut() {
        *slot = base;
        len = 1;
    }
    for argument in args {
        let slot = children.get_mut(len).ok_or_else(lane_rejection)?;
        *slot = intern_computed_tree(
            registry,
            facts,
            argument,
            owner,
            depth.saturating_add(1),
            spell,
        )?;
        len += 1;
    }
    intern_computed_row(
        registry,
        facts,
        SemanticTypeRecord::leaf(SemanticTypeTag::Apply),
        owner,
        &children[..len],
    )
}

/// Folds an arbitrarily wide union or intersection into binary rows. Every
/// source member remains reachable while each row obeys the fixed child lane.
fn intern_computed_associative<'source>(
    registry: &FactRegistry<'_, 'source>,
    facts: &mut FactSet<'source>,
    members: &[TypeTree],
    owner: u32,
    depth: u8,
    spell: SpellDomain,
    tag: SemanticTypeTag,
) -> Result<u32, TypeScriptCollectError> {
    if members.len() <= MAX_TYPE_CHILDREN {
        let children = intern_computed_children(registry, facts, members, owner, depth, spell)?;
        return intern_computed_row(
            registry,
            facts,
            SemanticTypeRecord::leaf(tag),
            owner,
            &children[..members.len()],
        );
    }
    // A wide union/intersection folds into MAX_TYPE_CHILDREN-wide rows rather
    // than binary pairs: the tag's child law is unbounded, so a 3000-member
    // union needs ~48 rows instead of 2999, staying inside the fixed computed
    // row lane without losing any member or nesting it deeper than necessary.
    let mut rows: Vec<u32> = Vec::with_capacity(members.len());
    for member in members {
        rows.push(intern_computed_tree(
            registry,
            facts,
            member,
            owner,
            depth.saturating_add(1),
            spell,
        )?);
    }
    while rows.len() > MAX_TYPE_CHILDREN {
        let mut next: Vec<u32> = Vec::with_capacity(rows.len().div_ceil(MAX_TYPE_CHILDREN));
        for chunk in rows.chunks(MAX_TYPE_CHILDREN) {
            next.push(intern_computed_row(
                registry,
                facts,
                SemanticTypeRecord::leaf(tag),
                owner,
                chunk,
            )?);
        }
        rows = next;
    }
    intern_computed_row(registry, facts, SemanticTypeRecord::leaf(tag), owner, &rows)
}

fn intern_computed_children<'source>(
    registry: &FactRegistry<'_, 'source>,
    facts: &mut FactSet<'source>,
    trees: &[TypeTree],
    owner: u32,
    depth: u8,
    spell: SpellDomain,
) -> Result<[u32; MAX_TYPE_CHILDREN], TypeScriptCollectError> {
    let mut children = [0_u32; MAX_TYPE_CHILDREN];
    for (position, tree) in trees.iter().enumerate() {
        let slot = children.get_mut(position).ok_or_else(lane_rejection)?;
        *slot = intern_computed_tree(registry, facts, tree, owner, depth.saturating_add(1), spell)?;
    }
    Ok(children)
}

/// Links one bounded child run under a new anonymous row and interns it.
fn intern_computed_row<'source>(
    registry: &FactRegistry<'_, 'source>,
    facts: &mut FactSet<'source>,
    record: SemanticTypeRecord<'source>,
    owner: u32,
    children: &[u32],
) -> Result<u32, TypeScriptCollectError> {
    for child in children {
        facts
            .computed_type_child(*child, None, 0)
            .map_err(|cause| computed_fault(registry, owner, cause))?;
    }
    facts
        .intern_computed_type_row(owner, record)
        .map_err(|cause| computed_fault(registry, owner, cause))
}

/// A fully lowered template segment. Type coordinates already own committed
/// rows; text borrows the exact source lease. No recursion may run while a
/// template's pooled child run is pending, because the next computed row
/// consumes that run regardless of its constructor.
#[derive(Clone, Copy)]
enum ComputedTemplatePart<'source> {
    Text(&'source [u8]),
    Type(u32),
}

fn intern_computed_template<'source>(
    registry: &FactRegistry<'_, 'source>,
    facts: &mut FactSet<'source>,
    owner: u32,
    parts: &[ComputedTemplatePart<'source>],
) -> Result<u32, TypeScriptCollectError> {
    for part in parts {
        match part {
            ComputedTemplatePart::Text(text) => facts.computed_type_text_child(text),
            ComputedTemplatePart::Type(target) => facts.computed_type_child(*target, None, 0),
        }
        .map_err(|cause| computed_fault(registry, owner, cause))?;
    }
    facts
        .intern_computed_type_row(
            owner,
            SemanticTypeRecord::leaf(SemanticTypeTag::TemplateLiteral),
        )
        .map_err(|cause| computed_fault(registry, owner, cause))
}

fn intern_computed_leaf<'source>(
    facts: &mut FactSet<'source>,
    record: SemanticTypeRecord<'source>,
    owner: u32,
) -> Result<u32, TypeScriptCollectError> {
    facts.intern_computed_type_row(owner, record).map_err(fault)
}

/// The closed primitive record of one checker primitive spelling.
fn checker_primitive(name: &str) -> Result<SemanticTypeRecord<'static>, TypeScriptCollectError> {
    let record = match name {
        "number" => {
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
            record.payload0 = u32::from(PrimitiveShape::Float);
            record.payload1 = TypeWidth::Fixed(64).to_cell();
            record
        }
        "string" => {
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
            record.payload0 = u32::from(PrimitiveShape::Str);
            record
        }
        "boolean" => {
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
            record.payload0 = u32::from(PrimitiveShape::Bool);
            record
        }
        "bigint" => {
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
            record.payload0 = u32::from(PrimitiveShape::Builtin);
            record.text = Some(&b"bigint"[..]);
            record
        }
        "void" => {
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
            record.payload0 = u32::from(PrimitiveShape::Builtin);
            record.text = Some(&b"void"[..]);
            record
        }
        "null" => {
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
            record.payload0 = u32::from(PrimitiveShape::Builtin);
            record.text = Some(&b"null"[..]);
            record
        }
        "undefined" => {
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
            record.payload0 = u32::from(PrimitiveShape::Builtin);
            record.text = Some(&b"undefined"[..]);
            record
        }
        "symbol" => {
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
            record.payload0 = u32::from(PrimitiveShape::Builtin);
            record.text = Some(&b"symbol"[..]);
            record
        }
        "never" => SemanticTypeRecord::leaf(SemanticTypeTag::Never),
        "unknown" | "object" | "ESObject" => SemanticTypeRecord::leaf(SemanticTypeTag::Any),
        "any" => unknown_record(TypeReason::DynamicallyTyped),
        // The vendored driver emits only the closed spelling set above; a
        // foreign spelling is an oracle gap with no lattice slot and no
        // borrowed spelling to retain.
        _ => unknown_record(TypeReason::OracleGap),
    };
    Ok(record)
}

#[cfg(test)]
mod projection_tests {
    use super::{TypeScriptCollectError, foreign_fault, grow_side_lane, lineage_fault};
    use backend_frontend_typescript::legacy::Span;
    use backend_semantic::ir::{ForeignKeyFault, PackageLineageFault};
    use backend_semantic::vocabulary::{
        ProjectionForeignKeyFault, ProjectionLineagePart, ProjectionPackageLineageFault,
        TypeScriptProjectionFault,
    };

    #[test]
    fn projector_sidecars_grow_with_fact_demand_and_keep_the_protocol_ceiling() {
        let mut lane = Vec::new();
        grow_side_lane(&mut lane, 1, super::MAX_EMISSION_FACTS, u32::MAX)
            .expect("first fact reserves a small sidecar prefix");
        assert_eq!(lane.len(), 8);
        assert_ne!(lane.len(), super::MAX_EMISSION_FACTS);

        grow_side_lane(&mut lane, 9, super::MAX_EMISSION_FACTS, u32::MAX)
            .expect("growing demand extends the lane geometrically");
        assert_eq!(lane.len(), 16);

        grow_side_lane(
            &mut lane,
            super::MAX_EMISSION_FACTS,
            super::MAX_EMISSION_FACTS,
            u32::MAX,
        )
        .expect("the exact protocol maximum remains admissible");
        assert_eq!(lane.len(), super::MAX_EMISSION_FACTS);
        assert!(matches!(
            grow_side_lane(
                &mut lane,
                super::MAX_EMISSION_FACTS + 1,
                super::MAX_EMISSION_FACTS,
                u32::MAX,
            ),
            Err(TypeScriptCollectError::Lowering(_))
        ));
    }

    /// A cross-package key grammar failure must remain distinguishable from
    /// an unsupported declaration at the TypeScript compile boundary.
    #[test]
    fn foreign_key_fault_retains_its_exact_closed_cause() {
        let TypeScriptCollectError::Projection(TypeScriptProjectionFault::ForeignKey {
            start,
            end,
            cause,
        }) = foreign_fault(ForeignKeyFault::BackslashInPath, Span::new(2, 7))
        else {
            panic!("foreign-key projection terminal was erased");
        };
        assert_eq!(
            (start, end, cause),
            (2, 7, ProjectionForeignKeyFault::BackslashInPath)
        );
    }

    /// A malformed authority lineage retains the exact invalid component
    /// rather than being relabelled as the npm universe.
    #[test]
    fn lineage_fault_retains_the_raw_invalid_component() {
        let TypeScriptCollectError::Projection(TypeScriptProjectionFault::PackageLineage {
            start,
            end,
            cause:
                ProjectionPackageLineageFault::Backslash {
                    part: ProjectionLineagePart::Invalid { segment },
                },
        }) = lineage_fault(
            PackageLineageFault::Backslash { segment: 7 },
            Span::new(11, 19),
        )
        else {
            panic!("package-lineage projection terminal was erased");
        };
        assert_eq!((start, end, segment), (11, 19, 7));
    }
}

#[cfg(test)]
mod lane_tests {
    use backend_semantic::ir::{ForeignOrigin, OccurrenceConfidence};
    use super::{TypeScriptCollectError, collect_with_checker, collect_with_tsz};
    use crate::driver::lower::{AdmissionFault, FactSet, admit};
    use backend_frontend_typescript::legacy::{
        GetSpan, Reference, Report, SymbolFlags, source_digest, with_analysis,
    };
    use backend_frontend_typescript::{
        TszCheckerOptions, TszEnvironmentFingerprint, TszFileInput, TszProject,
        TszProjectAuthority, TszProjectModuleRequestKind, TszProjectModuleResolution,
        TszProjectModuleResolutionTarget, TszProjectOptions, TszProjectSemanticOptions,
    };
    use backend_semantic::ir::{
        AnonymousCallableAnchorView, AnonymousCallableFamilyMultiplicity, CallableChildRole,
        CallableParentShape, CallableTypeContainerKind, ComputedType, DeclarationFamilyId,
        EntityId, EntityKind, FragmentError, FragmentView, ItemName, LinkKind, OccurrenceFault,
        OccurrenceTarget, ProductChildRole, ReferenceKind, SemanticReader, TYPESCRIPT_TSZ_SOURCE_ECOSYSTEM, TypeExpr,
        TypeId, TypeQuery, TypeScriptSourceCoordinate, VariantFingerprint,
    };
    use backend_semantic::vocabulary::{
        CompileRecipeFact, LanguageProfile, NativeTool, Stage, TypeScriptSource,
    };
    use backend_version::{ContentId, SourceFactDomain, ToolchainDomain};
    use thiserror::Error;

    #[test]
    fn declared_member_inventory_is_direct_and_partial_or_merged_stays_unavailable()
    -> Result<(), LaneError> {
        use backend_semantic::ir::FactAvailability;
        let ir = owned_ir(
            concat!(
                "class Base { inherited: number = 1; }\n",
                "class Child extends Base { own: string = 'x'; run(arg: number): number { return arg; } }\n",
                "class Empty {}\n",
                "class Runtime { static { const local = 1; } }\n",
                "class ParameterProperty { constructor(public field: number) {} }\n",
                "class Outer { run() { class Nested { constructor(public nested: number) {} } return Nested; } }\n",
                "interface Written { value: number; run(arg: string): void; }\n",
                "interface Merged { first: number; } interface Merged { second: number; }\n",
                "enum Choice { First, Second }\n",
            ),
            None,
        )?;
        for (name, expected) in [
            (&b"Child"[..], &[&b"own"[..], &b"run"[..]][..]),
            (&b"Empty"[..], &[][..]),
            (&b"Written"[..], &[&b"value"[..], &b"run"[..]][..]),
            (&b"Choice"[..], &[&b"First"[..], &b"Second"[..]][..]),
            (&b"Outer"[..], &[&b"run"[..]][..]),
            (
                &b"ParameterProperty"[..],
                &[&b"constructor"[..], &b"field"[..]][..],
            ),
        ] {
            let item = ir
                .items()
                .find(|item| item.name() == name)
                .ok_or(LaneError::Missing("inventory owner"))?;
            assert_eq!(
                ir.entity(item.id())
                    .expect("emitted entity")
                    .authority
                    .members,
                FactAvailability::Captured,
                "{name:?}"
            );
            let mut names = item
                .members()
                .iter()
                .map(|member| {
                    ir.item(*member)
                        .expect("captured member")
                        .name()
                        .named_bytes()
                        .expect("TypeScript inventory fixture member is named")
                        .to_vec()
                })
                .collect::<Vec<_>>();
            names.sort();
            let mut expected = expected
                .iter()
                .map(|name| name.to_vec())
                .collect::<Vec<_>>();
            expected.sort();
            assert_eq!(names, expected);
        }
        for name in [&b"Runtime"[..], &b"Merged"[..]] {
            let item = ir
                .items()
                .find(|item| item.name() == name)
                .ok_or(LaneError::Missing("unavailable owner"))?;
            assert_eq!(
                ir.entity(item.id())
                    .expect("emitted entity")
                    .authority
                    .members,
                FactAvailability::Unavailable
            );
            assert_eq!(item.members().len(), 0);
        }
        Ok(())
    }

    /// Typed fixture failure; every assertion failure names what was missing.
    #[derive(Debug, Error)]
    enum LaneError {
        #[error("TypeScript lane collection failed: {0:?}")]
        Collection(TypeScriptCollectError),
        #[error("lane admission rejected the fact set: {0:?}")]
        Admission(AdmissionFault),
        #[error("fragment validation rejected the bytes: {0}")]
        Validate(#[from] FragmentError),
        #[error("occurrence cursor rejected: {0:?}")]
        Occurrence(#[from] OccurrenceFault),
        #[error("owned image build rejected the fact set: {0:?}")]
        Build(#[from] backend_semantic::ir::BuildError),
        #[error("fixture scalar conversion failed")]
        Scalar,
        #[error("expected {0}")]
        Missing(&'static str),
        #[error("configured TypeScript project authority failed: {0}")]
        ConfiguredProject(String),
    }

    impl From<std::num::TryFromIntError> for LaneError {
        fn from(_: std::num::TryFromIntError) -> Self {
            Self::Scalar
        }
    }

    impl From<TypeScriptCollectError> for LaneError {
        fn from(cause: TypeScriptCollectError) -> Self {
            Self::Collection(cause)
        }
    }

    impl From<AdmissionFault> for LaneError {
        fn from(cause: AdmissionFault) -> Self {
            Self::Admission(cause)
        }
    }

    /// Lowers one fixture source with one caller-supplied checker report and
    /// returns its validated compact fragment.
    fn lower_fragment(
        source: &str,
        report: Option<&Report>,
    ) -> Result<FragmentView<'static>, LaneError> {
        let mut facts = FactSet::new();
        collect_with_checker(
            TypeScriptSource::TypeScript,
            source.as_bytes(),
            report,
            &mut facts,
        )
        .map_err(LaneError::from)?;
        let identity = backend_semantic::ir::SourceIdentity {
            identity: ContentId::<SourceFactDomain>::from_canonical_bytes(source.as_bytes()),
            byte_len: u32::try_from(source.len())?,
        };
        let recipe = CompileRecipeFact::derive(
            LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
            Stage::LowerIr,
            NativeTool::TypeScriptCompiler,
            ContentId::<SourceFactDomain>::from_canonical_bytes(source.as_bytes()),
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"typescript-oxc-lane-fixture"),
        );
        let mut output = vec![0xa5_u8; 65_536];
        let length = admit(&facts, identity, recipe, recipe.profile, &mut output)?.len();
        if !output[length..].iter().all(|byte| *byte == 0xa5) {
            return Err(LaneError::Missing("untouched output tail"));
        }
        output.truncate(length);
        let leaked: &'static [u8] = Box::leak(output.into_boxed_slice());
        FragmentView::validate(leaked).map_err(LaneError::from)
    }

    /// Builds the owned semantic image for one fixture source, so assertions
    /// can read entity provenance spans and absolute occurrence sites.
    fn owned_ir(
        source: &str,
        report: Option<&Report>,
    ) -> Result<backend_semantic::ir::Ir, LaneError> {
        let mut facts = FactSet::new();
        collect_with_checker(
            TypeScriptSource::TypeScript,
            source.as_bytes(),
            report,
            &mut facts,
        )
        .map_err(LaneError::from)?;
        let identity = backend_semantic::ir::SourceIdentity {
            identity: ContentId::<SourceFactDomain>::from_canonical_bytes(source.as_bytes()),
            byte_len: u32::try_from(source.len())?,
        };
        let recipe = CompileRecipeFact::derive(
            LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
            Stage::LowerIr,
            NativeTool::TypeScriptCompiler,
            ContentId::<SourceFactDomain>::from_canonical_bytes(source.as_bytes()),
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"typescript-oxc-lane-fixture"),
        );
        facts
            .build_ir(
                LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
                identity,
                recipe,
                crate::driver::types::DeclarationScope::fixture(),
            )
            .map_err(LaneError::from)
    }

    fn native_observation(
        ir: &backend_semantic::ir::Ir,
        name: &[u8],
    ) -> Option<(TypeId, TypeExpr)> {
        let item = ir.items().find(|item| item.name() == name)?;
        let observed = ir.typescript_extension(item.id())?.observed?;
        Some((observed, ir.ty(observed)?))
    }

    /// Runs a real in-process TSZ project over the exact fixture bytes and
    /// lowers its native checker facts through the canonical TypeScript lane.
    fn owned_tsz_ir(source: &str) -> Result<backend_semantic::ir::Ir, LaneError> {
        owned_tsz_ir_at_path(source, "input.ts")
    }

    fn owned_tsz_ir_at_path(
        source: &str,
        path: &str,
    ) -> Result<backend_semantic::ir::Ir, LaneError> {
        let profile = if path.ends_with(".tsx") || path.ends_with(".jsx") {
            TypeScriptSource::Tsx
        } else {
            TypeScriptSource::TypeScript
        };
        let mut checker = TszCheckerOptions::default();
        checker.no_lib = true;
        let options = TszProjectOptions {
            checker,
            semantic_options: TszProjectSemanticOptions::declaration_scoped(),
            program_diagnostics: Vec::new(),
            module_resolutions: Vec::new(),
            file_formats: Vec::new(),
            environment: TszEnvironmentFingerprint::from_sha256([0x5a; 32]),
        };
        let mut authority = TszProjectAuthority::new();
        authority
            .update(
                vec![TszFileInput {
                    path: path.to_owned(),
                    source: source.to_owned(),
                }],
                options,
                &[],
            )
            .map_err(|_| LaneError::Missing("native TSZ project admission"))?;
        let project = authority
            .project()
            .ok_or(LaneError::Missing("native TSZ project result"))?;
        let mut facts = FactSet::new();
        super::collect_with_tsz_unmetered_for_test(
            profile,
            source.as_bytes(),
            project,
            path,
            &mut facts,
        )
        .map_err(LaneError::from)?;
        let identity = backend_semantic::ir::SourceIdentity {
            identity: ContentId::<SourceFactDomain>::from_canonical_bytes(source.as_bytes()),
            byte_len: u32::try_from(source.len())?,
        };
        let recipe = CompileRecipeFact::derive(
            LanguageProfile::TypeScript(profile),
            Stage::LowerIr,
            NativeTool::TypeScriptCompiler,
            ContentId::<SourceFactDomain>::from_canonical_bytes(source.as_bytes()),
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"tsz-native-lane-fixture"),
        );
        facts
            .build_ir(
                LanguageProfile::TypeScript(profile),
                identity,
                recipe,
                crate::driver::types::DeclarationScope::fixture(),
            )
            .map_err(LaneError::from)
    }

    fn native_tsz_project_with_resolutions(
        files: &[(&str, &str)],
        module_resolutions: &[TszProjectModuleResolution],
    ) -> Result<TszProjectAuthority, LaneError> {
        let mut checker = TszCheckerOptions::default();
        checker.no_lib = true;
        let options = TszProjectOptions {
            checker,
            semantic_options: TszProjectSemanticOptions::declaration_scoped(),
            program_diagnostics: Vec::new(),
            module_resolutions: module_resolutions.to_vec(),
            file_formats: Vec::new(),
            environment: TszEnvironmentFingerprint::from_sha256([0x71; 32]),
        };
        let mut authority = TszProjectAuthority::new();
        authority
            .update(
                files
                    .iter()
                    .map(|(path, source)| TszFileInput {
                        path: (*path).to_owned(),
                        source: (*source).to_owned(),
                    })
                    .collect(),
                options,
                &[],
            )
            .map_err(|_| LaneError::Missing("multi-file native TSZ project admission"))?;
        Ok(authority)
    }

    fn module_resolution(
        importer_path: &str,
        specifier: &str,
        request_kind: TszProjectModuleRequestKind,
        target: TszProjectModuleResolutionTarget,
    ) -> TszProjectModuleResolution {
        TszProjectModuleResolution {
            importer_path: importer_path.to_owned(),
            specifier: specifier.to_owned(),
            request_kind,
            resolution_mode: None,
            target,
        }
    }

    fn assert_exact_module_resolution_metadata(
        project: &TszProject,
        expected: &[TszProjectModuleResolution],
    ) {
        let program = project.program();
        let outcomes = program
            .project_module_resolution_outcomes
            .as_ref()
            .expect("exact module-resolution outcomes are attached to the merged program");
        let mut actual = outcomes
            .iter()
            .map(|((importer, specifier, mode, kind), outcome)| {
                (
                    program.files[*importer].file_name.clone(),
                    specifier.clone(),
                    format!("{kind:?}"),
                    format!("{mode:?}"),
                    format!("{outcome:?}"),
                )
            })
            .collect::<Vec<_>>();
        let mut expected = expected
            .iter()
            .map(|resolution| {
                let target = match &resolution.target {
                    TszProjectModuleResolutionTarget::File { path } => {
                        let index = program
                            .files
                            .iter()
                            .position(|file| &file.file_name == path)
                            .expect("file target belongs to the admitted program");
                        format!("File({index})")
                    }
                    TszProjectModuleResolutionTarget::External { identity } => {
                        format!("External {{ identity: {identity:?} }}")
                    }
                    TszProjectModuleResolutionTarget::Unresolved => "Unresolved".to_owned(),
                };
                (
                    resolution.importer_path.clone(),
                    resolution.specifier.clone(),
                    format!("{:?}", resolution.request_kind),
                    format!("{:?}", resolution.resolution_mode),
                    target,
                )
            })
            .collect::<Vec<_>>();
        actual.sort();
        expected.sort();
        assert_eq!(
            actual, expected,
            "every source import has one exact project outcome, including external/unresolved requests"
        );
    }

    fn collect_native_tsz_file<'source>(
        project: &'source TszProject,
        path: &str,
        source: &'source str,
    ) -> Result<FactSet<'source>, LaneError> {
        let mut facts = FactSet::new();
        super::collect_with_tsz_unmetered_for_test(
            TypeScriptSource::TypeScript,
            source.as_bytes(),
            project,
            path,
            &mut facts,
        )
        .map_err(LaneError::from)?;
        Ok(facts)
    }

    #[test]
    fn native_tsz_destructured_variables_publish_only_exact_leaf_bindings() -> Result<(), LaneError> {
        use backend_semantic::ir::DeclarationPublicationRole as Role;
        let source = "declare function fallback(): number;\ndeclare const input: { source?: number; nested: { count: number }; extra: boolean };\nexport const { source: renamed = fallback(), nested: { count }, ...remaining } = input;\nexport let [head, , ...tail]: number[] = [1, 2, 3];\nexport function read() { return renamed + count + head; }\n";
        let authority = native_tsz_project_with_resolutions(&[("bindings.ts", source)], &[])?;
        let project = authority.project().ok_or(LaneError::Missing("destructured variable project"))?;
        let facts = collect_native_tsz_file(project, "bindings.ts", source)?;
        let mut leaves = std::collections::BTreeMap::new();
        for (name, kind) in [
            ("renamed", EntityKind::Constant), ("count", EntityKind::Constant),
            ("remaining", EntityKind::Constant), ("head", EntityKind::Static),
            ("tail", EntityKind::Static),
        ] {
            let matches = (0..facts.len()).filter(|&index|
                facts.names[index] == name.as_bytes() && facts.kinds[index] == kind
                    && facts.publication_roles[index] == Role::PrimaryOwned).collect::<Vec<_>>();
            assert_eq!(matches.len(), 1, "one authored binding with its lexical const/let kind: {name}");
            let leaf = matches[0];
            let span = facts.provenance.source_spans()[leaf].expect("exact authored leaf span");
            assert_eq!(&source.as_bytes()[span.start as usize..span.end as usize], name.as_bytes());
            leaves.insert(name, leaf as u32);
        }
        assert!(!(0..facts.len()).any(|index|
            facts.publication_roles[index] == Role::PrimaryOwned
                && (facts.names[index].starts_with(b"{") || facts.names[index].starts_with(b"["))),
            "object/array patterns never become named product rows");
        for name in ["source", "nested", "fallback"] {
            assert!(!(0..facts.len()).any(|index|
                facts.names[index] == name.as_bytes()
                    && matches!(facts.kinds[index], EntityKind::Constant | EntityKind::Static)
                    && facts.publication_roles[index] == Role::PrimaryOwned),
                "property labels and default-expression references are not leaf bindings: {name}");
        }
        let body = source.find("return renamed + count + head").expect("body");
        for name in ["renamed", "count", "head"] {
            let start = (body + source[body..].find(name).expect("exact body use")) as u32;
            assert!((0..facts.occurrence_len).any(|index|
                facts.provenance.source_spans()[facts.occurrence_owners[index] as usize]
                    .is_some_and(|owner| owner.start + facts.occurrences[index].span.start == start)
                    && facts.occurrences[index].target == OccurrenceTarget::Local(EntityId::new(leaves[name]))),
                "body references retain exact local leaf identity: {name}");
        }
        let initializer = source.find("renamed = fallback()").expect("default initializer") as u32
            + "renamed = ".len() as u32;
        let fallback = (0..facts.len()).find(|&index|
            facts.names[index] == b"fallback" && facts.kinds[index] == EntityKind::Function)
            .expect("real fallback function") as u32;
        assert!((0..facts.occurrence_len).any(|index| {
            let owner = facts.occurrence_owners[index] as usize;
            facts.publication_roles[owner] == Role::ReferenceOnly
                && facts.provenance.source_spans()[owner].is_some_and(|span|
                    span.start + facts.occurrences[index].span.start == initializer)
                && facts.occurrences[index].target == OccurrenceTarget::Local(EntityId::new(fallback))
        }), "the aggregate keeps its canonical initializer reference without a fabricated leaf owner");
        let ir = owned_tsz_ir_at_path(source, "bindings.ts")?;
        for name in leaves.keys() {
            let owned = ir.canonical_entities().find(|entity|
                entity.authority.publication == Role::PrimaryOwned
                    && entity.name.named_atom().and_then(|atom| ir.atom(atom)) == Some(name.as_bytes()))
                .expect("retained primary leaf");
            assert!(ir.typescript_extension(owned.id).and_then(|extension| extension.observed).is_some(),
                "native checker observes the actual leaf, not the aggregate annotation: {name}");
        }
        Ok(())
    }

    #[test]
    fn native_tsz_destructured_parameters_keep_one_carrier_and_exact_authored_leaves() -> Result<(), LaneError> {
        use backend_semantic::ir::{DeclarationPublicationRole as Role, SignatureCarrierRole, SignatureCarrierRoleObservation};
        let source = "declare function fallback(): string;\ninterface NavBarProps { title?: string; onClick: () => void; nested: { count: number }; extra: boolean; }\nexport function NavBar({\n  title = fallback(),\n  onClick: invoke,\n  nested: { count },\n  ...remaining\n}: NavBarProps) { invoke(); return title + count; }\nexport function array([first, , ...tail]: string[]) { return first; }\n";
        let authority = native_tsz_project_with_resolutions(&[("NavBar.ts", source)], &[])?;
        let project = authority.project().ok_or(LaneError::Missing("destructured parameter project"))?;
        let facts = collect_native_tsz_file(project, "NavBar.ts", source)?;
        for name in ["NavBar", "array"] {
            let function = (0..facts.len()).find(|&index| facts.names[index] == name.as_bytes()
                && facts.kinds[index] == EntityKind::Function).expect("function declaration");
            let start = facts.child_starts[function] as usize;
            let count = facts.child_counts[function] as usize;
            let inputs = (start..start + count).filter(|&index|
                facts.child_roles[index] == ProductChildRole::FunctionParameter).collect::<Vec<_>>();
            assert_eq!(inputs.len(), 1, "destructuring remains one argument position: {name}");
            let carrier = facts.child_targets[inputs[0]] as usize;
            assert_eq!(facts.publication_roles[carrier], Role::ReferenceOnly);
            let span = facts.provenance.source_spans()[carrier].expect("complete aggregate source span");
            assert_eq!(&source.as_bytes()[span.start as usize..span.end as usize], facts.names[carrier]);
            assert!(facts.names[carrier].starts_with(if name == "NavBar" { b"{" } else { b"[" }));
        }
        let ir = owned_tsz_ir_at_path(source, "NavBar.ts")?;
        for (name, declaration, reference) in [
            ("title", "title = fallback()", "return title"),
            ("invoke", "invoke,", "{ invoke()"),
            ("count", "count },", "+ count"),
            ("remaining", "remaining\n", "...remaining"),
            ("first", "first,", "return first"),
            ("tail", "tail]", "...tail"),
        ] {
            let start = source.find(declaration).expect("written binding") as u32;
            let leaf = (0..facts.len()).find(|&index|
                facts.names[index] == name.as_bytes()
                    && facts.kinds[index] == EntityKind::Parameter
                    && facts.publication_roles[index] == Role::PrimaryOwned
                    && facts.provenance.source_spans()[index].is_some_and(|span|
                        (span.start, span.end) == (start, start + name.len() as u32)))
                .expect("exact source-owned destructured leaf");
            let entity = ir.canonical_entities().find(|entity|
                entity.kind == EntityKind::Parameter
                    && entity.name.named_atom().and_then(|atom| ir.atom(atom)) == Some(name.as_bytes())
                    && entity.authority.publication == Role::PrimaryOwned)
                .expect("retained authored parameter");
            assert!(ir.typescript_extension(entity.id).and_then(|extension| extension.observed).is_some(),
                "leaf type comes from the exact native binding: {name}");
            if !reference.starts_with("...") {
                let at = source.find(reference).expect("body reference") + reference.len() - name.len();
                // invoke's prefix includes '(' rather than a trailing name.
                let at = if name == "invoke" { source.find("{ invoke()").unwrap() + 2 } else { at } as u32;
                assert!((0..facts.occurrence_len).any(|index|
                    facts.provenance.source_spans()[facts.occurrence_owners[index] as usize]
                        .is_some_and(|span| span.start + facts.occurrences[index].span.start == at)
                        && facts.occurrences[index].target == OccurrenceTarget::Local(EntityId::new(leaf as u32))),
                    "body reference targets its exact authored leaf: {name}");
            }
        }
        assert!(!(0..facts.len()).any(|index|
            facts.kinds[index] == EntityKind::Parameter && facts.publication_roles[index] == Role::PrimaryOwned
                && [b"onClick".as_slice(), b"nested", b"fallback"].contains(&facts.names[index])),
            "property labels and default-expression references are not parameter bindings");
        for entity in ir.canonical_entities().filter(|entity|
            entity.kind == EntityKind::Parameter && entity.authority.publication == Role::ReferenceOnly)
        {
            if entity.name.named_atom().and_then(|atom| ir.atom(atom)).is_some_and(|name| name.starts_with(b"{") || name.starts_with(b"[")) {
                assert_eq!(ir.signature_carrier_role(entity.id),
                    Some(SignatureCarrierRoleObservation::Captured(SignatureCarrierRole::Input)));
            }
        }
        Ok(())
    }

    #[test]
    fn native_tsz_class_members_match_syntax_extents_without_coalescing_roles() -> Result<(), LaneError>
    {
        use backend_compile::SourceBindingIdentity;
        // The two decorated forms are the actual Angular declarations that
        // produced duplicate member rows; neighboring controls retain ASI,
        // same-name declarations, and constructor parameter-property roles.
        let source = "declare function Input(): any;\nclass ArticleListComponent {\n  @Input() currentPage = 1;\n  automatic = 2\n  constructor(private http: unknown) {}\n  same() {}\n}\nclass IfAuthenticatedDirective {\n  @Input() set ifAuthenticated(condition: boolean) {}\n  same() {}\n}\n";
        let authority = native_tsz_project_with_resolutions(&[("input.ts", source)], &[])?;
        let project = authority
            .project()
            .ok_or(LaneError::Missing("native TSZ project"))?;
        let facts = collect_native_tsz_file(project, "input.ts", source)?;
        let syntax = backend_frontend_typescript::syntax_frontend()
            .map_err(|_| LaneError::Missing("TypeScript syntax frontend"))?
            .analyze(std::path::Path::new("input.ts"), source.as_bytes())
            .map_err(|_| LaneError::Missing("TypeScript syntax analysis"))?;
        for name in ["currentPage", "automatic", "ifAuthenticated", "same"] {
            let written = syntax
                .declarations()
                .iter()
                .filter(|row| row.name() == name)
                .collect::<Vec<_>>();
            let expected = if name == "same" { 2 } else { 1 };
            assert_eq!(written.len(), expected, "written {name}");
            assert_eq!(
                (0..facts.len())
                    .filter(|&index| facts.names[index] == name.as_bytes())
                    .count(),
                expected,
                "native {name}"
            );
            for declaration in written {
                let SourceBindingIdentity::Exact(capture) = declaration.binding_identity() else {
                    panic!("exact native join capture");
                };
                assert_eq!(
                    (0..facts.len())
                        .filter(|&index| {
                            facts.names[index] == name.as_bytes()
                                && facts.provenance.source_spans()[index]
                                    .is_some_and(|span| (span.start, span.end) == capture.owner())
                        })
                        .count(),
                    1,
                    "one exact native twin for {name}: {:?}",
                    capture.owner()
                );
            }
        }
        let roles = (0..facts.len())
            .filter(|&index| facts.names[index] == b"http")
            .map(|index| facts.kinds[index])
            .collect::<Vec<_>>();
        assert_eq!(
            roles.len(),
            2,
            "parameter properties retain both declarations"
        );
        assert!(roles.contains(&EntityKind::Parameter));
        assert!(roles.contains(&EntityKind::Field));
        Ok(())
    }

    /// The exact 1,858-byte production ArticlesService that failed semantic
    /// shape projection: inventory must be captured by lowering, not waived by
    /// the shape reader. The fixture is byte-identical to the real app courier.
    #[test]
    fn native_tsz_actual_articles_parameter_property_inventory_is_complete() -> Result<(), LaneError>
    {
        use backend_semantic::ir::{FactAvailability, ParentageAuthority};
        let source = include_str!(
            "../../application/typescript_program/direct/tests/fixtures/angular-current42/src/app/features/article/services/articles.service.ts"
        );
        let ir = owned_tsz_ir_at_path(
            source,
            "src/app/features/article/services/articles.service.ts",
        )?;
        let owner = ir
            .items()
            .find(|item| item.name() == b"ArticlesService")
            .ok_or(LaneError::Missing("real ArticlesService owner"))?;
        let owner_entity = ir.entity(owner.id()).expect("class entity");
        assert_eq!(owner_entity.authority.members, FactAvailability::Captured);
        let members = owner
            .members()
            .iter()
            .map(|id| ir.entity(*id).expect("captured exact member"))
            .collect::<Vec<_>>();
        let mut names = members
            .iter()
            .map(|entity| {
                entity
                    .name
                    .named_atom()
                    .and_then(|name| ir.atom(name))
                    .expect("named direct member")
                    .to_vec()
            })
            .collect::<Vec<_>>();
        names.sort();
        let mut expected = [
            "constructor",
            "query",
            "get",
            "delete",
            "create",
            "update",
            "favorite",
            "unfavorite",
            "http",
        ]
        .map(|name| name.as_bytes().to_vec());
        expected.sort();
        assert_eq!(
            names, expected,
            "all nine source-authored members, no inherited/effective rows"
        );
        let field = members
            .iter()
            .find(|entity| {
                entity.kind == EntityKind::Field
                    && entity.name.named_atom().and_then(|name| ir.atom(name)) == Some(b"http")
            })
            .expect("exact constructor parameter-property field");
        let constructor = members
            .iter()
            .find(|entity| {
                entity.name.named_atom().and_then(|name| ir.atom(name)) == Some(b"constructor")
            })
            .expect("actual constructor");
        let parameters = ir
            .canonical_entities()
            .filter(|entity| {
                entity.kind == EntityKind::Parameter
                    && entity.name.named_atom().and_then(|name| ir.atom(name)) == Some(b"http")
            })
            .collect::<Vec<_>>();
        assert_eq!(
            parameters.len(),
            1,
            "signature parameter is retained independently"
        );
        let parameter = parameters[0];
        assert_ne!(field.id, parameter.id);
        assert_eq!(
            field.authority.parentage,
            ParentageAuthority::Bound(owner_entity.version.identity())
        );
        assert_eq!(
            parameter.authority.parentage,
            ParentageAuthority::Bound(constructor.version.identity())
        );
        assert!(
            !members.iter().any(|entity| entity.id == parameter.id),
            "the signature Parameter role is not the class Field role"
        );
        Ok(())
    }

    #[test]
    fn native_tsz_parameter_property_inventory_excludes_foreign_and_nonfield_roles()
    -> Result<(), LaneError> {
        use backend_semantic::ir::{FactAvailability, ParentageAuthority};
        let source = concat!(
            "class Owner { constructor(private readonly http: string, ordinary: number) {} ",
            "run() { class Nested { constructor(public http: number) {} } return Nested; } }\n",
            "class Neighbor { constructor(protected http: boolean) {} }\n",
            "class Partial { constructor(public field: number) {} static { const runtime = 1; } }\n",
            "interface Merged { extra: number } class Merged { constructor(public field: number) {} }\n",
        );
        let ir = owned_tsz_ir(source)?;
        for (name, expected) in [
            (
                &b"Owner"[..],
                &[&b"constructor"[..], &b"http"[..], &b"run"[..]][..],
            ),
            (&b"Nested"[..], &[&b"constructor"[..], &b"http"[..]][..]),
            (&b"Neighbor"[..], &[&b"constructor"[..], &b"http"[..]][..]),
        ] {
            let item = ir
                .items()
                .find(|item| item.name() == name)
                .ok_or(LaneError::Missing("parameter-property inventory owner"))?;
            let entity = ir.entity(item.id()).expect("owner entity");
            assert_eq!(entity.authority.members, FactAvailability::Captured);
            let mut names = Vec::new();
            for id in item.members().iter() {
                let member = ir.entity(*id).expect("direct member");
                assert_eq!(
                    member.authority.parentage,
                    ParentageAuthority::Bound(entity.version.identity()),
                    "nested/neighbor same-spelling fields cannot become this class's members"
                );
                assert_ne!(member.kind, EntityKind::Parameter);
                names.push(
                    member
                        .name
                        .named_atom()
                        .and_then(|name| ir.atom(name))
                        .expect("named direct member")
                        .to_vec(),
                );
            }
            names.sort();
            let mut expected = expected
                .iter()
                .map(|name| name.to_vec())
                .collect::<Vec<_>>();
            expected.sort();
            assert_eq!(
                names, expected,
                "ordinary parameters and nested declarations are excluded"
            );
        }
        for name in [b"Partial".as_slice(), b"Merged".as_slice()] {
            let item = ir
                .items()
                .find(|item| item.name() == name)
                .ok_or(LaneError::Missing("unproved member inventory"))?;
            assert_eq!(
                ir.entity(item.id())
                    .expect("owner entity")
                    .authority
                    .members,
                FactAvailability::Unavailable,
                "parameter-property support cannot waive unsupported body or merge inventory for {name:?}"
            );
            assert!(item.members().is_empty());
        }
        Ok(())
    }

    fn staged_tsz_coordinates(
        facts: &FactSet<'_>,
    ) -> Result<Vec<(ReferenceKind, String, u32, u32, u32, u32)>, LaneError> {
        let mut coordinates = Vec::new();
        for index in 0..facts.occurrence_len {
            let Some(slot) = facts.occurrence_source_coordinate_slot[index] else {
                continue;
            };
            if !matches!(
                facts.occurrences[index].target,
                OccurrenceTarget::Foreign(key)
                    if matches!(
                        key.origin,
                        backend_semantic::ir::ForeignOrigin::Universe { ecosystem }
                            if ecosystem == TYPESCRIPT_TSZ_SOURCE_ECOSYSTEM
                    )
            ) {
                return Err(LaneError::Missing("typed TSZ foreign target key"));
            }
            let encoded = facts
                .foreign_source_coordinate_text
                .get(slot as usize)
                .ok_or(LaneError::Missing("staged TSZ target coordinate"))?;
            let coordinate = TypeScriptSourceCoordinate::decode(encoded)
                .ok_or(LaneError::Missing("decodable TSZ target coordinate"))?;
            coordinates.push((
                facts.occurrences[index].kind,
                coordinate.path.to_owned(),
                coordinate.declaration_start,
                coordinate.declaration_end,
                coordinate.name_start,
                facts.occurrence_owners[index],
            ));
        }
        Ok(coordinates)
    }

    type AnonymousCallableRow = (
        EntityId,
        DeclarationFamilyId,
        VariantFingerprint,
        u32,
        u32,
        AnonymousCallableFamilyMultiplicity,
    );

    fn anonymous_callable_rows(ir: &backend_semantic::ir::Ir) -> Vec<AnonymousCallableRow> {
        ir.canonical_entities()
            .filter_map(|entity| {
                let ItemName::AnonymousCallable(anchor) = entity.name else {
                    return None;
                };
                let encoded = ir.atom(anchor)?;
                let anchor = AnonymousCallableAnchorView::try_from_encoded(encoded)?;
                let multiplicity = anchor.family_multiplicity()?;
                let span = entity.source?;
                Some((
                    entity.id,
                    entity.version.family,
                    entity.version.variant,
                    span.start(),
                    span.end(),
                    multiplicity,
                ))
            })
            .collect()
    }

    fn smallest_callable_containing<'rows>(
        rows: &'rows [AnonymousCallableRow],
        source: &str,
        needle: &str,
    ) -> Result<&'rows AnonymousCallableRow, LaneError> {
        let start = u32::try_from(
            source
                .find(needle)
                .ok_or(LaneError::Missing("anonymous callable source needle"))?,
        )?;
        let end = start
            .checked_add(u32::try_from(needle.len())?)
            .ok_or(LaneError::Scalar)?;
        rows.iter()
            .filter(|row| row.3 <= start && end <= row.4)
            .min_by_key(|row| row.4.saturating_sub(row.3))
            .ok_or(LaneError::Missing(
                "anonymous callable enclosing source span",
            ))
    }

    #[test]
    fn native_tsz_joins_real_nest_controller_and_spec_calls_by_exact_owner() -> Result<(), LaneError>
    {
        let controller =
            include_str!("../../../tests/fixtures/typescript_nest_reference/src/app.controller.ts");
        let service =
            include_str!("../../../tests/fixtures/typescript_nest_reference/src/app.service.ts");
        let spec = include_str!(
            "../../../tests/fixtures/typescript_nest_reference/src/app.controller.spec.ts"
        );
        let module_resolutions = vec![
            module_resolution(
                "src/app.controller.ts",
                "@nestjs/common",
                TszProjectModuleRequestKind::EsmImport,
                TszProjectModuleResolutionTarget::Unresolved,
            ),
            module_resolution(
                "src/app.controller.ts",
                "./app.service.js",
                TszProjectModuleRequestKind::EsmImport,
                TszProjectModuleResolutionTarget::File {
                    path: "src/app.service.ts".to_owned(),
                },
            ),
            module_resolution(
                "src/app.service.ts",
                "@nestjs/common",
                TszProjectModuleRequestKind::EsmImport,
                TszProjectModuleResolutionTarget::Unresolved,
            ),
            module_resolution(
                "src/app.controller.spec.ts",
                "@nestjs/testing",
                TszProjectModuleRequestKind::EsmImport,
                TszProjectModuleResolutionTarget::Unresolved,
            ),
            module_resolution(
                "src/app.controller.spec.ts",
                "./app.controller.js",
                TszProjectModuleRequestKind::EsmImport,
                TszProjectModuleResolutionTarget::File {
                    path: "src/app.controller.ts".to_owned(),
                },
            ),
            module_resolution(
                "src/app.controller.spec.ts",
                "./app.service.js",
                TszProjectModuleRequestKind::EsmImport,
                TszProjectModuleResolutionTarget::File {
                    path: "src/app.service.ts".to_owned(),
                },
            ),
        ];
        let authority = native_tsz_project_with_resolutions(
            &[
                ("src/app.controller.ts", controller),
                ("src/app.service.ts", service),
                ("src/app.controller.spec.ts", &spec),
            ],
            &module_resolutions,
        )?;
        let project = authority
            .project()
            .ok_or(LaneError::Missing("Nest TSZ project"))?;
        assert_exact_module_resolution_metadata(project, &module_resolutions);

        let controller_facts =
            collect_native_tsz_file(project, "src/app.controller.ts", controller)?;
        let controller_targets = staged_tsz_coordinates(&controller_facts)?;
        let service_start = u32::try_from(
            service
                .find("getHello(): string")
                .ok_or(LaneError::Missing("Nest service method source coordinate"))?,
        )?;
        let service_name_start = service_start;
        let service_body_end = service
            .get(service_start as usize..)
            .and_then(|body| body.find("\n  }").map(|end| end + 4))
            .ok_or(LaneError::Missing("Nest service method end coordinate"))?;
        let service_end = service_start + u32::try_from(service_body_end)?;
        assert!(
            controller_targets
                .iter()
                .any(|(kind, path, start, end, name, _)| {
                    *kind == ReferenceKind::MethodCall
                        && path == "src/app.service.ts"
                        && *start == service_start
                        && *end == service_end
                        && *name == service_name_start
                }),
            "controller call must name the exact service declaration: {controller_targets:?}"
        );

        let spec_facts = collect_native_tsz_file(project, "src/app.controller.spec.ts", &spec)?;
        let spec_targets = staged_tsz_coordinates(&spec_facts)?;
        let controller_name_start = u32::try_from(controller.find("getHello(): string").ok_or(
            LaneError::Missing("Nest controller method source coordinate"),
        )?)?;
        let controller_declaration_start = u32::try_from(controller.find("@Get()").ok_or(
            LaneError::Missing("Nest controller method decorator coordinate"),
        )?)?;
        let controller_declaration_end_offset = controller
            .get(controller_name_start as usize..)
            .and_then(|body| body.find("\n  }").map(|end| end + 4))
            .ok_or(LaneError::Missing("Nest controller method end coordinate"))?;
        let controller_declaration_end = controller_name_start
            .checked_add(u32::try_from(controller_declaration_end_offset)?)
            .ok_or(LaneError::Missing("Nest controller method end coordinate"))?;
        assert!(
            spec_targets
                .iter()
                .any(|(kind, path, start, end, name, _)| {
                    *kind == ReferenceKind::MethodCall
                        && path == "src/app.controller.ts"
                        && *start == controller_declaration_start
                        && *end == controller_declaration_end
                        && *name == controller_name_start
                }),
            "spec call must name the exact controller declaration: {spec_targets:?}"
        );
        Ok(())
    }

    #[test]
    fn native_tsz_joins_injected_service_field_calls_to_the_exact_foreign_owner()
    -> Result<(), LaneError> {
        let service = "export class ExerciseAnalyticsService { totalsEntries() { return []; } }";
        let records = concat!(
            "import { ExerciseAnalyticsService } from './exercise-analytics.service.js';\n",
            "declare function inject<T>(token: new (...args: any[]) => T): T;\n",
            "class Decoy { totalsEntries() { return ['wrong']; } }\n",
            "export class RecordsPage {\n",
            "  private readonly svc = inject(ExerciseAnalyticsService);\n",
            "  load() { return this.svc.totalsEntries(); }\n",
            "}\n",
        );
        let training_totals = concat!(
            "import { ExerciseAnalyticsService } from './exercise-analytics.service.js';\n",
            "declare function inject<T>(token: new (...args: any[]) => T): T;\n",
            "class Decoy { totalsEntries() { return ['wrong']; } }\n",
            "export class TrainingTotalsPage {\n",
            "  private readonly svc = inject(ExerciseAnalyticsService);\n",
            "  load() { return this.svc.totalsEntries(); }\n",
            "}\n",
        );
        let resolutions = [
            module_resolution(
                "src/pages/records/records.ts",
                "./exercise-analytics.service.js",
                TszProjectModuleRequestKind::EsmImport,
                TszProjectModuleResolutionTarget::File {
                    path: "src/analytics/exercise-analytics.service.ts".to_owned(),
                },
            ),
            module_resolution(
                "src/pages/analytics/totals/training-totals.ts",
                "./exercise-analytics.service.js",
                TszProjectModuleRequestKind::EsmImport,
                TszProjectModuleResolutionTarget::File {
                    path: "src/analytics/exercise-analytics.service.ts".to_owned(),
                },
            ),
        ];
        let authority = native_tsz_project_with_resolutions(
            &[
                ("src/analytics/exercise-analytics.service.ts", service),
                ("src/pages/records/records.ts", records),
                (
                    "src/pages/analytics/totals/training-totals.ts",
                    training_totals,
                ),
            ],
            &resolutions,
        )?;
        let project = authority
            .project()
            .ok_or(LaneError::Missing("injected-service TSZ project"))?;
        let method_start = u32::try_from(
            service
                .find("totalsEntries()")
                .ok_or(LaneError::Missing("injected-service method source"))?,
        )?;
        let method_end = method_start
            .checked_add(u32::try_from("totalsEntries() { return []; }".len())?)
            .ok_or(LaneError::Scalar)?;

        let mut exact_target_count = 0;
        for (path, source) in [
            ("src/pages/records/records.ts", records),
            (
                "src/pages/analytics/totals/training-totals.ts",
                training_totals,
            ),
        ] {
            let facts = collect_native_tsz_file(project, path, source)?;
            let targets = staged_tsz_coordinates(&facts)?;
            let load_facts = (0..facts.len())
                .filter(|&index| {
                    facts.names[index] == b"load" && facts.kinds[index] == EntityKind::Function
                })
                .collect::<Vec<_>>();
            assert_eq!(load_facts.len(), 1, "one enclosing load method in {path}");
            let load = load_facts[0];
            let load_span = facts.provenance.source_spans()[load]
                .ok_or(LaneError::Missing("load method source span"))?;
            let call_name_start = u32::try_from(
                source
                    .find("this.svc.totalsEntries()")
                    .ok_or(LaneError::Missing("injected-service call source"))?
                    + "this.svc.".len(),
            )?;
            let call_name_end = call_name_start
                .checked_add(u32::try_from("totalsEntries".len())?)
                .ok_or(LaneError::Scalar)?;
            let call_uses = (0..facts.occurrence_len)
                .filter(|&index| {
                    let occurrence = facts.occurrences[index];
                    let absolute_start = load_span.start + occurrence.span.start;
                    let absolute_end = load_span.start + occurrence.span.end;
                    absolute_start == call_name_start && absolute_end == call_name_end
                })
                .collect::<Vec<_>>();
            assert_eq!(call_uses.len(), 1, "one exact call-site reference in {path}");
            let call_use = call_uses[0];
            assert_eq!(facts.occurrence_owners[call_use], load as u32);
            assert_eq!(facts.occurrences[call_use].kind, ReferenceKind::MethodCall);
            assert_eq!(
                source.as_bytes().get(call_name_start as usize..call_name_end as usize),
                Some(b"totalsEntries".as_slice()),
                "the reference span names exactly the called member in {path}"
            );
            let call_targets = targets
                .iter()
                .filter(|(kind, _, _, _, _, owner)| {
                    *kind == ReferenceKind::MethodCall && *owner == load as u32
                })
                .collect::<Vec<_>>();
            assert_eq!(call_targets.len(), 1, "one target for the call in {path}");
            exact_target_count += call_targets.len();
            assert!(
                matches!(
                    call_targets[0],
                    (ReferenceKind::MethodCall, target_path, start, end, name, owner)
                        if target_path == "src/analytics/exercise-analytics.service.ts"
                            && *start == method_start
                            && *end == method_end
                            && *name == method_start
                            && *owner == load as u32
                ),
                "injected service call must target the exact service method, not the same-named decoy, from {path}: {call_targets:?}"
            );
        }
        assert_eq!(exact_target_count, 2, "both reported caller files have one exact target");
        Ok(())
    }

    #[test]
    fn native_tsz_preserves_official_rxjs_bind_callback_overload_identities()
    -> Result<(), LaneError> {
        let source = include_str!(
            "../../../tests/fixtures/typescript/rxjs-7.8.1/src/internal/observable/bindCallback.ts"
        );
        assert_eq!(source.len(), 6_800, "pinned upstream source byte length");
        let digest = source_digest(source.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_eq!(
            digest, "67e2634b866722de677ba42e11b4d878d8642e76b580ff44d5328ed9bd34064f",
            "pinned upstream source digest"
        );

        let mut checker = TszCheckerOptions::default();
        checker.no_lib = true;
        let options = TszProjectOptions {
            checker,
            semantic_options: TszProjectSemanticOptions::declaration_scoped(),
            program_diagnostics: Vec::new(),
            module_resolutions: Vec::new(),
            file_formats: Vec::new(),
            environment: TszEnvironmentFingerprint::from_sha256([0x6b; 32]),
        };
        let mut authority = TszProjectAuthority::new();
        authority
            .update(
                vec![TszFileInput {
                    path: "input.ts".to_owned(),
                    source: source.to_owned(),
                }],
                options,
                &[],
            )
            .map_err(|_| LaneError::Missing("RxJS TSZ project admission"))?;
        let project = authority
            .project()
            .ok_or(LaneError::Missing("RxJS TSZ project result"))?;
        let mut facts = FactSet::new();
        super::collect_with_tsz_unmetered_for_test(
            TypeScriptSource::TypeScript,
            source.as_bytes(),
            project,
            "input.ts",
            &mut facts,
        )?;
        let profile = LanguageProfile::TypeScript(TypeScriptSource::TypeScript);
        let scope = crate::driver::types::DeclarationScope::fixture();
        let versions = super::super::identity::versions(&facts, scope, profile)?;
        let mut duplicate_details = Vec::new();
        for left in 0..versions.len() {
            for right in left + 1..versions.len() {
                if versions[left].family != versions[right].family
                    || versions[left].variant != versions[right].variant
                {
                    continue;
                }
                let left_name = String::from_utf8_lossy(&facts.names[left]);
                let right_name = String::from_utf8_lossy(&facts.names[right]);
                duplicate_details.push(format!(
                    "{left} {:?} {left_name:?} {:?} {:?} == {right} {:?} {right_name:?} {:?} {:?}",
                    facts.kinds[left],
                    facts.provenance.parentage()[left],
                    facts.provenance.source_spans()[left],
                    facts.kinds[right],
                    facts.provenance.parentage()[right],
                    facts.provenance.source_spans()[right],
                ));
            }
        }
        assert!(
            duplicate_details.is_empty(),
            "distinct source declarations collide in identity: {duplicate_details:?}"
        );
        let identity = backend_semantic::ir::SourceIdentity {
            identity: ContentId::<SourceFactDomain>::from_canonical_bytes(source.as_bytes()),
            byte_len: u32::try_from(source.len())?,
        };
        let recipe = CompileRecipeFact::derive(
            profile,
            Stage::LowerIr,
            NativeTool::TypeScriptCompiler,
            ContentId::<SourceFactDomain>::from_canonical_bytes(source.as_bytes()),
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"tsz-rxjs-overload-fixture"),
        );
        let ir = facts
            .build_ir(profile, identity, recipe, scope)
            .map_err(LaneError::Build)?;
        let overloads = ir
            .items()
            .filter(|item| item.kind() == EntityKind::Function && item.name() == b"bindCallback")
            .map(|item| item.version())
            .collect::<Vec<_>>();
        assert_eq!(overloads.len(), 3, "two signatures plus implementation");
        for left in 0..overloads.len() {
            for right in left + 1..overloads.len() {
                assert_eq!(overloads[left].family, overloads[right].family);
                assert_ne!(
                    overloads[left].variant, overloads[right].variant,
                    "distinct upstream overloads keep distinct declaration instances"
                );
            }
        }
        Ok(())
    }

    #[test]
    fn native_tsz_direct_overloads_keep_interface_ownership_with_conditional_aliases()
    -> Result<(), LaneError> {
        use backend_semantic::ir::FactAvailability;
        let source = concat!(
            "type Draft<T> = T extends object ? { [K in keyof T]: Draft<T[K]> } : T;\n",
            "type Recipe<T> = (draft: Draft<T>) => void;\n",
            "interface IProduce {\n",
            "  /** first overload */ <Curried>(recipe: Recipe<Curried>): Curried\n",
            "  /** second overload */ <State>(recipe: Recipe<State>): (base: State) => State\n",
            "  named<State>(recipe: Recipe<State>): State;\n",
            "}\n",
            "class Other { named(value: number): number { return value; } }\n",
        );
        let ir = owned_tsz_ir(source)?;
        let owner = ir
            .items()
            .find(|item| item.name() == b"IProduce")
            .ok_or(LaneError::Missing("direct overload interface"))?;
        assert_eq!(
            ir.entity(owner.id())
                .expect("emitted interface")
                .authority
                .members,
            FactAvailability::Captured
        );
        assert_eq!(
            owner.members().len(),
            3,
            "both overloads and the named member are direct"
        );
        for member in owner.members() {
            assert_eq!(
                ir.item(*member).expect("direct member").kind(),
                EntityKind::Function
            );
        }
        let other = ir
            .items()
            .find(|item| item.name() == b"Other")
            .ok_or(LaneError::Missing("independent named class"))?;
        assert_eq!(other.members().len(), 1);
        assert!(
            !owner.members().contains(&other.members()[0]),
            "equal named members in different owners must remain distinct"
        );
        Ok(())
    }

    #[test]
    fn native_tsz_owns_nested_alias_and_call_signature_types() -> Result<(), LaneError> {
        let source = "type Direct = (...args: any[]) => void;\n\
type Nested = (callback: (...args: any[]) => void) => void;\n\
type NestedTuple = (...args: [(...items: any[]) => void]) => void;\n\
type Container = {\n\
  (...args: any[]): void;\n\
  new (...args: any[]): Container;\n\
};\n";
        let ir = owned_tsz_ir(source)?;
        let rows = anonymous_callable_rows(&ir);
        assert_eq!(rows.len(), 7, "each source callable type has an exact row");

        let anchors = rows
            .iter()
            .map(|row| {
                ir.item(row.0)
                    .and_then(|item| item.name().anonymous_anchor())
                    .and_then(|anchor| anchor.steps())
                    .ok_or(LaneError::Missing("typed callable source anchor"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        assert!(anchors.iter().flatten().any(|step| {
            step.child_role == CallableChildRole::TypeAliasValue
                && step.parent == CallableParentShape::TypeAliasName(b"Direct")
        }));
        assert!(
            anchors.iter().flatten().any(|step| {
                step.child_role == CallableChildRole::TypeExpression
                    && matches!(
                        step.parent,
                        CallableParentShape::TypeContainer(CallableTypeContainerKind::Tuple)
                    )
            }),
            "nested type-expression anchors: {anchors:?}"
        );
        assert!(anchors.iter().flatten().any(|step| {
            step.child_role == CallableChildRole::SignatureParameterType
                && step.parent == CallableParentShape::SignatureParameter(b"callback")
        }));
        let call_signatures = anchors
            .iter()
            .flatten()
            .filter(|step| step.child_role == CallableChildRole::CallSignatureMember)
            .count();
        let construct_signatures = anchors
            .iter()
            .flatten()
            .filter(|step| step.child_role == CallableChildRole::ConstructSignatureMember)
            .count();
        assert_eq!(call_signatures, 1);
        assert_eq!(construct_signatures, 1);
        for (row, anchor) in rows.iter().zip(&anchors) {
            if anchor.iter().any(|step| {
                matches!(
                    step.child_role,
                    CallableChildRole::CallSignatureMember
                        | CallableChildRole::ConstructSignatureMember
                )
            }) {
                assert_eq!(
                    row.5,
                    AnonymousCallableFamilyMultiplicity::Unique,
                    "call and construct signatures have distinct typed roles"
                );
            }
        }
        Ok(())
    }

    #[test]
    fn native_tsz_compact_fragment_uses_typed_anonymous_anchors_and_keeps_empty_literals()
    -> Result<(), LaneError> {
        let source = "const empty = \"\";\nexport function currentImpl(copy: any) {\n  return [copy].map((key, childValue) => { return childValue; });\n}\n";
        let authority = native_tsz_project_with_resolutions(&[("input.ts", source)], &[])?;
        let project = authority
            .project()
            .ok_or(LaneError::Missing("native project"))?;
        let facts = collect_native_tsz_file(project, "input.ts", source)?;
        let identity = backend_semantic::ir::SourceIdentity {
            identity: ContentId::<SourceFactDomain>::from_canonical_bytes(source.as_bytes()),
            byte_len: u32::try_from(source.len())?,
        };
        let recipe = CompileRecipeFact::derive(
            LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
            Stage::LowerIr,
            NativeTool::TypeScriptCompiler,
            identity.identity,
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"native-anonymous-compact-fixture"),
        );
        let ir = facts.build_ir(
            recipe.profile,
            identity,
            recipe,
            crate::driver::types::DeclarationScope::fixture(),
        )?;
        assert!(ir.items().any(|item| item.name() == b"empty"));
        let anchors = ir
            .canonical_entities()
            .filter_map(|entity| {
                entity
                    .name
                    .anonymous_callable_anchor()
                    .and_then(|atom| ir.atom(atom))
            })
            .collect::<Vec<_>>();
        assert_eq!(anchors.len(), 1);
        assert!(AnonymousCallableAnchorView::try_from_encoded(anchors[0]).is_some());
        let mut output = vec![0xa5; 65_536];
        let bytes = admit(&facts, identity, recipe, recipe.profile, &mut output)?;
        let compact = FragmentView::validate(bytes)?;
        assert!(compact.atoms().all(|atom| !atom.bytes.is_empty()));
        assert!(compact.atoms().any(|atom| atom.bytes == anchors[0]));
        assert!(compact.atoms().any(|atom| atom.bytes == b"empty"));

        let mut invalid = FactSet::new();
        let rejected = invalid
            .push(crate::driver::lower::SemanticFact::new(
                EntityKind::Function,
                b"",
                crate::driver::lower::LEAF_PRODUCT,
            ))
            .expect_err("an absent required name is not an anonymous anchor");
        assert_eq!(rejected.cause, crate::driver::lower::FactFault::EmptyName);
        Ok(())
    }

    #[test]
    fn native_tsz_anonymous_callables_keep_instance_sites_and_ambiguous_families()
    -> Result<(), LaneError> {
        let source = "function helper(value: number) { return value; }\n\
const wrapper = (seed: number) => {\n\
  const captured = seed;\n\
  return [seed].map((entry: number) => helper(captured + entry));\n\
};\n\
describe('suite', () => {\n\
  it('one', () => helper(1));\n\
  it('two', () => helper(2));\n\
});\n";
        let ir = owned_tsz_ir(source)?;
        let rows = anonymous_callable_rows(&ir);
        assert_eq!(rows.len(), 5, "every supported arrow has an exact item row");

        let one = smallest_callable_containing(&rows, source, "helper(1)")?;
        let two = smallest_callable_containing(&rows, source, "helper(2)")?;
        assert_eq!(one.1, two.1, "same-role siblings share a durable family");
        assert_ne!(one.0, two.0, "ambiguous siblings retain separate item rows");
        assert_ne!(one.2, two.2, "source sites distinguish current instances");
        assert_ne!((one.3, one.4), (two.3, two.4));
        let ambiguous = AnonymousCallableFamilyMultiplicity::Ambiguous { instance_count: 2 };
        assert_eq!(one.5, ambiguous);
        assert_eq!(two.5, ambiguous);
        assert!(
            matches!(ir.item(one.0).map(|item| item.name()), Some(name) if name.named_bytes().is_none() && name.anonymous_anchor().is_some())
        );

        let helper = ir
            .canonical_entities()
            .find(|entity| {
                entity.kind == EntityKind::Function
                    && entity.name.named_atom().and_then(|atom| ir.atom(atom))
                        == Some(&b"helper"[..])
            })
            .ok_or(LaneError::Missing("named helper declaration"))?;
        let call_start = source
            .find("helper(captured + entry)")
            .ok_or(LaneError::Missing("captured helper call"))?;
        let call_start_u32 = u32::try_from(call_start)?;
        let call_end_u32 = u32::try_from(
            call_start
                .checked_add("helper".len())
                .ok_or(LaneError::Scalar)?,
        )?;
        let call_link = ir
            .link_occurrences()
            .find_map(|(_, occurrence)| {
                let site = occurrence.source?;
                let start = usize::try_from(site.start()).ok()?;
                let end = usize::try_from(site.end()).ok()?;
                (start == call_start && source.get(start..end) == Some("helper"))
                    .then(|| ir.link(occurrence.link))?
            })
            .ok_or(LaneError::Missing("captured helper call relation"))?;
        assert_eq!(call_link.kind, LinkKind::Calls);
        assert_eq!(
            call_link.target,
            backend_semantic::ir::LinkTarget::Local(helper.id)
        );
        let map_owner = rows
            .iter()
            .filter(|row| row.3 <= call_start_u32 && call_end_u32 <= row.4)
            .min_by_key(|row| row.4.saturating_sub(row.3))
            .ok_or(LaneError::Missing("map callback owner"))?;
        assert_eq!(
            call_link.from, map_owner.0,
            "captured call stays owned by its arrow"
        );

        let body_edited = source.replace("helper(1)", "helper(10000)");
        let body_ir = owned_tsz_ir(&body_edited)?;
        let body_rows = anonymous_callable_rows(&body_ir);
        let edited_one = smallest_callable_containing(&body_rows, &body_edited, "helper(10000)")?;
        let edited_two = smallest_callable_containing(&body_rows, &body_edited, "helper(2)")?;
        assert_eq!(
            edited_one.1, one.1,
            "body edits preserve callable family lineage"
        );
        assert_eq!(
            edited_two.1, two.1,
            "enclosing callback body edits preserve child lineage"
        );
        assert_eq!(edited_one.5, ambiguous);
        assert_eq!(edited_two.5, ambiguous);
        assert_ne!((edited_one.3, edited_one.4), (one.3, one.4));

        let relabeled = source.replace("describe('suite'", "describe('a longer suite'");
        let relabeled_ir = owned_tsz_ir(&relabeled)?;
        let relabeled_rows = anonymous_callable_rows(&relabeled_ir);
        let relabeled_one = smallest_callable_containing(&relabeled_rows, &relabeled, "helper(1)")?;
        let relabeled_two = smallest_callable_containing(&relabeled_rows, &relabeled, "helper(2)")?;
        assert_eq!(
            relabeled_one.1, one.1,
            "description text is not route identity"
        );
        assert_eq!(
            relabeled_two.1, two.1,
            "description text is not route identity"
        );

        let order = "  it('one', () => helper(1));\n  it('two', () => helper(2));";
        let reversed = "  it('two', () => helper(2));\n  it('one', () => helper(1));";
        let reordered = source.replace(order, reversed);
        let reordered_ir = owned_tsz_ir(&reordered)?;
        let reordered_rows = anonymous_callable_rows(&reordered_ir);
        let reordered_one = smallest_callable_containing(&reordered_rows, &reordered, "helper(1)")?;
        let reordered_two = smallest_callable_containing(&reordered_rows, &reordered, "helper(2)")?;
        assert_eq!(
            reordered_one.1, one.1,
            "sibling reordering preserves the family"
        );
        assert_eq!(
            reordered_two.1, two.1,
            "sibling reordering preserves the family"
        );
        assert_eq!(
            reordered_rows.len(),
            rows.len(),
            "reordering keeps both instances"
        );
        Ok(())
    }

    #[test]
    fn native_tsz_query_projection_boundaries_preserve_output_and_cancellation()
    -> Result<(), Box<dyn std::error::Error>> {
        use backend_frontend_typescript::{
            TszAuthorityError, TszExecutionCheckpoint, TszProjectExecutionEvent, TszProjectExecutionPhase as P,
            TszProjectExecutionStop, TszProjectExecutionTransition as T,
            TszProjectAuthority, TszFileInput, TszProjectOptions, TszCheckerOptions,
            TszProjectSemanticOptions, TszEnvironmentFingerprint, TszProjectCheckerExecution,
            TszProjectQuerySessionError,
        };
        use std::sync::{atomic::{AtomicBool, Ordering}, Mutex};
        type Boundary = (P, T, Option<(usize, String)>);
        struct Observe {
            cancelled: AtomicBool,
            arm: Option<P>,
            events: Mutex<Vec<Boundary>>,
        }
        impl TszExecutionCheckpoint for Observe {
            fn checkpoint(&self, _: u64) -> Result<(), TszProjectExecutionStop> {
                if self.cancelled.load(Ordering::Acquire) {
                    Err(TszProjectExecutionStop::Cancelled)
                } else { Ok(()) }
            }
            fn observe(&self, event: TszProjectExecutionEvent<'_>) {
                self.events.lock().unwrap().push((event.phase, event.transition,
                    event.file.map(|file| (file.index, file.path.to_owned()))));
                if event.transition == T::Entered && Some(event.phase) == self.arm {
                    self.cancelled.store(true, Ordering::Release);
                }
            }
        }
        // A small real native program exercises both authored occurrences and
        // computed types. It is not a reproduction of Paperless's dependencies.
        fn canonical(facts: &FactSet<'_>, source: &str) -> Result<Vec<u8>, LaneError> {
            let identity = backend_semantic::ir::SourceIdentity {
                identity: ContentId::<SourceFactDomain>::from_canonical_bytes(source.as_bytes()),
                byte_len: u32::try_from(source.len())?,
            };
            let recipe = CompileRecipeFact::derive(
                LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
                Stage::LowerIr, NativeTool::TypeScriptCompiler, identity.identity,
                ContentId::<ToolchainDomain>::from_canonical_bytes(b"query-timing"),
            );
            let ir = facts.build_ir(recipe.profile, identity, recipe,
                crate::driver::types::DeclarationScope::fixture())?;
            let mut bytes = vec![0; backend_semantic::ir::full_semantic_image_len(&ir)
                .map_err(|_| LaneError::Missing("canonical image size"))?];
            backend_semantic::ir::encode_full_semantic_image(&ir, &mut bytes)
                .map_err(|_| LaneError::Missing("canonical image encoding"))?;
            Ok(bytes)
        }
        let source = "export const value: number = 1; export function read(): number { return value; }";
        let phases = [P::FileQueryAnalysis, P::FileQueryStructure,
            P::FileQueryOccurrences, P::FileQueryTypes];
        let mut expected = None;
        for arm in [None, None, Some(phases[0]), Some(phases[1]), Some(phases[2]), Some(phases[3])] {
            let checkpoint = Observe { cancelled: AtomicBool::new(false), arm, events: Mutex::new(Vec::new()) };
            let mut authority = TszProjectAuthority::new();
            authority.update_with_checker_execution(vec![TszFileInput {
                path: "src/query.ts".into(), source: source.into(),
            }], TszProjectOptions {
                checker: TszCheckerOptions { no_lib: true, ..TszCheckerOptions::default() },
                semantic_options: TszProjectSemanticOptions::declaration_scoped(),
                program_diagnostics: Vec::new(), module_resolutions: Vec::new(), file_formats: Vec::new(),
                environment: TszEnvironmentFingerprint::from_sha256([0x75; 32]),
            }, &[], TszProjectCheckerExecution {
                checkpoint: &checkpoint, workers: std::num::NonZeroUsize::MIN,
            })?;
            let project = authority.project().ok_or("missing checked program")?;
            let session = project.checked_query_session(&checkpoint)?;
            let index = session.file_index("src/query.ts")?;
            let mut facts = FactSet::new();
            let result = collect_with_tsz(TypeScriptSource::TypeScript, source.as_bytes(),
                project, "src/query.ts", &session, &mut facts);
            if arm.is_some() {
                assert!(matches!(result, Err(TypeScriptCollectError::TszAuthority(
                    TszAuthorityError::ProjectCheckerSession(
                        TszProjectQuerySessionError::ExecutionStopped(TszProjectExecutionStop::Cancelled))))));
                assert!(session.with_file_checker_and_types(index, |_, _, _, _| ()).is_err(),
                    "a cancelled query never becomes reusable authority");
            } else {
                result.map_err(LaneError::from)?;
                let actual = canonical(&facts, source)?;
                if let Some(expected) = &expected { assert_eq!(&actual, expected); }
                else { expected = Some(actual.clone()); }
                let events = checkpoint.events.lock().unwrap();
                let query = events.iter().position(|event| event.0 == P::FileQuery && event.1 == T::Entered).unwrap();
                assert_eq!(events[query].2.as_ref(), Some(&(index, "src/query.ts".to_owned())));
                let observed: Vec<_> = events[query..].iter().filter(|event| phases.contains(&event.0)).collect();
                assert_eq!(observed.len(), 8, "one entered/returned pair per real pass");
                for (pair, phase) in observed.chunks_exact(2).zip(phases) {
                    assert_eq!((pair[0].0, pair[0].1), (phase, T::Entered));
                    assert_eq!((pair[1].0, pair[1].1), (phase, T::Returned));
                    assert_eq!(pair[0].2, None, "subpasses inherit the enclosing query's file");
                    assert_eq!(pair[1].2, None);
                }
                let checked = events.iter().filter(|event| event.0 == P::FileCheck && event.1 == T::Entered).count();
                drop(events);
                let mut repeated = FactSet::new();
                collect_with_tsz(TypeScriptSource::TypeScript, source.as_bytes(),
                    project, "src/query.ts", &session, &mut repeated).map_err(LaneError::from)?;
                assert_eq!(canonical(&repeated, source)?, actual,
                    "measurement does not change retained-session canonical output");
                assert_eq!(checkpoint.events.lock().unwrap().iter().filter(|event|
                    event.0 == P::FileCheck && event.1 == T::Entered).count(), checked,
                    "observing a second query does not recheck the source");
            }
        }
        Ok(())
    }

    #[test]
    fn native_tsz_preserves_reexport_owner_and_overload_implementation() -> Result<(), LaneError> {
        let implementation = "export class Service {\n  getHello(value: string): string;\n  getHello(value: number): string;\n  getHello(value: string | number): string { return ''; }\n}\n";
        let reexport = "export { Service as RenamedService } from './impl.js';\n";
        let dependency = "export declare class DependencyService { getHello(): string; }\n";
        let caller = "import { RenamedService } from './barrel.js';\nimport { DependencyService } from '../node_modules/@fixture/lib/index.js';\nconst local = new RenamedService();\nconst dependency = new DependencyService();\nexport function useLocal() { return local.getHello('local'); }\nexport function useDependency() { return dependency.getHello(); }\n";
        let module_resolutions = vec![
            module_resolution(
                "src/barrel.ts",
                "./impl.js",
                TszProjectModuleRequestKind::EsmReExport,
                TszProjectModuleResolutionTarget::File {
                    path: "src/impl.ts".to_owned(),
                },
            ),
            module_resolution(
                "src/caller.ts",
                "./barrel.js",
                TszProjectModuleRequestKind::EsmImport,
                TszProjectModuleResolutionTarget::File {
                    path: "src/barrel.ts".to_owned(),
                },
            ),
            module_resolution(
                "src/caller.ts",
                "../node_modules/@fixture/lib/index.js",
                TszProjectModuleRequestKind::EsmImport,
                TszProjectModuleResolutionTarget::File {
                    path: "node_modules/@fixture/lib/index.d.ts".to_owned(),
                },
            ),
        ];
        let authority = native_tsz_project_with_resolutions(
            &[
                ("src/impl.ts", implementation),
                ("src/barrel.ts", reexport),
                ("node_modules/@fixture/lib/index.d.ts", dependency),
                ("src/caller.ts", caller),
            ],
            &module_resolutions,
        )?;
        let project = authority
            .project()
            .ok_or(LaneError::Missing("alias TSZ project"))?;
        assert_exact_module_resolution_metadata(project, &module_resolutions);
        let caller_facts = collect_native_tsz_file(project, "src/caller.ts", caller)?;
        let targets = staged_tsz_coordinates(&caller_facts)?;
        let implementation_start = u32::try_from(
            implementation
                .find("getHello(value: string | number)")
                .ok_or(LaneError::Missing("overload implementation coordinate"))?,
        )?;
        assert!(
            targets.iter().any(|(kind, path, start, end, name, _)| {
                *kind == ReferenceKind::MethodCall
                    && path == "src/impl.ts"
                    && *start == implementation_start
                    && *end > *start
                    && *name == implementation_start
            }),
            "re-exported alias call must bind the implementation overload: {targets:?}"
        );
        assert!(
            targets.iter().any(|(kind, path, start, end, name, _)| {
                *kind == ReferenceKind::MethodCall
                    && path == "node_modules/@fixture/lib/index.d.ts"
                    && *start < *end
                    && *name >= *start
                    && *name < *end
            }),
            "same-named dependency member must keep its own receiver owner: {targets:?}"
        );
        assert_eq!(
            targets
                .iter()
                .filter(|(kind, _, _, _, _, _)| *kind == ReferenceKind::MethodCall)
                .count(),
            2,
            "each call site has one exact target, without a global name-only guess"
        );
        Ok(())
    }

    #[test]
    fn native_tsz_occurrence_targets_reuse_only_their_exact_session_source() -> Result<(), LaneError> {
        let models = format!(
            "/* {} */\nexport const version = 1;\nexport function alpha(value: number): number {{ return value; }}\nexport class Box {{ value = 1; getValue(): number {{ return this.value; }} }}\n",
            "target source padding ".repeat(256)
        );
        let mut caller = String::from(
            "import { alpha as run, version, Box } from './models';\nconst box = new Box();\nfunction local(value: number): number { return value; }\nexport function repeated() { let total = version;\n",
        );
        for value in 0..8 {
            caller.push_str(&format!(
                    "total += run({value}); total += box.getValue(); total += box.value; total += local({value});\n"
                ));
        }
        caller.push_str("return total; }\n");
        let resolutions = [module_resolution(
            "caller.ts",
            "./models",
            TszProjectModuleRequestKind::EsmImport,
            TszProjectModuleResolutionTarget::File {
                path: "models.ts".to_owned(),
            },
        )];
        let authority = native_tsz_project_with_resolutions(
            &[
                ("models.ts", models.as_str()),
                ("caller.ts", caller.as_str()),
            ],
            &resolutions,
        )?;
        let project = authority
            .project()
            .ok_or(LaneError::Missing("target cache project"))?;
        let session = project
            .checked_query_session_unmetered_for_test()
            .map_err(|_| LaneError::Missing("target cache session"))?;
        let model_index = session
            .file_index("models.ts")
            .map_err(|_| LaneError::Missing("exact model index"))?;
        let caller_index = session
            .file_index("caller.ts")
            .map_err(|_| LaneError::Missing("exact caller index"))?;
        let model_file = &project.program().files[model_index];
        let expected = *ContentId::<SourceFactDomain>::from_canonical_bytes(models.as_bytes());
        assert_eq!(
            session.native_source_identity_for_bound_file(model_index, model_file),
            Some(expected)
        );
        assert_eq!(
            session.native_source_identity_for_bound_file(
                model_index,
                &project.program().files[caller_index]
            ),
            None,
            "another file at an admitted index cannot acquire the target digest"
        );
        assert_eq!(
            session.native_source_identity_for_bound_file(usize::MAX, model_file),
            None
        );
        let initial_work = session.native_identity_hash_work_for_test();
        assert_eq!(initial_work, (2, (models.len() + caller.len()) as u64));

        // Same paths/text in a separately checked program are still another owner.
        let other_authority = native_tsz_project_with_resolutions(
            &[
                ("caller.ts", caller.as_str()),
                ("models.ts", models.as_str()),
            ],
            &resolutions,
        )?;
        let other = other_authority
            .project()
            .ok_or(LaneError::Missing("other target cache project"))?;
        let other_session = other
            .checked_query_session_unmetered_for_test()
            .map_err(|_| LaneError::Missing("other target cache session"))?;
        let other_model = &other.program().files[other_session
            .file_index("models.ts")
            .map_err(|_| LaneError::Missing("exact model index"))?];
        assert_eq!(
            session.native_source_identity_for_bound_file(model_index, other_model),
            None
        );

        for _ in 0..2 {
            let mut facts = FactSet::new();
            collect_with_tsz(
                TypeScriptSource::TypeScript,
                caller.as_bytes(),
                project,
                "caller.ts",
                &session,
                &mut facts,
            )?;
            let alpha_name = models.find("alpha(value").ok_or(LaneError::Missing("authored alpha name"))?;
            let box_name = models.find("Box {").ok_or(LaneError::Missing("authored Box name"))?;
            let constructor_site = caller.find("new Box()").ok_or(LaneError::Missing("authored constructor"))? + 4;
            let mut alpha_sites = std::collections::BTreeSet::new();
            let mut constructors = 0;
            let mut calls = 0;
            let mut methods = 0;
            let mut fields = 0;
            let mut local_calls = 0;
            for index in 0..facts.occurrence_len {
                let occurrence = facts.occurrences[index];
                if matches!(occurrence.target, OccurrenceTarget::Local(_))
                    && occurrence.kind == ReferenceKind::FunctionCall
                    && occurrence.confidence == OccurrenceConfidence::Oracle
                {
                    local_calls += 1;
                }
                let Some(slot) = facts.occurrence_source_coordinate_slot[index] else {
                    continue;
                };
                let coordinate = TypeScriptSourceCoordinate::decode(
                    &facts.foreign_source_coordinate_text[slot as usize],
                )
                .ok_or(LaneError::Missing("cached native target coordinate"))?;
                assert_eq!(coordinate.path, "models.ts");
                assert_eq!(coordinate.source, expected);
                assert_eq!(Some(coordinate.program), session.native_program_identity());
                assert_eq!(occurrence.confidence, OccurrenceConfidence::Oracle);
                match occurrence.kind {
                    ReferenceKind::FunctionCall => {
                        let owner = facts.provenance.source_spans()[facts.occurrence_owners[index] as usize]
                            .ok_or(LaneError::Missing("authored occurrence owner span"))?;
                        let start = owner.start + occurrence.span.start;
                        let end = owner.start + occurrence.span.end;
                        if coordinate.name_start as usize == alpha_name {
                            assert_eq!(caller.get(start as usize..end as usize), Some("run"));
                            assert!(alpha_sites.insert((start, end)), "one occurrence per authored alpha call");
                            calls += 1;
                        } else {
                            assert_eq!(coordinate.name_start as usize, box_name,
                                "only the authored Box construction is an additional foreign call");
                            assert_eq!((start as usize, end as usize), (constructor_site, constructor_site + 3));
                            assert_eq!(caller.get(start as usize..end as usize), Some("Box"));
                            constructors += 1;
                        }
                        assert!(coordinate.declaration_start <= coordinate.name_start
                            && coordinate.name_start < coordinate.declaration_end);
                    },
                    ReferenceKind::MethodCall => methods += 1,
                    ReferenceKind::FieldAccess => fields += 1,
                    _ => {}
                }
            }
            assert_eq!(
                calls, 8,
                "every imported call retains its exact authored target"
            );
            assert_eq!(constructors, 1, "the actual new Box() site retains its exact class target");
            let expected_alpha_sites = caller.match_indices("run(").map(|(start, _)|
                (start as u32, start as u32 + 3)).collect::<std::collections::BTreeSet<_>>();
            assert_eq!(alpha_sites, expected_alpha_sites, "all eight authored imported sites, with no extras");
            assert_eq!(
                methods, 8,
                "repeated receiver-owned methods retain their target"
            );
            assert!(
                fields >= 8,
                "each receiver-owned field read retains a native target"
            );
            assert_eq!(
                local_calls, 8,
                "same-file calls remain local rather than foreign"
            );
            assert_eq!(session.native_identity_hash_work_for_test(), initial_work);
        }
        Ok(())
    }

    #[test]
    fn native_tsz_session_identity_cache_preserves_outputs_and_exact_source_ownership()
    -> Result<(), LaneError> {
        use backend_semantic::ir::{
            NativeProgramSource, NativeProgramSourceManifest, SourceIdentity,
            encode_full_semantic_image, full_semantic_image_len, typescript_program_identity,
        };

        fn manifest(project: &TszProject, wrong_length: bool) -> NativeProgramSourceManifest {
            let rows = project
                .program()
                .files
                .iter()
                .enumerate()
                .map(|(index, file)| {
                    let source = file
                        .arena
                        .get_source_file_at(file.source_file)
                        .expect("actual source");
                    NativeProgramSource {
                        program_path: file.file_name.clone(),
                        source: SourceIdentity {
                            identity: ContentId::<SourceFactDomain>::from_canonical_bytes(
                                source.text.as_bytes(),
                            ),
                            byte_len: u32::try_from(source.text.len())
                                .expect("fixture source length")
                                + if wrong_length && index == 0 { 1 } else { 0 },
                        },
                        package_path: Some(file.file_name.clone()),
                    }
                })
                .collect();
            NativeProgramSourceManifest::from_sources(
                ContentId::<ToolchainDomain>::from_canonical_bytes(
                    b"native-identity-cache-fixture",
                ),
                rows,
            )
            .expect("actual native membership")
        }
        fn bytes(facts: &FactSet<'_>, source: &str) -> Result<Vec<u8>, LaneError> {
            let identity = SourceIdentity {
                identity: ContentId::<SourceFactDomain>::from_canonical_bytes(source.as_bytes()),
                byte_len: u32::try_from(source.len())?,
            };
            let recipe = CompileRecipeFact::derive(
                LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
                Stage::LowerIr,
                NativeTool::TypeScriptCompiler,
                identity.identity,
                ContentId::<ToolchainDomain>::from_canonical_bytes(
                    b"native-identity-cache-fixture",
                ),
            );
            let ir = facts.build_ir(
                recipe.profile,
                identity,
                recipe,
                crate::driver::types::DeclarationScope::fixture(),
            )?;
            let mut encoded = vec![
                0;
                full_semantic_image_len(&ir)
                    .map_err(|_| LaneError::Missing("canonical image size"))?
            ];
            encode_full_semantic_image(&ir, &mut encoded)
                .map_err(|_| LaneError::Missing("canonical image encoding"))?;
            Ok(encoded)
        }
        let models = "export function alpha(value: number): number { return value; }\n";
        let caller =
            "import { alpha } from './models';\nexport function read() { return alpha(1); }\n";
        let resolutions = [module_resolution(
            "caller.ts",
            "./models",
            TszProjectModuleRequestKind::EsmImport,
            TszProjectModuleResolutionTarget::File {
                path: "models.ts".to_owned(),
            },
        )];
        let authority = native_tsz_project_with_resolutions(
            &[("models.ts", models), ("caller.ts", caller)],
            &resolutions,
        )?;
        let project = authority
            .project()
            .ok_or(LaneError::Missing("native cache project"))?;
        let actual_manifest = manifest(project, false);
        let expected_members = project
            .program()
            .files
            .iter()
            .map(|file| {
                let source = file
                    .arena
                    .get_source_file_at(file.source_file)
                    .expect("native source");
                (
                    file.file_name.clone(),
                    *ContentId::<SourceFactDomain>::from_canonical_bytes(source.text.as_bytes()),
                )
            })
            .collect::<Vec<_>>();
        let expected_program = typescript_program_identity(&expected_members);
        let expected_bytes = project
            .program()
            .files
            .iter()
            .map(|file| {
                file.arena
                    .get_source_file_at(file.source_file)
                    .expect("source")
                    .text
                    .len() as u64
            })
            .sum::<u64>();
        let session = project
            .checked_query_session_unmetered_for_test()
            .expect("native session")
            .authenticate_native_program_sources(&actual_manifest)
            .expect("exact captured source membership");
        assert_eq!(session.native_program_identity(), expected_program);
        assert_eq!(
            session.native_program_identity(),
            Some(actual_manifest.program())
        );
        let initial_work = session.native_identity_hash_work_for_test();
        assert_eq!(
            initial_work,
            (project.program().files.len(), expected_bytes)
        );
        let mut outputs = std::collections::BTreeMap::new();
        for (path, source) in [
            ("models.ts", models),
            ("caller.ts", caller),
            ("caller.ts", caller),
            ("models.ts", models),
        ] {
            let index = session.file_index(path).expect("exact selected file");
            assert_eq!(
                session.native_source_identity(index, source.as_bytes()),
                Some(*ContentId::<SourceFactDomain>::from_canonical_bytes(
                    source.as_bytes()
                ))
            );
            assert_eq!(
                session.native_source_identity(index, b"different compile bytes"),
                None
            );
            let mut facts = FactSet::new();
            collect_with_tsz(
                TypeScriptSource::TypeScript,
                source.as_bytes(),
                project,
                path,
                &session,
                &mut facts,
            )?;
            assert_eq!(facts.tsz_program_identity, expected_program);
            assert_eq!(
                facts.tsz_source_identity,
                Some(*ContentId::<SourceFactDomain>::from_canonical_bytes(
                    source.as_bytes()
                ))
            );
            let encoded = bytes(&facts, source)?;
            if let Some(previous) = outputs.insert(path, encoded.clone()) {
                assert_eq!(
                    previous, encoded,
                    "query order cannot change canonical output"
                );
            }
        }
        assert_eq!(
            session.native_identity_hash_work_for_test(),
            initial_work,
            "repeated actual lowering never rehashes the whole program in the identity cache"
        );
        assert!(
            project
                .checked_query_session_unmetered_for_test()
                .expect("native session")
                .authenticate_native_program_sources(&manifest(project, true))
                .is_err(),
            "equal source hashes cannot admit a false byte length"
        );

        // These are real separate TSZ programs, not fabricated IR. Reordering
        // inputs preserves identity; source edits, adopted files and path moves
        // must not borrow the original program's cached ownership.
        let edited = "export function alpha(value: number): number { return value + 1; }\n";
        for (files, target, same_program) in [
            (
                vec![("caller.ts", caller), ("models.ts", models)],
                "models.ts",
                true,
            ),
            (
                vec![("caller.ts", caller), ("models.ts", edited)],
                "models.ts",
                false,
            ),
            (
                vec![
                    ("caller.ts", caller),
                    ("models.ts", models),
                    ("adopted.ts", models),
                ],
                "models.ts",
                false,
            ),
            (
                vec![("caller.ts", caller), ("moved.ts", models)],
                "moved.ts",
                false,
            ),
        ] {
            let resolutions = [module_resolution(
                "caller.ts",
                "./models",
                TszProjectModuleRequestKind::EsmImport,
                TszProjectModuleResolutionTarget::File {
                    path: target.to_owned(),
                },
            )];
            let other_authority = native_tsz_project_with_resolutions(&files, &resolutions)?;
            let other = other_authority
                .project()
                .ok_or(LaneError::Missing("other native program"))?;
            let other_manifest = manifest(other, false);
            let other_session = other
                .checked_query_session_unmetered_for_test()
                .expect("native session")
                .authenticate_native_program_sources(&other_manifest)
                .expect("other exact source membership");
            assert_eq!(
                other_session.native_program_identity() == expected_program,
                same_program
            );
            if !same_program {
                assert!(
                    other
                        .checked_query_session_unmetered_for_test()
                        .expect("native session")
                        .authenticate_native_program_sources(&actual_manifest)
                        .is_err(),
                    "changed/adopted/moved source cannot adopt a stale membership"
                );
            }
            let mut facts = FactSet::new();
            let result = collect_with_tsz(
                TypeScriptSource::TypeScript,
                caller.as_bytes(),
                other,
                "caller.ts",
                &session,
                &mut facts,
            );
            assert!(matches!(
                result,
                Err(TypeScriptCollectError::TszAuthority(
                    backend_frontend_typescript::TszAuthorityError::ProjectSessionMismatch
                ))
            ));
            assert_eq!(
                facts.len(),
                0,
                "wrong owner is refused before any fact publication"
            );
        }
        let mut facts = FactSet::new();
        assert!(
            collect_with_tsz(
                TypeScriptSource::TypeScript,
                b"different source",
                project,
                "caller.ts",
                &session,
                &mut facts
            )
            .is_err()
        );
        assert_eq!(
            facts.len(),
            0,
            "changed compile bytes publish no cached facts"
        );
        Ok(())
    }

    #[test]
    fn native_tsz_named_import_sites_and_bare_uses_keep_exact_local_targets() -> Result<(), LaneError> {
        use backend_compile::SourceBindingIdentity;
        let scheduler = "export interface Workout { name: string; }\nexport function generateSchedule(request: Workout): string { return request.name; }\nexport const scheduleVersion = 1;\n";
        let navbar = "export default class NavBar {}\n";
        let caller = "import { generateSchedule, Workout as ImportedWorkout, scheduleVersion } from '@/lib/scheduler';\nimport NavBar from '@/components/NavBar';\nimport { remote } from 'vendor';\nexport function POST(request: ImportedWorkout) { const run = generateSchedule; const component = NavBar; return run(request) + scheduleVersion; }\nexport function shadow(generateSchedule: number) { return generateSchedule; }\n";
        let path = "src/app/api/[[...v1]]/route.ts";
        let resolutions = [
            module_resolution(path, "@/lib/scheduler", TszProjectModuleRequestKind::EsmImport,
                TszProjectModuleResolutionTarget::File { path: "src/lib/scheduler.ts".to_owned() }),
            module_resolution(path, "@/components/NavBar", TszProjectModuleRequestKind::EsmImport,
                TszProjectModuleResolutionTarget::File { path: "src/components/NavBar.ts".to_owned() }),
            module_resolution(path, "vendor", TszProjectModuleRequestKind::EsmImport,
                TszProjectModuleResolutionTarget::Unresolved),
        ];
        let authority = native_tsz_project_with_resolutions(
            &[("src/lib/scheduler.ts", scheduler), ("src/components/NavBar.ts", navbar), (path, caller)],
            &resolutions,
        )?;
        let project = authority.project().ok_or(LaneError::Missing("import project"))?;
        assert_exact_module_resolution_metadata(project, &resolutions);
        let facts = collect_native_tsz_file(project, path, caller)?;
        let syntax = backend_frontend_typescript::syntax_frontend()
            .map_err(|_| LaneError::Missing("syntax frontend"))?
            .analyze(std::path::Path::new(path), caller.as_bytes())
            .map_err(|_| LaneError::Missing("syntax analysis"))?;
        for name in ["generateSchedule", "ImportedWorkout", "scheduleVersion", "NavBar"] {
            let imports = syntax.declarations().iter().filter(|row| row.name() == name).collect::<Vec<_>>();
            assert_eq!(imports.len(), 1, "one written import binding: {name}");
            let SourceBindingIdentity::Exact(capture) = imports[0].binding_identity() else {
                panic!("exact import source capture");
            };
            assert_eq!((0..facts.len()).filter(|&index| {
                facts.names[index] == name.as_bytes()
                    && facts.kinds[index] == EntityKind::Reexport
                    && facts.provenance.source_spans()[index]
                        .is_some_and(|span| (span.start, span.end) == capture.owner())
            }).count(), 1, "one compiler binding at the exact statement extent: {name}");
        }
        for (name, expected_path, kind, target_name, use_prefix) in [
            ("generateSchedule", "src/lib/scheduler.ts", EntityKind::Function, "generateSchedule", "const run = "),
            ("ImportedWorkout", "src/lib/scheduler.ts", EntityKind::Trait, "Workout", "POST(request: "),
            ("scheduleVersion", "src/lib/scheduler.ts", EntityKind::Constant, "scheduleVersion", "run(request) + "),
            ("NavBar", "src/components/NavBar.ts", EntityKind::Record, "NavBar", "const component = "),
        ] {
            let binding = (0..facts.len()).find(|&index| facts.names[index] == name.as_bytes()
                && facts.kinds[index] == EntityKind::Reexport).expect("import binding");
            let imported = (0..facts.occurrence_len).filter(|&index| {
                facts.occurrence_owners[index] == binding as u32
                    && facts.occurrences[index].kind == ReferenceKind::Import
            }).collect::<Vec<_>>();
            assert_eq!(imported.len(), 1, "one import edge for {name}");
            let index = imported[0];
            assert_eq!(facts.occurrences[index].confidence, OccurrenceConfidence::Oracle);
            let slot = facts.occurrence_source_coordinate_slot[index].expect("native source target");
            let coordinate = TypeScriptSourceCoordinate::decode(&facts.foreign_source_coordinate_text[slot as usize])
                .expect("typed native coordinate");
            assert_eq!(coordinate.path, expected_path);
            let OccurrenceTarget::Foreign(key) = facts.occurrences[index].target else { panic!("cross-file target"); };
            assert_eq!(key.kind, Some(kind));
            let target_source = if expected_path == "src/lib/scheduler.ts" { scheduler } else { navbar };
            let target_facts = collect_native_tsz_file(project, expected_path, target_source)?;
            let target = (0..target_facts.len()).find(|&index| target_facts.names[index] == target_name.as_bytes()
                && target_facts.kinds[index] == kind).expect("actual emitted native target");
            let extent = target_facts.provenance.source_spans()[target].expect("target source extent");
            let use_start = (caller.find(use_prefix).expect("exact bare use prefix") + use_prefix.len()) as u32;
            let uses = (0..facts.occurrence_len).filter(|&index| {
                let owner = facts.occurrence_owners[index] as usize;
                facts.provenance.source_spans()[owner].is_some_and(|span|
                    span.start + facts.occurrences[index].span.start == use_start
                        && span.start + facts.occurrences[index].span.end == use_start + name.len() as u32)
            }).collect::<Vec<_>>();
            assert_eq!(uses.len(), 1, "one exact bare use for {name}");
            let index = uses[0];
            assert_ne!(facts.occurrences[index].kind, ReferenceKind::Import);
            assert_eq!(facts.occurrences[index].confidence, OccurrenceConfidence::Oracle);
            let slot = facts.occurrence_source_coordinate_slot[index].expect("bare use native source target");
            let coordinate = TypeScriptSourceCoordinate::decode(&facts.foreign_source_coordinate_text[slot as usize])
                .expect("bare use native coordinate");
            assert_eq!(coordinate.path, expected_path, "{name}");
            assert_eq!((coordinate.declaration_start, coordinate.declaration_end), (extent.start, extent.end), "{name}");
            let OccurrenceTarget::Foreign(key) = facts.occurrences[index].target else { panic!("cross-file bare use"); };
            assert_eq!(key.kind, Some(kind), "{name}");
        }
        let remote = (0..facts.len()).find(|&index| facts.names[index] == b"remote").expect("external binding");
        assert!((0..facts.occurrence_len).any(|index| {
            facts.occurrence_owners[index] == remote as u32
                && facts.occurrences[index].confidence == OccurrenceConfidence::Import
                && matches!(facts.occurrences[index].target, OccurrenceTarget::Foreign(key)
                    if matches!(key.origin, ForeignOrigin::Package(_)))
        }), "unresolved modules retain their external operand");
        let shadow = caller.rfind("generateSchedule;").expect("shadow reference") as u32;
        assert!((0..facts.occurrence_len).any(|index| {
            let owner = facts.occurrence_owners[index] as usize;
            facts.provenance.source_spans()[owner].is_some_and(|span|
                span.start + facts.occurrences[index].span.start == shadow)
                && matches!(facts.occurrences[index].target, OccurrenceTarget::Local(_))
        }), "a same-spelled local parameter never borrows the import's target");
        Ok(())
    }

    #[test]
    fn native_tsz_named_imports_of_destructured_exports_keep_exact_leaf_targets()
    -> Result<(), LaneError> {
        use backend_semantic::ir::{
            DeclarationPublicationRole, ExternalTarget, ForeignTargetOrigin,
            SemanticCoreReader as _, SemanticImageView, encode_full_semantic_image,
            full_semantic_image_len,
        };

        fn canonical_bytes<'source>(
            facts: &FactSet<'source>,
            source: &str,
            path: &'source str,
        ) -> Result<Vec<u8>, LaneError> {
            let identity = backend_semantic::ir::SourceIdentity {
                identity: ContentId::<SourceFactDomain>::from_canonical_bytes(source.as_bytes()),
                byte_len: u32::try_from(source.len())?,
            };
            let recipe = CompileRecipeFact::derive(
                LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
                Stage::LowerIr,
                NativeTool::TypeScriptCompiler,
                identity.identity,
                ContentId::<ToolchainDomain>::from_canonical_bytes(
                    b"tsz-destructured-import-fixture",
                ),
            );
            let lineage =
                backend_semantic::ir::PackageLineage::new("npm", "destructured-import-fixture")
                    .map_err(|_| LaneError::Missing("fixture package lineage"))?;
            let scope = crate::driver::types::DeclarationScope::new(lineage, path)
                .map_err(|_| LaneError::Missing("exact package source scope"))?;
            let ir = facts.build_ir(recipe.profile, identity, recipe, scope)?;
            let length = full_semantic_image_len(&ir)
                .map_err(|_| LaneError::Missing("canonical native image length"))?;
            let mut bytes = vec![0; length];
            assert_eq!(
                encode_full_semantic_image(&ir, &mut bytes)
                    .map_err(|_| LaneError::Missing("canonical native image encoding"))?,
                length
            );
            Ok(bytes)
        }

        let dependency = concat!(
            "declare function fallback(): number;\n",
            "declare const input: { source?: number; nested: { count: number }; extra: boolean };\n",
            "export const { source: workout = fallback(), nested: { count }, ...rest } = input;\n",
            "export let [head, , ...tail]: number[] = [1, 2, 3];\n",
            "export function parameter({ workout }: { workout: number }) { return workout; }\n",
        );
        let caller = concat!(
            "import { workout as localWorkout, count, rest, head, tail } from '@/models';\n",
            "import { remote } from 'vendor';\n",
            "export function read() { return localWorkout + count + head + tail.length + rest.extra + remote(); }\n",
            "export function shadow(localWorkout: number) { return localWorkout; }\n",
        );
        let resolutions = [
            module_resolution(
                "caller.ts",
                "@/models",
                TszProjectModuleRequestKind::EsmImport,
                TszProjectModuleResolutionTarget::File {
                    path: "models.ts".to_owned(),
                },
            ),
            module_resolution(
                "caller.ts",
                "vendor",
                TszProjectModuleRequestKind::EsmImport,
                TszProjectModuleResolutionTarget::Unresolved,
            ),
        ];
        let authority = native_tsz_project_with_resolutions(
            &[("models.ts", dependency), ("caller.ts", caller)],
            &resolutions,
        )?;
        let project = authority
            .project()
            .ok_or(LaneError::Missing("native import project"))?;
        assert_exact_module_resolution_metadata(project, &resolutions);
        let targets = collect_native_tsz_file(project, "models.ts", dependency)?;
        let facts = collect_native_tsz_file(project, "caller.ts", caller)?;
        let target_bytes = canonical_bytes(&targets, dependency, "models.ts")?;
        let caller_bytes = canonical_bytes(&facts, caller, "caller.ts")?;
        let target_image = SemanticImageView::reopen(&target_bytes)
            .map_err(|_| LaneError::Missing("reopened actual target image"))?;
        let caller_image = SemanticImageView::reopen(&caller_bytes)
            .map_err(|_| LaneError::Missing("reopened actual caller image"))?;
        let bound_file = project
            .program()
            .files
            .iter()
            .find(|file| file.file_name == "models.ts")
            .ok_or(LaneError::Missing("native target file"))?;

        for (local, target_name, expected_kind) in [
            ("localWorkout", "workout", EntityKind::Constant),
            ("count", "count", EntityKind::Constant),
            ("rest", "rest", EntityKind::Constant),
            ("head", "head", EntityKind::Static),
            ("tail", "tail", EntityKind::Static),
        ] {
            let leaves = (0..targets.len())
                .filter(|&index| {
                    targets.names[index] == target_name.as_bytes()
                        && targets.kinds[index] == expected_kind
                        && targets.publication_roles[index]
                            == DeclarationPublicationRole::PrimaryOwned
                })
                .collect::<Vec<_>>();
            assert_eq!(leaves.len(), 1, "one actual authored target: {target_name}");
            let extent =
                targets.provenance.source_spans()[leaves[0]].expect("authored leaf extent");
            assert_eq!(
                &dependency.as_bytes()[extent.start as usize..extent.end as usize],
                target_name.as_bytes()
            );
            let native_leaf = bound_file
                .arena
                .nodes
                .iter()
                .enumerate()
                .find_map(|(raw, _)| {
                    let node = super::TszNodeIndex(u32::try_from(raw).ok()?);
                    (bound_file.arena.get_identifier_at(node).is_some()
                        && bound_file.arena.pos_end_at(node) == Some((extent.start, extent.end)))
                    .then_some(node)
                })
                .expect("actual native identifier leaf");
            assert_eq!(
                super::native_tsz_destructured_variable_kind(&bound_file.arena, native_leaf),
                Some(expected_kind)
            );
            let imported = (0..facts.len())
                .find(|&index| {
                    facts.names[index] == local.as_bytes()
                        && facts.kinds[index] == EntityKind::Reexport
                })
                .expect("actual authored import binding");
            let import_sites = (0..facts.occurrence_len)
                .filter(|&index| {
                    facts.occurrence_owners[index] == imported as u32
                        && facts.occurrences[index].kind == ReferenceKind::Import
                })
                .collect::<Vec<_>>();
            assert_eq!(import_sites.len(), 1, "one written import edge: {local}");
            let slot = facts.occurrence_source_coordinate_slot[import_sites[0]]
                .expect("destructured export must have native source target");
            assert_eq!(
                facts.occurrences[import_sites[0]].confidence,
                OccurrenceConfidence::Oracle
            );
            let coordinate = TypeScriptSourceCoordinate::decode(
                &facts.foreign_source_coordinate_text[slot as usize],
            )
            .expect("typed native target");
            assert_eq!(coordinate.path, "models.ts");
            assert_eq!(
                coordinate.source,
                *ContentId::<SourceFactDomain>::from_canonical_bytes(dependency.as_bytes())
            );
            assert_eq!(
                (
                    coordinate.declaration_start,
                    coordinate.declaration_end,
                    coordinate.name_start
                ),
                (extent.start, extent.end, extent.start)
            );
            // The native coordinate survives actual canonical image encoding
            // and names the exact owned target extent, never the aggregate.
            assert_eq!(
                target_image
                    .canonical_entities()
                    .filter(|entity| entity.kind == expected_kind
                        && entity.authority.publication == DeclarationPublicationRole::PrimaryOwned
                        && entity
                            .name
                            .named_atom()
                            .and_then(|atom| target_image.atom(atom))
                            == Some(target_name.as_bytes())
                        && entity.source.is_some_and(
                            |span| (span.start(), span.end()) == (extent.start, extent.end)
                        ))
                    .count(),
                1
            );
            assert!(
                caller_image.canonical_externals().any(|(_, target)| {
                    let ExternalTarget::Foreign(foreign) = target else {
                        return false;
                    };
                    let ForeignTargetOrigin::Universe { ecosystem } = foreign.origin else {
                        return false;
                    };
                    if caller_image.atom(ecosystem)
                        != Some(TYPESCRIPT_TSZ_SOURCE_ECOSYSTEM.as_bytes())
                        || foreign.kind != Some(expected_kind)
                    {
                        return false;
                    }
                    caller_image
                        .atom(foreign.path)
                        .and_then(|path| std::str::from_utf8(path).ok())
                        .and_then(TypeScriptSourceCoordinate::decode)
                        .is_some_and(|reopened| reopened == coordinate)
                }),
                "canonical endpoint retains exact leaf identity: {local}"
            );
            let body = caller.find("return localWorkout").expect("read body");
            let start = (body + caller[body..].find(local).expect("bare imported use")) as u32;
            assert!(
                (0..facts.occurrence_len).any(|index| facts.occurrence_source_coordinate_slot
                    [index]
                    .is_some()
                    && facts.occurrences[index].confidence == OccurrenceConfidence::Oracle
                    && facts.provenance.source_spans()[facts.occurrence_owners[index] as usize]
                        .is_some_and(
                            |owner| owner.start + facts.occurrences[index].span.start == start
                        )),
                "bare use retains native leaf authority: {local}"
            );
        }
        // Native parent edges must exclude property labels, default-expression
        // callees and parameter-pattern leaves from variable-target authority.
        for prefix in [
            "source: workout",
            "fallback(), nested",
            "workout }: { workout",
        ] {
            let start = dependency.find(prefix).expect("opposing native site") as u32;
            let end = start
                + prefix
                    .split(|c: char| !c.is_alphanumeric())
                    .next()
                    .expect("identifier")
                    .len() as u32;
            let node = bound_file
                .arena
                .nodes
                .iter()
                .enumerate()
                .find_map(|(raw, _)| {
                    let node = super::TszNodeIndex(u32::try_from(raw).ok()?);
                    (bound_file.arena.get_identifier_at(node).is_some()
                        && bound_file.arena.pos_end_at(node) == Some((start, end)))
                    .then_some(node)
                })
                .expect("actual opposing native identifier");
            assert_eq!(
                super::native_tsz_destructured_variable_kind(&bound_file.arena, node),
                None,
                "{prefix}"
            );
        }
        let remote = (0..facts.len())
            .find(|&index| {
                facts.names[index] == b"remote" && facts.kinds[index] == EntityKind::Reexport
            })
            .expect("vendor binding");
        assert!(
            (0..facts.occurrence_len).any(|index| facts.occurrence_owners[index] == remote as u32
                && facts.occurrences[index].confidence == OccurrenceConfidence::Import
                && matches!(facts.occurrences[index].target, OccurrenceTarget::Foreign(key)
                    if matches!(key.origin, ForeignOrigin::Package(_)))),
            "unresolved vendor remains foreign"
        );
        let shadow = caller.rfind("localWorkout;").expect("shadow use") as u32;
        assert!(
            (0..facts.occurrence_len).any(|index| facts.provenance.source_spans()
                [facts.occurrence_owners[index] as usize]
                .is_some_and(|owner| owner.start + facts.occurrences[index].span.start == shadow)
                && matches!(facts.occurrences[index].target, OccurrenceTarget::Local(_))),
            "same-spelled parameter never borrows the imported target"
        );
        Ok(())
    }

    #[test]
    fn native_tsz_namespace_callable_values_keep_exact_binding_targets() -> Result<(), LaneError> {
        let exports = concat!(
            "export const NavBar = (value: number): number => value;\n",
            "export let mutable = (value: number): number => value + 1;\n",
            "export const block = (value: number): number => { return value; } /* ;, not owned */ ;\n",
            "export const asi = (value: number): number => value\n",
            "export const comma = (value: number): number => value, last = (value: number): number => value;\n",
            "export const notCallable = 42;\n",
            "export function Mixed(value: number): number { return value; }\n",
            "export namespace Mixed { export const version = 1; }\n",
        );
        let other = "export const NavBar = (value: number): number => value + 2;\n";
        let barrel = "export { NavBar as draw } from './exports';\n";
        let caller = concat!(
            "import * as api from './barrel';\n",
            "import * as direct from './exports';\n",
            "import * as second from './other';\n",
            "import * as vendor from 'vendor';\n",
            "export function POST() { return api.draw(1) + direct.NavBar(2) + direct.mutable(3) + second.NavBar(4) + direct.Mixed(5) + direct.block(6) + direct.asi(7) + direct.comma(8) + direct.last(9); }\n",
            "export function shadow(api: { draw(value: number): number }) { return api.draw(6); }\n",
            "export function foreign() { return vendor.draw(7); }\n",
            "export const bothRoles = direct.Mixed;\n",
            "export function invalid() { return direct.notCallable(9); }\n",
        );
        let another =
            "import * as api from './barrel';\nexport function another() { return api.draw(8); }\n";
        let resolutions = [
            module_resolution(
                "caller.ts",
                "./barrel",
                TszProjectModuleRequestKind::EsmImport,
                TszProjectModuleResolutionTarget::File {
                    path: "barrel.ts".to_owned(),
                },
            ),
            module_resolution(
                "another.ts",
                "./barrel",
                TszProjectModuleRequestKind::EsmImport,
                TszProjectModuleResolutionTarget::File {
                    path: "barrel.ts".to_owned(),
                },
            ),
            module_resolution(
                "barrel.ts",
                "./exports",
                TszProjectModuleRequestKind::EsmReExport,
                TszProjectModuleResolutionTarget::File {
                    path: "exports.ts".to_owned(),
                },
            ),
            module_resolution(
                "caller.ts",
                "./exports",
                TszProjectModuleRequestKind::EsmImport,
                TszProjectModuleResolutionTarget::File {
                    path: "exports.ts".to_owned(),
                },
            ),
            module_resolution(
                "caller.ts",
                "./other",
                TszProjectModuleRequestKind::EsmImport,
                TszProjectModuleResolutionTarget::File {
                    path: "other.ts".to_owned(),
                },
            ),
            module_resolution(
                "caller.ts",
                "vendor",
                TszProjectModuleRequestKind::EsmImport,
                TszProjectModuleResolutionTarget::Unresolved,
            ),
        ];
        let authority = native_tsz_project_with_resolutions(
            &[
                ("exports.ts", exports),
                ("other.ts", other),
                ("barrel.ts", barrel),
                ("caller.ts", caller),
                ("another.ts", another),
            ],
            &resolutions,
        )?;
        let project = authority
            .project()
            .ok_or(LaneError::Missing("namespace function-value project"))?;
        assert_exact_module_resolution_metadata(project, &resolutions);
        for (path, source, cases) in [
            (
                "caller.ts",
                caller,
                vec![
                    ("api.", "draw", "exports.ts", "NavBar", EntityKind::Constant),
                    (
                        "direct.",
                        "NavBar",
                        "exports.ts",
                        "NavBar",
                        EntityKind::Constant,
                    ),
                    (
                        "direct.",
                        "mutable",
                        "exports.ts",
                        "mutable",
                        EntityKind::Static,
                    ),
                    (
                        "second.",
                        "NavBar",
                        "other.ts",
                        "NavBar",
                        EntityKind::Constant,
                    ),
                    (
                        "direct.",
                        "Mixed",
                        "exports.ts",
                        "Mixed",
                        EntityKind::Function,
                    ),
                    (
                        "direct.",
                        "block",
                        "exports.ts",
                        "block",
                        EntityKind::Constant,
                    ),
                    ("direct.", "asi", "exports.ts", "asi", EntityKind::Constant),
                    (
                        "direct.",
                        "comma",
                        "exports.ts",
                        "comma",
                        EntityKind::Constant,
                    ),
                    (
                        "direct.",
                        "last",
                        "exports.ts",
                        "last",
                        EntityKind::Constant,
                    ),
                ],
            ),
            (
                "another.ts",
                another,
                vec![("api.", "draw", "exports.ts", "NavBar", EntityKind::Constant)],
            ),
        ] {
            let facts = collect_native_tsz_file(project, path, source)?;
            for (receiver, name, target_path, target_name, kind) in cases {
                let token = format!("{receiver}{name}(");
                let start = (source.find(&token).expect("authored exact call") + receiver.len()) as u32;
                let sites = (0..facts.occurrence_len)
                    .filter(|&index| {
                        let owner = facts.occurrence_owners[index] as usize;
                        facts.provenance.source_spans()[owner].is_some_and(|span| {
                            span.start + facts.occurrences[index].span.start == start
                                && span.start + facts.occurrences[index].span.end
                                    == start + name.len() as u32
                        })
                    })
                    .collect::<Vec<_>>();
                assert_eq!(
                    sites.len(),
                    1,
                    "one call token, no external twin: {path}:{token}"
                );
                let site = sites[0];
                assert_eq!(facts.occurrences[site].kind, ReferenceKind::MethodCall);
                assert_eq!(
                    facts.occurrences[site].confidence,
                    OccurrenceConfidence::Oracle
                );
                let slot = facts.occurrence_source_coordinate_slot[site]
                    .expect("compiler-owned binding coordinate");
                let coordinate = TypeScriptSourceCoordinate::decode(
                    &facts.foreign_source_coordinate_text[slot as usize],
                )
                .expect("typed selected source target");
                let target_source = if target_path == "other.ts" {
                    other
                } else {
                    exports
                };
                let targets = collect_native_tsz_file(project, target_path, target_source)?;
                let target = (0..targets.len())
                    .find(|&index| {
                        targets.names[index] == target_name.as_bytes() && targets.kinds[index] == kind
                    })
                    .expect("actual published declaration, not a display-name guess");
                let span = targets.provenance.source_spans()[target].expect("authored target extent");
                assert_eq!(coordinate.path, target_path);
                assert_eq!(
                    coordinate.source,
                    *ContentId::<SourceFactDomain>::from_canonical_bytes(target_source.as_bytes())
                );
                assert_eq!(
                    (coordinate.declaration_start, coordinate.declaration_end),
                    (span.start, span.end)
                );
                assert!(
                    matches!(facts.occurrences[site].target, OccurrenceTarget::Foreign(key) if key.kind == Some(kind))
                );
            }
            if path == "caller.ts" {
                for token in [
                    "api.draw(6)",
                    "vendor.draw(7)",
                    "direct.Mixed;",
                    "direct.notCallable(9)",
                ] {
                    let name = token.split('.').nth(1).expect("member");
                    let name = name.split(['(', ';']).next().expect("member token");
                    let start = (source.find(token).expect("opposing call")
                        + token.find('.').expect("receiver")
                        + 1) as u32;
                    assert!(
                        !(0..facts.occurrence_len).any(|index| {
                            let owner = facts.occurrence_owners[index] as usize;
                            facts.provenance.source_spans()[owner].is_some_and(|span| {
                                span.start + facts.occurrences[index].span.start == start
                                    && span.start + facts.occurrences[index].span.end
                                        == start + name.len() as u32
                            }) && facts.occurrence_source_coordinate_slot[index].is_some()
                        }),
                        "shadow, unknown module, or mixed bare roles cannot borrow a target: {token}"
                    );
                }
            }
        }
        Ok(())
    }

    #[test]
    fn native_tsz_type_only_imports_do_not_guess_merged_or_overloaded_targets()
    -> Result<(), LaneError> {
        let dependency = "export interface Workout { name: string; }\nexport interface Merged { first: number; }\nexport interface Merged { second: number; }\nexport declare function pick(value: string): string;\nexport declare function pick(value: number): number;\nexport function Mixed() {}\nexport namespace Mixed { export const version = 1; }\n";
        let caller = "import type { Workout, Merged } from '@/models';\nimport { pick, Mixed } from '@/models';\nexport function take(value: Workout, merged: Merged) { const bothRoles = Mixed; return pick; }\n";
        let resolutions = [module_resolution("caller.ts", "@/models", TszProjectModuleRequestKind::EsmImport,
            TszProjectModuleResolutionTarget::File { path: "models.ts".to_owned() })];
        let authority = native_tsz_project_with_resolutions(
            &[("models.ts", dependency), ("caller.ts", caller)], &resolutions,
        )?;
        let project = authority.project().ok_or(LaneError::Missing("type-only import project"))?;
        let facts = collect_native_tsz_file(project, "caller.ts", caller)?;
        for (name, exact) in [("Workout", true), ("Merged", false), ("pick", false), ("Mixed", false)] {
            let fact = (0..facts.len()).find(|&index| facts.names[index] == name.as_bytes()
                && facts.kinds[index] == EntityKind::Reexport).expect("real import binding");
            let edge = (0..facts.occurrence_len).find(|&index| facts.occurrence_owners[index] == fact as u32
                && facts.occurrences[index].kind == ReferenceKind::Import).expect("written import edge");
            assert_eq!(facts.occurrence_source_coordinate_slot[edge].is_some(), exact,
                "only a singular exact declaration supplies the import target: {name}");
            assert_eq!(facts.occurrences[edge].confidence,
                if exact { OccurrenceConfidence::Oracle } else { OccurrenceConfidence::Import });
        }
        Ok(())
    }

    #[test]
    fn native_tsz_namespace_members_keep_terminal_export_owner_and_preserve_opposers()
    -> Result<(), LaneError> {
        let scheduler = "export function generateSchedule(value: number) { return value; }\nexport const version = 1;\nexport default class NavBar {}\n";
        let barrel = "export { generateSchedule as schedule } from './scheduler';\n";
        let caller = concat!(
            "import * as scheduler from '@/scheduler';\n",
            "import * as api from './barrel';\n",
            "import * as vendor from 'vendor';\n",
            "export function POST() { const run = scheduler.generateSchedule; const component = scheduler.default; return scheduler.generateSchedule(scheduler.version) + api.schedule(1); }\n",
            "export function shadow(scheduler: { generateSchedule(value: number): number }) { return scheduler.generateSchedule(2); }\n",
            "export function foreign() { return vendor.generateSchedule(3); }\n",
        );
        let resolutions = [
            module_resolution("caller.ts", "@/scheduler", TszProjectModuleRequestKind::EsmImport,
                TszProjectModuleResolutionTarget::File { path: "scheduler.ts".to_owned() }),
            module_resolution("caller.ts", "./barrel", TszProjectModuleRequestKind::EsmImport,
                TszProjectModuleResolutionTarget::File { path: "barrel.ts".to_owned() }),
            module_resolution("barrel.ts", "./scheduler", TszProjectModuleRequestKind::EsmReExport,
                TszProjectModuleResolutionTarget::File { path: "scheduler.ts".to_owned() }),
            module_resolution("caller.ts", "vendor", TszProjectModuleRequestKind::EsmImport,
                TszProjectModuleResolutionTarget::Unresolved),
        ];
        let authority = native_tsz_project_with_resolutions(
            &[("scheduler.ts", scheduler), ("barrel.ts", barrel), ("caller.ts", caller)],
            &resolutions,
        )?;
        let project = authority.project().ok_or(LaneError::Missing("namespace import project"))?;
        assert_exact_module_resolution_metadata(project, &resolutions);
        let facts = collect_native_tsz_file(project, "caller.ts", caller)?;
        let target_facts = collect_native_tsz_file(project, "scheduler.ts", scheduler)?;
        for (prefix, name, kind) in [
            ("const run = scheduler.", "generateSchedule", EntityKind::Function),
            ("const component = scheduler.", "default", EntityKind::Record),
            ("return scheduler.", "generateSchedule", EntityKind::Function),
            ("generateSchedule(scheduler.", "version", EntityKind::Constant),
            (" + api.", "schedule", EntityKind::Function),
        ] {
            let start = (caller.find(prefix).expect("exact namespace member site") + prefix.len()) as u32;
            let sites = (0..facts.occurrence_len).filter(|&index| {
                let owner = facts.occurrence_owners[index] as usize;
                facts.provenance.source_spans()[owner].is_some_and(|span|
                    span.start + facts.occurrences[index].span.start == start
                        && span.start + facts.occurrences[index].span.end == start + name.len() as u32)
            }).collect::<Vec<_>>();
            assert_eq!(sites.len(), 1, "one exact namespace member occurrence: {prefix}{name}");
            let site = sites[0];
            let slot = facts.occurrence_source_coordinate_slot[site]
                .expect("namespace member has its terminal export's source coordinate");
            let coordinate = TypeScriptSourceCoordinate::decode(
                &facts.foreign_source_coordinate_text[slot as usize],
            ).expect("typed terminal namespace member target");
            let target_name = match name { "schedule" => "generateSchedule", "default" => "NavBar", _ => name };
            let target = (0..target_facts.len()).find(|&index|
                target_facts.names[index] == target_name.as_bytes() && target_facts.kinds[index] == kind)
                .expect("actual published terminal export");
            let extent = target_facts.provenance.source_spans()[target].expect("terminal declaration extent");
            assert_eq!(coordinate.path, "scheduler.ts");
            assert_eq!(coordinate.source, *ContentId::<SourceFactDomain>::from_canonical_bytes(scheduler.as_bytes()));
            assert_eq!((coordinate.declaration_start, coordinate.declaration_end), (extent.start, extent.end));
            assert_eq!(facts.occurrences[site].confidence, OccurrenceConfidence::Oracle);
            assert!(matches!(facts.occurrences[site].target, OccurrenceTarget::Foreign(key) if key.kind == Some(kind)));
        }
        for prefix in ["return scheduler.generateSchedule(2)", "return vendor.generateSchedule(3)"] {
            let start = (caller.find(prefix).expect("opposing member site") + "return ".len()
                + prefix["return ".len()..].find('.').expect("receiver member") + 1) as u32;
            assert!(!(0..facts.occurrence_len).any(|index| {
                let owner = facts.occurrence_owners[index] as usize;
                facts.provenance.source_spans()[owner].is_some_and(|span|
                    span.start + facts.occurrences[index].span.start == start)
                    && facts.occurrence_source_coordinate_slot[index].is_some()
            }), "shadowing or unresolved namespace receiver never borrows a local module: {prefix}");
        }
        // A whole module has no authored declaration corresponding to the
        // namespace alias. Keep that honest external edge independently of
        // the exact declaration targets available for its named members.
        for name in ["scheduler", "api", "vendor"] {
            let binding = (0..facts.len()).find(|&index| facts.names[index] == name.as_bytes()
                && facts.kinds[index] == EntityKind::Reexport).expect("written namespace import");
            assert!((0..facts.occurrence_len).any(|index|
                facts.occurrence_owners[index] == binding as u32
                    && facts.occurrences[index].kind == ReferenceKind::Import
                    && facts.occurrences[index].confidence == OccurrenceConfidence::Import
                    && facts.occurrence_source_coordinate_slot[index].is_none()),
                "namespace binding keeps its module operand: {name}");
        }
        Ok(())
    }

    #[test]
    fn native_tsz_joins_named_import_call_to_exact_function_declaration() -> Result<(), LaneError> {
        let scheduler = "export interface ScheduleRequest { courses: string[]; }\nexport function generateSchedule(request: ScheduleRequest): string[] {\n  return request.courses;\n}\n";
        let caller = "import { generateSchedule, type ScheduleRequest } from '@/lib/scheduler';\nexport function POST() { const request: ScheduleRequest = { courses: ['CS101'] }; return generateSchedule(request); }\n";
        let resolutions = [module_resolution(
            "src/app/api/route.ts",
            "@/lib/scheduler",
            TszProjectModuleRequestKind::EsmImport,
            TszProjectModuleResolutionTarget::File {
                path: "src/lib/scheduler.ts".to_owned(),
            },
        )];
        let authority = native_tsz_project_with_resolutions(
            &[
                ("src/lib/scheduler.ts", scheduler),
                ("src/app/api/route.ts", caller),
            ],
            &resolutions,
        )?;
        let project = authority
            .project()
            .ok_or(LaneError::Missing("named function alias TSZ project"))?;
        assert_exact_module_resolution_metadata(project, &resolutions);
        let caller_facts = collect_native_tsz_file(project, "src/app/api/route.ts", caller)?;
        let targets = staged_tsz_coordinates(&caller_facts)?;
        let declaration_start = u32::try_from(
            scheduler
                .find("function generateSchedule")
                .ok_or(LaneError::Missing("named function declaration coordinate"))?,
        )?;
        let declaration_name = u32::try_from(
            scheduler
                .find("generateSchedule(request")
                .ok_or(LaneError::Missing("named function declaration name"))?,
        )?;
        let declaration_end = u32::try_from(
            scheduler
                .find("\n}\n")
                .ok_or(LaneError::Missing("named function declaration end"))?
                + 2,
        )?;
        assert!(
            targets.iter().any(|(kind, path, start, end, name, owner)| {
                *kind == ReferenceKind::FunctionCall
                    && path == "src/lib/scheduler.ts"
                    && *start == declaration_start
                    && *end == declaration_end
                    && *name == declaration_name
                    && *owner < caller_facts.len() as u32
            }),
            "named aliased call must point to the exact dependency declaration: {targets:?}"
        );
        Ok(())
    }

    #[test]
    fn native_tsz_javascript_rest_optionality_uses_one_variadic_marker() -> Result<(), LaneError> {
        use backend_semantic::ir::{ConcreteType, FunctionVariadicForm, TupleElementKind};
        for (path, source, first) in [
            (
                "api.js",
                "export const expect = (response, name = 'status', ...statuses) => statuses;",
                TupleElementKind::Optional,
            ),
            (
                "api.ts",
                "export const expect = (response: number, name?: string, ...statuses: number[]) => statuses;",
                TupleElementKind::Required,
            ),
        ] {
            let ir = owned_tsz_ir_at_path(source, path)?;
            let (
                _,
                TypeExpr::Concrete(ConcreteType::Function {
                    parameters,
                    variadic,
                    ..
                }),
            ) = native_observation(&ir, b"expect")
                .ok_or(LaneError::Missing("native callable observation"))?
            else {
                return Err(LaneError::Missing("concrete native callable"));
            };
            assert_eq!(variadic, FunctionVariadicForm::TypedLast, "{path}");
            let parameters = ir
                .tuple_elements(parameters)
                .ok_or(LaneError::Missing("native parameter tuple"))?;
            assert_eq!(
                parameters
                    .iter()
                    .map(|parameter| parameter.kind)
                    .collect::<Vec<_>>(),
                vec![first, TupleElementKind::Optional, TupleElementKind::Rest],
                "fixed optionality and the exact rest slot must both survive: {path}",
            );
        }
        Ok(())
    }

    #[test]
    fn native_tsz_lowers_whole_docs_javascript_api_with_rest_parameter() -> Result<(), LaneError> {
        use sha2::{Digest, Sha256};
        let source = include_str!("../../../tests/fixtures/typescript/docs-6e5dee38/api.js");
        assert_eq!(source.len(), 3_455);
        assert_eq!(
            format!("{:x}", Sha256::digest(source.as_bytes())),
            "483ea3f2947274476301bc627633c3f26b20c4bb2b708837af4661980cd62488"
        );
        let ir = owned_tsz_ir_at_path(source, "src/loadtest/k6/lib/api.js")?;
        for name in [
            b"expect".as_slice(),
            b"postMultipart",
            b"params",
            b"headers",
        ] {
            assert!(
                ir.items().any(|item| item.name() == name),
                "whole source keeps {name:?}"
            );
        }
        let (
            _,
            TypeExpr::Concrete(backend_semantic::ir::ConcreteType::Function {
                parameters,
                variadic,
                ..
            }),
        ) = native_observation(&ir, b"expect")
            .ok_or(LaneError::Missing("whole-source expect observation"))?
        else {
            return Err(LaneError::Missing("whole-source expect function"));
        };
        assert_eq!(
            variadic,
            backend_semantic::ir::FunctionVariadicForm::TypedLast
        );
        let parameters = ir
            .tuple_elements(parameters)
            .ok_or(LaneError::Missing("whole-source expect parameters"))?;
        assert_eq!(parameters.len(), 3);
        assert_eq!(
            parameters[2].kind,
            backend_semantic::ir::TupleElementKind::Rest
        );
        Ok(())
    }

    #[test]
    fn native_tsz_object_methods_own_repeated_inline_parameter_fields() -> Result<(), LaneError> {
        let source = "const handlers = [
  { drop({ active }: { active: { id: number } }) { return active.id; } },
  { drop({ active }: { active: { id: number } }) { return active.id + 1; } },
];
class Named { run({ active }: { active: { id: number } }) { return active.id; } }
";
        let ir = owned_tsz_ir(source)?;
        let methods = anonymous_callable_rows(&ir);
        assert_eq!(
            methods.len(),
            2,
            "only real object methods gain these anchors"
        );
        assert_eq!(
            methods[0].1, methods[1].1,
            "same member role keeps its durable family"
        );
        assert_ne!(
            methods[0].0, methods[1].0,
            "separate source methods are separate instances"
        );
        assert_ne!(methods[0].2, methods[1].2);
        for method in &methods {
            assert_eq!(
                method.5,
                AnonymousCallableFamilyMultiplicity::Ambiguous { instance_count: 2 }
            );
            let active = ir
                .items()
                .filter(|item| {
                    item.kind() == EntityKind::Field
                        && item.name() == b"active"
                        && item.parent() == Some(method.0)
                })
                .collect::<Vec<_>>();
            assert_eq!(
                active.len(),
                1,
                "each method owns its own inline parameter type"
            );
            let id = ir
                .items()
                .filter(|item| item.name() == b"id" && item.parent() == Some(active[0].id()))
                .collect::<Vec<_>>();
            assert_eq!(
                id.len(),
                1,
                "nested field stays under its exact parameter type"
            );
        }
        assert_eq!(
            ir.items()
                .filter(|item| item.name() == b"run" && item.kind() == EntityKind::Function)
                .count(),
            1,
            "named class method remains on the existing signature path"
        );
        Ok(())
    }

    #[test]
    fn native_tsz_lowers_whole_docs_tsx_with_exact_object_method_owners() -> Result<(), LaneError> {
        use sha2::{Digest, Sha256};
        let source =
            include_str!("../../../tests/fixtures/typescript/docs-6e5dee38/DocGridContentList.tsx");
        assert_eq!(source.len(), 9_106);
        assert_eq!(
            format!("{:x}", Sha256::digest(source.as_bytes())),
            "d602cb5ac73b02a2da9ac8513b0271252c2de0efad0062e1b3eccf94d53e27b3"
        );
        let ir = owned_tsz_ir_at_path(
            source,
            "src/frontend/apps/impress/src/features/docs/docs-grid/components/DocGridContentList.tsx",
        )?;
        for name in [
            b"DraggableDocGridContentList".as_slice(),
            b"DraggableDocGridItem",
            b"DocGridContentList",
        ] {
            assert!(
                ir.items().any(|item| item.name() == name),
                "whole TSX source keeps {name:?}"
            );
        }
        let methods = anonymous_callable_rows(&ir);
        for name in ["onDragStart", "onDragOver", "onDragEnd", "onDragCancel"] {
            let needle = format!("{name}(");
            let method = smallest_callable_containing(&methods, source, &needle)?;
            assert!(
                source[method.3 as usize..method.4 as usize]
                    .trim_start()
                    .starts_with(&needle)
            );
            let active = ir
                .items()
                .filter(|item| {
                    item.kind() == EntityKind::Field
                        && item.name() == b"active"
                        && item.parent() == Some(method.0)
                })
                .collect::<Vec<_>>();
            assert_eq!(
                active.len(),
                1,
                "{name} retains its exact active field, not a shared same-name row"
            );
        }
        Ok(())
    }

    #[test]
    fn native_tsz_anonymous_classes_keep_native_owners_and_declared_roles() -> Result<(), LaneError>
    {
        use crate::driver::types::ParentageState;
        let source = "const mocks = { First: class { input: unknown; constructor(input: unknown) {} }, Second: class { input: unknown; constructor(input: unknown) {} }, Named: class Named { input: unknown; constructor(private http: unknown) {} } };";
        let authority = native_tsz_project_with_resolutions(&[("input.ts", source)], &[])?;
        let project = authority
            .project()
            .ok_or(LaneError::Missing("anonymous class project"))?;
        let facts = collect_native_tsz_file(project, "input.ts", source)?;
        let fields = (0..facts.len())
            .filter(|&index| {
                facts.kinds[index] == EntityKind::Field && facts.names[index] == b"input"
            })
            .collect::<Vec<_>>();
        assert_eq!(fields.len(), 3, "every declared field survives");
        let parent = |index| facts.provenance.parentage()[index];
        let (
            ParentageState::UnrepresentedAuthorityOwner { identity: first },
            ParentageState::UnrepresentedAuthorityOwner { identity: second },
        ) = (parent(fields[0]), parent(fields[1]))
        else {
            return Err(LaneError::Missing("exact native anonymous class owners"));
        };
        assert_ne!(
            first, second,
            "distinct bound native classes own distinct fields"
        );
        assert!(
            matches!(parent(fields[2]), ParentageState::Bound { .. }),
            "named class keeps its emitted owner"
        );
        let constructors = (0..facts.len())
            .filter(|&index| {
                facts.names[index] == b"constructor" && facts.kinds[index] == EntityKind::Function
            })
            .collect::<Vec<_>>();
        assert_eq!(constructors.len(), 3);
        for (field, constructor) in fields.iter().zip(&constructors) {
            assert_eq!(
                parent(*field),
                parent(*constructor),
                "field and constructor retain their actual class owner"
            );
        }
        let parameters = (0..facts.len())
            .filter(|&index| {
                facts.kinds[index] == EntityKind::Parameter && facts.names[index] == b"input"
            })
            .collect::<Vec<_>>();
        assert_eq!(parameters.len(), 2, "both constructor parameters survive");
        for index in parameters {
            let ParentageState::Bound { parent: owner } = parent(index) else {
                return Err(LaneError::Missing("constructor parameter owner"));
            };
            assert!(
                constructors
                    .iter()
                    .any(|&constructor| constructor == owner.raw as usize)
            );
        }
        let roles = (0..facts.len())
            .filter(|&index| facts.names[index] == b"http")
            .map(|index| facts.kinds[index])
            .collect::<Vec<_>>();
        assert_eq!(roles.len(), 2);
        assert!(roles.contains(&EntityKind::Field));
        assert!(roles.contains(&EntityKind::Parameter));

        // A fresh authority with another file may allocate different native
        // SymbolIds; source-qualified ownership must remain identical.
        let other = native_tsz_project_with_resolutions(
            &[
                ("a.ts", "export const unrelated = 1;"),
                ("input.ts", source),
            ],
            &[],
        )?;
        let repeated = collect_native_tsz_file(
            other
                .project()
                .ok_or(LaneError::Missing("second class project"))?,
            "input.ts",
            source,
        )?;
        for index in &fields {
            assert_eq!(parent(*index), repeated.provenance.parentage()[*index]);
        }
        let ir = owned_tsz_ir(source)?;
        assert_eq!(
            ir.items()
                .filter(|item| item.kind() == EntityKind::Field && item.name() == b"input")
                .count(),
            3
        );
        Ok(())
    }

    #[test]
    fn native_tsz_anonymous_class_families_survive_unrelated_same_file_edits()
    -> Result<(), LaneError> {
        let source = "const mocks = { First: class { input: unknown; constructor(input: unknown) {} }, Second: class { input: unknown; constructor(input: unknown) {} } };";
        let before = owned_tsz_ir(source)?;
        let edited =
            format!("const unrelated = 'changed';\n{source}\n// unrelated trailing prose\n");
        let after = owned_tsz_ir(&edited)?;
        for name in [b"input".as_slice(), b"constructor"] {
            let rows = |ir: &backend_semantic::ir::Ir| {
                ir.items()
                    .filter(|item| item.name() == name)
                    .map(|item| (item.kind(), item.version()))
                    .collect::<Vec<_>>()
            };
            let original = rows(&before);
            let current = rows(&after);
            assert!(!original.is_empty());
            assert_eq!(
                original, current,
                "unrelated source/site changes do not remint {name:?} families or structural variants"
            );
        }
        let structural_change =
            owned_tsz_ir(&source.replacen("input: unknown;", "input: number;", 1))?;
        let field = |ir: &backend_semantic::ir::Ir| {
            ir.items()
                .find(|item| item.kind() == EntityKind::Field && item.name() == b"input")
                .map(|item| item.version())
                .ok_or(LaneError::Missing("anonymous class field version"))
        };
        assert_eq!(field(&before)?.family, field(&structural_change)?.family);
        assert_ne!(field(&before)?.variant, field(&structural_change)?.variant);

        let siblings = owned_tsz_ir(
            "const siblings = [class { value: unknown; }, class { value: unknown; }];",
        )?;
        let fields = siblings
            .items()
            .filter(|item| item.kind() == EntityKind::Field && item.name() == b"value")
            .map(|item| item.version())
            .collect::<Vec<_>>();
        assert_eq!(fields.len(), 2, "same-route source classes both survive");
        assert_eq!(
            fields[0].family, fields[1].family,
            "ambiguous siblings share their honest lexical family"
        );
        assert_ne!(
            fields[0].variant, fields[1].variant,
            "native sibling instances stay distinct"
        );

        let holder = "class Holder { left = class { value: unknown; constructor(value: unknown) {} }; right = class { value: unknown; constructor(value: unknown) {} }; first() { return class { value: unknown; constructor(value: unknown) {} }; } second() { return class { value: unknown; constructor(value: unknown) {} }; } }";
        let original = owned_tsz_ir(holder)?;
        let inserted = owned_tsz_ir(&holder.replacen(
            "class Holder {",
            "class Holder { unrelated = class { value: unknown; constructor(value: unknown) {} };",
            1,
        ))?;
        let values = |ir: &backend_semantic::ir::Ir| {
            ir.items()
                .filter(|item| {
                    (item.kind() == EntityKind::Field && item.name() == b"value")
                        || (item.kind() == EntityKind::Function && item.name() == b"constructor")
                })
                .map(|item| item.version())
                .collect::<Vec<_>>()
        };
        let original = values(&original);
        let inserted = values(&inserted);
        assert_eq!(original.len(), 8);
        assert_eq!(inserted.len(), 10);
        for version in original {
            assert!(
                inserted.contains(&version),
                "inserting a class in another named property must not renumber existing property/method classes"
            );
        }
        Ok(())
    }

    #[test]
    fn native_tsz_anonymous_parameter_properties_retain_both_exact_roles() -> Result<(), LaneError>
    {
        use crate::driver::types::ParentageState;
        let source = "const Service = class { constructor(private readonly http: string) {} }; class Named { constructor(public named: number) {} }";
        let authority = native_tsz_project_with_resolutions(&[("input.ts", source)], &[])?;
        let facts = collect_native_tsz_file(
            authority
                .project()
                .ok_or(LaneError::Missing("parameter-property project"))?,
            "input.ts",
            source,
        )?;
        let find = |name: &[u8], kind| {
            (0..facts.len())
                .find(|&index| facts.names[index] == name && facts.kinds[index] == kind)
                .ok_or(LaneError::Missing("parameter-property role"))
        };
        let field = find(b"http", EntityKind::Field)?;
        let parameter = find(b"http", EntityKind::Parameter)?;
        assert_eq!(
            (0..facts.len())
                .filter(|&index| facts.names[index] == b"http")
                .count(),
            2
        );
        let ParentageState::UnrepresentedAuthorityOwner { identity } =
            facts.provenance.parentage()[field]
        else {
            return Err(LaneError::Missing("anonymous field class owner"));
        };
        let ParentageState::Bound {
            parent: constructor,
        } = facts.provenance.parentage()[parameter]
        else {
            return Err(LaneError::Missing("parameter constructor owner"));
        };
        assert_eq!(facts.names[constructor.raw as usize], b"constructor");
        assert_eq!(facts.kinds[constructor.raw as usize], EntityKind::Function);
        let children = facts.child_starts[constructor.raw as usize] as usize;
        assert_eq!(facts.child_roles[children], ProductChildRole::FunctionParameter);
        assert_eq!(facts.child_targets[children], parameter as u32);
        assert_ne!(constructor.raw as usize, field);
        assert_eq!(
            facts.provenance.parentage()[constructor.raw as usize],
            ParentageState::UnrepresentedAuthorityOwner { identity }
        );
        assert_eq!(facts.type_records[field], facts.type_records[parameter]);
        let field_span = facts.provenance.source_spans()[field]
            .ok_or(LaneError::Missing("field parameter syntax span"))?;
        let parameter_span = facts.provenance.source_spans()[parameter]
            .ok_or(LaneError::Missing("parameter identifier span"))?;
        assert!(
            source[field_span.start as usize..field_span.end as usize]
                .contains("private readonly http: string")
        );
        assert_eq!(
            &source[parameter_span.start as usize..parameter_span.end as usize],
            "http"
        );
        let named_field = find(b"named", EntityKind::Field)?;
        let named_parameter = find(b"named", EntityKind::Parameter)?;
        let ParentageState::Bound {
            parent: named_class,
        } = facts.provenance.parentage()[named_field]
        else {
            return Err(LaneError::Missing("named property class owner"));
        };
        assert_eq!(facts.names[named_class.raw as usize], b"Named");
        assert_ne!(
            facts.provenance.parentage()[named_field],
            facts.provenance.parentage()[named_parameter]
        );
        assert_eq!(
            (0..facts.len())
                .filter(|&index| facts.kinds[index] == EntityKind::Record)
                .count(),
            1,
            "only the real named class has a Record"
        );
        let ir = owned_tsz_ir(source)?;
        assert_eq!(ir.items().filter(|item| item.name() == b"http").count(), 2);
        Ok(())
    }

    #[test]
    fn native_tsz_nested_anonymous_parameter_fields_do_not_borrow_outer_record()
    -> Result<(), LaneError> {
        use crate::driver::types::ParentageState;
        let source = "class Outer { http: unknown; Service = class { constructor(private http: unknown) {} read() { return this.http; } }; make() { return class { constructor(readonly http: unknown) {} read() { return this.http; } }; } }";
        let authority = native_tsz_project_with_resolutions(&[("input.ts", source)], &[])?;
        let facts = collect_native_tsz_file(
            authority
                .project()
                .ok_or(LaneError::Missing("nested parameter-property project"))?,
            "input.ts",
            source,
        )?;
        let fields = (0..facts.len())
            .filter(|&index| {
                facts.names[index] == b"http" && facts.kinds[index] == EntityKind::Field
            })
            .collect::<Vec<_>>();
        assert_eq!(
            fields.len(),
            3,
            "outer field plus both anonymous parameter-property fields"
        );
        let outer = fields
            .iter()
            .copied()
            .find(|&index| {
                matches!(
                    facts.provenance.parentage()[index],
                    ParentageState::Bound { .. }
                )
            })
            .ok_or(LaneError::Missing("real outer field"))?;
        let anonymous = fields
            .iter()
            .copied()
            .filter_map(|index| match facts.provenance.parentage()[index] {
                ParentageState::UnrepresentedAuthorityOwner { identity } => Some(identity),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(anonymous.len(), 2);
        assert_ne!(
            anonymous[0], anonymous[1],
            "property and method retain different native owner routes"
        );
        let parameters = (0..facts.len())
            .filter(|&index| {
                facts.names[index] == b"http" && facts.kinds[index] == EntityKind::Parameter
            })
            .collect::<Vec<_>>();
        assert_eq!(parameters.len(), 2);
        for parameter in parameters {
            let ParentageState::Bound { parent } = facts.provenance.parentage()[parameter] else {
                return Err(LaneError::Missing("nested constructor parameter parent"));
            };
            assert_eq!(facts.names[parent.raw as usize], b"constructor");
            assert_eq!(facts.kinds[parent.raw as usize], EntityKind::Function);
            let children = facts.child_starts[parent.raw as usize] as usize;
            assert_eq!(facts.child_roles[children], ProductChildRole::FunctionParameter);
            assert_eq!(facts.child_targets[children], parameter as u32);
        }
        assert!(
            !(0..facts.occurrence_len).any(|index| facts.occurrences[index].target
                == OccurrenceTarget::Local(EntityId::new(outer as u32))),
            "inner this.http must not resolve to the outer named class field"
        );
        owned_tsz_ir(source)?;
        Ok(())
    }

    #[test]
    fn native_tsz_anonymous_parameter_property_lineage_and_siblings_remain_distinct()
    -> Result<(), LaneError> {
        let source = "const Services = [class { constructor(private http: string) {} }, class { constructor(readonly http: string) {} }];";
        let before = owned_tsz_ir(source)?;
        let after = owned_tsz_ir(&format!(
            "const unrelated = 1;\n{source}\n// changed unrelated prose\n"
        ))?;
        let rows = |ir: &backend_semantic::ir::Ir| {
            ir.items()
                .filter(|item| item.name() == b"http")
                .map(|item| (item.kind(), item.version()))
                .collect::<Vec<_>>()
        };
        let original = rows(&before);
        assert_eq!(
            original.len(),
            4,
            "both roles in both native anonymous classes"
        );
        assert_eq!(
            original,
            rows(&after),
            "unrelated same-file edits preserve all role families/variants"
        );
        let fields = original
            .iter()
            .filter(|(kind, _)| *kind == EntityKind::Field)
            .map(|(_, version)| version)
            .collect::<Vec<_>>();
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].family, fields[1].family);
        assert_ne!(fields[0].variant, fields[1].variant);
        let changed = owned_tsz_ir(&source.replacen("http: string", "http: number", 1))?;
        let changed_fields = rows(&changed)
            .into_iter()
            .filter(|(kind, _)| *kind == EntityKind::Field)
            .map(|(_, version)| version)
            .collect::<Vec<_>>();
        assert_eq!(fields[0].family, changed_fields[0].family);
        assert_ne!(fields[0].variant, changed_fields[0].variant);
        Ok(())
    }

    #[test]
    fn native_tsz_lowers_whole_docs_migration_with_anonymous_class_members() -> Result<(), LaneError>
    {
        use sha2::{Digest, Sha256};
        let source =
            include_str!("../../../tests/fixtures/typescript/docs-6e5dee38/migration.spec.ts");
        assert_eq!(source.len(), 21_322);
        assert_eq!(
            format!("{:x}", Sha256::digest(source.as_bytes())),
            "d6ea04f08213179eab0e4b1eb6c208c55c10e7b7f193a8e08be76c2177d7d532"
        );
        let path = "src/yhub-server/__tests__/migration.spec.ts";
        let authority = native_tsz_project_with_resolutions(&[(path, source)], &[])?;
        let project = authority
            .project()
            .ok_or(LaneError::Missing("Docs migration project"))?;
        let facts = collect_native_tsz_file(project, path, source)?;
        let profile = LanguageProfile::TypeScript(TypeScriptSource::TypeScript);
        let scope = crate::driver::types::DeclarationScope::fixture();
        let versions = super::super::identity::versions(&facts, scope, profile)?;
        let mut duplicates = Vec::new();
        for left in 0..versions.len() {
            for right in left + 1..versions.len() {
                if versions[left].family == versions[right].family
                    && versions[left].variant == versions[right].variant
                {
                    duplicates.push(format!(
                        "{left} {:?} {:?} {:?} {:?} == {right} {:?} {:?} {:?} {:?}",
                        facts.kinds[left],
                        String::from_utf8_lossy(facts.names[left]),
                        facts.provenance.parentage()[left],
                        facts.provenance.source_spans()[left],
                        facts.kinds[right],
                        String::from_utf8_lossy(facts.names[right]),
                        facts.provenance.parentage()[right],
                        facts.provenance.source_spans()[right],
                    ));
                }
            }
        }
        assert!(
            duplicates.is_empty(),
            "whole Docs declarations collide: {duplicates:#?}"
        );
        let ir = owned_tsz_ir_at_path(source, path)?;
        for name in [b"input".as_slice(), b"kind"] {
            assert_eq!(
                ir.items()
                    .filter(|item| item.kind() == EntityKind::Field && item.name() == name)
                    .count(),
                2,
                "both real anonymous class fields survive: {name:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn fact_name_start_index_matches_first_current_ordinal_for_every_bounded_view() {
        let starts = [super::UNSET, 17, 5, 17, 0, super::UNSET, 91];
        for fact_len in [0, 1, 3, 4, 7, 10] {
            let index = super::index_name_starts(&starts, fact_len);
            for start in [super::UNSET, 0, 5, 17, 91, 999] {
                let previous_scan = (0..fact_len).find(|&ordinal|
                    starts.get(ordinal as usize) == Some(&start));
                assert_eq!(index.get(&start).copied(), previous_scan,
                    "first ordinal for start={start}, fact_len={fact_len}");
            }
            assert!(index.len() <= starts.len().min(fact_len as usize));
        }
        let index = super::index_name_starts(&starts, 7);
        assert_eq!(index.get(&17), Some(&1), "duplicate start keeps first fact");
        assert_eq!(index.get(&super::UNSET), Some(&0), "UNSET is not silently reinterpreted");
        assert!(!super::index_name_starts(&starts, 3).contains_key(&91),
            "later side-lane data cannot enter an earlier view");

        // A primary declaration adoption may replace a historical name start.
        // Rebuild from the current immutable coordinates, not an earlier map.
        let old = [12, 40];
        let current = [31, 40];
        assert_eq!(super::index_name_starts(&old, 2).get(&12), Some(&0));
        let current_index = super::index_name_starts(&current, 2);
        assert_eq!(current_index.get(&12), None);
        assert_eq!(current_index.get(&31), Some(&0));
    }

    #[test]
    fn native_tsz_indexed_name_joins_keep_each_authored_observed_type()
    -> Result<(), LaneError> {
        use backend_semantic::ir::{BuiltinType, ConcreteType};
        let mut source = String::from("// exact UTF-8 name offsets: café\n");
        for ordinal in 0..64 {
            let declaration = if ordinal % 2 == 0 {
                format!("export const entry_{ordinal}: number = {ordinal};\n")
            } else {
                format!("export const entry_{ordinal}: string = 'value';\n")
            };
            source.push_str(&declaration);
        }
        let ir = owned_tsz_ir(&source)?;
        for ordinal in 0..64 {
            let name = format!("entry_{ordinal}");
            let matching: Vec<_> = ir.items().filter(|item| item.name() == name.as_bytes()).collect();
            assert_eq!(matching.len(), 1, "one exact authored declaration for {name}");
            let extension = ir.typescript_extension(matching[0].id())
                .ok_or(LaneError::Missing("native observed extension"))?;
            let observed = extension.observed.ok_or(LaneError::Missing("native observed type"))?;
            let expected = if ordinal % 2 == 0 { BuiltinType::F64 } else { BuiltinType::String };
            assert_eq!(ir.ty(observed), Some(TypeExpr::Concrete(ConcreteType::Builtin(expected))),
                "the index must not attach another declaration's native type to {name}");
        }
        // A fresh program with shifted exact source offsets must obtain the
        // same owner/type joins without retaining the previous index.
        let shifted = format!("// another source revision\n{source}");
        let other = owned_tsz_ir(&shifted)?;
        for ordinal in 0..64 {
            let name = format!("entry_{ordinal}");
            let item = other.items().find(|item| item.name() == name.as_bytes())
                .ok_or(LaneError::Missing("shifted authored declaration"))?;
            let observed = other.typescript_extension(item.id())
                .and_then(|extension| extension.observed)
                .ok_or(LaneError::Missing("shifted native observed type"))?;
            let expected = if ordinal % 2 == 0 { BuiltinType::F64 } else { BuiltinType::String };
            assert_eq!(other.ty(observed), Some(TypeExpr::Concrete(ConcreteType::Builtin(expected))));
        }
        Ok(())
    }

    #[test]
    fn native_tsz_checker_populates_existing_observed_type_lane() -> Result<(), LaneError> {
        // The multibyte prefix exercises the TSZ-to-OXC name-span join against
        // the original UTF-8 source bytes, without UTF-16 or lossy conversion.
        let source = "// 🧭\nexport const answer = 42;";
        let ir = owned_tsz_ir(source)?;
        let answer = ir
            .items()
            .find(|item| item.name() == b"answer")
            .ok_or(LaneError::Missing("answer declaration"))?;
        let extension = ir
            .typescript_extension(answer.id())
            .ok_or(LaneError::Missing("TypeScript extension for answer"))?;
        let observed = extension
            .observed
            .ok_or(LaneError::Missing("native TSZ observed type"))?;
        if extension.declared == Some(observed) {
            return Err(LaneError::Missing(
                "checker-derived type distinct from the declared owner coordinate",
            ));
        }
        let observed_type = ir
            .ty(observed)
            .ok_or(LaneError::Missing("observed type row in the shared IR"))?;
        if matches!(observed_type, backend_semantic::ir::TypeExpr::Unknown(_)) {
            return Err(LaneError::Missing(
                "supported native number type instead of an explicit oracle gap",
            ));
        }
        Ok(())
    }

    #[test]
    fn native_tsz_owner_source_index_keeps_inherited_and_overridden_owners_distinct()
    -> Result<(), LaneError> {
        let authority = native_tsz_project_with_resolutions(
            &[(
                "input.ts",
                concat!(
                    r"export declare class Base { inherited: string; v\u0061lue: number; } ",
                    "export declare class Child extends Base { value: number; café: string; } ",
                    "export declare const settings: Child;",
                ),
            )],
            &[],
        )?;
        let project = authority
            .project()
            .ok_or(LaneError::Missing("inheritance project"))?;
        project.with_file_checker_and_types(0, |checker, binder, file, database| -> Result<(), LaneError> {
            let symbol = binder.file_locals.get("settings").ok_or(LaneError::Missing("bound inherited instance"))?;
            let ty = checker.get_type_of_symbol(symbol);
            let ty = checker.resolve_lazy_type(ty);
            let shape_id = match database.lookup(ty) {
                Some(super::TypeData::Object(shape)) | Some(super::TypeData::ObjectWithIndex(shape)) => shape,
                _ => return Err(LaneError::Missing("actual inherited class shape")),
            };
            let shape = database.object_shape(shape_id);
            assert!(shape.symbol.is_some(), "source-index test preserves actual nominal shape");
            let bound_owner = |expected: &str| file.arena.nodes.iter().enumerate().find_map(|(raw, node)| {
                let class = file.arena.get_class(node)?;
                let name = file.arena.get_identifier(file.arena.get(class.name)?)?;
                (name.escaped_text == expected).then(|| file.node_symbols.get(&u32::try_from(raw).ok()?).copied()).flatten()
            }).expect("actual declaring class binding");
            let base = bound_owner("Base");
            let child = bound_owner("Child");
            assert_ne!(base, child);
            let names = super::native_tsz_owner_name_sources(Some(project), database, &shape)?
                .ok_or(LaneError::Missing("complete inherited owner index"))?;
            assert_eq!(names.len(), 2, "each captured declaring owner has a separate borrowed index");
            assert_eq!(shape.properties.len(), 3);
            for property in &shape.properties {
                let name = database.resolve_atom_ref(property.name);
                let (owner, raw) = match name.as_ref() {
                    "inherited" => (base, "inherited"),
                    "value" => (child, "value"),
                    "café" => (child, "café"),
                    _ => return Err(LaneError::Missing("actual inherited property")),
                };
                assert_eq!(property.parent_id, Some(owner));
                assert_eq!(names.get(&owner).unwrap().get(name.as_ref()), Some(&Some(raw)));
            }
            assert_eq!(names.get(&base).unwrap().get("value"), Some(&Some(r"v\u0061lue")),
                "an overridden same-spelled base property cannot lend its different raw token to the child's property");
            assert!(!names.get(&base).unwrap().contains_key("café"));
            assert!(!names.get(&child).unwrap().contains_key("inherited"));
            Ok(())
        }).map_err(|_| LaneError::Missing("checked inherited owner query"))?
    }

    #[test]
    fn native_tsz_owner_source_index_charges_one_scan_and_latches_a_shared_stop()
    -> Result<(), LaneError> {
        use backend_frontend_typescript::{
            TszExecutionCheckpoint, TszProjectExecutionBudget, TszProjectExecutionStop,
            TszProjectQuerySessionError,
        };
        use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
        use std::time::{Duration, Instant};
        struct ScanCheckpoint<'a> {
            budget: TszProjectExecutionBudget<'a>,
            record: AtomicBool,
            charged: AtomicU64,
            allowance: AtomicU64,
        }
        impl TszExecutionCheckpoint for ScanCheckpoint<'_> {
            fn checkpoint(&self, work: u64) -> Result<(), TszProjectExecutionStop> {
                self.budget.checkpoint(work)?;
                if self.record.load(Ordering::Acquire) {
                    self.charged.fetch_add(work, Ordering::AcqRel);
                    self.allowance
                        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |remaining| {
                            remaining.checked_sub(work)
                        })
                        .map_err(|_| TszProjectExecutionStop::WorkBudgetExhausted)?;
                }
                Ok(())
            }
        }
        let cancelled = AtomicBool::new(false);
        let checkpoint = ScanCheckpoint {
            budget: TszProjectExecutionBudget::new(
                Instant::now() + Duration::from_secs(30),
                &cancelled,
                1_000_000,
            ),
            record: AtomicBool::new(false),
            charged: AtomicU64::new(0),
            allowance: AtomicU64::new(u64::MAX),
        };
        let source = r"export declare class Owner { café: string; v\u0061lue: number; } export declare const settings: Owner;";
        let mut options = TszProjectOptions {
            checker: TszCheckerOptions::default(),
            semantic_options: TszProjectSemanticOptions::declaration_scoped(),
            program_diagnostics: Vec::new(),
            module_resolutions: Vec::new(),
            file_formats: Vec::new(),
            environment: TszEnvironmentFingerprint::from_sha256([0x79; 32]),
        };
        options.checker.no_lib = true;
        let mut authority = TszProjectAuthority::new();
        authority
            .update_with_execution_checkpoint(
                vec![TszFileInput {
                    path: "input.ts".to_owned(),
                    source: source.to_owned(),
                }],
                options,
                &[],
                &checkpoint,
            )
            .map_err(|_| LaneError::Missing("metered owner project"))?;
        let project = authority
            .project()
            .ok_or(LaneError::Missing("metered owner project result"))?;
        let session = project
            .checked_query_session(&checkpoint)
            .map_err(|_| LaneError::Missing("shared metered query session"))?;
        let result = session.with_file_checker_and_types(0, |checker, binder, _file, database| -> Result<(), LaneError> {
            let symbol = binder.file_locals.get("settings").ok_or(LaneError::Missing("real class-typed declaration"))?;
            let ty = checker.get_type_of_symbol(symbol);
            let ty = checker.resolve_lazy_type(ty);
            let shape_id = match database.lookup(ty) {
                Some(super::TypeData::Object(shape)) | Some(super::TypeData::ObjectWithIndex(shape)) => shape,
                _ => return Err(LaneError::Missing("real class shape")),
            };
            let shape = database.object_shape(shape_id);
            assert!(shape.symbol.is_some(), "the actual nominal shape stays unchanged; this law tests only its source-index primitive");
            assert_eq!(shape.properties.len(), 2);
            checkpoint.record.store(true, Ordering::Release);
            let complete = super::native_tsz_owner_name_sources(Some(project), database, &shape)?
                .ok_or(LaneError::Missing("complete source index"))?;
            checkpoint.record.store(false, Ordering::Release);
            let tokens = complete.values().flat_map(|names| names.iter()).map(|(name, raw)| {
                name.len() + raw.expect("unambiguous token").len()
            }).sum::<usize>();
            assert_eq!(checkpoint.charged.load(Ordering::Acquire), 2 + 1 + 2 + tokens as u64,
                "two properties scan their one exact owner once and charge each token byte");
            checkpoint.allowance.store(15, Ordering::Release);
            checkpoint.record.store(true, Ordering::Release);
            let stopped = super::native_tsz_owner_name_sources(Some(project), database, &shape);
            assert!(matches!(stopped, Err(TypeScriptCollectError::TszAuthority(
                backend_frontend_typescript::TszAuthorityError::ExecutionStopped(TszProjectExecutionStop::WorkBudgetExhausted)))),
                "a stopped index returns the exact typed terminal, never a prefix or OracleGap");
            assert_eq!(database.execution_checkpoint(0), Err(TszProjectExecutionStop::WorkBudgetExhausted),
                "the same native QueryCache latches the source scan's stop");
            checkpoint.record.store(false, Ordering::Release);
            Ok(())
        });
        assert!(
            matches!(
                result,
                Err(TszProjectQuerySessionError::ExecutionStopped(
                    TszProjectExecutionStop::WorkBudgetExhausted
                ))
            ),
            "the owning query-session boundary refuses the stopped callback output"
        );
        Ok(())
    }

    #[test]
    fn native_tsz_property_parent_is_the_exact_cross_file_class_owner() -> Result<(), LaneError> {
        let authority = native_tsz_project_with_resolutions(
            &[
                (
                    "types.d.ts",
                    r"export declare class Owner { café: string; v\u0061lue: number; }",
                ),
                (
                    "foreign.d.ts",
                    r"export declare class Owner { c\u0061fé: string; foreignOnly: number; }",
                ),
                (
                    "src/main.ts",
                    "import type { Owner } from '../types'; export declare const settings: Owner;",
                ),
            ],
            &[module_resolution(
                "src/main.ts",
                "../types",
                TszProjectModuleRequestKind::EsmImport,
                TszProjectModuleResolutionTarget::File {
                    path: "types.d.ts".to_owned(),
                },
            )],
        )?;
        let project = authority
            .project()
            .ok_or(LaneError::Missing("owner project"))?;
        let importer = project
            .program()
            .files
            .iter()
            .position(|file| file.file_name == "src/main.ts")
            .ok_or(LaneError::Missing("exact importer"))?;
        project.with_file_checker_and_types(importer, |checker, binder, _file, database| {
            let symbol = binder.file_locals.get("settings").ok_or(LaneError::Missing("bound settings"))?;
            let ty = checker.get_type_of_symbol(symbol);
            let ty = checker.resolve_lazy_type(ty);
            let shape_id = match database.lookup(ty) {
                Some(super::TypeData::Object(shape)) | Some(super::TypeData::ObjectWithIndex(shape)) => shape,
                _ => return Err(LaneError::Missing("actual checker class instance shape")),
            };
            let shape = database.object_shape(shape_id);
            let owner = shape.symbol.ok_or(LaneError::Missing("preserved nominal class identity"))?;
            let declaration_file = project.program().files.iter().find(|file| file.file_name == "types.d.ts")
                .ok_or(LaneError::Missing("owner declaration file"))?;
            let owner_data = project.program().symbols.get(owner).ok_or(LaneError::Missing("captured owner symbol"))?;
            assert!(owner_data.declarations.iter().any(|node| declaration_file.node_symbols.get(&node.0) == Some(&owner)),
                "the nominal owner is bound to its actual dependency declaration");
            assert_eq!(shape.properties.len(), 2);
            let names = super::native_tsz_owner_name_sources(Some(project), database, &shape)?
                .ok_or(LaneError::Missing("complete owner index"))?;
            assert_eq!(names.len(), 1, "two real properties scan one captured owner");
            assert_eq!(names.get(&owner).unwrap().get("café"), Some(&Some("café")));
            assert_eq!(names.get(&owner).unwrap().get("value"), Some(&Some(r"v\u0061lue")));
            assert!(!names.get(&owner).unwrap().contains_key("foreignOnly"));
            for property in &shape.properties {
                assert_eq!(property.parent_id, Some(owner),
                    "the checker gives this engine consumer the class owner, not a property symbol");
                let name = database.resolve_atom_ref(property.name);
                let expected = match name.as_ref() {
                    "café" => "café",
                    "value" => r"v\u0061lue",
                    _ => return Err(LaneError::Missing("exact owner property names")),
                };
                assert_eq!(project.symbol_name_source(owner, &name), None,
                    "a member-bound spelling lookup cannot interpret this actual owner identity");
                assert_eq!(project.property_name_source_from_owner(owner, &name), Some(expected),
                    "the engine's owner-spelling boundary borrows the exact retained dependency token");
            }
            assert_eq!(project.property_name_source_from_owner(owner, "foreignOnly"), None);
            Ok(())
        }).map_err(|_| LaneError::Missing("checked owner query"))?
    }

    fn observed_template_parts<'ir>(
        ir: &'ir backend_semantic::ir::Ir,
        name: &[u8],
    ) -> Result<&'ir [backend_semantic::ir::TemplatePart], LaneError> {
        let (_, observed) = native_observation(ir, name)
            .ok_or(LaneError::Missing("template checker observation"))?;
        let TypeExpr::Computed(ComputedType::TemplateLiteral(parts)) = observed else {
            return Err(LaneError::Missing("computed template constructor"));
        };
        ir.template_parts(parts)
            .ok_or(LaneError::Missing("ordered template parts"))
    }

    #[test]
    fn native_tsz_template_children_are_committed_after_primitive_substitutions()
    -> Result<(), LaneError> {
        use backend_semantic::ir::{BuiltinType, ConcreteType, TemplatePart};
        let source = concat!(
            "export type Hook = `on${string}Changed`;\n",
            "export type Multi = `pre${number}mid${string}post`;\n",
        );
        let ir = owned_tsz_ir(source)?;
        let [
            TemplatePart::Bytes(prefix),
            TemplatePart::Placeholder(string),
            TemplatePart::Bytes(suffix),
        ] = observed_template_parts(&ir, b"Hook")?
        else {
            return Err(LaneError::Missing("text/string/text template ordering"));
        };
        assert_eq!(ir.atom(*prefix), Some(b"on".as_slice()));
        assert_eq!(ir.atom(*suffix), Some(b"Changed".as_slice()));
        assert_eq!(
            ir.ty(*string),
            Some(TypeExpr::Concrete(ConcreteType::Builtin(
                BuiltinType::String
            )))
        );
        let [
            TemplatePart::Bytes(pre),
            TemplatePart::Placeholder(number),
            TemplatePart::Bytes(mid),
            TemplatePart::Placeholder(second_string),
            TemplatePart::Bytes(post),
        ] = observed_template_parts(&ir, b"Multi")?
        else {
            return Err(LaneError::Missing(
                "two recursively lowered substitutions remain source-ordered",
            ));
        };
        assert_eq!(ir.atom(*pre), Some(b"pre".as_slice()));
        assert_eq!(ir.atom(*mid), Some(b"mid".as_slice()));
        assert_eq!(ir.atom(*post), Some(b"post".as_slice()));
        assert_eq!(ir.ty(*second_string), ir.ty(*string));
        assert_ne!(
            ir.ty(*number),
            ir.ty(*string),
            "primitive identity survives template staging"
        );
        Ok(())
    }

    #[test]
    fn native_tsz_nested_template_under_compound_substitution_keeps_its_constructor()
    -> Result<(), LaneError> {
        use backend_semantic::ir::{ConcreteType, TemplatePart};
        let source =
            "export type Nested = `pre${string & { readonly brand: `kind${number}Tail` }}post`;";
        let ir = owned_tsz_ir(source)?;
        let [
            TemplatePart::Bytes(pre),
            TemplatePart::Placeholder(compound),
            TemplatePart::Bytes(post),
        ] = observed_template_parts(&ir, b"Nested")?
        else {
            return Err(LaneError::Missing("outer template source ordering"));
        };
        assert_eq!(ir.atom(*pre), Some(b"pre".as_slice()));
        assert_eq!(ir.atom(*post), Some(b"post".as_slice()));
        assert!(
            matches!(
                ir.ty(*compound),
                Some(TypeExpr::Concrete(ConcreteType::Intersection(_)))
            ),
            "the compound placeholder cannot become its recursive string primitive: {:?}",
            ir.ty(*compound)
        );
        Ok(())
    }

    #[test]
    fn checker_template_children_preserve_nested_templates_and_order() -> Result<(), LaneError> {
        use backend_frontend_typescript::legacy::{Declaration, Origin, TemplatePart, TypeTree};
        use backend_semantic::ir::{BuiltinType, ConcreteType, TemplatePart as IrPart};
        let source = "export type Nested = `pre${`inner${string}tail`}post`;";
        let nested = TypeTree::TemplateLiteral {
            parts: vec![
                TemplatePart::Text {
                    text: "inner".to_owned(),
                },
                TemplatePart::Type {
                    r#type: Box::new(TypeTree::Primitive {
                        name: "string".to_owned(),
                    }),
                },
                TemplatePart::Text {
                    text: "tail".to_owned(),
                },
            ],
        };
        let mut checker = report(source, Vec::new());
        let start = u32::try_from(
            source
                .find("Nested")
                .ok_or(LaneError::Missing("alias name"))?,
        )?;
        checker.declarations = vec![Declaration {
            name_start: start,
            name_end: start + 6,
            origin: Origin::Computed,
            overload_index: None,
            r#type: Some(TypeTree::TemplateLiteral {
                parts: vec![
                    TemplatePart::Text {
                        text: "pre".to_owned(),
                    },
                    TemplatePart::Type {
                        r#type: Box::new(nested),
                    },
                    TemplatePart::Text {
                        text: "post".to_owned(),
                    },
                ],
            }),
        }]
        .into_boxed_slice();
        let ir = owned_ir(source, Some(&checker))?;
        let [
            IrPart::Bytes(pre),
            IrPart::Placeholder(inner),
            IrPart::Bytes(post),
        ] = observed_template_parts(&ir, b"Nested")?
        else {
            return Err(LaneError::Missing("outer template ordering"));
        };
        assert_eq!(ir.atom(*pre), Some(b"pre".as_slice()));
        assert_eq!(ir.atom(*post), Some(b"post".as_slice()));
        let Some(TypeExpr::Computed(ComputedType::TemplateLiteral(inner_parts))) = ir.ty(*inner)
        else {
            return Err(LaneError::Missing(
                "nested template keeps its compound constructor",
            ));
        };
        let Some(
            [
                IrPart::Bytes(prefix),
                IrPart::Placeholder(string),
                IrPart::Bytes(suffix),
            ],
        ) = ir.template_parts(inner_parts)
        else {
            return Err(LaneError::Missing("inner text/string/text ordering"));
        };
        assert_eq!(ir.atom(*prefix), Some(b"inner".as_slice()));
        assert_eq!(ir.atom(*suffix), Some(b"tail".as_slice()));
        assert_eq!(
            ir.ty(*string),
            Some(TypeExpr::Concrete(ConcreteType::Builtin(
                BuiltinType::String
            )))
        );
        assert_ne!(
            *inner, *string,
            "compound and primitive coordinates stay distinct"
        );
        Ok(())
    }

    #[test]
    fn native_tsz_merged_declarations_keep_exact_primary_and_part_occurrence_provenance()
    -> Result<(), LaneError> {
        let source = concat!(
            "declare namespace os {\n",
            "type Base = { value: number };\n",
            "interface File { first: Base; }\n",
            "const gap: Base;\n",
            "interface File extends Base { second: Base; }\n",
            "}\n",
            "declare namespace foreign { interface File { alien: string; } }\n",
        );
        let ir = owned_tsz_ir_at_path(source, "merged.d.ts")?;
        let os = ir
            .items()
            .find(|item| item.name() == b"os")
            .ok_or(LaneError::Missing("os namespace"))?;
        let foreign = ir
            .items()
            .find(|item| item.name() == b"foreign")
            .ok_or(LaneError::Missing("foreign namespace"))?;
        let file = ir
            .items()
            .find(|item| item.name() == b"File" && item.parent() == Some(os.id()))
            .ok_or(LaneError::Missing("merged os File"))?;
        let foreign_file = ir
            .items()
            .find(|item| item.name() == b"File" && item.parent() == Some(foreign.id()))
            .ok_or(LaneError::Missing("distinct foreign File"))?;
        assert_ne!(
            file.id(),
            foreign_file.id(),
            "equal names do not merge across lexical owners"
        );
        assert_eq!(ir.items().filter(|item| item.name() == b"File").count(), 2);
        let first_start = u32::try_from(source.find("interface File { first").unwrap())?;
        let second_start = source.find("interface File extends").unwrap();
        let basis = file
            .source()
            .ok_or(LaneError::Missing("finalized merge source basis"))?;
        let first_end =
            first_start + u32::try_from(source[first_start as usize..].find('}').unwrap())? + 1;
        assert_eq!((basis.start(), basis.end()), (first_start, first_end));
        assert!(basis.end() < u32::try_from(second_start)?);
        assert!(!source[basis.start() as usize..basis.end() as usize].contains("gap"));
        for name in [b"first".as_slice(), b"second".as_slice()] {
            let member = ir
                .items()
                .find(|item| item.name() == name)
                .ok_or(LaneError::Missing("merged File member"))?;
            assert_eq!(member.parent(), Some(file.id()));
        }
        let gap = ir
            .items()
            .find(|item| item.name() == b"gap")
            .ok_or(LaneError::Missing("gap declaration"))?;
        assert_eq!(
            gap.parent(),
            Some(os.id()),
            "family envelope gaps are not lexical scope"
        );
        let expected_start = u32::try_from(second_start + "interface File extends ".len())?;
        assert!(
            ir.link_occurrences_from(file.id()).any(|(_, site)| site
                .source
                .is_some_and(
                    |span| span.start() == expected_start && span.end() == expected_start + 4
                )),
            "later merge part keeps its exact extends Base occurrence"
        );
        assert!(
            ir.link_occurrences_from(file.id()).all(|(_, site)| site
                .source
                .is_none_or(|span| span.end()
                    <= u32::try_from(source.find("const gap").unwrap()).unwrap()
                    || span.start() >= u32::try_from(second_start).unwrap())),
            "no occurrence in the intervening gap is lent to File"
        );
        assert!(
            ir.entity(file.id())
                .is_some_and(|entity| entity.authority.members
                    == backend_semantic::ir::FactAvailability::Unavailable),
            "one merge part never claims the complete merged inventory"
        );
        Ok(())
    }

    #[test]
    fn native_tsz_merged_namespace_gap_callable_keeps_its_exact_scope() -> Result<(), LaneError> {
        let prefix = "namespace N { export interface A { first: number } }\n";
        let gap = "type Gap = (value: number) => number;\n";
        let control_source = format!("{prefix}{gap}");
        let merged_source =
            format!("{prefix}{gap}namespace N {{ export interface B {{ last: number }} }}\n");
        let control = owned_tsz_ir(&control_source)?;
        let merged = owned_tsz_ir(&merged_source)?;
        let control_rows = anonymous_callable_rows(&control);
        let merged_rows = anonymous_callable_rows(&merged);
        assert_eq!(control_rows.len(), 1);
        assert_eq!(merged_rows.len(), 1);
        assert_eq!(
            control_rows[0].1, merged_rows[0].1,
            "a later namespace merge cannot lend its scope to the gap callable"
        );
        let gap_owner = merged
            .items()
            .find(|item| item.name() == b"Gap")
            .ok_or(LaneError::Missing("gap type alias"))?;
        assert_eq!(gap_owner.parent(), None);
        let callable = merged
            .item(merged_rows[0].0)
            .ok_or(LaneError::Missing("gap callable"))?;
        assert_eq!(callable.parent(), Some(gap_owner.id()));
        Ok(())
    }

    #[test]
    fn native_tsz_merged_enum_refuses_a_same_name_variant_in_an_intervening_owner()
    -> Result<(), LaneError> {
        let source = concat!(
            "enum E { First = 1 }\n",
            "namespace Other { export enum Inner { OnlyGap = 2 } }\n",
            "enum E { Last = 3 }\n",
            "const missing = E.OnlyGap; const valid = E.Last;\n",
        );
        let ir = owned_tsz_ir(source)?;
        let target = ir
            .items()
            .find(|item| item.name() == b"Last")
            .ok_or(LaneError::Missing("later merged enum variant"))?;
        let valid_start = u32::try_from(source.rfind("Last").unwrap())?;
        assert!(
            ir.link_occurrences().any(|(_, site)| site
                .source
                .is_some_and(|span| span.start() == valid_start && span.end() == valid_start + 4,)
                && ir.link(site.link).is_some_and(|link| matches!(link.target,
            backend_semantic::ir::LinkTarget::Local(id) if id == target.id()))),
            "the later exact variant remains discoverable"
        );
        let gap = ir
            .items()
            .find(|item| item.name() == b"OnlyGap")
            .ok_or(LaneError::Missing("foreign gap enum variant"))?;
        let missing_start = u32::try_from(source.rfind("OnlyGap").unwrap())?;
        assert!(
            !ir.link_occurrences()
                .any(|(_, site)| site.source.is_some_and(
                    |span| span.start() == missing_start && span.end() == missing_start + 7,
                ) && ir.link(site.link).is_some_and(|link| matches!(link.target,
            backend_semantic::ir::LinkTarget::Local(id) if id == gap.id()))),
            "the family envelope cannot turn a foreign variant into E's member"
        );
        Ok(())
    }

    /// The real PocketBase declaration has a callable, input, and result
    /// carrier all named `stmt`, with the input and result typed as Stmt.
    /// Signature role, rather than a fabricated source site, separates them.
    #[test]
    fn native_tsz_same_named_input_and_result_keep_distinct_signature_roles()
    -> Result<(), LaneError> {
        use backend_semantic::ir::{
            SignatureCarrierBindingRole as Role, SignatureCarrierBindingsObservation,
        };
        for source in [
            "interface Stmt { value: string } interface Tx { stmt(stmt: Stmt): (Stmt) }",
            "declare function stmt(stmt: string): string;",
        ] {
            let ir = owned_tsz_ir_at_path(source, "same-name.d.ts")?;
            let owner = ir
                .items()
                .find(|item| item.kind() == EntityKind::Function && item.name() == b"stmt")
                .ok_or(LaneError::Missing("same-named callable"))?;
            let Some(SignatureCarrierBindingsObservation::Captured(bindings)) =
                ir.signature_carrier_bindings(owner.id())
            else {
                return Err(LaneError::Missing("same-named signature bindings"));
            };
            let bindings = bindings.collect::<Vec<_>>();
            assert_eq!(bindings.len(), 2);
            assert_eq!(bindings[0].role, Role::Parameter);
            assert_eq!(bindings[1].role, Role::Result);
            for binding in &bindings {
                assert_eq!(binding.owner, owner.id());
                assert_eq!(binding.position, 0);
            }
            let input = ir
                .item(bindings[0].carrier)
                .ok_or(LaneError::Missing("same-named authored input"))?;
            let result = ir
                .item(bindings[1].carrier)
                .ok_or(LaneError::Missing("same-named result carrier"))?;
            assert_ne!(input.id(), result.id());
            assert_ne!(input.version(), result.version());
            for item in [input, result] {
                assert_eq!(item.kind(), EntityKind::Parameter);
                assert_eq!(item.name(), b"stmt");
                assert_eq!(item.parent(), Some(owner.id()));
            }
            let ty = input
                .semantic_type()
                .ok_or(LaneError::Missing("input type"))?;
            assert_eq!(Some(ty), result.semantic_type());
            let expected = if let Some(stmt) = ir
                .items()
                .find(|item| item.kind() == EntityKind::Trait && item.name() == b"Stmt")
            {
                backend_semantic::ir::ConcreteType::Nominal(stmt.id())
            } else {
                backend_semantic::ir::ConcreteType::Builtin(
                    backend_semantic::ir::BuiltinType::String,
                )
            };
            assert_eq!(ir.ty(ty), Some(TypeExpr::Concrete(expected)));
            let owner_type = owner
                .semantic_type()
                .ok_or(LaneError::Missing("callable type"))?;
            let Some(TypeExpr::Concrete(backend_semantic::ir::ConcreteType::Function {
                parameters,
                results,
                ..
            })) = ir.ty(owner_type)
            else {
                return Err(LaneError::Missing("callable parameter/result type lanes"));
            };
            let parameters = ir
                .tuple_elements(parameters)
                .ok_or(LaneError::Missing("input tuple"))?;
            let results = ir
                .tuple_elements(results)
                .ok_or(LaneError::Missing("result tuple"))?;
            assert_eq!(parameters.len(), 1);
            assert_eq!(results.len(), 1);
            assert_eq!(parameters[0].ty, ty);
            assert_eq!(results[0].ty, ty);
            let start = source.find("stmt:").expect("written input") as u32;
            let span = input
                .source()
                .ok_or(LaneError::Missing("authored input span"))?;
            assert_eq!((span.start(), span.end()), (start, start + 4));
            assert!(result.source().is_none(), "result has no authored binding");
            assert_eq!(
                ir.items()
                    .filter(|item| item.kind() == EntityKind::Parameter
                        && item.parent() == Some(owner.id()))
                    .count(),
                2,
                "neither role is dropped or duplicated"
            );
        }
        Ok(())
    }

    #[test]
    fn native_tsz_result_role_identity_survives_unrelated_source_edits() -> Result<(), LaneError> {
        use backend_semantic::ir::SignatureCarrierBindingsObservation;
        let source = "interface Stmt { value: string } interface Tx { stmt(stmt: Stmt): (Stmt) }";
        let before = owned_tsz_ir_at_path(source, "same-name.d.ts")?;
        let edited = format!(
            "// shifted source\n{source}\ndeclare function unrelated(value: number): void;\n"
        );
        let after = owned_tsz_ir_at_path(&edited, "same-name.d.ts")?;
        let rows = |ir: &backend_semantic::ir::Ir| {
            let owner = ir
                .items()
                .find(|item| item.kind() == EntityKind::Function && item.name() == b"stmt")
                .ok_or(LaneError::Missing("stable same-named callable"))?;
            let Some(SignatureCarrierBindingsObservation::Captured(bindings)) =
                ir.signature_carrier_bindings(owner.id())
            else {
                return Err(LaneError::Missing("stable signature bindings"));
            };
            bindings
                .map(|binding| {
                    ir.item(binding.carrier)
                        .map(|item| (binding.role, binding.position, item.version()))
                        .ok_or(LaneError::Missing("stable signature carrier"))
                })
                .collect::<Result<Vec<_>, LaneError>>()
        };
        let original = rows(&before)?;
        assert_eq!(original.len(), 2);
        assert_eq!(
            original,
            rows(&after)?,
            "role identity excludes coordinates and insertion order"
        );
        Ok(())
    }

    #[test]
    fn native_tsz_lowers_whole_pocketbase_generated_declaration_file() -> Result<(), LaneError> {
        use sha2::{Digest, Sha256};
        let source =
            include_str!("../../../tests/fixtures/typescript/pocketbase-52a53b5/types.d.ts");
        assert_eq!(source.len(), 766_861);
        assert_eq!(
            format!("{:x}", Sha256::digest(source.as_bytes())),
            "a1bf4dfbb780099a2605764f3899234b7b47335f27f3f4e2600f9ecdbfbee098"
        );
        assert!(source.contains("Exclude<Property, `on${string}`|'cron'>"));
        let ir = owned_tsz_ir_at_path(source, "plugins/jsvm/internal/types/generated/types.d.ts")?;
        for name in [
            b"cronAdd".as_slice(),
            b"cronRemove".as_slice(),
            b"excludeHooks".as_slice(),
        ] {
            assert!(
                ir.items().any(|item| item.name() == name),
                "full generated source retains {:?}",
                name
            );
        }
        assert!(
            ir.items().count() > 100,
            "whole source lowered, not the extracted reproducer"
        );
        assert_eq!(&source.as_bytes()[89_418..89_426], b"_swPlPkc");
        assert!(
            ir.link_occurrences().any(|(_, site)| {
                site.source
                    .is_some_and(|span| span.start() == 89_418 && span.end() == 89_426)
                    && ir.link(site.link).is_some_and(|link| {
                        ir.item(link.from)
                            .is_some_and(|owner| owner.name() == b"File"
                                && owner.source().is_some_and(|primary| primary.end() < 89_418))
                            && matches!(link.target, backend_semantic::ir::LinkTarget::Local(target)
                            if ir.item(target).is_some_and(|item| item.name() == b"_swPlPkc"))
                    })
            }),
            "the original failing site keeps its exact owner/target while the primary excerpt stays in the first part"
        );
        Ok(())
    }

    #[test]
    fn native_tsz_preserves_a_source_owned_mapped_type_node() -> Result<(), LaneError> {
        let source = "export type Labels<T> = { [K in keyof T]: string };";
        let ir = owned_tsz_ir(source)?;
        let labels = ir
            .items()
            .find(|item| item.name() == b"Labels")
            .ok_or(LaneError::Missing("Labels type alias"))?;
        let extension = ir
            .typescript_extension(labels.id())
            .ok_or(LaneError::Missing("TypeScript extension for Labels"))?;
        let observed = extension
            .observed
            .ok_or(LaneError::Missing("native TSZ mapped observation"))?;
        if !matches!(
            ir.ty(observed),
            Some(backend_semantic::ir::TypeExpr::Computed(
                backend_semantic::ir::ComputedType::Mapped { .. }
            ))
        ) {
            return Err(LaneError::Missing(
                "mapped type expression in the existing computed-type lane",
            ));
        }
        Ok(())
    }

    #[test]
    fn native_tsz_preserves_declared_typeof_and_distinct_keyof_indexed_access()
    -> Result<(), LaneError> {
        let source = concat!(
            "export const marker = { value: 1 };\n",
            "export type MarkerQuery = typeof marker;\n",
            "export type Keys<T> = keyof T;\n",
            "export type Value<T, K extends keyof T> = T[K];\n",
        );
        let ir = owned_tsz_ir(source)?;
        let observed = |name: &[u8]| -> Result<TypeId, LaneError> {
            let item = ir
                .items()
                .find(|item| item.name() == name)
                .ok_or(LaneError::Missing("operator declaration"))?;
            ir.typescript_extension(item.id())
                .and_then(|extension| extension.observed)
                .ok_or(LaneError::Missing("native operator observation"))
        };

        let marker = ir
            .items()
            .find(|item| item.name() == b"marker")
            .ok_or(LaneError::Missing("typeof target entity"))?;
        let marker_query = ir
            .items()
            .find(|item| item.name() == b"MarkerQuery")
            .ok_or(LaneError::Missing("typeof alias entity"))?;
        let declared = ir
            .typescript_extension(marker_query.id())
            .and_then(|extension| extension.declared)
            .ok_or(LaneError::Missing("declared TypeQuery cell"))?;
        #[cfg(test)]
        eprintln!(
            "TYPE_QUERY_IR_TRACE declared={declared:?} type={:?} observed={:?}",
            ir.ty(declared),
            native_observation(&ir, b"MarkerQuery")
        );
        let TypeExpr::Computed(ComputedType::TypeOf(TypeQuery::Entity(target))) =
            ir.ty(declared)
                .ok_or(LaneError::Missing("declared TypeOf row"))?
        else {
            return Err(LaneError::Missing("declared entity-targeted TypeOf row"));
        };
        if target != marker.id() {
            return Err(LaneError::Missing(
                "exact declared TypeOf entity coordinate",
            ));
        }

        if !matches!(
            ir.ty(observed(b"Keys")?),
            Some(TypeExpr::Computed(ComputedType::KeyOf(_)))
        ) {
            return Err(LaneError::Missing("distinct computed KeyOf row"));
        }
        if !matches!(
            ir.ty(observed(b"Value")?),
            Some(TypeExpr::Computed(ComputedType::IndexedAccess { .. }))
        ) {
            return Err(LaneError::Missing(
                "distinct object/index-ordered IndexedAccess row",
            ));
        }
        // The authored query belongs to the declared lane. The checker may
        // normalize its observed value type; this test must not demand that
        // the observation repeat the source operator.
        if ir
            .typescript_extension(marker_query.id())
            .and_then(|extension| extension.observed)
            .is_none()
        {
            return Err(LaneError::Missing("checker-observed alias value type"));
        }
        Ok(())
    }

    /// Replays one acquired, fully installed TypeScript project through the
    /// production compiler-API closure, shared checkpoint, and borrowed query
    /// session. The corpus is external because it includes its original npm
    /// installation; paths and installed tool identities are supplied by the
    /// receipt-producing runner.
    #[test]
    #[ignore = "requires a fully installed configured TypeScript corpus and admitted Node path"]
    fn native_tsz_compiles_configured_project_alias_with_real_libraries() -> Result<(), LaneError> {
        use std::{
            path::PathBuf,
            sync::atomic::AtomicBool,
            time::{Duration, Instant},
        };

        use crate::application::{
            PackageSource, ToolchainProbeLimits, TypeScriptProjectHost, build_native_inputs,
        };
        use backend_frontend_typescript::TszProjectExecutionBudget;
        use std::num::NonZeroUsize;

        let root = std::env::var_os("TSZ_CONFIGURED_CORPUS_ROOT")
            .map(PathBuf::from)
            .ok_or(LaneError::Missing("TSZ_CONFIGURED_CORPUS_ROOT"))?;
        let node = std::env::var_os("TSZ_CONFIGURED_NODE")
            .map(PathBuf::from)
            .ok_or(LaneError::Missing("TSZ_CONFIGURED_NODE"))?;
        let source_paths = [
            "src/app/api/route.ts",
            "src/app/page.tsx",
            "src/app/tutorial/page.tsx",
            "src/components/NavBar.tsx",
            "src/lib/scheduler.ts",
        ];
        let source_texts = source_paths
            .iter()
            .map(|path| std::fs::read_to_string(root.join(path)))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| LaneError::Missing("configured project source bytes"))?;
        let package_sources = source_paths
            .iter()
            .zip(&source_texts)
            .map(|(path, source)| {
                PackageSource::new(path, source)
                    .map_err(|_| LaneError::Missing("normalized configured source path"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let limits = ToolchainProbeLimits::new(
            Duration::from_secs(600),
            NonZeroUsize::new(16 * 1024 * 1024)
                .ok_or(LaneError::Missing("nonzero compiler API stream limit"))?,
        )
        .map_err(|_| LaneError::Missing("bounded compiler API probe limits"))?;
        let host = TypeScriptProjectHost::new(None, Some(node), None, None, limits);
        let admitted = host
            .admit(&root)
            .map_err(|error| LaneError::ConfiguredProject(format!("admit: {error:?}")))?
            .ok_or(LaneError::Missing("project-local TypeScript installation"))?;
        let inputs = admitted.inputs();
        let mut resolver = inputs.resolver();
        let cancelled = AtomicBool::new(false);
        let deadline = Instant::now() + Duration::from_secs(900);
        let native = build_native_inputs(
            &inputs,
            &mut resolver,
            &package_sources,
            deadline,
            &cancelled,
        )
        .map_err(|error| LaneError::ConfiguredProject(format!("compiler API bridge: {error:?}")))?;
        native
            .closure_witness
            .validate_current(admitted.witness().as_ref())
            .map_err(|error| {
                LaneError::ConfiguredProject(format!("resolver witness: {error:?}"))
            })?;
        let package_membership = &native.package_membership;
        assert_eq!(package_membership.len(), source_paths.len());
        for path in source_paths {
            package_membership
                .get(path)
                .ok_or(LaneError::Missing(
                    "captured configured corpus source membership",
                ))?
                .selected_program_path()
                .ok_or(LaneError::Missing(
                    "configured corpus source excluded from selected program",
                ))?;
        }
        let route_path = package_membership
            .get("src/app/api/route.ts")
            .ok_or(LaneError::Missing("captured alias importer membership"))?
            .selected_program_path()
            .ok_or(LaneError::Missing("exact alias importer TSZ path"))?;
        let scheduler_path = package_membership
            .get("src/lib/scheduler.ts")
            .ok_or(LaneError::Missing("captured aliased target membership"))?
            .selected_program_path()
            .ok_or(LaneError::Missing("exact aliased target TSZ path"))?;
        assert!(
            !native.options.checker.no_lib,
            "configured ambient libraries are admitted"
        );
        assert!(native.sources.len() > source_paths.len());
        assert!(
            native.libraries.len() > 1,
            "TypeScript default libraries are retained"
        );
        assert!(
            native.options.module_resolutions.iter().any(|resolution| {
                resolution.importer_path.as_str() == route_path
                    && resolution.specifier == "@/lib/scheduler"
                    && matches!(
                        &resolution.target,
                        TszProjectModuleResolutionTarget::File { path }
                            if path.as_str() == scheduler_path
                    )
            }),
            "the compiler's paths alias resolves to its exact source file"
        );

        let work_units = native.work_units;
        let budget = TszProjectExecutionBudget::new(deadline, &cancelled, work_units);
        let mut authority = TszProjectAuthority::new();
        authority
            .update_with_execution_checkpoint(
                native.sources,
                native.options,
                &native.libraries,
                &budget,
            )
            .map_err(|_| LaneError::Missing("checked full configured TSZ project"))?;
        let project = authority
            .project()
            .ok_or(LaneError::Missing("published configured TSZ project"))?;
        let session = project
            .checked_query_session(&budget)
            .map_err(|_| LaneError::Missing("shared configured project query session"))?;
        let route_index = source_paths
            .iter()
            .position(|path| *path == "src/app/api/route.ts")
            .ok_or(LaneError::Missing("alias importer source"))?;
        let route_path = package_membership
            .get(source_paths[route_index])
            .ok_or(LaneError::Missing("captured alias importer membership"))?
            .selected_program_path()
            .ok_or(LaneError::Missing("exact alias importer TSZ path"))?;
        let mut route_facts = FactSet::new();
        collect_with_tsz(
            TypeScriptSource::TypeScript,
            source_texts[route_index].as_bytes(),
            project,
            route_path,
            &session,
            &mut route_facts,
        )?;
        let targets = staged_tsz_coordinates(&route_facts)?;
        let scheduler_name = source_texts[4]
            .find("generateSchedule")
            .ok_or(LaneError::Missing("aliased source declaration name"))?;
        assert!(
            targets.iter().any(|(_, path, _, _, name_start, _)| {
                path.as_str() == scheduler_path && *name_start == scheduler_name as u32
            }),
            "aliased use must point to the exact dependency declaration: {targets:?}"
        );
        Ok(())
    }

    #[test]
    fn native_tsz_wide_non_associative_tuple_fails_with_exact_bounded_cause() {
        let elements = std::iter::repeat_n("unknown", super::MAX_TYPE_CHILDREN + 1)
            .collect::<Vec<_>>()
            .join(", ");
        let source = format!("export declare const wide: [{elements}];");
        let error = match owned_tsz_ir(&source) {
            Ok(ir) => panic!(
                "wide tuple must refuse atomically; observed {:?}",
                native_observation(&ir, b"wide")
            ),
            Err(error) => error,
        };
        assert!(
            matches!(
                error,
                LaneError::Collection(TypeScriptCollectError::Rejected(
                    super::FactRejection {
                        cause: super::FactFault::TypeProjectionWidth {
                            actual,
                            maximum: super::MAX_TYPE_CHILDREN,
                        },
                        ..
                    }
                )) if actual == super::MAX_TYPE_CHILDREN + 1
            ),
            "wide tuple refusal must retain exact native width; got {error:?}"
        );
    }

    #[test]
    fn native_tsz_recursive_structural_type_fails_with_exact_typed_cause() {
        let source = "export type Recursive = { next: Recursive };";
        let error = match owned_tsz_ir(source) {
            Ok(ir) => panic!(
                "recursive row must refuse atomically; observed {:?}",
                native_observation(&ir, b"Recursive")
            ),
            Err(error) => error,
        };
        assert!(
            matches!(
                error,
                LaneError::Collection(TypeScriptCollectError::Rejected(super::FactRejection {
                    cause: super::FactFault::TypeProjectionRecursiveReference { distance: 0 },
                    ..
                })) | LaneError::Collection(TypeScriptCollectError::Rejected(
                    super::FactRejection {
                        cause: super::FactFault::TypeProjectionCycle { .. },
                        ..
                    }
                ))
            ),
            "recursive type refusal must retain exact cycle cause; got {error:?}"
        );
    }

    #[test]
    fn native_tsz_deep_structural_type_fails_at_the_exact_depth_bound() {
        let mut ty = "string".to_owned();
        for index in 0..(usize::from(super::MAX_TYPE_DEPTH) + 1) {
            ty = format!("{{ p{index}: {ty} }}");
        }
        let source = format!("export declare const deep: {ty};");
        let error = match owned_tsz_ir(&source) {
            Ok(ir) => panic!(
                "deep row must refuse atomically; observed {:?}",
                native_observation(&ir, b"deep")
            ),
            Err(error) => error,
        };
        assert!(
            matches!(
                error,
                LaneError::Collection(TypeScriptCollectError::Rejected(
                    super::FactRejection {
                        cause: super::FactFault::TypeProjectionDepthLimit {
                            depth,
                            maximum: super::MAX_TYPE_DEPTH,
                        },
                        ..
                    }
                )) if depth == super::MAX_TYPE_DEPTH + 1
            ),
            "deep type refusal must retain exact depth; got {error:?}"
        );
    }

    /// One validated report over the exact fixture source carrying the given
    /// references (the TSZ checker's wire shape).
    fn report(source: &str, references: Vec<Reference>) -> Report {
        let digest = source_digest(source.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        Report {
            schema_version: 1,
            source_digest: digest,
            declaration_file: false,
            diagnostics: Box::new([]),
            declarations: Box::new([]),
            references: references.into_boxed_slice(),
            narrowings: Box::new([]),
        }
    }

    /// Exact source ranges retained by OXC's bound declaration and typed
    /// parameter AST for one compiler symbol. The identity is joined through
    /// `Scoping::symbol_declarations`, never inferred from string positions.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum SignatureTypeSyntax {
        StringKeyword,
        NumberKeyword,
        Other,
    }

    #[derive(Debug)]
    struct SignatureSourceFacts {
        declaration: (u32, u32),
        parameters: Vec<((u32, u32), backend_semantic::ir::TupleElementKind)>,
        parameter_types: Vec<SignatureTypeSyntax>,
        type_parameters: Vec<(u32, u32)>,
        result: Option<(u32, u32)>,
    }

    fn signature_source_facts(
        source: &str,
        symbol_name: &str,
    ) -> Result<Vec<SignatureSourceFacts>, LaneError> {
        with_analysis(TypeScriptSource::TypeScript, source, |module| {
            let scoping = module.semantic.scoping();
            let Some(symbol) = scoping.symbol_ids().find(|symbol| {
                scoping.symbol_name(*symbol) == symbol_name
                    && scoping
                        .symbol_flags(*symbol)
                        .contains(SymbolFlags::Function)
            }) else {
                return Vec::new();
            };
            let declarations = scoping.symbol_declarations(symbol).collect::<Vec<_>>();
            module
                .semantic
                .nodes()
                .iter_enumerated()
                .filter_map(|(node_id, node)| {
                    if !declarations.contains(&node_id) {
                        return None;
                    }
                    let function = node.kind().as_function()?;
                    let parameter_types =
                        function
                            .params
                            .items
                            .iter()
                            .map(|parameter| {
                                let Some(annotation) = parameter.type_annotation.as_ref() else {
                                    return SignatureTypeSyntax::Other;
                                };
                                let annotation_span = annotation.type_annotation.span();
                                let kind = module.semantic.nodes().iter_enumerated().find_map(
                                    |(_, node)| {
                                        let kind = node.kind();
                                        (kind.span() == annotation_span).then_some(kind)
                                    },
                                );
                                if kind.is_some_and(|kind| kind.as_ts_string_keyword().is_some()) {
                                    SignatureTypeSyntax::StringKeyword
                                } else if kind
                                    .is_some_and(|kind| kind.as_ts_number_keyword().is_some())
                                {
                                    SignatureTypeSyntax::NumberKeyword
                                } else {
                                    SignatureTypeSyntax::Other
                                }
                            })
                            .collect();
                    let mut parameters = function
                        .params
                        .items
                        .iter()
                        .map(|parameter| {
                            (
                                (parameter.pattern.span().start, parameter.pattern.span().end),
                                if parameter.optional {
                                    backend_semantic::ir::TupleElementKind::Optional
                                } else {
                                    backend_semantic::ir::TupleElementKind::Required
                                },
                            )
                        })
                        .collect::<Vec<_>>();
                    if let Some(rest) = function.params.rest.as_ref() {
                        let span = rest.rest.argument.span();
                        parameters.push((
                            (span.start, span.end),
                            backend_semantic::ir::TupleElementKind::Rest,
                        ));
                    }
                    Some(SignatureSourceFacts {
                        declaration: (function.span.start, function.span.end),
                        parameters,
                        parameter_types,
                        type_parameters: function
                            .type_parameters
                            .as_ref()
                            .map(|parameters| {
                                parameters
                                    .params
                                    .iter()
                                    .map(|parameter| {
                                        (parameter.name.span.start, parameter.name.span.end)
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                        result: function.return_type.as_ref().map(|returned| {
                            let span = returned.type_annotation.span();
                            (span.start, span.end)
                        }),
                    })
                })
                .collect()
        })
        .map_err(|_| LaneError::Missing("fixture source parses under OXC authority"))
    }

    /// A fixture whose checker authority resolved the OXC-unresolved
    /// `HonoContext` annotation to the `hono` package. The import specifier
    /// spells the module and the alias spells the foreign display name, so
    /// every wire cell stays source-backed.
    const HONO_SOURCE: &str = "import { Hono as Context } from \"hono\";\n\nexport function route(app: HonoContext): void {\n    return;\n}\n";

    fn hono_report() -> Report {
        let start = u32::try_from(HONO_SOURCE.find("HonoContext").expect("fixture site"))
            .expect("fixture offset");
        let end = start + u32::try_from("HonoContext".len()).expect("fixture length");
        report(
            HONO_SOURCE,
            vec![Reference {
                start,
                end,
                target_start: None,
                target_end: None,
                module: Some("hono".to_owned()),
                name: Some("Context".to_owned()),
                overload_index: None,
                is_field: false,
                is_enum_member: false,
                is_const: false,
                is_variable: false,
            }],
        )
    }

    /// A checker-resolved foreign reference carries an oracle-confident
    /// foreign key naming the resolved package (ecosystem, package) and the
    /// foreign display symbol, with the use-site spelling as the path.
    #[test]
    fn checker_resolved_foreign_references_carry_module_and_display() -> Result<(), LaneError> {
        let report = hono_report();
        let view = lower_fragment(HONO_SOURCE, Some(&report))?;
        let mut found = false;
        for row in view
            .occurrences()
            .ok_or(LaneError::Missing("occurrence plane"))?
        {
            let row = row.map_err(LaneError::from)?;
            let occurrence = row.occurrence;
            let backend_semantic::ir::OccurrenceTarget::Foreign(key) = occurrence.target else {
                continue;
            };
            if key.path != "HonoContext" {
                continue;
            }
            found = true;
            let backend_semantic::ir::ForeignOrigin::Package(lineage) = key.origin else {
                return Err(LaneError::Missing("hono package origin"));
            };
            if lineage.ecosystem != "npm" || lineage.name != "hono" {
                return Err(LaneError::Missing("hono package lineage"));
            }
            if key.display != "Context" {
                return Err(LaneError::Missing("foreign display symbol"));
            }
            if occurrence.confidence != backend_semantic::ir::OccurrenceConfidence::Oracle {
                return Err(LaneError::Missing("oracle confidence"));
            }
            if occurrence.kind != backend_semantic::ir::ReferenceKind::TypeReference {
                return Err(LaneError::Missing("type-reference kind"));
            }
        }
        if !found {
            return Err(LaneError::Missing("checker-resolved foreign occurrence"));
        }
        Ok(())
    }

    /// An identifier the checker never resolved stays at syntactic
    /// confidence against the npm universe, carrying its written spelling.
    #[test]
    fn genuinely_unresolved_references_stay_syntactic_universe() -> Result<(), LaneError> {
        let source = "export function other(app: MysteryBox): void {\n    return;\n}\n";
        let view = lower_fragment(source, Some(&report(source, Vec::new())))?;
        let mut found = false;
        for row in view
            .occurrences()
            .ok_or(LaneError::Missing("occurrence plane"))?
        {
            let row = row.map_err(LaneError::from)?;
            let occurrence = row.occurrence;
            let backend_semantic::ir::OccurrenceTarget::Foreign(key) = occurrence.target else {
                continue;
            };
            if key.path != "MysteryBox" {
                continue;
            }
            found = true;
            if !matches!(
                key.origin,
                backend_semantic::ir::ForeignOrigin::Universe { ecosystem: "npm" }
            ) {
                return Err(LaneError::Missing("npm-universe origin"));
            }
            if key.display != "MysteryBox" {
                return Err(LaneError::Missing("written display"));
            }
            if occurrence.confidence != backend_semantic::ir::OccurrenceConfidence::Syntactic {
                return Err(LaneError::Missing("syntactic confidence"));
            }
        }
        if !found {
            return Err(LaneError::Missing("unresolved foreign occurrence"));
        }
        Ok(())
    }

    /// Different initializers keep sibling block bindings distinct: both
    /// `const value` facts publish, and a use inside the second block
    /// targets the second binding's own ordinal.
    #[test]
    fn merged_block_redeclaration_uses_keep_their_written_spelling() -> Result<(), LaneError> {
        let source = "export function probe(): void {\n    {\n        const value = 1;\n    }\n    {\n        const value = 2;\n        value;\n    }\n}\n";
        let view = lower_fragment(source, None)?;
        let probe = {
            let mut ordinal = None;
            for entity in view.entities() {
                if entity.kind != EntityKind::Function {
                    continue;
                }
                let atom_index = usize::try_from(entity.name.raw)?;
                let Some(atom) = view.atoms().nth(atom_index) else {
                    continue;
                };
                if atom.bytes == b"probe".as_slice() {
                    ordinal = Some(entity.entity.raw);
                    break;
                }
            }
            ordinal.ok_or(LaneError::Missing("probe function fact"))?
        };
        let mut values = Vec::new();
        for entity in view.entities() {
            if entity.kind != EntityKind::Constant {
                continue;
            }
            let atom_index = usize::try_from(entity.name.raw)?;
            let Some(atom) = view.atoms().nth(atom_index) else {
                continue;
            };
            if atom.bytes == b"value".as_slice() {
                values.push(entity.entity.raw);
            }
        }
        if values.len() != 2 {
            return Err(LaneError::Missing("two distinct value constants"));
        }
        let second_value = values[1];
        let mut found = false;
        for row in view
            .occurrences()
            .ok_or(LaneError::Missing("occurrence plane"))?
        {
            let row = row.map_err(LaneError::from)?;
            let occurrence = row.occurrence;
            if row.owner.raw != probe {
                continue;
            }
            if occurrence.kind != backend_semantic::ir::ReferenceKind::VariableUse {
                continue;
            }
            if occurrence.target
                != backend_semantic::ir::OccurrenceTarget::Local(
                    backend_semantic::ir::EntityId::new(second_value),
                )
            {
                continue;
            }
            found = true;
            if occurrence.confidence != backend_semantic::ir::OccurrenceConfidence::Index {
                return Err(LaneError::Missing("index confidence"));
            }
        }
        if !found {
            return Err(LaneError::Missing("second-block value use occurrence"));
        }
        Ok(())
    }

    /// Every declared entity carries its declaration extent as a
    /// source-backed provenance span (synthetic type-expression embodiments
    /// carry none, honestly), and the occurrence site is the exact name
    /// extent inside that span — the containment law the image build
    /// re-checks against owner-relative spans.
    #[test]
    fn declared_entities_carry_source_verified_spans() -> Result<(), LaneError> {
        let report = hono_report();
        let ir = owned_ir(HONO_SOURCE, Some(&report))?;
        let mut route_span = None;
        let mut context_span = None;
        let mut route_entity = None;
        let mut app_parameter = None;
        for entity in ir.canonical_entities() {
            if entity.name.named_atom().and_then(|atom| ir.atom(atom)) == Some(b"route".as_slice())
                && entity.kind == EntityKind::Function
            {
                route_span = entity.source;
                route_entity = Some(entity.id);
            }
            if entity.name.named_atom().and_then(|atom| ir.atom(atom)) == Some(b"app".as_slice())
                && entity.kind == EntityKind::Parameter
            {
                app_parameter = Some(entity.id);
            }
            if entity.name.named_atom().and_then(|atom| ir.atom(atom))
                == Some(b"Context".as_slice())
                && entity.kind == EntityKind::Reexport
            {
                context_span = entity.source;
            }
        }
        // The declared entity's provenance span is its declaration extent:
        // source-backed and strictly covering the declaration name.
        let Some(span) = route_span else {
            return Err(LaneError::Missing("route declaration source span"));
        };
        let route_name_at = HONO_SOURCE
            .find("route(")
            .ok_or(LaneError::Missing("fixture declaration name"))?;
        if span.start() as usize > route_name_at
            || (span.end() as usize) < route_name_at + "route".len()
            || span.end() as usize > HONO_SOURCE.len()
        {
            return Err(LaneError::Missing("route declaration extent span"));
        }
        let route_entity = route_entity.ok_or(LaneError::Missing("route semantic entity"))?;
        let app_parameter = app_parameter.ok_or(LaneError::Missing("app parameter entity"))?;
        if ir.signature_carrier_role(route_entity)
            != Some(
                backend_semantic::ir::SignatureCarrierRoleObservation::Captured(
                    backend_semantic::ir::SignatureCarrierRole::NotCarrier,
                ),
            )
            || ir.signature_carrier_role(app_parameter)
                != Some(
                    backend_semantic::ir::SignatureCarrierRoleObservation::Captured(
                        backend_semantic::ir::SignatureCarrierRole::Input,
                    ),
                )
        {
            return Err(LaneError::Missing("owned TypeScript carrier roles"));
        }
        // The import binding's provenance span is the import declaration.
        if context_span.is_none() {
            return Err(LaneError::Missing("import binding source span"));
        }
        // The occurrence site is the exact name extent of the reference,
        // absolute in the entered source.
        let site_at = HONO_SOURCE
            .find("HonoContext")
            .ok_or(LaneError::Missing("fixture site"))?;
        let site_end = site_at + "HonoContext".len();
        let mut verified = false;
        for (_, occurrence) in ir.link_occurrences() {
            let Some(site) = occurrence.source else {
                continue;
            };
            let start = usize::try_from(site.start())?;
            let end = usize::try_from(site.end())?;
            // The span's file atom is the declaration-scope path, and its
            // bounds are the exact name extent of the reference site.
            if ir.atom(site.file()) == Some(b"fixture/source".as_slice())
                && start == site_at
                && end == site_end
            {
                verified = true;
            }
        }
        if !verified {
            return Err(LaneError::Missing("absolute name-extent site"));
        }
        Ok(())
    }

    /// Optional and rest modifiers do not renumber exact signature slots:
    /// the rest carrier remains the final parameter and the return carrier
    /// occupies the separate result role.
    #[test]
    fn optional_and_rest_parameters_keep_owner_local_binding_positions() -> Result<(), LaneError> {
        let source = "type GatherResult = string;\nexport function gather<T>(head: T, suffix?: number, ...items: boolean[]): GatherResult { return head; }\n";
        let ir = owned_ir(source, None)?;
        let source_signatures = signature_source_facts(source, "gather")?;
        let source_signature = source_signatures
            .first()
            .ok_or(LaneError::Missing("OXC gather signature"))?;
        if source_signatures.len() != 1 || source_signature.parameters.len() != 3 {
            return Err(LaneError::Missing("one exact OXC gather signature"));
        }
        let result_alias = ir
            .items()
            .find(|item| item.name() == b"GatherResult" && item.kind() == EntityKind::Alias)
            .ok_or(LaneError::Missing("GatherResult alias"))?;
        let owner = ir
            .items()
            .find(|item| item.name() == b"gather" && item.kind() == EntityKind::Function)
            .ok_or(LaneError::Missing("gather function"))?;
        let carrier = |name: &[u8]| {
            ir.items()
                .find(|item| item.name() == name && item.kind() == EntityKind::Parameter)
                .map(|item| item.id())
        };
        let head = carrier(b"head").ok_or(LaneError::Missing("head parameter"))?;
        let suffix = carrier(b"suffix").ok_or(LaneError::Missing("optional parameter"))?;
        let items = carrier(b"items").ok_or(LaneError::Missing("rest parameter"))?;
        let generic = ir
            .items()
            .find(|item| {
                item.name() == b"T"
                    && item.kind() == EntityKind::Parameter
                    && item.parent() == Some(owner.id())
            })
            .ok_or(LaneError::Missing("owner-local generic parameter"))?;
        let generic_source = source_signature
            .type_parameters
            .first()
            .ok_or(LaneError::Missing("OXC gather generic parameter"))?;
        let bindings = match ir.signature_carrier_bindings(owner.id()) {
            Some(backend_semantic::ir::SignatureCarrierBindingsObservation::Captured(bindings)) => {
                bindings.collect::<Vec<_>>()
            }
            _ => return Err(LaneError::Missing("gather signature bindings")),
        };
        let slots: Vec<_> = bindings
            .iter()
            .map(|binding| {
                (
                    binding.owner,
                    binding.role,
                    binding.position,
                    binding.carrier,
                )
            })
            .collect();
        let result_target = slots
            .iter()
            .find(|(_, role, _, _)| {
                *role == backend_semantic::ir::SignatureCarrierBindingRole::Result
            })
            .map(|(_, _, _, target)| *target)
            .ok_or(LaneError::Missing("gather result carrier binding"))?;
        if slots
            != vec![
                (
                    owner.id(),
                    backend_semantic::ir::SignatureCarrierBindingRole::Parameter,
                    0,
                    head,
                ),
                (
                    owner.id(),
                    backend_semantic::ir::SignatureCarrierBindingRole::Parameter,
                    1,
                    suffix,
                ),
                (
                    owner.id(),
                    backend_semantic::ir::SignatureCarrierBindingRole::Parameter,
                    2,
                    items,
                ),
                (
                    owner.id(),
                    backend_semantic::ir::SignatureCarrierBindingRole::Result,
                    0,
                    result_target,
                ),
            ]
        {
            return Err(LaneError::Missing("TypeScript optional/rest slot order"));
        }
        let result_carrier = ir
            .item(result_target)
            .ok_or(LaneError::Missing("gather result carrier row"))?;
        let function_type = ir
            .ty(owner
                .semantic_type()
                .ok_or(LaneError::Missing("gather type"))?)
            .ok_or(LaneError::Missing("gather semantic function type"))?;
        let backend_semantic::ir::TypeExpr::Concrete(
            backend_semantic::ir::ConcreteType::Function {
                parameters,
                results,
                ..
            },
        ) = function_type
        else {
            return Err(LaneError::Missing("gather concrete function shape"));
        };
        let parameters = ir
            .tuple_elements(parameters)
            .ok_or(LaneError::Missing("gather parameter tuple"))?;
        let results = ir
            .tuple_elements(results)
            .ok_or(LaneError::Missing("gather result tuple"))?;
        let exact_parameter = |id, position: usize| -> bool {
            let Some(((start, end), _)) = source_signature.parameters.get(position) else {
                return false;
            };
            ir.item(id)
                .and_then(|item| item.source())
                .is_some_and(|span| span.start() == *start && span.end() == *end)
        };
        if parameters.len() != 3
            || parameters
                .iter()
                .map(|element| element.kind)
                .collect::<Vec<_>>()
                != vec![
                    backend_semantic::ir::TupleElementKind::Required,
                    backend_semantic::ir::TupleElementKind::Optional,
                    backend_semantic::ir::TupleElementKind::Rest,
                ]
            || source_signature
                .parameters
                .iter()
                .map(|(_, kind)| *kind)
                .collect::<Vec<_>>()
                != parameters
                    .iter()
                    .map(|element| element.kind)
                    .collect::<Vec<_>>()
            || parameters
                .iter()
                .map(|element| element.label.and_then(|label| ir.atom(label)))
                .collect::<Vec<_>>()
                != vec![Some(b"head".as_slice()), Some(b"suffix"), Some(b"items")]
            || results.len() != 1
            || source_signature.result.and_then(|(start, end)| {
                source
                    .get(usize::try_from(start).ok()?..usize::try_from(end).ok()?)
                    .map(str::as_bytes)
            }) != Some(
                result_alias
                    .name()
                    .named_bytes()
                    .ok_or(LaneError::Missing("named Result alias"))?,
            )
            || ir.ty(results[0].ty)
                != Some(backend_semantic::ir::TypeExpr::Concrete(
                    backend_semantic::ir::ConcreteType::Nominal(result_alias.id()),
                ))
            || result_carrier.kind() != EntityKind::Parameter
            || result_carrier.name() != b"gather"
            || result_carrier.parent() != Some(owner.id())
            || result_carrier.source().is_some()
            || result_carrier.semantic_type() != Some(results[0].ty)
            || owner.source().is_none_or(|span| {
                span.start() != source_signature.declaration.0
                    || span.end() != source_signature.declaration.1
            })
            || generic.source().is_none_or(|span| {
                span.start() != generic_source.0 || span.end() != generic_source.1
            })
            || [head, suffix, items].into_iter().any(|id| {
                ir.item(id).is_none_or(|item| {
                    item.kind() != EntityKind::Parameter || item.parent() != Some(owner.id())
                })
            })
            || !exact_parameter(head, 0)
            || !exact_parameter(suffix, 1)
            || !exact_parameter(items, 2)
            || generic.id() == head
            || bindings
                .iter()
                .any(|binding| binding.carrier == generic.id())
        {
            return Err(LaneError::Missing(
                "generic optional/rest slots and owner-local result carrier",
            ));
        }
        Ok(())
    }

    /// Distinct overloads with the same symbol retain their own exact
    /// parameter and result carriers. Authored owner/parameter spans identify
    /// each overload; each synthetic result carrier keeps the overload's
    /// exact owner and nominal result-alias type.
    #[test]
    fn overloads_keep_exact_owner_local_parameter_and_result_carriers() -> Result<(), LaneError> {
        let source = concat!(
            "type TextResult = string;\n",
            "type CountResult = number;\n",
            "declare function decode(value: string): TextResult;\n",
            "declare function decode(value: number): CountResult;\n",
        );
        let ir = owned_ir(source, None)?;
        let source_signatures = signature_source_facts(source, "decode")?;
        let text_source_signature = source_signatures
            .iter()
            .find(|signature| {
                signature.parameter_types.first() == Some(&SignatureTypeSyntax::StringKeyword)
            })
            .ok_or(LaneError::Missing("OXC string overload"))?;
        let count_source_signature = source_signatures
            .iter()
            .find(|signature| {
                signature.parameter_types.first() == Some(&SignatureTypeSyntax::NumberKeyword)
            })
            .ok_or(LaneError::Missing("OXC number overload"))?;
        if source_signatures.len() != 2
            || text_source_signature.declaration == count_source_signature.declaration
        {
            return Err(LaneError::Missing("two exact OXC decode overloads"));
        }
        let owner_at = |range: (u32, u32)| {
            ir.items().find(|item| {
                item.name() == b"decode"
                    && item.kind() == EntityKind::Function
                    && item
                        .source()
                        .is_some_and(|source| source.start() == range.0 && source.end() == range.1)
            })
        };
        let text_owner = owner_at(text_source_signature.declaration).ok_or(LaneError::Missing(
            "string overload at its OXC source range",
        ))?;
        let count_owner = owner_at(count_source_signature.declaration).ok_or(
            LaneError::Missing("number overload at its OXC source range"),
        )?;
        let parameter_at = |range: (u32, u32), owner| {
            ir.items().find(|item| {
                item.name() == b"value"
                    && item.kind() == EntityKind::Parameter
                    && item
                        .source()
                        .is_some_and(|source| source.start() == range.0 && source.end() == range.1)
                    && item.parent() == Some(owner)
            })
        };
        let text_parameter_range = text_source_signature
            .parameters
            .first()
            .map(|(range, _)| *range)
            .ok_or(LaneError::Missing("OXC string overload parameter"))?;
        let count_parameter_range = count_source_signature
            .parameters
            .first()
            .map(|(range, _)| *range)
            .ok_or(LaneError::Missing("OXC number overload parameter"))?;
        let text_parameter = parameter_at(text_parameter_range, text_owner.id()).ok_or(
            LaneError::Missing("string overload's exact parameter carrier"),
        )?;
        let count_parameter = parameter_at(count_parameter_range, count_owner.id()).ok_or(
            LaneError::Missing("number overload's exact parameter carrier"),
        )?;
        let text_result_spelling = text_source_signature
            .result
            .and_then(|(start, end)| {
                Some(source.get(usize::try_from(start).ok()?..usize::try_from(end).ok()?)?)
            })
            .ok_or(LaneError::Missing("OXC string overload result spelling"))?;
        let count_result_spelling = count_source_signature
            .result
            .and_then(|(start, end)| {
                Some(source.get(usize::try_from(start).ok()?..usize::try_from(end).ok()?)?)
            })
            .ok_or(LaneError::Missing("OXC number overload result spelling"))?;
        let exact_alias = |name: &[u8], declaration: &str| {
            let start = u32::try_from(source.find(declaration)?).ok()?;
            let end = start.checked_add(u32::try_from(declaration.len()).ok()?)?;
            ir.items().find(|item| {
                item.name() == name
                    && item.kind() == EntityKind::Alias
                    && item
                        .source()
                        .is_some_and(|source| source.start() == start && source.end() == end)
            })
        };
        let text_result_alias = exact_alias(b"TextResult", "type TextResult = string;").ok_or(
            LaneError::Missing("TextResult alias at its exact source range"),
        )?;
        let count_result_alias = exact_alias(b"CountResult", "type CountResult = number;").ok_or(
            LaneError::Missing("CountResult alias at its exact source range"),
        )?;

        let text_type = text_owner
            .semantic_type()
            .ok_or(LaneError::Missing("string overload function type"))?;
        let count_type = count_owner
            .semantic_type()
            .ok_or(LaneError::Missing("number overload function type"))?;
        if text_type == count_type || ir.ty(text_type) == ir.ty(count_type) {
            return Err(LaneError::Missing(
                "overload-specific function parameter/result types",
            ));
        }
        let bindings = |owner| match ir.signature_carrier_bindings(owner) {
            Some(backend_semantic::ir::SignatureCarrierBindingsObservation::Captured(bindings)) => {
                Some(bindings.collect::<Vec<_>>())
            }
            _ => None,
        };
        let text_bindings = bindings(text_owner.id())
            .ok_or(LaneError::Missing("string overload signature bindings"))?;
        let count_bindings = bindings(count_owner.id())
            .ok_or(LaneError::Missing("number overload signature bindings"))?;
        let result_carrier = |owner, rows: &[backend_semantic::ir::SignatureCarrierBinding]| {
            rows.iter()
                .find(|binding| {
                    binding.owner == owner
                        && binding.role == backend_semantic::ir::SignatureCarrierBindingRole::Result
                        && binding.position == 0
                })
                .and_then(|binding| ir.item(binding.carrier))
        };
        let text_result_carrier = result_carrier(text_owner.id(), &text_bindings)
            .ok_or(LaneError::Missing("string overload result carrier"))?;
        let count_result_carrier = result_carrier(count_owner.id(), &count_bindings)
            .ok_or(LaneError::Missing("number overload result carrier"))?;
        let nominal_type = |alias: backend_semantic::ir::ItemView<'_>| {
            backend_semantic::ir::TypeExpr::Concrete(backend_semantic::ir::ConcreteType::Nominal(
                alias.id(),
            ))
        };
        let function_result_type = |owner: backend_semantic::ir::ItemView<'_>| -> Result<
            backend_semantic::ir::TypeExpr,
            LaneError,
        > {
            let type_id = owner
                .semantic_type()
                .ok_or(LaneError::Missing("overload function type"))?;
            let type_expr = ir
                .ty(type_id)
                .ok_or(LaneError::Missing("overload function type expression"))?;
            let backend_semantic::ir::TypeExpr::Concrete(
                backend_semantic::ir::ConcreteType::Function { results, .. },
            ) = type_expr
            else {
                return Err(LaneError::Missing("overload concrete function type"));
            };
            let results = ir
                .tuple_elements(results)
                .ok_or(LaneError::Missing("overload result tuple"))?;
            let result = results
                .first()
                .ok_or(LaneError::Missing("overload result cell"))?;
            ir.ty(result.ty)
                .ok_or(LaneError::Missing("overload result type expression"))
        };
        if text_bindings
            != vec![
                backend_semantic::ir::SignatureCarrierBinding {
                    owner: text_owner.id(),
                    role: backend_semantic::ir::SignatureCarrierBindingRole::Parameter,
                    position: 0,
                    carrier: text_parameter.id(),
                },
                backend_semantic::ir::SignatureCarrierBinding {
                    owner: text_owner.id(),
                    role: backend_semantic::ir::SignatureCarrierBindingRole::Result,
                    position: 0,
                    carrier: text_result_carrier.id(),
                },
            ]
            || count_bindings
                != vec![
                    backend_semantic::ir::SignatureCarrierBinding {
                        owner: count_owner.id(),
                        role: backend_semantic::ir::SignatureCarrierBindingRole::Parameter,
                        position: 0,
                        carrier: count_parameter.id(),
                    },
                    backend_semantic::ir::SignatureCarrierBinding {
                        owner: count_owner.id(),
                        role: backend_semantic::ir::SignatureCarrierBindingRole::Result,
                        position: 0,
                        carrier: count_result_carrier.id(),
                    },
                ]
            || text_parameter.parent() != Some(text_owner.id())
            || count_parameter.parent() != Some(count_owner.id())
            || text_parameter.id() == count_parameter.id()
            || text_result_carrier.kind() != EntityKind::Parameter
            || count_result_carrier.kind() != EntityKind::Parameter
            || text_result_carrier.name() != b"decode"
            || count_result_carrier.name() != b"decode"
            || text_result_carrier.parent() != Some(text_owner.id())
            || count_result_carrier.parent() != Some(count_owner.id())
            || text_result_carrier.id() == count_result_carrier.id()
            || text_result_carrier.source().is_some()
            || count_result_carrier.source().is_some()
            || text_result_carrier.semantic_type().and_then(|ty| ir.ty(ty))
                != Some(nominal_type(text_result_alias))
            || count_result_carrier
                .semantic_type()
                .and_then(|ty| ir.ty(ty))
                != Some(nominal_type(count_result_alias))
            || text_result_alias.name() != text_result_spelling.as_bytes()
            || count_result_alias.name() != count_result_spelling.as_bytes()
            || function_result_type(text_owner)? != nominal_type(text_result_alias)
            || function_result_type(count_owner)? != nominal_type(count_result_alias)
            || text_bindings
                .iter()
                .any(|binding| binding.owner != text_owner.id())
            || count_bindings
                .iter()
                .any(|binding| binding.owner != count_owner.id())
        {
            return Err(LaneError::Missing(
                "overload-specific parameter and result carrier identities",
            ));
        }
        Ok(())
    }
}

/// The closed primitive record of one checker literal base.
fn checker_literal(
    base: backend_frontend_typescript::legacy::LiteralBase,
) -> SemanticTypeRecord<'static> {
    match base {
        backend_frontend_typescript::legacy::LiteralBase::Number => {
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
            record.payload0 = u32::from(PrimitiveShape::Integer);
            record.payload1 = (32_u32 << 1) | SemanticTypeRecord::INTEGER_SIGNED_FLAG;
            record
        }
        backend_frontend_typescript::legacy::LiteralBase::String => {
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
            record.payload0 = u32::from(PrimitiveShape::Str);
            record
        }
        backend_frontend_typescript::legacy::LiteralBase::Boolean => {
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
            record.payload0 = u32::from(PrimitiveShape::Bool);
            record
        }
        backend_frontend_typescript::legacy::LiteralBase::Bigint => {
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
            record.payload0 = u32::from(PrimitiveShape::Builtin);
            record.text = Some(&b"bigint"[..]);
            record
        }
    }
}

/// Byte width of the `@code`/`@link` inline-tag headers.
const TAG_WIDTH: usize = 6;

/// Finds the earliest `{@link ...}` or `{@code ...}` tag header at or after
/// `from`, returning its position and whether it is a link.
fn earliest_inline_tag(line: &[u8], from: usize) -> Option<(usize, bool)> {
    let link = find_sub(line, b"{@link", from).map(|at| (at, true));
    let code = find_sub(line, b"{@code", from).map(|at| (at, false));
    match (link, code) {
        (Some(at_link), Some(at_code)) => Some(if at_link.0 <= at_code.0 {
            at_link
        } else {
            at_code
        }),
        (found, None) | (None, found) => found,
    }
}

/// Strips one wrapping quote pair from a string-literal span.
fn strip_quotes(span: Span) -> Span {
    if span.end.saturating_sub(span.start) >= 2 {
        Span::new(span.start + 1, span.end - 1)
    } else {
        span
    }
}

/// Maps the syntax authority's mapped-member token into the frozen lattice.
fn syntax_mapped_modifier_cell(modifier: SyntaxMappedModifier) -> u32 {
    match modifier {
        SyntaxMappedModifier::Absent => u32::from(LatticeMappedModifier::Absent),
        SyntaxMappedModifier::Add => u32::from(LatticeMappedModifier::Add),
        SyntaxMappedModifier::Remove => u32::from(LatticeMappedModifier::Remove),
    }
}

/// Maps the checker protocol's independently ordered modifier vocabulary
/// into the frozen semantic lattice.  Never cast its Rust discriminant: the
/// checker uses Preserve/Add/Remove while the wire uses Add/Remove/Absent.
fn checker_mapped_modifier(modifier: CheckerMappedModifier) -> u32 {
    match modifier {
        CheckerMappedModifier::Preserve => u32::from(LatticeMappedModifier::Absent),
        CheckerMappedModifier::Add => u32::from(LatticeMappedModifier::Add),
        CheckerMappedModifier::Remove => u32::from(LatticeMappedModifier::Remove),
    }
}

/// Finds one byte substring at or after `from`, bounds-checked.
fn find_sub(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    let mut at = from;
    while at
        .checked_add(needle.len())
        .is_some_and(|end| end <= haystack.len())
    {
        let end = at + needle.len();
        if haystack.get(at..end) == Some(needle) {
            return Some(at);
        }
        at += 1;
    }
    None
}

/// Trims the given byte set from the front of `bytes`.
fn trim_bytes<'bytes>(bytes: &'bytes [u8], set: &[u8]) -> &'bytes [u8] {
    let mut result = bytes;
    while let Some((first, rest)) = result.split_first() {
        if set.contains(first) {
            result = rest;
        } else {
            break;
        }
    }
    result
}

/// Strips one JSDoc line's leading whitespace and star.
fn trim_jsdoc_line(line: &[u8]) -> &[u8] {
    let trimmed = trim_bytes(line, b" \t");
    let trimmed = match trimmed.split_first() {
        Some((b'*', rest)) => rest,
        _ => trimmed,
    };
    trim_bytes(trimmed, b" \t")
}
