//! Projects direct libclang authority facts into the canonical semantic lane.
//! Owns no scanner, token reconstruction, source guessing, or native process fallback.
//! Retains the exact libclang collection failure until the driver authority terminal.
//!
//! Lane encoding laws this lowering obeys:
//! - Emission is two-pass and declarations-first. Pass one commits every
//!   named declaration that later type rows can name — records, enums,
//!   typedefs, templates, and namespaces — each with the legal diagonal
//!   self-nominal, so every later nominal target is a strictly backward
//!   fact ordinal. Forward declarations are legal: cursors sharing one
//!   libclang USR collapse to one fact that prefers the definition, so
//!   measured layout facts come from the complete declaration.
//! - Declared types project row-by-row from the authority's recursive
//!   type graph. `int[]` becomes an `Array` row (the payload cell carries
//!   length+1, zero meaning an unmeasured length), `T*` a
//!   `Primitive(CPointer)`, `const T*` a pointer to a separate
//!   `CQualified` pointee row, and `T * const` a `CQualified` wrapper over
//!   the pointer itself,
//!   `T&`/`T&&` distinct C++ reference forms, and every function type a
//!   `FunctionPointer` row over its ordered parameter and result rows.
//!   Named uses resolve to backward nominal rows for pushed declarations,
//!   `TypeVar` rows for template parameters, and `Apply` rows for
//!   specializations; anonymous structs become `AnonymousRecord(Struct)`
//!   rows whose named children are exactly their pushed field facts.
//! - A function is `function(p, r)`: its `Parameter` facts (one per
//!   matched `CXCursor_ParamDecl`) and its non-void result carrier are
//!   pushed immediately before it, and its `FunctionPointer` row names
//!   exactly those rows. Signature positions beyond the lane's fixed
//!   child width are rejected with the exact capacity cause, never silently
//!   truncated.
//! - Qualifiers, storage, measured layout, template parameters, and the
//!   translation unit's include spellings travel only in the per-fact
//!   `ClangFacts` extension row; layout cells stay empty whenever
//!   libclang could not measure the type (incomplete or dependent).
//! - Occurrences project the authority's reference plane: resolved
//!   in-TU targets become local rows at oracle confidence; resolved
//!   project-local targets become stable references keyed on their
//!   package-relative file; resolved out-of-root system targets become
//!   stable references to one fixed system fragment keyed on the USR alone,
//!   never a host path; and unresolved sites keep their written spelling at
//!   index confidence. Occurrence owners are the pushed declarations
//!   the authority names; module-owned references have no honest owner
//!   row and are never synthesized onto another fact.
//! - Raw doxygen comments become borrowed text lines with soft breaks
//!   and `@ref`/`\ref` links; targets naming a pushed declaration link
//!   locally and every other target links to the `c` ecosystem.
//! - Positions the consumed authority surface cannot prove — typedef
//!   underlying spellings, default arguments, bit-field widths, and inline
//!   assembly bodies — stay absent or fold
//!   to typed `Unknown` rows. Never recovers C facts by scanning source
//!   text or a native parser fallback.
//! - Local C++ virtual override edges are emitted as occurrence kind 7,
//!   owned by the pushed overriding declaration and targeting its backward
//!   local base row. Foreign override edges have no honest schema-1 path
//!   representation: they are explicitly deferred to the authorized schema-2
//!   identity-cell packet rather than fabricating a path or silently dropping
//!   the edge.

use core::sync::atomic::AtomicBool;
use std::ffi::CString;

use sha2::{Digest, Sha256};

use backend_frontend_clang::ClangProject;
use backend_frontend_clang::legacy::{
    ClangInput, ClangScratch, CollectError, DeclarationFact, DeclarationId, DeclarationKind,
    DefinitionState, IncludeFact, IntegerRank, MAX_CLANG_DECLARATIONS, MAX_CLANG_DIAGNOSTICS,
    MAX_CLANG_INCLUDES, MAX_CLANG_OVERRIDES, MAX_CLANG_REFERENCES, MAX_CLANG_TYPE_EDGES,
    MAX_CLANG_TYPES, MethodVirtuality, OverrideFact, ParseFailure, ReferenceFact, ReferenceKind,
    ReferenceTarget, SYMBOL_IDENTITY_BYTES, SourceSpan, StorageClass, SymbolIdentity, TypeEdge,
    TypeFact, TypeId as AuthorityTypeId, TypeKind, TypeRelation, collect_cancellable,
};
use backend_semantic::ir::{
    ClangFacts as WireClangFacts, ClangLayout, ClangQualifiers, ClangStorageClass,
    DeclarationFamilyId, DeclarationIdentity, DocFragmentInput, DocLinkTarget, EntityId,
    EntityKind, ExternalFragmentId, ForeignKey, ForeignOrigin, NominalRef, Occurrence,
    OccurrenceConfidence, OccurrenceTarget, PackageLineage, ProductChildRole,
    ReferenceKind as LaneReferenceKind, RelSpan, SemanticProductConstructor, SemanticTypeRecord,
    SemanticTypeTag, StableRef,
    TypeParameterListId, TypeReason, TypeWidth, VariantFingerprint,
};
use backend_semantic::vocabulary::{LanguageProfile, LoweringUnsupported};

use crate::driver::lower::{
    AdmissionFault, EmissionExtension, FactSet, LEAF_PRODUCT, MAX_FACT_CHILDREN, MAX_TYPE_CHILDREN,
    SemanticFact, StagedSourceSpan, push_fact,
};
use crate::driver::types::{ClangProjectionFault, FactFault, FactRejection};

/// Exact direct-authority rejection while borrowing libclang facts.
///
/// The variant set is consumed exhaustively by the driver's terminal
/// mapping, so projection faults intentionally do not widen it: every
/// internal fault class folds to the same closed terminals through
/// [`terminal`], keeping the driver seam frozen while the operands stay
/// named at the fold site.
#[derive(Debug)]
pub(crate) enum ClangCollectError {
    /// libclang failed before yielding a complete fact image.
    Authority(CollectError),
    /// The bounded canonical declaration lane rejected a direct fact.
    Lowering(LoweringUnsupported),
    /// Canonical admission rejected one exact fact; operands retained.
    Rejected(FactRejection),
    /// Projection rejected one exact direct-authority fact; operands retained.
    Projection(ClangProjectionFault),
    /// Canonical admission rejected the completed fact image; its cause is retained.
    Admission(AdmissionFault),
}

/// The common Clang fault vocabulary is the only projection-terminal route.
///
/// It retains authority operands beyond this module; unsupported lowering is
/// reserved exclusively for a supported authority fact with no language
/// lowering recipe.
type ProjectionFault = ClangProjectionFault;

fn terminal(fault: ProjectionFault) -> ClangCollectError {
    ClangCollectError::Projection(fault)
}

/// Folds one bounded-lane fact rejection into the exact typed rejection. The
/// snapshot preserves the ordinal the fact would have occupied, the rejected
/// name's byte length when the fold site knows it (zero when the rejected
/// object carries no name), and the full typed cause — the same operands the
/// shared lane retains for direct fact rejections. A cause-erased terminal
/// here would misreport a capacity wall as an unsupported declaration.
fn lane_terminal(facts: &FactSet, name_len: usize, fault: FactFault) -> ClangCollectError {
    ClangCollectError::Rejected(FactRejection {
        fact: facts.len(),
        name_len,
        cause: fault,
    })
}

/// Admits one fact and returns its proven backward ordinal.
fn push<'source>(
    facts: &mut FactSet<'source>,
    fact: SemanticFact<'source>,
) -> Result<u32, ClangCollectError> {
    let ordinal = push_fact(facts, fact).map_err(ClangCollectError::Rejected)?;
    u32::try_from(ordinal).map_err(|_| terminal(ProjectionFault::IndexCapacity))
}

/// Producer depth budget of the recursive type projection, matched to the
/// other lanes. The authority's type graph is acyclic by construction, so
/// the budget is purely defensive.
const DEPTH_LIMIT: usize = 64;

/// Minimum declaration slots reserved for any source. Small but
/// declaration-dense translation units (macros, generated headers) must not be
/// rejected by a byte-count heuristic, so this is the recorded
/// `ScratchCapacity{Declarations:1024}` floor the real corpus previously hit.
const MIN_DECLARATION_SLOTS: usize = 1_024;
/// Minimum recursive type and type-edge slots reserved for any source.
const MIN_TYPE_SLOTS: usize = 4_096;
/// Minimum reference slots reserved for any source. This is the recorded
/// `ScratchCapacity{References:4096}` floor.
const MIN_REFERENCE_SLOTS: usize = 4_096;
/// Minimum include slots reserved for any source.
const MIN_AUXILIARY_SLOTS: usize = 1_024;

/// Measured declaration budget: at least the small-source floor, at most the
/// protocol maximum. Each declaration owns written source bytes, so entered
/// length is a safe upper bound on the count.
fn declaration_slots(source_len: usize) -> usize {
    source_len
        .saturating_add(1)
        .clamp(MIN_DECLARATION_SLOTS, MAX_CLANG_DECLARATIONS)
}

/// Measured recursive-type budget; a type row is reached through a cursor, so
/// four typed rows per entered byte is a conservative measured bound.
fn type_slots(source_len: usize) -> usize {
    source_len
        .saturating_mul(4)
        .clamp(MIN_TYPE_SLOTS, MAX_CLANG_TYPES)
}

/// Measured recursive type-edge budget.
fn type_edge_slots(source_len: usize) -> usize {
    source_len
        .saturating_mul(4)
        .clamp(MIN_TYPE_SLOTS, MAX_CLANG_TYPE_EDGES)
}

/// Measured reference budget.
fn reference_slots(source_len: usize) -> usize {
    source_len
        .saturating_mul(4)
        .clamp(MIN_REFERENCE_SLOTS, MAX_CLANG_REFERENCES)
}

/// Diagnostic slots. Diagnostics are not main-file bounded: libclang can
/// report header diagnostics against a tiny primary source, so this lane keeps
/// the full protocol reservation instead of a source-derived budget.
const DIAGNOSTIC_SLOTS: usize = MAX_CLANG_DIAGNOSTICS;

/// Measured include budget; an include cursor is recorded only when its span
/// belongs to the primary source, so at most one row per entered byte.
fn include_slots(source_len: usize) -> usize {
    source_len
        .saturating_add(1)
        .clamp(MIN_AUXILIARY_SLOTS, MAX_CLANG_INCLUDES)
}

/// Override slots. Override pairs are keyed by USR and can cross into headers
/// from a small primary source, so this lane keeps the full protocol
/// reservation instead of a source-derived budget.
const OVERRIDE_SLOTS: usize = MAX_CLANG_OVERRIDES;

/// `PrimitiveShape::Integer` wire cell (`repr(u32)` discriminant).
const SHAPE_INTEGER: u32 = 0;
/// `PrimitiveShape::Float` wire cell.
const SHAPE_FLOAT: u32 = 1;
/// `PrimitiveShape::Bool` wire cell.
const SHAPE_BOOL: u32 = 2;
/// `PrimitiveShape::CPlainSignedChar` wire cell.
const SHAPE_C_PLAIN_SIGNED_CHAR: u32 = 20;
/// `PrimitiveShape::CPlainUnsignedChar` wire cell.
const SHAPE_C_PLAIN_UNSIGNED_CHAR: u32 = 21;
/// `PrimitiveShape::CSignedChar` wire cell.
const SHAPE_C_SIGNED_CHAR: u32 = 22;
/// `PrimitiveShape::CUnsignedChar` wire cell.
const SHAPE_C_UNSIGNED_CHAR: u32 = 23;
/// `PrimitiveShape::CWideChar` wire cell for unavailable native signedness.
const SHAPE_C_WIDE_CHAR: u32 = 24;
/// `PrimitiveShape::CBlockPointer` wire cell.
const SHAPE_C_BLOCK_POINTER: u32 = 25;
/// `PrimitiveShape::CWideSignedChar` wire cell.
const SHAPE_C_WIDE_SIGNED_CHAR: u32 = 26;
/// `PrimitiveShape::CWideUnsignedChar` wire cell.
const SHAPE_C_WIDE_UNSIGNED_CHAR: u32 = 27;
/// `PrimitiveShape::Utf16CodeUnit` wire cell.
const SHAPE_UTF16_CODE_UNIT: u32 = 18;
/// `PrimitiveShape::Utf32CodeUnit` wire cell.
const SHAPE_UTF32_CODE_UNIT: u32 = 19;
/// `PrimitiveShape::Builtin` wire cell.
const SHAPE_BUILTIN: u32 = 8;
/// `PrimitiveShape::CPointer` wire cell.
const SHAPE_C_POINTER: u32 = 9;
/// `PrimitiveShape::CxxLvalueReference` wire cell.
const SHAPE_CXX_LVALUE_REFERENCE: u32 = 10;
/// `PrimitiveShape::CxxRvalueReference` wire cell.
const SHAPE_CXX_RVALUE_REFERENCE: u32 = 11;
/// `PrimitiveShape::CxxMemberPointer` wire cell.
const SHAPE_CXX_MEMBER_POINTER: u32 = 12;

/// Integer signedness bit below the shifted width cell.
const INTEGER_SIGNED_FLAG: u32 = 1;
/// Bit offset of the integer width cell above the signedness bit.
const INTEGER_WIDTH_SHIFT: u32 = 1;

/// Canonical C spelling of `long`, carried in the `PrimitiveShape::Builtin`
/// text cell because the width-bearing integer row cannot express the rank.
const C_LONG_SPELLING: &[u8] = b"long";
/// Canonical C spelling of `unsigned long`.
const C_UNSIGNED_LONG_SPELLING: &[u8] = b"unsigned long";
/// Canonical C spelling of `long long`.
const C_LONG_LONG_SPELLING: &[u8] = b"long long";
/// Canonical C spelling of `unsigned long long`.
const C_UNSIGNED_LONG_LONG_SPELLING: &[u8] = b"unsigned long long";

/// `void` builtin spelling; the void row is a primitive builtin leaf.
const VOID_SPELLING: &[u8] = b"void";

/// Nested-position coordinate meaning the pooled lane cannot host this row
/// — no anchor fact, or a full pool. Callers fold the enclosing row to the
/// typed gap instead of emitting a fabricated coordinate. Cannot collide:
/// anonymous rows live below `ANONYMOUS_ROW_BASE + MAX_ANONYMOUS_TYPE_ROWS`
/// and fact ordinals below the lane bound.
const UNHOSTABLE: u32 = u32::MAX;

/// The typed unknown row for a position the pooled lane cannot host or the
/// consumed authority surface could not classify.
fn gap() -> Projected<'static> {
    Projected::leaf(unknown_record(TypeReason::OracleGap, None))
}

/// `AnonymousRecordForm::Struct` wire cell.
const ANON_RECORD_STRUCT: u32 = 0;

/// C ecosystem name of every foreign reference key and doc link.
const ECOSYSTEM: &str = "c";

/// Doxygen reference-command openers the doc scanner splits on.
const REF_OPENERS: [&[u8]; 2] = [b"@ref ", b"\\ref "];

/// Streams every provable libclang fact — declarations, recursive types,
/// signatures, occurrences, includes, and doxygen comments — into the
/// shared fact lane.
///
/// When a project is supplied, the entry translation unit is parsed through
/// its compilation-database arguments and package root so project-local
/// `#include` closures resolve to stable cross-fragment identities. Only the
/// entry closure is analyzed; the project's other translation units are never
/// walked eagerly. Without a project, the single caller buffer is parsed under
/// the synthetic profile arguments, exactly as before.
pub(crate) fn collect<'source>(
    profile: LanguageProfile,
    project: Option<&ClangProject>,
    source: &'source [u8],
    cancelled: &AtomicBool,
    facts: &mut FactSet<'source>,
) -> Result<(), ClangCollectError> {
    match project {
        Some(project) => collect_project(project, source, cancelled, facts),
        None => {
            let input =
                ClangInput::from_profile(c"nudox-input", source, profile).map_err(|_| {
                    ClangCollectError::Lowering(LoweringUnsupported::ClangDeclarationForm)
                })?;
            collect_input(input, source, cancelled, facts)
        }
    }
}

/// Parses the project's one entry translation unit with its compilation
/// arguments and package working directory, then lowers its include and
/// reference closure through the same canonical lane.
fn collect_project<'source>(
    project: &ClangProject,
    source: &'source [u8],
    cancelled: &AtomicBool,
    facts: &mut FactSet<'source>,
) -> Result<(), ClangCollectError> {
    let invalid = || {
        ClangCollectError::Authority(CollectError::Parse {
            failure: ParseFailure::InvalidArguments,
        })
    };
    let file_name =
        CString::new(project.entry().to_string_lossy().as_bytes()).map_err(|_| invalid())?;
    let directory =
        CString::new(project.root().to_string_lossy().as_bytes()).map_err(|_| invalid())?;
    let arguments = project.arguments();
    let owned = arguments
        .iter()
        .map(|argument| CString::new(argument.as_bytes()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| invalid())?;
    let borrowed = owned.iter().map(CString::as_c_str).collect::<Vec<_>>();
    let input = ClangInput::from_database(&file_name, source, &borrowed, &directory)
        .map_err(|_| invalid())?;
    collect_input(input, source, cancelled, facts)
}

/// Collects one already-authorized database command and admits it through the
/// same canonical lane as [`crate::driver::types::compile`].
pub(crate) fn lower_database<'source, 'output>(
    input: ClangInput<'source>,
    source: &'source [u8],
    source_identity: backend_semantic::ir::SourceIdentity,
    recipe: backend_semantic::ir::RecipeFact,
    profile: LanguageProfile,
    cancelled: &AtomicBool,
    output: &'output mut [u8],
) -> Result<&'output [u8], ClangCollectError> {
    let mut facts = FactSet::new();
    collect_input(input, source, cancelled, &mut facts)?;
    super::admit(&facts, source_identity, recipe, profile, output)
        .map_err(ClangCollectError::Admission)
}

fn collect_input<'input, 'source>(
    input: ClangInput<'input>,
    source: &'source [u8],
    cancelled: &AtomicBool,
    facts: &mut FactSet<'source>,
) -> Result<(), ClangCollectError> {
    let source_len = source.len();
    let mut declarations =
        vec![empty_declaration(); declaration_slots(source_len)].into_boxed_slice();
    let mut types = vec![empty_type(); type_slots(source_len)].into_boxed_slice();
    let mut type_edges = vec![empty_type_edge(); type_edge_slots(source_len)].into_boxed_slice();
    let mut references = vec![empty_reference(); reference_slots(source_len)].into_boxed_slice();
    let mut diagnostics = vec![empty_diagnostic(); DIAGNOSTIC_SLOTS].into_boxed_slice();
    let mut includes = vec![empty_include(); include_slots(source_len)].into_boxed_slice();
    let mut overrides = vec![empty_override(); OVERRIDE_SLOTS].into_boxed_slice();
    let authority = collect_cancellable(
        input,
        ClangScratch {
            declarations: &mut declarations,
            types: &mut types,
            type_edges: &mut type_edges,
            references: &mut references,
            diagnostics: &mut diagnostics,
            includes: &mut includes,
            overrides: &mut overrides,
        },
        cancelled,
    )
    .map_err(ClangCollectError::Authority)?;
    let mut projector = Projector::new(source, authority, facts);
    projector.select_representatives()?;
    projector.push_type_anchors()?;
    projector.push_members()?;
    projector.attach_topology()?;
    projector.push_includes()?;
    projector.push_occurrences()?;
    projector.push_docs()?;
    Ok(())
}

const fn empty_span() -> SourceSpan {
    SourceSpan { start: 0, end: 0 }
}

const fn empty_declaration() -> DeclarationFact {
    DeclarationFact {
        id: DeclarationId { raw: 0 },
        kind: DeclarationKind::Unknown,
        definition: DefinitionState::Declaration,
        virtuality: MethodVirtuality::NonVirtual,
        identity: None,
        span: empty_span(),
        name: None,
        owner: None,
        documentation: None,
        storage: StorageClass::None,
        type_root: None,
        enum_underlying: None,
    }
}

const fn empty_type() -> TypeFact {
    TypeFact {
        id: AuthorityTypeId { raw: 0 },
        kind: TypeKind::Unknown,
        qualifiers: backend_frontend_clang::legacy::TypeQualifiers {
            is_const: false,
            is_volatile: false,
            is_restrict: false,
        },
        declaration: None,
        array_len: None,
        builtin: None,
        size_bits: None,
        align_bits: None,
        is_variadic: false,
    }
}

const fn empty_type_edge() -> TypeEdge {
    TypeEdge {
        source: AuthorityTypeId { raw: 0 },
        relation: TypeRelation::Pointee,
        target: AuthorityTypeId { raw: 0 },
    }
}

/// Packs direct libclang cv/restrict facts only at the vocabulary boundary.
/// The corresponding row is always a `CQualified` wrapper, so this bit cell
/// has a single canonical owner and never leaks as generic mutability.
const fn c_qualifiers(qualifiers: backend_frontend_clang::legacy::TypeQualifiers) -> u32 {
    (if qualifiers.is_const { 1 } else { 0 })
        | (if qualifiers.is_volatile { 1 << 1 } else { 0 })
        | (if qualifiers.is_restrict { 1 << 2 } else { 0 })
}

/// Validates native qualifier placement before the common lattice sees a
/// structural wrapper. `restrict` qualifies only an ordinary direct pointer;
/// const/volatile on a function or reference would have no truthful C-family
/// semantic node and therefore terminate with the original authority row.
const fn qualifiers_are_legal(
    kind: TypeKind,
    qualifiers: backend_frontend_clang::legacy::TypeQualifiers,
) -> bool {
    if qualifiers.is_restrict && !matches!(kind, TypeKind::Pointer) {
        return false;
    }
    if (qualifiers.is_const || qualifiers.is_volatile)
        && matches!(
            kind,
            TypeKind::Function | TypeKind::LvalueReference | TypeKind::RvalueReference
        )
    {
        return false;
    }
    true
}

