//! Projects source-backed OXC syntax, bindings, and spans into the canonical
//! declaration lane with recursive declared types, exact signatures, resolved
//! references, and JSDoc documentation.
//! Keeps TypeScript syntax and lexical authority in-process beside the configured checker.
//! Contains no token reconstruction, fallback collector, or declaration guessing.

use compiler_ir::{
    AnonRecordForm, ComputedType, ComputedTypeId, DocFragmentInput, DocLinkTarget, EntityId,
    EntityKind, ForeignKey, ForeignOrigin, IrBuilder, LatticeMappedModifier, NominalRef,
    Occurrence, OccurrenceConfidence, OccurrenceTarget, PackageLineage, PrimitiveShape,
    ProductChildRole, ReferenceKind, RelSpan, SemanticProductConstructor, SemanticTypeChild,
    SemanticTypeRecord, SemanticTypeTag, TypeId, TypeParameterListId, TypeQuery, TypeReason,
    TypeWidth,
};
use compiler_languages_typescript::{
    AuthorityError, BoundReference, Checker, CheckerError, CheckerIndex, GetSpan, Origin,
    ReferenceFlags, Semantic, Span, SymbolFlags, SymbolId, TypeTree, Utf8Span, with_analysis,
};
use compiler_vocabulary::TypeScriptSource;

use crate::{
    lower::{
        EmissionExtension, FactFault, FactSet, FactTypeChild, LEAF_PRODUCT, MAX_EMISSION_FACTS,
        MAX_FACT_CHILDREN, MAX_TYPE_CHILDREN, SemanticFact, push_fact,
    },
    types::LoweringUnsupported,
};

/// Bound of one declaration's staged type-parameter rows; a source with more
/// generic parameters on one declaration is a typed lane rejection.
const MAX_DECL_TYPE_PARAMETERS: usize = 16;
/// Recursion bound for type-expression lowering; deeper expressions are
/// honestly unknown with [`TypeReason::TruncatedAtDepthLimit`].
const MAX_TYPE_DEPTH: u8 = 24;
/// Sentinel marking an unset projection-table row.
const UNSET: u32 = u32::MAX;
/// First pool-local ordinal of an anonymous type row, mirroring the frozen
/// emission lane's own constant (`lower.rs` keeps it private; the value is
/// the fact-lane bound by definition).
const ANONYMOUS_ROW_BASE: u32 = MAX_EMISSION_FACTS as u32;
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
    /// An OXC declaration span could not name a slice of the admitted source.
    Span { start: u32, end: u32 },
}

/// The lane's one coarse terminal, shared by every bounded-lane rejection
/// exactly as [`push_fact`] reports them.
fn lane_rejection() -> TypeScriptCollectError {
    TypeScriptCollectError::Lowering(LoweringUnsupported::NoSupportedDeclaration)
}

/// Maps one typed lane rejection onto the coarse lane terminal, retaining the
/// mandated mapping from every [`FactFault`] to [`LoweringUnsupported`].
fn fault(_cause: FactFault) -> TypeScriptCollectError {
    lane_rejection()
}

/// Maps one foreign-key rejection onto the coarse lane terminal. The path and
/// display cells here are proven non-empty source slices, so a rejection
/// means a malformed cross-package path and the lane cannot proceed.
fn foreign_fault(_cause: compiler_ir::ForeignKeyFault) -> TypeScriptCollectError {
    lane_rejection()
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
            None => Err(lane_rejection()),
        }
    }

    fn iter(&self) -> impl Iterator<Item = &TypeParamRow> {
        self.rows.iter().take(self.len)
    }
}

/// One staged parameter: its declared name span plus the optional span of its
/// annotated type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ParamRow {
    name: Span,
    annotation: Option<Span>,
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
            None => Err(lane_rejection()),
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
                Err(lane_rejection())
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
    source: &'source str,
    facts: &'x mut FactSet<'source>,
    /// Binding-name span start per pushed fact (`UNSET` when unregistered).
    name_starts: [u32; MAX_EMISSION_FACTS],
    name_ends: [u32; MAX_EMISSION_FACTS],
    /// Declaring-node span start per pushed fact.
    decl_starts: [u32; MAX_EMISSION_FACTS],
    decl_ends: [u32; MAX_EMISSION_FACTS],
    fact_kinds: [EntityKind; MAX_EMISSION_FACTS],
    /// One row per import-binding fact: the module and imported-name spans
    /// its foreign keys are built from.
    import_modules: [ImportModule; MAX_EMISSION_FACTS],
    import_module_len: usize,
    /// The span-bound checker report, when the authority ran.
    checker: Option<CheckerIndex<'report>>,
    /// The pooled type-parameter start of the fact about to be pushed; every
    /// push path builds its extension immediately before pushing.
    pending_type_parameters: u32,
    /// Pooled type-parameter start per pushed fact, retained so the checker
    /// pass can re-attach a completed extension with the computed cell.
    extension_type_parameters: [u32; MAX_EMISSION_FACTS],
    /// The computed-cell proof mint: one session builder whose computed
    /// arena allocates exactly one node per computed type row, in lane
    /// order, so every minted coordinate equals the row's final type-lane
    /// ordinal (anonymous rows remap to `row - ANONYMOUS_ROW_BASE`).
    mint: ComputedMint,
}

/// The session computed-cell mint. Every mint interns one genuine computed
/// node into its own arena at the next dense coordinate, so a minted proof's
/// coordinate is exactly the number of computed rows interned before it.
struct ComputedMint {
    builder: IrBuilder,
    count: u32,
}

impl ComputedMint {
    fn mint(&mut self, row: u32) -> Result<ComputedTypeId, TypeScriptCollectError> {
        while self.count < row {
            self.intern(self.count)?;
            self.count = self.count.checked_add(1).ok_or_else(lane_rejection)?;
        }
        if self.count != row {
            // A row behind the mint cursor cannot mint again; the lane order
            // is violated and the computed cell stays unfabricated.
            return Err(lane_rejection());
        }
        let proof = self.intern(row)?;
        self.count = row.checked_add(1).ok_or_else(lane_rejection)?;
        Ok(proof)
    }

