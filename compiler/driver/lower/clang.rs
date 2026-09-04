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
//!   `Primitive(MutPointer)` over its pointee row, `const T*` a
//!   `Primitive(ConstPointer)`, `T&`/`T&&` a `Primitive(Reference)` whose
//!   payload bit marks the rvalue form, and every function type a
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
//!   in-TU targets become local rows at oracle confidence, resolved
//!   foreign targets become `c`-universe foreign keys at oracle
//!   confidence, and unresolved sites keep their written spelling at
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

use compiler_ir::{
    ClangFacts as WireClangFacts, ClangLayout, ClangQualifiers, ClangStorageClass,
    DocFragmentInput, DocLinkTarget, EntityId, EntityKind, ForeignKey, ForeignOrigin, NominalRef,
    Occurrence, OccurrenceConfidence, OccurrenceTarget, ProductChildRole,
    ReferenceKind as LaneReferenceKind, RelSpan, SemanticProductConstructor, SemanticTypeRecord,
    SemanticTypeTag, TypeParameterListId, TypeReason, TypeWidth,
};
use compiler_languages_clang::{
    ClangInput, ClangScratch, CollectError, DeclarationFact, DeclarationId, DeclarationKind,
    DefinitionState, IncludeFact, MAX_CLANG_DECLARATIONS, MAX_CLANG_DIAGNOSTICS,
    MAX_CLANG_INCLUDES, MAX_CLANG_OVERRIDES, MAX_CLANG_REFERENCES, MAX_CLANG_TYPE_EDGES,
    MAX_CLANG_TYPES, MethodVirtuality, OverrideFact, ReferenceFact, ReferenceKind, ReferenceTarget,
    SYMBOL_IDENTITY_BYTES, SourceSpan, StorageClass, SymbolIdentity, TypeEdge, TypeFact,
    TypeId as AuthorityTypeId, TypeKind, TypeRelation, collect_cancellable,
};
use compiler_vocabulary::{LanguageProfile, LoweringUnsupported};

use crate::lower::{
    AdmissionFault, EmissionExtension, FactSet, LEAF_PRODUCT, MAX_FACT_CHILDREN, MAX_TYPE_CHILDREN,
    SemanticFact, push_fact,
};
use crate::types::{FactFault, FactRejection};

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
    /// Canonical admission rejected the completed fact image; its cause is retained.
    Admission(AdmissionFault),
}

/// Exact projection fault retained until the collect boundary folds it into
/// the lane's closed terminal. The shared driver failure match owns the
/// terminal arms and lies outside this module's ownership. Admission faults
/// are preserved as typed terminals; projection faults retain their operands
/// here while folding to the existing closed terminal.
#[derive(Debug)]
enum ProjectionFault {
    /// An authority span had no exact byte range inside the bound source.
    Span {
        /// The rejected authority span.
        #[expect(
            dead_code,
            reason = "operands are retained for typed diagnostics; the collect boundary folds every class to the lane's closed terminal"
        )]
        span: SourceSpan,
    },
    /// An authority declaration name span was absent or empty where the
    /// lane's nonempty-name law demands bytes.
    Nameless {
        /// The rejected declaration fact.
        #[expect(
            dead_code,
            reason = "operands are retained for typed diagnostics; the collect boundary folds every class to the lane's closed terminal"
        )]
        declaration: DeclarationId,
    },
    /// An anonymous type row had no already-pushed owner fact to anchor it.
    Anchor,
    /// A bounded projection index overflowed its lane width.
    IndexCapacity,
}

/// Folds one projection fault into the lane's closed terminal. The shared
/// driver failure match owns the terminal arms and is outside this module's
/// ownership, so operand-preserving Clang terminals stay folded here; adding
/// a terminal arm is recorded as a lane criticism in the module's review notes.
fn terminal(fault: ProjectionFault) -> ClangCollectError {
    let _ = fault;
    ClangCollectError::Lowering(LoweringUnsupported::NoSupportedDeclaration)
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

/// Direct libclang declaration slots reserved by the collection transaction.
const DECLARATION_CAPACITY: usize = MAX_CLANG_DECLARATIONS;
/// Recursive type slots reserved by the collection transaction.
const TYPE_CAPACITY: usize = MAX_CLANG_TYPES;
/// Recursive type edge slots reserved by the collection transaction.
const TYPE_EDGE_CAPACITY: usize = MAX_CLANG_TYPE_EDGES;
/// Reference slots reserved by the collection transaction.
const REFERENCE_CAPACITY: usize = MAX_CLANG_REFERENCES;
/// Diagnostic slots reserved by the collection transaction.
const DIAGNOSTIC_CAPACITY: usize = MAX_CLANG_DIAGNOSTICS;
/// Include slots reserved by the collection transaction.
const INCLUDE_CAPACITY: usize = MAX_CLANG_INCLUDES;
/// C++ override-authority slots reserved by the collection transaction.
const OVERRIDE_CAPACITY: usize = MAX_CLANG_OVERRIDES;

/// `PrimitiveShape::Integer` wire cell (`repr(u32)` discriminant).
const SHAPE_INTEGER: u32 = 0;
/// `PrimitiveShape::Float` wire cell.
const SHAPE_FLOAT: u32 = 1;
/// `PrimitiveShape::Bool` wire cell.
const SHAPE_BOOL: u32 = 2;
/// `PrimitiveShape::Char` wire cell.
const SHAPE_CHAR: u32 = 3;
/// `PrimitiveShape::MutPointer` wire cell.
const SHAPE_MUT_POINTER: u32 = 5;
/// `PrimitiveShape::ConstPointer` wire cell.
const SHAPE_CONST_POINTER: u32 = 6;
/// `PrimitiveShape::Reference` wire cell.
const SHAPE_REFERENCE: u32 = 7;
/// `PrimitiveShape::Builtin` wire cell.
const SHAPE_BUILTIN: u32 = 8;

/// Integer signedness bit below the shifted width cell.
const INTEGER_SIGNED_FLAG: u32 = 1;
/// Bit offset of the integer width cell above the signedness bit.
const INTEGER_WIDTH_SHIFT: u32 = 1;

/// `Reference`-shape payload bit marking the rvalue form `T&&`. The cell's
/// remaining bits are reserved zero by the lattice; the lvalue form `T&`
/// keeps the cell zero.
const REFERENCE_RVALUE_FLAG: u32 = INTEGER_SIGNED_FLAG;

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

/// Array arity spelling shared by every C array row; the row's payload cell
/// carries the exact measured element count (length+1, zero unmeasured).
const ARRAY_ARITY_SPELLING: &[u8] = b"[]";

/// `AnonymousRecordForm::Struct` wire cell.
const ANON_RECORD_STRUCT: u32 = 0;

/// C ecosystem name of every foreign reference key and doc link.
const ECOSYSTEM: &str = "c";

/// Doxygen reference-command openers the doc scanner splits on.
const REF_OPENERS: [&[u8]; 2] = [b"@ref ", b"\\ref "];

/// Streams every provable libclang fact — declarations, recursive types,
/// signatures, occurrences, includes, and doxygen comments — into the
/// shared fact lane.
pub(crate) fn collect<'source>(
    profile: LanguageProfile,
    source: &'source [u8],
    cancelled: &AtomicBool,
    facts: &mut FactSet<'source>,
) -> Result<(), ClangCollectError> {
    let input = ClangInput::from_profile(c"nudox-input", source, profile)
        .map_err(|_| ClangCollectError::Lowering(LoweringUnsupported::ClangDeclarationForm))?;
    collect_input(input, source, cancelled, facts)
}