const fn empty_reference() -> ReferenceFact {
    ReferenceFact {
        kind: ReferenceKind::Value,
        span: empty_span(),
        owner: None,
        target: ReferenceTarget::Unresolved,
    }
}

const fn empty_diagnostic() -> backend_frontend_clang::legacy::DiagnosticFact {
    backend_frontend_clang::legacy::DiagnosticFact {
        severity: backend_frontend_clang::legacy::DiagnosticSeverity::Ignored,
        location: None,
        category: 0,
        message: None,
    }
}

const fn empty_include() -> IncludeFact {
    IncludeFact {
        kind: backend_frontend_clang::legacy::SourceDependencyKind::Include,
        span: empty_span(),
        resolved: None,
    }
}

const fn empty_override() -> OverrideFact {
    OverrideFact {
        source: SymbolIdentity {
            bytes: [0; SYMBOL_IDENTITY_BYTES],
        },
        target: SymbolIdentity {
            bytes: [0; SYMBOL_IDENTITY_BYTES],
        },
        target_file: None,
    }
}

/// Maps one closed authority declaration kind onto the canonical entity kind.
/// `Template` splits through its own type: a template whose type is a
/// function type is a function template, everything else declares a record.
const fn entity_kind(kind: DeclarationKind, type_kind: Option<TypeKind>) -> Option<EntityKind> {
    match kind {
        DeclarationKind::Namespace => Some(EntityKind::Namespace),
        DeclarationKind::Macro => Some(EntityKind::Macro),
        DeclarationKind::Record => Some(EntityKind::Record),
        DeclarationKind::Enumeration => Some(EntityKind::Enum),
        DeclarationKind::Enumerator => Some(EntityKind::Variant),
        DeclarationKind::Function
        | DeclarationKind::Method
        | DeclarationKind::Constructor
        | DeclarationKind::Destructor => Some(EntityKind::Function),
        DeclarationKind::Field => Some(EntityKind::Field),
        DeclarationKind::Variable => Some(EntityKind::Static),
        DeclarationKind::Parameter => Some(EntityKind::Parameter),
        DeclarationKind::TypeAlias => Some(EntityKind::Alias),
        DeclarationKind::Template => match type_kind {
            Some(TypeKind::Function) => Some(EntityKind::Function),
            _ => Some(EntityKind::Record),
        },
        DeclarationKind::TemplateParameter | DeclarationKind::Unknown => None,
    }
}

const fn constructor(kind: EntityKind) -> SemanticProductConstructor {
    match kind {
        EntityKind::Function => SemanticProductConstructor::function(0, 0),
        EntityKind::Record | EntityKind::Namespace => SemanticProductConstructor::PRODUCT,
        EntityKind::Enum => SemanticProductConstructor::UNION,
        EntityKind::Trait => SemanticProductConstructor::INTERSECTION,
        EntityKind::Constant
        | EntityKind::Module
        | EntityKind::Field
        | EntityKind::Alias
        | EntityKind::Implementation
        | EntityKind::Variant
        | EntityKind::Static
        | EntityKind::Reexport
        | EntityKind::Parameter
        | EntityKind::Macro => LEAF_PRODUCT,
    }
}

/// Maps the authority's closed storage lattice onto the extension wire enum.
const fn wire_storage(storage: StorageClass) -> ClangStorageClass {
    match storage {
        StorageClass::None => ClangStorageClass::None,
        StorageClass::Auto => ClangStorageClass::Auto,
        StorageClass::Static => ClangStorageClass::Static,
        StorageClass::Extern => ClangStorageClass::Extern,
        StorageClass::Register => ClangStorageClass::Register,
        StorageClass::ThreadLocal => ClangStorageClass::ThreadLocal,
    }
}

/// The typed unknown record for one optional spelling. `Unknown` rows whose
/// reason carries a spelling require the text cell; the others forbid it.
const fn unknown_record(reason: TypeReason, spelling: Option<&[u8]>) -> SemanticTypeRecord<'_> {
    let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Unknown);
    record.payload0 = match reason {
        TypeReason::Unannotated => 0,
        TypeReason::DynamicallyTyped => 1,
        TypeReason::UnresolvedLocalName => 2,
        TypeReason::UnresolvedExternal => 3,
        TypeReason::TruncatedAtDepthLimit => 4,
        TypeReason::OracleGap => 5,
        TypeReason::NoIrRepresentation => 6,
    };
    record.text = spelling;
    record
}

/// The recursive terminal: a nominal row naming the fact's own ordinal.
const fn nominal_record(ordinal: u32) -> SemanticTypeRecord<'static> {
    let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Nominal);
    record.nominal = Some(NominalRef::Local(EntityId::new(ordinal)));
    record
}

/// One projected type prepared for a fact: the lattice record plus its
/// ordered child coordinates. Children name already-interned anonymous rows
/// or strictly backward fact ordinals; anonymous-record members carry their
/// member spelling in the child name cell.
struct Projected<'source> {
    record: SemanticTypeRecord<'source>,
    children: [u32; MAX_TYPE_CHILDREN],
    child_names: [Option<&'source [u8]>; MAX_TYPE_CHILDREN],
    child_count: usize,
}

impl<'source> Projected<'source> {
    /// Wraps one childless lattice record.
    const fn leaf(record: SemanticTypeRecord<'source>) -> Self {
        Self {
            record,
            children: [0; MAX_TYPE_CHILDREN],
            child_names: [None; MAX_TYPE_CHILDREN],
            child_count: 0,
        }
    }

    /// Appends one child coordinate with its optional member spelling, or
    /// `None` when the bounded type-child lane is full.
    fn child(&mut self, target: u32, name: Option<&'source [u8]>) -> Option<()> {
        let slot = self.children.get_mut(self.child_count)?;
        let name_slot = self.child_names.get_mut(self.child_count)?;
        *slot = target;
        *name_slot = name;
        self.child_count += 1;
        Some(())
    }

    /// Commits the projection onto one fact under construction.
    fn attach(self, fact: SemanticFact<'source>) -> SemanticFact<'source> {
        let mut fact = fact.typed(self.record);
        for position in 0..self.child_count {
            let target = self.children.get(position).copied().unwrap_or(0);
            let name = self.child_names.get(position).copied().flatten();
            fact = fact.type_child(target, name, 0);
        }
        fact
    }
}

/// The two-pass Clang projector over one direct authority image.
struct Projector<'authority, 'scratch, 'source> {
    source: &'source [u8],
    authority: backend_frontend_clang::legacy::ClangFacts<'scratch>,
    facts: &'authority mut FactSet<'source>,
    /// For every authority declaration, the winning declaration of its
    /// identity group: the definition when the group has one, else the
    /// first declaration. Forward declarations collapse to their winner.
    representative: Vec<Option<usize>>,
    /// Lane ordinal per authority declaration index; `None` for declarations
    /// that carry no fact of their own (template parameters, anonymous
    /// records, skipped unknowns).
    ordinals: Vec<Option<u32>>,
    /// Libclang USR identity to pushed lane ordinal, for nominal targets
    /// and occurrence resolution.
    identities: Vec<(SymbolIdentity, u32)>,
    /// Anonymous record identity to its authority declaration index.
    anonymous: Vec<(SymbolIdentity, usize)>,
    /// Template parameter identity to its borrowed spelling, for `TypeVar`
    /// rows at use sites.
    template_parameters: Vec<(SymbolIdentity, &'source [u8])>,
    /// The pooled atom list naming every include spelling, interned once.
    includes: Option<backend_semantic::ir::AtomListId>,
    /// Type-edge adjacency: edges grouped by source row, preserving
    /// authority order, with a prefix-sum index.
    edge_order: Vec<usize>,
    edge_starts: Vec<usize>,
}

// Keeps the native topology decision typed until it reaches the common
// transaction staging plane; `Option<u32>` cannot distinguish root from a
// skipped authority owner.
enum ClangParentage {
    Root,
    Bound(u32),
    Unrepresented(SymbolIdentity),
}

impl<'authority, 'scratch, 'source> Projector<'authority, 'scratch, 'source> {
    fn new(
        source: &'source [u8],
        authority: backend_frontend_clang::legacy::ClangFacts<'scratch>,
        facts: &'authority mut FactSet<'source>,
    ) -> Self {
        let declaration_count = authority.declarations.len();
        Self {
            source,
            authority,
            facts,
            representative: vec![None; declaration_count],
            ordinals: vec![None; declaration_count],
            identities: Vec::new(),
            anonymous: Vec::new(),
            template_parameters: Vec::new(),
            includes: None,
            edge_order: Vec::new(),
            edge_starts: Vec::new(),
        }
    }

    /// Borrows an exact source range, or fails with the typed span terminal.
    fn slice(&self, span: SourceSpan) -> Result<&'source [u8], ProjectionFault> {
        let start = usize::try_from(span.start).map_err(|_| ProjectionFault::Span { span })?;
        let end = usize::try_from(span.end).map_err(|_| ProjectionFault::Span { span })?;
        self.source
            .get(start..end)
            .ok_or(ProjectionFault::Span { span })
    }

