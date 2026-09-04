//! Projects source-backed OXC syntax, bindings, and spans into the canonical
//! declaration lane with recursive declared types, exact signatures, resolved
//! references, and JSDoc documentation.
//! Keeps TypeScript syntax and lexical authority in-process beside the configured checker.
//! Contains no token reconstruction, fallback collector, or declaration guessing.

use compiler_ir::{
    AnnotationKind, AnonRecordForm, DocFragmentInput, DocLinkTarget, EntityId, EntityKind, ExternalEntityRef,
    ExternalFragmentId, ForeignKey, ForeignOrigin,
    LatticeMappedModifier, NominalRef, Occurrence, OccurrenceConfidence, OccurrenceTarget,
    PackageLineage, PrimitiveShape, ProductChildRole, ReferenceKind, RelSpan,
    SemanticProductConstructor, SemanticTypeChild, SemanticTypeRecord, SemanticTypeTag, TypeId,
    TypeParameterListId, TypeReason, TypeWidth,
};
use compiler_languages_typescript::{
    AuthorityError, BoundReference, Checker, CheckerIndex, GetSpan, Origin, ReferenceFlags,
    MappedModifier as CheckerMappedModifier, NodeId, Semantic, Span, SymbolFlags, SymbolId,
    TemplatePart, SyntaxMappedModifier, TypeTree, Utf8Span, syntax_mapped_modifier, with_analysis,
};
use compiler_vocabulary::TypeScriptSource;

use crate::{
    lower::{
        COMPUTED_ROW_BASE, EmissionExtension, FactSet, FactTypeChild, LEAF_PRODUCT,
        MAX_EMISSION_FACTS, MAX_FACT_CHILDREN, MAX_TYPE_CHILDREN, SemanticFact, push_fact,
    },
    types::{FactFault, FactRejection, LoweringUnsupported},
};

/// Bound of one declaration's staged type-parameter rows; a source with more
/// generic parameters on one declaration is a typed lane rejection.
const MAX_DECL_TYPE_PARAMETERS: usize = 16;
/// Recursion bound for type-expression lowering; deeper expressions are
/// honestly unknown with [`TypeReason::TruncatedAtDepthLimit`].
const MAX_TYPE_DEPTH: u8 = 24;
/// Sentinel marking an unset projection-table row.
const UNSET: u32 = u32::MAX;
/// The closed foreign ecosystem every unresolved TypeScript name lives in.
const NPM_ECOSYSTEM: &str = "npm";
/// Bound of staged JSDoc segments on one comment line.
const MAX_JSDOC_SEGMENTS: usize = 16;

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
    /// An OXC declaration span could not name a slice of the admitted source.
    Span { start: u32, end: u32 },
}

/// The lane's one coarse terminal, shared by every bounded-lane rejection
/// exactly as [`push_fact`] reports them.
fn lane_rejection() -> TypeScriptCollectError {
    TypeScriptCollectError::Lowering(LoweringUnsupported::NoSupportedDeclaration)
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

/// Maps one foreign-key rejection onto the coarse lane terminal. The path and
/// display cells here are proven non-empty source slices, so a rejection
/// means a malformed cross-package path and the lane cannot proceed.
fn foreign_fault(_cause: compiler_ir::ForeignKeyFault) -> TypeScriptCollectError {
    lane_rejection()
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

/// Bounded staging for one declaration's generic parameters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TypeParamRows {
    rows: [TypeParamRow; MAX_DECL_TYPE_PARAMETERS],
    len: usize,
}

impl TypeParamRows {
    const fn new() -> Self {
        Self {
            rows: [TypeParamRow {
                name: Span::new(0, 0),
                constraint: None,
                default: None,
            }; MAX_DECL_TYPE_PARAMETERS],
            len: 0,
        }
    }

    /// Stages one row; a source declaring more generics on one declaration
    /// than the staged bound is the typed lane rejection the bounded
    /// extension-pool lane would raise anyway.
    fn push(&mut self, row: TypeParamRow) -> Result<(), TypeScriptCollectError> {
        match self.rows.get_mut(self.len) {
            Some(slot) => {
                *slot = row;
                self.len += 1;
                Ok(())
            }
            None => Err(fault(FactFault::TypeParameterCapacity)),
        }
    }

    fn iter(&self) -> impl Iterator<Item = &TypeParamRow> {
        self.rows.iter().take(self.len)
    }
}

/// One staged parameter: its declared name, optional annotation, and the
/// source-owned optional/rest modifier bits that are later written into the
/// canonical function type row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ParamRow {
    name: Span,
    annotation: Option<Span>,
    flags: u8,
}

/// Bounded staging for one signature's parameters. One slot beyond the fact
/// lane's child bound lets an over-wide signature reach the lane's own typed
/// child-capacity rejection instead of a staging one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ParamRows {
    rows: [ParamRow; MAX_FACT_CHILDREN + 1],
    len: usize,
}

impl ParamRows {
    const fn new() -> Self {
        Self {
            rows: [ParamRow {
                name: Span::new(0, 0),
                annotation: None,
                flags: 0,
            }; MAX_FACT_CHILDREN + 1],
            len: 0,
        }
    }

    fn push(&mut self, row: ParamRow) -> Result<(), TypeScriptCollectError> {
        match self.rows.get_mut(self.len) {
            Some(slot) => {
                *slot = row;
                self.len += 1;
                Ok(())
            }
            None => Err(fault(FactFault::ChildCapacity)),
        }
    }

    fn count(&self) -> usize {
        self.len
    }

    fn iter(&self) -> impl Iterator<Item = &ParamRow> {
        self.rows.iter().take(self.len)
    }
}

/// One object-literal member link: its fact ordinal, name bytes, and flags.
type MemberLink<'source> = (u32, &'source [u8], u8);

/// One lowered type expression: the lattice record plus its bounded,
/// strictly-backward child links into already-pushed facts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TypeCells<'source> {
    record: SemanticTypeRecord<'source>,
    children: [FactTypeChild<'source>; MAX_TYPE_CHILDREN],
    len: usize,
    truncated: bool,
}