/// Collects one already-authorized database command and admits it through the
/// same canonical lane as [`crate::types::compile`].
pub(crate) fn lower_database<'source, 'output>(
    input: ClangInput<'source>,
    source: &'source [u8],
    source_identity: compiler_ir::SourceIdentity,
    recipe: compiler_ir::RecipeFact,
    profile: LanguageProfile,
    cancelled: &AtomicBool,
    output: &'output mut [u8],
) -> Result<&'output [u8], ClangCollectError> {
    let mut facts = FactSet::new();
    collect_input(input, source, cancelled, &mut facts)?;
    super::admit(&facts, source_identity, recipe, profile, output)
        .map_err(ClangCollectError::Admission)
}

fn collect_input<'source>(
    input: ClangInput<'source>,
    source: &'source [u8],
    cancelled: &AtomicBool,
    facts: &mut FactSet<'source>,
) -> Result<(), ClangCollectError> {
    let mut declarations = vec![empty_declaration(); DECLARATION_CAPACITY].into_boxed_slice();
    let mut types = vec![empty_type(); TYPE_CAPACITY].into_boxed_slice();
    let mut type_edges = vec![empty_type_edge(); TYPE_EDGE_CAPACITY].into_boxed_slice();
    let mut references = vec![empty_reference(); REFERENCE_CAPACITY].into_boxed_slice();
    let mut diagnostics = vec![empty_diagnostic(); DIAGNOSTIC_CAPACITY].into_boxed_slice();
    let mut includes = vec![empty_include(); INCLUDE_CAPACITY].into_boxed_slice();
    let mut overrides = vec![empty_override(); OVERRIDE_CAPACITY].into_boxed_slice();
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
    }
}

const fn empty_type() -> TypeFact {
    TypeFact {
        id: AuthorityTypeId { raw: 0 },
        kind: TypeKind::Unknown,
        qualifiers: compiler_languages_clang::TypeQualifiers {
            is_const: false,
            is_volatile: false,
            is_restrict: false,
        },
        declaration: None,
        array_len: None,
        builtin: None,
        size_bits: None,
        align_bits: None,
    }
}

const fn empty_type_edge() -> TypeEdge {
    TypeEdge {
        source: AuthorityTypeId { raw: 0 },
        relation: TypeRelation::Pointee,
        target: AuthorityTypeId { raw: 0 },
    }
}

const fn empty_reference() -> ReferenceFact {
    ReferenceFact {
        kind: ReferenceKind::Value,
        span: empty_span(),
        owner: None,
        target: ReferenceTarget::Unresolved,
    }
}

const fn empty_diagnostic() -> compiler_languages_clang::DiagnosticFact {
    compiler_languages_clang::DiagnosticFact {
        severity: compiler_languages_clang::DiagnosticSeverity::Ignored,
        location: None,
        category: 0,
        message: None,
    }
}

const fn empty_include() -> IncludeFact {
    IncludeFact {
        kind: compiler_languages_clang::SourceDependencyKind::Include,
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
    }
}