    /// Borrows one declaration's exact declared-name bytes.
    fn name_of(&self, declaration: &DeclarationFact) -> Result<&'source [u8], ProjectionFault> {
        let span = declaration.name.ok_or(ProjectionFault::Nameless {
            declaration: declaration.id,
        })?;
        let bytes = self.slice(span)?;
        if bytes.is_empty() {
            return Err(ProjectionFault::Nameless {
                declaration: declaration.id,
            });
        }
        Ok(bytes)
    }

    /// The authority declaration row of one index.
    fn declaration(&self, index: usize) -> Option<&'scratch DeclarationFact> {
        self.authority.declarations.get(index)
    }

    /// The authority type row of one coordinate.
    fn type_row(&self, id: AuthorityTypeId) -> Option<&'scratch TypeFact> {
        self.authority.types.get(id.raw as usize)
    }

    /// Resolves one pushed declaration ordinal by libclang USR identity.
    fn ordinal_of(&self, identity: SymbolIdentity) -> Option<u32> {
        self.identities
            .iter()
            .find(|(known, _)| *known == identity)
            .map(|(_, ordinal)| *ordinal)
    }

    /// The authority declaration index of one anonymous record identity.
    fn anonymous_declaration(&self, identity: SymbolIdentity) -> Option<usize> {
        self.anonymous
            .iter()
            .find(|(known, _)| *known == identity)
            .map(|(_, index)| *index)
    }

    /// The reserved coordinate of the fact currently being built.
    fn anchor(&self) -> Result<u32, ProjectionFault> {
        u32::try_from(self.facts.len()).map_err(|_| ProjectionFault::Anchor)
    }

    /// Builds the type-edge adjacency index: edges grouped by source row in
    /// authority order behind a prefix-sum start table.
    fn index_edges(&mut self) -> Result<(), ProjectionFault> {
        let edge_count = self.authority.type_edges.len();
        let type_count = self.authority.types.len();
        let mut starts = vec![0_usize; type_count + 1];
        for edge in self.authority.type_edges {
            let slot = starts
                .get_mut(edge.source.raw as usize)
                .ok_or(ProjectionFault::IndexCapacity)?;
            *slot += 1;
        }
        let mut running = 0_usize;
        for start in starts.iter_mut() {
            let begin = running;
            running += *start;
            *start = begin;
        }
        let mut order = vec![0_usize; edge_count];
        let mut cursor = starts.clone();
        for (position, edge) in self.authority.type_edges.iter().enumerate() {
            let source = edge.source.raw as usize;
            let slot = cursor
                .get_mut(source)
                .ok_or(ProjectionFault::IndexCapacity)?;
            let at = *slot;
            *slot += 1;
            *order.get_mut(at).ok_or(ProjectionFault::IndexCapacity)? = position;
        }
        self.edge_order = order;
        self.edge_starts = starts;
        Ok(())
    }

    /// The ordered child type coordinates of one authority type row.
    fn edges_of(&self, source: AuthorityTypeId) -> impl Iterator<Item = AuthorityTypeId> + '_ {
        let at = source.raw as usize;
        let begin = self.edge_starts.get(at).copied().unwrap_or(0);
        let end = self.edge_starts.get(at + 1).copied().unwrap_or(begin);
        self.edge_order
            .get(begin..end)
            .unwrap_or(&[])
            .iter()
            .filter_map(|position| self.authority.type_edges.get(*position))
            .map(|edge| edge.target)
    }

    /// The first child of one authority type row under one relation.
    fn edge_of(&self, source: AuthorityTypeId, relation: TypeRelation) -> Option<AuthorityTypeId> {
        let begin = self.edge_starts.get(source.raw as usize).copied()?;
        let end = self.edge_starts.get(source.raw as usize + 1).copied()?;
        self.edge_order
            .get(begin..end)?
            .iter()
            .filter_map(|position| self.authority.type_edges.get(*position))
            .find(|edge| edge.relation == relation)
            .map(|edge| edge.target)
    }

    /// Pass zero: collapses every forward-declaration group to its winning
    /// declaration — the definition when the group has one, else the first
    /// cursor — and registers anonymous record identities.
    fn select_representatives(&mut self) -> Result<(), ClangCollectError> {
        for index in 0..self.authority.declarations.len() {
            let Some(declaration) = self.declaration(index) else {
                continue;
            };
            let mut winner = Some(index);
            if let Some(identity) = declaration.identity {
                let earlier = (0..index).find(|earlier| {
                    self.representative.get(*earlier).copied().flatten() == Some(*earlier)
                        && self.declaration(*earlier).and_then(|known| known.identity)
                            == Some(identity)
                });
                if let Some(earlier) = earlier {
                    winner = None;
                    let earlier_declaration = self.declaration(earlier);
                    let becomes_definition = declaration.definition == DefinitionState::Definition
                        && earlier_declaration
                            .is_some_and(|known| known.definition == DefinitionState::Declaration);
                    let named_over_stub = declaration.name.is_some()
                        && earlier_declaration.is_some_and(foreign_value_authority_stub);
                    if becomes_definition || named_over_stub {
                        *self
                            .representative
                            .get_mut(earlier)
                            .ok_or(terminal(ProjectionFault::IndexCapacity))? = None;
                        winner = Some(index);
                    }
                }
            }
            *self
                .representative
                .get_mut(index)
                .ok_or(terminal(ProjectionFault::IndexCapacity))? = winner;
            if winner != Some(index) {
                continue;
            }
            if declaration.kind == DeclarationKind::Record && declaration.name.is_none() {
                if let Some(identity) = declaration.identity {
                    self.anonymous.push((identity, index));
                }
            }
        }
        Ok(())
    }

    /// Pass one: every named declaration later type rows can name —
    /// records, enums, typedefs, templates, and namespaces — each with the
    /// legal diagonal self-nominal (namespaces carry no type). Template
    /// declarations also intern their template parameters into the pooled
    /// lane so their extension row names exactly them.
    fn push_type_anchors(&mut self) -> Result<(), ClangCollectError> {
        // Function-template anchors project their signature in this pass, so
        // the authority type-edge adjacency must be live before the first push.
        self.index_edges().map_err(terminal)?;
        let declarations = self.authority.declarations;
        for index in 0..declarations.len() {
            if self.representative.get(index).copied().flatten() != Some(index) {
                continue;
            }
            let Some(declaration) = declarations.get(index) else {
                continue;
            };
            if !matches!(
                declaration.kind,
                DeclarationKind::Record
                    | DeclarationKind::Enumeration
                    | DeclarationKind::TypeAlias
                    | DeclarationKind::Template
                    | DeclarationKind::Namespace
            ) {
                continue;
            }
            let Ok(name) = self.name_of(declaration) else {
                // An anchorless named declaration has no honest name cell.
                continue;
            };
            let kind = entity_kind(
                declaration.kind,
                declaration
                    .type_root
                    .and_then(|root| self.type_row(root))
                    .map(|row| row.kind),
            );
            let Some(kind) = kind else {
                continue;
            };
            // Template parameters first so the pooled start coordinate the
            // extension row carries is final before the fact is pushed.
            let template_start = self.push_template_parameters(index)?;
            let own_ordinal = u32::try_from(self.facts.len())
                .map_err(|_| terminal(ProjectionFault::IndexCapacity))?;
            let extension = self.extension(declaration, template_start)?;
            // A function template is an executable anchor, not a nominal
            // type: two same-named overloads that differ only in a parameter
            // type (for example `float` versus `double`) must still mint
            // distinct coordinate-free families. Carrying the projected
            // function signature makes the overloads structurally distinct and
            // lets every contained parameter inherit that distinction. Every
            // other anchor keeps its recursive nominal self-record.
            let fact = if kind == EntityKind::Function {
                match declaration.type_root {
                    Some(root) => {
                        let projected = self.project_root(root)?;
                        projected.attach(SemanticFact::new(kind, name, constructor(kind)))
                    }
                    None => SemanticFact::new(kind, name, constructor(kind))
                        .typed(unknown_record(TypeReason::Unannotated, None)),
                }
            } else {
                let record = match kind {
                    EntityKind::Module => unknown_record(TypeReason::Unannotated, None),
                    _ => nominal_record(own_ordinal),
                };
                SemanticFact::new(kind, name, constructor(kind)).typed(record)
            }
            .with_extension(EmissionExtension::Clang(extension));
            // A C++ class-template explicit or partial specialization differs
            // from its siblings only by template arguments, and a non-type
            // argument is erased from the type graph (its libclang type is the
            // argument's type, not its value). Two specializations of one
            // primary template therefore project byte-identical variants. The
            // authority's canonical USR-derived identity is the ground truth
            // for "distinct declaration", so it enters the variant for records
            // and templates exactly when libclang proves one.
            let fact = match declaration.identity.as_ref() {
                Some(identity)
                    if matches!(
                        declaration.kind,
                        DeclarationKind::Record | DeclarationKind::Template
                    ) =>
                {
                    fact.with_identity_discriminator(identity.bytes)
                }
                _ => fact,
            };
            let ordinal = push(self.facts, fact)?;
            self.record_pushed(index, ordinal, declaration);
            if declaration.kind == DeclarationKind::Enumeration {
                for candidate in 0..declarations.len() {
                    if self.representative.get(candidate).copied().flatten() != Some(candidate) {
                        continue;
                    }
                    let Some(enumerator) = declarations.get(candidate) else {
                        continue;
                    };
                    if enumerator.kind == DeclarationKind::Enumerator
                        && enumerator.owner == declaration.identity
                        && enumerator.owner.is_some()
                        && span_contains(declaration.span, enumerator.span)
                    {
                        self.push_enumerator(candidate)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Interns one pooled type-parameter row per named template parameter of
    /// one template declaration and returns the pooled start coordinate.
    fn push_template_parameters(
        &mut self,
        template_index: usize,
    ) -> Result<u32, ClangCollectError> {
        let start = u32::try_from(self.facts.type_parameter_len)
            .map_err(|_| terminal(ProjectionFault::IndexCapacity))?;
        let Some(template) = self.declaration(template_index) else {
            return Ok(start);
        };
        let declarations = self.authority.declarations;
        for index in 0..declarations.len() {
            if self.representative.get(index).copied().flatten() != Some(index) {
                continue;
            }
            let Some(declaration) = declarations.get(index) else {
                continue;
            };
            if declaration.kind != DeclarationKind::TemplateParameter {
                continue;
            }
            if declaration.owner != template.identity || declaration.owner.is_none() {
                continue;
            }
            if !span_contains(template.span, declaration.span) {
                continue;
            }
            let Ok(name) = self.name_of(declaration) else {
                continue;
            };
            self.facts
                .push_type_parameter(name, None, None)
                .map_err(|fault| lane_terminal(&self.facts, name.len(), fault))?;
            if let Some(identity) = declaration.identity {
                self.template_parameters.push((identity, name));
            }
        }
        Ok(start)
    }

    /// Pass two: members, values, macros, and executables in declaration
    /// order — every remaining named winner. Anonymous struct members are
    /// pushed on demand by [`Projector::project_anonymous_record`] and
    /// skipped here through the ordinal table.
    fn push_members(&mut self) -> Result<(), ClangCollectError> {
        self.index_edges().map_err(terminal)?;
        let declarations = self.authority.declarations;
        for index in 0..declarations.len() {
            if self.representative.get(index).copied().flatten() != Some(index) {
                continue;
            }
            if self.ordinals.get(index).copied().flatten().is_some() {
                continue;
            }
            let Some(declaration) = declarations.get(index) else {
                continue;
            };
            match declaration.kind {
                DeclarationKind::Record
                | DeclarationKind::Enumeration
                | DeclarationKind::TypeAlias
                | DeclarationKind::Template
                | DeclarationKind::Namespace => {}
                DeclarationKind::Enumerator => {
                    if declaration.name.is_some() {
                        self.push_enumerator(index)?;
                    }
                }
                DeclarationKind::Field => self.push_typed_member(index, EntityKind::Field)?,
                DeclarationKind::Variable => {
                    if declaration.name.is_none() {
                        continue;
                    }
                    // Block-scope variables share their function's parentage
                    // but not its block scope: two same-named same-typed
                    // locals in different blocks of one function would mint
                    // identical coordinate-free identities. They carry no
                    // stable declaration identity and are not admitted as
                    // facts. The locals of an unrepresented executable (a
                    // lambda closure, say) share that refusal — their honest
                    // parentage target has no row. File, namespace, and
                    // record scope variables keep their honest parentage.
                    if !self.owner_is_functional(declaration)
                        && !self.owner_is_unrepresented(declaration)
                    {
                        self.push_typed_member(index, EntityKind::Static)?;
                    }
                }
                DeclarationKind::Macro => self.push_macro(index)?,
                DeclarationKind::Function
                | DeclarationKind::Method
                | DeclarationKind::Constructor
                | DeclarationKind::Destructor => self.push_function(index)?,
                DeclarationKind::Parameter if !self.owner_is_unrepresented(declaration) => {
                    // Signature storage of a represented executable is pushed
                    // by that function's own signature framing; what remains
                    // here is owner-less storage (a function-pointer
                    // typedef's parameter) with file-scope parentage, and a
                    // function template's parameters (the template anchor is
                    // pushed in pass one without signature framing). Both
                    // keep the executable framing's twin discrimination, or
                    // macro-generated template operators whose parameters
                    // share the invocation-text name and a dependent frame
                    // would mint colliding identities.
                    self.push_orphan_parameter(index)?
                }
                DeclarationKind::Parameter => {}
                DeclarationKind::TemplateParameter | DeclarationKind::Unknown => {}
            }
        }
        Ok(())
    }

    /// Records one pushed declaration: its lane ordinal by declaration
    /// index and its USR identity for later nominal and owner resolution.
    fn record_pushed(&mut self, index: usize, ordinal: u32, declaration: &DeclarationFact) {
        if let Some(slot) = self.ordinals.get_mut(index) {
            *slot = Some(ordinal);
        }
        if let Some(identity) = declaration.identity {
            self.identities.push((identity, ordinal));
        }
    }

    /// True when one declaration's authoritative owner is a function-like
    /// executable. The check reads only the authority's USR identities and
    /// closed declaration kinds — never a lane ordinal or source coordinate.
    /// A function template owns its body locals exactly like a plain
    /// function does; a class template instead owns static data members,
    /// which keep their honest template parentage and are still admitted.
    fn owner_is_functional(&self, declaration: &DeclarationFact) -> bool {
        let Some(owner) = declaration.owner else {
            return false;
        };
        let Some(owner_decl) = self
            .authority
            .declarations
            .iter()
            .find(|candidate| candidate.identity == Some(owner))
        else {
            return false;
        };
        match owner_decl.kind {
            DeclarationKind::Function
            | DeclarationKind::Method
            | DeclarationKind::Constructor
            | DeclarationKind::Destructor => true,
            DeclarationKind::Template => owner_decl
                .type_root
                .and_then(|root| self.type_row(root))
                .is_some_and(|row| row.kind == TypeKind::Function),
            _ => false,
        }
    }

    /// True when one declaration's authoritative owner carries an identity
    /// that has no retained declaration row at all.
    ///
    /// That form is body storage of an executable the lane never visits as a
    /// declaration: `tinycbor`'s `tst_parser.cpp` is the measured case, where
    /// four lambda closures inside the file-scope `byteArrayOps` initializer
    /// each declare a block-scope `auto input` local and a `(void *token,
    /// size_t len)` parameter run whose semantic parent is the closure's
    /// unvisited call-operator method. Neither is file, namespace, or record
    /// scope storage: the honest parentage target has no row, and
    /// same-named same-typed locals or parameters of distinct closures would
    /// mint colliding coordinate-free identities. Only a nameless owner
    /// (`None`) is different storage — file scope — and stays admitted.
    fn owner_is_unrepresented(&self, declaration: &DeclarationFact) -> bool {
        let Some(owner) = declaration.owner else {
            return false;
        };
        !self
            .authority
            .declarations
            .iter()
            .any(|candidate| candidate.identity == Some(owner))
    }

    /// Projects libclang's authoritative owner and declaration extent rows
    /// after both declaration passes have assigned every local ordinal.  This
    /// is deliberately a relation pass, not source re-parsing: the same USR
    /// identity and half-open span that own occurrence facts own the tree.
    fn attach_topology(&mut self) -> Result<(), ClangCollectError> {
        let declarations = self.authority.declarations;
        let mut bindings = Vec::new();
        for (index, declaration) in declarations.iter().enumerate() {
            let Some(child) = self.ordinals.get(index).copied().flatten() else {
                continue;
            };
            let span = StagedSourceSpan::new(declaration.span.start, declaration.span.end)
                .ok_or_else(|| {
                    terminal(ProjectionFault::Span {
                        span: declaration.span,
                    })
                })?;
            let parentage = match declaration.owner {
                None => ClangParentage::Root,
                Some(owner) => match self.ordinal_of(owner) {
                    Some(parent) if parent != child => ClangParentage::Bound(parent),
                    // A self owner is not a root claim.  Retain the exact
                    // authority cell as an unrepresentable containment fact.
                    Some(_) | None => ClangParentage::Unrepresented(owner),
                },
            };
            bindings.push((child, parentage, span));
        }
        for (child, parentage, span) in bindings {
            match parentage {
                ClangParentage::Root => self
                    .facts
                    .mark_parentage_root(child)
                    .map_err(|fault| lane_terminal(&self.facts, 0, fault))?,
                ClangParentage::Bound(parent) => self
                    .facts
                    .attach_parent(child, parent)
                    .map_err(|fault| lane_terminal(&self.facts, 0, fault))?,
                ClangParentage::Unrepresented(owner) => self
                    .facts
                    .mark_unrepresented_parent(child, owner.bytes)
                    .map_err(|fault| lane_terminal(&self.facts, 0, fault))?,
            }
            self.facts
                .attach_source_span(child, span)
                .map_err(|fault| lane_terminal(&self.facts, 0, fault))?;
        }

        // A member set is complete exactly when every direct child named by
        // the authority has an admitted representative. Duplicate native
        // declarations may share that representative; ignored or unsupported
        // children keep the lane unavailable rather than manufacturing a
        // partial list. Synthetic signature carriers have no authority row
        // and therefore do not participate in containment completeness.
        for (index, ordinal) in self.ordinals.iter().copied().enumerate() {
            let Some(ordinal) = ordinal else { continue };
            let Some(owner) = declarations
                .get(index)
                .and_then(|declaration| declaration.identity)
            else {
                continue;
            };
            let complete = declarations
                .iter()
                .enumerate()
                .filter(|(_, declaration)| declaration.owner == Some(owner))
                .all(|(child, _)| {
                    self.representative
                        .get(child)
                        .copied()
                        .flatten()
                        .and_then(|representative| self.ordinals.get(representative))
                        .copied()
                        .flatten()
                        .is_some()
                });
            if complete {
                self.facts
                    .mark_members_captured(ordinal)
                    .map_err(|fault| lane_terminal(&self.facts, 0, fault))?;
            }
        }
        Ok(())
    }

    /// Pushes one enumerator from its owning enumeration's direct native
    /// underlying-type fact. Enum layout and C defaults are not a substitute
    /// for this authority: absent or invalid underlying type remains the
    /// typed oracle gap instead of a fabricated signed integer.
    fn push_enumerator(&mut self, index: usize) -> Result<(), ClangCollectError> {
        let declarations = self.authority.declarations;
        let Some(declaration) = declarations.get(index) else {
            return Ok(());
        };
        let Ok(name) = self.name_of(declaration) else {
            return Ok(());
        };
        let underlying = declaration.owner.and_then(|owner| {
            self.authority
                .declarations
                .iter()
                .find(|candidate| {
                    candidate.identity == Some(owner)
                        && candidate.kind == DeclarationKind::Enumeration
                })
                .and_then(|enumeration| enumeration.enum_underlying)
        });
        let projected = match underlying {
            Some(underlying) => self.project_root(underlying)?,
            None => Projected::leaf(unknown_record(TypeReason::OracleGap, None)),
        };
        let extension = self.extension(declaration, 0)?;
        let fact = projected
            .attach(SemanticFact::new(
                EntityKind::Variant,
                name,
                constructor(EntityKind::Variant),
            ))
            .with_extension(EmissionExtension::Clang(extension));
        let ordinal = push(self.facts, fact)?;
        self.record_pushed(index, ordinal, declaration);
        Ok(())
    }

    /// Pushes one field, variable, or orphan parameter with its projected
    /// declared type.
    fn push_typed_member(
        &mut self,
        index: usize,
        kind: EntityKind,
    ) -> Result<(), ClangCollectError> {
        let declarations = self.authority.declarations;
        let Some(declaration) = declarations.get(index) else {
            return Ok(());
        };
        let Ok(name) = self.name_of(declaration) else {
            return Ok(());
        };
        let projected = match declaration.type_root {
            Some(root) => self
                .project_dependent(root, declaration.owner)
                .unwrap_or(self.project_root(root)?),
            None => Projected::leaf(unknown_record(TypeReason::Unannotated, None)),
        };
        let extension = self.extension(declaration, 0)?;
        let fact = projected
            .attach(SemanticFact::new(kind, name, constructor(kind)))
            .with_extension(EmissionExtension::Clang(extension));
        let ordinal = push(self.facts, fact)?;
        self.record_pushed(index, ordinal, declaration);
        Ok(())
    }

    /// Pushes one parameter that no executable's signature framing consumed:
    /// owner-less function-pointer typedef storage, or the parameters of a
    /// function template, whose anchor is pushed in pass one without framing
    /// its own carriers.
    ///
    /// The executable framing separates two same-named parameters whose whole
    /// flattened frame is byte-identical (one macro invocation can expand a
    /// signature whose parameters share the invocation-text spelling and one
    /// dependent type) with its twin discrimination. This path applies exactly
    /// the same discrimination over the earlier same-owner parameters this
    /// pass already pushed, so template parameters of one expansion mint
    /// distinct coordinate-free identities instead of colliding.
    fn push_orphan_parameter(&mut self, index: usize) -> Result<(), ClangCollectError> {
        let declarations = self.authority.declarations;
        let Some(declaration) = declarations.get(index) else {
            return Ok(());
        };
        let Ok(name) = self.name_of(declaration) else {
            return Ok(());
        };
        // Earlier same-owner parameter facts already pushed: the twin
        // candidates the framing's discriminator counts.
        let mut carriers: Vec<u32> = Vec::new();
        for candidate in 0..index {
            if self.representative.get(candidate).copied().flatten() != Some(candidate) {
                continue;
            }
            let Some(earlier) = declarations.get(candidate) else {
                continue;
            };
            if earlier.kind != DeclarationKind::Parameter || earlier.owner != declaration.owner {
                continue;
            }
            if let Some(ordinal) = self.ordinals.get(candidate).copied().flatten() {
                carriers.push(ordinal);
            }
        }
        let projected = match declaration.type_root {
            Some(root) => self
                .project_dependent(root, declaration.owner)
                .unwrap_or(self.project_root(root)?),
            None => Projected::leaf(unknown_record(TypeReason::Unannotated, None)),
        };
        let twin_frame = (
            projected.record,
            projected.children,
            projected.child_names,
            projected.child_count,
        );
        let extension = self.extension(declaration, 0)?;
        let mut fact = projected
            .attach(SemanticFact::new(EntityKind::Parameter, name, LEAF_PRODUCT))
            .with_extension(EmissionExtension::Clang(extension));
        let twins = self.identical_carrier_twins(&carriers, &twin_frame, name);
        let fact = match (twins, declaration.identity) {
            (0, _) => fact,
            (_, Some(identity)) => fact.with_identity_discriminator(identity.bytes),
            (_, None) => {
                let mut hash = Sha256::new();
                hash.update(b"compiler.clang.signature-twin.v1\0");
                hash.update(twins.to_le_bytes());
                let mut discriminator = [0_u8; 16];
                discriminator.copy_from_slice(&hash.finalize()[..16]);
                fact.with_identity_discriminator(discriminator)
            }
        };
        let ordinal = push(self.facts, fact)?;
        self.record_pushed(index, ordinal, declaration);
        Ok(())
    }

    /// Projects a dependent field type when the authority preserves the
    /// template owner but leaves the dependent type root opaque.
    fn project_dependent(
        &self,
        type_id: AuthorityTypeId,
        owner: Option<SymbolIdentity>,
    ) -> Option<Projected<'source>> {
        if self.type_row(type_id)?.kind != TypeKind::Unknown {
            return None;
        }
        let mut parameters = self.authority.declarations.iter().filter(|declaration| {
            declaration.kind == DeclarationKind::TemplateParameter
                && declaration.owner == owner
                && owner.is_some()
        });
        let parameter = parameters.next()?;
        if parameters.next().is_some() {
            return None;
        }
        let spelling = self.name_of(parameter).ok()?;
        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::TypeVar);
        record.text = Some(spelling);
        Some(Projected::leaf(record))
    }

    /// Pushes one macro definition with the honest unwritten
    /// type: the authority proves the macro's existence and extent, never a
    /// replacement-list type.
    fn push_macro(&mut self, index: usize) -> Result<(), ClangCollectError> {
        let declarations = self.authority.declarations;
        let Some(declaration) = declarations.get(index) else {
            return Ok(());
        };
        let Ok(name) = self.name_of(declaration) else {
            return Ok(());
        };
        let extension = self.extension(declaration, 0)?;
        let fact = SemanticFact::new(EntityKind::Macro, name, constructor(EntityKind::Macro))
            .typed(unknown_record(TypeReason::Unannotated, None))
            .with_extension(EmissionExtension::Clang(extension));
        let fact = match declaration.identity {
            Some(identity) => fact.with_identity_discriminator(identity.bytes),
            None => fact,
        };
        let ordinal = push(self.facts, fact)?;
        self.record_pushed(index, ordinal, declaration);
        Ok(())
    }

    /// Pushes one function: its parameter facts (one per matched
    /// `CXCursor_ParamDecl`) and its non-void result carrier first, then the
    /// executable fact whose function product and `FunctionPointer` row name
    /// exactly those carriers. Signature positions beyond the lane's fixed
    /// child width are omitted from the constructor payload; the constructor
    /// and child counts always agree.
    fn push_function(&mut self, index: usize) -> Result<(), ClangCollectError> {
        let declarations = self.authority.declarations;
        let Some(declaration) = declarations.get(index) else {
            return Ok(());
        };
        let name = self.name_of(declaration).unwrap_or(&[]);
        if name.is_empty() {
            return Ok(());
        }

        // Matched parameters: named winners owned by this function, in
        // span order.
        let mut parameters: Vec<usize> = Vec::new();
        for candidate in 0..declarations.len() {
            if self.representative.get(candidate).copied().flatten() != Some(candidate) {
                continue;
            }
            let Some(parameter) = declarations.get(candidate) else {
                continue;
            };
            if parameter.kind != DeclarationKind::Parameter {
                continue;
            }
            if parameter.owner != declaration.identity || parameter.owner.is_none() {
                continue;
            }
            if !span_contains(declaration.span, parameter.span) {
                continue;
            }
            if parameter.name.is_none() {
                continue;
            }
            parameters.push(candidate);
        }
        parameters.sort_by_key(|candidate| {
            declarations
                .get(*candidate)
                .map(|parameter| parameter.span.start)
                .unwrap_or(u32::MAX)
        });

        // Parameter carriers first so every executable target stays backward.
        let mut signature_children: Vec<u32> = Vec::new();
        for candidate in &parameters {
            let Some(parameter) = declarations.get(*candidate) else {
                continue;
            };
            let projected = match parameter.type_root {
                Some(root) => self.project_root(root)?,
                None => Projected::leaf(unknown_record(TypeReason::Unannotated, None)),
            };
            let Ok(parameter_name) = self.name_of(parameter) else {
                continue;
            };
            let extension = self.extension(parameter, 0)?;
            let twin_frame = (
                projected.record,
                projected.children,
                projected.child_names,
                projected.child_count,
            );
            let mut fact = projected
                .attach(SemanticFact::new(
                    EntityKind::Parameter,
                    parameter_name,
                    LEAF_PRODUCT,
                ))
                .with_extension(EmissionExtension::Clang(extension));
            // Within one macro-generated signature several parameters can
            // share the invocation-text projected name and an identical type
            // graph (`void *oldp` and `void *newp` under one
            // `CTL_RO_NL_CGEN(...)` expansion), and every flattened identity
            // plane then collides. The authority's canonical USR-derived
            // identity is the ground truth for "distinct declaration", so it
            // enters the variant for such twins exactly when the collision is
            // real — the parameter's own USR when libclang proves one, and
            // otherwise the count of already-pushed identical carriers, which
            // is span-order authority structure. Every non-twin parameter
            // frames no new bytes.
            let twins =
                self.identical_carrier_twins(&signature_children, &twin_frame, parameter_name);
            let fact = match (twins, parameter.identity) {
                (0, _) => fact,
                (_, Some(identity)) => fact.with_identity_discriminator(identity.bytes),
                (_, None) => {
                    let mut hash = Sha256::new();
                    hash.update(b"compiler.clang.signature-twin.v1\0");
                    hash.update(twins.to_le_bytes());
                    let mut discriminator = [0_u8; 16];
                    discriminator.copy_from_slice(&hash.finalize()[..16]);
                    fact.with_identity_discriminator(discriminator)
                }
            };
            let ordinal = push(self.facts, fact)?;
            self.record_pushed(*candidate, ordinal, parameter);
            signature_children.push(ordinal);
        }

        // Result carrier for non-void returns, named by the function the
        // way the lane's result slots are named in every other language.
        let mut result_ordinal = None;
        if let Some(root) = declaration.type_root
            && let Some(result) = self.edge_of(root, TypeRelation::Result)
            && let Some(result_row) = self.type_row(result)
            && !result_row.is_void()
        {
            let projected = self.project_root(result)?;
            let extension = self.extension(declaration, 0)?;
            // The carrier shares the function's projected name, and a
            // macro-generated function's name is the same invocation text
            // its parameters carry, so a scalar parameter and this carrier
            // can hold identical flattened frames. The same twin
            // discrimination applies: the function's own USR when the
            // authority proves one, else the identical-carrier count.
            let twin_frame = (
                projected.record,
                projected.children,
                projected.child_names,
                projected.child_count,
            );
            let mut fact = projected
                .attach(SemanticFact::new(EntityKind::Parameter, name, LEAF_PRODUCT))
                .with_extension(EmissionExtension::Clang(extension));
            let twins = self.identical_carrier_twins(&signature_children, &twin_frame, name);
            let fact = match (twins, declaration.identity) {
                (0, _) => fact,
                (_, Some(identity)) => fact.with_identity_discriminator(identity.bytes),
                (_, None) => {
                    let mut hash = Sha256::new();
                    hash.update(b"compiler.clang.signature-twin.v1\0");
                    hash.update(twins.to_le_bytes());
                    let mut discriminator = [0_u8; 16];
                    discriminator.copy_from_slice(&hash.finalize()[..16]);
                    fact.with_identity_discriminator(discriminator)
                }
            };
            let ordinal = push(self.facts, fact)?;
            signature_children.push(ordinal);
            result_ordinal = Some(ordinal);
        }

        // The lane's fixed child width is an exact invariant, not a truncation policy.
        if signature_children.len() > MAX_FACT_CHILDREN {
            return Err(lane_terminal(
                self.facts,
                name.len(),
                FactFault::ChildCapacity,
            ));
        }
        let kept_children = signature_children;
        let kept_parameters = usize::from(result_ordinal.is_some());
        let kept_parameters = kept_children.len().saturating_sub(kept_parameters);
        let mut fact = SemanticFact::new(
            EntityKind::Function,
            name,
            SemanticProductConstructor::function(
                u32::try_from(kept_parameters)
                    .map_err(|_| terminal(ProjectionFault::IndexCapacity))?,
                u32::from(result_ordinal.is_some() && kept_children.len() > kept_parameters),
            ),
        );
        for (position, ordinal) in kept_children.iter().enumerate() {
            let role = if Some(*ordinal) == result_ordinal && position + 1 == kept_children.len() {
                ProductChildRole::FunctionResult
            } else {
                ProductChildRole::FunctionParameter
            };
            fact = fact.child(role, *ordinal);
        }
        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::FunctionPointer);
        if result_ordinal.is_some_and(|result| kept_children.contains(&result)) {
            record.payload1 = SemanticTypeRecord::FUNCTION_RESULT_COUNT_ONE;
        }
        let mut projected = Projected::leaf(record);
        for ordinal in &kept_children {
            let _ = projected.child(*ordinal, None);
        }
        let extension = self.extension(declaration, 0)?;
        let fact = projected
            .attach(fact)
            .with_extension(EmissionExtension::Clang(extension));
        // Every executable kind overloads on one name inside one owner scope:
        // `operator=` copy/move pairs, constructor overload sets, and member
        // functions can differ only in an *unnamed* parameter's type, and a
        // nameless parameter has no honest spelling, so it is never admitted
        // and the structural graph cannot separate the legal overloads. The
        // authority's canonical USR-derived identity is the ground truth for
        // "distinct declaration", so it enters the variant for every
        // executable row exactly when libclang proves one. Each executable's
        // synthetic result carrier inherits the distinction through its
        // parent's full minted identity.
        let fact = match declaration.identity {
            Some(identity)
                if matches!(
                    declaration.kind,
                    DeclarationKind::Function
                        | DeclarationKind::Method
                        | DeclarationKind::Constructor
                        | DeclarationKind::Destructor
                ) =>
            {
                fact.with_identity_discriminator(identity.bytes)
            }
            _ => fact,
        };
        let ordinal = push(self.facts, fact)?;
        self.record_pushed(index, ordinal, declaration);
        // The synthetic result carrier is contained by its function exactly
        // like a real parameter. Binding the already-minted function ordinal
        // keeps the carriers of two same-named, same-return-typed overloads
        // distinct without an ordinal or source coordinate entering identity:
        // the function's coordinate-free family is the parent.
        if let Some(carrier) = result_ordinal {
            self.facts
                .attach_parent(carrier, ordinal)
                .map_err(|fault| lane_terminal(self.facts, name.len(), fault))?;
        }
        Ok(())
    }

    /// Counts this signature's already-pushed carriers whose whole flattened
    /// frame — kind, name, declared-type record cells, and every type child —
    /// is byte-identical to the incoming `twin_frame` carrier named
    /// `parameter_name`. Carriers that differ in any committed cell never
    /// match. Anonymous staging mints fresh coordinates for genuinely
    /// identical type graphs, so the comparison is structural through the
    /// staged rows, never ordinal-exact.
    fn identical_carrier_twins(
        &self,
        carriers: &[u32],
        twin_frame: &(
            SemanticTypeRecord<'source>,
            [u32; MAX_TYPE_CHILDREN],
            [Option<&'source [u8]>; MAX_TYPE_CHILDREN],
            usize,
        ),
        parameter_name: &[u8],
    ) -> u32 {
        let (record, children, child_names, child_count) = twin_frame;
        let mut twins = 0_u32;
        for &ordinal in carriers {
            let index = u32::try_from(ordinal).unwrap_or(u32::MAX) as usize;
            let row = ordinal;
            if self.facts.kinds.get(index).copied() != Some(EntityKind::Parameter)
                || self.facts.names.get(index).copied() != Some(parameter_name)
                || !self.facts.staged_record_matches(row, record)
            {
                continue;
            }
            let Some(count) = self.facts.staged_type_child_count(row) else {
                continue;
            };
            if count != *child_count {
                continue;
            }
            let mut same = true;
            for position in 0..count {
                let Some((target, name, flags)) = self.facts.staged_type_child(row, position)
                else {
                    same = false;
                    break;
                };
                if name != child_names.get(position).copied().flatten()
                    || flags != 0
                    || !self
                        .facts
                        .staged_rows_structurally_equal(target, children[position], 8)
                {
                    same = false;
                    break;
                }
            }
            if same {
                twins += 1;
            }
        }
        twins
    }

    /// Projects one authority type row onto a fact root: the record and its
    /// ordered child coordinates, with every nested compound interned as a
    /// pooled anonymous row.
    fn project_root(
        &mut self,
        root: AuthorityTypeId,
    ) -> Result<Projected<'source>, ClangCollectError> {
        self.project_type(root, DEPTH_LIMIT)
    }

    /// Projects one authority type row into its lattice record and ordered
    /// child coordinates. Nested compound positions intern anonymous rows
    /// owned by the lane's current anchor; an anchorless lane folds the
    /// whole position to the typed oracle gap instead of a fabricated row.
    fn project_type(
        &mut self,
        type_id: AuthorityTypeId,
        depth: usize,
    ) -> Result<Projected<'source>, ClangCollectError> {
        let Some(row) = self.type_row(type_id) else {
            return Ok(Projected::leaf(unknown_record(TypeReason::OracleGap, None)));
        };
        if depth == 0 {
            return Ok(Projected::leaf(unknown_record(
                TypeReason::TruncatedAtDepthLimit,
                None,
            )));
        }
        let projected = match row.kind {
            TypeKind::Builtin => Ok(self.project_builtin(row)),
            TypeKind::Named => self.project_named(row, type_id, depth),
            TypeKind::Pointer => self.project_pointer(row, depth),
            TypeKind::BlockPointer => self.project_block_pointer(row, depth),
            TypeKind::MemberPointer => self.project_member_pointer(row, depth),
            TypeKind::LvalueReference | TypeKind::RvalueReference => {
                self.project_reference(row, depth)
            }
            TypeKind::Array => self.project_array(row, depth),
            TypeKind::Function => self.project_function_type(row, depth),
            TypeKind::Unknown => Ok(Projected::leaf(unknown_record(TypeReason::OracleGap, None))),
        }?;
        self.apply_c_qualifiers(row, projected)
    }

    /// Wraps any directly qualified authority row in one structural C-family
    /// qualifier node. The wrapper applies to exactly this row; children own
    /// their own qualifiers, which preserves `const T * const` instead of
    /// guessing pointee mutability from the outer pointer.
    fn apply_c_qualifiers(
        &mut self,
        row: &TypeFact,
        projected: Projected<'source>,
    ) -> Result<Projected<'source>, ClangCollectError> {
        let qualifiers = c_qualifiers(row.qualifiers);
        if qualifiers == 0 {
            return Ok(projected);
        }
        if !qualifiers_are_legal(row.kind, row.qualifiers) {
            return Err(terminal(ProjectionFault::IllegalQualifierTarget {
                type_id: row.id,
                kind: row.kind,
                qualifiers: row.qualifiers,
            }));
        }
        let child = self.intern_row(projected)?;
        if child == UNHOSTABLE {
            return Ok(gap());
        }
        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::CQualified);
        record.payload0 = qualifiers;
        self.finish_row(record, vec![(child, None)])
    }

    /// Projects one builtin row onto its exact width, signedness, and shape
    /// cells. Exotic builtins the classification keeps as `Other` have no
    /// spelling cell on the consumed surface and fold to the typed gap.
    fn project_builtin(&self, row: &TypeFact) -> Projected<'source> {
        let Some(builtin) = row.builtin else {
            return Projected::leaf(unknown_record(TypeReason::OracleGap, None));
        };
        let width = |bits: Option<u32>| match bits {
            Some(bits) => u32::try_from(bits).ok().map_or(None, |bits| {
                u32::try_from(TypeWidth::try_from_cell(bits).ok()?.to_cell()).ok()
            }),
            None => None,
        };
        let record = match builtin {
            backend_frontend_clang::legacy::BuiltinClass::Void => {
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
                record.payload0 = SHAPE_BUILTIN;
                record.text = Some(VOID_SPELLING);
                record
            }
            backend_frontend_clang::legacy::BuiltinClass::Bool => {
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
                record.payload0 = SHAPE_BOOL;
                record
            }
            backend_frontend_clang::legacy::BuiltinClass::PlainCharSigned
            | backend_frontend_clang::legacy::BuiltinClass::PlainCharUnsigned
            | backend_frontend_clang::legacy::BuiltinClass::SignedChar
            | backend_frontend_clang::legacy::BuiltinClass::UnsignedChar
            | backend_frontend_clang::legacy::BuiltinClass::Utf16CodeUnit
            | backend_frontend_clang::legacy::BuiltinClass::Utf32CodeUnit
            | backend_frontend_clang::legacy::BuiltinClass::WideCharSigned
            | backend_frontend_clang::legacy::BuiltinClass::WideCharUnsigned
            | backend_frontend_clang::legacy::BuiltinClass::WideCharSignednessUnavailable => {
                let Some(width) = width(row.size_bits) else {
                    return Projected::leaf(unknown_record(TypeReason::OracleGap, None));
                };
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
                record.payload0 = match builtin {
                    backend_frontend_clang::legacy::BuiltinClass::PlainCharSigned => {
                        SHAPE_C_PLAIN_SIGNED_CHAR
                    }
                    backend_frontend_clang::legacy::BuiltinClass::PlainCharUnsigned => {
                        SHAPE_C_PLAIN_UNSIGNED_CHAR
                    }
                    backend_frontend_clang::legacy::BuiltinClass::SignedChar => SHAPE_C_SIGNED_CHAR,
                    backend_frontend_clang::legacy::BuiltinClass::UnsignedChar => {
                        SHAPE_C_UNSIGNED_CHAR
                    }
                    backend_frontend_clang::legacy::BuiltinClass::Utf16CodeUnit => {
                        SHAPE_UTF16_CODE_UNIT
                    }
                    backend_frontend_clang::legacy::BuiltinClass::Utf32CodeUnit => {
                        SHAPE_UTF32_CODE_UNIT
                    }
                    backend_frontend_clang::legacy::BuiltinClass::WideCharSigned => {
                        SHAPE_C_WIDE_SIGNED_CHAR
                    }
                    backend_frontend_clang::legacy::BuiltinClass::WideCharUnsigned => {
                        SHAPE_C_WIDE_UNSIGNED_CHAR
                    }
                    backend_frontend_clang::legacy::BuiltinClass::WideCharSignednessUnavailable => {
                        SHAPE_C_WIDE_CHAR
                    }
                    backend_frontend_clang::legacy::BuiltinClass::Void
                    | backend_frontend_clang::legacy::BuiltinClass::Bool
                    | backend_frontend_clang::legacy::BuiltinClass::Integer { .. }
                    | backend_frontend_clang::legacy::BuiltinClass::Float
                    | backend_frontend_clang::legacy::BuiltinClass::Other => {
                        unreachable!("character class matched above")
                    }
                };
                record.payload1 = width;
                record
            }
            backend_frontend_clang::legacy::BuiltinClass::Integer { signed, rank } => {
                let Some(width) = width(row.size_bits) else {
                    return Projected::leaf(unknown_record(TypeReason::OracleGap, None));
                };
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
                match rank {
                    // `short`, `int`, and `__int128` have a width that no
                    // sibling C rank shares on any supported ABI, so the
                    // width/signedness cell already proves their exact
                    // identity.  `long` and `long long` do not: they share a
                    // width and signedness on LP64 (and `int`/`long` do on
                    // LLP64, which this spelling separates as well).  Those
                    // ranks carry their canonical C spelling, the exact fact
                    // the closed spelling-bearing builtin shape exists to own,
                    // while the measured width stays in the free payload1 cell
                    // so the owned-IR conversion never assumes an LP64 ABI.
                    IntegerRank::Short | IntegerRank::Int | IntegerRank::Int128 => {
                        record.payload0 = SHAPE_INTEGER;
                        record.payload1 = (width << INTEGER_WIDTH_SHIFT)
                            | u32::from(signed) * INTEGER_SIGNED_FLAG;
                    }
                    IntegerRank::Long => {
                        record.payload0 = SHAPE_BUILTIN;
                        record.text = Some(if signed {
                            C_LONG_SPELLING
                        } else {
                            C_UNSIGNED_LONG_SPELLING
                        });
                        record.payload1 = width;
                    }
                    IntegerRank::LongLong => {
                        record.payload0 = SHAPE_BUILTIN;
                        record.text = Some(if signed {
                            C_LONG_LONG_SPELLING
                        } else {
                            C_UNSIGNED_LONG_LONG_SPELLING
                        });
                        record.payload1 = width;
                    }
                }
                record
            }
            backend_frontend_clang::legacy::BuiltinClass::Float => {
                let Some(width) = width(row.size_bits) else {
                    return Projected::leaf(unknown_record(TypeReason::OracleGap, None));
                };
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
                record.payload0 = SHAPE_FLOAT;
                record.payload1 = width;
                record
            }
            backend_frontend_clang::legacy::BuiltinClass::Other => {
                return Projected::leaf(unknown_record(TypeReason::OracleGap, None));
            }
        };
        Projected::leaf(record)
    }

    /// Projects one named row: a pushed declaration becomes its backward
    /// nominal row, a template parameter its `TypeVar` row, an anonymous
    /// record its member row, a resolved specialization its `Apply` row over
    /// the backward base and the projected argument rows, an out-of-root alias
    /// with a measured canonical builtin its underlying primitive row, and
    /// every other target the typed unresolved gap.
    fn project_named(
        &mut self,
        row: &TypeFact,
        type_id: AuthorityTypeId,
        depth: usize,
    ) -> Result<Projected<'source>, ClangCollectError> {
        let Some(identity) = row.declaration else {
            return Ok(gap());
        };
        if let Some(ordinal) = self.ordinal_of(identity) {
            let arguments: Vec<AuthorityTypeId> = self.edges_of(type_id).collect();
            if arguments.is_empty() {
                return Ok(Projected::leaf(nominal_record(ordinal)));
            }
            // A specialization: `Apply` over the backward base nominal and
            // every projected template-argument row.
            let mut children = vec![(ordinal, None)];
            for argument in arguments {
                match self.project_child(argument, depth)? {
                    UNHOSTABLE => return Ok(gap()),
                    coordinate => children.push((coordinate, None)),
                }
            }
            return self.finish_row(SemanticTypeRecord::leaf(SemanticTypeTag::Apply), children);
        }
        if let Some(spelling) = self
            .template_parameters
            .iter()
            .find(|(known, _)| *known == identity)
            .map(|(_, spelling)| *spelling)
        {
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::TypeVar);
            record.text = Some(spelling);
            return Ok(Projected::leaf(record));
        }
        if let Some(parameter) = self.authority.declarations.iter().find(|declaration| {
            declaration.kind == DeclarationKind::TemplateParameter
                && declaration.identity == Some(identity)
        }) {
            if let Ok(spelling) = self.name_of(parameter) {
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::TypeVar);
                record.text = Some(spelling);
                return Ok(Projected::leaf(record));
            }
        }
        if let Some(anonymous_index) = self.anonymous_declaration(identity) {
            return self.project_anonymous_record(anonymous_index, depth);
        }
        // An out-of-root named type has no local declaration row, but an alias
        // that canonicalizes to a builtin keeps that closed underlying class
        // and measured width. Projecting it keeps two overloads on distinct
        // typedefs (`uint16_t` versus `uint32_t`) structurally distinct instead
        // of collapsing every unresolved name to one shared unknown row.
        if row.builtin.is_some() {
            return Ok(self.project_builtin(row));
        }
        // An out-of-root named type has no local declaration row, but libclang
        // still proves its USR. Emitting a declaration-identified external
        // nominal keeps two overloads on genuinely distinct out-of-root types
        // structurally distinct instead of collapsing both to one unknown row.
        // An identity that does have an authority declaration row but has not
        // been projected yet keeps the existing typed gap rather than becoming
        // a fabricated external target.
        if self
            .authority
            .declarations
            .iter()
            .any(|declaration| declaration.identity == Some(identity))
        {
            return Ok(gap());
        }
        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Nominal);
        record.nominal = Some(NominalRef::Stable(StableRef {
            fragment: ExternalFragmentId::from_canonical_bytes(SYSTEM_FRAGMENT_DOMAIN),
            declaration: DeclarationIdentity {
                family: DeclarationFamilyId::from_canonical_bytes(&identity.bytes),
                variant: VariantFingerprint::from_canonical_bytes(&identity.bytes),
            },
        }));
        Ok(Projected::leaf(record))
    }

    /// Projects one C-family pointer row. The common wrapper around this
    /// record owns direct pointer cv qualifiers; the child independently
    /// retains any pointee qualifier.
    fn project_pointer(
        &mut self,
        row: &TypeFact,
        depth: usize,
    ) -> Result<Projected<'source>, ClangCollectError> {
        let Some(pointee) = self.edge_of(row.id, TypeRelation::Pointee) else {
            return Ok(gap());
        };
        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
        record.payload0 = SHAPE_C_POINTER;
        let child = match self.project_child(pointee, depth)? {
            UNHOSTABLE => return Ok(gap()),
            coordinate => coordinate,
        };
        self.finish_row(record, vec![(child, None)])
    }

    /// Projects an Objective-C block pointer without collapsing it into the
    /// ordinary C pointer form. The later declarator dialect receives one
    /// exact structural node and owns `^` placement.
    fn project_block_pointer(
        &mut self,
        row: &TypeFact,
        depth: usize,
    ) -> Result<Projected<'source>, ClangCollectError> {
        let Some(pointee) = self.edge_of(row.id, TypeRelation::Pointee) else {
            return Ok(gap());
        };
        let child = match self.project_child(pointee, depth)? {
            UNHOSTABLE => return Ok(gap()),
            coordinate => coordinate,
        };
        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
        record.payload0 = SHAPE_C_BLOCK_POINTER;
        self.finish_row(record, vec![(child, None)])
    }

    /// Projects a C++ reference category over its referent. This path never
    /// converts `T&&` into the Rust borrow mutability bit.
    fn project_reference(
        &mut self,
        row: &TypeFact,
        depth: usize,
    ) -> Result<Projected<'source>, ClangCollectError> {
        let Some(referent) = self.edge_of(row.id, TypeRelation::Referent) else {
            return Ok(gap());
        };
        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
        record.payload0 = if row.kind == TypeKind::RvalueReference {
            SHAPE_CXX_RVALUE_REFERENCE
        } else {
            SHAPE_CXX_LVALUE_REFERENCE
        };
        let child = match self.project_child(referent, depth)? {
            UNHOSTABLE => return Ok(gap()),
            coordinate => coordinate,
        };
        self.finish_row(record, vec![(child, None)])
    }

    /// Projects a C++ member pointer with ordered owner then member type.
    /// Missing either authority edge is an exact projection gap; a member
    /// pointer must never silently become an ordinary raw pointer.
    ///
    /// The owner's class-ness is checked against what the authority actually
    /// proved, because C++ admits a member pointer only into a complete class
    /// type and libclang guarantees the class operand of a
    /// `CXType_MemberPointer` is that class. An owner whose USR carries an
    /// authority declaration row of record kind is the in-root class. An
    /// owner whose USR has no declaration row is a class defined outside
    /// the walked translation unit — `fmt`'s
    /// `int (testing::TestSuite::*)() const` parameter is the
    /// measured case: the class lives in the included `gtest.h`, whose
    /// declarations are not children of the walked translation unit, while
    /// the type plane still carries its USR. The shared build admits only a
    /// local record nominal as a member-pointer owner, so that form has no
    /// representable owner row and the whole member pointer keeps the lane's
    /// typed projection gap instead of a fabricated shape or a whole-source
    /// terminal. Every other owner kind stays the illegal-owner
    /// terminal the original guard existed for.
    fn project_member_pointer(
        &mut self,
        row: &TypeFact,
        depth: usize,
    ) -> Result<Projected<'source>, ClangCollectError> {
        let Some(owner) = self.edge_of(row.id, TypeRelation::MemberOwner) else {
            return Ok(gap());
        };
        let Some(member) = self.edge_of(row.id, TypeRelation::Pointee) else {
            return Ok(gap());
        };
        let Some(owner_row) = self.type_row(owner) else {
            return Ok(gap());
        };
        let declared_record = owner_row.declaration.is_some_and(|identity| {
            self.authority.declarations.iter().any(|declaration| {
                declaration.identity == Some(identity)
                    && declaration.kind == DeclarationKind::Record
            })
        });
        // A USR that has no authority declaration row at all is a class
        // defined outside the walked translation unit; one that has a row of
        // another kind names a non-class entity in-root and stays illegal.
        let out_of_root_class = owner_row.declaration.is_some_and(|identity| {
            !self
                .authority
                .declarations
                .iter()
                .any(|declaration| declaration.identity == Some(identity))
        });
        if owner_row.kind != TypeKind::Named {
            return Err(terminal(ProjectionFault::IllegalMemberPointerOwner {
                pointer: row.id,
                owner,
                kind: owner_row.kind,
                declaration: owner_row.declaration,
            }));
        }
        if !declared_record {
            if !out_of_root_class {
                return Err(terminal(ProjectionFault::IllegalMemberPointerOwner {
                    pointer: row.id,
                    owner,
                    kind: owner_row.kind,
                    declaration: owner_row.declaration,
                }));
            }
            return Ok(gap());
        }
        let owner = match self.project_child(owner, depth)? {
            UNHOSTABLE => return Ok(gap()),
            coordinate => coordinate,
        };
        let member = match self.project_child(member, depth)? {
            UNHOSTABLE => return Ok(gap()),
            coordinate => coordinate,
        };
        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
        record.payload0 = SHAPE_CXX_MEMBER_POINTER;
        self.finish_row(record, vec![(owner, None), (member, None)])
    }

    /// Projects one array row with a typed fixed extent or an explicit
    /// incomplete/dependent extent. No sentinel length shares either state.
    fn project_array(
        &mut self,
        row: &TypeFact,
        depth: usize,
    ) -> Result<Projected<'source>, ClangCollectError> {
        let Some(element) = self.edge_of(row.id, TypeRelation::Element) else {
            return Ok(gap());
        };
        let record = match row.array_len {
            Some(length) => {
                let bytes = length.to_le_bytes();
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::ArrayFixed);
                record.payload0 = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                record.payload1 = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
                record
            }
            None => SemanticTypeRecord::leaf(SemanticTypeTag::ArrayIncomplete),
        };
        let child = match self.project_child(element, depth)? {
            UNHOSTABLE => return Ok(gap()),
            coordinate => coordinate,
        };
        self.finish_row(record, vec![(child, None)])
    }

    /// Projects one function type: the structural `FunctionPointer` row over
    /// its projected parameter rows and its non-void result row, the result
    /// marked in the reserved payload cell.
    fn project_function_type(
        &mut self,
        row: &TypeFact,
        depth: usize,
    ) -> Result<Projected<'source>, ClangCollectError> {
        let mut children: Vec<(u32, Option<&'source [u8]>)> = Vec::new();
        let edges: Vec<AuthorityTypeId> = self.edges_of(row.id).collect();
        for edge in edges {
            if self.edge_relation(row.id, edge) == Some(TypeRelation::Parameter) {
                match self.project_child(edge, depth)? {
                    UNHOSTABLE => return Ok(gap()),
                    coordinate => children.push((coordinate, None)),
                }
            }
        }
        let mut has_result = false;
        if let Some(result) = self.edge_of(row.id, TypeRelation::Result) {
            let is_void = self.type_row(result).is_some_and(|result| result.is_void());
            if !is_void {
                match self.project_child(result, depth)? {
                    UNHOSTABLE => return Ok(gap()),
                    coordinate => {
                        children.push((coordinate, None));
                        has_result = true;
                    }
                }
            }
        }
        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::FunctionPointer);
        if row.is_variadic {
            record.payload0 = SemanticTypeRecord::FUNCTION_C_VARIADIC_FLAG;
        }
        if has_result {
            record.payload1 = SemanticTypeRecord::FUNCTION_RESULT_COUNT_ONE;
        }
        self.finish_row(record, children)
    }

    /// The relation of one direct edge, for ordered signature projections.
    fn edge_relation(
        &self,
        source: AuthorityTypeId,
        target: AuthorityTypeId,
    ) -> Option<TypeRelation> {
        let begin = self.edge_starts.get(source.raw as usize).copied()?;
        let end = self.edge_starts.get(source.raw as usize + 1).copied()?;
        self.edge_order
            .get(begin..end)?
            .iter()
            .filter_map(|position| self.authority.type_edges.get(*position))
            .find(|edge| edge.source == source && edge.target == target)
            .map(|edge| edge.relation)
    }

    /// Projects one type row for a nested position: an already-namable row
    /// keeps its record, every other row interns as a pooled anonymous row
    /// owned by the lane's reserved next fact and returns its coordinate.
    fn project_child(
        &mut self,
        type_id: AuthorityTypeId,
        depth: usize,
    ) -> Result<u32, ClangCollectError> {
        let projected = self.project_type(type_id, depth)?;
        if let Some(NominalRef::Local(entity)) = projected.record.nominal {
            return Ok(entity.raw);
        }
        self.intern_row(projected)
    }

    /// Interns one projected row into the pooled anonymous lane, appending
    /// its children before the parent row so the pooled lane stays
    /// topologically backward. Every admission fault crosses the language
    /// boundary intact rather than being collapsed into an unhostable row.
    fn intern_row(&mut self, projected: Projected<'source>) -> Result<u32, ClangCollectError> {
        let anchor = self.anchor().map_err(terminal)?;
        for position in 0..projected.child_count {
            let target = projected.children.get(position).copied().unwrap_or(0);
            let name = projected.child_names.get(position).copied().flatten();
            self.facts
                .anonymous_type_child(target, name, 0)
                .map_err(|fault| lane_terminal(self.facts, name.map_or(0, <[u8]>::len), fault))?;
        }
        self.facts
            .intern_reserved_anchor_type_row(anchor, projected.record)
            .map_err(|fault| lane_terminal(self.facts, 0, fault))
    }

    /// Finishes one compound row over its ordered children. A row wider than
    /// the type-child lane is a typed capacity fault, never an oracle gap
    /// and never a partial row.
    fn finish_row(
        &self,
        record: SemanticTypeRecord<'source>,
        children: Vec<(u32, Option<&'source [u8]>)>,
    ) -> Result<Projected<'source>, ClangCollectError> {
        if children.len() > MAX_TYPE_CHILDREN {
            return Err(lane_terminal(self.facts, 0, FactFault::TypeChildCapacity));
        }
        let mut projected = Projected::leaf(record);
        for (target, name) in children {
            if projected.child(target, name).is_none() {
                return Err(lane_terminal(
                    self.facts,
                    name.map_or(0, <[u8]>::len),
                    FactFault::TypeChildCapacity,
                ));
            }
        }
        Ok(projected)
    }

    /// Projects one anonymous record: its named member fields are pushed as
    /// facts on demand (so the row's children stay backward), then the
    /// `AnonymousRecord(Struct)` row is returned over those field ordinals.
    /// Reused anonymous records intern their nested row once.
    fn project_anonymous_record(
        &mut self,
        declaration_index: usize,
        depth: usize,
    ) -> Result<Projected<'source>, ClangCollectError> {
        let Some(record) = self.declaration(declaration_index) else {
            return Ok(Projected::leaf(unknown_record(TypeReason::OracleGap, None)));
        };
        if depth == 0 {
            return Ok(Projected::leaf(unknown_record(
                TypeReason::TruncatedAtDepthLimit,
                None,
            )));
        }
        let declarations = self.authority.declarations;
        let mut members: Vec<usize> = Vec::new();
        for candidate in 0..declarations.len() {
            if self.representative.get(candidate).copied().flatten() != Some(candidate) {
                continue;
            }
            let Some(member) = declarations.get(candidate) else {
                continue;
            };
            if member.kind != DeclarationKind::Field {
                continue;
            }
            if member.owner != record.identity || member.owner.is_none() {
                continue;
            }
            if !span_contains(record.span, member.span) {
                continue;
            }
            members.push(candidate);
        }
        members.sort_by_key(|candidate| {
            declarations
                .get(*candidate)
                .map(|member| member.span.start)
                .unwrap_or(u32::MAX)
        });
        // Push every not-yet-pushed member field so the row's children stay
        // strictly backward, then build the member row over their ordinals.
        for candidate in &members {
            if self.ordinals.get(*candidate).copied().flatten().is_some() {
                continue;
            }
            self.push_typed_member(*candidate, EntityKind::Field)?;
        }
        let mut projected = Projected::leaf({
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::AnonymousRecord);
            record.payload0 = ANON_RECORD_STRUCT;
            record
        });
        for candidate in &members {
            let Some(member) = declarations.get(*candidate) else {
                continue;
            };
            let Some(ordinal) = self.ordinals.get(*candidate).copied().flatten() else {
                continue;
            };
            let Ok(member_name) = self.name_of(member) else {
                continue;
            };
            if projected.child(ordinal, Some(member_name)).is_none() {
                return Err(lane_terminal(
                    self.facts,
                    member_name.len(),
                    FactFault::TypeChildCapacity,
                ));
            }
        }
        Ok(projected)
    }

    /// Builds the per-declaration Clang extension row: the declared type's
    /// qualifiers and measured layout, the storage binding, the pooled
    /// template-parameter start, and the shared include-spelling list.
    fn extension(
        &mut self,
        declaration: &DeclarationFact,
        template_start: u32,
    ) -> Result<WireClangFacts, ClangCollectError> {
        let includes = self.includes_list()?;
        let (qualifiers, size_bits, align_bits) = declaration
            .type_root
            .and_then(|root| self.type_row(root))
            .map_or(
                (
                    backend_frontend_clang::legacy::TypeQualifiers {
                        is_const: false,
                        is_volatile: false,
                        is_restrict: false,
                    },
                    None,
                    None,
                ),
                |row| (row.qualifiers, row.size_bits, row.align_bits),
            );
        Ok(WireClangFacts {
            qualifiers: ClangQualifiers {
                is_const: qualifiers.is_const,
                is_volatile: qualifiers.is_volatile,
                is_restrict: qualifiers.is_restrict,
            },
            storage: wire_storage(declaration.storage),
            layout: ClangLayout {
                size_bits,
                align_bits,
            },
            templates: TypeParameterListId::new(template_start),
            includes,
        })
    }

    /// Interns the translation unit's include spellings once: every include
    /// directive's delimited path borrowed from the authority's own span.
    fn includes_list(&mut self) -> Result<backend_semantic::ir::AtomListId, ClangCollectError> {
        if let Some(interned) = self.includes {
            return Ok(interned);
        }
        let mut atoms = Vec::new();
        for include in self.authority.includes {
            let Some(spelling) = include_spelling(self.source, include) else {
                continue;
            };
            let atom = self
                .facts
                .intern_atom(spelling)
                .map_err(|fault| lane_terminal(&self.facts, spelling.len(), fault))?;
            atoms.push(atom);
        }
        let interned = self
            .facts
            .intern_atom_list(&atoms)
            .map_err(|fault| lane_terminal(&self.facts, 0, fault))?;
        self.includes = Some(interned);
        Ok(interned)
    }

    /// Streams every retained include directive into the lane as one
    /// file-scope module row plus the Import occurrence that names the
    /// included path.
    ///
    /// The delimited path spelling is the only target text libclang proves
    /// for a directive, so it is both the module row's name and the
    /// path-keyed foreign key; a directive whose extent does not carry the
    /// closed delimited form stays absent. One uniquely spelled path is one
    /// module row: libclang emits a directive cursor per textual occurrence,
    /// and a repeated directive would otherwise mint a second module with a
    /// colliding identity. The occurrence rides the first directive, whose
    /// extent is the row's captured span; a later directive's occurrence
    /// would escape that extent and stays absent rather than wrapping.
    fn push_includes(&mut self) -> Result<(), ClangCollectError> {
        let includes = self.authority.includes;
        let mut seen: Vec<&'source [u8]> = Vec::new();
        for include in includes {
            let Some((spelling, path_span)) = include_spelling_span(self.source, include) else {
                continue;
            };
            let Some(target) = foreign_universe_key(spelling, Some(EntityKind::Module)) else {
                continue;
            };
            let relative = owner_relative_span(include.span, path_span);
            let first = !seen.iter().any(|known| *known == spelling);
            if first {
                seen.push(spelling);
                let fact = SemanticFact::new(
                    EntityKind::Module,
                    spelling,
                    constructor(EntityKind::Module),
                )
                .typed(unknown_record(TypeReason::Unannotated, None));
                let ordinal = push(self.facts, fact)?;
                let span = StagedSourceSpan::new(include.span.start, include.span.end)
                    .ok_or_else(|| terminal(ProjectionFault::Span { span: include.span }))?;
                self.facts
                    .attach_source_span(ordinal, span)
                    .map_err(|fault| lane_terminal(&self.facts, spelling.len(), fault))?;
                self.facts
                    .mark_parentage_root(ordinal)
                    .map_err(|fault| lane_terminal(&self.facts, spelling.len(), fault))?;
                // A preprocessor directive has no documentation comment; the
                // lane's captured-empty contract applies to every row.
                self.facts
                    .mark_documentation_captured(ordinal)
                    .map_err(|fault| lane_terminal(&self.facts, spelling.len(), fault))?;
                if let Some(relative) = relative {
                    self.facts
                        .push_occurrence(
                            ordinal,
                            Occurrence {
                                target,
                                kind: LaneReferenceKind::Import,
                                confidence: OccurrenceConfidence::Import,
                                span: relative,
                            },
                        )
                        .map_err(|fault| lane_terminal(&self.facts, spelling.len(), fault))?;
                }
            }
        }
        Ok(())
    }

    /// Streams every reference fact into the occurrence lane: an in-TU
    /// target folds to its local ordinal and a resolved external target to
    /// the `c` universe, both at oracle confidence; an unresolved site keeps
    /// its written spelling at index confidence. References without a
    /// pushed owner have no honest owner row and stay absent.
    fn push_occurrences(&mut self) -> Result<(), ClangCollectError> {
        let references = self.authority.references;
        for reference in references {
            let Some(owner) = self.reference_owner(reference) else {
                continue;
            };
            let Ok(written) = self.slice(reference.span) else {
                continue;
            };
            let Some(owner_span) = self.owner_span(owner) else {
                continue;
            };
            let Some(span) = owner_relative_span(owner_span, reference.span) else {
                continue;
            };
            let member_field = (reference.kind == ReferenceKind::Member)
                .then(|| member_field_name_bytes(self.source, reference.span, written))
                .flatten()
                .map(|name_bytes| {
                    (
                        name_bytes,
                        member_field_name_span(self.source, reference.span, written),
                    )
                });
            if matches!(
                reference.kind,
                ReferenceKind::Call | ReferenceKind::Type | ReferenceKind::Template
            ) || member_field.is_some()
            {
                if let ReferenceTarget::Foreign {
                    path: Some(slot), ..
                } = reference.target
                {
                    if let Some(package_path) = self.authority.project_paths.get(slot as usize) {
                        if PackageLineage::new(ECOSYSTEM, package_path.as_ref()).is_ok() {
                            let (name_bytes, site_span) = match reference.kind {
                                ReferenceKind::Call => (
                                    member_call_callee_name(self.source, reference.span)
                                        .unwrap_or(written),
                                    member_call_callee_span(self.source, reference.span)
                                        .unwrap_or(reference.span),
                                ),
                                ReferenceKind::Type | ReferenceKind::Template => (
                                    type_reference_name_bytes(written).unwrap_or(written),
                                    type_reference_name_span(self.source, reference.span, written),
                                ),
                                ReferenceKind::Member => member_field.expect("member field"),
                                _ => unreachable!(),
                            };
                            let Some(span) = owner_relative_span(owner_span, site_span) else {
                                continue;
                            };
                            let entity_kind = match reference.kind {
                                ReferenceKind::Call => Some(EntityKind::Function),
                                ReferenceKind::Type | ReferenceKind::Template => {
                                    Some(EntityKind::Record)
                                }
                                ReferenceKind::Member => Some(EntityKind::Field),
                                _ => unreachable!(),
                            };
                            if let Ok(name) = core::str::from_utf8(name_bytes) {
                                self.facts
                                    .push_owned_package_occurrence(
                                        owner,
                                        ECOSYSTEM,
                                        package_path.as_ref(),
                                        name,
                                        name,
                                        entity_kind,
                                        lane_reference_kind(reference.kind),
                                        OccurrenceConfidence::Oracle,
                                        span,
                                    )
                                    .map_err(|fault| lane_terminal(&self.facts, 0, fault))?;
                                continue;
                            }
                        }
                    }
                }
            }
            if reference.kind == ReferenceKind::Value {
                if let ReferenceTarget::Foreign {
                    identity,
                    path: Some(slot),
                    ..
                } = reference.target
                {
                    if let Some(package_path) = self.authority.project_paths.get(slot as usize) {
                        if PackageLineage::new(ECOSYSTEM, package_path.as_ref()).is_ok() {
                            let entity_kind = self
                                .authority
                                .declarations
                                .iter()
                                .find(|declaration| declaration.identity == Some(identity))
                                .and_then(|declaration| match declaration.kind {
                                    DeclarationKind::Variable => Some(EntityKind::Static),
                                    DeclarationKind::Enumerator => Some(EntityKind::Variant),
                                    DeclarationKind::Function | DeclarationKind::Method => {
                                        Some(EntityKind::Function)
                                    }
                                    DeclarationKind::Constructor
                                    | DeclarationKind::Destructor => None,
                                    _ => None,
                                });
                            if let Some(entity_kind) = entity_kind {
                                if let Some(ordinal) = self.ordinal_of(identity) {
                                    self.facts
                                        .push_occurrence(
                                            owner,
                                            Occurrence {
                                                target: OccurrenceTarget::Local(
                                                    EntityId::new(ordinal),
                                                ),
                                                kind: lane_reference_kind(reference.kind),
                                                confidence: OccurrenceConfidence::Oracle,
                                                span,
                                            },
                                        )
                                        .map_err(|fault| {
                                            lane_terminal(&self.facts, 0, fault)
                                        })?;
                                    continue;
                                }
                                if is_source_identifier(written) {
                                    if let Ok(name) = core::str::from_utf8(written) {
                                        self.facts
                                            .push_owned_package_occurrence(
                                                owner,
                                                ECOSYSTEM,
                                                package_path.as_ref(),
                                                name,
                                                name,
                                                Some(entity_kind),
                                                lane_reference_kind(reference.kind),
                                                OccurrenceConfidence::Oracle,
                                                span,
                                            )
                                            .map_err(|fault| {
                                                lane_terminal(&self.facts, 0, fault)
                                            })?;
                                        continue;
                                    }
                                }
                            }
                        }
                    }
                }
            }
            let target = match reference.target {
                ReferenceTarget::Local(identity) => match self.ordinal_of(identity) {
                    Some(ordinal) => Some((
                        OccurrenceTarget::Local(EntityId::new(ordinal)),
                        OccurrenceConfidence::Oracle,
                    )),
                    // Block-scope variables are not admitted as declaration
                    // facts, but the authority still proves the reference
                    // site. Keep the written spelling at index confidence
                    // instead of dropping the occurrence.
                    None => foreign_universe(written, reference.kind)
                        .map(|target| (target, OccurrenceConfidence::Index)),
                },
                ReferenceTarget::Foreign { identity, file, .. } => {
                    let target = match file {
                        Some(file) => stable_foreign_target(identity, file),
                        None => system_fragment_target(identity),
                    };
                    Some((target, OccurrenceConfidence::Oracle))
                }
                ReferenceTarget::Unresolved => foreign_universe(written, reference.kind)
                    .map(|target| (target, OccurrenceConfidence::Index)),
            };
            let Some((target, confidence)) = target else {
                continue;
            };
            self.facts
                .push_occurrence(
                    owner,
                    Occurrence {
                        target,
                        kind: lane_reference_kind(reference.kind),
                        confidence,
                        span,
                    },
                )
                .map_err(|fault| lane_terminal(&self.facts, 0, fault))?;
        }
        for override_fact in self.authority.overrides {
            let Some(owner) = self.ordinal_of(override_fact.source) else {
                continue;
            };
            let Some(declaration) = self
                .authority
                .declarations
                .iter()
                .find(|declaration| declaration.identity == Some(override_fact.source))
            else {
                continue;
            };
            let Some(owner_span) = self.owner_span(owner) else {
                continue;
            };
            let span = declaration
                .name
                .and_then(|name| owner_relative_span(owner_span, name))
                .or_else(|| owner_relative_span(owner_span, declaration.span));
            let Some(span) = span else {
                continue;
            };
            self.facts
                .push_occurrence(
                    owner,
                    Occurrence {
                        target: match self.ordinal_of(override_fact.target) {
                            Some(target) => OccurrenceTarget::Local(EntityId::new(target)),
                            None => self.foreign_override_target(
                                override_fact.target,
                                override_fact.target_file,
                            )?,
                        },
                        kind: LaneReferenceKind::Overrides,
                        confidence: OccurrenceConfidence::Oracle,
                        span,
                    },
                )
                .map_err(|fault| lane_terminal(&self.facts, 0, fault))?;
        }
        Ok(())
    }

    /// Resolves the innermost pushed declaration that owns one reference.
    /// libclang's semantic parent of a use site is often a block-scope
    /// variable or parameter the lane never admits as a declaration fact, so
    /// the walk climbs the authority's owner chain until it reaches a pushed
    /// row whose span contains the reference (typically the enclosing
    /// function) instead of fabricating a span on an absent parent. When the
    /// authority leaves the owner cell empty — macro expansions name no
    /// semantic parent — the innermost executable whose span contains the
    /// reference site supplies the owner; file-scope initializers stay
    /// unowned because no executable wraps them.
    fn reference_owner(&self, reference: &ReferenceFact) -> Option<u32> {
        let mut current = reference.owner;
        while let Some(identity) = current {
            if let Some(ordinal) = self.ordinal_of(identity) {
                if self.owner_span_covers(ordinal, reference.span) {
                    return Some(ordinal);
                }
            }
            current = self
                .authority
                .declarations
                .iter()
                .find(|declaration| declaration.identity == Some(identity))
                .and_then(|declaration| declaration.owner);
        }
        self.executable_owner_containing(reference.span)
    }

    /// True when one pushed owner's authority span contains a reference site.
    fn owner_span_covers(&self, owner: u32, reference: SourceSpan) -> bool {
        self.owner_span(owner)
            .is_some_and(|owner_span| span_contains(owner_span, reference))
    }

    /// The innermost pushed executable whose authority span contains one
    /// reference site. Used only when libclang leaves the owner cell empty.
    fn executable_owner_containing(&self, reference: SourceSpan) -> Option<u32> {
        let mut best: Option<(u32, u32)> = None;
        for (index, ordinal) in self.ordinals.iter().enumerate() {
            let Some(ordinal) = *ordinal else {
                continue;
            };
            let Some(declaration) = self.authority.declarations.get(index) else {
                continue;
            };
            if !matches!(
                declaration.kind,
                DeclarationKind::Function
                    | DeclarationKind::Method
                    | DeclarationKind::Constructor
                    | DeclarationKind::Destructor
            ) {
                continue;
            }
            if !span_contains(declaration.span, reference) {
                continue;
            }
            let extent = declaration
                .span
                .end
                .checked_sub(declaration.span.start)
                .unwrap_or(u32::MAX);
            if best.is_none_or(|(_, best_extent)| extent < best_extent) {
                best = Some((ordinal, extent));
            }
        }
        best.map(|(ordinal, _)| ordinal)
    }

    /// Resolves a foreign override target to its stable cross-fragment key when
    /// the authority supplied a package-relative file, and to the fixed
    /// system fragment otherwise.
    ///
    /// A declaration spelling is presentation, not a foreign declaration
    /// identity: overloads can share it. An out-of-root target (a system
    /// header such as `std::exception`, which proves a USR but no
    /// package-relative path) keeps its exact USR-derived declaration
    /// identity against the fixed system fragment — the same honest
    /// path-free projection every out-of-root reference already receives —
    /// never a host path and never a fabricated local edge.
    fn foreign_override_target(
        &self,
        identity: SymbolIdentity,
        file: Option<SymbolIdentity>,
    ) -> Result<OccurrenceTarget<'source>, ClangCollectError> {
        match file {
            Some(file) => Ok(stable_foreign_target(identity, file)),
            None => Ok(system_fragment_target(identity)),
        }
    }

    /// Retrieves the exact source extent for a pushed owner ordinal.
    ///
    /// The span must be the one attached by [`Projector::attach_topology`],
    /// which is the winning representative's span. Looking up an identity's
    /// first declaration would return a forward declaration's smaller extent
    /// while the occurrence's relative range is measured from the definition,
    /// producing a range that escapes the owner.
    fn owner_span(&self, owner: u32) -> Option<SourceSpan> {
        let index = self.ordinals.iter().position(|slot| *slot == Some(owner))?;
        self.authority
            .declarations
            .get(index)
            .map(|declaration| declaration.span)
    }

    /// Streams every declaration's raw doxygen comment into the
    /// documentation lane as text lines with soft breaks and `@ref`/`\ref`
    /// links. libclang's raw comment text does not prove link resolution, so
    /// every parsed link remains an explicit foreign spelling.
    fn push_docs(&mut self) -> Result<(), ClangCollectError> {
        let declarations = self.authority.declarations;
        for index in 0..declarations.len() {
            let Some(ordinal) = self.ordinals.get(index).copied().flatten() else {
                continue;
            };
            // libclang supplied the documentation cell on every declaration
            // row; an absent comment is captured empty documentation, not an
            // unavailable plane.
            self.facts
                .mark_documentation_captured(ordinal)
                .map_err(|fault| lane_terminal(&self.facts, 0, fault))?;
            let Some(declaration) = declarations.get(index) else {
                continue;
            };
            let Some(span) = declaration.documentation else {
                continue;
            };
            let Ok(raw) = self.slice(span) else {
                continue;
            };
            for fragment in comment_fragments(raw) {
                let fragment = match fragment {
                    CommentFragment::Text(bytes) => DocFragmentInput::Text(bytes),
                    CommentFragment::SoftBreak => DocFragmentInput::SoftBreak,
                    CommentFragment::Link { target, label } => DocFragmentInput::Link {
                        label,
                        target: DocLinkTarget::Foreign {
                            ecosystem: ECOSYSTEM.as_bytes(),
                            path: target,
                        },
                    },
                };
                self.facts
                    .push_doc(ordinal, fragment)
                    .map_err(|fault| lane_terminal(&self.facts, 0, fault))?;
            }
        }
        Ok(())
    }
}