impl<'source> TypeCells<'source> {
    /// A leaf record with no children and no cells.
    const fn leaf(tag: SemanticTypeTag) -> Self {
        Self {
            record: SemanticTypeRecord::leaf(tag),
            children: [FactTypeChild {
                target: 0,
                name: None,
                flags: 0,
            }; MAX_TYPE_CHILDREN],
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
        match self.children.get_mut(self.len) {
            Some(slot) => {
                *slot = FactTypeChild {
                    target,
                    name,
                    flags,
                };
                self.len += 1;
                Ok(())
            }
            None => {
                self.truncated = true;
                Err(fault(FactFault::TypeChildCapacity))
            }
        }
    }
}

/// The outcome of lowering one type expression: either the type is embodied
/// by an already-pushed fact (a declared entity or a type parameter), or it
/// carries cells the caller applies to its own fact or to a new synthetic
/// type-expression fact.
#[expect(
    clippy::large_enum_variant,
    reason = "TypeCells is a fixed stack record bounded by the lane's type-child law; boxing every lowered type expression would trade a bounded stack value for a heap allocation"
)]
enum TypeOutcome<'source> {
    Existing(u32),
    Cells(TypeCells<'source>),
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
struct Projector<'x, 'report, 'source> {
    semantic: &'x Semantic<'x>,
    /// Span index built once for this projection; type probes use binary
    /// search instead of rescanning the complete syntax arena.
    node_index: Vec<(Span, NodeId)>,
    source: &'source str,
    facts: &'x mut FactSet<'source>,
    /// Binding-name span start per pushed fact (`UNSET` when unregistered).
    name_starts: Box<[u32]>,
    name_ends: Box<[u32]>,
    /// Declaring-node span start per pushed fact.
    decl_starts: Box<[u32]>,
    decl_ends: Box<[u32]>,
    fact_kinds: Box<[EntityKind]>,
    /// One row per import-binding fact: the module and imported-name spans
    /// its foreign keys are built from.
    import_modules: Box<[ImportModule]>,
    import_module_len: usize,
    /// The span-bound checker report, when the authority ran.
    checker: Option<CheckerIndex<'report>>,
    /// The pooled type-parameter start of the fact about to be pushed; every
    /// push path builds its extension immediately before pushing.
    pending_type_parameters: u32,
    /// Pooled type-parameter start per pushed fact, retained so the checker
    /// pass can re-attach a completed extension with the computed cell.
    extension_type_parameters: Box<[u32]>,
}

/// The immutable registration view one computed lowering needs: the source
/// itself plus the pushed facts' exact source coordinates. Split borrows of
/// the projector's fields keep the mutable fact lane free during recursion.
/// The definition lives beside the computed lowering functions below.
struct FactRegistry<'a, 'source> {
    source: &'source str,
    decl_starts: &'a [u32],
    decl_ends: &'a [u32],
    name_starts: &'a [u32],
    name_ends: &'a [u32],
    fact_kinds: &'a [EntityKind],
    fact_len: u32,
}