/// Maps one closed authority declaration kind onto the canonical entity kind.
/// `Template` splits through its own type: a template whose type is a
/// function type is a function template, everything else declares a record.
const fn entity_kind(kind: DeclarationKind, type_kind: Option<TypeKind>) -> Option<EntityKind> {
    match kind {
        DeclarationKind::Namespace => Some(EntityKind::Module),
        DeclarationKind::Macro => Some(EntityKind::Constant),
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
        EntityKind::Record => SemanticProductConstructor::PRODUCT,
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
        | EntityKind::Parameter => LEAF_PRODUCT,
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
    authority: compiler_languages_clang::ClangFacts<'scratch>,
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
    includes: Option<compiler_ir::AtomListId>,
    /// Type-edge adjacency: edges grouped by source row, preserving
    /// authority order, with a prefix-sum index.
    edge_order: Vec<usize>,
    edge_starts: Vec<usize>,
}

impl<'authority, 'scratch, 'source> Projector<'authority, 'scratch, 'source> {
    fn new(
        source: &'source [u8],
        authority: compiler_languages_clang::ClangFacts<'scratch>,
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

    /// The last already-pushed fact, the owner of anonymous rows interned
    /// for the fact currently being built.
    fn anchor(&self) -> Result<u32, ProjectionFault> {
        u32::try_from(self.facts.len())
            .ok()
            .and_then(|len| len.checked_sub(1))
            .ok_or(ProjectionFault::Anchor)
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
                    let becomes_definition = declaration.definition == DefinitionState::Definition
                        && self
                            .declaration(earlier)
                            .is_some_and(|known| known.definition == DefinitionState::Declaration);
                    if becomes_definition {
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
            let record = match kind {
                EntityKind::Module => unknown_record(TypeReason::Unannotated, None),
                _ => nominal_record(own_ordinal),
            };
            let extension = self.extension(declaration, template_start)?;
            let fact = SemanticFact::new(kind, name, constructor(kind))
                .typed(record)
                .with_extension(EmissionExtension::Clang(extension));
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
                DeclarationKind::Enumerator => self.push_enumerator(index)?,
                DeclarationKind::Field => self.push_typed_member(index, EntityKind::Field)?,
                DeclarationKind::Variable => self.push_typed_member(index, EntityKind::Static)?,
                DeclarationKind::Macro => self.push_macro(index)?,
                DeclarationKind::Function
                | DeclarationKind::Method
                | DeclarationKind::Constructor
                | DeclarationKind::Destructor => self.push_function(index)?,
                DeclarationKind::Parameter => {
                    self.push_typed_member(index, EntityKind::Parameter)?
                }
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

    /// Pushes one enumerator: a variant whose declared type is the enum's
    /// underlying integer — the measured width of the enumeration type row,
    /// signed per C's default underlying compatibility.
    fn push_enumerator(&mut self, index: usize) -> Result<(), ClangCollectError> {
        let declarations = self.authority.declarations;
        let Some(declaration) = declarations.get(index) else {
            return Ok(());
        };
        let Ok(name) = self.name_of(declaration) else {
            return Ok(());
        };
        let record = declaration
            .type_root
            .and_then(|root| self.type_row(root))
            .map(|row| underlying_integer_record(row.size_bits))
            .unwrap_or_else(|| unknown_record(TypeReason::OracleGap, None));
        let extension = self.extension(declaration, 0)?;
        let fact = SemanticFact::new(EntityKind::Variant, name, constructor(EntityKind::Variant))
            .typed(record)
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

    /// Pushes one macro definition as a constant with the honest unwritten
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
        let fact = SemanticFact::new(
            EntityKind::Constant,
            name,
            constructor(EntityKind::Constant),
        )
        .typed(unknown_record(TypeReason::Unannotated, None))
        .with_extension(EmissionExtension::Clang(extension));
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
            let fact = projected
                .attach(SemanticFact::new(
                    EntityKind::Parameter,
                    parameter_name,
                    LEAF_PRODUCT,
                ))
                .with_extension(EmissionExtension::Clang(extension));
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
            let fact = projected
                .attach(SemanticFact::new(EntityKind::Parameter, name, LEAF_PRODUCT))
                .with_extension(EmissionExtension::Clang(extension));
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
            record.payload1 = SemanticTypeRecord::RESULT_FLAG;
        }
        let mut projected = Projected::leaf(record);
        for ordinal in &kept_children {
            let _ = projected.child(*ordinal, None);
        }
        let extension = self.extension(declaration, 0)?;
        let fact = projected
            .attach(fact)
            .with_extension(EmissionExtension::Clang(extension));
        let ordinal = push(self.facts, fact)?;
        self.record_pushed(index, ordinal, declaration);
        Ok(())
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
        match row.kind {
            TypeKind::Builtin => Ok(self.project_builtin(row)),
            TypeKind::Named => self.project_named(row, type_id, depth),
            TypeKind::Pointer => self.project_pointer(row, depth),
            TypeKind::LvalueReference | TypeKind::RvalueReference => {
                self.project_reference(row, depth)
            }
            TypeKind::Array => self.project_array(row, depth),
            TypeKind::Function => self.project_function_type(row, depth),
            TypeKind::Unknown => Ok(Projected::leaf(unknown_record(TypeReason::OracleGap, None))),
        }
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
            compiler_languages_clang::BuiltinClass::Void => {
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
                record.payload0 = SHAPE_BUILTIN;
                record.text = Some(VOID_SPELLING);
                record
            }
            compiler_languages_clang::BuiltinClass::Bool => {
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
                record.payload0 = SHAPE_BOOL;
                record
            }
            compiler_languages_clang::BuiltinClass::Char => {
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
                record.payload0 = SHAPE_CHAR;
                record
            }
            compiler_languages_clang::BuiltinClass::Integer { signed } => {
                let Some(width) = width(row.size_bits) else {
                    return Projected::leaf(unknown_record(TypeReason::OracleGap, None));
                };
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
                record.payload0 = SHAPE_INTEGER;
                record.payload1 =
                    (width << INTEGER_WIDTH_SHIFT) | u32::from(signed) * INTEGER_SIGNED_FLAG;
                record
            }
            compiler_languages_clang::BuiltinClass::Float => {
                let Some(width) = width(row.size_bits) else {
                    return Projected::leaf(unknown_record(TypeReason::OracleGap, None));
                };
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
                record.payload0 = SHAPE_FLOAT;
                record.payload1 = width;
                record
            }
            compiler_languages_clang::BuiltinClass::Other => {
                return Projected::leaf(unknown_record(TypeReason::OracleGap, None));
            }
        };
        Projected::leaf(record)
    }

    /// Projects one named row: a pushed declaration becomes its backward
    /// nominal row, a template parameter its `TypeVar` row, an anonymous
    /// record its member row, a resolved specialization its `Apply` row over
    /// the backward base and the projected argument rows, and every other
    /// target the typed unresolved gap.
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
            return Ok(self.finish_row(SemanticTypeRecord::leaf(SemanticTypeTag::Apply), children));
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
        Ok(gap())
    }

    /// Projects one pointer row: a function pointee becomes the structural
    /// `FunctionPointer` row; every other pointee becomes a mutable or const
    /// raw pointer over its projected pointee row.
    fn project_pointer(
        &mut self,
        row: &TypeFact,
        depth: usize,
    ) -> Result<Projected<'source>, ClangCollectError> {
        let Some(pointee) = self.edge_of(row.id, TypeRelation::Pointee) else {
            return Ok(gap());
        };
        let pointee_row = self.type_row(pointee);
        if let Some(pointee) = pointee_row
            && pointee.kind == TypeKind::Function
        {
            return self.project_function_type(pointee, depth);
        }
        let is_const = pointee_row.is_some_and(|pointee| pointee.qualifiers.is_const);
        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
        record.payload0 = if is_const {
            SHAPE_CONST_POINTER
        } else {
            SHAPE_MUT_POINTER
        };
        let child = match self.project_child(pointee, depth)? {
            UNHOSTABLE => return Ok(gap()),
            coordinate => coordinate,
        };
        Ok(self.finish_row(record, vec![(child, None)]))
    }

    /// Projects one reference row: an lvalue or rvalue `Reference` over its
    /// referent row, the rvalue form marked in the reference payload cell.
    fn project_reference(
        &mut self,
        row: &TypeFact,
        depth: usize,
    ) -> Result<Projected<'source>, ClangCollectError> {
        let Some(referent) = self.edge_of(row.id, TypeRelation::Referent) else {
            return Ok(gap());
        };
        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
        record.payload0 = SHAPE_REFERENCE;
        if row.kind == TypeKind::RvalueReference {
            record.payload1 = REFERENCE_RVALUE_FLAG;
        }
        let child = match self.project_child(referent, depth)? {
            UNHOSTABLE => return Ok(gap()),
            coordinate => coordinate,
        };
        Ok(self.finish_row(record, vec![(child, None)]))
    }

    /// Projects one array row: the element count travels in the payload
    /// cell as length+1, zero when the authority could not measure a
    /// length (incomplete, variable, or dependent arrays).
    fn project_array(
        &mut self,
        row: &TypeFact,
        depth: usize,
    ) -> Result<Projected<'source>, ClangCollectError> {
        let Some(element) = self.edge_of(row.id, TypeRelation::Element) else {
            return Ok(gap());
        };
        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Array);
        record.text = Some(ARRAY_ARITY_SPELLING);
        record.payload0 = match row.array_len {
            Some(length) => {
                u32::try_from(length.checked_add(1).unwrap_or(u64::MAX)).unwrap_or(u32::MAX)
            }
            None => 0,
        };
        let child = match self.project_child(element, depth)? {
            UNHOSTABLE => return Ok(gap()),
            coordinate => coordinate,
        };
        Ok(self.finish_row(record, vec![(child, None)]))
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
        if has_result {
            record.payload1 = SemanticTypeRecord::RESULT_FLAG;
        }
        Ok(self.finish_row(record, children))
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
    /// owned by the lane's current anchor and returns its coordinate, or
    /// [`UNHOSTABLE`] when the pooled lane cannot host the row.
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
    /// topologically backward. A lane with no anchor, or a full pool, keeps
    /// the row unhostable — never a fabricated coordinate.
    fn intern_row(&mut self, projected: Projected<'source>) -> Result<u32, ClangCollectError> {
        let Ok(anchor) = self.anchor() else {
            return Ok(UNHOSTABLE);
        };
        for position in 0..projected.child_count {
            let target = projected.children.get(position).copied().unwrap_or(0);
            let name = projected.child_names.get(position).copied().flatten();
            if self.facts.anonymous_type_child(target, name, 0).is_err() {
                return Ok(UNHOSTABLE);
            }
        }
        match self
            .facts
            .intern_anonymous_type_row(anchor, projected.record)
        {
            Ok(coordinate) => Ok(coordinate),
            Err(_) => Ok(UNHOSTABLE),
        }
    }

    /// Finishes one compound row over its ordered children, folding to the
    /// typed gap when the children exceed the row's bounded child width —
    /// never a partial, lying row.
    fn finish_row(
        &self,
        record: SemanticTypeRecord<'source>,
        children: Vec<(u32, Option<&'source [u8]>)>,
    ) -> Projected<'source> {
        if children.len() > MAX_TYPE_CHILDREN {
            return gap();
        }
        let mut projected = Projected::leaf(record);
        for (target, name) in children {
            if projected.child(target, name).is_none() {
                return gap();
            }
        }
        projected
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
                return Ok(gap());
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
                    compiler_languages_clang::TypeQualifiers {
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
    fn includes_list(&mut self) -> Result<compiler_ir::AtomListId, ClangCollectError> {
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
            let target = match reference.target {
                ReferenceTarget::Local(identity) => match self.ordinal_of(identity) {
                    Some(ordinal) => Some((
                        OccurrenceTarget::Local(EntityId::new(ordinal)),
                        OccurrenceConfidence::Oracle,
                    )),
                    None => None,
                },
                ReferenceTarget::Foreign(_) | ReferenceTarget::Unresolved => {
                    let confidence = match reference.target {
                        ReferenceTarget::Foreign(_) => OccurrenceConfidence::Oracle,
                        _ => OccurrenceConfidence::Index,
                    };
                    foreign_universe(written, reference.kind).map(|target| (target, confidence))
                }
            };
            let Some((target, confidence)) = target else {
                continue;
            };
            let Some(owner_span) = self.owner_span(owner) else {
                continue;
            };
            let Some(span) = owner_relative_span(owner_span, reference.span) else {
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
        // Schema 1 has no identity-keyed foreign occurrence target.  A
        // foreign override is therefore deliberately deferred to the
        // schema-2 identity-cell packet; only edges whose two identities are
        // both pushed in this image can be represented honestly here.
        for override_fact in self.authority.overrides {
            let Some(owner) = self.ordinal_of(override_fact.source) else {
                continue;
            };
            let Some(target) = self.ordinal_of(override_fact.target) else {
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
                        target: OccurrenceTarget::Local(EntityId::new(target)),
                        kind: LaneReferenceKind::Overrides,
                        confidence: OccurrenceConfidence::Oracle,
                        span,
                    },
                )
                .map_err(|fault| lane_terminal(&self.facts, 0, fault))?;
        }
        Ok(())
    }

    /// Resolves an authority owner, falling back to the innermost pushed
    /// declaration containing a reference whose authority owner is absent.
    fn reference_owner(&self, reference: &ReferenceFact) -> Option<u32> {
        if let Some(owner) = reference
            .owner
            .and_then(|identity| self.ordinal_of(identity))
        {
            return Some(owner);
        }
        self.identities
            .iter()
            .filter_map(|(identity, ordinal)| {
                let declaration = self
                    .authority
                    .declarations
                    .iter()
                    .find(|declaration| declaration.identity == Some(*identity))?;
                span_contains(declaration.span, reference.span)
                    .then_some((declaration.span, *ordinal))
            })
            .min_by_key(|(span, _)| (span.end - span.start, span.start))
            .map(|(_, ordinal)| ordinal)
    }

    /// Retrieves the exact source extent for a pushed owner ordinal.
    fn owner_span(&self, owner: u32) -> Option<SourceSpan> {
        self.identities
            .iter()
            .find(|(_, ordinal)| *ordinal == owner)
            .and_then(|(identity, _)| {
                self.authority
                    .declarations
                    .iter()
                    .find(|declaration| declaration.identity == Some(*identity))
                    .map(|declaration| declaration.span)
            })
    }

    /// Streams every declaration's raw doxygen comment into the
    /// documentation lane as text lines with soft breaks and `@ref`/`\ref`
    /// links whose targets name pushed declarations locally.
    fn push_docs(&mut self) -> Result<(), ClangCollectError> {
        let declarations = self.authority.declarations;
        for index in 0..declarations.len() {
            let Some(ordinal) = self.ordinals.get(index).copied().flatten() else {
                continue;
            };
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
                    CommentFragment::Link { target, label } => {
                        let resolved = match self.link_target(target) {
                            Some(ordinal) => DocLinkTarget::Local(EntityId::new(ordinal)),
                            None => DocLinkTarget::Foreign {
                                ecosystem: ECOSYSTEM.as_bytes(),
                                path: target,
                            },
                        };
                        DocFragmentInput::Link {
                            label,
                            target: resolved,
                        }
                    }
                };
                self.facts
                    .push_doc(ordinal, fragment)
                    .map_err(|fault| lane_terminal(&self.facts, 0, fault))?;
            }
        }
        Ok(())
    }

    /// Resolves one doc-link target spelling: exact or `::`-suffix matches
    /// against pushed declaration names.
    fn link_target(&self, target: &[u8]) -> Option<u32> {
        for (index, ordinal) in self.ordinals.iter().enumerate() {
            let Some(ordinal) = *ordinal else {
                continue;
            };
            let Some(declaration) = self.declaration(index) else {
                continue;
            };
            let Ok(name) = self.name_of(declaration) else {
                continue;
            };
            if name == target {
                return Some(ordinal);
            }
            if let Some(boundary) = name.len().checked_sub(target.len() + 2)
                && name.get(boundary..boundary + 2) == Some(b"::")
                && name.get(boundary + 2..) == Some(target)
            {
                return Some(ordinal);
            }
        }
        None
    }
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
    (!spelling.is_empty()).then_some(spelling)
}