    fn intern(&mut self, row: u32) -> Result<ComputedTypeId, TypeScriptCollectError> {
        self.builder
            .intern_computed(ComputedType::TypeOf(TypeQuery::Entity(EntityId::new(row))))
            .map_err(|_| lane_rejection())
    }
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
        let ordinal = push_fact(self.facts, fact).map_err(|_| lane_rejection())?;
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
                computed: None,
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
            record.payload1 = SemanticTypeRecord::RESULT_FLAG;
        }
        let extension = self.extension(type_parameter_start)?;
        let mut fact = SemanticFact::new(
            EntityKind::Function,
            name_bytes,
            SemanticProductConstructor::function(param_count, result_count),
        )
        .typed(record)
        .with_extension(extension);
        for ordinal in param_ordinals.iter().take(params.count()) {
            fact = fact.child(ProductChildRole::FunctionParameter, *ordinal);
            fact = fact.type_child(*ordinal, None, 0);
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
            let span = RelSpan::new(relative_start, relative_end).map_err(|_| lane_rejection())?;
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

    /// The honest backlog record for a proven-but-unrepresented construct:
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
        let semantic = self.semantic;
        let next_depth = depth.saturating_add(1);
        for node in semantic.nodes().iter() {
            let kind = node.kind();
            if kind.span() != span {
                continue;
            }

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
                let mut cells = TypeCells::leaf(SemanticTypeTag::Union);
                for member in union.types.iter() {
                    let member_span = member.span();
                    let target =
                        self.child_target(member_span.start, member_span.end, next_depth)?;
                    cells.push_child(target, None, 0)?;
                }
                return Ok(TypeOutcome::Cells(cells));
            }
            if let Some(intersection) = kind.as_ts_intersection_type() {
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
                    for wrapped in semantic.nodes().iter() {
                        let wrapped_kind = wrapped.kind();
                        if wrapped_kind.span() != element_span {
                            continue;
                        }
                        if let Some(named) = wrapped_kind.as_ts_named_tuple_member() {
                            label = Some(named.label.span);
                            inner = named.element_type.span();
                        } else if let Some(optional) = wrapped_kind.as_ts_optional_type() {
                            // Tuple elements carry no flag cell in the
                            // lattice; optionality has no wire form here.
                            inner = optional.type_annotation.span();
                        } else if let Some(rest) = wrapped_kind.as_ts_rest_type() {
                            inner = rest.type_annotation.span();
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
                    cells.push_child(target, name, 0)?;
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
                    cells.push_child(target, None, 0)?;
                }
                let returned = function_type.return_type.type_annotation.span();
                let target = self.child_target(returned.start, returned.end, next_depth)?;
                cells.record.payload1 = SemanticTypeRecord::RESULT_FLAG;
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
                // Genuinely unresolvable names stay honestly external; a
                // checker-resolved foreign module type is known and named
                // but has no foreign-nominal row form in the closed lattice,
                // so its exact spelling backs the backlog reason instead.
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
                let full = self.slice_span(span).unwrap_or(&[]);
                let mut cells = TypeCells::leaf(SemanticTypeTag::Mapped);
                cells.record.payload0 = mapped_readonly_modifier(full);
                cells.record.payload1 = mapped_optional_modifier(full);
                cells.record.text = self.slice_span(mapped.key.span);
                let constraint = mapped.constraint.span();
                let constraint_target =
                    self.child_target(constraint.start, constraint.end, next_depth)?;
                let value_target = match mapped.type_annotation.as_ref() {
                    Some(value) => {
                        let value_span = value.span();
                        self.child_target(value_span.start, value_span.end, next_depth)?
                    }
                    None => self.unannotated_fact(span)?,
                };
                cells.push_child(constraint_target, None, 0)?;
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
                let mut cells = TypeCells::leaf(SemanticTypeTag::Array);
                cells.record.text = Some(&b"[]"[..]);
                cells.push_child(element_target, None, 0)?;
                return Ok(TypeOutcome::Cells(cells));
            }
            if let Some(operator) = kind.as_ts_type_operator() {
                let full = self.slice_span(span).unwrap_or(&[]);
                if full.starts_with(b"readonly") {
                    let inner = operator.type_annotation.span();
                    let inner_target = self.child_target(inner.start, inner.end, next_depth)?;
                    let mut cells = TypeCells::leaf(SemanticTypeTag::Annotated);
                    cells.record.text = Some(&b"readonly"[..]);
                    cells.push_child(inner_target, None, 0)?;
                    return Ok(TypeOutcome::Cells(cells));
                }
                return Ok(TypeOutcome::Cells(self.unrepresented(span)));
            }
            if kind.as_ts_this_type().is_some() {
                return Ok(TypeOutcome::Cells(TypeCells::leaf(
                    SemanticTypeTag::SelfType,
                )));
            }
            if kind.as_ts_template_literal_type().is_some() {
                // The frozen lane commits every type-record child as a nested
                // type coordinate, so a template literal's literal text parts
                // have no wire form; the tag alone proves the construct.
                return Ok(TypeOutcome::Cells(TypeCells::leaf(
                    SemanticTypeTag::TemplateLiteral,
                )));
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

    /// Pushes one member fact of a type-position object literal and returns
    /// its fact ordinal, member name bytes, and member flags. Unnamed
    /// signature members (call and construct signatures) carry no linkable
    /// name and are skipped.
    fn push_type_literal_member(
        &mut self,
        member_span: Span,
        depth: u8,
    ) -> Result<Option<MemberLink<'source>>, TypeScriptCollectError> {
        let semantic = self.semantic;
        for probed in semantic.nodes().iter() {
            let member_kind = probed.kind();
            if member_kind.span() != member_span {
                continue;
            }
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
/// The exact TypeScript checker is spawned once for the source; when the
/// tool or its `typescript` module is unavailable the lane proceeds at OXC
/// fidelity with every checker-derived cell absent. A checker that RAN and
/// violated its protocol is a typed authority rejection.
///
/// This accepts no reconstructed token stream. OXC contributes its distinct
/// syntax, lexical-binding, source-coordinate, and declaration authorities.
pub(crate) fn collect<'source>(
    profile: TypeScriptSource,
    source: &'source [u8],
    facts: &mut FactSet<'source>,
) -> Result<(), TypeScriptCollectError> {
    let report = match Checker::default().run(profile, source) {
        Ok(report) => Some(report),
        Err(CheckerError::ToolingUnavailable { .. } | CheckerError::ModuleUnavailable { .. }) => {
            None
        }
        Err(cause) => {
            return Err(TypeScriptCollectError::Authority(AuthorityError::Checker {
                cause,
            }));
        }
    };
    collect_with_checker(profile, source, report.as_ref(), facts)
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
            source,
            facts,
            name_starts: [UNSET; MAX_EMISSION_FACTS],
            name_ends: [UNSET; MAX_EMISSION_FACTS],
            decl_starts: [UNSET; MAX_EMISSION_FACTS],
            decl_ends: [UNSET; MAX_EMISSION_FACTS],
            fact_kinds: [EntityKind::Function; MAX_EMISSION_FACTS],
            import_modules: [ImportModule::unset(); MAX_EMISSION_FACTS],
            import_module_len: 0,
            checker: index,
            pending_type_parameters: 0,
            extension_type_parameters: [0; MAX_EMISSION_FACTS],
            mint: ComputedMint {
                builder: IrBuilder::new(),
                count: 0,
            },
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
                    })?;
                }
                if let Some(rest) = function.params.rest.as_ref() {
                    params.push(ParamRow {
                        name: rest.rest.span(),
                        annotation: rest
                            .type_annotation
                            .as_ref()
                            .map(|annotation| annotation.type_annotation.span()),
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
    /// declaration whose name binds to a published fact receives one computed
    /// type row in the anonymous row pool — compound shapes interning their
    /// children first so the pooled lane stays topologically backward — and
    /// a completed extension whose computed cell carries the row's coordinate.
    ///
    /// The computed cell's proof is minted through the session builder, which
    /// allocates exactly one computed node per computed row in lane order, so
    /// every minted coordinate equals that row's final type-lane ordinal
    /// (anonymous rows remap to `row - ANONYMOUS_ROW_BASE` at admission).
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
            let ordinal = row
                .checked_sub(ANONYMOUS_ROW_BASE)
                .ok_or_else(lane_rejection)?;
            let proof = self.mint.mint(ordinal)?;
            let type_parameters = self
                .extension_type_parameters
                .get(owner_index)
                .copied()
                .ok_or_else(lane_rejection)?;
            let extension = EmissionExtension::TypeScript(compiler_ir::TypeScriptFacts {
                type_parameters: TypeParameterListId::new(type_parameters),
                declared: Some(TypeId::new(owner)),
                computed: Some(proof),
            });
            self.facts
                .attach_extension(owner_index, extension)
                .map_err(fault)?;
        }
        Ok(())
    }

    /// Pass four: the checker's assignment narrowings. Every reported
    /// narrowing whose declaration name binds to a published fact receives
    /// one extra owned computed row in the anonymous pool — the
    /// control-flow-sensitive type at that exact assignment site, a fact
    /// the syntax plane cannot see. The extension's computed cell keeps
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
        let relative = RelSpan::new(relative_start, relative_end).map_err(|_| lane_rejection())?;
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
    /// syntactic degradation: their module bytes are not spelled anywhere
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
        let resolved = checker
            .references()
            .find(|reference| reference.span.start == span.start && reference.span.end == span.end);
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
        let resolved = checker.references().find(|reference| {
            reference.span.start == name_span.start && reference.span.end == name_span.end
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
            .references()
            .find(|reference| {
                reference.span.start == name_span.start && reference.span.end == name_span.end
            })
            .is_some_and(|reference| reference.module.is_some())
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
        let resolved = checker
            .references()
            .find(|reference| reference.span.start == span.start && reference.span.end == span.end);
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
            return Err(lane_rejection());
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

/// Interns one checker-computed type as an anonymous type row owned by
/// `owner`, interning every child row first so the pooled lane stays
/// topologically backward. Returns the row's lane coordinate
/// (`ANONYMOUS_ROW_BASE` plus its pool ordinal).
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
        TypeTree::This => intern_computed_leaf(
            facts,
            SemanticTypeRecord::leaf(SemanticTypeTag::SelfType),
            owner,
        ),
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
        TypeTree::Union { members } => {
            let children = intern_computed_children(registry, facts, members, owner, depth, spell)?;
            intern_computed_row(
                facts,
                SemanticTypeRecord::leaf(SemanticTypeTag::Union),
                owner,
                &children,
            )
        }
        TypeTree::Intersection { members } => {
            let children = intern_computed_children(registry, facts, members, owner, depth, spell)?;
            intern_computed_row(
                facts,
                SemanticTypeRecord::leaf(SemanticTypeTag::Intersection),
                owner,
                &children,
            )
        }
        TypeTree::Tuple { elements } => {
            let children =
                intern_computed_children(registry, facts, elements, owner, depth, spell)?;
            intern_computed_row(
                facts,
                SemanticTypeRecord::leaf(SemanticTypeTag::Tuple),
                owner,
                &children,
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
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Array);
            record.text = Some(&b"[]"[..]);
            intern_computed_row(facts, record, owner, &children)
        }
        TypeTree::Function { parameters, result } => {
            let mut children = [0_u32; MAX_TYPE_CHILDREN];
            let mut len = 0_usize;
            for parameter in parameters {
                let slot = children.get_mut(len).ok_or_else(lane_rejection)?;
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
            let slot = children.get_mut(len).ok_or_else(lane_rejection)?;
            *slot = result_row;
            len += 1;
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::FunctionPointer);
            record.payload1 = SemanticTypeRecord::RESULT_FLAG;
            intern_computed_row(facts, record, owner, &children[..len])
        }
        TypeTree::Object { members } => {
            // Members carry names and flags, so each member's row is
            // interned first and then linked with its spelling-domain
            // source spelling.
            let mut rows = [0_u32; MAX_TYPE_CHILDREN];
            if members.len() > MAX_TYPE_CHILDREN {
                return Err(lane_rejection());
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
                if let Some(slot) = rows.get_mut(position) {
                    *slot = row;
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
                    .ok_or_else(lane_rejection)?;
                let row = rows.get(position).copied().ok_or_else(lane_rejection)?;
                facts
                    .anonymous_type_child(row, Some(spelling), flags)
                    .map_err(fault)?;
            }
            facts
                .intern_anonymous_type_row(owner, record)
                .map_err(fault)
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

/// Interns one computed reference: a bare same-file type names its local
/// nominal, a foreign module type names the checker-resolved spelling —
/// the closed lattice has no foreign-nominal row form — and applied
/// foreign bases keep their exact argument structure.
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
    let base = match local {
        Some(fact) => fact,
        None => {
            // A checker-resolved foreign module type is known and named,
            // but the closed lattice has no foreign-nominal row form; the
            // record keeps the spelling-domain source slice so the
            // backlog stays countable.
            let record = match registry.source_spelling(spell, name.as_bytes(), owner) {
                Some(spelling) => {
                    let mut record = unknown_record(TypeReason::NoIrRepresentation);
                    record.text = Some(spelling);
                    record
                }
                None => unknown_record(TypeReason::OracleGap),
            };
            intern_computed_leaf(facts, record, owner)?
        }
    };
    if args.is_empty() {
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
        facts,
        SemanticTypeRecord::leaf(SemanticTypeTag::Apply),
        owner,
        &children[..len],
    )
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
    facts: &mut FactSet<'source>,
    record: SemanticTypeRecord<'source>,
    owner: u32,
    children: &[u32],
) -> Result<u32, TypeScriptCollectError> {
    for child in children {
        facts.anonymous_type_child(*child, None, 0).map_err(fault)?;
    }
    facts
        .intern_anonymous_type_row(owner, record)
        .map_err(fault)
}

fn intern_computed_leaf<'a, 'source>(
    facts: &mut FactSet<'source>,
    record: SemanticTypeRecord<'source>,
    owner: u32,
) -> Result<u32, TypeScriptCollectError> {
    facts
        .intern_anonymous_type_row(owner, record)
        .map_err(fault)
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

/// The readonly modifier cell of one mapped type, read from the exact source
/// head before the bracket.
fn mapped_readonly_modifier(mapped: &[u8]) -> u32 {
    let head = mapped.split(|byte| *byte == b'[').next().unwrap_or(&[]);
    if find_sub(head, b"-readonly", 0).is_some() {
        u32::from(LatticeMappedModifier::Remove)
    } else if find_sub(head, b"readonly", 0).is_some() {
        u32::from(LatticeMappedModifier::Add)
    } else {
        u32::from(LatticeMappedModifier::Absent)
    }
}

/// The optional modifier cell of one mapped type, read from the exact source.
fn mapped_optional_modifier(mapped: &[u8]) -> u32 {
    match mapped.iter().position(|byte| *byte == b'?') {
        Some(at) if at > 0 && mapped.get(at - 1) == Some(&b'-') => {
            u32::from(LatticeMappedModifier::Remove)
        }
        Some(_) => u32::from(LatticeMappedModifier::Add),
        None => u32::from(LatticeMappedModifier::Absent),
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

#[cfg(test)]
mod tests {
    use compiler_ir::{
        DecodedDocFact, DecodedOccurrence, DecodedTypeFact, EntityKind, FragmentView,
        OccurrenceConfidence, OccurrenceTarget, PrimitiveShape, ReferenceKind, SemanticTypeRecord,
        SemanticTypeTag, SourceIdentity, TypeReason, TypeWidth,
    };
    use compiler_vocabulary::{
        CompileRecipeFact, LanguageProfile, NativeTool, Stage, TypeScriptSource,
    };
    use heart_identity::{ContentId, SourceFactDomain, ToolchainDomain};

    use super::{TypeScriptCollectError, collect, collect_with_checker};

    #[derive(Debug, thiserror::Error)]
    enum TestError {
        #[error("collect rejected the source: {0:?}")]
        Collect(TypeScriptCollectError),
        #[error("lane admission fault: {0:?}")]
        Admission(crate::lower::AdmissionFault),
        #[error("fragment validation failed")]
        Validate(#[from] compiler_ir::FragmentError),
        #[error("source length does not fit the source identity")]
        Source(#[from] core::num::TryFromIntError),
        #[error("fragment output tail changed")]
        Tail,
        #[error("no entity named {name}")]
        MissingEntity { name: String },
        #[error("no type fact for owner {owner}")]
        MissingTypeFact { owner: u32 },
        #[error("no occurrence fact")]
        MissingOccurrence,
        #[error("no documentation fact")]
        MissingDoc,
        #[error("unexpected record: expected {expected:?}, observed {observed:?}")]
        Record {
            expected: SemanticTypeTag,
            observed: SemanticTypeTag,
        },
        #[error("golden checker transcript fault: {0}")]
        Golden(String),
        #[error("no TypeScript extension section payload")]
        MissingExtension,
    }

    /// Lowers one source and admits the lane into one validated fragment.
    /// The declared-only OXC path: no checker report, no subprocess.
    fn fragment(source: &[u8]) -> Result<Vec<u8>, TestError> {
        fragment_with_checker(source, None)
    }

    /// Lowers one source with one caller-supplied checker report and admits
    /// the lane into one validated fragment, returning the exact committed
    /// bytes with the untouched tail proven unchanged.
    fn fragment_with_checker(
        source: &[u8],
        checker: Option<&compiler_languages_typescript::Report>,
    ) -> Result<Vec<u8>, TestError> {
        let mut facts = crate::lower::FactSet::new();
        collect_with_checker(TypeScriptSource::TypeScript, source, checker, &mut facts)
            .map_err(TestError::Collect)?;
        let identity = SourceIdentity {
            identity: ContentId::<SourceFactDomain>::from_canonical_bytes(source),
            byte_len: u32::try_from(source.len())?,
        };
        let recipe = CompileRecipeFact::derive(
            LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
            Stage::LowerIr,
            NativeTool::TypeScriptCompiler,
            ContentId::<SourceFactDomain>::from_canonical_bytes(source),
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"typescript-lane-toolchain"),
        );
        let mut output = vec![0xa5_u8; 65_536];
        let length = crate::lower::admit(&facts, identity, recipe, recipe.profile, &mut output)
            .map_err(TestError::Admission)?
            .len();
        if !output[length..].iter().all(|byte| *byte == 0xa5) {
            return Err(TestError::Tail);
        }
        output.truncate(length);
        Ok(output)
    }

    fn view(bytes: &[u8]) -> Result<FragmentView<'_>, TestError> {
        FragmentView::validate(bytes).map_err(TestError::Validate)
    }

    /// Offline falsifiers for the checker authority's consumption: computed
    /// cells, oracle-confidence overload targets, and the foreign-base
    /// distinction, all driven by the recorded golden checker transcript.
    mod checker_consumption {
        use super::*;

        const GOLDEN: &str =
            include_str!("../../languages/typescript/tests/transcripts/golden.json");
        const GOLDEN_SOURCE: &[u8] =
            include_bytes!("../../languages/typescript/tests/fixtures/source.ts");

        /// The wire `NONE` sentinel of an absent extension cell.
        const NONE: u32 = u32::MAX;

        fn golden() -> Result<compiler_languages_typescript::Report, TestError> {
            compiler_languages_typescript::Checker::default()
                .decode(GOLDEN.as_bytes())
                .map_err(|cause| TestError::Golden(cause.to_string()))
        }

        /// Lowers the golden fixture with the golden transcript. The owned
        /// bytes outlive the caller's borrowed view.
        fn fragment_from_golden() -> Result<Vec<u8>, TestError> {
            let report = golden()?;
            fragment_with_checker(GOLDEN_SOURCE, Some(&report))
        }

        /// Opens one validated view of the lowered golden fragment. The
        /// fragment bytes are leaked so the borrowed view can travel as
        /// `'static`; the test process is short-lived, so the leak is
        /// bounded and deliberate.
        fn golden_view() -> Result<FragmentView<'static>, TestError> {
            let leaked: &'static [u8] = Box::leak(fragment_from_golden()?.into_boxed_slice());
            FragmentView::validate(leaked).map_err(TestError::Validate)
        }

        /// Decodes one entity's TypeScript extension wire fact straight from
        /// the committed section: `(type_parameters, declared, computed)`.
        /// The TypeScript plane is plane zero; its fact rows are 12 bytes of
        /// three `u32` cells.
        fn typescript_cell(
            view: &FragmentView<'_>,
            entity: u32,
        ) -> Result<(u32, u32, u32), TestError> {
            const HEADER: usize = 16;
            const DIRECTORY: usize = 20;
            let bytes = view
                .language_extension_payload()
                .ok_or(TestError::MissingExtension)?;
            let directory = HEADER;
            let word = |at: usize| -> Result<u32, TestError> {
                let raw = bytes
                    .get(at..at.checked_add(4).ok_or(TestError::MissingExtension)?)
                    .ok_or(TestError::MissingExtension)?;
                Ok(u32::from_le_bytes(
                    raw.try_into().map_err(|_| TestError::MissingExtension)?,
                ))
            };
            let plane_kind = bytes
                .get(directory)
                .copied()
                .ok_or(TestError::MissingExtension)?;
            if plane_kind != 1 {
                return Err(TestError::MissingExtension);
            }
            let rows = word(directory + 4)?;
            let fact_count = word(directory + 8)?;
            let payload = word(directory + 12)?;
            if entity >= rows || fact_count == 0 {
                return Err(TestError::MissingExtension);
            }
            let ordinal = word(
                usize::try_from(payload).map_err(|_| TestError::MissingExtension)?
                    + usize::try_from(entity).map_err(|_| TestError::MissingExtension)? * 4,
            )?;
            if ordinal == NONE {
                return Err(TestError::MissingExtension);
            }
            let fact_at = usize::try_from(payload).map_err(|_| TestError::MissingExtension)?
                + usize::try_from(rows).map_err(|_| TestError::MissingExtension)? * 4
                + usize::try_from(ordinal).map_err(|_| TestError::MissingExtension)? * 12;
            Ok((word(fact_at)?, word(fact_at + 4)?, word(fact_at + 8)?))
        }

        /// Every type row in lane order: anonymous computed rows first, then
        /// one row per pushed fact.
        fn type_rows<'a>(
            view: &'a FragmentView<'a>,
        ) -> Result<Vec<compiler_ir::DecodedTypeFact<'a>>, TestError> {
            let cursor = view
                .type_facts()
                .ok_or(TestError::MissingTypeFact { owner: 0 })?;
            Ok(cursor.filter_map(|result| result.ok()).collect())
        }

        fn computed_row_of<'a>(
            rows: &'a [compiler_ir::DecodedTypeFact<'a>],
            cell: (u32, u32, u32),
        ) -> Result<compiler_ir::DecodedTypeFact<'a>, TestError> {
            rows.get(cell.2 as usize)
                .copied()
                .ok_or(TestError::MissingTypeFact { owner: cell.2 })
        }

        #[test]
        fn golden_computed_cells_fill_extension_facts_with_checker_rows() -> Result<(), TestError> {
            let report = golden()?;
            let without = fragment_with_checker(GOLDEN_SOURCE, None)?;
            let with_bytes = fragment_with_checker(GOLDEN_SOURCE, Some(&report))?;
            let plain = FragmentView::validate(&without).map_err(TestError::Validate)?;
            let checked = FragmentView::validate(&with_bytes).map_err(TestError::Validate)?;
            // Without the checker the computed cell is absent for every fact.
            let (n, _, _) = fact_named(&plain, b"n")?;
            let (_, _, computed) = typescript_cell(&plain, n)?;
            assert_eq!(computed, NONE);
            // With the checker the cell names the computed row.
            let (n_checked, _, n_computed) = typescript_cell(&checked, n)?;
            assert_ne!(n_computed, NONE);
            let rows = type_rows(&checked)?;
            let row = computed_row_of(&rows, (0, n_checked, n_computed))?;
            expect_tag(&row, SemanticTypeTag::Primitive)?;
            assert_eq!(row.record.payload0, u32::from(PrimitiveShape::Float));
            Ok(())
        }

        #[test]
        fn golden_inferred_const_and_union_records_match_the_checker() -> Result<(), TestError> {
            let checked = golden_view()?;
            let rows = type_rows(&checked)?;
            // `inferred = 7` computes a 32-bit signed integer literal record.
            let (inferred, _, inferred_cell) =
                typescript_cell(&checked, fact_named(&checked, b"inferred")?.0)?;
            let inferred_row = computed_row_of(&rows, (0, inferred, inferred_cell))?;
            expect_tag(&inferred_row, SemanticTypeTag::Primitive)?;
            assert_eq!(
                inferred_row.record.payload0,
                u32::from(PrimitiveShape::Integer)
            );
            assert_eq!(
                inferred_row.record.payload1,
                (32_u32 << 1) | SemanticTypeRecord::INTEGER_SIGNED_FLAG
            );
            // `union: string | number` computes a two-member union record.
            let (union_entity, _, union_cell) =
                typescript_cell(&checked, fact_named(&checked, b"union")?.0)?;
            let union_row = computed_row_of(&rows, (0, union_entity, union_cell))?;
            expect_tag(&union_row, SemanticTypeTag::Union)?;
            assert_eq!(union_row.record.children.length, 2);
            Ok(())
        }

        #[test]
        fn golden_foreign_generic_base_names_the_resolved_spelling() -> Result<(), TestError> {
            let checked = golden_view()?;
            // The DECLARED annotation `Map<string, number>` keeps an honest
            // unknown: the checker resolved `Map` to the foreign `typescript`
            // module, and the closed lattice has no foreign-nominal row, so
            // the record names the resolved spelling for the backlog instead
            // of claiming it is genuinely unresolvable.
            let (table, _, _) = fact_named(&checked, b"table")?;
            let declared = type_fact(&checked, table)?;
            expect_tag(&declared, SemanticTypeTag::Unknown)?;
            assert_eq!(
                declared.record.payload0,
                u32::from(TypeReason::NoIrRepresentation)
            );
            assert_eq!(declared.record.text, Some(&b"Map"[..]));
            // The COMPUTED row carries the full applied structure: base plus
            // exactly two arguments.
            let (_, _, table_cell) = typescript_cell(&checked, table)?;
            let rows = type_rows(&checked)?;
            let applied = computed_row_of(&rows, (0, table, table_cell))?;
            expect_tag(&applied, SemanticTypeTag::Apply)?;
            assert_eq!(applied.record.children.length, 3);
            Ok(())
        }

        #[test]
        fn golden_this_type_and_local_nominal_computed_rows_bind_by_name() -> Result<(), TestError>
        {
            let checked = golden_view()?;
            let rows = type_rows(&checked)?;
            // `made` computes to the `Box` nominal: the computed row names
            // the same-file class fact.
            let (box_entity, _) = fact_named(&checked, b"Box")?;
            let (made, _, made_cell) = typescript_cell(&checked, fact_named(&checked, b"made")?.0)?;
            let made_row = computed_row_of(&rows, (0, made, made_cell))?;
            expect_tag(&made_row, SemanticTypeTag::Nominal)?;
            assert_eq!(
                made_row.record.nominal,
                Some(compiler_ir::NominalRef::Local(compiler_ir::EntityId::new(
                    box_entity
                )))
            );
            // The `self(): this` method computes a function row whose result
            // child is the SelfType leaf.
            let (_, _, self_cell) = typescript_cell(&checked, fact_named(&checked, b"self")?.0)?;
            let self_row = computed_row_of(&rows, (0, 0, self_cell))?;
            expect_tag(&self_row, SemanticTypeTag::FunctionPointer)?;
            assert_ne!(
                self_row.record.payload1 & SemanticTypeRecord::RESULT_FLAG,
                0
            );
            Ok(())
        }

        #[test]
        fn golden_oracle_confidence_picks_distinct_overload_targets() -> Result<(), TestError> {
            let checked = golden_view()?;
            let oracle_calls: Vec<DecodedOccurrence<'_>> = occurrences(&checked)?
                .into_iter()
                .filter(|occurrence| {
                    occurrence.occurrence.confidence == OccurrenceConfidence::Oracle
                        && occurrence.occurrence.kind == ReferenceKind::FunctionCall
                })
                .collect();
            // The two `g` overload call sites resolve at oracle confidence to
            // the two distinct `g` overload facts.
            let g_targets: Vec<u32> = oracle_calls
                .iter()
                .filter_map(|occurrence| match occurrence.occurrence.target {
                    OccurrenceTarget::Local(entity) => Some(entity.raw),
                    OccurrenceTarget::Foreign(_) => None,
                })
                .filter(|target| {
                    entities(&checked)
                        .into_iter()
                        .any(|(ordinal, name, _)| ordinal == *target && name == b"g")
                })
                .collect();
            assert_eq!(g_targets.len(), 2, "observed {g_targets:?}");
            assert_ne!(g_targets[0], g_targets[1]);
            // The checker-only property-call site `slot.get()` also commits
            // at oracle confidence and targets the exact `get` member fact.
            let get_target = oracle_calls
                .iter()
                .filter_map(|occurrence| match occurrence.occurrence.target {
                    OccurrenceTarget::Local(entity) => Some(entity.raw),
                    OccurrenceTarget::Foreign(_) => None,
                })
                .find(|target| {
                    entities(&checked)
                        .into_iter()
                        .any(|(ordinal, name, _)| ordinal == *target && name == b"get")
                })
                .ok_or(TestError::MissingEntity {
                    name: "get call target".to_owned(),
                })?;
            let (_, get_kind) = fact_named(&checked, b"get")?;
            assert_eq!(get_kind, EntityKind::Function);
            let _ = get_target;
            // No span commits twice: every committed occurrence names a
            // distinct (owner, relative-span) pair.
            let mut covered: Vec<(u32, u32, u32)> = Vec::new();
            for fact in occurrences(&checked)? {
                let key = (
                    fact.owner.raw,
                    u32::from(fact.occurrence.span.start),
                    u32::from(fact.occurrence.span.end),
                );
                if covered.contains(&key) {
                    return Err(TestError::Tail);
                }
                covered.push(key);
            }
            Ok(())
        }

        #[test]
        fn golden_narrowing_extends_the_declared_fact_with_a_site_row() -> Result<(), TestError> {
            let checked = golden_view()?;
            let (widened, _) = fact_named(&checked, b"widened")?;
            // The declaration-time computed cell keeps the annotated union.
            let (_, _, computed) = typescript_cell(&checked, widened)?;
            assert_ne!(computed, NONE);
            let rows = type_rows(&checked)?;
            let computed_row = computed_row_of(&rows, (0, widened, computed))?;
            expect_tag(&computed_row, SemanticTypeTag::Union)?;
            assert_eq!(computed_row.record.children.length, 2);
            // The assignment narrowing commits one extra owned row: the
            // string-literal type of `widened = "text"` at that exact site.
            let site_rows: Vec<_> = rows
                .iter()
                .filter(|fact| fact.owner.raw == widened)
                .collect();
            assert!(
                site_rows
                    .iter()
                    .any(|fact| fact.record.tag == SemanticTypeTag::Primitive
                        && fact.record.payload0 == u32::from(PrimitiveShape::Str)),
                "expected a narrowed string row, observed {:?}",
                site_rows
                    .iter()
                    .map(|fact| fact.record.tag)
                    .collect::<Vec<_>>()
            );
            Ok(())
        }

        #[test]
        fn narrowing_object_members_bind_spellings_at_the_assignment_site() -> Result<(), TestError>
        {
            // `alpha` is spelled only at the assignment site, never in the
            // owner declaration; the row commits, which proves the spelling
            // domain is the site span.
            let source: &[u8] = b"let wide: number = 0;\nwide = { alpha: 1 };";
            let mut report = golden()?;
            report.source_digest = hex_digest_of(source);
            report.declarations = Box::new([]);
            report.references = Box::new([]);
            report.narrowings = Box::new([compiler_languages_typescript::Narrowing {
                name_start: 4,
                name_end: 8,
                start: 22,
                end: 42,
                r#type: Some(compiler_languages_typescript::TypeTree::Object {
                    members: Box::new([compiler_languages_typescript::ObjectMember {
                        name: "alpha".to_owned(),
                        optional: false,
                        readonly: false,
                        member_type: compiler_languages_typescript::TypeTree::Primitive {
                            name: "number".to_owned(),
                        },
                    }]),
                }),
            }]);
            let bytes = fragment_with_checker(source, Some(&report))?;
            let checked = FragmentView::validate(&bytes).map_err(TestError::Validate)?;
            let (wide, _) = fact_named(&checked, b"wide")?;
            let rows = type_rows(&checked)?;
            let record_row = rows
                .iter()
                .find(|fact| {
                    fact.owner.raw == wide && fact.record.tag == SemanticTypeTag::AnonymousRecord
                })
                .ok_or(TestError::MissingTypeFact { owner: wide })?;
            assert_eq!(record_row.record.children.length, 1);
            // Renaming the member to a name the site never spells is the
            // typed lane rejection: computed child names are proven source
            // slices, never borrowed report bytes.
            let mut renamed = report.clone();
            renamed.narrowings = Box::new([compiler_languages_typescript::Narrowing {
                name_start: 4,
                name_end: 8,
                start: 22,
                end: 42,
                r#type: Some(compiler_languages_typescript::TypeTree::Object {
                    members: Box::new([compiler_languages_typescript::ObjectMember {
                        name: "beta".to_owned(),
                        optional: false,
                        readonly: false,
                        member_type: compiler_languages_typescript::TypeTree::Primitive {
                            name: "number".to_owned(),
                        },
                    }]),
                }),
            }]);
            let failure = fragment_with_checker(source, Some(&renamed))
                .expect_err("an unspelled member name must reject");
            assert!(matches!(failure, TestError::Collect(..)));
            Ok(())
        }

        #[test]
        fn checker_resolved_global_reaches_an_oracle_universe_key() -> Result<(), TestError> {
            let source: &[u8] = b"export const term = console;";
            let mut report = golden()?;
            report.source_digest = hex_digest_of(source);
            report.declarations = Box::new([]);
            report.references = Box::new([compiler_languages_typescript::Reference {
                start: 20,
                end: 27,
                target_start: None,
                target_end: None,
                module: Some("typescript".to_owned()),
                name: Some("console".to_owned()),
                overload_index: None,
            }]);
            report.narrowings = Box::new([]);
            let bytes = fragment_with_checker(source, Some(&report))?;
            let checked = FragmentView::validate(&bytes).map_err(TestError::Validate)?;
            let (term, _) = fact_named(&checked, b"term")?;
            let upgraded = occurrences(&checked)?
                .into_iter()
                .find(|fact| {
                    fact.owner.raw == term
                        && fact.occurrence.confidence == OccurrenceConfidence::Oracle
                })
                .ok_or(TestError::MissingOccurrence)?;
            match upgraded.occurrence.target {
                OccurrenceTarget::Foreign(key) => {
                    assert_eq!(key.path, "console");
                    assert!(matches!(
                        key.origin,
                        compiler_ir::ForeignOrigin::Universe { ecosystem: "npm" }
                    ));
                }
                OccurrenceTarget::Local(_) => return Err(TestError::MissingOccurrence),
                OccurrenceTarget::Stable(_) => return Err(TestError::MissingOccurrence),
            }
            Ok(())
        }

        #[test]
        fn package_module_bases_stay_honestly_syntactic() -> Result<(), TestError> {
            // A checker-resolved npm package module whose bytes are not
            // spelled in this source cannot become a borrowed foreign key;
            // the occurrence keeps the honest syntactic degradation.
            let source: &[u8] = b"export const q = missing;";
            let mut report = golden()?;
            report.source_digest = hex_digest_of(source);
            report.declarations = Box::new([]);
            report.references = Box::new([compiler_languages_typescript::Reference {
                start: 17,
                end: 24,
                target_start: None,
                target_end: None,
                module: Some("left-pad".to_owned()),
                name: Some("missing".to_owned()),
                overload_index: None,
            }]);
            report.narrowings = Box::new([]);
            let bytes = fragment_with_checker(source, Some(&report))?;
            let checked = FragmentView::validate(&bytes).map_err(TestError::Validate)?;
            let (q, _) = fact_named(&checked, b"q")?;
            let syntactic = occurrences(&checked)?
                .into_iter()
                .find(|fact| {
                    fact.owner.raw == q
                        && fact.occurrence.confidence == OccurrenceConfidence::Syntactic
                })
                .ok_or(TestError::MissingOccurrence)?;
            assert!(matches!(
                syntactic.occurrence.target,
                OccurrenceTarget::Foreign(_)
            ));
            assert!(
                !occurrences(&checked)?
                    .into_iter()
                    .any(|fact| fact.owner.raw == q
                        && fact.occurrence.confidence == OccurrenceConfidence::Oracle)
            );
            Ok(())
        }

        #[test]
        fn checker_only_property_call_targets_the_exact_member() -> Result<(), TestError> {
            let source: &[u8] = b"export class Box { tick(): number { return 1; } }\nexport const box = new Box();\nexport const t = box.tick();";
            let mut report = golden()?;
            report.source_digest = hex_digest_of(source);
            report.declarations = Box::new([]);
            // `tick` at 101..105 resolves to the method declared at 19..23;
            // OXC binds no reference for a property-access name.
            report.references = Box::new([compiler_languages_typescript::Reference {
                start: 101,
                end: 105,
                target_start: Some(19),
                target_end: Some(23),
                module: None,
                name: None,
                overload_index: Some(0),
            }]);
            report.narrowings = Box::new([]);
            let bytes = fragment_with_checker(source, Some(&report))?;
            let checked = FragmentView::validate(&bytes).map_err(TestError::Validate)?;
            let (tick, tick_kind) = fact_named(&checked, b"tick")?;
            assert_eq!(tick_kind, EntityKind::Function);
            // The checker-only call is the one oracle call that targets the
            // `tick` method fact; OXC's own call-site commits (`new Box()`)
            // stay untouched.
            let calls: Vec<_> = occurrences(&checked)?
                .into_iter()
                .filter(|fact| {
                    fact.occurrence.kind == ReferenceKind::FunctionCall
                        && fact.occurrence.target
                            == OccurrenceTarget::Local(compiler_ir::EntityId::new(tick))
                })
                .collect();
            assert_eq!(calls.len(), 1, "expected exactly the checker-only call");
            assert_eq!(calls[0].occurrence.confidence, OccurrenceConfidence::Oracle);
            Ok(())
        }

        #[test]
        fn genuinely_unresolvable_names_stay_honestly_external() -> Result<(), TestError> {
            // With no checker report the unresolved name keeps the closed
            // `UnresolvedExternal` reason.
            let source: &[u8] = b"export const q: TotallyMissing = 1;\n";
            let bytes = fragment(source)?;
            let plain = FragmentView::validate(&bytes).map_err(TestError::Validate)?;
            let (q, _, _) = fact_named(&plain, b"q")?;
            let declared = type_fact(&plain, q)?;
            assert_eq!(
                declared.record.payload0,
                u32::from(TypeReason::UnresolvedExternal)
            );
            // With a checker reference that names a foreign module origin,
            // the same spelling is known-but-unrepresentable instead.
            let mut report = golden()?;
            let use_start = 16_u32;
            let use_end =
                use_start + u32::try_from("TotallyMissing".len()).map_err(|_| TestError::Tail)?;
            report.source_digest = hex_digest_of(source);
            report.references = Box::new([compiler_languages_typescript::Reference {
                start: use_start,
                end: use_end,
                target_start: None,
                target_end: None,
                module: Some("somewhere".to_owned()),
                name: Some("TotallyMissing".to_owned()),
                overload_index: None,
            }]);
            let bytes = fragment_with_checker(source, Some(&report))?;
            let checked = FragmentView::validate(&bytes).map_err(TestError::Validate)?;
            let (q_checked, _, _) = fact_named(&checked, b"q")?;
            let resolved = type_fact(&checked, q_checked)?;
            assert_eq!(
                resolved.record.payload0,
                u32::from(TypeReason::NoIrRepresentation)
            );
            assert_eq!(resolved.record.text, Some(&b"TotallyMissing"[..]));
            Ok(())
        }

        #[test]
        fn computed_row_pool_bound_and_union_child_bound_are_typed_rejections()
        -> Result<(), TestError> {
            // Nine union members overflow the bounded type-child lane.
            let report = golden()?;
            let members: Vec<compiler_languages_typescript::TypeTree> = (0..9)
                .map(|_| compiler_languages_typescript::TypeTree::This)
                .collect();
            let mut wide = report.clone();
            wide.source_digest = hex_digest_of(GOLDEN_SOURCE);
            wide.declarations = Box::new([compiler_languages_typescript::Declaration {
                name_start: 13,
                name_end: 14,
                origin: compiler_languages_typescript::Origin::Computed,
                overload_index: None,
                r#type: Some(compiler_languages_typescript::TypeTree::Union { members }),
            }]);
            let failure = fragment_with_checker(GOLDEN_SOURCE, Some(&wide))
                .expect_err("nine union members must reject");
            assert!(matches!(failure, TestError::Collect(..)));
            Ok(())
        }

        fn hex_digest_of(bytes: &[u8]) -> String {
            compiler_languages_typescript::source_digest(bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect()
        }
    }

    /// One committed entity row: its ordinal, borrowed name bytes, and kind.
    fn entities(view: &FragmentView<'_>) -> Vec<(u32, Vec<u8>, EntityKind)> {
        let atoms: Vec<(u32, Vec<u8>)> = view
            .atoms()
            .map(|atom| (atom.ordinal.raw, atom.bytes.to_vec()))
            .collect();
        view.entities()
            .map(|entity| {
                let name = atoms
                    .iter()
                    .find(|(ordinal, _)| *ordinal == entity.name.raw)
                    .map(|(_, bytes)| bytes.clone())
                    .unwrap_or_default();
                (entity.entity.raw, name, entity.kind)
            })
            .collect()
    }

    fn fact_named<'view>(
        view: &FragmentView<'view>,
        name: &[u8],
    ) -> Result<(u32, EntityKind), TestError> {
        entities(view)
            .into_iter()
            .find(|(_, entity_name, _)| entity_name == name)
            .map(|(ordinal, _, kind)| (ordinal, kind))
            .ok_or(TestError::MissingEntity {
                name: String::from_utf8_lossy(name).into_owned(),
            })
    }

    fn type_fact<'fragment>(
        view: &FragmentView<'fragment>,
        owner: u32,
    ) -> Result<DecodedTypeFact<'fragment>, TestError> {
        let Some(cursor) = view.type_facts() else {
            return Err(TestError::MissingTypeFact { owner });
        };
        cursor
            .filter_map(|result| result.ok())
            .find(|fact| fact.owner.raw == owner)
            .ok_or(TestError::MissingTypeFact { owner })
    }

    fn occurrences<'fragment>(
        view: &FragmentView<'fragment>,
    ) -> Result<Vec<DecodedOccurrence<'fragment>>, TestError> {
        let Some(cursor) = view.occurrences() else {
            return Err(TestError::MissingOccurrence);
        };
        Ok(cursor.filter_map(|result| result.ok()).collect())
    }

    fn docs<'fragment>(
        view: &FragmentView<'fragment>,
    ) -> Result<Vec<DecodedDocFact<'fragment>>, TestError> {
        let Some(cursor) = view.docs() else {
            return Err(TestError::MissingDoc);
        };
        Ok(cursor.filter_map(|result| result.ok()).collect())
    }

    fn expect_tag(fact: &DecodedTypeFact<'_>, tag: SemanticTypeTag) -> Result<(), TestError> {
        if fact.record.tag != tag {
            return Err(TestError::Record {
                expected: tag,
                observed: fact.record.tag,
            });
        }
        Ok(())
    }

    #[test]
    fn declared_scalar_annotations_map_onto_lattice_records() -> Result<(), TestError> {
        let bytes = fragment(
            b"export const n: number = 1;
export const s: string = \"x\";
export const b: boolean = false;
export const big: bigint = 1n;
export const any_value: any = 1;
export const unknown_value: unknown = undefined;
export const nothing: void = undefined;
export const nullish: null = null;",
        )?;
        let view = view(&bytes)?;
        let scalar = |name: &'static str| -> Result<DecodedTypeFact<'_>, TestError> {
            let (ordinal, kind) = fact_named(&view, name.as_bytes())?;
            assert_eq!(kind, EntityKind::Constant);
            type_fact(&view, ordinal)
        };
        let number = scalar("n")?;
        expect_tag(&number, SemanticTypeTag::Primitive)?;
        assert_eq!(number.record.payload0, u32::from(PrimitiveShape::Float));
        assert_eq!(number.record.payload1, TypeWidth::Fixed(64).to_cell());
        let string = scalar("s")?;
        assert_eq!(string.record.payload0, u32::from(PrimitiveShape::Str));
        let boolean = scalar("b")?;
        assert_eq!(boolean.record.payload0, u32::from(PrimitiveShape::Bool));
        let bigint = scalar("big")?;
        assert_eq!(bigint.record.payload0, u32::from(PrimitiveShape::Builtin));
        assert_eq!(bigint.record.text, Some(&b"bigint"[..]));
        let dynamic = scalar("any_value")?;
        expect_tag(&dynamic, SemanticTypeTag::Unknown)?;
        assert_eq!(
            dynamic.record.payload0,
            u32::from(TypeReason::DynamicallyTyped)
        );
        let top = scalar("unknown_value")?;
        expect_tag(&top, SemanticTypeTag::Any)?;
        let void = scalar("nothing")?;
        assert_eq!(void.record.text, Some(&b"void"[..]));
        let null = scalar("nullish")?;
        assert_eq!(null.record.text, Some(&b"null"[..]));
        Ok(())
    }

    #[test]
    fn mutually_recursive_interfaces_keep_diagonal_self_nominals_and_linked_members()
    -> Result<(), TestError> {
        let bytes = fragment(
            b"export interface A { b: B; }
export interface B { a: A; }",
        )?;
        let view = view(&bytes)?;
        let (a_ordinal, a_kind) = fact_named(&view, b"A")?;
        assert_eq!(a_kind, EntityKind::Trait);
        let (b_ordinal, b_kind) = fact_named(&view, b"B")?;
        assert_eq!(b_kind, EntityKind::Trait);
        // Diagonal self-references: the terminal recursive case.
        let a_record = type_fact(&view, a_ordinal)?;
        expect_tag(&a_record, SemanticTypeTag::Nominal)?;
        assert_eq!(
            a_record.record.nominal,
            Some(compiler_ir::NominalRef::Local(compiler_ir::EntityId::new(
                a_ordinal
            )))
        );
        let b_record = type_fact(&view, b_ordinal)?;
        assert_eq!(
            b_record.record.nominal,
            Some(compiler_ir::NominalRef::Local(compiler_ir::EntityId::new(
                b_ordinal
            )))
        );
        // Member facts are pushed after both declarations and link at them.
        let (b_member, _) = fact_named(&view, b"b")?;
        let member_record = type_fact(&view, b_member)?;
        assert_eq!(member_record.record.children.length, 0);
        assert_eq!(
            member_record.record.nominal,
            Some(compiler_ir::NominalRef::Local(compiler_ir::EntityId::new(
                b_ordinal
            )))
        );
        let (a_member, _) = fact_named(&view, b"a")?;
        let a_member_record = type_fact(&view, a_member)?;
        assert_eq!(
            a_member_record.record.nominal,
            Some(compiler_ir::NominalRef::Local(compiler_ir::EntityId::new(
                a_ordinal
            )))
        );
        // Member ordinals strictly follow their declarations.
        assert!(a_member > b_ordinal && b_member > b_ordinal);
        Ok(())
    }

    #[test]
    fn structural_types_commit_union_intersection_tuple_array_and_apply_records()
    -> Result<(), TestError> {
        let bytes = fragment(
            b"export type U = string | number;
export type I = string & number;
export type Tup = [a: string, number?];
export const arr: number[] = [];
export interface Holder<T extends string = string> { value: T; }
export type Applied = Holder<string>;",
        )?;
        let view = view(&bytes)?;
        let (union_ordinal, _) = fact_named(&view, b"U")?;
        let union = type_fact(&view, union_ordinal)?;
        expect_tag(&union, SemanticTypeTag::Union)?;
        assert_eq!(union.record.children.length, 2);
        let (intersection_ordinal, _) = fact_named(&view, b"I")?;
        let intersection = type_fact(&view, intersection_ordinal)?;
        expect_tag(&intersection, SemanticTypeTag::Intersection)?;
        assert_eq!(intersection.record.children.length, 2);
        let (tuple_ordinal, _) = fact_named(&view, b"Tup")?;
        let tuple = type_fact(&view, tuple_ordinal)?;
        expect_tag(&tuple, SemanticTypeTag::Tuple)?;
        assert_eq!(tuple.record.children.length, 2);
        let (array_ordinal, _) = fact_named(&view, b"arr")?;
        let array = type_fact(&view, array_ordinal)?;
        expect_tag(&array, SemanticTypeTag::Array)?;
        assert_eq!(array.record.text, Some(&b"[]"[..]));
        assert_eq!(array.record.children.length, 1);
        // The generic application carries its base and one argument child.
        let (applied_ordinal, _) = fact_named(&view, b"Applied")?;
        let applied = type_fact(&view, applied_ordinal)?;
        expect_tag(&applied, SemanticTypeTag::Apply)?;
        assert_eq!(applied.record.children.length, 2);
        // The generic parameter is a TypeVar fact naming itself.
        let (parameter_ordinal, parameter_kind) = fact_named(&view, b"T")?;
        assert_eq!(parameter_kind, EntityKind::Parameter);
        let parameter = type_fact(&view, parameter_ordinal)?;
        expect_tag(&parameter, SemanticTypeTag::TypeVar)?;
        assert_eq!(parameter.record.text, Some(&b"T"[..]));
        // A labeled tuple element changes the committed bytes.
        let relabeled = fragment(b"export type Tup = [z: string, number?];")?;
        if relabeled == bytes {
            return Err(TestError::Tail);
        }
        Ok(())
    }

    #[test]
    fn signatures_commit_parameter_and_result_facts_and_distinct_overloads() -> Result<(), TestError>
    {
        let bytes = fragment(
            b"export function f(x: number, y: string): boolean { return true; }
declare function g(x: string): void;
declare function g(x: number): void;",
        )?;
        let view = view(&bytes)?;
        let (function_ordinal, function_kind) = fact_named(&view, b"f")?;
        assert_eq!(function_kind, EntityKind::Function);
        let signature = type_fact(&view, function_ordinal)?;
        expect_tag(&signature, SemanticTypeTag::FunctionPointer)?;
        assert_ne!(
            signature.record.payload1 & SemanticTypeRecord::RESULT_FLAG,
            0
        );
        assert_eq!(signature.record.children.length, 3);
        let (x_ordinal, x_kind) = fact_named(&view, b"x")?;
        assert_eq!(x_kind, EntityKind::Parameter);
        let x_record = type_fact(&view, x_ordinal)?;
        assert_eq!(x_record.record.payload0, u32::from(PrimitiveShape::Float));
        let (y_ordinal, y_kind) = fact_named(&view, b"y")?;
        assert_eq!(y_kind, EntityKind::Parameter);
        let y_record = type_fact(&view, y_ordinal)?;
        assert_eq!(y_record.record.payload0, u32::from(PrimitiveShape::Str));
        // Both overload signatures survive as distinct facts with their own
        // parameter rows, so their constructor and child cells differ.
        let g_rows = entities(&view)
            .into_iter()
            .filter(|(_, name, kind)| name == b"g" && *kind == EntityKind::Function)
            .count();
        assert_eq!(g_rows, 2);
        // Dropping one overload changes the committed bytes.
        let single = fragment(b"declare function g(x: string): void;")?;
        if single == bytes {
            return Err(TestError::Tail);
        }
        Ok(())
    }

    #[test]
    fn references_resolve_local_import_and_unresolved_targets() -> Result<(), TestError> {
        let bytes = fragment(
            b"import { foreign } from 'pkg';
export interface Shape { area(): number; }
export const s: Shape = foreign();
export const broken = missingGlobal;",
        )?;
        let view = view(&bytes)?;
        let (import_ordinal, _) = fact_named(&view, b"foreign")?;
        let (shape_ordinal, _) = fact_named(&view, b"Shape")?;
        let (s_ordinal, _) = fact_named(&view, b"s")?;
        let references = occurrences(&view)?;
        // The import site references its npm origin at import confidence.
        assert!(references.iter().any(|fact| {
            fact.owner.raw == import_ordinal
                && fact.occurrence.kind == compiler_ir::ReferenceKind::Import
                && fact.occurrence.confidence == compiler_ir::OccurrenceConfidence::Import
        }));
        // The annotation reference resolves locally at index confidence.
        let local = references.iter().find(|fact| {
            fact.owner.raw == s_ordinal
                && fact.occurrence.kind == compiler_ir::ReferenceKind::TypeReference
        });
        let Some(local) = local else {
            return Err(TestError::MissingOccurrence);
        };
        assert_eq!(
            local.occurrence.target,
            compiler_ir::OccurrenceTarget::Local(compiler_ir::EntityId::new(shape_ordinal))
        );
        assert_eq!(
            local.occurrence.confidence,
            compiler_ir::OccurrenceConfidence::Index
        );
        // The call of the import binding resolves through the import table.
        assert!(references.iter().any(|fact| {
            fact.owner.raw == s_ordinal
                && fact.occurrence.kind == compiler_ir::ReferenceKind::FunctionCall
                && fact.occurrence.confidence == compiler_ir::OccurrenceConfidence::Import
        }));
        // The unresolved global stays syntactic against the npm universe.
        let (broken_ordinal, _) = fact_named(&view, b"broken")?;
        assert!(references.iter().any(|fact| {
            fact.owner.raw == broken_ordinal
                && fact.occurrence.confidence == compiler_ir::OccurrenceConfidence::Syntactic
                && matches!(
                    fact.occurrence.target,
                    compiler_ir::OccurrenceTarget::Foreign(_)
                )
        }));
        // Spans are owner-relative and non-negative by construction.
        for fact in &references {
            assert!(fact.occurrence.span.start <= fact.occurrence.span.end);
        }
        Ok(())
    }

    #[test]
    fn jsdoc_commits_text_code_and_local_link_fragments() -> Result<(), TestError> {
        let bytes = fragment(
            b"/** Checks {@code x} and {@link Shape}. */
export interface Shape {}",
        )?;
        let view = view(&bytes)?;
        let (shape_ordinal, _) = fact_named(&view, b"Shape")?;
        let fragments = docs(&view)?;
        assert!(fragments.len() >= 5);
        assert_eq!(fragments[0].owner.raw, shape_ordinal);
        assert!(matches!(
            fragments[0].fragment,
            compiler_ir::DocFragmentInput::Text(b"Checks ")
        ));
        assert!(matches!(
            fragments[1].fragment,
            compiler_ir::DocFragmentInput::Code(b"x")
        ));
        let Some(link_position) = fragments
            .iter()
            .position(|fact| matches!(fact.fragment, compiler_ir::DocFragmentInput::Link { .. }))
        else {
            return Err(TestError::MissingDoc);
        };
        match fragments[link_position].fragment {
            compiler_ir::DocFragmentInput::Link { label, target } => {
                assert_eq!(label, b"Shape");
                assert_eq!(
                    target,
                    compiler_ir::DocLinkTarget::Local(compiler_ir::EntityId::new(shape_ordinal))
                );
            }
            _ => return Err(TestError::MissingDoc),
        }
        Ok(())
    }

    #[test]
    fn absent_jsdoc_commits_no_documentation_section() -> Result<(), TestError> {
        let bytes = fragment(b"export interface Plain {}")?;
        let view = view(&bytes)?;
        assert!(view.docs().is_none());
        Ok(())
    }

    #[test]
    fn template_literal_mapped_and_conditional_records_commit_their_tags() -> Result<(), TestError>
    {
        let bytes = fragment(
            b"export type Lit = `pre${string}post`;
export type Mapped = { readonly [K in keyof string]: number };
export type Removed = { [K in string]-?: number };
export type Branch = string extends string ? number : boolean;",
        )?;
        let view = view(&bytes)?;
        let (literal_ordinal, _) = fact_named(&view, b"Lit")?;
        let literal = type_fact(&view, literal_ordinal)?;
        expect_tag(&literal, SemanticTypeTag::TemplateLiteral)?;
        // The frozen lane cannot express literal text parts as children.
        assert_eq!(literal.record.children.length, 0);
        let (mapped_ordinal, _) = fact_named(&view, b"Mapped")?;
        let mapped = type_fact(&view, mapped_ordinal)?;
        expect_tag(&mapped, SemanticTypeTag::Mapped)?;
        assert_eq!(
            mapped.record.payload0,
            u32::from(compiler_ir::LatticeMappedModifier::Add)
        );
        assert_eq!(
            mapped.record.payload1,
            u32::from(compiler_ir::LatticeMappedModifier::Absent)
        );
        assert_eq!(mapped.record.text, Some(&b"K"[..]));
        assert_eq!(mapped.record.children.length, 2);
        let (removed_ordinal, _) = fact_named(&view, b"Removed")?;
        let removed = type_fact(&view, removed_ordinal)?;
        assert_eq!(
            removed.record.payload1,
            u32::from(compiler_ir::LatticeMappedModifier::Remove)
        );
        let (branch_ordinal, _) = fact_named(&view, b"Branch")?;
        let branch = type_fact(&view, branch_ordinal)?;
        expect_tag(&branch, SemanticTypeTag::Conditional)?;
        assert_eq!(branch.record.children.length, 4);
        Ok(())
    }

    #[test]
    fn anonymous_object_literals_commit_named_member_children() -> Result<(), TestError> {
        let bytes =
            fragment(b"export const p: { readonly a: string; b?: number } = { a: \"x\", b: 1 };")?;
        let view = view(&bytes)?;
        let (record_ordinal, _) = fact_named(&view, b"p")?;
        let record = type_fact(&view, record_ordinal)?;
        expect_tag(&record, SemanticTypeTag::AnonymousRecord)?;
        assert_eq!(
            record.record.payload0,
            u32::from(compiler_ir::AnonRecordForm::Interface)
        );
        assert_eq!(record.record.children.length, 2);
        let (a_ordinal, a_kind) = fact_named(&view, b"a")?;
        assert_eq!(a_kind, EntityKind::Field);
        let a_record = type_fact(&view, a_ordinal)?;
        assert_eq!(a_record.record.payload0, u32::from(PrimitiveShape::Str));
        let (b_ordinal, b_kind) = fact_named(&view, b"b")?;
        assert_eq!(b_kind, EntityKind::Field);
        let b_record = type_fact(&view, b_ordinal)?;
        assert_eq!(b_record.record.payload0, u32::from(PrimitiveShape::Float));
        // A member flag change alters the committed child flags.
        let unflagged =
            fragment(b"export const p: { a: string; b?: number } = { a: \"x\", b: 1 };")?;
        if unflagged == bytes {
            return Err(TestError::Tail);
        }
        Ok(())
    }

    #[test]
    fn single_declaration_owns_its_annotation_without_synthetic_rows() -> Result<(), TestError> {
        let bytes = fragment(b"export const only: number = 1;")?;
        let view = view(&bytes)?;
        let rows = entities(&view);
        assert_eq!(rows.len(), 1, "expected exactly one entity row");
        let (ordinal, kind) = fact_named(&view, b"only")?;
        assert_eq!(kind, EntityKind::Constant);
        let record = type_fact(&view, ordinal)?;
        assert_eq!(record.record.payload0, u32::from(PrimitiveShape::Float));
        assert_eq!(record.record.payload1, TypeWidth::Fixed(64).to_cell());
        Ok(())
    }

    #[test]
    fn foreign_generic_reference_is_unknown_with_its_qualified_spelling() -> Result<(), TestError> {
        let bytes = fragment(b"export const m: Map<string, number> = new Map();")?;
        let view = view(&bytes)?;
        let (ordinal, _) = fact_named(&view, b"m")?;
        let record = type_fact(&view, ordinal)?;
        expect_tag(&record, SemanticTypeTag::Unknown)?;
        assert_eq!(
            record.record.payload0,
            u32::from(TypeReason::UnresolvedExternal)
        );
        assert_eq!(record.record.text, Some(&b"Map"[..]));
        Ok(())
    }

    #[test]
    fn empty_source_admits_the_schema1_fragment_without_semantic_data() -> Result<(), TestError> {
        let bytes = fragment(b"")?;
        let view = view(&bytes)?;
        assert_eq!(view.entities().count(), 0);
        assert!(view.type_facts().is_none());
        Ok(())
    }

    #[test]
    fn declarations_beyond_the_lane_bound_are_the_typed_rejection() {
        let mut source = Vec::new();
        for index in 0..130 {
            source.extend_from_slice(b"export const x");
            let digits = index.to_string();
            source.extend_from_slice(digits.as_bytes());
            source.extend_from_slice(b" = 1;\n");
        }
        let mut facts = crate::lower::FactSet::new();
        match collect(TypeScriptSource::TypeScript, &source, &mut facts) {
            Err(TypeScriptCollectError::Lowering(_)) => {}
            Err(_) | Ok(()) => panic!("expected the typed lane rejection"),
        }
    }

    #[test]
    fn oxc_bound_symbols_fill_the_canonical_declaration_lane() -> Result<(), TestError> {
        let bytes = fragment(
            b"import { foreign } from 'pkg'; export class Box {} export const value = foreign;",
        )?;
        let view = view(&bytes)?;
        let rows = entities(&view);
        assert_eq!(rows.len(), 3, "expected import, class, and constant rows");
        let (_, box_kind) = fact_named(&view, b"Box")?;
        assert_eq!(box_kind, EntityKind::Record);
        Ok(())
    }

    #[test]
    fn oxc_interface_is_never_rewritten_as_a_record() -> Result<(), TestError> {
        let bytes = fragment(b"export interface Shape { area(): number; }")?;
        let view = view(&bytes)?;
        let (ordinal, kind) = fact_named(&view, b"Shape")?;
        assert_eq!(kind, EntityKind::Trait);
        let record = type_fact(&view, ordinal)?;
        // The interface's declared type is its own diagonal nominal, never a
        // rewritten record.
        expect_tag(&record, SemanticTypeTag::Nominal)?;
        let (member, member_kind) = fact_named(&view, b"area")?;
        assert_eq!(member_kind, EntityKind::Function);
        assert!(member > ordinal);
        Ok(())
    }
}