impl<'x, 'report, 'source> Projector<'x, 'report, 'source> {
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
        if let Some(slot) = self.extension_type_parameters.get_mut(ordinal as usize) {
            *slot = pending;
        }
        Ok(ordinal)
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
            compiler_ir::TypeScriptFacts {
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
    /// kind so references and links can resolve to its ordinal.
    fn register(&mut self, fact: u32, declaration: Span, name: Span, kind: EntityKind) {
        let Some(index) = usize::try_from(fact).ok() else {
            return;
        };
        if let Some(slot) = self.name_starts.get_mut(index) {
            *slot = name.start;
        }
        if let Some(slot) = self.name_ends.get_mut(index) {
            *slot = name.end;
        }
        if let Some(slot) = self.decl_starts.get_mut(index) {
            *slot = declaration.start;
        }
        if let Some(slot) = self.decl_ends.get_mut(index) {
            *slot = declaration.end;
        }
        if let Some(slot) = self.fact_kinds.get_mut(index) {
            *slot = kind;
        }
    }

    /// Resolves one source position to the fact whose binding name starts
    /// exactly there.
    fn fact_at_name_start(&self, start: u32) -> Option<u32> {
        let length = coordinate(self.facts.len()).ok()?;
        for ordinal in 0..length {
            let index = usize::try_from(ordinal).ok()?;
            if self.name_starts.get(index) == Some(&start) {
                return Some(ordinal);
            }
        }
        None
    }

    /// Resolves one source position to the innermost pushed fact whose
    /// declaring span contains it.
    fn owning_fact(&self, position: u32) -> Option<u32> {
        let length = coordinate(self.facts.len()).ok()?;
        let mut best: Option<(u32, u32)> = None;
        for ordinal in 0..length {
            let index = usize::try_from(ordinal).ok()?;
            let start = *self.decl_starts.get(index)?;
            let end = *self.decl_ends.get(index)?;
            if start != UNSET
                && end != UNSET
                && start <= position
                && position < end
                && best.is_none_or(|(known, _)| start >= known)
            {
                best = Some((start, ordinal));
            }
        }
        best.map(|(_, ordinal)| ordinal)
    }

    /// Resolves the first pushed fact whose declaring span starts at or after
    /// `position` — the natural owner of a JSDoc block ending there.
    fn next_fact_after(&self, position: u32) -> Option<u32> {
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

    /// Resolves the first pushed fact whose exact binding-name bytes equal
    /// `name`.
    fn fact_by_name_bytes(&self, name: &[u8]) -> Option<u32> {
        let length = coordinate(self.facts.len()).ok()?;
        for ordinal in 0..length {
            let index = usize::try_from(ordinal).ok()?;
            let start = usize::try_from(*self.name_starts.get(index)?).ok()?;
            let end = usize::try_from(*self.name_ends.get(index)?).ok()?;
            if self.source.as_bytes().get(start..end) == Some(name) {
                return Some(ordinal);
            }
        }
        None
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
        for node in nodes.iter() {
            let kind = node.kind();
            if kind.span().start != start {
                continue;
            }
            let Some(identifier) = kind.as_identifier_reference() else {
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

    /// Reports whether the identifier at `span` sits in callee position of an
    /// enclosing call or construction expression.
    fn is_call_position(&self, span: Span) -> bool {
        let nodes = self.semantic.nodes();
        let mut found = false;
        for node in nodes.iter() {
            let kind = node.kind();
            if kind.span() != span || kind.as_identifier_reference().is_none() {
                continue;
            }
            let parent = nodes.get_node(nodes.parent_id(node.id())).kind();
            if let Some(call) = parent.as_call_expression()
                && call.callee.span() == span
            {
                found = true;
            } else if let Some(construction) = parent.as_new_expression()
                && construction.callee.span() == span
            {
                found = true;
            }
        }
        found
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
            self.register(ordinal, row.name, row.name, EntityKind::Parameter);
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
            let name = self
                .slice_span(row.name)
                .ok_or(TypeScriptCollectError::Span {
                    start: row.name.start,
                    end: row.name.end,
                })?;
            let type_parameter_start = coordinate(self.facts.type_parameter_len)?;
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
            self.register(ordinal, row.name, row.name, EntityKind::Parameter);
            if let Some(slot) = ordinals.get_mut(index) {
                *slot = ordinal;
            }
        }
        Ok(ordinals)
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
    ) -> Result<u32, TypeScriptCollectError> {
        self.push_type_parameter_facts(rows)?;
        let type_parameter_start = self.push_type_params(rows, 0)?;
        let param_ordinals = self.push_parameter_facts(params)?;
        let result_target = match result {
            Some(span) => Some(self.child_target(span.start, span.end, 0)?),
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
        let extension = self.extension(type_parameter_start)?;
        let mut fact = SemanticFact::new(
            EntityKind::Function,
            name_bytes,
            SemanticProductConstructor::function(param_count, result_count),
        )
        .typed(record)
        .with_extension(extension);
        for (ordinal, parameter) in param_ordinals.iter().zip(params.iter()) {
            fact = fact.child(ProductChildRole::FunctionParameter, *ordinal);
            fact = fact.type_child(*ordinal, None, parameter.flags);
        }
        if let Some(target) = result_target {
            fact = fact.child(ProductChildRole::FunctionResult, target);
            fact = fact.type_child(target, None, 0);
        }
        let ordinal = self.push(fact)?;
        self.register(ordinal, declaration, name, EntityKind::Function);
        Ok(ordinal)
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
        let type_parameter_start = self.push_type_params(rows, 0)?;
        let own = coordinate(self.facts.len())?;
        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Nominal);
        record.nominal = Some(NominalRef::Local(EntityId::new(own)));
        let extension = self.extension(type_parameter_start)?;
        let fact = SemanticFact::new(kind, name_bytes, constructor)
            .typed(record)
            .with_extension(extension);
        let ordinal = self.push(fact)?;
        self.register(ordinal, declaration, name, kind);
        Ok(())
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
        self.register(ordinal, declaration, local, EntityKind::Reexport);
        if let Some(slot) = self.import_modules.get_mut(self.import_module_len) {
            *slot = ImportModule {
                fact: ordinal,
                module_start: module.start,
                module_end: module.end,
                display_start: display.start,
                display_end: display.end,
            };
            self.import_module_len += 1;
        }
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
                .ok_or_else(lane_rejection)?;
            let display = self
                .text_span(Span::new(row.display_start, row.display_end))
                .ok_or_else(lane_rejection)?;
            let origin = match PackageLineage::new(NPM_ECOSYSTEM, module) {
                Ok(lineage) => ForeignOrigin::Package(lineage),
                Err(_) => ForeignOrigin::Universe {
                    ecosystem: NPM_ECOSYSTEM,
                },
            };
            let key = ForeignKey::new(origin, display, display, Some(EntityKind::Reexport))
                .map_err(foreign_fault)?;
            return Ok(OccurrenceTarget::Foreign(key));
        }
        Err(lane_rejection())
    }

    /// Pushes one synthetic fact embodying an anonymous type expression,
    /// named by its exact source spelling (kind [`EntityKind::Alias`]).
    fn synthetic_cells_fact(
        &mut self,
        span: Span,
        cells: TypeCells<'source>,
    ) -> Result<u32, TypeScriptCollectError> {
        let name = self.slice_span(span).ok_or(TypeScriptCollectError::Span {
            start: span.start,
            end: span.end,
        })?;
        let type_parameter_start = coordinate(self.facts.type_parameter_len)?;
        let extension = self.extension(type_parameter_start)?;
        let fact = with_cells(
            SemanticFact::new(EntityKind::Alias, name, LEAF_PRODUCT).with_extension(extension),
            cells,
        );
        self.push(fact)
    }

    /// Pushes one synthetic honestly-unknown fact used where a structural
    /// position demands a fact ordinal and the source wrote no type.
    fn unannotated_fact(&mut self, fallback_name: Span) -> Result<u32, TypeScriptCollectError> {
        self.synthetic_cells_fact(fallback_name, TypeCells::unknown(TypeReason::Unannotated))
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
    /// type-expression fact.
    fn child_target(
        &mut self,
        start: u32,
        end: u32,
        depth: u8,
    ) -> Result<u32, TypeScriptCollectError> {
        match self.lower_type(start, end, depth)? {
            TypeOutcome::Existing(fact) => Ok(fact),
            TypeOutcome::Cells(cells) => self.synthetic_cells_fact(Span::new(start, end), cells),
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
            if let Some(parenthesized) = kind.as_ts_parenthesized_type() {
                let inner = parenthesized.type_annotation.span();
                return self.lower_type(inner.start, inner.end, next_depth);
            }
            if let Some(named) = kind.as_ts_named_tuple_member() {
                let inner = named.element_type.span();
                return self.lower_type(inner.start, inner.end, next_depth);
            }
            if let Some(optional) = kind.as_ts_optional_type() {
                let inner = optional.type_annotation.span();
                return self.lower_type(inner.start, inner.end, next_depth);
            }
            if let Some(rest) = kind.as_ts_rest_type() {
                let inner = rest.type_annotation.span();
                return self.lower_type(inner.start, inner.end, next_depth);
            }

            if let Some(union) = kind.as_ts_union_type() {
                if union.types.len() > MAX_TYPE_CHILDREN {
                    let spans: Vec<_> = union.types.iter().map(GetSpan::span).collect();
                    return self.associative_cells(&spans, next_depth, SemanticTypeTag::Union);
                }
                let mut cells = TypeCells::leaf(SemanticTypeTag::Union);
                for member in union.types.iter() {
                    let member_span = member.span();
                    let target =
                        self.child_target(member_span.start, member_span.end, next_depth)?;
                    cells.push_child(target, None, 0)?;
                }
                return Ok(TypeOutcome::Cells(cells));
            }
            if let Some(template) = kind.as_ts_template_literal_type() {
                let mut cells = TypeCells::leaf(SemanticTypeTag::TemplateLiteral);
                // OXC preserves one quasi before, between, and after every
                // substitution. Keep that ordered alternating sequence in the
                // canonical child lane; `record.text` cannot represent it.
                for (position, quasi) in template.quasis.iter().enumerate() {
                    let text = self.slice_span(quasi.span).ok_or(TypeScriptCollectError::Span {
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
            if let Some(intersection) = kind.as_ts_intersection_type() {
                if intersection.types.len() > MAX_TYPE_CHILDREN {
                    let spans: Vec<_> = intersection.types.iter().map(GetSpan::span).collect();
                    return self.associative_cells(
                        &spans,
                        next_depth,
                        SemanticTypeTag::Intersection,
                    );
                }
                let mut cells = TypeCells::leaf(SemanticTypeTag::Intersection);
                for member in intersection.types.iter() {
                    let member_span = member.span();
                    let target =
                        self.child_target(member_span.start, member_span.end, next_depth)?;
                    cells.push_child(target, None, 0)?;
                }
                return Ok(TypeOutcome::Cells(cells));
            }
            if let Some(tuple) = kind.as_ts_tuple_type() {
                let mut cells = TypeCells::leaf(SemanticTypeTag::Tuple);
                for element in tuple.element_types.iter() {
                    let element_span = element.span();
                    let mut label: Option<Span> = None;
                    let mut inner = element_span;
                    let mut flags = 0_u8;
                    if let Some(position) = self
                        .node_index
                        .binary_search_by_key(
                            &(element_span.start, element_span.end),
                            |(known, _)| (known.start, known.end),
                        )
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
            if let Some(literal) = kind.as_ts_type_literal() {
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
            if let Some(function_type) = kind.as_ts_function_type() {
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
                    cells.push_child(target, None, flags)?;
                }
                if let Some(rest) = function_type.params.rest.as_ref() {
                    let target = match rest.type_annotation.as_ref() {
                        Some(annotation) => {
                            let inner = annotation.type_annotation.span();
                            self.child_target(inner.start, inner.end, next_depth)?
                        }
                        None => self.unannotated_fact(rest.span())?,
                    };
                    cells.push_child(target, None, SemanticTypeChild::FLAG_REST)?;
                }
                let returned = function_type.return_type.type_annotation.span();
                let target = self.child_target(returned.start, returned.end, next_depth)?;
                cells.record.payload1 = SemanticTypeRecord::FUNCTION_RESULT_COUNT_ONE;
                cells.push_child(target, None, 0)?;
                return Ok(TypeOutcome::Cells(cells));
            }
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
                    let index = usize::try_from(fact).map_err(|_| lane_rejection())?;
                    if self.fact_kinds.get(index) == Some(&EntityKind::Parameter) {
                        // A use of a generic parameter is a TypeVar naming it.
                        let mut cells = TypeCells::leaf(SemanticTypeTag::TypeVar);
                        cells.record.text = self.slice_span(name_span);
                        return Ok(TypeOutcome::Cells(cells));
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
            if let Some(query) = kind.as_ts_type_query() {
                let inner = query.expr_name.span();
                if let Some(fact) = self.local_fact_at(inner.start) {
                    return Ok(TypeOutcome::Existing(fact));
                }
                let mut cells = TypeCells::unknown(TypeReason::UnresolvedExternal);
                cells.record.text = self.slice_span(span);
                return Ok(TypeOutcome::Cells(cells));
            }
            if let Some(mapped) = kind.as_ts_mapped_type() {
                let mut cells = TypeCells::leaf(SemanticTypeTag::Mapped);
                cells.record.payload0 = syntax_mapped_modifier_cell(syntax_mapped_modifier(mapped.readonly));
                cells.record.payload1 = syntax_mapped_modifier_cell(syntax_mapped_modifier(mapped.optional));
                cells.record.text = self.slice_span(mapped.key.span);
                let constraint = mapped.constraint.span();
                let constraint_target =
                    self.child_target(constraint.start, constraint.end, next_depth)?;
                let name_as_target = mapped.name_type.as_ref().map(|name_type| {
                    let span = name_type.span();
                    self.child_target(span.start, span.end, next_depth)
                }).transpose()?;
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
            if let Some(conditional) = kind.as_ts_conditional_type() {
                let mut cells = TypeCells::leaf(SemanticTypeTag::Conditional);
                let check = conditional.check_type.span();
                let extends = conditional.extends_type.span();
                let true_branch = conditional.true_type.span();
                let false_branch = conditional.false_type.span();
                let check_target = self.child_target(check.start, check.end, next_depth)?;
                let extends_target = self.child_target(extends.start, extends.end, next_depth)?;
                let true_target =
                    self.child_target(true_branch.start, true_branch.end, next_depth)?;
                let false_target =
                    self.child_target(false_branch.start, false_branch.end, next_depth)?;
                cells.push_child(check_target, None, 0)?;
                cells.push_child(extends_target, None, 0)?;
                cells.push_child(true_target, None, 0)?;
                cells.push_child(false_target, None, 0)?;
                return Ok(TypeOutcome::Cells(cells));
            }
            if let Some(array) = kind.as_ts_array_type() {
                let element = array.element_type.span();
                let element_target = self.child_target(element.start, element.end, next_depth)?;
                let mut cells = TypeCells::leaf(SemanticTypeTag::ArraySequence);
                cells.push_child(element_target, None, 0)?;
                return Ok(TypeOutcome::Cells(cells));
            }
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
            if kind.as_ts_this_type().is_some() {
                let mut cells = TypeCells::leaf(SemanticTypeTag::SelfType);
                cells.record.text = Some(&b"this"[..]);
                return Ok(TypeOutcome::Cells(cells));
            }
            if let Some(number) = kind.as_ts_number_keyword() {
                let _ = number;
                let mut cells = TypeCells::leaf(SemanticTypeTag::Primitive);
                cells.record.payload0 = u32::from(PrimitiveShape::Float);
                cells.record.payload1 = TypeWidth::Fixed(64).to_cell();
                return Ok(TypeOutcome::Cells(cells));
            }
            if kind.as_ts_string_keyword().is_some() {
                let mut cells = TypeCells::leaf(SemanticTypeTag::Primitive);
                cells.record.payload0 = u32::from(PrimitiveShape::Str);
                return Ok(TypeOutcome::Cells(cells));
            }
            if kind.as_ts_boolean_keyword().is_some() {
                let mut cells = TypeCells::leaf(SemanticTypeTag::Primitive);
                cells.record.payload0 = u32::from(PrimitiveShape::Bool);
                return Ok(TypeOutcome::Cells(cells));
            }
            if kind.as_ts_big_int_keyword().is_some() {
                let mut cells = TypeCells::leaf(SemanticTypeTag::Primitive);
                cells.record.payload0 = u32::from(PrimitiveShape::Builtin);
                cells.record.text = Some(&b"bigint"[..]);
                return Ok(TypeOutcome::Cells(cells));
            }
            if kind.as_ts_void_keyword().is_some() {
                let mut cells = TypeCells::leaf(SemanticTypeTag::Primitive);
                cells.record.payload0 = u32::from(PrimitiveShape::Builtin);
                cells.record.text = Some(&b"void"[..]);
                return Ok(TypeOutcome::Cells(cells));
            }
            if kind.as_ts_null_keyword().is_some() {
                let mut cells = TypeCells::leaf(SemanticTypeTag::Primitive);
                cells.record.payload0 = u32::from(PrimitiveShape::Builtin);
                cells.record.text = Some(&b"null"[..]);
                return Ok(TypeOutcome::Cells(cells));
            }
            if kind.as_ts_undefined_keyword().is_some() {
                let mut cells = TypeCells::leaf(SemanticTypeTag::Primitive);
                cells.record.payload0 = u32::from(PrimitiveShape::Builtin);
                cells.record.text = Some(&b"undefined"[..]);
                return Ok(TypeOutcome::Cells(cells));
            }
            if kind.as_ts_any_keyword().is_some() {
                return Ok(TypeOutcome::Cells(TypeCells::unknown(
                    TypeReason::DynamicallyTyped,
                )));
            }
            if kind.as_ts_unknown_keyword().is_some() {
                return Ok(TypeOutcome::Cells(TypeCells::leaf(SemanticTypeTag::Any)));
            }
            if kind.as_ts_never_keyword().is_some() {
                return Ok(TypeOutcome::Cells(TypeCells::leaf(SemanticTypeTag::Never)));
            }
        }
        Ok(TypeOutcome::Cells(self.unrepresented(span)))
    }

    /// Recognizes the closed literal spellings on the source plane. The
    /// checker reports their bases, while the source owns the exact value.
    fn literal_cells(&self, span: Span) -> Option<TypeCells<'source>> {
        let text = self.slice_span(span)?;
        let text = trim_bytes(text, b" \t\r\n");
        let base = if (text.starts_with(b"\"") && text.ends_with(b"\""))
            || (text.starts_with(b"'") && text.ends_with(b"'"))
        {
            Some(compiler_languages_typescript::LiteralBase::String)
        } else if text == b"true" || text == b"false" {
            Some(compiler_languages_typescript::LiteralBase::Boolean)
        } else if text.ends_with(b"n")
            && text[..text.len().saturating_sub(1)]
                .iter()
                .all(u8::is_ascii_digit)
        {
            Some(compiler_languages_typescript::LiteralBase::Bigint)
        } else if !text.is_empty()
            && text
                .iter()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'.' | b'-' | b'+'))
        {
            Some(compiler_languages_typescript::LiteralBase::Number)
        } else {
            None
        }?;
        let mut cells = TypeCells::leaf(SemanticTypeTag::Primitive);
        cells.record.payload0 = u32::from(PrimitiveShape::Builtin);
        cells.record.text = Some(text);
        cells.record.payload1 = u32::from(base as u8);
        Some(cells)
    }

    fn associative_cells(
        &mut self,
        spans: &[Span],
        depth: u8,
        tag: SemanticTypeTag,
    ) -> Result<TypeOutcome<'source>, TypeScriptCollectError> {
        let first = spans.first().ok_or_else(lane_rejection)?;
        let second = spans.get(1).ok_or_else(lane_rejection)?;
        let mut left = self.child_target(first.start, first.end, depth)?;
        let right = self.child_target(second.start, second.end, depth)?;
        let mut pair = TypeCells::leaf(tag);
        pair.push_child(left, None, 0)?;
        pair.push_child(right, None, 0)?;
        left = self.synthetic_cells_fact(*second, pair)?;
        for span in spans.iter().skip(2).take(spans.len().saturating_sub(3)) {
            let right = self.child_target(span.start, span.end, depth)?;
            let mut pair = TypeCells::leaf(tag);
            pair.push_child(left, None, 0)?;
            pair.push_child(right, None, 0)?;
            left = self.synthetic_cells_fact(*span, pair)?;
        }
        let last = spans.last().ok_or_else(lane_rejection)?;
        let right = self.child_target(last.start, last.end, depth)?;
        let mut root = TypeCells::leaf(tag);
        root.push_child(left, None, 0)?;
        root.push_child(right, None, 0)?;
        Ok(TypeOutcome::Cells(root))
    }

    /// Pushes one member fact of a type-position object literal and returns
    /// its fact ordinal, member name bytes, and member flags. Unnamed
    /// signature members (call and construct signatures) carry no linkable
    /// name and are skipped.
    fn push_type_literal_member(
        &mut self,
        member_span: Span,
        depth: u8,
    ) -> Result<Option<MemberLink<'source>>, TypeScriptCollectError> {
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
                    SemanticFact::new(EntityKind::Field, name, LEAF_PRODUCT)
                        .with_extension(extension),
                    cells,
                );
                let ordinal = self.push(fact)?;
                self.register(ordinal, member_span, key_span, EntityKind::Field);
                return Ok(Some((ordinal, name, flags)));
            }
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
                )?;
                let flags = if method.optional {
                    SemanticTypeChild::FLAG_OPTIONAL
                } else {
                    0
                };
                return Ok(Some((ordinal, name, flags)));
            }
        }
        Ok(None)
    }
}

/// Streams every OXC-bound declaration with its declared types, signatures,
/// references, and documentation into canonical declaration facts, running
/// the configured TypeScript checker authority as the type plane beside the
/// in-process syntax projection.
///
/// The exact TypeScript checker is required once for the source. Any checker
/// failure is a typed authority rejection; syntax projection is never used as
/// a fallback for missing semantic facts.
///
/// This accepts no reconstructed token stream. OXC contributes its distinct
/// syntax, lexical-binding, source-coordinate, and declaration authorities.
pub(crate) fn collect<'source>(
    profile: TypeScriptSource,
    source: &'source [u8],
    facts: &mut FactSet<'source>,
) -> Result<(), TypeScriptCollectError> {
    let report = Checker::default()
        .run(profile, source)
        .map_err(|cause| TypeScriptCollectError::Authority(AuthorityError::Checker { cause }))?;
    collect_with_checker(profile, source, Some(&report), facts)
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
    checker: Option<&'report compiler_languages_typescript::Report>,
    facts: &mut FactSet<'source>,
) -> Result<(), TypeScriptCollectError> {
    let source = std::str::from_utf8(source).map_err(TypeScriptCollectError::Utf8)?;
    let index = match checker {
        Some(report) => Some(CheckerIndex::bind(report, source).map_err(|cause| {
            TypeScriptCollectError::Authority(AuthorityError::Checker { cause })
        })?),
        None => None,
    };
    with_analysis(profile, source, |module| {
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
            name_starts: vec![UNSET; MAX_EMISSION_FACTS].into_boxed_slice(),
            name_ends: vec![UNSET; MAX_EMISSION_FACTS].into_boxed_slice(),
            decl_starts: vec![UNSET; MAX_EMISSION_FACTS].into_boxed_slice(),
            decl_ends: vec![UNSET; MAX_EMISSION_FACTS].into_boxed_slice(),
            fact_kinds: vec![EntityKind::Function; MAX_EMISSION_FACTS].into_boxed_slice(),
            import_modules: vec![ImportModule::unset(); MAX_EMISSION_FACTS].into_boxed_slice(),
            import_module_len: 0,
            checker: index,
            pending_type_parameters: 0,
            extension_type_parameters: vec![0; MAX_EMISSION_FACTS].into_boxed_slice(),
        };
        projector.run()
    })
    .map_err(TypeScriptCollectError::Authority)?
}