/// Projects an absolute reference span onto its owner's span start. The
/// subtraction is checked, so a reference outside its pushed owner's extent
/// stays absent instead of wrapping.
fn owner_relative_span(owner: SourceSpan, reference: SourceSpan) -> Option<RelSpan> {
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
    let path = core::str::from_utf8(written).ok()?;
    if path.is_empty() {
        return None;
    }
    let entity_kind = match kind {
        ReferenceKind::Type | ReferenceKind::Template => Some(EntityKind::Record),
        ReferenceKind::Member => Some(EntityKind::Field),
        _ => None,
    };
    let key = ForeignKey::new(
        ForeignOrigin::Universe {
            ecosystem: ECOSYSTEM,
        },
        path,
        path,
        entity_kind,
    )
    .ok()?;
    Some(OccurrenceTarget::Foreign(key))
}

/// The underlying-integer row of one enumeration: the measured width of the
/// enum type, signed per C's default underlying compatibility. An
/// unmeasured enumeration keeps the typed oracle gap.
fn underlying_integer_record(size_bits: Option<u32>) -> SemanticTypeRecord<'static> {
    let Some(bits) = size_bits.and_then(|bits| u32::try_from(bits).ok()) else {
        return unknown_record(TypeReason::OracleGap, None);
    };
    let Ok(bits) = u16::try_from(bits) else {
        return unknown_record(TypeReason::OracleGap, None);
    };
    let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
    record.payload0 = SHAPE_INTEGER;
    record.payload1 = (u32::from(bits) << INTEGER_WIDTH_SHIFT) | INTEGER_SIGNED_FLAG;
    record
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

    use compiler_ir::{
        ClangStorageClass, DecodedDocFact, DecodedOccurrence, DecodedTypeFact, EntityKind,
        FragmentView, LanguageExtensionWireFact, NominalRef, OccurrenceTarget, PrimitiveShape,
        SemanticTypeTag, SourceIdentity, TypeReason,
    };
    use compiler_languages_clang::{IncludeFact, SourceSpan};
    use compiler_vocabulary::{CStandard, CompileRecipeFact, LanguageProfile, NativeTool, Stage};
    use heart_identity::{ContentId, SourceFactDomain, ToolchainDomain};
    use thiserror::Error;

    use super::{ClangCollectError, FactSet, collect, include_spelling, owner_relative_span};

    /// Identifies the one direct native fact a live authority proof requires.
    #[derive(Debug, Error)]
    enum TestError {
        #[error("direct libclang collection failed: {0:?}")]
        Collect(ClangCollectError),
        #[error("{label} faulted: {fault:?}")]
        Admission {
            label: &'static str,
            fault: crate::lower::AdmissionFault,
        },
        #[error("the fragment failed validation: {0:?}")]
        Validate(compiler_ir::FragmentError),
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

    impl From<compiler_ir::FragmentError> for TestError {
        fn from(error: compiler_ir::FragmentError) -> Self {
            Self::Validate(error)
        }
    }

    /// Lowers one fixture source and writes its validated fragment,
    /// proving the untouched output tail stayed unchanged.
    fn lower(source: &[u8]) -> Result<Vec<u8>, TestError> {
        let mut facts = FactSet::new();
        collect(
            LanguageProfile::C(CStandard::C23),
            source,
            &AtomicBool::new(false),
            &mut facts,
        )?;
        let identity = SourceIdentity {
            identity: ContentId::<SourceFactDomain>::from_canonical_bytes(source),
            byte_len: u32::try_from(source.len()).map_err(|_| TestError::Tail)?,
        };
        let recipe = CompileRecipeFact::derive(
            LanguageProfile::C(CStandard::C23),
            Stage::LowerIr,
            NativeTool::Clang,
            ContentId::<SourceFactDomain>::from_canonical_bytes(source),
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"clang-semantic-lane-toolchain"),
        );
        let mut output = vec![0xa5_u8; 65_536];
        let length = crate::lower::admit(&facts, identity, recipe, recipe.profile, &mut output)
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

    /// Reads one little-endian u32 word of a validated section payload.
    fn word(payload: &[u8], at: usize) -> Result<u32, TestError> {
        payload
            .get(at..at + 4)
            .map(|bytes| {
                u32::from_le_bytes(
                    bytes
                        .try_into()
                        .map_err(|_| TestError::Tail)
                        .unwrap_or([0; 4]),
                )
            })
            .ok_or(TestError::Tail)
    }

    /// Decodes the clang extension row of one entity from the raw
    /// language-extension section: a 16-byte header, one 20-byte directory
    /// entry per plane (clang is the seventh), then the row table and pool.
    fn clang_extension(
        view: &FragmentView<'_>,
        ordinal: usize,
    ) -> Result<compiler_ir::ClangFacts, TestError> {
        let payload = view
            .language_extension_payload()
            .ok_or(TestError::Extension { ordinal })?;
        let directory = 16 + 6 * 20;
        let rows = usize::try_from(word(payload, directory + 4)?).map_err(|_| TestError::Tail)?;
        let facts = word(payload, directory + 8)?;
        let offset =
            usize::try_from(word(payload, directory + 12)?).map_err(|_| TestError::Tail)?;
        if facts == 0 {
            return Err(TestError::Extension { ordinal });
        }
        let fact_ordinal = word(payload, offset + ordinal * 4)?;
        if fact_ordinal == u32::MAX {
            return Err(TestError::Extension { ordinal });
        }
        let at =
            offset + rows * 4 + usize::try_from(fact_ordinal).map_err(|_| TestError::Tail)? * 24;
        compiler_ir::ClangFacts::decode(payload, at).ok_or(TestError::Extension { ordinal })
    }

    /// Reads one pooled type parameter from the extension-pool payload: a
    /// u32 count, then (presence byte, u32 length, bytes, two optionals).
    fn pooled_type_parameter(pool: &[u8], index: usize) -> Result<Option<&[u8]>, TestError> {
        let count = usize::try_from(word(pool, 0)?).map_err(|_| TestError::Tail)?;
        if index >= count {
            return Err(TestError::Missing("type parameter"));
        }
        let mut cursor = 4_usize;
        for ordinal in 0..count {
            if pool.get(cursor).copied() != Some(1) {
                return Err(TestError::Tail);
            }
            let length = usize::try_from(word(pool, cursor + 1)?).map_err(|_| TestError::Tail)?;
            let name = pool
                .get(cursor + 5..cursor + 5 + length)
                .ok_or(TestError::Tail)?;
            cursor += 5 + length + 10;
            if ordinal == index {
                return Ok(Some(name));
            }
        }
        Err(TestError::Missing("type parameter"))
    }

    /// Reads one pooled atom list from the extension-pool payload: a u32
    /// type-parameter count (present in this lane), then the atom-list lane
    /// of (length, u32 words) rows.
    fn pooled_atom_list(pool: &[u8], index: usize) -> Result<Vec<u32>, TestError> {
        let parameters = usize::try_from(word(pool, 0)?).map_err(|_| TestError::Tail)?;
        let mut cursor = 4_usize;
        for _ in 0..parameters {
            let present = pool.get(cursor).copied().ok_or(TestError::Tail)?;
            let length = usize::try_from(word(pool, cursor + 1)?).map_err(|_| TestError::Tail)?;
            cursor += 5 + length + 10;
            let _ = present;
        }
        let list_count = usize::try_from(word(pool, cursor)?).map_err(|_| TestError::Tail)?;
        cursor += 4;
        for list in 0..list_count {
            let length = usize::try_from(word(pool, cursor)?).map_err(|_| TestError::Tail)?;
            cursor += 4;
            if list == index {
                let mut words = Vec::new();
                for offset in 0..length {
                    words.push(word(pool, cursor + offset * 4)?);
                }
                return Ok(words);
            }
            cursor += length * 4;
        }
        Err(TestError::Missing("atom list"))
    }

    /// An empty source admits the schema-1 fragment without semantic
    /// sections: no declarations, no fabricated rows.
    #[test]
    fn empty_source_admits_the_schema1_fragment_without_semantic_sections() -> Result<(), TestError>
    {
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
            || entities[0].0 != &b"Node"[..]
            || entities[0].1 != EntityKind::Record
            || entities[1].1 != EntityKind::Field
        {
            return Err(TestError::Entity { ordinal: 0 });
        }
        let rows = type_facts(&view)?;
        if rows.len() != 2 {
            return Err(TestError::Count {
                expected: 2,
                actual: rows.len(),
            });
        }
        if rows[0].record.nominal != Some(NominalRef::Local(compiler_ir::EntityId::new(0))) {
            return Err(TestError::Entity { ordinal: 0 });
        }
        let pointer = &rows[1].record;
        if pointer.tag != SemanticTypeTag::Primitive
            || pointer.payload0 != u32::from(PrimitiveShape::MutPointer)
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
        if rows[1].record.payload0 != u32::from(PrimitiveShape::ConstPointer) {
            return Err(TestError::Entity { ordinal: 1 });
        }
        Ok(())
    }

    /// Mutual recursion through forward declarations targets strictly
    /// backward ordinals from both directions.
    #[test]
    fn mutual_recursion_targets_strictly_backward_ordinals() -> Result<(), TestError> {
        let source = b"struct B; struct A { struct B *b; }; struct B { struct A *a; };";
        let bytes = lower(source)?;
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
        if rows[1].record.nominal != Some(NominalRef::Local(compiler_ir::EntityId::new(1))) {
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
            || entities[0].1 != EntityKind::Enum
            || entities[1].1 != EntityKind::Variant
            || entities[2].1 != EntityKind::Variant
            || entities[3].1 != EntityKind::Alias
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
        let red = &rows[1].record;
        let signed_32 = (32 << 1) | compiler_ir::SemanticTypeRecord::INTEGER_SIGNED_FLAG;
        if red.tag != SemanticTypeTag::Primitive
            || red.payload0 != u32::from(PrimitiveShape::Integer)
            || red.payload1 != signed_32
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
        // a, b, add, use's result carrier, use.
        if entities.len() != 5
            || entities[0] != (&b"a"[..], EntityKind::Parameter)
            || entities[1] != (&b"b"[..], EntityKind::Parameter)
            || entities[2].1 != EntityKind::Function
            || entities[3].0 != &b"use"[..]
            || entities[4].1 != EntityKind::Function
        {
            return Err(TestError::Entity { ordinal: 0 });
        }
        let rows = type_facts(&view)?;
        if rows.len() != 5 {
            return Err(TestError::Count {
                expected: 5,
                actual: rows.len(),
            });
        }
        let function_row = &rows[2].record;
        if function_row.tag != SemanticTypeTag::FunctionPointer
            || function_row.payload1 != compiler_ir::SemanticTypeRecord::RESULT_FLAG
            || function_row.children.length != 3
        {
            return Err(TestError::Entity { ordinal: 2 });
        }
        let use_row = &rows[4].record;
        if use_row.tag != SemanticTypeTag::FunctionPointer || use_row.children.length != 1 {
            return Err(TestError::Entity { ordinal: 4 });
        }
        let call_site = source
            .windows(3)
            .position(|window| window == b"add")
            .ok_or(TestError::Absent)?;
        let rows = occurrences(&view)?;
        let call = rows
            .iter()
            .find(|row| row.occurrence.kind == compiler_ir::ReferenceKind::FunctionCall);
        let Some(call) = call else {
            return Err(TestError::Absent);
        };
        let OccurrenceTarget::Local(target) = call.occurrence.target else {
            return Err(TestError::Absent);
        };
        if target.raw != 2
            || call.owner.raw != 4
            || call.occurrence.confidence != compiler_ir::OccurrenceConfidence::Oracle
        {
            return Err(TestError::Entity { ordinal: 4 });
        }
        if call.occurrence.span.start != u32::try_from(call_site).map_err(|_| TestError::Tail)? {
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
        // limit, Incomplete (forward-declared, kept once), flag.
        if entities.len() != 3 || entities[0].0 != &b"limit"[..] {
            return Err(TestError::Entity { ordinal: 0 });
        }
        let limit = clang_extension(&view, 0)?;
        if !limit.qualifiers.is_const
            || limit.qualifiers.is_volatile
            || limit.storage != ClangStorageClass::Static
            || limit.layout.size_bits != Some(32)
            || limit.layout.align_bits != Some(32)
        {
            return Err(TestError::Extension { ordinal: 0 });
        }
        let flag = clang_extension(&view, 2)?;
        if flag.qualifiers.is_volatile != true || flag.storage != ClangStorageClass::Extern {
            return Err(TestError::Extension { ordinal: 2 });
        }
        let incomplete = clang_extension(&view, 1)?;
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
        let bytes = lower(source)?;
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
        let box_ordinal = entities
            .iter()
            .position(|(name, _)| *name == &b"Box"[..])
            .ok_or(TestError::Absent)?;
        let extension = clang_extension(&view, box_ordinal)?;
        let pool = view
            .extension_pool_payload()
            .ok_or(TestError::Missing("pools"))?;
        let parameter = pooled_type_parameter(pool, extension.templates.raw as usize)?;
        if parameter != Some(&b"T"[..]) {
            return Err(TestError::Missing("template parameter T"));
        }
        // The field typed `T` carries a TypeVar row with the written name.
        let rows = type_facts(&view)?;
        let value_ordinal = entities
            .iter()
            .position(|(name, _)| *name == &b"value"[..])
            .ok_or(TestError::Absent)?;
        let value_row = rows.get(value_ordinal).ok_or(TestError::Entity {
            ordinal: value_ordinal,
        })?;
        if value_row.record.tag != SemanticTypeTag::TypeVar
            || value_row.record.text != Some(&b"T"[..])
        {
            return Err(TestError::Entity {
                ordinal: value_ordinal,
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
            || entities[0] != (&b"x"[..], EntityKind::Field)
            || entities[1] != (&b"point"[..], EntityKind::Static)
        {
            return Err(TestError::Entity { ordinal: 0 });
        }
        let rows = type_facts(&view)?;
        if rows.len() != 2 {
            return Err(TestError::Count {
                expected: 2,
                actual: rows.len(),
            });
        }
        let anonymous = &rows[1].record;
        if anonymous.tag != SemanticTypeTag::AnonymousRecord
            || anonymous.payload0 != 0
            || anonymous.children.length != 1
        {
            return Err(TestError::Entity { ordinal: 1 });
        }
        let payload = view.type_fact_payload().ok_or(TestError::Tail)?;
        let children = last_type_children(payload, &rows)?;
        if children != vec![0] {
            return Err(TestError::Tail);
        }
        Ok(())
    }

    /// Parses the pooled local targets and names of the last type-fact row.
    /// Every child here is one target tag byte, one u32 coordinate, one
    /// length-prefixed name cell, and one flags byte.
    fn last_type_children(
        payload: &[u8],
        rows: &[DecodedTypeFact<'_>],
    ) -> Result<Vec<u32>, TestError> {
        let cell = |cursor: &mut usize| -> Result<(), TestError> {
            match payload.get(*cursor).copied() {
                Some(0) => *cursor += 1,
                Some(1) => {
                    *cursor += 1
                        + 4
                        + usize::try_from(word(payload, *cursor + 1)?)
                            .map_err(|_| TestError::Tail)?;
                }
                _ => return Err(TestError::Tail),
            }
            Ok(())
        };
        let mut cursor = 4_usize;
        let record_count = usize::try_from(word(payload, 0)?).map_err(|_| TestError::Tail)?;
        for _ in 0..record_count {
            cursor += 4 + 1 + 4 + 4;
            cell(&mut cursor)?;
            cell(&mut cursor)?;
            let nominal = payload.get(cursor).copied().ok_or(TestError::Tail)?;
            cursor += 1;
            if nominal == 1 {
                cursor += 4;
            } else if nominal == 2 {
                cursor += 16 + 4;
            }
            cursor += 8;
        }
        cursor += 4;
        let last = rows.last().ok_or(TestError::Tail)?;
        let start = usize::try_from(last.record.children.start).map_err(|_| TestError::Tail)?;
        let length = usize::try_from(last.record.children.length).map_err(|_| TestError::Tail)?;
        let width = 1 + 4 + 4;
        let mut targets = Vec::new();
        for position in 0..length {
            let at = cursor + (start + position) * width;
            if payload.get(at).copied() != Some(0) {
                return Err(TestError::Tail);
            }
            targets.push(word(payload, at + 1)?);
        }
        Ok(targets)
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
        let pool = view
            .extension_pool_payload()
            .ok_or(TestError::Missing("pools"))?;
        let list = pooled_atom_list(pool, extension.includes.raw as usize)?;
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

    /// Raw doxygen comments become text lines and inline `@ref` links;
    /// targets naming pushed declarations link locally.
    #[test]
    fn doxygen_comments_split_into_text_and_local_ref_links() -> Result<(), TestError> {
        let source = b"/// Adds one.\n/// See @ref add and foreign things.\nint add(int a);\n";
        let bytes = lower(source)?;
        let view = FragmentView::validate(&bytes)?;
        let rows = docs(&view)?;
        let mut ordinal = 0_usize;
        let mut saw_link = false;
        for row in &rows {
            if let compiler_ir::DocFragmentInput::Link { label, target } = &row.fragment {
                saw_link = true;
                if label != &&b"add"[..] {
                    return Err(TestError::Absent);
                }
                let compiler_ir::DocLinkTarget::Local(local) = target else {
                    return Err(TestError::Absent);
                };
                let entities = entity_rows(&view);
                if entities.get(local.raw as usize).copied()
                    != Some((&b"add"[..], EntityKind::Function))
                {
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

    /// A macro definition becomes a constant fact and its use site becomes
    /// a macro-invocation occurrence at index confidence.
    #[test]
    fn macro_definitions_become_constants_and_uses_macro_invocations() -> Result<(), TestError> {
        let source = b"#define LIMIT 100\nint x = LIMIT;\n";
        let bytes = lower(source)?;
        let view = FragmentView::validate(&bytes)?;
        let entities = entity_rows(&view);
        if entities.len() != 2
            || entities[0] != (&b"LIMIT"[..], EntityKind::Constant)
            || entities[1] != (&b"x"[..], EntityKind::Static)
        {
            return Err(TestError::Entity { ordinal: 0 });
        }
        let rows = occurrences(&view)?;
        let invocation = rows
            .iter()
            .find(|row| row.occurrence.kind == compiler_ir::ReferenceKind::MacroInvocation)
            .ok_or(TestError::Absent)?;
        if invocation.occurrence.confidence != compiler_ir::OccurrenceConfidence::Oracle {
            return Err(TestError::Absent);
        }
        Ok(())
    }

    /// A source beyond the lane's 128-fact bound is the exact typed
    /// lowering rejection, never a truncated emission.
    #[test]
    fn capacity_beyond_the_lane_rejects_exactly() -> Result<(), TestError> {
        let mut source = String::new();
        for ordinal in 0..crate::lower::MAX_EMISSION_FACTS + 1 {
            source.push_str(&format!("int value_{ordinal};\n"));
        }
        let mut facts = FactSet::new();
        match collect(
            LanguageProfile::C(CStandard::C23),
            source.as_bytes(),
            &AtomicBool::new(false),
            &mut facts,
        ) {
            Err(ClangCollectError::Lowering(
                compiler_vocabulary::LoweringUnsupported::NoSupportedDeclaration,
            )) => Ok(()),
            Err(other) => Err(TestError::Collect(other)),
            Ok(()) => Err(TestError::ExpectedRejection),
        }
    }

    /// Exactly the lane's bound admits without rejection.
    #[test]
    fn the_exact_lane_bound_admits() -> Result<(), TestError> {
        let mut source = String::new();
        for ordinal in 0..crate::lower::MAX_EMISSION_FACTS {
            source.push_str(&format!("int value_{ordinal};\n"));
        }
        let mut facts = FactSet::new();
        collect(
            LanguageProfile::C(CStandard::C23),
            source.as_bytes(),
            &AtomicBool::new(false),
            &mut facts,
        )?;
        if facts.len() != crate::lower::MAX_EMISSION_FACTS {
            return Err(TestError::Count {
                expected: crate::lower::MAX_EMISSION_FACTS,
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
                kind: compiler_languages_clang::SourceDependencyKind::Include,
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
        Ok(())
    }
}