/// True when one declaration row was minted by foreign value-authority
/// collection for a cross-header value reference. The row carries identity
/// and kind only and must never win representative election over a named
/// main-source declaration.
const fn foreign_value_authority_stub(declaration: &DeclarationFact) -> bool {
    declaration.name.is_none()
        && declaration.span.start == 0
        && declaration.span.end == 0
        && matches!(
            declaration.kind,
            DeclarationKind::Function | DeclarationKind::Method
        )
}

/// Builds one honest cross-fragment reference from an authority-proved USR and
/// its package-relative file identity.
///
/// The fragment key is the package-relative file digest, never an absolute
/// system or store path, so the same project seals byte-identically on every
/// host. The declaration cells are separately domain-separated digests of the
/// USR; no local ordinal or staged coordinate enters the key.
fn stable_foreign_target(
    identity: SymbolIdentity,
    file: SymbolIdentity,
) -> OccurrenceTarget<'static> {
    OccurrenceTarget::Stable(StableRef {
        fragment: ExternalFragmentId::from_canonical_bytes(&file.bytes),
        declaration: DeclarationIdentity {
            family: DeclarationFamilyId::from_canonical_bytes(&identity.bytes),
            variant: VariantFingerprint::from_canonical_bytes(&identity.bytes),
        },
    })
}