impl<'x, 'report, 'source> Projector<'x, 'report, 'source> {
    /// Runs the ordered projection: the self-nominal declaration pass, the
    /// alias/member/signature/variable pass, the checker computed pass, the
    /// narrowing pass, the reference pass, the checker-only reference pass,
    /// then the documentation pass.
    fn run(&mut self) -> Result<(), TypeScriptCollectError> {
        self.pass_declarations()?;
        self.pass_members()?;
        self.pass_checker()?;
        self.pass_narrowings()?;
        self.pass_references()?;
        self.pass_checker_references()?;
        self.pass_docs()?;
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
                self.register(ordinal, declaration_span, alias.id.span, EntityKind::Alias);
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
                );
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
                        flags: if parameter.optional {
                            SemanticTypeChild::FLAG_OPTIONAL
                        } else {
                            0
                        },
                    })?;
                }
                if let Some(rest) = function.params.rest.as_ref() {
                    params.push(ParamRow {
                        name: rest.rest.span(),
                        annotation: rest
                            .type_annotation
                            .as_ref()
                            .map(|annotation| annotation.type_annotation.span()),
                        flags: SemanticTypeChild::FLAG_REST,
                    })?;
                }
                let result = function
                    .return_type
                    .as_ref()
                    .map(|returned| returned.type_annotation.span());
                self.push_signature(id.span, declaration_span, &rows, &params, result)?;
            } else if let Some(declarator) = kind.as_variable_declarator() {
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
                let cells = match declarator.type_annotation.as_ref() {
                    Some(annotation) => {
                        let inner = annotation.type_annotation.span();
                        self.owner_cells(inner.start, inner.end, 0)?
                    }
                    None => TypeCells::unknown(TypeReason::Unannotated),
                };
                let extension = self.extension(type_parameter_start)?;
                let fact = with_cells(
                    SemanticFact::new(entity_kind, name_bytes, LEAF_PRODUCT)
                        .with_extension(extension),
                    cells,
                );
                let ordinal = self.push(fact)?;
                self.register(ordinal, declaration_span, name_span, entity_kind);
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
                let ordinal = self.push(fact)?;
                self.register(ordinal, declaration_span, key_span, EntityKind::Field);
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
                self.push_signature(key_span, declaration_span, &rows, &params, result)?;
            } else if let Some(index_signature) = kind.as_ts_index_signature() {
                let inner = index_signature.type_annotation.type_annotation.span();
                let name_span = Span::new(declaration_span.start, inner.start);
                let name_bytes =
                    self.slice_span(name_span)
                        .ok_or(TypeScriptCollectError::Span {
                            start: name_span.start,
                            end: name_span.end,
                        })?;
                let type_parameter_start = coordinate(self.facts.type_parameter_len)?;
                let cells = self.owner_cells(inner.start, inner.end, 0)?;
                let extension = self.extension(type_parameter_start)?;
                let fact = with_cells(
                    SemanticFact::new(EntityKind::Field, name_bytes, LEAF_PRODUCT)
                        .with_extension(extension),
                    cells,
                );
                let ordinal = self.push(fact)?;
                self.register(ordinal, declaration_span, name_span, EntityKind::Field);
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
                let cells = match definition.type_annotation.as_ref() {
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
                self.register(ordinal, declaration_span, key_span, EntityKind::Field);
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
                self.push_signature(key_span, declaration_span, &rows, &params, result)?;
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
                self.register(ordinal, declaration_span, name_span, EntityKind::Variant);
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
        let registry = FactRegistry {
            source: self.source,
            decl_starts: &self.decl_starts,
            decl_ends: &self.decl_ends,
            name_starts: &self.name_starts,
            name_ends: &self.name_ends,
            fact_kinds: &self.fact_kinds,
            fact_len: u32::try_from(self.facts.len()).map_err(|_| lane_rejection())?,
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
            let row =
                intern_computed_tree(&registry, self.facts, tree, owner, 0, SpellDomain::Owner)?;
            let _ordinal = row
                .checked_sub(COMPUTED_ROW_BASE)
                .ok_or_else(lane_rejection)?;
            let type_parameters = self
                .extension_type_parameters
                .get(owner_index)
                .copied()
                .ok_or_else(lane_rejection)?;
            let extension = EmissionExtension::TypeScript(compiler_ir::TypeScriptFacts {
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
        let registry = FactRegistry {
            source: self.source,
            decl_starts: &self.decl_starts,
            decl_ends: &self.decl_ends,
            name_starts: &self.name_starts,
            name_ends: &self.name_ends,
            fact_kinds: &self.fact_kinds,
            fact_len: u32::try_from(self.facts.len()).map_err(|_| lane_rejection())?,
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
            intern_computed_tree(&registry, self.facts, tree, owner, 0, site)?;
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
    /// and a checker-resolved language-library base upgrades to an
    /// npm-universe oracle key.
    fn pass_references(&mut self) -> Result<(), TypeScriptCollectError> {
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
    /// when one exists and its category from the reference flags and call
    /// position.
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
                    let name = self.text_span(span).unwrap_or("");
                    let key = ForeignKey::new(
                        ForeignOrigin::Universe {
                            ecosystem: NPM_ECOSYSTEM,
                        },
                        name,
                        name,
                        None,
                    )
                    .map_err(foreign_fault)?;
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
    /// misattributed.
    fn commit_occurrence(
        &mut self,
        owner: u32,
        span: Span,
        kind: ReferenceKind,
        target: OccurrenceTarget<'source>,
        confidence: OccurrenceConfidence,
    ) -> Result<(), TypeScriptCollectError> {
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
    /// oracle confidence, and a language-library foreign base becomes an
    /// npm-universe occurrence at oracle confidence whose path is the exact
    /// use-site spelling. Foreign package modules stay at the honest
    /// syntactic `TypeReason`: their module bytes are not spelled anywhere
    /// in this source, and every borrowed wire cell must stay
    /// source-backed.
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
        if resolved.module == Some("typescript")
            && let Some(path) = self.text_span(span)
            && !path.is_empty()
        {
            let key = ForeignKey::new(
                ForeignOrigin::Universe {
                    ecosystem: NPM_ECOSYSTEM,
                },
                path,
                path,
                None,
            )
            .map_err(foreign_fault)?;
            return Ok(Some((
                OccurrenceTarget::Foreign(key),
                OccurrenceConfidence::Oracle,
            )));
        }
        Ok(None)
    }

    /// Pass six: the checker's references that OXC never visits — property
    /// accesses and other member sites OXC does not bind. Every published
    /// same-file target commits a Local occurrence at oracle confidence
    /// (a call site targets the exact overload member the checker picked);
    /// a language-library foreign base commits its npm-universe oracle
    /// key. Every other site stays absent rather than guessed, and a span
    /// already covered by an OXC reference is never committed twice.
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
            let Some(owner) = self.owning_fact(reference.span.start) else {
                continue;
            };
            let Some((target, confidence, kind)) = self.checker_only_target(reference)? else {
                continue;
            };
            let span = Span::new(reference.span.start, reference.span.end);
            self.commit_occurrence(owner, span, kind, target, confidence)?;
        }
        Ok(())
    }

    /// Reports whether a committed occurrence already names the exact
    /// absolute source span.
    fn occurrence_covers(&self, span: Utf8Span) -> Result<bool, TypeScriptCollectError> {
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
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Resolves the target, confidence, and kind of one checker-only
    /// reference site. Same-file targets resolve through the published fact
    /// of the target's binding name; language-library bases resolve through
    /// the use-site spelling against the npm universe; every other base is
    /// left absent instead of guessed.
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
        let call = reference.overload_index.is_some();
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
        if reference.module == Some("typescript")
            && let Some(path) = self.slice_span(Span::new(reference.span.start, reference.span.end))
            && let Ok(path) = core::str::from_utf8(path)
            && !path.is_empty()
        {
            let key = ForeignKey::new(
                ForeignOrigin::Universe {
                    ecosystem: NPM_ECOSYSTEM,
                },
                path,
                path,
                None,
            )
            .map_err(foreign_fault)?;
            let kind = if call {
                ReferenceKind::FunctionCall
            } else {
                ReferenceKind::VariableUse
            };
            return Ok(Some((
                OccurrenceTarget::Foreign(key),
                OccurrenceConfidence::Oracle,
                kind,
            )));
        }
        Ok(None)
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

    /// Stages one JSDoc line's bounded fragment run and commits it after the
    /// inter-line soft break.
    fn push_jsdoc_line(
        &mut self,
        owner: u32,
        line: &'source [u8],
        leading_break: bool,
    ) -> Result<bool, TypeScriptCollectError> {
        let mut staged = [DocFragmentInput::SoftBreak; MAX_JSDOC_SEGMENTS];
        let mut len = 0_usize;
        let mut overflow = false;
        let mut cursor = 0_usize;
        loop {
            let Some((at, is_link)) = earliest_inline_tag(line, cursor) else {
                if let Some(rest) = line.get(cursor..)
                    && !rest.is_empty()
                {
                    stage_fragment(
                        &mut staged,
                        &mut len,
                        &mut overflow,
                        DocFragmentInput::Text(rest),
                    );
                }
                break;
            };
            if let Some(before) = line.get(cursor..at)
                && !before.is_empty()
            {
                stage_fragment(
                    &mut staged,
                    &mut len,
                    &mut overflow,
                    DocFragmentInput::Text(before),
                );
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
                        stage_fragment(&mut staged, &mut len, &mut overflow, fragment);
                    }
                    cursor = close + 1;
                }
                None => {
                    if let Some(rest) = line.get(cursor..)
                        && !rest.is_empty()
                    {
                        stage_fragment(
                            &mut staged,
                            &mut len,
                            &mut overflow,
                            DocFragmentInput::Text(rest),
                        );
                    }
                    break;
                }
            }
        }
        if overflow {
            // A JSDoc line with more segments than the staged bound cannot be
            // emitted without truncation, which the lane forbids.
            return Err(fault(FactFault::DocCapacity));
        }
        if len == 0 {
            return Ok(false);
        }
        if leading_break {
            self.facts
                .push_doc(owner, DocFragmentInput::SoftBreak)
                .map_err(fault)?;
        }
        for fragment in staged.iter().take(len) {
            self.facts.push_doc(owner, *fragment).map_err(fault)?;
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

impl<'a, 'source> FactRegistry<'a, 'source> {
    /// Resolves one source position to the fact whose binding name starts
    /// exactly there.
    fn fact_at_name_start(&self, start: u32) -> Option<u32> {
        for ordinal in 0..self.fact_len {
            let index = usize::try_from(ordinal).ok()?;
            if self.name_starts.get(index) == Some(&start) {
                return Some(ordinal);
            }
        }
        None
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
}

/// The source span that owns one computed tree's member spellings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SpellDomain {
    /// The owning declaration's declaring span.
    Owner,
    /// One explicit source range — a narrowing's assignment site.
    Range(u32, u32),
}

/// Interns one checker-computed type as a computed type row owned by
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
        TypeTree::This => {
            match registry.source_spelling(spell, b"this", owner) {
                Some(spelling) => {
                    let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::SelfType);
                    record.text = Some(spelling);
                    intern_computed_leaf(facts, record, owner)
                }
                None => intern_computed_leaf(facts, unknown_record(TypeReason::OracleGap), owner),
            }
        }
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
            let constraint = intern_computed_tree(registry, facts, constraint, owner, depth, spell)?;
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
            record.text = registry.source_spelling(spell, parameter.as_bytes(), owner);
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
                    let Some(source_text) = registry.source_spelling(spell, text.as_bytes(), owner) else {
                        return intern_computed_leaf(
                            facts,
                            unknown_record(TypeReason::OracleGap),
                            owner,
                        );
                    };
                    text_parts[position] = Some(source_text);
                }
            }
            let record = SemanticTypeRecord::leaf(SemanticTypeTag::TemplateLiteral);
            for (position, part) in parts.iter().enumerate() {
                match part {
                    TemplatePart::Text { .. } => {
                        let text = text_parts[position].ok_or_else(|| {
                            computed_fault(registry, owner, FactFault::TypeChildCapacity)
                        })?;
                        facts
                            .computed_type_text_child(text)
                            .map_err(|cause| computed_fault(registry, owner, cause))?;
                    }
                    TemplatePart::Type { r#type } => {
                        let child = intern_computed_tree(
                            registry,
                            facts,
                            r#type,
                            owner,
                            depth.saturating_add(1),
                            spell,
                        )?;
                        facts
                            .computed_type_child(child, None, 0)
                            .map_err(|cause| computed_fault(registry, owner, cause))?;
                    }
                }
            }
            facts
                .intern_computed_type_row(owner, record)
                .map_err(|cause| computed_fault(registry, owner, cause))
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
            // Members carry names and flags, so each member's row is
            // interned first and then linked with its spelling-domain
            // source spelling.
            let mut rows = [0_u32; MAX_TYPE_CHILDREN];
            if members.len() > MAX_TYPE_CHILDREN {
                return Err(computed_fault(
                    registry,
                    owner,
                    FactFault::TypeChildCapacity,
                ));
            }
            for (position, member) in members.iter().enumerate() {
                let row = intern_computed_tree(
                    registry,
                    facts,
                    &member.member_type,
                    owner,
                    depth.saturating_add(1),
                    spell,
                )?;
                #[expect(
                    clippy::indexing_slicing,
                    reason = "the preceding member-count check proves every position fits the fixed row lane"
                )]
                {
                    rows[position] = row;
                }
            }
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::AnonymousRecord);
            record.payload0 = u32::from(AnonRecordForm::Interface);
            for (position, member) in members.iter().enumerate() {
                let mut flags = 0_u8;
                if member.optional {
                    flags |= SemanticTypeChild::FLAG_OPTIONAL;
                }
                if member.readonly {
                    flags |= SemanticTypeChild::FLAG_READONLY;
                }
                let spelling = registry
                    .source_spelling(spell, member.name.as_bytes(), owner)
                    .ok_or_else(|| {
                        computed_fault(
                            registry,
                            owner,
                            FactFault::TypeChild {
                                position,
                                fault: compiler_ir::SemanticTypeFault::ChildNameRequired {
                                    tag: SemanticTypeTag::AnonymousRecord,
                                    position: position as u32,
                                },
                            },
                        )
                    })?;
                #[expect(
                    clippy::indexing_slicing,
                    reason = "the preceding member-count check proves every position fits the fixed row lane"
                )]
                let row = rows[position];
                facts
                    .computed_type_child(row, Some(spelling), flags)
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