/// The fixed, path-free fragment identity for every system or out-of-root
/// header reference.
///
/// System headers are stable external authority: libclang proves the target's
/// USR even when its file lives outside the package root (so no
/// package-relative path exists), and an unkeyed fault here would discard a
/// real resolution. The key is derived from this fixed domain string and the
/// USR alone — never a host or store path — so the same reference seals
/// byte-identically on every host, and the target's declaration identity
/// survives losslessly.
const SYSTEM_FRAGMENT_DOMAIN: &[u8] = b"nudox.clang.system-fragment.v1";

/// Projects one resolved out-of-root (system) target to the fixed system
/// fragment with its USR-derived declaration identity.
fn system_fragment_target(identity: SymbolIdentity) -> OccurrenceTarget<'static> {
    OccurrenceTarget::Stable(StableRef {
        fragment: ExternalFragmentId::from_canonical_bytes(SYSTEM_FRAGMENT_DOMAIN),
        declaration: DeclarationIdentity {
            family: DeclarationFamilyId::from_canonical_bytes(&identity.bytes),
            variant: VariantFingerprint::from_canonical_bytes(&identity.bytes),
        },
    })
}

/// True when `outer` fully contains `inner`.
const fn span_contains(outer: SourceSpan, inner: SourceSpan) -> bool {
    outer.start <= inner.start && inner.end <= outer.end
}

/// Borrows one include directive's delimited path spelling from the exact
/// authority span: the bytes between `<…>` or `"…"`. Directives whose
/// extent does not carry the closed delimited form stay absent.
fn include_spelling<'source>(
    source: &'source [u8],
    include: &IncludeFact,
) -> Option<&'source [u8]> {
    include_spelling_span(source, include).map(|(spelling, _)| spelling)
}

/// Borrows one include directive's delimited path spelling together with its
/// exact source extent inside the directive span.
fn include_spelling_span<'source>(
    source: &'source [u8],
    include: &IncludeFact,
) -> Option<(&'source [u8], SourceSpan)> {
    let start = usize::try_from(include.span.start).ok()?;
    let end = usize::try_from(include.span.end).ok()?;
    let bytes = source.get(start..end)?;
    let open_at = bytes
        .iter()
        .position(|byte| *byte == b'<' || *byte == b'"')?;
    let open = *bytes.get(open_at)?;
    let (_opener, closer) = match open {
        b'<' => (open, b'>'),
        b'"' => (open, b'"'),
        _ => return None,
    };
    let relative = bytes
        .get(open_at + 1..)?
        .iter()
        .position(|byte| *byte == closer)?;
    let spelling = bytes.get(open_at + 1..open_at + 1 + relative)?;
    (!spelling.is_empty()).then_some((
        spelling,
        SourceSpan {
            start: include.span.start + (open_at as u32 + 1),
            end: include.span.start + (open_at as u32 + 1 + relative as u32),
        },
    ))
}

/// Borrows the type-name identifier when a type-reference span still carries
/// a specifier prefix such as `struct Name`. When the span is already one
/// identifier token, returns it unchanged.
fn type_reference_name_bytes<'source>(written: &'source [u8]) -> Option<&'source [u8]> {
    if written.is_empty() {
        return None;
    }
    if written
        .iter()
        .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
    {
        return Some(written);
    }
    for prefix in [
        b"struct".as_slice(),
        b"class",
        b"enum",
        b"union",
        b"typename",
    ] {
        if written.len() <= prefix.len() || !written.starts_with(prefix) {
            continue;
        }
        let rest = written.get(prefix.len()..)?;
        let skip = rest.iter().take_while(|byte| byte.is_ascii_whitespace()).count();
        let ident = rest.get(skip..)?;
        let name_len = ident
            .iter()
            .take_while(|byte| byte.is_ascii_alphanumeric() || **byte == b'_')
            .count();
        if name_len > 0 {
            return ident.get(..name_len);
        }
    }
    None
}

/// Narrows one type-reference span to the identifier token inside it.
fn type_reference_name_span(source: &[u8], span: SourceSpan, written: &[u8]) -> SourceSpan {
    let Some(name) = type_reference_name_bytes(written) else {
        return span;
    };
    let offset = name.as_ptr() as usize - source.as_ptr() as usize;
    match (
        u32::try_from(offset),
        u32::try_from(offset + name.len()),
    ) {
        (Ok(start), Ok(end)) if start >= span.start && end <= span.end => SourceSpan { start, end },
        _ => span,
    }
}

/// When libclang maps a C++ member call to `CallExpr` but leaves the reference
/// span on the receiver, the callee token still lives in the main-source bytes
/// immediately after that span as `.name(` or `->name(`.
fn member_call_callee_span(source: &[u8], span: SourceSpan) -> Option<SourceSpan> {
    let tail_start = usize::try_from(span.end).ok()?;
    let tail = source.get(tail_start..)?;
    let (prefix_len, rest) = if let Some(rest) = tail.strip_prefix(b".") {
        (1, rest)
    } else if let Some(rest) = tail.strip_prefix(b"->") {
        (2, rest)
    } else {
        return None;
    };
    let name_len = rest
        .iter()
        .take_while(|byte| byte.is_ascii_alphanumeric() || **byte == b'_')
        .count();
    if name_len == 0 {
        return None;
    }
    let after_name = rest.get(name_len..)?;
    let next = after_name
        .iter()
        .find(|byte| !byte.is_ascii_whitespace())?;
    if *next != b'(' {
        return None;
    }
    let start = tail_start.checked_add(prefix_len)?;
    let end = start.checked_add(name_len)?;
    Some(SourceSpan {
        start: u32::try_from(start).ok()?,
        end: u32::try_from(end).ok()?,
    })
}

/// Borrows the callee identifier of one member call when the authority span
/// names the receiver instead of the method.
fn member_call_callee_name<'source>(
    source: &'source [u8],
    span: SourceSpan,
) -> Option<&'source [u8]> {
    let callee = member_call_callee_span(source, span)?;
    source.get(
        usize::try_from(callee.start).ok()?..usize::try_from(callee.end).ok()?,
    )
}

/// Maximum bytes after a member-access authority span to recover a field name
/// from the receiver tail. Keeps the scan on the immediate access, not a later
/// field in the same statement.
const MEMBER_FIELD_SCAN_WINDOW: usize = 32;

/// True when one borrowed slice is a single source identifier token.
fn is_source_identifier(bytes: &[u8]) -> bool {
    !bytes.is_empty()
        && !bytes[0].is_ascii_digit()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
}

/// Narrows one member-access span to the field-name identifier. When the
/// authority span already names the field token, returns it unchanged. When
/// libclang leaves the span on the receiver, recovers `.field` or `->field`
/// from the bytes immediately after that span.
fn member_field_name_span(source: &[u8], span: SourceSpan, written: &[u8]) -> SourceSpan {
    if member_access_is_call_tail(source, span) {
        return span;
    }
    if is_source_identifier(written) {
        return span;
    }
    member_field_name_span_after_receiver(source, span).unwrap_or(span)
}

/// Borrows the field identifier of one member access when the authority span
/// already names the field, or when it names the receiver and the field token
/// follows immediately as `.name` or `->name` that is not a call. Call-shaped
/// tails and receiver misses never fall back to the receiver spelling.
fn member_field_name_bytes<'source>(
    source: &'source [u8],
    span: SourceSpan,
    written: &'source [u8],
) -> Option<&'source [u8]> {
    if member_access_is_call_tail(source, span) {
        return None;
    }
    if is_source_identifier(written) {
        return Some(written);
    }
    member_field_name_span_after_receiver(source, span).and_then(|field| {
        source.get(
            usize::try_from(field.start).ok()?..usize::try_from(field.end).ok()?,
        )
    })
}

/// True when a member-access span is immediately followed by a call argument
/// list, including `receiver.method(` and `receiver->method(` when libclang
/// leaves the span on the receiver. libclang also emits a `Member` reference on
/// the callee name of `receiver.method(...)`, which must stay on the default
/// projection so the paired `Call` occurrence carries the package key.
fn member_access_is_call_tail(source: &[u8], span: SourceSpan) -> bool {
    let tail_start = usize::try_from(span.end).ok();
    let tail = tail_start.and_then(|start| source.get(start..));
    let Some(tail) = tail else {
        return false;
    };
    if matches!(
        tail.iter().find(|byte| !byte.is_ascii_whitespace()),
        Some(b'(')
    ) {
        return true;
    }
    member_receiver_call_shape(tail)
}

/// True when one tail begins with `.name(` or `->name(`.
fn member_receiver_call_shape(tail: &[u8]) -> bool {
    let rest = if let Some(rest) = tail.strip_prefix(b".") {
        rest
    } else if let Some(rest) = tail.strip_prefix(b"->") {
        rest
    } else {
        return false;
    };
    let name_len = rest
        .iter()
        .take_while(|byte| byte.is_ascii_alphanumeric() || **byte == b'_')
        .count();
    if name_len == 0 {
        return false;
    }
    let Some(after_name) = rest.get(name_len..) else {
        return false;
    };
    matches!(
        after_name.iter().find(|byte| !byte.is_ascii_whitespace()),
        Some(b'(')
    )
}

/// When libclang maps a field read to the receiver, the field token still
/// lives in the main-source bytes immediately after that span as `.name` or
/// `->name`, and the next non-whitespace byte must not be `(`.
fn member_field_name_span_after_receiver(source: &[u8], span: SourceSpan) -> Option<SourceSpan> {
    let tail_start = usize::try_from(span.end).ok()?;
    let window_end = tail_start.saturating_add(MEMBER_FIELD_SCAN_WINDOW).min(source.len());
    let tail = source.get(tail_start..window_end)?;
    let (prefix_len, rest) = if let Some(rest) = tail.strip_prefix(b".") {
        (1, rest)
    } else if let Some(rest) = tail.strip_prefix(b"->") {
        (2, rest)
    } else {
        return None;
    };
    let name_len = rest
        .iter()
        .take_while(|byte| byte.is_ascii_alphanumeric() || **byte == b'_')
        .count();
    if name_len == 0 {
        return None;
    }
    let after_name = rest.get(name_len..)?;
    let next = after_name
        .iter()
        .find(|byte| !byte.is_ascii_whitespace())?;
    if *next == b'(' {
        return None;
    }
    let start = tail_start.checked_add(prefix_len)?;
    let end = start.checked_add(name_len)?;
    Some(SourceSpan {
        start: u32::try_from(start).ok()?,
        end: u32::try_from(end).ok()?,
    })
}

/// Projects an absolute reference span onto its owner's span start. The
/// subtraction is checked and the range must stay inside the owner's captured
/// extent, so a reference libclang attributed to an owner that does not
/// strictly contain it stays absent instead of wrapping or escaping.
fn owner_relative_span(owner: SourceSpan, reference: SourceSpan) -> Option<RelSpan> {
    if reference.start < owner.start || reference.end > owner.end {
        return None;
    }
    let start = reference.start.checked_sub(owner.start)?;
    let end = reference.end.checked_sub(owner.start)?;
    RelSpan::new(start, end).ok()
}

/// The total, name-preserving reference-kind mapping from the authority's
/// closed reference lattice onto the lane's reference lattice.
const fn lane_reference_kind(kind: ReferenceKind) -> LaneReferenceKind {
    match kind {
        ReferenceKind::Call => LaneReferenceKind::FunctionCall,
        ReferenceKind::Type | ReferenceKind::Template => LaneReferenceKind::TypeReference,
        ReferenceKind::Member => LaneReferenceKind::FieldAccess,
        ReferenceKind::MacroExpansion => LaneReferenceKind::MacroInvocation,
        ReferenceKind::Value => LaneReferenceKind::VariableUse,
    }
}

/// A foreign key outside every package, carrying the exact written spelling
/// of the use site and the entity kind the reference kind implies.
fn foreign_universe<'source>(
    written: &'source [u8],
    kind: ReferenceKind,
) -> Option<OccurrenceTarget<'source>> {
    let entity_kind = match kind {
        ReferenceKind::Type | ReferenceKind::Template => Some(EntityKind::Record),
        ReferenceKind::Member => Some(EntityKind::Field),
        _ => None,
    };
    foreign_universe_key(written, entity_kind)
}

/// Builds one valid self-describing foreign target. Its path is source text
/// from the reference site or the authoritative declaration name, never a
/// hand-minted unresolved `StableRef`.
fn foreign_universe_key<'source>(
    written: &'source [u8],
    kind: Option<EntityKind>,
) -> Option<OccurrenceTarget<'source>> {
    let path = core::str::from_utf8(written).ok()?;
    (!path.is_empty())
        .then(|| {
            ForeignKey::new(
                ForeignOrigin::Universe {
                    ecosystem: ECOSYSTEM,
                },
                path,
                path,
                kind,
            )
            .ok()
            .map(OccurrenceTarget::Foreign)
        })
        .flatten()
}

/// One cleaned comment line piece: prose or an inline `@ref`/`\ref` link.
enum CommentFragment<'source> {
    /// A prose run.
    Text(&'source [u8]),
    /// A soft line break between retained comment lines.
    SoftBreak,
    /// An inline reference command: the written target and its label.
    Link {
        /// The referenced spelling.
        target: &'source [u8],
        /// The displayed label.
        label: &'source [u8],
    },
}

/// Splits one raw comment into borrowed fragments: comment decoration is
/// stripped, prose lines are kept, and `@ref`/`\ref` commands become links.
/// Consecutive fragments join through soft breaks pushed by the caller
/// between lines.
fn comment_fragments(raw: &[u8]) -> Vec<CommentFragment<'_>> {
    let mut fragments = Vec::new();
    let mut any_line = false;
    for line in raw.split(|byte| *byte == b'\n') {
        let cleaned = clean_comment_line(line);
        if cleaned.is_empty() {
            continue;
        }
        if any_line {
            fragments.push(CommentFragment::SoftBreak);
        }
        any_line = true;
        inline_refs(cleaned, &mut fragments);
    }
    fragments
}

/// Emits one cleaned line's prose and inline reference fragments.
fn inline_refs<'line>(line: &'line [u8], fragments: &mut Vec<CommentFragment<'line>>) {
    let mut cursor = 0_usize;
    while cursor < line.len() {
        let rest = line.get(cursor..).unwrap_or(&[]);
        let Some((opener_at, opener_len)) = REF_OPENERS.iter().find_map(|opener| {
            rest.windows(opener.len())
                .position(|window| window == *opener)
                .map(|at| (at, opener.len()))
        }) else {
            fragments.push(CommentFragment::Text(rest));
            return;
        };
        let prose = rest.get(..opener_at).unwrap_or(&[]);
        if !prose.is_empty() {
            fragments.push(CommentFragment::Text(prose));
        }
        let after = opener_at + opener_len;
        let tail = rest.get(after..).unwrap_or(&[]);
        let target_end = tail
            .iter()
            .position(|byte| byte.is_ascii_whitespace())
            .unwrap_or(tail.len());
        let target = tail.get(..target_end).unwrap_or(&[]);
        if !target.is_empty() {
            fragments.push(CommentFragment::Link {
                target,
                label: target,
            });
        }
        cursor += after + target_end;
    }
}

/// Strips one line's comment decoration: `//`, `///`, `//!`, `/*`, `/**`,
/// leading `*`, and trailing `*/`, then trims ASCII whitespace.
fn clean_comment_line(line: &[u8]) -> &[u8] {
    let mut bytes = trim_ascii(line);
    for opener in [
        b"///".as_slice(),
        b"//!".as_slice(),
        b"//".as_slice(),
        b"/**".as_slice(),
        b"/*".as_slice(),
    ] {
        if bytes.starts_with(opener) {
            bytes = bytes.get(opener.len()..).unwrap_or(&[]);
            break;
        }
    }
    if let Some(stripped) = bytes.strip_suffix(b"*/") {
        bytes = stripped;
    }
    bytes = trim_ascii(bytes);
    if bytes.first() == Some(&b'*') {
        bytes = trim_ascii(bytes.get(1..).unwrap_or(&[]));
    }
    bytes
}