/// Interns one computed reference. Foreign bases retain a typed external
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
            if module.is_none() {
                let mut record = unknown_record(TypeReason::UnresolvedExternal);
                record.text = registry.source_spelling(spell, name.as_bytes(), owner);
                return intern_computed_leaf(facts, record, owner);
            }
            let module_bytes = module.map_or(name.as_bytes(), str::as_bytes);
            let fragment = ExternalFragmentId::from_canonical_bytes(module_bytes);
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Nominal);
            record.nominal = Some(NominalRef::External(ExternalEntityRef::bind(fragment, 0)));
            record.text = registry.source_spelling(spell, name.as_bytes(), owner);
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
    let first = members.first().ok_or_else(lane_rejection)?;
    let second = members.get(1).ok_or_else(lane_rejection)?;
    let mut left = intern_computed_tree(
        registry,
        facts,
        first,
        owner,
        depth.saturating_add(1),
        spell,
    )?;
    let right = intern_computed_tree(
        registry,
        facts,
        second,
        owner,
        depth.saturating_add(1),
        spell,
    )?;
    left = intern_computed_row(
        registry,
        facts,
        SemanticTypeRecord::leaf(tag),
        owner,
        &[left, right],
    )?;
    for member in members.iter().skip(2) {
        let right = intern_computed_tree(
            registry,
            facts,
            member,
            owner,
            depth.saturating_add(1),
            spell,
        )?;
        left = intern_computed_row(
            registry,
            facts,
            SemanticTypeRecord::leaf(tag),
            owner,
            &[left, right],
        )?;
    }
    Ok(left)
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
fn intern_computed_row<'a, 'source>(
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

fn intern_computed_leaf<'a, 'source>(
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

/// The closed primitive record of one checker literal base.
fn checker_literal(
    base: compiler_languages_typescript::LiteralBase,
) -> SemanticTypeRecord<'static> {
    match base {
        compiler_languages_typescript::LiteralBase::Number => {
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
            record.payload0 = u32::from(PrimitiveShape::Integer);
            record.payload1 = (32_u32 << 1) | SemanticTypeRecord::INTEGER_SIGNED_FLAG;
            record
        }
        compiler_languages_typescript::LiteralBase::String => {
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
            record.payload0 = u32::from(PrimitiveShape::Str);
            record
        }
        compiler_languages_typescript::LiteralBase::Boolean => {
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
            record.payload0 = u32::from(PrimitiveShape::Bool);
            record
        }
        compiler_languages_typescript::LiteralBase::Bigint => {
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
            record.payload0 = u32::from(PrimitiveShape::Builtin);
            record.text = Some(&b"bigint"[..]);
            record
        }
    }
}

/// Byte width of the `@code`/`@link` inline-tag headers.
const TAG_WIDTH: usize = 6;

/// Stages one JSDoc fragment, marking overflow past the bounded segment lane.
fn stage_fragment<'source>(
    staged: &mut [DocFragmentInput<'source>; MAX_JSDOC_SEGMENTS],
    len: &mut usize,
    overflow: &mut bool,
    fragment: DocFragmentInput<'source>,
) {
    match staged.get_mut(*len) {
        Some(slot) => {
            *slot = fragment;
            *len += 1;
        }
        None => *overflow = true,
    }
}

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