/// Trims ASCII whitespace from both borrowed ends without copying.
fn trim_ascii(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(start, |at| at + 1);
    bytes.get(start..end).unwrap_or(&[])
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::AtomicBool;
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU32, Ordering},
    };

    use backend_frontend_clang::ClangProject;
    use backend_frontend_clang::legacy::{
        CollectError, IncludeFact, MAX_CLANG_DECLARATIONS, SourceSpan, SymbolIdentity,
    };
    use backend_semantic::ir::{
        ClangStorageClass, DecodedDocFact, DecodedOccurrence, DecodedTypeFact, EntityKind,
        ForeignOrigin, FragmentView, NominalRef, OccurrenceTarget, PrimitiveShape, SemanticTypeTag,
        SourceIdentity,
    };
    use backend_semantic::vocabulary::{
        CStandard, CompileRecipeFact, LanguageProfile, NativeTool, Stage,
    };
    use backend_version::{ContentId, SourceFactDomain, ToolchainDomain};
    use thiserror::Error;

    use super::{ClangCollectError, FactSet, collect, include_spelling};

    /// Identifies the one direct native fact a live authority proof requires.
    #[derive(Debug, Error)]
    enum TestError {
        #[error("direct libclang collection failed: {0:?}")]
        Collect(ClangCollectError),
        #[error("{label} faulted: {fault:?}")]
        Admission {
            label: &'static str,
            fault: crate::driver::lower::AdmissionFault,
        },
        #[error("the fragment failed validation: {0:?}")]
        Validate(backend_semantic::ir::FragmentError),
        #[error("the fragment output tail changed")]
        Tail,
        #[error("entity {ordinal} differed from the expected fact row")]
        Entity { ordinal: usize },
        #[error("the fragment committed {actual} rows, not {expected}")]
        Count { expected: usize, actual: usize },
        #[error("the clang extension row for entity {ordinal} was absent or undecodable")]
        Extension { ordinal: usize },
        #[error("{0}")]
        Missing(&'static str),
        #[error("expected the fixture to be rejected")]
        ExpectedRejection,
        #[error("the expected occurrence or doc fact was absent")]
        Absent,
    }

    impl From<ClangCollectError> for TestError {
        fn from(error: ClangCollectError) -> Self {
            Self::Collect(error)
        }
    }

    impl From<backend_semantic::ir::FragmentError> for TestError {
        fn from(error: backend_semantic::ir::FragmentError) -> Self {
            Self::Validate(error)
        }
    }

    /// Lowers one fixture source and writes its validated fragment,
    /// proving the untouched output tail stayed unchanged.
    fn lower(source: &[u8]) -> Result<Vec<u8>, TestError> {
        lower_with_profile(LanguageProfile::C(CStandard::C23), source)
    }

    fn lower_with_profile(profile: LanguageProfile, source: &[u8]) -> Result<Vec<u8>, TestError> {
        lower_with_authority(profile, None, source)
    }

    fn lower_with_authority(
        profile: LanguageProfile,
        project: Option<&ClangProject>,
        source: &[u8],
    ) -> Result<Vec<u8>, TestError> {
        let mut facts = FactSet::new();
        collect(
            profile,
            project,
            source,
            &AtomicBool::new(false),
            &mut facts,
        )?;
        let identity = SourceIdentity {
            identity: ContentId::<SourceFactDomain>::from_canonical_bytes(source),
            byte_len: u32::try_from(source.len()).map_err(|_| TestError::Tail)?,
        };
        let recipe = CompileRecipeFact::derive(
            profile,
            Stage::LowerIr,
            NativeTool::Clang,
            ContentId::<SourceFactDomain>::from_canonical_bytes(source),
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"clang-semantic-lane-toolchain"),
        );
        let mut output = vec![0xa5_u8; 65_536];
        let length =
            crate::driver::lower::admit(&facts, identity, recipe, recipe.profile, &mut output)
                .map_err(|fault| TestError::Admission {
                    label: "admission",
                    fault,
                })?
                .len();
        if !output[length..].iter().all(|byte| *byte == 0xa5) {
            return Err(TestError::Tail);
        }
        output.truncate(length);
        Ok(output)
    }

    /// Decodes the entity rows of one validated fragment as (name, kind).
    fn entity_rows<'fragment>(
        view: &FragmentView<'fragment>,
    ) -> Vec<(&'fragment [u8], EntityKind)> {
        let atoms: Vec<&[u8]> = view.atoms().map(|atom| atom.bytes).collect();
        view.entities()
            .map(|entity| {
                (
                    atoms.get(entity.name.raw as usize).copied().unwrap_or(&[]),
                    entity.kind,
                )
            })
            .collect()
    }

    /// Decodes every type-fact row from the validated fragment.
    fn type_facts<'fragment>(
        view: &FragmentView<'fragment>,
    ) -> Result<Vec<DecodedTypeFact<'fragment>>, TestError> {
        let Some(cursor) = view.type_facts() else {
            return Err(TestError::Missing("type facts"));
        };
        cursor
            .map(|row| row.map_err(|_| TestError::Missing("type row")))
            .collect()
    }

    /// Finds one canonical entity by semantic name and kind.
    fn entity_of(
        view: &FragmentView<'_>,
        name: &[u8],
        kind: EntityKind,
    ) -> Result<backend_semantic::ir::EntityId, TestError> {
        for entity in view.entities() {
            if entity.kind != kind {
                continue;
            }
            let atom = view
                .atoms()
                .nth(entity.name.raw as usize)
                .ok_or(TestError::Missing("entity atom"))?;
            if atom.bytes == name {
                return Ok(entity.entity);
            }
        }
        Err(TestError::Missing("entity by name"))
    }

    /// Finds the principal declared row for one canonical entity. Anonymous
    /// rows precede fact rows in the validated type grammar, so the final
    /// matching declared row is the entity's own row.
    fn row_for_entity<'fragment>(
        view: &'fragment FragmentView<'fragment>,
        entity: backend_semantic::ir::EntityId,
    ) -> Result<DecodedTypeFact<'fragment>, TestError> {
        type_facts(view)?
            .into_iter()
            .filter(|row| row.owner == entity)
            .next_back()
            .ok_or(TestError::Missing("entity type row"))
    }

    /// Borrows the local children of one validated type record.
    fn local_type_children(
        view: &FragmentView<'_>,
        record: backend_semantic::ir::SemanticTypeRecord<'_>,
    ) -> Result<Vec<backend_semantic::ir::TypeId>, TestError> {
        let end = record
            .children
            .start
            .checked_add(record.children.length)
            .ok_or(TestError::Tail)?;
        let mut children = Vec::new();
        for child in view
            .type_facts()
            .ok_or(TestError::Missing("type facts"))?
            .children()
            .map_err(|_| TestError::Missing("type children"))?
        {
            let child = child.map_err(|_| TestError::Missing("type child"))?;
            if child.ordinal < record.children.start || child.ordinal >= end {
                continue;
            }
            let backend_semantic::ir::TypeChildTarget::Type(backend_semantic::ir::TypeRef::Local(
                target,
            )) = child.child.target
            else {
                return Err(TestError::Missing("local type child"));
            };
            children.push(target);
        }
        Ok(children)
    }

    /// Decodes every occurrence fact from the validated fragment.
    fn occurrences<'fragment>(
        view: &FragmentView<'fragment>,
    ) -> Result<Vec<DecodedOccurrence<'fragment>>, TestError> {
        let Some(cursor) = view.occurrences() else {
            return Err(TestError::Missing("occurrences"));
        };
        cursor
            .map(|row| row.map_err(|_| TestError::Missing("occurrence")))
            .collect()
    }

    /// Decodes every documentation fact from the validated fragment.
    fn docs<'fragment>(
        view: &FragmentView<'fragment>,
    ) -> Result<Vec<DecodedDocFact<'fragment>>, TestError> {
        let Some(cursor) = view.docs() else {
            return Err(TestError::Missing("docs"));
        };
        cursor
            .map(|row| row.map_err(|_| TestError::Missing("doc")))
            .collect()
    }

    /// Borrows one typed Clang extension bound to a canonical entity.
    fn clang_extension(
        view: &FragmentView<'_>,
        ordinal: usize,
    ) -> Result<backend_semantic::ir::ClangFacts, TestError> {
        view.discover()
            .language_extensions()
            .map_err(|_| TestError::Extension { ordinal })?
            .ok_or(TestError::Extension { ordinal })?
            .clang
            .get(backend_semantic::ir::EntityId::new(
                u32::try_from(ordinal).map_err(|_| TestError::Tail)?,
            ))
            .map_err(|_| TestError::Extension { ordinal })?
            .ok_or(TestError::Extension { ordinal })
    }

    /// Lends one pooled parameter through the schema-aware view. Tests never
    /// derive a row stride from a historical payload grammar.
    fn pooled_type_parameter<'a>(
        view: &'a FragmentView<'a>,
        list: backend_semantic::ir::TypeParameterListId,
    ) -> Result<&'a [u8], TestError> {
        let pools = view
            .discover()
            .extension_pools()
            .map_err(|_| TestError::Tail)?
            .ok_or(TestError::Missing("pools"))?;
        let backend_semantic::ir::ReopenedTypeParameterList::Exact(parameters) = pools
            .type_parameter_list(list)
            .map_err(|_| TestError::Missing("type parameter list"))?
        else {
            return Err(TestError::Missing("exact type parameter list"));
        };
        parameters
            .get(0)
            .map(|parameter| parameter.name)
            .map_err(|_| TestError::Missing("type parameter"))
    }

    /// Lends one pooled atom list through the schema-aware pool view.
    fn pooled_atom_list<'a>(view: &'a FragmentView<'a>, index: u32) -> Result<Vec<u32>, TestError> {
        let pools = view
            .discover()
            .extension_pools()
            .map_err(|_| TestError::Tail)?
            .ok_or(TestError::Missing("pools"))?;
        Ok(pools
            .atom_list(index)
            .map_err(|_| TestError::Missing("atom list"))?
            .iter()
            .collect())
    }

    /// An empty source admits the current-schema fragment without semantic
    /// sections: no declarations, no fabricated rows.
    #[test]
    fn empty_source_admits_the_current_schema_fragment_without_semantic_sections()
    -> Result<(), TestError> {
        let bytes = lower(b"")?;
        let view = FragmentView::validate(&bytes)?;
        if view.type_facts().is_some() || view.occurrences().is_some() || view.docs().is_some() {
            return Err(TestError::Missing("absent semantic sections"));
        }
        Ok(())
    }

    /// A record commits its recursive diagonal self-nominal, and a pointer
    /// field projects to a mutable raw pointer whose child names the
    /// record's backward ordinal.
    #[test]
    fn recursive_pointer_field_projects_mut_pointer_over_backward_nominal() -> Result<(), TestError>
    {
        let source = b"struct Node { struct Node *next; };";
        let bytes = lower(source)?;
        let view = FragmentView::validate(&bytes)?;
        let entities = entity_rows(&view);
        if entities.len() != 2
            || !entities.contains(&(&b"Node"[..], EntityKind::Record))
            || !entities.contains(&(&b"next"[..], EntityKind::Field))
        {
            return Err(TestError::Entity { ordinal: 0 });
        }
        let node = entity_of(&view, b"Node", EntityKind::Record)?;
        let next = entity_of(&view, b"next", EntityKind::Field)?;
        if row_for_entity(&view, node)?.record.nominal != Some(NominalRef::Local(node)) {
            return Err(TestError::Entity { ordinal: 0 });
        }
        let pointer = row_for_entity(&view, next)?.record;
        if pointer.tag != SemanticTypeTag::Primitive
            || pointer.payload0 != u32::from(PrimitiveShape::CPointer)
            || pointer.children.length != 1
        {
            return Err(TestError::Entity { ordinal: 1 });
        }
        // Falsifier: a const pointee must flip the pointer shape to
        // ConstPointer and change the committed bytes.
        let mutated = lower(b"struct Node { const struct Node *next; };")?;
        if mutated == bytes {
            return Err(TestError::Tail);
        }
        let view = FragmentView::validate(&mutated)?;
        let rows = type_facts(&view)?;
        let next = entity_of(&view, b"next", EntityKind::Field)?;
        let pointer = row_for_entity(&view, next)?;
        let qualified = local_type_children(&view, pointer.record)?
            .into_iter()
            .filter_map(|target| rows.get(target.index()))
            .any(|row| row.record.tag == SemanticTypeTag::CQualified);
        if pointer.record.payload0 != u32::from(PrimitiveShape::CPointer) || !qualified {
            return Err(TestError::Entity { ordinal: 1 });
        }
        Ok(())
    }

    /// Mutual recursion through forward declarations targets strictly
    /// backward ordinals from both directions.
    #[test]
    fn mutual_recursion_targets_strictly_backward_ordinals() -> Result<(), TestError> {
        let source = b"struct B; struct A { struct B *b; }; struct B { struct A *a; };";
        let bytes = lower_with_profile(
            LanguageProfile::Cxx(backend_semantic::vocabulary::CxxStandard::Cxx23),
            source,
        )?;
        let view = FragmentView::validate(&bytes)?;
        let entities = entity_rows(&view);
        if entities.len() != 4
            || entities[0].0 != &b"B"[..]
            || entities[1].0 != &b"A"[..]
            || entities[2].0 != &b"b"[..]
            || entities[3].0 != &b"a"[..]
        {
            return Err(TestError::Entity { ordinal: 0 });
        }
        let rows = type_facts(&view)?;
        if rows.len() != 4 {
            return Err(TestError::Count {
                expected: 4,
                actual: rows.len(),
            });
        }
        // The forward declaration group collapsed to the definition: B's
        // definition (row 1) carries B's self-nominal, and A's field names
        // B's ordinal while B's field names A's.
        if rows[1].record.nominal != Some(NominalRef::Local(backend_semantic::ir::EntityId::new(1)))
        {
            return Err(TestError::Entity { ordinal: 1 });
        }
        if rows[2].record.children.length != 1 {
            return Err(TestError::Entity { ordinal: 2 });
        }
        Ok(())
    }

    /// Enumerators carry the underlying integer row with the enum's
    /// measured width, and typedefs keep their own nameable type anchor.
    #[test]
    fn enumerators_carry_the_underlying_integer_and_typedefs_anchor_their_name()
    -> Result<(), TestError> {
        let source = b"enum Color { RED, GREEN };\ntypedef enum Color ColorAlias;\n";
        let bytes = lower(source)?;
        let view = FragmentView::validate(&bytes)?;
        let entities = entity_rows(&view);
        if entities.len() != 4
            || !entities.contains(&(&b"Color"[..], EntityKind::Enum))
            || !entities.contains(&(&b"RED"[..], EntityKind::Variant))
            || !entities.contains(&(&b"GREEN"[..], EntityKind::Variant))
            || !entities.contains(&(&b"ColorAlias"[..], EntityKind::Alias))
        {
            return Err(TestError::Entity { ordinal: 0 });
        }
        let red = row_for_entity(&view, entity_of(&view, b"RED", EntityKind::Variant)?)?.record;
        let unsigned_32 = 32 << 1;
        if red.tag != SemanticTypeTag::Primitive
            || red.payload0 != u32::from(PrimitiveShape::Integer)
            || red.payload1 != unsigned_32
        {
            return Err(TestError::Entity { ordinal: 1 });
        }
        Ok(())
    }

    /// A function lowers to parameter carriers, a non-void result carrier,
    /// and a function fact whose constructor and FunctionPointer row name
    /// exactly those rows; a local call resolves at oracle confidence with
    /// an owner-relative span.
    #[test]
    fn signature_carriers_and_local_call_occurrences_round_trip() -> Result<(), TestError> {
        let source = b"int add(int a, int b);\nint use(void) { return add(1, 2); }\n";
        let bytes = lower(source)?;
        let view = FragmentView::validate(&bytes)?;
        let entities = entity_rows(&view);
        if entities.len() != 6
            || !entities.contains(&(&b"a"[..], EntityKind::Parameter))
            || !entities.contains(&(&b"b"[..], EntityKind::Parameter))
            || !entities.contains(&(&b"add"[..], EntityKind::Parameter))
            || !entities.contains(&(&b"add"[..], EntityKind::Function))
            || !entities.contains(&(&b"use"[..], EntityKind::Parameter))
            || !entities.contains(&(&b"use"[..], EntityKind::Function))
        {
            return Err(TestError::Entity { ordinal: 0 });
        }
        let add = entity_of(&view, b"add", EntityKind::Function)?;
        let use_function = entity_of(&view, b"use", EntityKind::Function)?;
        let function_row = row_for_entity(&view, add)?.record;
        if function_row.tag != SemanticTypeTag::FunctionPointer
            || function_row.payload1
                != backend_semantic::ir::SemanticTypeRecord::FUNCTION_RESULT_COUNT_ONE
            || function_row.children.length != 3
        {
            return Err(TestError::Entity { ordinal: 2 });
        }
        let use_row = row_for_entity(&view, use_function)?.record;
        if use_row.tag != SemanticTypeTag::FunctionPointer || use_row.children.length != 1 {
            return Err(TestError::Entity { ordinal: 4 });
        }
        let call_site = source
            .windows(3)
            .rposition(|window| window == b"add")
            .ok_or(TestError::Absent)?;
        let rows = occurrences(&view)?;
        let call = rows
            .iter()
            .find(|row| row.occurrence.kind == backend_semantic::ir::ReferenceKind::FunctionCall);
        let Some(call) = call else {
            return Err(TestError::Absent);
        };
        let OccurrenceTarget::Local(target) = call.occurrence.target else {
            return Err(TestError::Absent);
        };
        if target != add
            || call.owner != use_function
            || call.occurrence.confidence != backend_semantic::ir::OccurrenceConfidence::Oracle
        {
            return Err(TestError::Entity { ordinal: 4 });
        }
        let owner_start = source
            .windows(b"int use".len())
            .position(|window| window == b"int use")
            .ok_or(TestError::Absent)?;
        if call.occurrence.span.start
            != u32::try_from(call_site - owner_start).map_err(|_| TestError::Tail)?
        {
            return Err(TestError::Entity { ordinal: 4 });
        }
        Ok(())
    }

    /// Qualifiers, storage, and measured layout travel only in the
    /// extension row; an incomplete record keeps the layout cells empty.
    #[test]
    fn qualifiers_storage_and_layout_travel_in_the_extension_row() -> Result<(), TestError> {
        let source =
            b"static const int limit = 10;\nstruct Incomplete;\nextern volatile int flag;\n";
        let bytes = lower(source)?;
        let view = FragmentView::validate(&bytes)?;
        let entities = entity_rows(&view);
        if entities.len() != 3 || !entities.contains(&(&b"limit"[..], EntityKind::Static)) {
            return Err(TestError::Entity { ordinal: 0 });
        }
        let limit_id = entity_of(&view, b"limit", EntityKind::Static)?;
        let incomplete_id = entity_of(&view, b"Incomplete", EntityKind::Record)?;
        let flag_id = entity_of(&view, b"flag", EntityKind::Static)?;
        let limit = clang_extension(&view, limit_id.index())?;
        if !limit.qualifiers.is_const
            || limit.qualifiers.is_volatile
            || limit.storage != ClangStorageClass::Static
            || limit.layout.size_bits != Some(32)
            || limit.layout.align_bits != Some(32)
        {
            return Err(TestError::Extension { ordinal: 0 });
        }
        let flag = clang_extension(&view, flag_id.index())?;
        if flag.qualifiers.is_volatile != true || flag.storage != ClangStorageClass::Extern {
            return Err(TestError::Extension { ordinal: 2 });
        }
        let incomplete = clang_extension(&view, incomplete_id.index())?;
        if incomplete.layout.size_bits.is_some() || incomplete.layout.align_bits.is_some() {
            return Err(TestError::Extension { ordinal: 1 });
        }
        Ok(())
    }

    /// A class template interns its template parameter into the pooled
    /// lane, names it from its extension row, and a use of the parameter
    /// inside the record projects to a TypeVar row.
    #[test]
    fn template_parameters_intern_as_pooled_rows_and_typevar_uses() -> Result<(), TestError> {
        let source = b"template<typename T> struct Box { T value; };\nstruct User { struct Box<int> box; };\n";
        let bytes = lower_with_profile(
            LanguageProfile::Cxx(backend_semantic::vocabulary::CxxStandard::Cxx23),
            source,
        )?;
        let view = FragmentView::validate(&bytes)?;
        let entities = entity_rows(&view);
        if entities
            .iter()
            .any(|(name, kind)| *name == &b"T"[..] && *kind != EntityKind::Parameter)
        {
            return Err(TestError::Entity { ordinal: 0 });
        }
        // No carrier fact for the template parameter: T is not an entity.
        if entities.iter().any(|(name, _)| *name == &b"T"[..]) {
            return Err(TestError::Entity { ordinal: 0 });
        }
        let box_id = entity_of(&view, b"Box", EntityKind::Record)?;
        let extension = clang_extension(&view, box_id.index())?;
        let parameter = pooled_type_parameter(&view, extension.templates)?;
        if parameter != b"T" {
            return Err(TestError::Missing("template parameter T"));
        }
        // The field typed `T` carries a TypeVar row with the written name.
        let value_id = entity_of(&view, b"value", EntityKind::Field)?;
        let value_row = row_for_entity(&view, value_id)?;
        if value_row.record.tag != SemanticTypeTag::TypeVar
            || value_row.record.text != Some(&b"T"[..])
        {
            return Err(TestError::Entity {
                ordinal: value_id.index(),
            });
        }
        Ok(())
    }

    /// An anonymous struct becomes an AnonymousRecord(Struct) row whose
    /// named children are exactly its pushed field facts.
    #[test]
    fn anonymous_struct_projects_member_row_over_pushed_fields() -> Result<(), TestError> {
        let source = b"struct { int x; } point;\n";
        let bytes = lower(source)?;
        let view = FragmentView::validate(&bytes)?;
        let entities = entity_rows(&view);
        if entities.len() != 2
            || !entities.contains(&(&b"x"[..], EntityKind::Field))
            || !entities.contains(&(&b"point"[..], EntityKind::Static))
        {
            return Err(TestError::Entity { ordinal: 0 });
        }
        let rows = type_facts(&view)?;
        let point = entity_of(&view, b"point", EntityKind::Static)?;
        let field = entity_of(&view, b"x", EntityKind::Field)?;
        let anonymous = row_for_entity(&view, point)?.record;
        if anonymous.tag != SemanticTypeTag::AnonymousRecord
            || anonymous.payload0 != 0
            || anonymous.children.length != 1
        {
            return Err(TestError::Entity { ordinal: 1 });
        }
        let children = local_type_children(&view, anonymous)?;
        if children.len() != 1 || rows[children[0].index()].owner != field {
            return Err(TestError::Tail);
        }
        Ok(())
    }

    /// Include directives are borrowed as delimited path atoms in one
    /// shared pooled list referenced by every extension row.
    #[test]
    fn include_directives_borrow_path_atoms_into_one_shared_list() -> Result<(), TestError> {
        let source = b"#include <stdio.h>\n#include \"local.h\"\nint x;\n";
        let bytes = lower(source)?;
        let view = FragmentView::validate(&bytes)?;
        let atoms: Vec<&[u8]> = view.atoms().map(|atom| atom.bytes).collect();
        if !atoms.contains(&&b"stdio.h"[..]) || !atoms.contains(&&b"local.h"[..]) {
            return Err(TestError::Missing("include atoms"));
        }
        let extension = clang_extension(&view, 0)?;
        let list = pooled_atom_list(&view, extension.includes.raw)?;
        if list.len() != 2 {
            return Err(TestError::Count {
                expected: 2,
                actual: list.len(),
            });
        }
        for word in list {
            let coordinate = usize::try_from(word).map_err(|_| TestError::Tail)?;
            let atom = atoms.get(coordinate).copied().ok_or(TestError::Tail)?;
            if atom != &b"stdio.h"[..] && atom != &b"local.h"[..] {
                return Err(TestError::Tail);
            }
        }
        Ok(())
    }

    /// Raw doxygen comments become text lines and inline `@ref` links. A
    /// spelling collision with a pushed declaration never fabricates native
    /// link authority: without a resolved comment target it remains foreign.
    #[test]
    fn doxygen_comments_keep_unresolved_ref_links_foreign() -> Result<(), TestError> {
        let source = b"/// Adds one.\n/// See @ref add and foreign things.\nint add(int a);\n";
        let bytes = lower(source)?;
        let view = FragmentView::validate(&bytes)?;
        let rows = docs(&view)?;
        let mut ordinal = 0_usize;
        let mut saw_link = false;
        for row in &rows {
            if let backend_semantic::ir::DocFragmentInput::Link { label, target } = &row.fragment {
                saw_link = true;
                if label != &&b"add"[..] {
                    return Err(TestError::Absent);
                }
                let backend_semantic::ir::DocLinkTarget::Foreign { ecosystem, path } = target
                else {
                    return Err(TestError::Absent);
                };
                if ecosystem != &&b"c"[..] || path != &&b"add"[..] {
                    return Err(TestError::Absent);
                }
            }
            ordinal += 1;
        }
        if ordinal < 4 || !saw_link {
            return Err(TestError::Absent);
        }
        Ok(())
    }

    /// An out-of-root (system) target keys on the one fixed system fragment
    /// and the USR-derived declaration identity, never a host path. The key is
    /// stable across runs and distinct per USR.
    #[test]
    fn system_targets_key_on_fixed_fragment_and_usr() -> Result<(), TestError> {
        let identity = SymbolIdentity { bytes: [0xA5; 16] };
        let other = SymbolIdentity { bytes: [0x5A; 16] };
        let first = super::system_fragment_target(identity);
        let OccurrenceTarget::Stable(stable) = first else {
            return Err(TestError::Missing("system stable target"));
        };
        if stable.fragment
            != backend_semantic::ir::ExternalFragmentId::from_canonical_bytes(
                super::SYSTEM_FRAGMENT_DOMAIN,
            )
        {
            return Err(TestError::Missing("fixed system fragment"));
        }
        if stable.declaration.family
            != backend_semantic::ir::DeclarationFamilyId::from_canonical_bytes(&identity.bytes)
            || stable.declaration.variant
                != backend_semantic::ir::VariantFingerprint::from_canonical_bytes(&identity.bytes)
        {
            return Err(TestError::Missing("usr-derived declaration"));
        }
        let OccurrenceTarget::Stable(other_stable) = super::system_fragment_target(other) else {
            return Err(TestError::Missing("system stable target"));
        };
        if stable.fragment != other_stable.fragment {
            return Err(TestError::Missing("system fragment is shared"));
        }
        if stable.declaration == other_stable.declaration {
            return Err(TestError::Missing("distinct usr declarations"));
        }
        Ok(())
    }

    /// A cross-file project closure resolves a project-local header call to a
    /// package foreign key on the header path and callee token, and a system
    /// header call to the fixed system fragment without faulting.
    #[test]
    fn project_closure_resolves_local_and_system_references() -> Result<(), TestError> {
        let root = unique_temp_project()?;
        let header = root.join("include").join("decl.h");
        let entry = root.join("src").join("main.c");
        let source = b"#include <stdio.h>\n#include \"decl.h\"\nint use(void) { printf(\"x\"); return declared_in_header(); }\n";
        fs::write(&header, b"int declared_in_header(void);\n")
            .map_err(|_| TestError::Missing("header write"))?;
        fs::write(&entry, source).map_err(|_| TestError::Missing("entry write"))?;
        let database = format!(
            "[{{\"directory\":\"{directory}\",\"file\":\"src/main.c\",\"arguments\":[\"cc\",\"-Iinclude\",\"-std=c11\",\"-c\",\"src/main.c\"]}}]",
            directory = root.base.display(),
        );
        fs::write(root.join("compile_commands.json"), database)
            .map_err(|_| TestError::Missing("database write"))?;

        let project =
            ClangProject::open(&root, &entry).map_err(|_| TestError::Missing("project open"))?;
        let bytes =
            lower_with_authority(LanguageProfile::C(CStandard::C11), Some(&project), source)?;
        let view = FragmentView::validate(&bytes)?;
        let use_fn = entity_of(&view, b"use", EntityKind::Function)?;
        let system_fragment = match super::system_fragment_target(SymbolIdentity { bytes: [0; 16] })
        {
            OccurrenceTarget::Stable(stable) => stable.fragment,
            _ => return Err(TestError::Missing("system fragment")),
        };
        let printf_start = source
            .windows(6)
            .position(|window| window == b"printf")
            .ok_or(TestError::Absent)?;
        let mut header_call = false;
        let mut printf_stable = false;
        for row in occurrences(&view)? {
            if row.owner != use_fn {
                continue;
            }
            if row.occurrence.kind != backend_semantic::ir::ReferenceKind::FunctionCall {
                continue;
            }
            match row.occurrence.target {
                OccurrenceTarget::Foreign(key) => {
                    let ForeignOrigin::Package(lineage) = key.origin else {
                        return Err(TestError::Missing("package foreign origin"));
                    };
                    if lineage.ecosystem != "c" || lineage.name != "include/decl.h" {
                        return Err(TestError::Missing("c:include/decl.h package"));
                    }
                    if key.path != "declared_in_header" || key.display != "declared_in_header" {
                        return Err(TestError::Missing("declared_in_header path"));
                    }
                    if key.kind != Some(EntityKind::Function) {
                        return Err(TestError::Missing("function entity kind"));
                    }
                    if row.occurrence.confidence
                        != backend_semantic::ir::OccurrenceConfidence::Oracle
                    {
                        return Err(TestError::Missing("oracle confidence"));
                    }
                    header_call = true;
                }
                OccurrenceTarget::Stable(stable) => {
                    if stable.fragment != system_fragment {
                        return Err(TestError::Missing("unexpected stable fragment"));
                    }
                    let owner_start = source
                        .windows(b"int use".len())
                        .position(|window| window == b"int use")
                        .ok_or(TestError::Absent)?;
                    if row.occurrence.span.start
                        == u32::try_from(printf_start - owner_start).map_err(|_| TestError::Tail)?
                    {
                        printf_stable = true;
                    }
                }
                _ => {}
            }
        }
        if !header_call || !printf_stable {
            return Err(TestError::Missing("header foreign call and printf stable"));
        }
        Ok(())
    }

    /// Allocates a unique temporary project directory and removes it on drop.
    fn unique_temp_project() -> Result<TempProject, TestError> {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let ordinal = NEXT.fetch_add(1, Ordering::Relaxed);
        let base = std::env::temp_dir().join(format!(
            "nudox-clang-project-{pid}-{ordinal}",
            pid = std::process::id()
        ));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("include"))
            .and_then(|()| fs::create_dir_all(base.join("src")))
            .map_err(|_| TestError::Missing("temp project"))?;
        Ok(TempProject { base })
    }

    struct TempProject {
        base: PathBuf,
    }

    impl TempProject {
        fn join(&self, path: impl AsRef<Path>) -> PathBuf {
            self.base.join(path)
        }
    }

    impl AsRef<Path> for TempProject {
        fn as_ref(&self) -> &Path {
            &self.base
        }
    }

    impl Drop for TempProject {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.base);
        }
    }

    /// A project-local foreign target keys on the package-relative file digest
    /// and the USR digest alone: the same inputs seal equal, a different file
    /// changes the fragment, and no absolute path enters the key.
    #[test]
    fn stable_foreign_targets_key_on_package_relative_file_and_usr() -> Result<(), TestError> {
        let identity = SymbolIdentity { bytes: [0x11; 16] };
        let file = SymbolIdentity { bytes: [0x22; 16] };
        let other_file = SymbolIdentity { bytes: [0x33; 16] };
        let first = super::stable_foreign_target(identity, file);
        if first != super::stable_foreign_target(identity, file) {
            return Err(TestError::Missing("stable foreign target equality"));
        }
        let OccurrenceTarget::Stable(stable) = first else {
            return Err(TestError::Missing("stable foreign target"));
        };
        let OccurrenceTarget::Stable(other) = super::stable_foreign_target(identity, other_file)
        else {
            return Err(TestError::Missing("stable foreign target"));
        };
        if stable.fragment == other.fragment {
            return Err(TestError::Missing("distinct foreign fragments"));
        }
        if stable.fragment
            != backend_semantic::ir::ExternalFragmentId::from_canonical_bytes(&file.bytes)
        {
            return Err(TestError::Missing("file-derived fragment"));
        }
        if stable.declaration.family
            != backend_semantic::ir::DeclarationFamilyId::from_canonical_bytes(&identity.bytes)
        {
            return Err(TestError::Missing("usr-derived declaration"));
        }
        Ok(())
    }

    /// A macro definition becomes a typed macro fact. libclang does not bind
    /// this expansion cursor to the containing variable declaration, so the
    /// lowerer refuses to fabricate an owner-relative occurrence.
    #[test]
    fn macro_definitions_are_typed_without_fabricated_occurrence_owners() -> Result<(), TestError> {
        let source = b"#define LIMIT 100\nint x = LIMIT;\n";
        let bytes = lower(source)?;
        let view = FragmentView::validate(&bytes)?;
        let entities = entity_rows(&view);
        if entities.len() != 2
            || !entities.contains(&(&b"LIMIT"[..], EntityKind::Macro))
            || !entities.contains(&(&b"x"[..], EntityKind::Static))
        {
            return Err(TestError::Entity { ordinal: 0 });
        }
        if view.occurrences().is_some() {
            return Err(TestError::Missing("no ownerless macro occurrence"));
        }
        Ok(())
    }

    /// A member pointer into a class defined in a project header keeps the
    /// typed projection gap instead of the illegal-owner terminal: the class
    /// lives outside the walked translation unit, so its USR carries no
    /// declaration row, and no representable owner row exists. `fmt`'s
    /// `int (testing::TestSuite::*)() const` parameter is the corpus case.
    #[test]
    fn member_pointer_into_header_class_lowers_without_terminal() -> Result<(), TestError> {
        let root = unique_temp_project()?;
        let header = root.join("include").join("decl.h");
        let entry = root.join("src").join("main.cpp");
        let source =
            b"#include \"decl.h\"\nint fold(int (TestSuite::*method)() const) { return 0; }\n";
        fs::write(&header, b"class TestSuite { public: int run(); };\n")
            .map_err(|_| TestError::Missing("header write"))?;
        fs::write(&entry, source).map_err(|_| TestError::Missing("entry write"))?;
        let database = format!(
            "[{{\"directory\":\"{directory}\",\"file\":\"src/main.cpp\",\"arguments\":[\"cc\",\"-Iinclude\",\"-std=c++23\",\"-c\",\"src/main.cpp\"]}}]",
            directory = root.base.display(),
        );
        fs::write(root.join("compile_commands.json"), database)
            .map_err(|_| TestError::Missing("database write"))?;
        let project =
            ClangProject::open(&root, &entry).map_err(|_| TestError::Missing("project open"))?;
        // The whole lower must admit: an illegal-owner terminal here is the
        // regression this test exists for.
        let _ = lower_with_authority(
            LanguageProfile::Cxx(backend_semantic::vocabulary::CxxStandard::Cxx23),
            Some(&project),
            source,
        )?;
        Ok(())
    }

    /// Same-named same-typed locals and parameters of distinct lambda
    /// closures do not mint colliding coordinate-free identities: a closure's
    /// call-operator has no retained declaration row, so its body storage is
    /// not admitted as file, namespace, or record scope facts. `tinycbor`'s
    /// `tst_parser.cpp` (four `auto input` locals and repeated
    /// `(void *token, size_t len)` parameter runs inside one file-scope
    /// initializer) is the corpus case that terminalled as a duplicate
    /// declaration identity.
    #[test]
    fn lambda_closure_storage_does_not_duplicate_identities() -> Result<(), TestError> {
        let source = b"struct Input { int consumed; };\n\
                       struct Ops { int (*first)(void *token); int (*second)(void *token); };\n\
                       static const Ops ops = {\n\
                       [](void *token) { Input *input = (Input *)token; return input->consumed; },\n\
                       [](void *token) { Input *input = (Input *)token; return input->consumed; }};\n\
                       int use(void) { return ops.first(0) + ops.second(0); }\n";
        let bytes = lower_with_profile(
            LanguageProfile::Cxx(backend_semantic::vocabulary::CxxStandard::Cxx23),
            source,
        )?;
        let view = FragmentView::validate(&bytes)?;
        let entities = entity_rows(&view);
        if entities.contains(&(&b"input"[..], EntityKind::Static)) {
            return Err(TestError::Missing("no lambda local admitted as static"));
        }
        Ok(())
    }

    /// Every retained include directive becomes one file-scope module row
    /// named by the included path plus an Import occurrence keyed on that
    /// path; one uniquely spelled path stays one module row even when the
    /// directive repeats.
    #[test]
    fn include_directives_become_module_occurrences() -> Result<(), TestError> {
        let source = b"#include <stdio.h>\n#include <stdlib.h>\n#include <stdio.h>\nint x;\n";
        let bytes = lower(source)?;
        let view = FragmentView::validate(&bytes)?;
        let entities = entity_rows(&view);
        if !entities.contains(&(&b"stdio.h"[..], EntityKind::Module))
            || !entities.contains(&(&b"stdlib.h"[..], EntityKind::Module))
        {
            return Err(TestError::Missing("module rows for included paths"));
        }
        if entities
            .iter()
            .copied()
            .filter(|(name, kind)| *name == b"stdio.h" && *kind == EntityKind::Module)
            .count()
            != 1
        {
            return Err(TestError::Missing("one module row per unique path"));
        }
        let imports = occurrences(&view)?
            .into_iter()
            .filter(|row| row.occurrence.kind == backend_semantic::ir::ReferenceKind::Import)
            .count();
        // One directive per textual occurrence: the repeated stdio.h keeps
        // its first occurrence, the single stdlib.h keeps its own.
        if imports != 2 {
            return Err(TestError::Count {
                expected: 2,
                actual: imports,
            });
        }
        Ok(())
    }

    /// Call and member reference spans narrow to the referenced name token:
    /// the call site's bytes are exactly the callee, the member access's
    /// bytes are exactly the member, never the whole expression.
    #[test]
    fn call_and_member_spans_narrow_to_name_tokens() -> Result<(), TestError> {
        let source = b"int f(void);\nstruct S { int field; };\nint use(struct S *s) { return f() + s->field; }\n";
        let bytes = lower(source)?;
        let view = FragmentView::validate(&bytes)?;
        let needle = |text: &[u8]| {
            source
                .windows(text.len())
                .position(|window| window == text)
                .ok_or(TestError::Missing("needle"))
                .map(|start| (start as u32, (start + text.len()) as u32))
        };
        // Occurrence spans are relative to the owning declaration's start.
        let owner_start = needle(b"int use")?.0;
        let call_site = needle(b"f()")?;
        let member_site = needle(b"s->field")?;
        let mut call_named = false;
        let mut member_named = false;
        for row in occurrences(&view)? {
            let span = (row.occurrence.span.start, row.occurrence.span.end);
            if span == (call_site.0 - owner_start, call_site.0 - owner_start + 1) {
                call_named = true;
            }
            if span == (member_site.0 + 3 - owner_start, member_site.1 - owner_start) {
                member_named = true;
            }
        }
        if !call_named || !member_named {
            return Err(TestError::Missing("name-token occurrence spans"));
        }
        Ok(())
    }

    /// A source beyond the lane's fact bound retains the exact projection
    /// index terminal, never a truncated emission or a support claim.
    #[test]
    fn capacity_beyond_the_lane_rejects_exactly() -> Result<(), TestError> {
        let mut source = String::new();
        for ordinal in 0..crate::driver::lower::MAX_EMISSION_FACTS + 1 {
            source.push_str(&format!("int value_{ordinal};\n"));
        }
        let mut facts = FactSet::new();
        match collect(
            LanguageProfile::C(CStandard::C23),
            None,
            source.as_bytes(),
            &AtomicBool::new(false),
            &mut facts,
        ) {
            Err(ClangCollectError::Authority(CollectError::ScratchCapacity {
                lane: backend_frontend_clang::legacy::ScratchLane::Declarations,
                capacity,
                required,
            })) if capacity == MAX_CLANG_DECLARATIONS && required == capacity + 1 => Ok(()),
            Err(other) => Err(TestError::Collect(other)),
            Ok(()) => Err(TestError::ExpectedRejection),
        }
    }

    /// Exactly the lane's bound admits without rejection.
    #[test]
    fn the_exact_lane_bound_admits() -> Result<(), TestError> {
        let mut source = String::new();
        for ordinal in 0..crate::driver::lower::MAX_EMISSION_FACTS {
            source.push_str(&format!("int value_{ordinal};\n"));
        }
        let mut facts = FactSet::new();
        collect(
            LanguageProfile::C(CStandard::C23),
            None,
            source.as_bytes(),
            &AtomicBool::new(false),
            &mut facts,
        )?;
        if facts.len() != crate::driver::lower::MAX_EMISSION_FACTS {
            return Err(TestError::Count {
                expected: crate::driver::lower::MAX_EMISSION_FACTS,
                actual: facts.len(),
            });
        }
        Ok(())
    }

    /// Include spellings borrow only the delimited path bytes.
    #[test]
    fn include_spellings_borrow_the_delimited_path() -> Result<(), TestError> {
        let source = b"#include <stdio.h>\nint x;\n";
        let directive = SourceSpan { start: 0, end: 18 };
        let spelling = include_spelling(
            source,
            &IncludeFact {
                kind: backend_frontend_clang::legacy::SourceDependencyKind::Include,
                span: directive,
                resolved: None,
            },
        );
        if spelling != Some(&b"stdio.h"[..]) {
            return Err(TestError::Missing("delimited path"));
        }
        Ok(())
    }

    /// Owner-relative spans subtract the owner's start and reject inverted
    /// or wrapped references.
    #[test]
    fn owner_relative_spans_subtract_and_reject_wrapping() -> Result<(), TestError> {
        let inside = super::owner_relative_span(
            SourceSpan { start: 10, end: 30 },
            SourceSpan { start: 12, end: 15 },
        );
        let Some(inside) = inside else {
            return Err(TestError::Missing("relative span"));
        };
        if inside.start != 2 || inside.end != 5 {
            return Err(TestError::Missing("relative bounds"));
        }
        if super::owner_relative_span(
            SourceSpan { start: 20, end: 30 },
            SourceSpan { start: 12, end: 15 },
        )
        .is_some()
        {
            return Err(TestError::Missing("wrapped rejection"));
        }
        // A reference that starts inside the owner but extends past its end is
        // not representable as an owner-relative range and must stay absent.
        if super::owner_relative_span(
            SourceSpan { start: 10, end: 20 },
            SourceSpan { start: 15, end: 25 },
        )
        .is_some()
        {
            return Err(TestError::Missing("escaping rejection"));
        }
        Ok(())
    }

    /// A macro expansion inside a function body targets the pushed macro row
    /// at oracle confidence rather than disappearing when libclang leaves the
    /// reference owner cell empty.
    #[test]
    fn macro_use_inside_a_function_targets_the_pushed_macro_row() -> Result<(), TestError> {
        let source = b"#define LIMIT 100\nint use_macro(void) { return LIMIT; }\n";
        let bytes = lower(source)?;
        let view = FragmentView::validate(&bytes)?;
        let function = entity_of(&view, b"use_macro", EntityKind::Function)?;
        let macro_row = entity_of(&view, b"LIMIT", EntityKind::Macro)?;
        let use_site = source
            .windows(5)
            .rposition(|window| window == b"LIMIT")
            .ok_or(TestError::Absent)?;
        let owner_start = source
            .windows(b"int use_macro".len())
            .position(|window| window == b"int use_macro")
            .ok_or(TestError::Absent)?;
        let mut found = false;
        for row in occurrences(&view)? {
            if row.owner != function {
                continue;
            }
            let OccurrenceTarget::Local(target) = row.occurrence.target else {
                continue;
            };
            if target != macro_row {
                continue;
            }
            found = true;
            if row.occurrence.kind != backend_semantic::ir::ReferenceKind::MacroInvocation {
                return Err(TestError::Missing("macro-invocation kind"));
            }
            if row.occurrence.confidence != backend_semantic::ir::OccurrenceConfidence::Oracle {
                return Err(TestError::Missing("oracle confidence"));
            }
            if row.occurrence.span.start
                != u32::try_from(use_site - owner_start).map_err(|_| TestError::Tail)?
            {
                return Err(TestError::Missing("owner-relative name extent"));
            }
        }
        if !found {
            return Err(TestError::Absent);
        }
        Ok(())
    }

    /// A block-scope local is not a pushed declaration fact, but its use
    /// inside the owning function still carries the written spelling at index
    /// confidence rather than disappearing from the occurrence lane.
    #[test]
    fn block_local_variable_use_keeps_its_written_spelling() -> Result<(), TestError> {
        let source = b"int use_local(void) {\n    int local = 42;\n    return local;\n}\n";
        let bytes = lower(source)?;
        let view = FragmentView::validate(&bytes)?;
        let function = entity_of(&view, b"use_local", EntityKind::Function)?;
        let use_site = source
            .windows(5)
            .rposition(|window| window == b"local")
            .ok_or(TestError::Absent)?;
        let owner_start = source
            .windows(b"int use_local".len())
            .position(|window| window == b"int use_local")
            .ok_or(TestError::Absent)?;
        let mut found = false;
        for row in occurrences(&view)? {
            if row.owner != function {
                continue;
            }
            let OccurrenceTarget::Foreign(key) = row.occurrence.target else {
                continue;
            };
            if key.path != "local" || key.display != "local" {
                continue;
            }
            found = true;
            if row.occurrence.kind != backend_semantic::ir::ReferenceKind::VariableUse {
                return Err(TestError::Missing("variable-use kind"));
            }
            if row.occurrence.confidence != backend_semantic::ir::OccurrenceConfidence::Index {
                return Err(TestError::Missing("index confidence"));
            }
            if row.occurrence.span.start
                != u32::try_from(use_site - owner_start).map_err(|_| TestError::Tail)?
            {
                return Err(TestError::Missing("owner-relative name extent"));
            }
        }
        if !found {
            return Err(TestError::Absent);
        }
        Ok(())
    }
}
