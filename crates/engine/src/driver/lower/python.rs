//! Projects source-backed Ruff Python facts into the canonical semantic lane.
//! Keeps parsing in-process and accepts no line scanner or reconstructed tokens.
//! The projection is honest by construction: every annotation becomes exactly
//! the lattice record the extractor proves, every unresolved position stays an
//! `Unknown` row carrying the census reason and its exact spelling, and no
//! dynamic fact is ever invented.
//!
//! Lane encoding laws this lowering obeys:
//! - Facts are emitted declarations-first (every class, with its legal
//!   diagonal self-nominal), then parameters, result slots, methods,
//!   variables, and aliases, so every type-record child and every non-self
//!   nominal target is strictly backward. A `TypedDict` class lowers to an
//!   `AnonymousRecord` over its member keys, and a `Protocol` class to an
//!   `AnonymousRecord` interface over its method signatures — both degrade
//!   to the self-nominal only when the pool cannot host a member row.
//! - A function is `function(param_count, result_count)`; its parameters are
//!   `Parameter` facts pushed immediately before it, an annotated return
//!   creates one `Parameter` result-slot fact, and the function's declared
//!   type is the `FunctionPointer` row over exactly those rows.
//! - Occurrences resolve against the module's own declared names at the
//!   extractor's `Index` tier: same-file targets are `Local`, import
//!   bindings become foreign `pypi` package keys, and everything else stays
//!   foreign with the written spelling.
//! - Because the lane borrows source bytes only, cells that would need a
//!   fresh buffer (slashed foreign paths, module-level owners, the module
//!   docstring) are documented limitations, never synthesized data.

use std::collections::{HashMap, HashSet};

use backend_frontend_python::legacy::{
    Annotation, AnnotationFact, AnnotationPosition, AttributeChainRoot, AttributeStep,
    BindingScopeFact, CheckerError, CheckerReport, ClassForm, DeclarationFact, DeclarationKind,
    ExtractionError, InferredType, LiteralValue, ModuleFacts, OccurrenceFact, OccurrenceKind,
    OccurrenceReceiver, ParameterKind, Pyrefly, ReceiverKind, Span, SymbolOutcome,
    TypeReason as ExtractedReason, extract,
};
use backend_semantic::ir::{
    AnonRecordForm, Confidence, DocFragmentInput, DocLinkTarget, EntityId, EntityKind, ForeignKey,
    ForeignOrigin, ListSpan, NominalRef, Occurrence, OccurrenceConfidence, OccurrenceTarget,
    PackageLineage, PrimitiveShape, ProductChildRole, PythonFacts, PythonParameterKind,
    ReferenceKind, RelSpan, SemanticProductConstructor, SemanticTypeChild, SemanticTypeRecord,
    SemanticTypeTag, TypeReason, TypeWidth,
};
use backend_semantic::vocabulary::{
    ProjectionForeignKeyFault, ProjectionLineagePart, ProjectionPackageLineageFault,
    PythonProjectionFault, PythonVersion,
};

use crate::driver::{
    lower::{
        EmissionExtension, FactSet, LEAF_PRODUCT, MAX_TYPE_CHILDREN, SemanticFact,
        StagedSourceSpan, push_fact,
    },
    types::{FactRejection, LoweringUnsupported},
};

/// Exact direct-authority rejection while borrowing Ruff declarations.
///
/// The variant set is consumed exhaustively by the driver's terminal
/// mapping, so checker failures intentionally do not widen it: a missing or
/// failing pyrefly never blocks the proven syntax lane, and the lane's
/// confidence tiers record the checker's absence honestly.
#[derive(Debug)]
pub(crate) enum PythonCollectError {
    /// Ruff rejected the caller's selected Python grammar or source bytes.
    Authority(ExtractionError),
    /// The optional checker was selected and then failed. This is distinct
    /// from an unavailable checker: erasing it would incorrectly claim a
    /// syntax-only projection succeeded under checker authority.
    Checker(CheckerError),
    /// Canonical declaration admission rejected an exact borrowed fact.
    Lowering(LoweringUnsupported),
    /// Canonical admission rejected one exact fact; operands retained.
    Rejected(FactRejection),
    /// Projection rejected one exact foreign-key authority fact.
    Projection(PythonProjectionFault),
    /// Ruff returned a declaration span outside the exact caller source.
    Span { start: u32, end: u32 },
}

/// Streams the complete Python semantic plane into the canonical lane.
///
/// Ruff owns spans, declarations, and written annotations. pyrefly runs as
/// the peer type authority: its typed report fills unannotated constants and
/// parameters, upgrades resolved call sites to `Import`/`Oracle` evidence,
/// and lifts dynamic-confidence tiers. Only an unavailable checker degrades
/// to syntax facts; once the checker starts, its exact failure is terminal.
pub(crate) fn collect<'source>(
    profile: PythonVersion,
    source: &'source [u8],
    facts: &mut FactSet<'source>,
) -> Result<(), PythonCollectError> {
    let module = extract(source, profile).map_err(PythonCollectError::Authority)?;
    let checker = match checked_report(profile, source, &module) {
        CheckerOutcome::Unavailable => None,
        CheckerOutcome::Available(report) => Some(report),
        CheckerOutcome::Rejected(cause) => return Err(PythonCollectError::Checker(cause)),
    };
    collect_with_checker(&module, source, facts, checker.as_ref())
}

/// Streams the syntax-proven plane only, with no type-authority transaction.
///
/// Test and deterministic paths pin this entry point so laws hold identically
/// on machines with and without pyrefly provisioned.
pub(crate) fn collect_syntax_only<'source>(
    profile: PythonVersion,
    source: &'source [u8],
    facts: &mut FactSet<'source>,
) -> Result<(), PythonCollectError> {
    let module = extract(source, profile).map_err(PythonCollectError::Authority)?;
    collect_with_checker(&module, source, facts, None)
}

/// Streams the plane with a caller-supplied type-authority report.
pub(crate) fn collect_with_checker<'a, 'source>(
    module: &'a ModuleFacts,
    source: &'source [u8],
    facts: &mut FactSet<'source>,
    checker: Option<&'a CheckerReport>,
) -> Result<(), PythonCollectError> {
    let mut emitter = Emitter::new(source, module, facts, checker);
    emitter.emit_classes()?;
    emitter.emit_non_class_declarations()?;
    emitter.emit_parentage()?;
    emitter.emit_occurrences()?;
    emitter.emit_docs()?;
    Ok(())
}

/// Runs the pyrefly authority transaction, preserving its typed terminal.
///
/// An explicit checker result. `Unavailable` means no authority transaction
/// was attempted. `Rejected` retains the exact transaction cause; it must
/// never be silently recast as an absent report.
pub(crate) enum CheckerOutcome {
    Unavailable,
    Available(CheckerReport),
    Rejected(CheckerError),
}

/// `Unavailable` is the honest answer when pyrefly is not configured. A
/// started checker returns either its report or its untouched typed failure.
pub(crate) fn checked_report(
    profile: PythonVersion,
    source: &[u8],
    module: &ModuleFacts,
) -> CheckerOutcome {
    let adapter = Pyrefly::from_env();
    if !adapter.is_available() {
        return CheckerOutcome::Unavailable;
    }
    match adapter.analyze(source, profile, module) {
        Ok(report) => CheckerOutcome::Available(report),
        Err(cause) => CheckerOutcome::Rejected(cause),
    }
}

/// One per-emission lookup index over a checker report. The report remains
/// the immutable authority record; this staging-only index removes repeated
/// linear scans while preserving every borrowed outcome.
struct CheckerIndex<'report> {
    inferences: std::collections::HashMap<(u32, u32), &'report InferredType>,
    symbols: std::collections::HashMap<(u32, u32), &'report SymbolOutcome>,
}

impl<'report> CheckerIndex<'report> {
    fn build(report: &'report CheckerReport) -> Self {
        let mut inferences = std::collections::HashMap::with_capacity(report.inferences.len());
        for inference in &report.inferences {
            inferences
                .entry((inference.site.start, inference.site.end))
                .or_insert(&inference.observed);
        }
        let mut symbols = std::collections::HashMap::with_capacity(report.symbols.len());
        for symbol in &report.symbols {
            symbols
                .entry((symbol.span.start, symbol.span.end))
                .or_insert(&symbol.outcome);
        }
        Self {
            inferences,
            symbols,
        }
    }

    fn inference_at(&self, site: Span) -> Option<&'report InferredType> {
        self.inferences.get(&(site.start, site.end)).copied()
    }

    fn symbol_at(&self, span: Span) -> Option<&'report SymbolOutcome> {
        self.symbols.get(&(span.start, span.end)).copied()
    }
}

/// One pushed declaration row with the coordinates every later phase needs.
#[derive(Clone, Copy)]
struct Pushed<'source> {
    ordinal: u32,
    name: &'source [u8],
    span: Span,
    /// The imported module spelling range when this row is an alias.
    imported: Option<Span>,
}

/// The two-pass Python semantic emitter over one extracted module.
struct Emitter<'a, 'source> {
    source: &'source [u8],
    module: &'a ModuleFacts,
    facts: &'a mut FactSet<'source>,
    /// Lane ordinal per extracted declaration index; `None` for the module
    /// row, which the frozen driver fact set does not carry.
    ordinals: Vec<Option<u32>>,
    /// Live bindings after Python shadowing collapse: `false` for a prior
    /// same `(owner, kind, name)` binding shadowed by a later byte-identical
    /// twin (later wins) and for the dead subtree it owns. Distinct overloads
    /// keep distinct fingerprints, so every live signature survives.
    live: Vec<bool>,
    /// Pushed declaration rows in lane order, built while emitting.
    pushed: Vec<Pushed<'source>>,
    /// Parameter and result-slot ordinals with their owning declaration
    /// index and source span, bound to their function by the parentage pass.
    child_rows: Vec<(u32, usize, Span)>,
    /// The pyrefly authority report, when the checker answered.
    checker: Option<CheckerIndex<'a>>,
    /// Reserved owner while the first structural class is being lowered.
    reserved_anchor: Option<u32>,
}

/// How one module-level spelling is bound for nominal resolution.
enum BindingResolution {
    Nominal(u32),
    Ambiguous,
    External,
}

impl BindingResolution {
    fn nominal_ordinal(&self) -> Option<u32> {
        match self {
            BindingResolution::Nominal(ordinal) => Some(*ordinal),
            BindingResolution::Ambiguous => None,
            BindingResolution::External => None,
        }
    }
}

/// The interned name tables annotation lowering resolves against.
struct TypeTables<'source> {
    classes: Vec<(&'source [u8], u32)>,
    typevars: Vec<&'source [u8]>,
    bindings: HashMap<String, BindingResolution>,
}

impl TypeTables<'_> {
    fn bound_nominal(&self, name: &str) -> Option<u32> {
        if let Some(ordinal) = self.direct_bound_nominal(name) {
            return Some(ordinal);
        }
        self.qualified_bound_nominal(name)
    }

    fn direct_bound_nominal(&self, name: &str) -> Option<u32> {
        match self.bindings.get(name) {
            Some(resolution) => resolution.nominal_ordinal(),
            None => None,
        }
    }

    fn qualified_bound_nominal(&self, name: &str) -> Option<u32> {
        let mut start = 0;
        while let Some(rel) = name[start..].find('.') {
            let dot = start + rel;
            let prefix = &name[..dot];
            let rest = &name[dot + 1..];
            if self.direct_bound_nominal(prefix).is_some() {
                if let Some((_, ordinal)) = self
                    .classes
                    .iter()
                    .find(|(known, _)| *known == rest.as_bytes())
                {
                    return Some(*ordinal);
                }
            }
            start = dot + 1;
        }
        None
    }

    fn binding_state(&self, name: &str) -> Option<&BindingResolution> {
        self.bindings.get(name)
    }
}

/// One annotation lowered into its lattice record plus the fact ordinals
/// its record needs as type-record children (only unions produce any).
struct LoweredType<'source> {
    record: SemanticTypeRecord<'source>,
    children: Vec<u32>,
    /// True when the record is a proven (non-`Unknown`) type state.
    resolved: bool,
}

impl<'a, 'source> Emitter<'a, 'source> {
    fn new(
        source: &'source [u8],
        module: &'a ModuleFacts,
        facts: &'a mut FactSet<'source>,
        checker: Option<&'a CheckerReport>,
    ) -> Self {
        let ordinals = vec![None; module.declarations.len()];
        let live = compute_live_set(module);
        Self {
            source,
            module,
            facts,
            ordinals,
            live,
            pushed: Vec::new(),
            child_rows: Vec::new(),
            checker: checker.map(CheckerIndex::build),
            reserved_anchor: None,
        }
    }

    /// Borrows an exact source range or fails with the typed span terminal.
    fn slice(&self, span: Span) -> Result<&'source [u8], PythonCollectError> {
        let (start, end) = span_bounds(span)?;
        self.source.get(start..end).ok_or(PythonCollectError::Span {
            start: span.start,
            end: span.end,
        })
    }

    /// Widenes one lane ordinal to its wire coordinate, retaining the source
    /// span when the coordinate cannot be represented. Unreachable within
    /// the lane's 128-fact bound, but never silently truncated.
    fn coordinate(span: Span, ordinal: usize) -> Result<u32, PythonCollectError> {
        u32::try_from(ordinal).map_err(|_| PythonCollectError::Span {
            start: span.start,
            end: span.end,
        })
    }

    /// Pass one: every class, each with its legal diagonal self-nominal, so
    /// any later annotation can name any class in the module — except a
    /// `TypedDict` class, which lowers to an `AnonymousRecord` over its
    /// member keys, and a `Protocol` class, which lowers to an
    /// `AnonymousRecord` interface over its method signatures. Either
    /// structural form degrades to the self-nominal only when a member row
    /// cannot be hosted (no anchor yet, a member the pool cannot express, or
    /// more members than the bounded child lane holds).
    fn emit_classes(&mut self) -> Result<(), PythonCollectError> {
        let indices: Vec<usize> = self
            .module
            .declarations
            .iter()
            .enumerate()
            .filter(|(index, declaration)| {
                declaration.kind == DeclarationKind::Class && self.live[*index]
            })
            .map(|(index, _)| index)
            .collect();
        let mut tables = TypeTables {
            classes: Vec::new(),
            typevars: self.module_typevar_names()?,
            bindings: HashMap::new(),
        };
        for index in indices {
            let declaration = &self.module.declarations[index];
            let name = self.slice(declaration.name_span)?;
            // The self-nominal names the row being pushed: the one closed
            // diagonal case the lane's backward law admits.
            let own_ordinal = Self::coordinate(declaration.name_span, self.facts.len())?;
            let anchor = own_ordinal;
            self.reserved_anchor = Some(anchor);
            let (record, members, tier) =
                match self.structural_class_members(declaration, &mut tables, anchor)? {
                    Some((record, members)) => (record, members, Confidence::Indexed),
                    None => (
                        SemanticTypeRecord {
                            tag: SemanticTypeTag::Nominal,
                            payload0: 0,
                            payload1: 0,
                            text: None,
                            text2: None,
                            nominal: Some(NominalRef::Local(EntityId::new(own_ordinal))),
                            children: ListSpan::new(0, 0),
                        },
                        Vec::new(),
                        Confidence::Syntactic,
                    ),
                };
            self.reserved_anchor = None;
            let extension = self.python_extension(&declaration.decorator_spans, None, tier)?;
            let mut fact = SemanticFact::new(
                EntityKind::Record,
                name,
                SemanticProductConstructor::PRODUCT,
            )
            .typed(record);
            for (key, row, flags) in members {
                fact = fact.type_child(row, Some(key), flags);
            }
            let fact = fact.with_extension(EmissionExtension::Python(extension));
            let ordinal = push_fact(self.facts, fact).map_err(PythonCollectError::Rejected)?;
            if let Ok(coordinate) = u32::try_from(ordinal) {
                tables.classes.push((name, coordinate));
            }
            self.record_pushed(index, ordinal, name, declaration);
        }
        Ok(())
    }

    /// The member rows of one structural class record: `TypedDict` fields
    /// under their exact member keys (with `total=`/`Required`/`NotRequired`
    /// optionality flags), or `Protocol` methods under their exact names as
    /// callable signature rows. `None` means the class keeps its plain
    /// self-nominal: no anchor row exists yet, a member has no hostable row,
    /// or the bounded member lane would overflow.
    fn structural_class_members(
        &mut self,
        declaration: &DeclarationFact,
        tables: &mut TypeTables<'source>,
        anchor: u32,
    ) -> Result<
        Option<(SemanticTypeRecord<'source>, Vec<(&'source [u8], u32, u8)>)>,
        PythonCollectError,
    > {
        let form = match declaration.class_form {
            Some(ClassForm::TypedDict) => AnonRecordForm::Struct,
            Some(ClassForm::Protocol) => AnonRecordForm::Interface,
            _ => return Ok(None),
        };
        let mut members: Vec<(&'source [u8], u32, u8)> = Vec::new();
        for member_index in self.structural_member_indices(declaration) {
            if members.len() >= MAX_TYPE_CHILDREN {
                return Ok(None);
            }
            let member = &self.module.declarations[member_index];
            let key = self.slice(member.name_span)?;
            let (row, flags) = match declaration.class_form {
                Some(ClassForm::TypedDict) => {
                    match self.typed_dict_member_row(member, declaration.total, tables, anchor)? {
                        Some(member_row) => member_row,
                        None => return Ok(None),
                    }
                }
                _ => match self.protocol_method_row(member, tables, anchor)? {
                    Some(row) => (row, 0),
                    None => return Ok(None),
                },
            };
            members.push((key, row, flags));
        }
        let record = SemanticTypeRecord {
            tag: SemanticTypeTag::AnonymousRecord,
            payload0: u32::from(form),
            payload1: 0,
            text: None,
            text2: None,
            nominal: None,
            children: ListSpan::new(0, 0),
        };
        Ok(Some((record, members)))
    }

    /// The declaration indices of one class's direct structural members:
    /// fields for a `TypedDict`, functions for a `Protocol`, each belonging
    /// to the innermost class containing it so nested classes never leak
    /// members.
    fn structural_member_indices(&self, class: &DeclarationFact) -> Vec<usize> {
        let wanted_kind = match class.class_form {
            Some(ClassForm::TypedDict) => DeclarationKind::Field,
            _ => DeclarationKind::Function,
        };
        self.module
            .declarations
            .iter()
            .enumerate()
            .filter(|(index, candidate)| {
                self.live[*index]
                    && candidate.kind == wanted_kind
                    && span_contains(class.span, candidate.span)
                    && self.module.declarations.iter().all(|other| {
                        other.kind != DeclarationKind::Class
                            || other.span == class.span
                            || !span_contains(other.span, candidate.span)
                    })
            })
            .map(|(index, _)| index)
            .collect()
    }

    /// One `TypedDict` member: its annotation's row plus its optionality
    /// flags. PEP 589 makes every member required unless the class declared
    /// `total=False`; PEP 655 `NotRequired[...]`/`Required[...]` override
    /// the class default per key. `None` means the member row is not
    /// hostable, degrading the whole record.
    fn typed_dict_member_row(
        &mut self,
        member: &DeclarationFact,
        class_total: Option<bool>,
        tables: &TypeTables<'source>,
        anchor: u32,
    ) -> Result<Option<(u32, u8)>, PythonCollectError> {
        let Some(found) = self.field_annotation(member) else {
            return Ok(None);
        };
        let (inner, required) = match &found.annotation {
            Annotation::Generic { base, args } if matches!(base.as_ref(), Annotation::Name { name, .. } if name == "NotRequired" || name == "typing.NotRequired") => {
                (args.first().cloned(), Some(false))
            }
            Annotation::Generic { base, args } if matches!(base.as_ref(), Annotation::Name { name, .. } if name == "Required" || name == "typing.Required") => {
                (args.first().cloned(), Some(true))
            }
            other => (Some(other.clone()), None),
        };
        let optional = match (required, class_total) {
            (Some(required), _) => !required,
            (None, Some(total)) => !total,
            (None, None) => false,
        };
        let Some(inner) = inner else {
            return Ok(None);
        };
        let Some(row) = self.type_row(&inner, Some(found.span), tables, anchor)? else {
            return Ok(None);
        };
        let flags = if optional {
            SemanticTypeChild::FLAG_OPTIONAL
        } else {
            0
        };
        Ok(Some((row, flags)))
    }

    /// One `Protocol` member: the callable signature row of one method —
    /// parameters in declaration order, then the optional result row with
    /// the result flag committed. A property member is the row of its
    /// return type; a classmethod drops its `cls` receiver. Unannotated
    /// parameters take the type authority's inference when it proved one,
    /// otherwise the honest unknown row, so the signature arity is always
    /// preserved. `None` means the member row is not hostable, degrading
    /// the whole record.
    fn protocol_method_row(
        &mut self,
        member: &DeclarationFact,
        tables: &TypeTables<'source>,
        anchor: u32,
    ) -> Result<Option<u32>, PythonCollectError> {
        if member.receiver == ReceiverKind::Property {
            return match self.return_annotation(member) {
                Some(annotation) => self.type_row(
                    &annotation.annotation,
                    Some(annotation.span),
                    tables,
                    anchor,
                ),
                None => self.leaf_row(unknown_record(TypeReason::Unannotated), anchor),
            };
        }
        let mut children = Vec::new();
        for (position, parameter) in member.parameters.iter().enumerate() {
            // A classmethod signature does not include its `cls` receiver.
            if position == 0 && member.receiver == ReceiverKind::ClassMethod {
                continue;
            }
            let unannotated = matches!(
                parameter.annotation,
                Annotation::Unknown(ExtractedReason::Unannotated { .. })
            );
            let row = if unannotated {
                // An unannotated parameter takes the type authority's
                // inference when it proved one, otherwise the honest
                // unknown row, so the signature arity stays exact.
                match self.checker_inference(parameter.name_span, true) {
                    Some(inferred) => match self.inferred_row(inferred, tables, anchor)? {
                        Some(row) => row,
                        None => return Ok(None),
                    },
                    None => {
                        match self.leaf_row(unknown_record(TypeReason::Unannotated), anchor)? {
                            Some(row) => row,
                            None => return Ok(None),
                        }
                    }
                }
            } else {
                match self.type_row(
                    &parameter.annotation,
                    parameter.annotation_span,
                    tables,
                    anchor,
                )? {
                    Some(row) => row,
                    None => return Ok(None),
                }
            };
            children.push(row);
        }
        let mut payload1 = 0;
        if let Some(annotation) = self.return_annotation(member) {
            match self.type_row(
                &annotation.annotation,
                Some(annotation.span),
                tables,
                anchor,
            )? {
                Some(row) => {
                    children.push(row);
                    payload1 = SemanticTypeRecord::FUNCTION_RESULT_COUNT_ONE;
                }
                None => return Ok(None),
            }
        }
        self.parent_row(function_pointer_record(payload1), &children, anchor)
    }

    /// The module-level `TypeVar(...)` binding names annotations resolve
    /// against, independent of any pushed row. Shadowed bindings are dead,
    /// so only live rows contribute names.
    fn module_typevar_names(&self) -> Result<Vec<&'source [u8]>, PythonCollectError> {
        let mut typevars = Vec::new();
        for (index, declaration) in self.module.declarations.iter().enumerate() {
            if !self.live[index] {
                continue;
            }
            let is_typevar = declaration.kind == DeclarationKind::Constant
                && declaration
                    .value_source
                    .as_deref()
                    .is_some_and(|value| value.starts_with("TypeVar("));
            if is_typevar {
                typevars.push(self.slice(declaration.name_span)?);
            }
        }
        Ok(typevars)
    }

    /// Pass two: functions (parameters and result slots first), then
    /// variables and aliases, all in source order. Shadowed bindings are
    /// skipped: the extractor already dropped later module-level rebindings,
    /// and identical twins keep the first declaration.
    fn emit_non_class_declarations(&mut self) -> Result<(), PythonCollectError> {
        let tables = self.type_tables()?;
        for index in 0..self.module.declarations.len() {
            if !self.live[index] {
                continue;
            }
            let declaration = &self.module.declarations[index];
            match declaration.kind {
                DeclarationKind::Module | DeclarationKind::Class => {}
                DeclarationKind::Function => self.emit_function(index, &tables)?,
                DeclarationKind::Field | DeclarationKind::Constant => {
                    self.emit_variable(index, &tables)?
                }
                DeclarationKind::Alias => self.emit_alias(index, &tables)?,
            }
        }
        Ok(())
    }

    /// Pass three: binds every pushed row to its lexical owner. Parameters
    /// and result slots join their function; every other declaration joins
    /// its innermost enclosing class or function, or the module root when
    /// nothing encloses it. Real modules reuse parameter and attribute
    /// names across siblings, so without this pass two same-named rows
    /// share one parentage-unavailable family and the image build rejects
    /// the honest duplicate. An owner that was never pushed leaves its
    /// child parentage-unavailable rather than fabricated as a root.
    /// Shadowed owners are dead, so the search skips them: live rows only
    /// ever bind to live parents.
    ///
    /// The same pass attaches every row's authority-backed source span, so
    /// a real module keeps exact name extents instead of spanless rows.
    fn emit_parentage(&mut self) -> Result<(), PythonCollectError> {
        let module = self.module;
        for index in 0..module.declarations.len() {
            let Some(ordinal) = self.ordinals[index] else {
                continue;
            };
            let span = module.declarations[index].span;
            self.attach_span(ordinal, span)?;
            let mut owner: Option<usize> = None;
            let mut owner_area = u64::MAX;
            for (candidate, declaration) in module.declarations.iter().enumerate() {
                if candidate == index || !self.live[candidate] {
                    continue;
                }
                if !matches!(
                    declaration.kind,
                    DeclarationKind::Class | DeclarationKind::Function
                ) {
                    continue;
                }
                if !span_contains(declaration.span, span) {
                    continue;
                }
                if declaration.span.start == span.start && declaration.span.end == span.end {
                    continue;
                }
                let area = u64::from(declaration.span.end.saturating_sub(declaration.span.start));
                if area < owner_area {
                    owner_area = area;
                    owner = Some(candidate);
                }
            }
            match owner {
                None => self
                    .facts
                    .mark_parentage_root(ordinal)
                    .map_err(|fault| parentage_fault(span.start, span.end, fault))?,
                Some(candidate) => {
                    if let Some(parent) = self.ordinals[candidate] {
                        self.facts
                            .attach_parent(ordinal, parent)
                            .map_err(|fault| parentage_fault(span.start, span.end, fault))?;
                    }
                }
            }
        }
        for (child, candidate, span) in core::mem::take(&mut self.child_rows) {
            self.attach_span(child, span)?;
            if let Some(parent) = self.ordinals[candidate] {
                self.facts
                    .attach_parent(child, parent)
                    .map_err(|fault| parentage_fault(span.start, span.end, fault))?;
            }
        }
        Ok(())
    }

    /// Attaches one authority-backed source span to an admitted row.
    fn attach_span(&mut self, ordinal: u32, span: Span) -> Result<(), PythonCollectError> {
        let Some(staged) = StagedSourceSpan::new(span.start, span.end) else {
            return Err(PythonCollectError::Span {
                start: span.start,
                end: span.end,
            });
        };
        self.facts
            .attach_source_span(ordinal, staged)
            .map_err(|fault| parentage_fault(span.start, span.end, fault))
    }

    /// Interns the class and `TypeVar` name tables annotations resolve
    /// against. Classes are all pushed by pass one, so every nominal target
    /// they name is already in the lane. The `TypeVar` table joins
    /// module-level `TypeVar(...)` bindings with every PEP 695
    /// type-parameter name declared in this module (`class Box[T]`,
    /// `def f[T]`, `type X[T]`): all of them mint the same `TypeVar` row
    /// carrying the annotation's exact written spelling, so the flat table
    /// is name-true even where its scope is wider than PEP 695's.
    fn type_tables(&self) -> Result<TypeTables<'source>, PythonCollectError> {
        let mut classes = Vec::new();
        for (index, declaration) in self.module.declarations.iter().enumerate() {
            if declaration.kind != DeclarationKind::Class {
                continue;
            }
            let Some(ordinal) = self.ordinals[index] else {
                continue;
            };
            let name = self.slice(declaration.name_span)?;
            classes.push((name, ordinal));
        }
        let mut typevars = self.module_typevar_names()?;
        for (index, declaration) in self.module.declarations.iter().enumerate() {
            if !self.live[index] {
                continue;
            }
            for parameter in &declaration.type_parameters {
                typevars.push(self.slice(*parameter)?);
            }
        }
        let bindings = self.name_bindings(&classes)?;
        Ok(TypeTables { classes, typevars, bindings })
    }

    fn name_bindings(
        &self,
        classes: &[(&'source [u8], u32)],
    ) -> Result<HashMap<String, BindingResolution>, PythonCollectError> {
        let text = std::str::from_utf8(self.source).map_err(|error| {
            let start = error.valid_up_to();
            let end = error
                .error_len()
                .map_or(self.source.len(), |l| start.saturating_add(l))
                .min(self.source.len());
            PythonCollectError::Span {
                start: u32::try_from(start).unwrap_or(0),
                end: u32::try_from(end).unwrap_or(0),
            }
        })?;
        let mut bound = import_name_bindings(text, &self.module.identity, classes);
        let mut locals: HashMap<String, u32> = HashMap::new();
        for (name, ordinal) in classes {
            record_local_binding(&mut bound, &mut locals, name, *ordinal);
        }
        for (index, declaration) in self.module.declarations.iter().enumerate() {
            if !self.live[index] || declaration.kind != DeclarationKind::Alias {
                continue;
            }
            let is_type_alias =
                declaration.value_span.is_none() && declaration.value_source.is_some();
            if !is_type_alias {
                continue;
            }
            let Some(annotation) = self.alias_value_annotation(declaration) else {
                continue;
            };
            let Some(target) = annotation_root_name(&annotation.annotation) else {
                continue;
            };
            let resolution = resolve_alias_target(&bound, classes, &target);
            apply_type_alias_binding(
                &mut bound,
                &mut locals,
                &declaration.name,
                resolution,
            );
        }
        Ok(bound)
    }

    /// Lowers one function: its parameter facts and annotated-return result
    /// slot first, then the function row whose ordered product children and
    /// `FunctionPointer` type children are exactly those rows.
    fn emit_function(
        &mut self,
        index: usize,
        tables: &TypeTables<'source>,
    ) -> Result<(), PythonCollectError> {
        let declaration = &self.module.declarations[index];
        let name = self.slice(declaration.name_span)?;
        let mut parameter_ordinals = Vec::new();
        // The shape each minted parameter fact carries, kept so the result
        // slot below can prove identity reuse instead of minting a duplicate
        // declaration.
        let mut parameter_shapes: Vec<(&'source [u8], SemanticTypeRecord<'source>, Vec<u32>)> =
            Vec::new();
        let mut any_resolved = false;
        let mut any_checked = false;
        for parameter in &declaration.parameters {
            if is_receiver_parameter(declaration.receiver, &parameter.name) {
                continue;
            }
            let unannotated = matches!(
                parameter.annotation,
                Annotation::Unknown(ExtractedReason::Unannotated { .. })
            );
            let (lowered_record, children, tier) =
                match self.checker_inference(parameter.name_span, unannotated) {
                    Some(inferred) => match self.inferred_root(inferred, tables)? {
                        Some((record, children)) => (record, children, Confidence::Compiler),
                        // The checker answered; the lane cannot host the
                        // inferred shape. The honest gap names the oracle.
                        None => (
                            unknown_record(TypeReason::OracleGap),
                            Vec::new(),
                            Confidence::Compiler,
                        ),
                    },
                    None => {
                        let lowered = self.lower_annotation(
                            &parameter.annotation,
                            parameter.annotation_span,
                            tables,
                        )?;
                        (
                            lowered.record,
                            lowered.children,
                            lowered_tier(lowered.resolved),
                        )
                    }
                };
            any_resolved = any_resolved || tier == Confidence::Indexed;
            any_checked = any_checked || tier == Confidence::Compiler;
            let parameter_name = self.slice(parameter.name_span)?;
            let extension = self.python_extension(&[], Some(parameter.kind), tier)?;
            let mut fact = SemanticFact::new(EntityKind::Parameter, parameter_name, LEAF_PRODUCT)
                .typed(lowered_record);
            for ordinal in &children {
                fact = fact.type_child(*ordinal, None, 0);
            }
            let fact = fact.with_extension(EmissionExtension::Python(extension));
            let ordinal = push_fact(self.facts, fact).map_err(PythonCollectError::Rejected)?;
            let coordinate = Self::coordinate(parameter.name_span, ordinal)?;
            parameter_ordinals.push(coordinate);
            parameter_shapes.push((parameter_name, lowered_record, children));
            self.child_rows
                .push((coordinate, index, parameter.name_span));
        }
        let returns = self.return_annotation(declaration);
        let mut result_ordinal = None;
        let mut return_resolved = false;
        if let Some(annotation) = returns {
            let lowered =
                self.lower_annotation(&annotation.annotation, Some(annotation.span), tables)?;
            return_resolved = lowered.resolved;
            // The result slot belongs to the function's key family: it is a
            // `Parameter` fact named by the function, carrying the return
            // annotation's record and its ordered type children exactly like
            // every other annotation fact. A parameter that shares the
            // function's name and proves the identical shape is that same
            // declaration under the identity model — minting a second fact
            // would collide byte-for-byte in family and variant, so the slot
            // reuses the parameter's row and the function's result child
            // points there.
            let reused = parameter_ordinals
                .iter()
                .zip(&parameter_shapes)
                .find(|(_, (parameter_name, record, children))| {
                    *parameter_name == name
                        && *record == lowered.record
                        && *children == lowered.children
                })
                .map(|(ordinal, _)| *ordinal);
            if let Some(ordinal) = reused {
                result_ordinal = Some(ordinal);
            } else {
                let extension = self.python_extension(&[], None, lowered_tier(lowered.resolved))?;
                let mut fact = SemanticFact::new(EntityKind::Parameter, name, LEAF_PRODUCT)
                    .typed(lowered.record);
                for ordinal in lowered.children {
                    fact = fact.type_child(ordinal, None, 0);
                }
                let fact = fact.with_extension(EmissionExtension::Python(extension));
                let ordinal = push_fact(self.facts, fact).map_err(PythonCollectError::Rejected)?;
                let coordinate = Self::coordinate(declaration.name_span, ordinal)?;
                self.child_rows
                    .push((coordinate, index, declaration.name_span));
                result_ordinal = Some(coordinate);
            }
        }
        let arity =
            u32::try_from(parameter_ordinals.len()).map_err(|_| PythonCollectError::Span {
                start: declaration.name_span.start,
                end: declaration.name_span.end,
            })?;
        let mut fact = SemanticFact::new(
            EntityKind::Function,
            name,
            SemanticProductConstructor::function(arity, u32::from(result_ordinal.is_some())),
        );
        for ordinal in &parameter_ordinals {
            fact = fact.child(ProductChildRole::FunctionParameter, *ordinal);
        }
        if let Some(result) = result_ordinal {
            fact = fact.child(ProductChildRole::FunctionResult, result);
        }
        let mut payload1 = 0;
        if result_ordinal.is_some() {
            payload1 = SemanticTypeRecord::FUNCTION_RESULT_COUNT_ONE;
        }
        for ordinal in parameter_ordinals.iter().copied().chain(result_ordinal) {
            fact = fact.type_child(ordinal, None, 0);
        }
        let record = SemanticTypeRecord {
            tag: SemanticTypeTag::FunctionPointer,
            payload0: 0,
            payload1,
            text: None,
            text2: None,
            nominal: None,
            children: ListSpan::new(0, 0),
        };
        let extension = self.python_extension(
            &declaration.decorator_spans,
            None,
            combined_tier(any_checked, any_resolved || return_resolved),
        )?;
        let fact = fact
            .typed(record)
            .with_extension(EmissionExtension::Python(extension));
        let ordinal = push_fact(self.facts, fact).map_err(PythonCollectError::Rejected)?;
        self.record_pushed(index, ordinal, name, declaration);
        Ok(())
    }

    /// Lowers one annotated or unannotated variable (class field or module
    /// constant). An unwritten annotation is filled from the type authority
    /// when pyrefly inferred a usable type; when it inferred nothing, the
    /// position stays exactly `Unknown(Unannotated)`.
    fn emit_variable(
        &mut self,
        index: usize,
        tables: &TypeTables<'source>,
    ) -> Result<(), PythonCollectError> {
        let declaration = &self.module.declarations[index];
        let name = self.slice(declaration.name_span)?;
        let kind = match declaration.kind {
            DeclarationKind::Field => EntityKind::Field,
            _ => EntityKind::Static,
        };
        let (lowered_record, children, tier) = match self.field_annotation(declaration) {
            Some(annotation) => {
                let lowered =
                    self.lower_annotation(&annotation.annotation, Some(annotation.span), tables)?;
                (
                    lowered.record,
                    lowered.children,
                    lowered_tier(lowered.resolved),
                )
            }
            None => match self.checker_inference(declaration.name_span, true) {
                Some(inferred) => match self.inferred_root(inferred, tables)? {
                    Some((record, children)) => (record, children, Confidence::Compiler),
                    None => (
                        unknown_record(TypeReason::OracleGap),
                        Vec::new(),
                        Confidence::Compiler,
                    ),
                },
                None => (
                    unknown_record(TypeReason::Unannotated),
                    Vec::new(),
                    Confidence::Syntactic,
                ),
            },
        };
        let extension = self.python_extension(&[], None, tier)?;
        let mut fact = SemanticFact::new(kind, name, LEAF_PRODUCT).typed(lowered_record);
        for ordinal in children {
            fact = fact.type_child(ordinal, None, 0);
        }
        let fact = fact.with_extension(EmissionExtension::Python(extension));
        let ordinal = push_fact(self.facts, fact).map_err(PythonCollectError::Rejected)?;
        self.record_pushed(index, ordinal, name, declaration);
        Ok(())
    }

    /// The type authority's inference at one exact site, when the position
    /// is unannotated and the checker proved something usable. `Any` means
    /// the checker proved nothing, so the answer is `None`.
    fn checker_inference(&self, site: Span, unannotated: bool) -> Option<&'a InferredType> {
        if !unannotated {
            return None;
        }
        self.checker
            .as_ref()
            .and_then(|report| report.inference_at(site))
            .filter(|inferred| **inferred != InferredType::Any)
    }

    /// Lowers one alias declaration. An import binding's declared type is
    /// honestly unwritten. A PEP 695 `type` alias lowers its written value
    /// exactly like any annotation: a hostable value becomes the alias's
    /// typed record at the indexed tier, and an unhostable one keeps its
    /// exact written spelling as the countable gap.
    fn emit_alias(
        &mut self,
        index: usize,
        tables: &TypeTables<'source>,
    ) -> Result<(), PythonCollectError> {
        let declaration = &self.module.declarations[index];
        let name = self.slice(declaration.name_span)?;
        // A `type` alias carries a written value and no import module span;
        // an import binding carries the module span instead.
        let is_type_alias = declaration.value_span.is_none() && declaration.value_source.is_some();
        if is_type_alias && let Some(annotation) = self.alias_value_annotation(declaration) {
            let lowered =
                self.lower_annotation(&annotation.annotation, Some(annotation.span), tables)?;
            let extension = self.python_extension(&[], None, lowered_tier(lowered.resolved))?;
            let mut fact =
                SemanticFact::new(EntityKind::Alias, name, LEAF_PRODUCT).typed(lowered.record);
            for ordinal in lowered.children {
                fact = fact.type_child(ordinal, None, 0);
            }
            let fact = fact.with_extension(EmissionExtension::Python(extension));
            let ordinal = push_fact(self.facts, fact).map_err(PythonCollectError::Rejected)?;
            self.record_pushed(index, ordinal, name, declaration);
            return Ok(());
        }
        let extension = self.python_extension(&[], None, Confidence::Syntactic)?;
        let fact = SemanticFact::new(EntityKind::Alias, name, LEAF_PRODUCT)
            .typed(unknown_record(TypeReason::Unannotated))
            .with_extension(EmissionExtension::Python(extension));
        let ordinal = push_fact(self.facts, fact).map_err(PythonCollectError::Rejected)?;
        self.record_pushed(index, ordinal, name, declaration);
        Ok(())
    }

    /// The written value annotation of one PEP 695 `type` alias, matched by
    /// exact owner name and span containment, the same law as the other
    /// annotation lookups.
    fn alias_value_annotation(&self, declaration: &DeclarationFact) -> Option<&'a AnnotationFact> {
        let module = self.module;
        module.annotations.iter().find(|candidate| {
            candidate.position == AnnotationPosition::AliasValue
                && candidate.owner == declaration.name
                && span_contains(declaration.span, candidate.span)
        })
    }

    /// Records one pushed declaration row for the owner, target, and link
    /// resolution of the later phases.
    fn record_pushed(
        &mut self,
        index: usize,
        ordinal: usize,
        name: &'source [u8],
        declaration: &DeclarationFact,
    ) {
        if let Ok(coordinate) = u32::try_from(ordinal) {
            self.ordinals[index] = Some(coordinate);
            self.pushed.push(Pushed {
                ordinal: coordinate,
                name,
                span: declaration.span,
                imported: declaration.value_span,
            });
        }
    }

    /// The return annotation of one function, found by exact owner name and
    /// span containment, so overloaded names never swap annotations.
    fn return_annotation(&self, declaration: &DeclarationFact) -> Option<&'a AnnotationFact> {
        let module = self.module;
        module.annotations.iter().find(|candidate| {
            candidate.position == AnnotationPosition::Return
                && candidate.owner == declaration.name
                && span_contains(declaration.span, candidate.span)
        })
    }

    /// The annotation of one field or constant, matched the same way.
    fn field_annotation(&self, declaration: &DeclarationFact) -> Option<&'a AnnotationFact> {
        let module = self.module;
        module.annotations.iter().find(|candidate| {
            candidate.position == AnnotationPosition::Field
                && candidate.owner == declaration.name
                && span_contains(declaration.span, candidate.span)
        })
    }

    /// Builds the per-declaration Python extension row: interned decorator
    /// spellings, the exact parameter convention where the row is a
    /// parameter, and the dynamic-confidence tier of its type state.
    fn python_extension(
        &mut self,
        decorators: &[Span],
        parameter_kind: Option<ParameterKind>,
        tier: Confidence,
    ) -> Result<PythonFacts, PythonCollectError> {
        let mut atoms = Vec::new();
        for span in decorators {
            let spelling = self.decorator_spelling(*span)?;
            let atom = self.facts.intern_atom(spelling).map_err(lane_rejected)?;
            atoms.push(atom);
        }
        let decorators = self.facts.intern_atom_list(&atoms).map_err(lane_rejected)?;
        Ok(PythonFacts {
            decorators,
            parameter_kind: parameter_kind.map_or(
                PythonParameterKind::PositionalOrKeyword,
                parameter_convention,
            ),
            dynamic_confidence: tier,
        })
    }

    /// Borrows one decorator spelling: the decorator range minus its `@`.
    fn decorator_spelling(&self, span: Span) -> Result<&'source [u8], PythonCollectError> {
        let bytes = self.slice(span)?;
        match bytes.first() {
            Some(b'@') => bytes.get(1..).ok_or(PythonCollectError::Span {
                start: span.start,
                end: span.end,
            }),
            _ => Ok(bytes),
        }
    }

    /// Lowers one extractor annotation into its lattice record.
    ///
    /// The mapping is the frozen census table: leaf built-ins become
    /// primitive rows, `Any` becomes the honest dynamic reason, module
    /// classes become backward nominal rows, `TypeVar` uses keep their name,
    /// and unions become union rows over their member rows. Compound
    /// applications (`list[int]`, `dict[str, int]`, tuples, `Callable`,
    /// `Optional`, generic classes) express through the anonymous type-row
    /// pool the lane now hosts; only a construct the pool cannot host — or a
    /// pool overflow — keeps `Unknown(NoIrRepresentation)` with its exact
    /// written spelling, so the backlog stays countable.
    fn lower_annotation(
        &mut self,
        annotation: &Annotation,
        spelling: Option<Span>,
        tables: &TypeTables<'source>,
    ) -> Result<LoweredType<'source>, PythonCollectError> {
        match annotation {
            Annotation::Name { name, span } => self.lower_name(name, *span, tables),
            Annotation::None => Ok(LoweredType {
                record: none_record(),
                children: Vec::new(),
                resolved: true,
            }),
            Annotation::StringLiteral(_) => {
                // The extractor no longer emits bare string facts (unparseable
                // quoted annotations carry their span instead); this arm stays
                // total for the public enum and reports the honest gap.
                Ok(LoweredType {
                    record: unknown_record(TypeReason::OracleGap),
                    children: Vec::new(),
                    resolved: false,
                })
            }
            Annotation::Literal(values) => self.lower_literal(values, spelling),
            Annotation::List(_) => {
                // A bare display outside a `Callable` parameter list has no
                // lane slot; it keeps its written spelling as unrepresentable.
                self.unrepresentable(spelling)
            }
            Annotation::Unknown(unknown) => Ok(match unknown {
                ExtractedReason::Unannotated { .. } => LoweredType {
                    record: unknown_record(TypeReason::Unannotated),
                    children: Vec::new(),
                    resolved: false,
                },
                ExtractedReason::UnsupportedSyntax { span, .. } => LoweredType {
                    record: spelled_unknown(TypeReason::NoIrRepresentation, self.slice(*span)?),
                    children: Vec::new(),
                    resolved: false,
                },
                ExtractedReason::TruncatedAtDepthLimit => LoweredType {
                    record: unknown_record(TypeReason::TruncatedAtDepthLimit),
                    children: Vec::new(),
                    resolved: false,
                },
            }),
            Annotation::Union(members) => self.lower_union(members, spelling, tables),
            Annotation::Generic { base, args } => {
                let is_union_base = matches!(base.as_ref(), Annotation::Name { name, .. } if name == "Union" || name == "typing.Union");
                if is_union_base {
                    return self.lower_union(args, spelling, tables);
                }
                match self.compound_root(annotation, spelling, tables)? {
                    Some((record, children)) => Ok(LoweredType {
                        record,
                        children,
                        resolved: true,
                    }),
                    None => self.unrepresentable(spelling),
                }
            }
        }
    }

    /// Lowers one written compound application to its root record and the
    /// ordered row coordinates of its children. The root record sits
    /// directly on the owning fact; only nested compounds consume pooled
    /// anonymous rows.
    fn compound_root(
        &mut self,
        annotation: &Annotation,
        spelling: Option<Span>,
        tables: &TypeTables<'source>,
    ) -> Result<Option<(SemanticTypeRecord<'source>, Vec<u32>)>, PythonCollectError> {
        let Some(anchor) = self.anchor() else {
            return Ok(None);
        };
        let Annotation::Generic { base, args } = annotation else {
            return Ok(None);
        };
        let base_name = match base.as_ref() {
            Annotation::Name { name, .. } => Some(name.as_str()),
            _ => None,
        };
        let is_class_base = base_name.is_some_and(|name| {
            tables
                .classes
                .iter()
                .any(|(known, _)| *known == name.as_bytes())
        });
        match base_name {
            Some("tuple") | Some("typing.Tuple") => {
                let Some(children) = self.member_rows(args, tables, anchor)? else {
                    return Ok(None);
                };
                let Some(children) =
                    self.admit_flat_children(tuple_record(), children, anchor)?
                else {
                    return Ok(None);
                };
                Ok(Some((tuple_record(), children)))
            }
            Some("Callable") | Some("typing.Callable") => {
                let Some(Annotation::List(parameters)) = args.first() else {
                    return Ok(None);
                };
                let mut children = Vec::new();
                for parameter in parameters {
                    match self.type_row(parameter, spelling, tables, anchor)? {
                        Some(row) => children.push(row),
                        None => return Ok(None),
                    }
                }
                let result_row = match args.get(1) {
                    Some(result) => match self.type_row(result, spelling, tables, anchor)? {
                        Some(row) => Some(row),
                        None => return Ok(None),
                    },
                    None => None,
                };
                let mut payload1 = 0;
                if let Some(result) = result_row {
                    children.push(result);
                    payload1 = SemanticTypeRecord::FUNCTION_RESULT_COUNT_ONE;
                }
                Ok(Some((function_pointer_record(payload1), children)))
            }
            Some("Optional") | Some("typing.Optional") => {
                let mut members = args.clone();
                members.push(Annotation::None);
                let children = self.row_children(&members, tables, anchor)?;
                Ok(children.map(|children| (union_record(), children)))
            }
            Some("list")
            | Some("set")
            | Some("frozenset")
            | Some("dict")
            | Some("typing.List")
            | Some("typing.Set")
            | Some("typing.FrozenSet")
            | Some("typing.Dict") => {
                let base_record = match base_name {
                    Some("list") | Some("typing.List") => builtin_record(b"list"),
                    Some("set") | Some("typing.Set") => builtin_record(b"set"),
                    Some("frozenset") | Some("typing.FrozenSet") => builtin_record(b"frozenset"),
                    _ => builtin_record(b"dict"),
                };
                let argument_rows = self.row_children(args, tables, anchor)?;
                let Some(argument_rows) = argument_rows else {
                    return Ok(None);
                };
                let Some(base_row) = self.leaf_row(base_record, anchor)? else {
                    return Ok(None);
                };
                let mut children = vec![base_row];
                children.extend(argument_rows);
                Ok(Some((apply_record(), children)))
            }
            _ if is_class_base => {
                let Some((_, base_ordinal)) = base_name.and_then(|name| {
                    tables
                        .classes
                        .iter()
                        .find(|(known, _)| *known == name.as_bytes())
                        .copied()
                }) else {
                    return Ok(None);
                };
                let argument_rows = self.row_children(args, tables, anchor)?;
                let Some(argument_rows) = argument_rows else {
                    return Ok(None);
                };
                let mut children = vec![base_ordinal];
                children.extend(argument_rows);
                Ok(Some((apply_record(), children)))
            }
            _ => Ok(None),
        }
    }

    /// Lowers every member of one written compound to its row coordinate.
    /// A run wider than one type-child row stays `None`. Tuple and union
    /// callers use [`Self::member_rows`] and fold the same tag instead.
    fn row_children(
        &mut self,
        members: &[Annotation],
        tables: &TypeTables<'source>,
        anchor: u32,
    ) -> Result<Option<Vec<u32>>, PythonCollectError> {
        if members.len() > MAX_TYPE_CHILDREN {
            return Ok(None);
        }
        self.member_rows(members, tables, anchor)
    }

    /// Lowers every member without the per-row width gate. The caller folds
    /// a tuple or union, or rejects a tag that cannot be nested honestly.
    fn member_rows(
        &mut self,
        members: &[Annotation],
        tables: &TypeTables<'source>,
        anchor: u32,
    ) -> Result<Option<Vec<u32>>, PythonCollectError> {
        let mut rows = Vec::with_capacity(members.len());
        for member in members {
            match self.type_row(member, None, tables, anchor)? {
                Some(row) => rows.push(row),
                None => return Ok(None),
            }
        }
        Ok(Some(rows))
    }

    /// Lowers one annotation to a pooled row coordinate: a module class is
    /// its already-pushed fact ordinal, every leaf or nested compound is an
    /// interned anonymous row, and an inexpressible member stays `None`.
    fn type_row(
        &mut self,
        annotation: &Annotation,
        spelling: Option<Span>,
        tables: &TypeTables<'source>,
        anchor: u32,
    ) -> Result<Option<u32>, PythonCollectError> {
        match annotation {
            Annotation::Name { name, span } => {
                if let Some((_, ordinal)) = tables
                    .classes
                    .iter()
                    .find(|(known, _)| *known == name.as_bytes())
                {
                    return Ok(Some(*ordinal));
                }
                if tables.typevars.contains(&name.as_bytes()) {
                    let text = self.spelling_bytes(*span)?;
                    let record = if text.is_empty() {
                        unknown_record(TypeReason::OracleGap)
                    } else {
                        typevar_record(text)
                    };
                    return self.leaf_row(record, anchor);
                }
                match name.as_str() {
                    "int" => self.leaf_row(integer_record(), anchor),
                    "float" => self.leaf_row(float64_record(), anchor),
                    "str" => self.leaf_row(primitive_record(PrimitiveShape::Str, 0), anchor),
                    "bool" => self.leaf_row(primitive_record(PrimitiveShape::Bool, 0), anchor),
                    "bytes" => self.leaf_row(builtin_record(b"bytes"), anchor),
                    "None" => self.leaf_row(none_record(), anchor),
                    _ => Ok(None),
                }
            }
            Annotation::None => self.leaf_row(none_record(), anchor),
            Annotation::List(_) => Ok(None),
            Annotation::Union(members) => {
                let children = self.row_children(members, tables, anchor)?;
                match children {
                    Some(children) => self.parent_row(union_record(), &children, anchor),
                    None => Ok(None),
                }
            }
            Annotation::Literal(values) => {
                let mut widened: Vec<SemanticTypeRecord<'source>> = Vec::new();
                for value in values {
                    let Some(record) = widened_literal_record(value) else {
                        return Ok(None);
                    };
                    if !widened.contains(&record) {
                        widened.push(record);
                    }
                }
                match widened.as_slice() {
                    [single] => self.leaf_row(*single, anchor),
                    [] => Ok(None),
                    many => {
                        let mut children = Vec::new();
                        for record in many {
                            match self.leaf_row(*record, anchor)? {
                                Some(row) => children.push(row),
                                None => return Ok(None),
                            }
                        }
                        let Some(children) =
                            self.admit_flat_children(union_record(), children, anchor)?
                        else {
                            return Ok(None);
                        };
                        self.parent_row(union_record(), &children, anchor)
                    }
                }
            }
            Annotation::Generic { .. } => match self.compound_root(annotation, spelling, tables)? {
                Some((record, children)) => self.parent_row(record, &children, anchor),
                None => Ok(None),
            },
            Annotation::StringLiteral(_) | Annotation::Unknown(_) => Ok(None),
        }
    }

    /// Keeps every member of a tuple or union inside the type-child lane.
    ///
    /// A run that already fits is returned unchanged. A wider run becomes a
    /// tree of anonymous rows of the same tag, each at most
    /// [`MAX_TYPE_CHILDREN`] wide, and the returned coordinates are those
    /// chunk rows. Flattening same-tag nesting recovers the member order.
    /// A callable is not folded: nesting function pointers would claim a
    /// different type. Pool overflow stays `None`, the same fallback a
    /// single unhostable row already uses.
    fn admit_flat_children(
        &mut self,
        record: SemanticTypeRecord<'source>,
        mut children: Vec<u32>,
        anchor: u32,
    ) -> Result<Option<Vec<u32>>, PythonCollectError> {
        let folds = record.tag == SemanticTypeTag::Tuple || record.tag == SemanticTypeTag::Union;
        if !folds {
            if children.len() > MAX_TYPE_CHILDREN {
                return Ok(None);
            }
            return Ok(Some(children));
        }
        while children.len() > MAX_TYPE_CHILDREN {
            let mut folded = Vec::new();
            let mut start = 0;
            while start < children.len() {
                let end = start.saturating_add(MAX_TYPE_CHILDREN).min(children.len());
                let Some(chunk) = children.get(start..end) else {
                    return Ok(None);
                };
                match self.parent_row(record, chunk, anchor)? {
                    Some(row) => folded.push(row),
                    None => return Ok(None),
                }
                start = end;
            }
            children = folded;
        }
        Ok(Some(children))
    }

    /// Appends already-lowered children and interns one parent row.
    fn parent_row(
        &mut self,
        record: SemanticTypeRecord<'source>,
        children: &[u32],
        anchor: u32,
    ) -> Result<Option<u32>, PythonCollectError> {
        for child in children {
            let admitted = self.facts.anonymous_type_child(*child, None, 0).is_ok();
            if !admitted {
                return Ok(None);
            }
        }
        self.intern_row(record, anchor)
    }

    /// Interns one anonymous leaf row with no children.
    fn leaf_row(
        &mut self,
        record: SemanticTypeRecord<'source>,
        anchor: u32,
    ) -> Result<Option<u32>, PythonCollectError> {
        self.intern_row(record, anchor)
    }

    /// Interns one anonymous row owned by the fact about to be pushed.
    /// A pooled-lane overflow falls back to `None`, never a lane rejection.
    fn intern_row(
        &mut self,
        record: SemanticTypeRecord<'source>,
        anchor: u32,
    ) -> Result<Option<u32>, PythonCollectError> {
        let row = match self.reserved_anchor {
            Some(owner) => self.facts.intern_reserved_anchor_type_row(owner, record),
            None => self.facts.intern_anonymous_type_row(anchor, record),
        };
        Ok(row.ok())
    }

    /// The anchor fact for rows emitted after the first declaration.
    fn anchor(&self) -> Option<u32> {
        self.pushed.first().map(|row| row.ordinal)
    }

    /// The honest cell for a known compound the lane cannot host: the
    /// construct keeps its exact written spelling so the backlog stays
    /// countable.
    fn unrepresentable(
        &self,
        spelling: Option<Span>,
    ) -> Result<LoweredType<'source>, PythonCollectError> {
        Ok(LoweredType {
            record: spelled_unknown(
                TypeReason::NoIrRepresentation,
                self.spelling_bytes(spelling)?,
            ),
            children: Vec::new(),
            resolved: false,
        })
    }

    /// Lowers one written `Literal[...]` annotation by widening every
    /// literal member to its base-primitive row — the same frozen widening
    /// law the type authority applies to revealed `Literal[...]`
    /// refinements. One distinct widened member sits directly on the fact;
    /// several become a union over the member rows; a literal the widening
    /// law cannot name (ellipsis, unsupported syntax) keeps its exact
    /// written spelling as the countable gap.
    fn lower_literal(
        &mut self,
        values: &[LiteralValue],
        spelling: Option<Span>,
    ) -> Result<LoweredType<'source>, PythonCollectError> {
        let mut widened: Vec<SemanticTypeRecord<'source>> = Vec::new();
        for value in values {
            let Some(record) = widened_literal_record(value) else {
                return self.unrepresentable(spelling);
            };
            if !widened.contains(&record) {
                widened.push(record);
            }
        }
        let Some(first) = widened.first() else {
            return self.unrepresentable(spelling);
        };
        if widened.len() == 1 {
            return Ok(LoweredType {
                record: *first,
                children: Vec::new(),
                resolved: true,
            });
        }
        let Some(anchor) = self.anchor() else {
            return self.unrepresentable(spelling);
        };
        let mut children = Vec::new();
        for record in &widened {
            match self.leaf_row(*record, anchor)? {
                Some(row) => children.push(row),
                None => return self.unrepresentable(spelling),
            }
        }
        let Some(children) = self.admit_flat_children(union_record(), children, anchor)? else {
            return self.unrepresentable(spelling);
        };
        Ok(LoweredType {
            record: union_record(),
            children,
            resolved: true,
        })
    }

    /// Lowers one written name annotation through the closed resolution
    /// order: built-ins, dynamic `Any`, `TypeVar` bindings, module classes,
    /// imported bindings, other module names, and finally the exact unknown.
    fn lower_name(
        &self,
        name: &str,
        spelling: Option<Span>,
        tables: &TypeTables<'source>,
    ) -> Result<LoweredType<'source>, PythonCollectError> {
        let leaf = |record, resolved| {
            Ok(LoweredType {
                record,
                children: Vec::new(),
                resolved,
            })
        };
        match name {
            "int" => return leaf(integer_record(), true),
            "float" => return leaf(float64_record(), true),
            "str" => return leaf(primitive_record(PrimitiveShape::Str, 0), true),
            "bool" => return leaf(primitive_record(PrimitiveShape::Bool, 0), true),
            "bytes" => return leaf(builtin_record(b"bytes"), true),
            "None" => return leaf(none_record(), true),
            "Any" | "typing.Any" => {
                return leaf(unknown_record(TypeReason::DynamicallyTyped), true);
            }
            _ => {}
        }
        if tables.typevars.contains(&name.as_bytes()) {
            // The written annotation spelling is the exact `TypeVar` text;
            // the extractor's owned name string can never enter the lane.
            let text = self.spelling_bytes(spelling)?;
            let record = if text.is_empty() {
                unknown_record(TypeReason::OracleGap)
            } else {
                SemanticTypeRecord {
                    tag: SemanticTypeTag::TypeVar,
                    payload0: 0,
                    payload1: 0,
                    text: Some(text),
                    text2: None,
                    nominal: None,
                    children: ListSpan::new(0, 0),
                }
            };
            return leaf(record, true);
        }
        if let Some((_, ordinal)) = tables
            .classes
            .iter()
            .find(|(known, _)| *known == name.as_bytes())
        {
            let record = SemanticTypeRecord {
                tag: SemanticTypeTag::Nominal,
                payload0: 0,
                payload1: 0,
                text: None,
                text2: None,
                nominal: Some(NominalRef::Local(EntityId::new(*ordinal))),
                children: ListSpan::new(0, 0),
            };
            return leaf(record, true);
        }
        if let Some(ordinal) = tables.bound_nominal(name) {
            let record = SemanticTypeRecord {
                tag: SemanticTypeTag::Nominal,
                payload0: 0,
                payload1: 0,
                text: None,
                text2: None,
                nominal: Some(NominalRef::Local(EntityId::new(ordinal))),
                children: ListSpan::new(0, 0),
            };
            return leaf(record, true);
        }
        let reason = match tables.binding_state(name) {
            Some(BindingResolution::External) => TypeReason::UnresolvedExternal,
            Some(BindingResolution::Ambiguous) => TypeReason::UnresolvedLocalName,
            Some(BindingResolution::Nominal(_)) => TypeReason::UnresolvedLocalName,
            None if self.is_import_binding(name) => TypeReason::UnresolvedExternal,
            None => TypeReason::UnresolvedLocalName,
        };
        Ok(LoweredType {
            record: spelled_unknown(reason, self.spelling_bytes(spelling)?),
            children: Vec::new(),
            resolved: false,
        })
    }

    fn is_import_binding(&self, name: &str) -> bool {
        self.module.declarations.iter().enumerate().any(|(index, declaration)| {
            self.live[index]
                && declaration.kind == DeclarationKind::Alias
                && declaration.value_span.is_some()
                && declaration.name == name
        })
    }

    /// Lowers a union: expressible exactly when every member lowers to a
    /// row coordinate (module class rows, leaf rows, or nested compound
    /// rows), because union children are strictly backward row coordinates.
    /// Any other member keeps the exact spelling as unrepresentable.
    fn lower_union(
        &mut self,
        members: &[Annotation],
        spelling: Option<Span>,
        tables: &TypeTables<'source>,
    ) -> Result<LoweredType<'source>, PythonCollectError> {
        let Some(anchor) = self.anchor() else {
            return self.unrepresentable(spelling);
        };
        let Some(children) = self.member_rows(members, tables, anchor)? else {
            return self.unrepresentable(spelling);
        };
        let Some(children) = self.admit_flat_children(union_record(), children, anchor)? else {
            return self.unrepresentable(spelling);
        };
        Ok(LoweredType {
            record: union_record(),
            children,
            resolved: true,
        })
    }

    /// The exact written annotation bytes, or the empty cell when the
    /// annotation carried no span (a position with no annotation at all).
    fn spelling_bytes(&self, spelling: Option<Span>) -> Result<&'source [u8], PythonCollectError> {
        match spelling {
            Some(span) => self.slice(span),
            None => Ok(&[]),
        }
    }

    /// Streams every extractor occurrence row into the lane: the owner is
    /// the innermost already-pushed declaration whose span precedes the
    /// reference, and the target resolves through the module's own names.
    /// The type authority upgrades the evidence tier: a call site the oracle
    /// bound to a resolved import becomes `Import`, and a call site the
    /// oracle bound to a local declaration becomes `Oracle`. Every
    /// unresolved site keeps its index-tier fact unchanged.
    fn emit_occurrences(&mut self) -> Result<(), PythonCollectError> {
        let rows = self.pushed.clone();
        for occurrence in &self.module.occurrences {
            let Some(owner) = owner_row(&rows, occurrence) else {
                // The module itself is not a lane fact (the driver fact set
                // carries declarations only), so module-owned references
                // have no honest owner row and are not synthesized one.
                continue;
            };
            let Some((target, confidence)) = self.occurrence_target(&rows, occurrence)? else {
                continue;
            };
            let lane_occurrence = Occurrence {
                target,
                kind: reference_kind(occurrence.kind),
                confidence,
                span: owner_relative_span(owner, occurrence),
            };
            self.facts
                .push_occurrence(owner.ordinal, lane_occurrence)
                .map_err(lane_rejected)?;
        }
        Ok(())
    }

    /// Resolves one occurrence target with its evidence tier. Bare-name and
    /// module-gated rows resolve through the module's own names: the
    /// earliest pushed row with the same name. Import bindings become
    /// foreign `pypi` package keys, since the referenced declaration lives
    /// outside this fragment; the checker's resolution decides the tier.
    /// Widened attribute rows key by receiver: a `self`/`cls` call resolves
    /// to the method its enclosing class declares, and every other receiver
    /// stays honestly foreign (an imported module receiver still resolves
    /// through its own package key) — never a fabricated local.
    fn occurrence_target(
        &self,
        rows: &[Pushed<'source>],
        occurrence: &OccurrenceFact,
    ) -> Result<Option<(OccurrenceTarget<'source>, OccurrenceConfidence)>, PythonCollectError> {
        let checked = self
            .checker
            .as_ref()
            .and_then(|report| report.symbol_at(occurrence.span));
        match &occurrence.receiver {
            OccurrenceReceiver::None | OccurrenceReceiver::Module => {
                if let Some(class_name) = self.class_name_from_qualified_span(occurrence)? {
                    if let Some(resolved) =
                        self.class_qualified_call_target(occurrence, class_name, checked)?
                    {
                        return Ok(Some(resolved));
                    }
                }
                let matched = rows
                    .iter()
                    .find(|row| row.name == occurrence.target.as_bytes());
                let Some(row) = matched else {
                    // The extractor only records targets matched against module
                    // names; an unmatched row keeps its written target spelling:
                    // a bare call's own name, or — for a module-gated attribute
                    // call — the attribute token at the callee's tail, because
                    // the receiver is a namespace qualifier the target key never
                    // carries (`pp.Word` keys `Word`, exactly like the matched
                    // import path below).
                    let written = self.slice(target_spelling_span(occurrence))?;
                    return Ok(Some((
                        foreign_universe(written, occurrence.span)?,
                        OccurrenceConfidence::Index,
                    )));
                };
                if let Some(imported) = row.imported {
                    let module_spelling = self.slice(imported)?;
                    let binding = core::str::from_utf8(row.name).map_err(|_| {
                        PythonCollectError::Projection(PythonProjectionFault::ForeignSpellingUtf8 {
                            start: occurrence.span.start,
                            end: occurrence.span.end,
                        })
                    })?;
                    let target =
                        foreign_package(module_spelling, binding, imported, None)?;
                    let confidence = match checked {
                        Some(SymbolOutcome::Foreign { .. }) => OccurrenceConfidence::Import,
                        _ => OccurrenceConfidence::Index,
                    };
                    return Ok(Some((target, confidence)));
                }
                let confidence = match checked {
                    Some(SymbolOutcome::Local) => OccurrenceConfidence::Oracle,
                    _ => OccurrenceConfidence::Index,
                };
                Ok(Some((
                    OccurrenceTarget::Local(EntityId::new(row.ordinal)),
                    confidence,
                )))
            }
            OccurrenceReceiver::EnclosingClass { class } => {
                if occurrence.kind == OccurrenceKind::AttributeRead {
                    match self.enclosing_field(occurrence, class) {
                        Some(ordinal) => {
                            let confidence = match checked {
                                Some(SymbolOutcome::Local) => OccurrenceConfidence::Oracle,
                                _ => OccurrenceConfidence::Index,
                            };
                            Ok(Some((
                                OccurrenceTarget::Local(EntityId::new(ordinal)),
                                confidence,
                            )))
                        }
                        None => {
                            if let Some(class_index) = self.enclosing_class_index(occurrence, class)
                            {
                                let class_span = self.module.declarations[class_index].span;
                                if self.field_count_in_class(occurrence, class_span) == 0 {
                                    match self.inherited_member(
                                        class_index,
                                        occurrence,
                                        DeclarationKind::Field,
                                    ) {
                                        InheritedMemberLookup::Unique(ordinal) => {
                                            let confidence = match checked {
                                                Some(SymbolOutcome::Local) => {
                                                    OccurrenceConfidence::Oracle
                                                }
                                                _ => OccurrenceConfidence::Index,
                                            };
                                            return Ok(Some((
                                                OccurrenceTarget::Local(EntityId::new(ordinal)),
                                                confidence,
                                            )));
                                        }
                                        InheritedMemberLookup::Ambiguous => {}
                                        InheritedMemberLookup::Absent => {
                                            if let Some(ordinal) =
                                                self.enclosing_method(occurrence, class)
                                            {
                                                let confidence = match checked {
                                                    Some(SymbolOutcome::Local) => {
                                                        OccurrenceConfidence::Oracle
                                                    }
                                                    _ => OccurrenceConfidence::Index,
                                                };
                                                return Ok(Some((
                                                    OccurrenceTarget::Local(EntityId::new(
                                                        ordinal,
                                                    )),
                                                    confidence,
                                                )));
                                            }
                                            if let InheritedMemberLookup::Unique(ordinal) = self
                                                .inherited_member(
                                                    class_index,
                                                    occurrence,
                                                    DeclarationKind::Function,
                                                )
                                            {
                                                let confidence = match checked {
                                                    Some(SymbolOutcome::Local) => {
                                                        OccurrenceConfidence::Oracle
                                                    }
                                                    _ => OccurrenceConfidence::Index,
                                                };
                                                return Ok(Some((
                                                    OccurrenceTarget::Local(EntityId::new(
                                                        ordinal,
                                                    )),
                                                    confidence,
                                                )));
                                            }
                                        }
                                    }
                                }
                            }
                            Ok(Some((
                                foreign_field(self.slice(occurrence.span)?, occurrence.span)?,
                                OccurrenceConfidence::Index,
                            )))
                        }
                    }
                } else {
                    match self.enclosing_method(occurrence, class) {
                        Some(ordinal) => {
                            let confidence = match checked {
                                Some(SymbolOutcome::Local) => OccurrenceConfidence::Oracle,
                                _ => OccurrenceConfidence::Index,
                            };
                            Ok(Some((
                                OccurrenceTarget::Local(EntityId::new(ordinal)),
                                confidence,
                            )))
                        }
                        None => {
                            if let Some(class_index) = self.enclosing_class_index(occurrence, class)
                                && let InheritedMemberLookup::Unique(ordinal) = self
                                    .inherited_member(
                                        class_index,
                                        occurrence,
                                        DeclarationKind::Function,
                                    )
                            {
                                let confidence = match checked {
                                    Some(SymbolOutcome::Local) => OccurrenceConfidence::Oracle,
                                    _ => OccurrenceConfidence::Index,
                                };
                                return Ok(Some((
                                    OccurrenceTarget::Local(EntityId::new(ordinal)),
                                    confidence,
                                )));
                            }
                            // No live method on the class and no unique same-file
                            // base member: an honest typed foreign method key.
                            Ok(Some((
                                foreign_method(self.slice(occurrence.span)?, occurrence.span)?,
                                OccurrenceConfidence::Index,
                            )))
                        }
                    }
                }
            }
            OccurrenceReceiver::Constructed { class } => match occurrence.kind {
                OccurrenceKind::MethodCall | OccurrenceKind::FunctionCall => {
                    if let Some(resolved) =
                        self.class_qualified_call_target(occurrence, class, checked)?
                    {
                        Ok(Some(resolved))
                    } else {
                        Ok(Some((
                            foreign_method(self.slice(occurrence.span)?, occurrence.span)?,
                            OccurrenceConfidence::Index,
                        )))
                    }
                }
                OccurrenceKind::AttributeRead => {
                    if let Some(resolved) =
                        self.class_qualified_read_target(occurrence, class, checked)?
                    {
                        Ok(Some(resolved))
                    } else if let Some(ordinal) = self.module_field(occurrence) {
                        let confidence = match checked {
                            Some(SymbolOutcome::Local) => OccurrenceConfidence::Oracle,
                            _ => OccurrenceConfidence::Index,
                        };
                        Ok(Some((
                            OccurrenceTarget::Local(EntityId::new(ordinal)),
                            confidence,
                        )))
                    } else {
                        Ok(Some((
                            foreign_field(self.slice(occurrence.span)?, occurrence.span)?,
                            OccurrenceConfidence::Index,
                        )))
                    }
                }
            }
            OccurrenceReceiver::InstanceAttribute { class, attribute } => {
                self.instance_or_named_attribute_target(
                    occurrence,
                    checked,
                    match self.enclosing_class_index(occurrence, class) {
                        Some(class_index) => self
                            .field_annotation_class_name_for_class(class_index, attribute),
                        None => None,
                    },
                )
            }
            OccurrenceReceiver::NamedAttribute { name, attribute } => {
                self.instance_or_named_attribute_target(
                    occurrence,
                    checked,
                    match self.named_attribute_class_index(occurrence, name) {
                        Some(class_index) => self
                            .field_annotation_class_name_for_class(class_index, attribute),
                        None => None,
                    },
                )
            }
            OccurrenceReceiver::ChainedAttribute { root, attributes } => {
                let mut class_index = match root {
                    AttributeChainRoot::Enclosing { class } => {
                        self.enclosing_class_index(occurrence, class)
                    }
                    AttributeChainRoot::Name { name } => {
                        self.named_attribute_class_index(occurrence, name)
                    }
                };
                if let Some(mut class_index) = class_index {
                    for attribute in attributes {
                        let Some(class_name) =
                            self.field_annotation_class_name_for_class(class_index, attribute)
                        else {
                            return self.instance_or_named_attribute_target(
                                occurrence,
                                checked,
                                None,
                            );
                        };
                        let Some(next_index) = self.unique_live_class_index(&class_name) else {
                            return self.instance_or_named_attribute_target(
                                occurrence,
                                checked,
                                None,
                            );
                        };
                        class_index = next_index;
                    }
                    let final_class_name = self.module.declarations[class_index].name.clone();
                    return self.instance_or_named_attribute_target(
                        occurrence,
                        checked,
                        Some(final_class_name),
                    );
                }
                self.instance_or_named_attribute_target(occurrence, checked, None)
            }
            OccurrenceReceiver::SubscriptedAttribute { root, steps } => self
                .instance_or_named_attribute_target(
                    occurrence,
                    checked,
                    self.subscripted_attribute_class_name(occurrence, root, steps),
                ),
            OccurrenceReceiver::SubscriptedCall { call, steps } => self
                .instance_or_named_attribute_target(
                    occurrence,
                    checked,
                    self.subscripted_call_class_name(occurrence, call.as_ref(), steps),
                ),
            OccurrenceReceiver::CallReturn { method, receiver } => {
                self.call_return_target(occurrence, checked, method, receiver.as_ref())
            }
            OccurrenceReceiver::Super { class, after } => {
                let confidence = |checked: Option<&SymbolOutcome>| match checked {
                    Some(SymbolOutcome::Local) => OccurrenceConfidence::Oracle,
                    _ => OccurrenceConfidence::Index,
                };
                let Some(class_index) = self.unique_live_class_index(class) else {
                    return Ok(Some((
                        self.super_foreign_target(occurrence)?,
                        OccurrenceConfidence::Index,
                    )));
                };
                let mut stack = HashSet::new();
                let Some(mro) = self.c3_mro(class_index, &mut stack) else {
                    return Ok(Some((
                        self.super_foreign_target(occurrence)?,
                        OccurrenceConfidence::Index,
                    )));
                };
                let start_after = match after {
                    None => 0,
                    Some(start) => {
                        let Some(start_index) = self.unique_live_class_index(start) else {
                            return Ok(Some((
                                self.super_foreign_target(occurrence)?,
                                OccurrenceConfidence::Index,
                            )));
                        };
                        match mro.iter().position(|&index| index == start_index) {
                            Some(index) => index,
                            None => {
                                return Ok(Some((
                                    self.super_foreign_target(occurrence)?,
                                    OccurrenceConfidence::Index,
                                )));
                            }
                        }
                    }
                };
                match occurrence.kind {
                    OccurrenceKind::MethodCall | OccurrenceKind::FunctionCall => {
                        match self.super_member_in_mro(
                            occurrence,
                            &mro,
                            start_after,
                            DeclarationKind::Function,
                        ) {
                            InheritedMemberLookup::Unique(ordinal) => Ok(Some((
                                OccurrenceTarget::Local(EntityId::new(ordinal)),
                                confidence(checked),
                            ))),
                            InheritedMemberLookup::Ambiguous | InheritedMemberLookup::Absent => {
                                Ok(Some((
                                    foreign_method(
                                        self.slice(occurrence.span)?,
                                        occurrence.span,
                                    )?,
                                    OccurrenceConfidence::Index,
                                )))
                            }
                        }
                    }
                    OccurrenceKind::AttributeRead => {
                        match self.super_member_in_mro(
                            occurrence,
                            &mro,
                            start_after,
                            DeclarationKind::Field,
                        ) {
                            InheritedMemberLookup::Unique(ordinal) => Ok(Some((
                                OccurrenceTarget::Local(EntityId::new(ordinal)),
                                confidence(checked),
                            ))),
                            InheritedMemberLookup::Ambiguous => Ok(Some((
                                foreign_field(self.slice(occurrence.span)?, occurrence.span)?,
                                OccurrenceConfidence::Index,
                            ))),
                            InheritedMemberLookup::Absent => {
                                match self.super_member_in_mro(
                                    occurrence,
                                    &mro,
                                    start_after,
                                    DeclarationKind::Function,
                                ) {
                                    InheritedMemberLookup::Unique(ordinal) => Ok(Some((
                                        OccurrenceTarget::Local(EntityId::new(ordinal)),
                                        confidence(checked),
                                    ))),
                                    InheritedMemberLookup::Ambiguous
                                    | InheritedMemberLookup::Absent => Ok(Some((
                                        foreign_field(
                                            self.slice(occurrence.span)?,
                                            occurrence.span,
                                        )?,
                                        OccurrenceConfidence::Index,
                                    ))),
                                }
                            }
                        }
                    }
                }
            }
            OccurrenceReceiver::Foreign { receiver } => {
                if occurrence.kind == OccurrenceKind::AttributeRead {
                    if let Some(receiver) = receiver {
                        if let Some(row) = rows
                            .iter()
                            .find(|row| row.name == receiver.as_bytes() && row.imported.is_some())
                        {
                            let imported =
                                row.imported.expect("imported span proven non-None above");
                            let module_spelling =
                                self.imported_module_spelling(receiver, imported)?;
                            let module_span = self
                                .spelling_span(core::str::from_utf8(module_spelling).map_err(
                                    |_| {
                                        PythonCollectError::Projection(
                                            PythonProjectionFault::ForeignSpellingUtf8 {
                                                start: imported.start,
                                                end: imported.end,
                                            },
                                        )
                                    },
                                )?)
                                .unwrap_or(imported);
                            let binding = self.slice(occurrence.span)?;
                            let binding = core::str::from_utf8(binding).map_err(|_| {
                                PythonCollectError::Projection(
                                    PythonProjectionFault::ForeignSpellingUtf8 {
                                        start: occurrence.span.start,
                                        end: occurrence.span.end,
                                    },
                                )
                            })?;
                            let target = foreign_package(
                                module_spelling,
                                binding,
                                module_span,
                                Some(EntityKind::Field),
                            )?;
                            let confidence = match checked {
                                Some(SymbolOutcome::Foreign { .. }) => {
                                    OccurrenceConfidence::Import
                                }
                                _ => OccurrenceConfidence::Index,
                            };
                            return Ok(Some((target, confidence)));
                        }
                    }
                    if let Some(receiver) = receiver {
                        if let Some(resolved) =
                            self.class_qualified_read_target(occurrence, receiver, checked)?
                        {
                            return Ok(Some(resolved));
                        }
                    }
                    if let Some(receiver) = receiver {
                        if let Some(resolved) =
                            self.annotated_receiver_read_target(occurrence, receiver, checked)?
                        {
                            return Ok(Some(resolved));
                        }
                    }
                    if let Some(ordinal) = self.module_field(occurrence) {
                        let confidence = match checked {
                            Some(SymbolOutcome::Local) => OccurrenceConfidence::Oracle,
                            _ => OccurrenceConfidence::Index,
                        };
                        return Ok(Some((
                            OccurrenceTarget::Local(EntityId::new(ordinal)),
                            confidence,
                        )));
                    }
                    return Ok(Some((
                        foreign_field(self.slice(occurrence.span)?, occurrence.span)?,
                        OccurrenceConfidence::Index,
                    )));
                }
                // A receiver that names an import binding resolves through
                // that binding's own package key; any other receiver stays
                // an honest typed foreign method key.
                if let Some(receiver) = receiver {
                    if let Some(row) = rows
                        .iter()
                        .find(|row| row.name == receiver.as_bytes() && row.imported.is_some())
                    {
                        let imported = row.imported.expect("imported span proven non-None above");
                        let module_spelling =
                            self.imported_module_spelling(receiver, imported)?;
                        let module_span = self
                            .spelling_span(core::str::from_utf8(module_spelling).map_err(
                                |_| {
                                    PythonCollectError::Projection(
                                        PythonProjectionFault::ForeignSpellingUtf8 {
                                            start: imported.start,
                                            end: imported.end,
                                        },
                                    )
                                },
                            )?)
                            .unwrap_or(imported);
                        let binding = self.slice(occurrence.span)?;
                        let binding = core::str::from_utf8(binding).map_err(|_| {
                            PythonCollectError::Projection(
                                PythonProjectionFault::ForeignSpellingUtf8 {
                                    start: occurrence.span.start,
                                    end: occurrence.span.end,
                                },
                            )
                        })?;
                        let target =
                            foreign_package(module_spelling, binding, module_span, None)?;
                        let confidence = match checked {
                            Some(SymbolOutcome::Foreign { .. }) => OccurrenceConfidence::Import,
                            _ => OccurrenceConfidence::Index,
                        };
                        return Ok(Some((target, confidence)));
                    }
                    if let Some(resolved) =
                        self.class_qualified_call_target(occurrence, receiver, checked)?
                    {
                        return Ok(Some(resolved));
                    }
                    if let Some(resolved) =
                        self.annotated_receiver_target(occurrence, receiver, checked)?
                    {
                        return Ok(Some(resolved));
                    }
                }
                Ok(Some((
                    foreign_method(self.slice(occurrence.span)?, occurrence.span)?,
                    OccurrenceConfidence::Index,
                )))
            }
        }
    }

    /// The lane ordinal of the live method one widened `self`/`cls` call
    /// resolves to: the innermost live function declaration with the
    /// attribute's spelling inside the innermost live class declaration
    /// with the recorded class's name whose extent contains the call site.
    /// The same live and innermost laws the parentage pass applies pick
    /// exactly one row; anything else has no honest local target.
    fn enclosing_method(&self, occurrence: &OccurrenceFact, class: &str) -> Option<u32> {
        let class_bytes = class.as_bytes();
        let attribute_bytes = occurrence.target.as_bytes();
        let mut class_span: Option<Span> = None;
        for (index, declaration) in self.module.declarations.iter().enumerate() {
            if declaration.kind != DeclarationKind::Class
                || declaration.name.as_bytes() != class_bytes
                || !self.live[index]
                || !span_contains(declaration.span, occurrence.span)
            {
                continue;
            }
            let area = declaration.span.end - declaration.span.start;
            let occupied = class_span.map_or(true, |span| area < span.end - span.start);
            if occupied {
                class_span = Some(declaration.span);
            }
        }
        let class_span = class_span?;
        let mut method: Option<(Span, u32)> = None;
        for (index, declaration) in self.module.declarations.iter().enumerate() {
            if declaration.kind != DeclarationKind::Function
                || declaration.name.as_bytes() != attribute_bytes
                || !self.live[index]
                || !span_contains(class_span, declaration.span)
            {
                continue;
            }
            let area = declaration.span.end - declaration.span.start;
            let occupied = method.map_or(true, |(span, _)| area < span.end - span.start);
            if occupied && let Some(ordinal) = self.ordinals[index] {
                method = Some((declaration.span, ordinal));
            }
        }
        method.map(|(_, ordinal)| ordinal)
    }

    fn enclosing_field(&self, occurrence: &OccurrenceFact, class: &str) -> Option<u32> {
        let class_bytes = class.as_bytes();
        let attribute_bytes = occurrence.target.as_bytes();
        let mut class_span: Option<Span> = None;
        for (index, declaration) in self.module.declarations.iter().enumerate() {
            if declaration.kind != DeclarationKind::Class
                || declaration.name.as_bytes() != class_bytes
                || !self.live[index]
                || !span_contains(declaration.span, occurrence.span)
            {
                continue;
            }
            let area = declaration.span.end - declaration.span.start;
            let occupied = class_span.map_or(true, |span| area < span.end - span.start);
            if occupied {
                class_span = Some(declaration.span);
            }
        }
        let class_span = class_span?;
        let mut matches: Vec<u32> = Vec::new();
        for (index, declaration) in self.module.declarations.iter().enumerate() {
            if declaration.kind != DeclarationKind::Field
                || declaration.name.as_bytes() != attribute_bytes
                || !self.live[index]
                || !span_contains(class_span, declaration.span)
            {
                continue;
            }
            if let Some(ordinal) = self.ordinals[index] {
                matches.push(ordinal);
            }
        }
        if matches.len() == 1 {
            Some(matches[0])
        } else {
            None
        }
    }

    /// The borrowed module spelling of one import binding when a `from … import
    /// …` row carries a dotted module in `value_source`; otherwise the
    /// binding's own `value_span` spelling.
    fn imported_module_spelling(
        &self,
        receiver: &str,
        imported: Span,
    ) -> Result<&'source [u8], PythonCollectError> {
        if let Some(declaration) = self
            .module
            .declarations
            .iter()
            .find(|declaration| declaration.name == receiver)
        {
            if let Some(module) = declaration
                .value_source
                .as_deref()
                .filter(|module| *module != receiver)
            {
                if let Some(span) = self.spelling_span(module) {
                    return self.slice(span);
                }
            }
        }
        self.slice(imported)
    }

    /// The first exact source span of one written spelling, when it appears in
    /// the module bytes.
    fn spelling_span(&self, spelling: &str) -> Option<Span> {
        let spelling = spelling.as_bytes();
        self.source
            .windows(spelling.len())
            .position(|window| window == spelling)
            .and_then(|start| {
                let end = start + spelling.len();
                let start = u32::try_from(start).ok()?;
                let end = u32::try_from(end).ok()?;
                Some(Span { start, end })
            })
    }

    /// The simple class name on the left of one receiver-qualified occurrence
    /// span when the right side equals the attribute target and the left is a
    /// single identifier.
    fn class_name_from_qualified_span(
        &self,
        occurrence: &OccurrenceFact,
    ) -> Result<Option<&str>, PythonCollectError> {
        let bytes = self.slice(occurrence.span)?;
        let text = core::str::from_utf8(bytes).map_err(|_| {
            PythonCollectError::Projection(PythonProjectionFault::ForeignSpellingUtf8 {
                start: occurrence.span.start,
                end: occurrence.span.end,
            })
        })?;
        if !text.contains('.') {
            return Ok(None);
        }
        let (left, right) = match text.rsplit_once('.') {
            Some(parts) => parts,
            None => return Ok(None),
        };
        let left = left.trim();
        let right = right.trim();
        if right != occurrence.target.as_str() || left.is_empty() || left.contains('.') {
            return Ok(None);
        }
        if !simple_identifier(left) {
            return Ok(None);
        }
        Ok(Some(left))
    }

    /// Live class declaration indices sharing one exact name. Import aliases are
    /// ignored.
    fn live_plain_class_indices(&self, name: &str) -> Vec<usize> {
        let name_bytes = name.as_bytes();
        self.module
            .declarations
            .iter()
            .enumerate()
            .filter(|(index, declaration)| {
                self.live[*index]
                    && declaration.kind == DeclarationKind::Class
                    && declaration.name.as_bytes() == name_bytes
            })
            .map(|(index, _)| index)
            .collect()
    }

    /// Resolves one class-qualified call through a live class receiver name.
    /// Zero live classes fall through; every other class-count case returns a
    /// target and never reaches module-name or `module_field` lookup.
    fn class_qualified_call_target(
        &self,
        occurrence: &OccurrenceFact,
        class_name: &str,
        checked: Option<&SymbolOutcome>,
    ) -> Result<Option<(OccurrenceTarget<'source>, OccurrenceConfidence)>, PythonCollectError> {
        let foreign = || {
            let span = attribute_token_span(self.slice(occurrence.span)?, occurrence)?;
            foreign_method(self.slice(span)?, span)
                .map(|target| (target, OccurrenceConfidence::Index))
        };
        let candidates = self.live_plain_class_indices(class_name);
        if candidates.is_empty() {
            return Ok(None);
        }
        if candidates.len() >= 2 {
            return foreign().map(Some);
        }
        let index = candidates[0];
        let declaration = &self.module.declarations[index];
        let local_confidence = || match checked {
            Some(SymbolOutcome::Local) => OccurrenceConfidence::Oracle,
            _ => OccurrenceConfidence::Index,
        };
        if declaration.kind != DeclarationKind::Class {
            return Ok(None);
        }
        if let Some(ordinal) = self.method_in_class(occurrence, declaration.span) {
            return Ok(Some((
                OccurrenceTarget::Local(EntityId::new(ordinal)),
                local_confidence(),
            )));
        }
        if self.method_count_in_class(occurrence, declaration.span) > 1 {
            return foreign().map(Some);
        }
        if let InheritedMemberLookup::Unique(ordinal) =
            self.inherited_member(index, occurrence, DeclarationKind::Function)
        {
            return Ok(Some((
                OccurrenceTarget::Local(EntityId::new(ordinal)),
                local_confidence(),
            )));
        }
        foreign().map(Some)
    }

    /// Resolves one class-qualified attribute read through a live class receiver
    /// name with the same field-before-method laws as one annotated class.
    fn class_qualified_read_target(
        &self,
        occurrence: &OccurrenceFact,
        class_name: &str,
        checked: Option<&SymbolOutcome>,
    ) -> Result<Option<(OccurrenceTarget<'source>, OccurrenceConfidence)>, PythonCollectError> {
        let foreign = || {
            foreign_field(self.slice(occurrence.span)?, occurrence.span)
                .map(|target| (target, OccurrenceConfidence::Index))
        };
        let candidates = self.live_plain_class_indices(class_name);
        if candidates.is_empty() {
            return Ok(None);
        }
        if candidates.len() >= 2 {
            return foreign().map(Some);
        }
        let index = candidates[0];
        let declaration = &self.module.declarations[index];
        let local_confidence = || match checked {
            Some(SymbolOutcome::Local) => OccurrenceConfidence::Oracle,
            _ => OccurrenceConfidence::Index,
        };
        if declaration.kind != DeclarationKind::Class {
            return Ok(None);
        }
        let class_span = declaration.span;
        let field_count = self.field_count_in_class(occurrence, class_span);
        if field_count > 1 {
            return foreign().map(Some);
        }
        if field_count == 1 {
            if let InheritedMemberLookup::Unique(ordinal) =
                self.member_lookup_in_class(occurrence, class_span, DeclarationKind::Field)
            {
                return Ok(Some((
                    OccurrenceTarget::Local(EntityId::new(ordinal)),
                    local_confidence(),
                )));
            }
            return foreign().map(Some);
        }
        match self.inherited_member(index, occurrence, DeclarationKind::Field) {
            InheritedMemberLookup::Unique(ordinal) => Ok(Some((
                OccurrenceTarget::Local(EntityId::new(ordinal)),
                local_confidence(),
            ))),
            InheritedMemberLookup::Ambiguous => foreign().map(Some),
            InheritedMemberLookup::Absent => {
                let method_count = self.method_count_in_class(occurrence, class_span);
                if method_count > 1 {
                    return foreign().map(Some);
                }
                if method_count == 1 {
                    if let Some(ordinal) = self.method_in_class(occurrence, class_span) {
                        return Ok(Some((
                            OccurrenceTarget::Local(EntityId::new(ordinal)),
                            local_confidence(),
                        )));
                    }
                    return foreign().map(Some);
                }
                if let InheritedMemberLookup::Unique(ordinal) =
                    self.inherited_member(index, occurrence, DeclarationKind::Function)
                {
                    return Ok(Some((
                        OccurrenceTarget::Local(EntityId::new(ordinal)),
                        local_confidence(),
                    )));
                }
                foreign().map(Some)
            }
        }
    }

    /// The lane ordinal of the live field one attribute read resolves to when
    /// exactly one live field in the module carries the attribute spelling.
    fn module_field(&self, occurrence: &OccurrenceFact) -> Option<u32> {
        let attribute_bytes = occurrence.target.as_bytes();
        let mut matches: Vec<u32> = Vec::new();
        for (index, declaration) in self.module.declarations.iter().enumerate() {
            if declaration.kind != DeclarationKind::Field
                || declaration.name.as_bytes() != attribute_bytes
                || !self.live[index]
            {
                continue;
            }
            if let Some(ordinal) = self.ordinals[index] {
                matches.push(ordinal);
            }
        }
        if matches.len() == 1 {
            Some(matches[0])
        } else {
            None
        }
    }

    /// Resolves one plain-name receiver through a function-local annotation,
    /// then its parameter annotation, then a live module-constant annotation,
    /// when the import-binding arm did not apply and the site is an attribute
    /// read. A unique live class yields the unique field or bound method inside
    /// that class (fields before methods, then inherited members); a unique
    /// live import alias yields the alias statement's package field key; a union
    /// of one class name peels to that class; an ambiguous union stays on an
    /// honest universe field key. Multiply-matched cases stay on an honest
    /// universe field key. A local or module annotation that names no live class
    /// returns that universe field key and does not fall through to
    /// `module_field`. A parameter annotation with zero candidates still falls
    /// through to `module_field`.
    fn annotated_receiver_read_target(
        &self,
        occurrence: &OccurrenceFact,
        receiver: &str,
        checked: Option<&SymbolOutcome>,
    ) -> Result<Option<(OccurrenceTarget<'source>, OccurrenceConfidence)>, PythonCollectError>
    {
        let function_index = match self.enclosing_function_index(occurrence) {
            Some(index) => index,
            None => return Ok(None),
        };
        let function = &self.module.declarations[function_index];
        let foreign = || {
            foreign_field(self.slice(occurrence.span)?, occurrence.span)
                .map(|target| (target, OccurrenceConfidence::Index))
        };
        let (type_name, from_local) = match self.local_binding_name(function, receiver, occurrence) {
            LocalBinding::Foreign => return foreign().map(Some),
            LocalBinding::Unique(name) => (name, true),
            LocalBinding::Absent => match receiver_annotation_name(function, receiver) {
                ReceiverAnnotationName::Absent => {
                    match self.module_annotation_name(function, receiver, occurrence) {
                        LocalBinding::Foreign => return foreign().map(Some),
                        LocalBinding::Unique(name) => (name, true),
                        LocalBinding::Absent => return Ok(None),
                    }
                }
                ReceiverAnnotationName::Ambiguous => return foreign().map(Some),
                ReceiverAnnotationName::Unique(name) => (name.to_owned(), false),
            },
        };
        if self.assigned_name_hides_annotation(occurrence, receiver) {
            return Ok(None);
        }
        let local_confidence = || match checked {
            Some(SymbolOutcome::Local) => OccurrenceConfidence::Oracle,
            _ => OccurrenceConfidence::Index,
        };
        let mut candidates = self.live_class_or_alias_indices(type_name.as_str());
        if candidates.is_empty() {
            if let Some(class_index) = self.unique_live_class_index(type_name.as_str()) {
                candidates.push(class_index);
            }
        }
        if candidates.is_empty() {
            // A local annotation named no live class. Do not fall through to
            // `module_field`; a parameter annotation with no candidate still does.
            if from_local {
                return foreign().map(Some);
            }
            return Ok(None);
        }
        if candidates.len() >= 2 {
            return foreign().map(Some);
        }
        let index = candidates[0];
        let declaration = &self.module.declarations[index];
        match declaration.kind {
            DeclarationKind::Class => {
                let class_span = declaration.span;
                let field_count = self.field_count_in_class(occurrence, class_span);
                if field_count > 1 {
                    return foreign().map(Some);
                }
                if field_count == 1 {
                    if let InheritedMemberLookup::Unique(ordinal) =
                        self.member_lookup_in_class(occurrence, class_span, DeclarationKind::Field)
                    {
                        return Ok(Some((
                            OccurrenceTarget::Local(EntityId::new(ordinal)),
                            local_confidence(),
                        )));
                    }
                    return foreign().map(Some);
                }
                match self.inherited_member(index, occurrence, DeclarationKind::Field) {
                    InheritedMemberLookup::Unique(ordinal) => Ok(Some((
                        OccurrenceTarget::Local(EntityId::new(ordinal)),
                        local_confidence(),
                    ))),
                    InheritedMemberLookup::Ambiguous => foreign().map(Some),
                    InheritedMemberLookup::Absent => {
                        let method_count = self.method_count_in_class(occurrence, class_span);
                        if method_count > 1 {
                            return foreign().map(Some);
                        }
                        if method_count == 1 {
                            if let Some(ordinal) =
                                self.method_in_class(occurrence, class_span)
                            {
                                return Ok(Some((
                                    OccurrenceTarget::Local(EntityId::new(ordinal)),
                                    local_confidence(),
                                )));
                            }
                            return foreign().map(Some);
                        }
                        if let InheritedMemberLookup::Unique(ordinal) =
                            self.inherited_member(index, occurrence, DeclarationKind::Function)
                        {
                            return Ok(Some((
                                OccurrenceTarget::Local(EntityId::new(ordinal)),
                                local_confidence(),
                            )));
                        }
                        foreign().map(Some)
                    }
                }
            }
            DeclarationKind::Alias => {
                let module_span = match alias_import_module_span(self.source, declaration.span) {
                    Some(span) => span,
                    None => {
                        if from_local {
                            return foreign().map(Some);
                        }
                        return Ok(None);
                    }
                };
                let module_spelling = self.slice(module_span)?;
                let binding = self.slice(occurrence.span)?;
                let binding = core::str::from_utf8(binding).map_err(|_| {
                    PythonCollectError::Projection(PythonProjectionFault::ForeignSpellingUtf8 {
                        start: occurrence.span.start,
                        end: occurrence.span.end,
                    })
                })?;
                let target = foreign_package(
                    module_spelling,
                    binding,
                    module_span,
                    Some(EntityKind::Field),
                )?;
                let confidence = match checked {
                    Some(SymbolOutcome::Foreign { .. }) => OccurrenceConfidence::Import,
                    _ => OccurrenceConfidence::Index,
                };
                Ok(Some((target, confidence)))
            }
            _ => {
                if from_local {
                    return foreign().map(Some);
                }
                Ok(None)
            }
        }
    }

    /// Resolves one plain-name receiver through a function-local annotation,
    /// then its parameter annotation, then a live module-constant annotation,
    /// when the import-binding arm did not apply. A unique live class yields the
    /// unique method inside that class; a unique live import alias yields the
    /// alias statement's package key; a union of one class name peels to that
    /// class; an ambiguous union stays on an honest universe method key. A local
    /// or module annotation that must not bind returns that universe key. Every
    /// other unproven case keeps today's universe key by returning `None`.
    fn annotated_receiver_target(
        &self,
        occurrence: &OccurrenceFact,
        receiver: &str,
        checked: Option<&SymbolOutcome>,
    ) -> Result<Option<(OccurrenceTarget<'source>, OccurrenceConfidence)>, PythonCollectError> {
        let function_index = match self.enclosing_function_index(occurrence) {
            Some(index) => index,
            None => return Ok(None),
        };
        let function = &self.module.declarations[function_index];
        let foreign = || {
            foreign_method(self.slice(occurrence.span)?, occurrence.span)
                .map(|target| (target, OccurrenceConfidence::Index))
        };
        let (type_name, from_local) = match self.local_binding_name(function, receiver, occurrence) {
            LocalBinding::Foreign => return foreign().map(Some),
            LocalBinding::Unique(name) => (name, true),
            LocalBinding::Absent => match receiver_annotation_name(function, receiver) {
                ReceiverAnnotationName::Absent => {
                    match self.module_annotation_name(function, receiver, occurrence) {
                        LocalBinding::Foreign => return foreign().map(Some),
                        LocalBinding::Unique(name) => (name, true),
                        LocalBinding::Absent => return Ok(None),
                    }
                }
                ReceiverAnnotationName::Ambiguous => return foreign().map(Some),
                ReceiverAnnotationName::Unique(name) => (name.to_owned(), false),
            },
        };
        if self.assigned_name_hides_annotation(occurrence, receiver) {
            return Ok(None);
        }
        let mut candidates = self.live_class_or_alias_indices(type_name.as_str());
        if candidates.is_empty() {
            if let Some(class_index) = self.unique_live_class_index(type_name.as_str()) {
                candidates.push(class_index);
            }
        }
        if candidates.len() != 1 {
            if from_local {
                return foreign().map(Some);
            }
            return Ok(None);
        }
        let index = candidates[0];
        let declaration = &self.module.declarations[index];
        match declaration.kind {
            DeclarationKind::Class => {
                if let Some(ordinal) = self.method_in_class(occurrence, declaration.span) {
                    let confidence = match checked {
                        Some(SymbolOutcome::Local) => OccurrenceConfidence::Oracle,
                        _ => OccurrenceConfidence::Index,
                    };
                    return Ok(Some((
                        OccurrenceTarget::Local(EntityId::new(ordinal)),
                        confidence,
                    )));
                }
                if self.method_count_in_class(occurrence, declaration.span) > 1 {
                    return Ok(None);
                }
                if let InheritedMemberLookup::Unique(ordinal) =
                    self.inherited_member(index, occurrence, DeclarationKind::Function)
                {
                    let confidence = match checked {
                        Some(SymbolOutcome::Local) => OccurrenceConfidence::Oracle,
                        _ => OccurrenceConfidence::Index,
                    };
                    return Ok(Some((
                        OccurrenceTarget::Local(EntityId::new(ordinal)),
                        confidence,
                    )));
                }
                Ok(None)
            }
            DeclarationKind::Alias => {
                let module_span = match alias_import_module_span(self.source, declaration.span) {
                    Some(span) => span,
                    None => return Ok(None),
                };
                let module_spelling = self.slice(module_span)?;
                let binding = self.slice(occurrence.span)?;
                let binding = core::str::from_utf8(binding).map_err(|_| {
                    PythonCollectError::Projection(PythonProjectionFault::ForeignSpellingUtf8 {
                        start: occurrence.span.start,
                        end: occurrence.span.end,
                    })
                })?;
                let target = foreign_package(
                    module_spelling,
                    binding,
                    module_span,
                    Some(EntityKind::Function),
                )?;
                let confidence = match checked {
                    Some(SymbolOutcome::Foreign { .. }) => OccurrenceConfidence::Import,
                    _ => OccurrenceConfidence::Index,
                };
                Ok(Some((target, confidence)))
            }
            _ => Ok(None),
        }
    }

    /// Resolves a function-body local `AnnAssign` annotation for one receiver
    /// name before parameter annotations are consulted.
    fn local_binding_name(
        &self,
        function: &DeclarationFact,
        receiver: &str,
        occurrence: &OccurrenceFact,
    ) -> LocalBinding {
        let mut relevant: Vec<&AnnotationFact> = Vec::new();
        for fact in &self.module.annotations {
            if fact.position != AnnotationPosition::Local {
                continue;
            }
            if fact.owner != receiver {
                continue;
            }
            if !span_contains(function.span, fact.span) {
                continue;
            }
            if self
                .module
                .declarations
                .iter()
                .enumerate()
                .any(|(index, declaration)| {
                    declaration.kind == DeclarationKind::Function
                        && self.live[index]
                        && span_contains(function.span, declaration.span)
                        && declaration.span != function.span
                        && span_contains(declaration.span, fact.span)
                })
            {
                continue;
            }
            if fact.span.start >= occurrence.span.start {
                continue;
            }
            relevant.push(fact);
        }
        if relevant.is_empty() {
            return LocalBinding::Absent;
        }
        let mut unique_name: Option<String> = None;
        for fact in relevant {
            match classify_receiver_annotation(&fact.annotation) {
                ReceiverAnnotationName::Ambiguous | ReceiverAnnotationName::Absent => {
                    return LocalBinding::Foreign;
                }
                ReceiverAnnotationName::Unique(name) => {
                    if let Some(existing) = &unique_name {
                        if existing != name {
                            return LocalBinding::Foreign;
                        }
                    } else {
                        unique_name = Some(name.to_owned());
                    }
                }
            }
        }
        match unique_name {
            Some(name) => LocalBinding::Unique(name),
            None => LocalBinding::Foreign,
        }
    }

    /// Resolves one receiver through enclosing function-local `AnnAssign`
    /// annotations, enclosing parameter annotations when no closer local
    /// assignment exists, and live module-constant annotations. A local
    /// binding with an annotation binds that annotation; a local binding
    /// without one stays foreign.
    fn module_annotation_name(
        &self,
        function: &DeclarationFact,
        receiver: &str,
        occurrence: &OccurrenceFact,
    ) -> LocalBinding {
        let mut scopes: Vec<&BindingScopeFact> = self
            .module
            .binding_scopes
            .iter()
            .filter(|scope| {
                span_contains(scope.span, occurrence.span)
                    && (span_contains(function.span, scope.span)
                        || span_contains(scope.span, function.span))
            })
            .collect();
        scopes.sort_by_key(|scope| scope.span.end - scope.span.start);
        for scope in scopes {
            if scope.nonlocals.iter().any(|name| name == receiver) {
                continue;
            }
            if scope.globals.iter().any(|name| name == receiver) {
                return self.module_constant_annotation(receiver);
            }
            if scope.locals.iter().any(|name| name == receiver) {
                let scope_function = self
                    .module
                    .declarations
                    .iter()
                    .enumerate()
                    .find(|(index, declaration)| {
                        declaration.kind == DeclarationKind::Function
                            && self.live[*index]
                            && declaration.span == scope.span
                    })
                    .map(|(_, declaration)| declaration);
                return match scope_function {
                    Some(scope_function) => match self.local_binding_name(
                        scope_function,
                        receiver,
                        occurrence,
                    ) {
                        LocalBinding::Unique(name) => LocalBinding::Unique(name),
                        LocalBinding::Foreign => LocalBinding::Foreign,
                        LocalBinding::Absent => {
                            match receiver_annotation_name(scope_function, receiver) {
                                ReceiverAnnotationName::Unique(name) => {
                                    LocalBinding::Unique(name.to_owned())
                                }
                                ReceiverAnnotationName::Ambiguous
                                | ReceiverAnnotationName::Absent => LocalBinding::Foreign,
                            }
                        }
                    },
                    None => LocalBinding::Foreign,
                };
            }
        }
        self.module_constant_annotation(receiver)
    }

    fn module_constant_annotation(&self, receiver: &str) -> LocalBinding {
        let mut annotated: Vec<&AnnotationFact> = Vec::new();
        for (index, declaration) in self.module.declarations.iter().enumerate() {
            if declaration.kind != DeclarationKind::Constant
                || declaration.name != receiver
                || !self.live[index]
            {
                continue;
            }
            if let Some(fact) = self.module.annotations.iter().find(|fact| {
                fact.position == AnnotationPosition::Field
                    && fact.owner == receiver
                    && span_contains(declaration.span, fact.span)
            }) {
                annotated.push(fact);
            }
        }
        if annotated.is_empty() {
            return LocalBinding::Absent;
        }
        let mut unique_name: Option<String> = None;
        for fact in annotated {
            match classify_receiver_annotation(&fact.annotation) {
                ReceiverAnnotationName::Ambiguous | ReceiverAnnotationName::Absent => {
                    return LocalBinding::Foreign;
                }
                ReceiverAnnotationName::Unique(name) => {
                    if let Some(existing) = &unique_name {
                        if existing != name {
                            return LocalBinding::Foreign;
                        }
                    } else {
                        unique_name = Some(name.to_owned());
                    }
                }
            }
        }
        match unique_name {
            Some(name) => LocalBinding::Unique(name),
            None => LocalBinding::Foreign,
        }
    }

    /// The declaration index of the innermost live function whose name equals
    /// `occurrence.owner` and whose span contains the call site.
    fn enclosing_function_index(&self, occurrence: &OccurrenceFact) -> Option<usize> {
        let owner_bytes = occurrence.owner.as_bytes();
        let mut best: Option<(Span, usize)> = None;
        for (index, declaration) in self.module.declarations.iter().enumerate() {
            if declaration.kind != DeclarationKind::Function
                || declaration.name.as_bytes() != owner_bytes
                || !self.live[index]
                || !span_contains(declaration.span, occurrence.span)
            {
                continue;
            }
            let area = declaration.span.end - declaration.span.start;
            let occupied = best.map_or(true, |(span, _)| area < span.end - span.start);
            if occupied {
                best = Some((declaration.span, index));
            }
        }
        best.map(|(_, index)| index)
    }

    /// Live class and import-alias declaration indices sharing one name.
    fn live_class_or_alias_indices(&self, name: &str) -> Vec<usize> {
        let name_bytes = name.as_bytes();
        self.module
            .declarations
            .iter()
            .enumerate()
            .filter(|(index, declaration)| {
                self.live[*index]
                    && declaration.name.as_bytes() == name_bytes
                    && (declaration.kind == DeclarationKind::Class
                        || (declaration.kind == DeclarationKind::Alias
                            && declaration.value_span.is_some()))
            })
            .map(|(index, _)| index)
            .collect()
    }

    /// The lane ordinal of the sole live method with the attribute spelling
    /// inside `class_span`, or `None` when zero or more than one match.
    fn method_in_class(&self, occurrence: &OccurrenceFact, class_span: Span) -> Option<u32> {
        match self.member_lookup_in_class(occurrence, class_span, DeclarationKind::Function) {
            InheritedMemberLookup::Unique(ordinal) => Some(ordinal),
            InheritedMemberLookup::Absent | InheritedMemberLookup::Ambiguous => None,
        }
    }

    /// The declaration index of the innermost live class whose name equals
    /// `class` and whose span contains the occurrence site.
    fn enclosing_class_index(&self, occurrence: &OccurrenceFact, class: &str) -> Option<usize> {
        let class_bytes = class.as_bytes();
        let mut best: Option<(Span, usize)> = None;
        for (index, declaration) in self.module.declarations.iter().enumerate() {
            if declaration.kind != DeclarationKind::Class
                || declaration.name.as_bytes() != class_bytes
                || !self.live[index]
                || !span_contains(declaration.span, occurrence.span)
            {
                continue;
            }
            let area = declaration.span.end - declaration.span.start;
            let occupied = best.map_or(true, |(span, _)| area < span.end - span.start);
            if occupied {
                best = Some((declaration.span, index));
            }
        }
        best.map(|(_, index)| index)
    }

    /// Live field declarations with the attribute spelling inside `class_span`.
    fn field_count_in_class(&self, occurrence: &OccurrenceFact, class_span: Span) -> usize {
        let attribute_bytes = occurrence.target.as_bytes();
        self.module
            .declarations
            .iter()
            .enumerate()
            .filter(|(index, declaration)| {
                declaration.kind == DeclarationKind::Field
                    && declaration.name.as_bytes() == attribute_bytes
                    && self.live[*index]
                    && span_contains(class_span, declaration.span)
            })
            .count()
    }

    /// Live method declarations with the attribute spelling inside `class_span`.
    fn method_count_in_class(&self, occurrence: &OccurrenceFact, class_span: Span) -> usize {
        let attribute_bytes = occurrence.target.as_bytes();
        self.module
            .declarations
            .iter()
            .enumerate()
            .filter(|(index, declaration)| {
                declaration.kind == DeclarationKind::Function
                    && declaration.name.as_bytes() == attribute_bytes
                    && self.live[*index]
                    && span_contains(class_span, declaration.span)
            })
            .count()
    }

    /// Resolves one inherited member by walking simple same-file base classes.
    fn inherited_member(
        &self,
        class_index: usize,
        occurrence: &OccurrenceFact,
        member_kind: DeclarationKind,
    ) -> InheritedMemberLookup {
        self.inherited_member_from_bases(class_index, occurrence, member_kind, 0)
    }

    /// Walks the base classes of `class_index`. Each sibling base starts its
    /// own path with a fresh depth budget and visited set.
    fn inherited_member_from_bases(
        &self,
        class_index: usize,
        occurrence: &OccurrenceFact,
        member_kind: DeclarationKind,
        depth: usize,
    ) -> InheritedMemberLookup {
        let declaration = &self.module.declarations[class_index];
        let mut base_results = Vec::new();
        for base in &declaration.bases {
            let Some(base_name) = simple_base_name(base) else {
                continue;
            };
            let Some(base_index) = self.unique_live_class_index(base_name) else {
                continue;
            };
            if depth >= MAX_INHERITED_BASE_LINKS {
                continue;
            }
            let mut visited = HashSet::new();
            visited.insert(class_index);
            let result = self.inherited_member_in_class(
                base_index,
                occurrence,
                member_kind,
                depth + 1,
                &mut visited,
            );
            if result != InheritedMemberLookup::Absent {
                base_results.push(result);
            }
        }
        merge_inherited_base_results(base_results)
    }

    fn inherited_member_in_class(
        &self,
        class_index: usize,
        occurrence: &OccurrenceFact,
        member_kind: DeclarationKind,
        depth: usize,
        visited: &mut HashSet<usize>,
    ) -> InheritedMemberLookup {
        if !visited.insert(class_index) {
            return InheritedMemberLookup::Absent;
        }
        let class_span = self.module.declarations[class_index].span;
        match self.member_lookup_in_class(occurrence, class_span, member_kind) {
            InheritedMemberLookup::Unique(ordinal) => InheritedMemberLookup::Unique(ordinal),
            InheritedMemberLookup::Ambiguous => InheritedMemberLookup::Ambiguous,
            InheritedMemberLookup::Absent => self.inherited_member_from_bases_with_visited(
                class_index,
                occurrence,
                member_kind,
                depth,
                visited,
            ),
        }
    }

    /// Continues one inherited-member path through `class_index`'s bases,
    /// sharing the path-local visited set and depth counter.
    fn inherited_member_from_bases_with_visited(
        &self,
        class_index: usize,
        occurrence: &OccurrenceFact,
        member_kind: DeclarationKind,
        depth: usize,
        visited: &mut HashSet<usize>,
    ) -> InheritedMemberLookup {
        let declaration = &self.module.declarations[class_index];
        let mut base_results = Vec::new();
        for base in &declaration.bases {
            let Some(base_name) = simple_base_name(base) else {
                continue;
            };
            let Some(base_index) = self.unique_live_class_index(base_name) else {
                continue;
            };
            if depth >= MAX_INHERITED_BASE_LINKS {
                continue;
            }
            let result = self.inherited_member_in_class(
                base_index,
                occurrence,
                member_kind,
                depth + 1,
                visited,
            );
            if result != InheritedMemberLookup::Absent {
                base_results.push(result);
            }
        }
        merge_inherited_base_results(base_results)
    }

    /// The lane ordinal of the sole live declaration of `member_kind` with
    /// the attribute spelling inside `class_span`.
    fn member_lookup_in_class(
        &self,
        occurrence: &OccurrenceFact,
        class_span: Span,
        member_kind: DeclarationKind,
    ) -> InheritedMemberLookup {
        let attribute_bytes = occurrence.target.as_bytes();
        let mut matches = Vec::new();
        for (index, declaration) in self.module.declarations.iter().enumerate() {
            if declaration.kind != member_kind
                || declaration.name.as_bytes() != attribute_bytes
                || !self.live[index]
                || !span_contains(class_span, declaration.span)
            {
                continue;
            }
            if let Some(ordinal) = self.ordinals[index] {
                matches.push(ordinal);
            }
        }
        match matches.len() {
            0 => InheritedMemberLookup::Absent,
            1 => InheritedMemberLookup::Unique(matches[0]),
            _ => InheritedMemberLookup::Ambiguous,
        }
    }

    /// Live own-field declaration indices with one name inside `class_span`.
    fn own_field_indices_named(&self, class_span: Span, field_name: &str) -> Vec<usize> {
        let field_bytes = field_name.as_bytes();
        self.module
            .declarations
            .iter()
            .enumerate()
            .filter_map(|(index, declaration)| {
                if declaration.kind == DeclarationKind::Field
                    && declaration.name.as_bytes() == field_bytes
                    && self.live[index]
                    && span_contains(class_span, declaration.span)
                    && !self
                        .module
                        .declarations
                        .iter()
                        .enumerate()
                        .any(|(inner_index, inner)| {
                            inner.kind == DeclarationKind::Class
                                && self.live[inner_index]
                                && span_contains(class_span, inner.span)
                                && inner.span != class_span
                                && span_contains(inner.span, declaration.span)
                        })
                {
                    Some(index)
                } else {
                    None
                }
            })
            .collect()
    }

    fn call_return_returned_class(
        &self,
        occurrence: &OccurrenceFact,
        method: &str,
        receiver: &OccurrenceReceiver,
    ) -> Option<String> {
        match receiver {
            OccurrenceReceiver::EnclosingClass { class } => self
                .enclosing_class_index(occurrence, class)
                .and_then(|class_index| self.call_return_method_return_class(class_index, method)),
            OccurrenceReceiver::Foreign { receiver: Some(name) } => {
                if self.receiver_assigned_in_scope(occurrence, name) {
                    None
                } else {
                    self.named_attribute_class_index(occurrence, name)
                        .and_then(|class_index| self.call_return_method_return_class(class_index, method))
                }
            }
            OccurrenceReceiver::None => {
                let indices = self.module_level_function_indices_named(method);
                if indices.len() == 1 {
                    self.return_annotation_class_name_from_index(indices[0])
                } else {
                    None
                }
            }
            OccurrenceReceiver::InstanceAttribute { class, attribute } => self
                .enclosing_class_index(occurrence, class)
                .and_then(|class_index| {
                    self.field_annotation_class_name_for_class(class_index, attribute)
                })
                .and_then(|field_class| self.unique_live_class_index(&field_class))
                .and_then(|class_index| self.call_return_method_return_class(class_index, method)),
            OccurrenceReceiver::NamedAttribute { name, attribute } => {
                if self.receiver_assigned_in_scope(occurrence, name) {
                    None
                } else {
                    self.named_attribute_class_index(occurrence, name)
                        .and_then(|class_index| {
                            self.field_annotation_class_name_for_class(class_index, attribute)
                        })
                        .and_then(|field_class| self.unique_live_class_index(&field_class))
                        .and_then(|class_index| {
                            self.call_return_method_return_class(class_index, method)
                        })
                }
            }
            OccurrenceReceiver::ChainedAttribute { root, attributes } => {
                let class_index = match root {
                    AttributeChainRoot::Enclosing { class } => {
                        self.enclosing_class_index(occurrence, class)
                    }
                    AttributeChainRoot::Name { name } => {
                        if self.receiver_assigned_in_scope(occurrence, name) {
                            None
                        } else {
                            self.named_attribute_class_index(occurrence, name)
                        }
                    }
                };
                if let Some(mut class_index) = class_index {
                    for attribute in attributes {
                        let Some(class_name) =
                            self.field_annotation_class_name_for_class(class_index, attribute)
                        else {
                            return None;
                        };
                        let Some(next_index) = self.unique_live_class_index(&class_name) else {
                            return None;
                        };
                        class_index = next_index;
                    }
                    self.call_return_method_return_class(class_index, method)
                } else {
                    None
                }
            }
            OccurrenceReceiver::SubscriptedAttribute { root, steps } => self
                .subscripted_attribute_class_name(occurrence, root, steps)
                .and_then(|class_name| self.unique_live_class_index(&class_name))
                .and_then(|class_index| self.call_return_method_return_class(class_index, method)),
            OccurrenceReceiver::SubscriptedCall { call, steps } => self
                .subscripted_call_class_name(occurrence, call.as_ref(), steps)
                .and_then(|class_name| self.unique_live_class_index(&class_name))
                .and_then(|class_index| self.call_return_method_return_class(class_index, method)),
            OccurrenceReceiver::Constructed { class } => self
                .unique_live_class_index(class)
                .and_then(|class_index| self.call_return_method_return_class(class_index, method)),
            OccurrenceReceiver::CallReturn {
                method: inner_method,
                receiver: inner_receiver,
            } => {
                let inner_class =
                    self.call_return_returned_class(occurrence, inner_method, inner_receiver.as_ref())?;
                let class_index = self.unique_live_class_index(&inner_class)?;
                self.call_return_method_return_class(class_index, method)
            }
            OccurrenceReceiver::Foreign { receiver: None }
            | OccurrenceReceiver::Module
            | OccurrenceReceiver::Super { .. } => None,
        }
    }

    fn call_return_target(
        &self,
        occurrence: &OccurrenceFact,
        checked: Option<&SymbolOutcome>,
        method: &str,
        receiver: &OccurrenceReceiver,
    ) -> Result<Option<(OccurrenceTarget<'source>, OccurrenceConfidence)>, PythonCollectError> {
        let class_name = self.call_return_returned_class(occurrence, method, receiver);
        self.instance_or_named_attribute_target(occurrence, checked, class_name)
    }

    /// Live module-level function declaration indices with one name outside
    /// every live class body.
    fn module_level_function_indices_named(&self, method: &str) -> Vec<usize> {
        let method_bytes = method.as_bytes();
        self.module
            .declarations
            .iter()
            .enumerate()
            .filter_map(|(index, declaration)| {
                if declaration.kind != DeclarationKind::Function
                    || declaration.name.as_bytes() != method_bytes
                    || !self.live[index]
                {
                    return None;
                }
                let in_class = self.module.declarations.iter().enumerate().any(|(class_index, class)| {
                    class.kind == DeclarationKind::Class
                        && self.live[class_index]
                        && span_contains(class.span, declaration.span)
                });
                if in_class {
                    None
                } else {
                    Some(index)
                }
            })
            .collect()
    }

    /// Live own-method declaration indices with one name inside `class_span`
    /// and outside any strictly inner live class.
    fn own_method_indices_named(&self, class_span: Span, method_name: &str) -> Vec<usize> {
        let method_bytes = method_name.as_bytes();
        self.module
            .declarations
            .iter()
            .enumerate()
            .filter_map(|(index, declaration)| {
                if declaration.kind != DeclarationKind::Function
                    || declaration.name.as_bytes() != method_bytes
                    || !self.live[index]
                    || !span_contains(class_span, declaration.span)
                    || self
                        .module
                        .declarations
                        .iter()
                        .enumerate()
                        .any(|(inner_index, inner)| {
                            inner.kind == DeclarationKind::Class
                                && self.live[inner_index]
                                && span_contains(class_span, inner.span)
                                && inner.span != class_span
                                && span_contains(inner.span, declaration.span)
                        })
                {
                    None
                } else {
                    Some(index)
                }
            })
            .collect()
    }

    /// The class name one call-return callee's return annotation names, when
    /// unique and usable.
    fn call_return_method_return_class(&self, class_index: usize, method: &str) -> Option<String> {
        match self.call_return_method_index(class_index, method) {
            CallReturnMethodLookup::Unique(index) => {
                self.return_annotation_class_name_from_index(index)
            }
            CallReturnMethodLookup::Absent | CallReturnMethodLookup::Ambiguous => None,
        }
    }

    fn call_return_method_index(
        &self,
        class_index: usize,
        method: &str,
    ) -> CallReturnMethodLookup {
        let class_span = self.module.declarations[class_index].span;
        let own = self.own_method_indices_named(class_span, method);
        match own.len() {
            0 => self.call_return_inherited_method_from_bases(class_index, method, 0),
            1 => CallReturnMethodLookup::Unique(own[0]),
            _ => CallReturnMethodLookup::Ambiguous,
        }
    }

    fn call_return_inherited_method_from_bases(
        &self,
        class_index: usize,
        method: &str,
        depth: usize,
    ) -> CallReturnMethodLookup {
        let declaration = &self.module.declarations[class_index];
        let mut base_results = Vec::new();
        for base in &declaration.bases {
            let Some(base_name) = simple_base_name(base) else {
                continue;
            };
            let Some(base_index) = self.unique_live_class_index(base_name) else {
                continue;
            };
            if depth >= MAX_INHERITED_BASE_LINKS {
                continue;
            }
            let mut visited = HashSet::new();
            visited.insert(class_index);
            let result = self.call_return_inherited_method_index(
                base_index,
                method,
                depth + 1,
                &mut visited,
            );
            if result != CallReturnMethodLookup::Absent {
                base_results.push(result);
            }
        }
        merge_call_return_method_results(base_results)
    }

    fn call_return_inherited_method_index(
        &self,
        class_index: usize,
        method: &str,
        depth: usize,
        visited: &mut HashSet<usize>,
    ) -> CallReturnMethodLookup {
        if !visited.insert(class_index) {
            return CallReturnMethodLookup::Absent;
        }
        let class_span = self.module.declarations[class_index].span;
        let own = self.own_method_indices_named(class_span, method);
        match own.len() {
            0 => self.call_return_inherited_method_from_bases_with_visited(
                class_index,
                method,
                depth,
                visited,
            ),
            1 => CallReturnMethodLookup::Unique(own[0]),
            _ => CallReturnMethodLookup::Ambiguous,
        }
    }

    fn call_return_inherited_method_from_bases_with_visited(
        &self,
        class_index: usize,
        method: &str,
        depth: usize,
        visited: &mut HashSet<usize>,
    ) -> CallReturnMethodLookup {
        let declaration = &self.module.declarations[class_index];
        let mut base_results = Vec::new();
        for base in &declaration.bases {
            let Some(base_name) = simple_base_name(base) else {
                continue;
            };
            let Some(base_index) = self.unique_live_class_index(base_name) else {
                continue;
            };
            if depth >= MAX_INHERITED_BASE_LINKS {
                continue;
            }
            let result = self.call_return_inherited_method_index(
                base_index,
                method,
                depth + 1,
                visited,
            );
            if result != CallReturnMethodLookup::Absent {
                base_results.push(result);
            }
        }
        merge_call_return_method_results(base_results)
    }

    /// True when a binding scope containing the occurrence records `name` as
    /// assigned inside that scope.
    fn receiver_assigned_in_scope(&self, occurrence: &OccurrenceFact, name: &str) -> bool {
        self.module.binding_scopes.iter().any(|scope| {
            span_contains(scope.span, occurrence.span)
                && scope.assigned.iter().any(|assigned| assigned == name)
        })
    }

    /// An assigned parameter hides its annotation, including from a nested
    /// function. A function-local `AnnAssign` does not: that store is the
    /// annotation. A `global` name keeps the module annotation.
    fn assigned_name_hides_annotation(&self, occurrence: &OccurrenceFact, name: &str) -> bool {
        if matches!(
            self.raw_name_receiver_annotation(occurrence, name),
            RawReceiverAnnotation::Local(_)
        ) {
            return false;
        }
        if self.module.binding_scopes.iter().any(|scope| {
            span_contains(scope.span, occurrence.span)
                && scope.globals.iter().any(|global| global == name)
        }) {
            return false;
        }
        self.receiver_assigned_in_scope(occurrence, name)
    }

    fn return_annotation_class_name_from_index(&self, fn_index: usize) -> Option<String> {
        let declaration = &self.module.declarations[fn_index];
        let Some(fact) = self.return_annotation(declaration) else {
            return None;
        };
        match classify_receiver_annotation(&fact.annotation) {
            ReceiverAnnotationName::Unique(name) => Some(name.to_owned()),
            ReceiverAnnotationName::Absent | ReceiverAnnotationName::Ambiguous => None,
        }
    }

    fn instance_or_named_attribute_target(
        &self,
        occurrence: &OccurrenceFact,
        checked: Option<&SymbolOutcome>,
        class_name: Option<String>,
    ) -> Result<Option<(OccurrenceTarget<'source>, OccurrenceConfidence)>, PythonCollectError> {
        let method_foreign = || {
            foreign_method(self.slice(occurrence.span)?, occurrence.span)
                .map(|target| (target, OccurrenceConfidence::Index))
        };
        let field_foreign = || {
            foreign_field(self.slice(occurrence.span)?, occurrence.span)
                .map(|target| (target, OccurrenceConfidence::Index))
        };
        let class_name = class_name.and_then(|name| {
            self.unique_live_class_index(&name)
                .map(|index| self.module.declarations[index].name.clone())
        });
        let Some(class_name) = class_name else {
            return match occurrence.kind {
                OccurrenceKind::MethodCall | OccurrenceKind::FunctionCall => {
                    method_foreign().map(Some)
                }
                OccurrenceKind::AttributeRead => field_foreign().map(Some),
            };
        };
        match occurrence.kind {
            OccurrenceKind::MethodCall | OccurrenceKind::FunctionCall => {
                if let Some(resolved) =
                    self.class_qualified_call_target(occurrence, &class_name, checked)?
                {
                    Ok(Some(resolved))
                } else {
                    method_foreign().map(Some)
                }
            }
            OccurrenceKind::AttributeRead => {
                if let Some(resolved) =
                    self.class_qualified_read_target(occurrence, &class_name, checked)?
                {
                    Ok(Some(resolved))
                } else {
                    field_foreign().map(Some)
                }
            }
        }
    }

    /// The annotated class name one field on a class names, when unique.
    fn field_annotation_class_name_for_class(
        &self,
        class_index: usize,
        attribute: &str,
    ) -> Option<String> {
        let class_span = self.module.declarations[class_index].span;
        let own_fields = self.own_field_indices_named(class_span, attribute);
        match own_fields.len() {
            0 => {
                if let Some(result) =
                    self.instance_field_annotation_class_name(class_index, attribute)
                {
                    match result {
                        InstanceAttributeClassLookup::Unique { class_name, .. } => {
                            Some(class_name)
                        }
                        InstanceAttributeClassLookup::Absent
                        | InstanceAttributeClassLookup::Ambiguous
                        | InstanceAttributeClassLookup::Unannotated => None,
                    }
                } else {
                    match self.inherited_field_annotation_class_name(class_index, attribute) {
                        InstanceAttributeClassLookup::Unique { class_name, .. } => Some(class_name),
                        InstanceAttributeClassLookup::Absent
                        | InstanceAttributeClassLookup::Ambiguous
                        | InstanceAttributeClassLookup::Unannotated => None,
                    }
                }
            }
            1 => match self.field_annotation_class_name_from_index(own_fields[0]) {
                InstanceAttributeClassLookup::Unique { class_name, .. } => Some(class_name),
                InstanceAttributeClassLookup::Absent
                | InstanceAttributeClassLookup::Ambiguous
                | InstanceAttributeClassLookup::Unannotated => None,
            },
            _ => None,
        }
    }

    /// Raw field annotation for one attribute on a class, with the same ownership
    /// rules as `field_annotation_class_name_for_class`.
    fn raw_field_annotation_for_class(
        &self,
        class_index: usize,
        attribute: &str,
    ) -> Option<&'a Annotation> {
        let class_span = self.module.declarations[class_index].span;
        let own_fields = self.own_field_indices_named(class_span, attribute);
        match own_fields.len() {
            0 => {
                let attribute_bytes = attribute.as_bytes();
                let mut instance_facts: Vec<&AnnotationFact> = Vec::new();
                for fact in &self.module.annotations {
                    if fact.position != AnnotationPosition::Instance {
                        continue;
                    }
                    if fact.owner.as_bytes() != attribute_bytes {
                        continue;
                    }
                    if !span_contains(class_span, fact.span) {
                        continue;
                    }
                    if self.module.declarations.iter().enumerate().any(|(index, declaration)| {
                        declaration.kind == DeclarationKind::Class
                            && self.live[index]
                            && span_contains(class_span, declaration.span)
                            && declaration.span != class_span
                            && span_contains(declaration.span, fact.span)
                    }) {
                        continue;
                    }
                    instance_facts.push(fact);
                }
                if instance_facts.len() > 1 {
                    return None;
                }
                if let Some(fact) = instance_facts.first() {
                    return Some(&fact.annotation);
                }
                match self.inherited_field_annotation_class_name(class_index, attribute) {
                    InstanceAttributeClassLookup::Unique { field_index, .. } => {
                        let declaration = &self.module.declarations[field_index];
                        self.field_annotation(declaration)
                            .map(|fact| &fact.annotation)
                    }
                    InstanceAttributeClassLookup::Absent
                    | InstanceAttributeClassLookup::Ambiguous
                    | InstanceAttributeClassLookup::Unannotated => None,
                }
            }
            1 => {
                let declaration = &self.module.declarations[own_fields[0]];
                self.field_annotation(declaration)
                    .map(|fact| &fact.annotation)
            }
            _ => None,
        }
    }

    /// Raw annotation for a plain-name receiver with leading index subscripts.
    fn raw_local_annotation(
        &self,
        function: &DeclarationFact,
        name: &str,
        occurrence: &OccurrenceFact,
    ) -> RawReceiverAnnotation<'a> {
        let mut relevant: Vec<&AnnotationFact> = Vec::new();
        for fact in &self.module.annotations {
            if fact.position != AnnotationPosition::Local {
                continue;
            }
            if fact.owner != name {
                continue;
            }
            if !span_contains(function.span, fact.span) {
                continue;
            }
            if self
                .module
                .declarations
                .iter()
                .enumerate()
                .any(|(index, declaration)| {
                    declaration.kind == DeclarationKind::Function
                        && self.live[index]
                        && span_contains(function.span, declaration.span)
                        && declaration.span != function.span
                        && span_contains(declaration.span, fact.span)
                })
            {
                continue;
            }
            if fact.span.start >= occurrence.span.start {
                continue;
            }
            relevant.push(fact);
        }
        match relevant.len() {
            0 => RawReceiverAnnotation::Absent,
            1 => RawReceiverAnnotation::Local(&relevant[0].annotation),
            _ => RawReceiverAnnotation::Blocked,
        }
    }

    fn raw_name_receiver_annotation(
        &self,
        occurrence: &OccurrenceFact,
        name: &str,
    ) -> RawReceiverAnnotation<'a> {
        let Some(function_index) = self.enclosing_function_index(occurrence) else {
            return RawReceiverAnnotation::Absent;
        };
        let function = &self.module.declarations[function_index];
        match self.raw_local_annotation(function, name, occurrence) {
            RawReceiverAnnotation::Local(annotation) => {
                return RawReceiverAnnotation::Local(annotation);
            }
            RawReceiverAnnotation::Inherited(annotation) => {
                return RawReceiverAnnotation::Inherited(annotation);
            }
            RawReceiverAnnotation::Blocked => return RawReceiverAnnotation::Blocked,
            RawReceiverAnnotation::Absent => {}
        }
        match self.local_binding_name(function, name, occurrence) {
            LocalBinding::Foreign | LocalBinding::Unique(_) => {
                return RawReceiverAnnotation::Blocked;
            }
            LocalBinding::Absent => {}
        }
        for parameter in &function.parameters {
            if is_receiver_parameter(function.receiver, &parameter.name) {
                continue;
            }
            if parameter.name == name {
                return RawReceiverAnnotation::Inherited(&parameter.annotation);
            }
        }
        self.raw_module_level_name_annotation(function, name, occurrence)
    }

    fn raw_module_level_name_annotation(
        &self,
        function: &DeclarationFact,
        name: &str,
        occurrence: &OccurrenceFact,
    ) -> RawReceiverAnnotation<'a> {
        let mut scopes: Vec<&BindingScopeFact> = self
            .module
            .binding_scopes
            .iter()
            .filter(|scope| {
                span_contains(scope.span, occurrence.span)
                    && (span_contains(function.span, scope.span)
                        || span_contains(scope.span, function.span))
            })
            .collect();
        scopes.sort_by_key(|scope| scope.span.end - scope.span.start);
        for scope in scopes {
            if scope.nonlocals.iter().any(|assigned| assigned == name) {
                continue;
            }
            if scope.globals.iter().any(|assigned| assigned == name) {
                return match self.raw_module_constant_annotation(name) {
                    Some(annotation) => RawReceiverAnnotation::Inherited(annotation),
                    None => RawReceiverAnnotation::Blocked,
                };
            }
            if scope.locals.iter().any(|assigned| assigned == name) {
                let scope_function = self
                    .module
                    .declarations
                    .iter()
                    .enumerate()
                    .find(|(index, declaration)| {
                        declaration.kind == DeclarationKind::Function
                            && self.live[*index]
                            && declaration.span == scope.span
                    })
                    .map(|(_, declaration)| declaration);
                let Some(scope_function) = scope_function else {
                    return RawReceiverAnnotation::Blocked;
                };
                match self.raw_local_annotation(scope_function, name, occurrence) {
                    RawReceiverAnnotation::Local(annotation) => {
                        return RawReceiverAnnotation::Local(annotation);
                    }
                    RawReceiverAnnotation::Inherited(annotation) => {
                        return RawReceiverAnnotation::Inherited(annotation);
                    }
                    RawReceiverAnnotation::Blocked => return RawReceiverAnnotation::Blocked,
                    RawReceiverAnnotation::Absent => {}
                }
                if let Some(annotation) = parameter_annotation_raw(scope_function, name) {
                    return RawReceiverAnnotation::Inherited(annotation);
                }
                return RawReceiverAnnotation::Blocked;
            }
        }
        match self.raw_module_constant_annotation(name) {
            Some(annotation) => RawReceiverAnnotation::Inherited(annotation),
            None => RawReceiverAnnotation::Absent,
        }
    }

    fn raw_module_constant_annotation(&self, name: &str) -> Option<&'a Annotation> {
        let mut annotated: Vec<&AnnotationFact> = Vec::new();
        for (index, declaration) in self.module.declarations.iter().enumerate() {
            if declaration.kind != DeclarationKind::Constant
                || declaration.name != name
                || !self.live[index]
            {
                continue;
            }
            if let Some(fact) = self.module.annotations.iter().find(|fact| {
                fact.position == AnnotationPosition::Field
                    && fact.owner == name
                    && span_contains(declaration.span, fact.span)
            }) {
                annotated.push(fact);
            }
        }
        if annotated.len() == 1 {
            Some(&annotated[0].annotation)
        } else {
            None
        }
    }

    /// Class named by `annotation` after `count` one-argument index peels.
    /// A failed peel is `None` and does not fall back to a same-named class.
    fn peeled_index_class(&self, annotation: &Annotation, count: usize) -> Option<usize> {
        let expanded = self.expand_type_alias(annotation)?;
        let peeled = peel_indexes(&expanded, count)?;
        match classify_receiver_annotation(&peeled) {
            ReceiverAnnotationName::Unique(class_name) => self.unique_live_class_index(class_name),
            ReceiverAnnotationName::Absent | ReceiverAnnotationName::Ambiguous => None,
        }
    }

    fn subscripted_attribute_class_name(
        &self,
        occurrence: &OccurrenceFact,
        root: &AttributeChainRoot,
        steps: &[AttributeStep],
    ) -> Option<String> {
        let assigned = match root {
            AttributeChainRoot::Name { name } => self.receiver_assigned_in_scope(occurrence, name),
            AttributeChainRoot::Enclosing { .. } => false,
        };
        let (leading_indexes, groups) = split_subscript_steps(root, steps)?;
        let mut class_index = match root {
            AttributeChainRoot::Enclosing { class } => {
                self.enclosing_class_index(occurrence, class)?
            }
            AttributeChainRoot::Name { name } if leading_indexes > 0 => {
                let class_only = groups.is_empty()
                    && steps.iter().all(|step| matches!(step, AttributeStep::NameIndex));
                match self.raw_name_receiver_annotation(occurrence, name) {
                    RawReceiverAnnotation::Local(annotation) => {
                        self.peeled_index_class(annotation, leading_indexes)?
                    }
                    RawReceiverAnnotation::Inherited(annotation) => {
                        if assigned {
                            return None;
                        }
                        self.peeled_index_class(annotation, leading_indexes)?
                    }
                    RawReceiverAnnotation::Blocked => return None,
                    // `Child[int].note` has no value annotation. A plain-name
                    // subscript of a live class is that class. A numeric
                    // `Holder[0]` is not, and a failed peel does not fall back.
                    RawReceiverAnnotation::Absent if class_only && !assigned => {
                        self.unique_live_class_index(name)?
                    }
                    RawReceiverAnnotation::Absent => return None,
                }
            }
            AttributeChainRoot::Name { name } => {
                // A local `AnnAssign` store is not a shadow. A parameter,
                // enclosing parameter, or module binding is.
                if assigned
                    && !matches!(
                        self.raw_name_receiver_annotation(occurrence, name),
                        RawReceiverAnnotation::Local(_)
                    )
                {
                    return None;
                }
                self.named_attribute_class_index(occurrence, name)?
            }
        };
        for (field, index_count) in groups {
            if index_count == 0 {
                let Some(class_name) =
                    self.field_annotation_class_name_for_class(class_index, &field)
                else {
                    return None;
                };
                class_index = self.unique_live_class_index(&class_name)?;
            } else {
                let raw = self.raw_field_annotation_for_class(class_index, &field)?;
                let expanded = self.expand_type_alias(raw)?;
                let peeled = peel_indexes(&expanded, index_count)?;
                match classify_receiver_annotation(&peeled) {
                    ReceiverAnnotationName::Unique(class_name) => {
                        class_index = self.unique_live_class_index(class_name)?;
                    }
                    ReceiverAnnotationName::Absent | ReceiverAnnotationName::Ambiguous => {
                        return None;
                    }
                }
            }
        }
        Some(self.module.declarations[class_index].name.clone())
    }

    fn subscripted_call_class_name(
        &self,
        occurrence: &OccurrenceFact,
        call: &OccurrenceReceiver,
        steps: &[AttributeStep],
    ) -> Option<String> {
        let OccurrenceReceiver::CallReturn { method, receiver } = call else {
            return None;
        };
        let (leading_indexes, groups) = split_call_subscript_steps(steps)?;
        let mut class_index = if leading_indexes > 0 {
            let fn_index = self.call_return_method_fn_index(occurrence, method, receiver)?;
            let declaration = &self.module.declarations[fn_index];
            let raw = &self.return_annotation(declaration)?.annotation;
            self.peeled_index_class(raw, leading_indexes)?
        } else {
            let class_name = self.call_return_returned_class(occurrence, method, receiver)?;
            self.unique_live_class_index(&class_name)?
        };
        for (field, index_count) in groups {
            if index_count == 0 {
                let Some(class_name) =
                    self.field_annotation_class_name_for_class(class_index, &field)
                else {
                    return None;
                };
                class_index = self.unique_live_class_index(&class_name)?;
            } else {
                let raw = self.raw_field_annotation_for_class(class_index, &field)?;
                let expanded = self.expand_type_alias(raw)?;
                let peeled = peel_indexes(&expanded, index_count)?;
                match classify_receiver_annotation(&peeled) {
                    ReceiverAnnotationName::Unique(class_name) => {
                        class_index = self.unique_live_class_index(class_name)?;
                    }
                    ReceiverAnnotationName::Absent | ReceiverAnnotationName::Ambiguous => {
                        return None;
                    }
                }
            }
        }
        Some(self.module.declarations[class_index].name.clone())
    }

    /// Live function declaration index for one call-return callee, using the
    /// same receiver walk as `call_return_returned_class` but without classifying
    /// the return annotation.
    fn call_return_method_fn_index(
        &self,
        occurrence: &OccurrenceFact,
        method: &str,
        receiver: &OccurrenceReceiver,
    ) -> Option<usize> {
        match receiver {
            OccurrenceReceiver::EnclosingClass { class } => self
                .enclosing_class_index(occurrence, class)
                .and_then(|class_index| self.call_return_method_fn_index_on_class(class_index, method)),
            OccurrenceReceiver::Foreign { receiver: Some(name) } => {
                let assigned = self.receiver_assigned_in_scope(occurrence, name);
                if assigned
                    && !matches!(
                        self.raw_name_receiver_annotation(occurrence, name),
                        RawReceiverAnnotation::Local(_)
                    )
                {
                    None
                } else {
                    self.named_attribute_class_index(occurrence, name).and_then(|class_index| {
                        self.call_return_method_fn_index_on_class(class_index, method)
                    })
                }
            }
            OccurrenceReceiver::None => {
                let indices = self.module_level_function_indices_named(method);
                if indices.len() == 1 {
                    Some(indices[0])
                } else {
                    None
                }
            }
            OccurrenceReceiver::InstanceAttribute { class, attribute } => self
                .enclosing_class_index(occurrence, class)
                .and_then(|class_index| {
                    self.field_annotation_class_name_for_class(class_index, attribute)
                })
                .and_then(|field_class| self.unique_live_class_index(&field_class))
                .and_then(|class_index| self.call_return_method_fn_index_on_class(class_index, method)),
            OccurrenceReceiver::NamedAttribute { name, attribute } => {
                let assigned = self.receiver_assigned_in_scope(occurrence, name);
                if assigned
                    && !matches!(
                        self.raw_name_receiver_annotation(occurrence, name),
                        RawReceiverAnnotation::Local(_)
                    )
                {
                    None
                } else {
                    self.named_attribute_class_index(occurrence, name)
                        .and_then(|class_index| {
                            self.field_annotation_class_name_for_class(class_index, attribute)
                        })
                        .and_then(|field_class| self.unique_live_class_index(&field_class))
                        .and_then(|class_index| {
                            self.call_return_method_fn_index_on_class(class_index, method)
                        })
                }
            }
            OccurrenceReceiver::ChainedAttribute { root, attributes } => {
                let class_index = match root {
                    AttributeChainRoot::Enclosing { class } => {
                        self.enclosing_class_index(occurrence, class)
                    }
                    AttributeChainRoot::Name { name } => {
                        let assigned = self.receiver_assigned_in_scope(occurrence, name);
                        if assigned
                            && !matches!(
                                self.raw_name_receiver_annotation(occurrence, name),
                                RawReceiverAnnotation::Local(_)
                            )
                        {
                            None
                        } else {
                            self.named_attribute_class_index(occurrence, name)
                        }
                    }
                };
                if let Some(mut class_index) = class_index {
                    for attribute in attributes {
                        let Some(class_name) =
                            self.field_annotation_class_name_for_class(class_index, attribute)
                        else {
                            return None;
                        };
                        let Some(next_index) = self.unique_live_class_index(&class_name) else {
                            return None;
                        };
                        class_index = next_index;
                    }
                    self.call_return_method_fn_index_on_class(class_index, method)
                } else {
                    None
                }
            }
            OccurrenceReceiver::SubscriptedAttribute { root, steps } => self
                .subscripted_attribute_class_name(occurrence, root, steps)
                .and_then(|class_name| self.unique_live_class_index(&class_name))
                .and_then(|class_index| self.call_return_method_fn_index_on_class(class_index, method)),
            OccurrenceReceiver::Constructed { class } => self
                .unique_live_class_index(class)
                .and_then(|class_index| self.call_return_method_fn_index_on_class(class_index, method)),
            OccurrenceReceiver::CallReturn {
                method: inner_method,
                receiver: inner_receiver,
            } => {
                let inner_class =
                    self.call_return_returned_class(occurrence, inner_method, inner_receiver.as_ref())?;
                let class_index = self.unique_live_class_index(&inner_class)?;
                self.call_return_method_fn_index_on_class(class_index, method)
            }
            OccurrenceReceiver::SubscriptedCall { call, steps } => self
                .subscripted_call_class_name(occurrence, call.as_ref(), steps)
                .and_then(|class_name| self.unique_live_class_index(&class_name))
                .and_then(|class_index| self.call_return_method_fn_index_on_class(class_index, method)),
            OccurrenceReceiver::Foreign { receiver: None }
            | OccurrenceReceiver::Module
            | OccurrenceReceiver::Super { .. } => None,
        }
    }

    fn call_return_method_fn_index_on_class(
        &self,
        class_index: usize,
        method: &str,
    ) -> Option<usize> {
        match self.call_return_method_index(class_index, method) {
            CallReturnMethodLookup::Unique(index) => Some(index),
            CallReturnMethodLookup::Absent | CallReturnMethodLookup::Ambiguous => None,
        }
    }

    /// Resolves one instance-attribute annotation written on `self.name` or
    /// `cls.name` inside class-body methods. Returns `None` when no matching
    /// facts exist so callers can fall through to inherited lookup.
    fn instance_field_annotation_class_name(
        &self,
        class_index: usize,
        attribute: &str,
    ) -> Option<InstanceAttributeClassLookup> {
        let class_span = self.module.declarations[class_index].span;
        let attribute_bytes = attribute.as_bytes();
        let mut saw_fact = false;
        let mut unique_name: Option<String> = None;
        for fact in &self.module.annotations {
            if fact.position != AnnotationPosition::Instance {
                continue;
            }
            if fact.owner.as_bytes() != attribute_bytes {
                continue;
            }
            if !span_contains(class_span, fact.span) {
                continue;
            }
            if self.module.declarations.iter().enumerate().any(|(index, declaration)| {
                declaration.kind == DeclarationKind::Class
                    && self.live[index]
                    && span_contains(class_span, declaration.span)
                    && declaration.span != class_span
                    && span_contains(declaration.span, fact.span)
            }) {
                continue;
            }
            saw_fact = true;
            match classify_receiver_annotation(&fact.annotation) {
                ReceiverAnnotationName::Unique(name) => {
                    if let Some(existing) = &unique_name {
                        if existing.as_str() != name {
                            return Some(InstanceAttributeClassLookup::Ambiguous);
                        }
                    } else {
                        unique_name = Some(name.to_owned());
                    }
                }
                ReceiverAnnotationName::Ambiguous | ReceiverAnnotationName::Absent => {
                    return Some(InstanceAttributeClassLookup::Absent);
                }
            }
        }
        if !saw_fact {
            return None;
        }
        unique_name.map(|class_name| InstanceAttributeClassLookup::Unique {
            field_index: class_index,
            class_name,
        })
    }

    /// Resolves one plain-name receiver to a live class index for a named
    /// attribute chain.
    fn named_attribute_class_index(
        &self,
        occurrence: &OccurrenceFact,
        name: &str,
    ) -> Option<usize> {
        let function_index = self.enclosing_function_index(occurrence)?;
        let function = &self.module.declarations[function_index];
        match self.local_binding_name(function, name, occurrence) {
            LocalBinding::Foreign => return None,
            LocalBinding::Unique(type_name) => return self.unique_live_class_index(&type_name),
            LocalBinding::Absent => {}
        }
        match receiver_annotation_name(function, name) {
            ReceiverAnnotationName::Ambiguous => return None,
            ReceiverAnnotationName::Unique(type_name) => {
                return self.unique_live_class_index(type_name);
            }
            ReceiverAnnotationName::Absent => {}
        }
        match self.module_annotation_name(function, name, occurrence) {
            LocalBinding::Foreign => None,
            LocalBinding::Unique(type_name) => self.unique_live_class_index(&type_name),
            LocalBinding::Absent => {
                let candidates = self.live_plain_class_indices(name);
                if candidates.len() == 1 {
                    Some(candidates[0])
                } else {
                    None
                }
            }
        }
    }

    /// The class name one own field's annotation names, when unique.
    fn field_annotation_class_name_from_index(
        &self,
        field_index: usize,
    ) -> InstanceAttributeClassLookup {
        let declaration = &self.module.declarations[field_index];
        let Some(fact) = self.field_annotation(declaration) else {
            return InstanceAttributeClassLookup::Unannotated;
        };
        match classify_receiver_annotation(&fact.annotation) {
            ReceiverAnnotationName::Unique(name) => InstanceAttributeClassLookup::Unique {
                field_index,
                class_name: name.to_owned(),
            },
            ReceiverAnnotationName::Ambiguous | ReceiverAnnotationName::Absent => {
                InstanceAttributeClassLookup::Unannotated
            }
        }
    }

    /// Resolves one inherited field's annotation class name by walking simple
    /// same-file base classes for a field named `attribute`.
    fn inherited_field_annotation_class_name(
        &self,
        class_index: usize,
        attribute: &str,
    ) -> InstanceAttributeClassLookup {
        self.inherited_field_annotation_from_bases(class_index, attribute, 0)
    }

    fn inherited_field_annotation_from_bases(
        &self,
        class_index: usize,
        attribute: &str,
        depth: usize,
    ) -> InstanceAttributeClassLookup {
        let declaration = &self.module.declarations[class_index];
        let mut base_results = Vec::new();
        for base in &declaration.bases {
            let Some(base_name) = simple_base_name(base) else {
                continue;
            };
            let Some(base_index) = self.unique_live_class_index(base_name) else {
                continue;
            };
            if depth >= MAX_INHERITED_BASE_LINKS {
                continue;
            }
            let mut visited = HashSet::new();
            visited.insert(class_index);
            let result = self.inherited_field_annotation_in_class(
                base_index,
                attribute,
                depth + 1,
                &mut visited,
            );
            if result != InstanceAttributeClassLookup::Absent {
                base_results.push(result);
            }
        }
        merge_instance_attribute_class_results(base_results)
    }

    fn inherited_field_annotation_in_class(
        &self,
        class_index: usize,
        attribute: &str,
        depth: usize,
        visited: &mut HashSet<usize>,
    ) -> InstanceAttributeClassLookup {
        if !visited.insert(class_index) {
            return InstanceAttributeClassLookup::Absent;
        }
        let class_span = self.module.declarations[class_index].span;
        let own_fields = self.own_field_indices_named(class_span, attribute);
        match own_fields.len() {
            0 => self.inherited_field_annotation_from_bases_with_visited(
                class_index,
                attribute,
                depth,
                visited,
            ),
            1 => self.field_annotation_class_name_from_index(own_fields[0]),
            _ => InstanceAttributeClassLookup::Ambiguous,
        }
    }

    fn inherited_field_annotation_from_bases_with_visited(
        &self,
        class_index: usize,
        attribute: &str,
        depth: usize,
        visited: &mut HashSet<usize>,
    ) -> InstanceAttributeClassLookup {
        let declaration = &self.module.declarations[class_index];
        let mut base_results = Vec::new();
        for base in &declaration.bases {
            let Some(base_name) = simple_base_name(base) else {
                continue;
            };
            let Some(base_index) = self.unique_live_class_index(base_name) else {
                continue;
            };
            if depth >= MAX_INHERITED_BASE_LINKS {
                continue;
            }
            let result = self.inherited_field_annotation_in_class(
                base_index,
                attribute,
                depth + 1,
                visited,
            );
            if result != InstanceAttributeClassLookup::Absent {
                base_results.push(result);
            }
        }
        merge_instance_attribute_class_results(base_results)
    }

    /// Exactly one live class declaration carries `name`, or `None`.
    fn unique_live_class_index(&self, name: &str) -> Option<usize> {
        self.unique_live_class_index_seen(name, &mut HashSet::new())
    }

    /// Class index for `name`, following module-level PEP 695 type aliases when
    /// no live class carries the spelling.
    fn unique_live_class_index_seen(
        &self,
        name: &str,
        seen_aliases: &mut HashSet<String>,
    ) -> Option<usize> {
        let name_bytes = name.as_bytes();
        let mut matches = Vec::new();
        for (index, declaration) in self.module.declarations.iter().enumerate() {
            if declaration.kind == DeclarationKind::Class
                && declaration.name.as_bytes() == name_bytes
                && self.live[index]
            {
                matches.push(index);
            }
        }
        if matches.len() > 1 {
            return None;
        }
        if matches.len() == 1 {
            return Some(matches[0]);
        }
        if !seen_aliases.insert(name.to_owned()) {
            return None;
        }
        match self.unique_type_alias_value(name) {
            AliasValueLookup::Absent | AliasValueLookup::Ambiguous => None,
            AliasValueLookup::Unique(value) => {
                let expanded = self.expand_type_alias(value)?;
                match classify_receiver_annotation(&expanded) {
                    ReceiverAnnotationName::Unique(class_name) => {
                        self.unique_live_class_index_seen(class_name, seen_aliases)
                    }
                    ReceiverAnnotationName::Absent | ReceiverAnnotationName::Ambiguous => None,
                }
            }
        }
    }

    /// Live classes of `name`. A class spelling is not expanded as an alias.
    fn live_class_count(&self, name: &str) -> usize {
        let name_bytes = name.as_bytes();
        self.module
            .declarations
            .iter()
            .enumerate()
            .filter(|(index, declaration)| {
                declaration.kind == DeclarationKind::Class
                    && declaration.name.as_bytes() == name_bytes
                    && self.live[*index]
            })
            .count()
    }

    /// Whether one live module-level PEP 695 `type` alias names `name`.
    fn unique_type_alias_value(&self, name: &str) -> AliasValueLookup<'a> {
        let name_bytes = name.as_bytes();
        let mut matches = Vec::new();
        for (index, declaration) in self.module.declarations.iter().enumerate() {
            if declaration.kind == DeclarationKind::Alias
                && declaration.name.as_bytes() == name_bytes
                && declaration.value_span.is_none()
                && self.live[index]
            {
                matches.push(index);
            }
        }
        if matches.is_empty() {
            return AliasValueLookup::Absent;
        }
        if matches.len() > 1 {
            return AliasValueLookup::Ambiguous;
        }
        let module_alias_values: Vec<&AnnotationFact> = self
            .module
            .annotations
            .iter()
            .filter(|fact| {
                fact.position == AnnotationPosition::AliasValue && fact.owner == name
            })
            .collect();
        if module_alias_values.len() != 1 {
            return AliasValueLookup::Ambiguous;
        }
        let declaration = &self.module.declarations[matches[0]];
        if !span_contains(declaration.span, module_alias_values[0].span) {
            return AliasValueLookup::Ambiguous;
        }
        AliasValueLookup::Unique(&module_alias_values[0].annotation)
    }

    /// Expands module-level PEP 695 type aliases inside one annotation.
    fn expand_type_alias(&self, annotation: &Annotation) -> Option<Annotation> {
        self.expand_type_alias_seen(annotation, &mut HashSet::new())
    }

    fn expand_type_alias_seen(
        &self,
        annotation: &Annotation,
        seen: &mut HashSet<String>,
    ) -> Option<Annotation> {
        match annotation {
            Annotation::Name { name, .. } => {
                if name.contains('.') || self.live_class_count(name) > 0 {
                    return Some(annotation.clone());
                }
                match self.unique_type_alias_value(name) {
                    AliasValueLookup::Absent => Some(annotation.clone()),
                    AliasValueLookup::Ambiguous => None,
                    AliasValueLookup::Unique(value) => {
                        if !seen.insert(name.clone()) {
                            return None;
                        }
                        self.expand_type_alias_seen(value, seen)
                    }
                }
            }
            Annotation::Generic { base, args } => {
                let expanded_base = self.expand_type_alias_seen(base, seen)?;
                let expanded_args = args
                    .iter()
                    .map(|arg| self.expand_type_alias_seen(arg, seen))
                    .collect::<Option<Vec<Annotation>>>()?;
                Some(Annotation::Generic {
                    base: Box::new(expanded_base),
                    args: expanded_args,
                })
            }
            Annotation::Union(members) => {
                let expanded = members
                    .iter()
                    .map(|member| self.expand_type_alias_seen(member, seen))
                    .collect::<Option<Vec<Annotation>>>()?;
                Some(Annotation::Union(expanded))
            }
            Annotation::List(items) => {
                let expanded = items
                    .iter()
                    .map(|item| self.expand_type_alias_seen(item, seen))
                    .collect::<Option<Vec<Annotation>>>()?;
                Some(Annotation::List(expanded))
            }
            Annotation::None
            | Annotation::StringLiteral(_)
            | Annotation::Literal(_)
            | Annotation::Unknown(_) => Some(annotation.clone()),
        }
    }

    /// Builds the C3 linearization of one same-file class, including the class
    /// itself at index zero. Unresolvable bases are skipped; cycles and merge
    /// failures return `None`.
    fn c3_mro(&self, class_index: usize, stack: &mut HashSet<usize>) -> Option<Vec<usize>> {
        if !stack.insert(class_index) {
            return None;
        }
        let declaration = &self.module.declarations[class_index];
        let mut direct_bases = Vec::new();
        for base in &declaration.bases {
            let Some(base_name) = simple_base_name(base) else {
                continue;
            };
            let Some(base_index) = self.unique_live_class_index(base_name) else {
                continue;
            };
            direct_bases.push(base_index);
        }
        let mut base_mros = Vec::with_capacity(direct_bases.len());
        for &base_index in &direct_bases {
            base_mros.push(self.c3_mro(base_index, stack)?);
        }
        let merged = c3_merge(base_mros, direct_bases)?;
        stack.remove(&class_index);
        let mut mro = Vec::with_capacity(1 + merged.len());
        mro.push(class_index);
        mro.extend(merged);
        Some(mro)
    }

    /// Resolves one `super()` member by walking the C3 MRO after
    /// `start_after`. Only indexes `1..=MAX_INHERITED_BASE_LINKS` are searched.
    fn super_member_in_mro(
        &self,
        occurrence: &OccurrenceFact,
        mro: &[usize],
        start_after: usize,
        member_kind: DeclarationKind,
    ) -> InheritedMemberLookup {
        let first = start_after + 1;
        if first > MAX_INHERITED_BASE_LINKS {
            return InheritedMemberLookup::Absent;
        }
        let take = MAX_INHERITED_BASE_LINKS - first + 1;
        for &class_index in mro.iter().skip(first).take(take) {
            let class_span = self.module.declarations[class_index].span;
            match self.member_lookup_in_class(occurrence, class_span, member_kind) {
                InheritedMemberLookup::Unique(ordinal) => {
                    return InheritedMemberLookup::Unique(ordinal);
                }
                InheritedMemberLookup::Ambiguous => return InheritedMemberLookup::Ambiguous,
                InheritedMemberLookup::Absent => {}
            }
        }
        InheritedMemberLookup::Absent
    }

    /// Honest foreign key for one unresolved `super()` site.
    fn super_foreign_target(
        &self,
        occurrence: &OccurrenceFact,
    ) -> Result<OccurrenceTarget<'source>, PythonCollectError> {
        match occurrence.kind {
            OccurrenceKind::MethodCall | OccurrenceKind::FunctionCall => {
                foreign_method(self.slice(occurrence.span)?, occurrence.span)
            }
            OccurrenceKind::AttributeRead => {
                foreign_field(self.slice(occurrence.span)?, occurrence.span)
            }
        }
    }

    /// Lowers one checker-inferred type to its root record and the ordered
    /// row coordinates of its children. The root record sits directly on
    /// the owning fact; nested compounds consume pooled anonymous rows.
    /// A `Named` spelling that resolves to a module class becomes that
    /// class's nominal row; any other named spelling has no borrowed bytes
    /// to own, so the honest gap names the oracle. A tuple or union wider
    /// than one type-child row is folded into same-tag chunks so every
    /// member stays reachable. A callable wider than that lane answers
    /// `None` — nesting function pointers would invent a different type.
    fn inferred_root(
        &mut self,
        inferred: &InferredType,
        tables: &TypeTables<'source>,
    ) -> Result<Option<(SemanticTypeRecord<'source>, Vec<u32>)>, PythonCollectError> {
        // Only a compound needs an anchor: its pooled child rows are owned
        // by an already-pushed fact. A scalar or local nominal is the root
        // record itself, so a module whose first declaration is an inferred
        // constant (`inferred = 1`) still takes the checker's answer instead
        // of an oracle gap.
        let anchor = self.anchor();
        match inferred {
            InferredType::Integer => Ok(Some((integer_record(), Vec::new()))),
            InferredType::Float => Ok(Some((float64_record(), Vec::new()))),
            InferredType::Boolean => Ok(Some((
                primitive_record(PrimitiveShape::Bool, 0),
                Vec::new(),
            ))),
            InferredType::Str => Ok(Some((primitive_record(PrimitiveShape::Str, 0), Vec::new()))),
            InferredType::Bytes => Ok(Some((builtin_record(b"bytes"), Vec::new()))),
            InferredType::Complex => Ok(Some((builtin_record(b"complex"), Vec::new()))),
            InferredType::NoneType => Ok(Some((none_record(), Vec::new()))),
            InferredType::List(element) => {
                let Some(anchor) = anchor else {
                    return Ok(None);
                };
                let (base, element) = (builtin_record(b"list"), element.as_deref());
                self.inferred_apply(base, element, tables, anchor)
            }
            InferredType::Set(element) => {
                let Some(anchor) = anchor else {
                    return Ok(None);
                };
                let (base, element) = (builtin_record(b"set"), element.as_deref());
                self.inferred_apply(base, element, tables, anchor)
            }
            InferredType::Dict(pair) => {
                let Some(anchor) = anchor else {
                    return Ok(None);
                };
                let mut children = Vec::new();
                let Some(base_row) = self.leaf_row(builtin_record(b"dict"), anchor)? else {
                    return Ok(None);
                };
                children.push(base_row);
                if let Some((key, value)) = pair.as_ref() {
                    for member in [key.as_ref(), value.as_ref()] {
                        match self.inferred_row(member, tables, anchor)? {
                            Some(row) => children.push(row),
                            None => return Ok(None),
                        }
                    }
                }
                Ok(Some((apply_record(), children)))
            }
            InferredType::Tuple(elements) => {
                let Some(anchor) = anchor else {
                    return Ok(None);
                };
                let mut children = Vec::new();
                for element in elements.as_ref() {
                    match self.inferred_row(element, tables, anchor)? {
                        Some(row) => children.push(row),
                        None => return Ok(None),
                    }
                }
                let Some(children) =
                    self.admit_flat_children(tuple_record(), children, anchor)?
                else {
                    return Ok(None);
                };
                Ok(Some((tuple_record(), children)))
            }
            InferredType::Union(members) => {
                let Some(anchor) = anchor else {
                    return Ok(None);
                };
                let mut children = Vec::new();
                for member in members.as_ref() {
                    match self.inferred_row(member, tables, anchor)? {
                        Some(row) => children.push(row),
                        None => return Ok(None),
                    }
                }
                let Some(children) =
                    self.admit_flat_children(union_record(), children, anchor)?
                else {
                    return Ok(None);
                };
                Ok(Some((union_record(), children)))
            }
            InferredType::Callable { params, result } => {
                let Some(anchor) = anchor else {
                    return Ok(None);
                };
                if params.len().saturating_add(usize::from(result.is_some())) > MAX_TYPE_CHILDREN {
                    return Ok(None);
                }
                let mut children = Vec::new();
                for parameter in params.as_ref() {
                    match self.inferred_row(parameter, tables, anchor)? {
                        Some(row) => children.push(row),
                        None => return Ok(None),
                    }
                }
                let mut payload1 = 0;
                if let Some(result) = result.as_deref() {
                    match self.inferred_row(result, tables, anchor)? {
                        Some(row) => children.push(row),
                        None => return Ok(None),
                    }
                    payload1 = SemanticTypeRecord::FUNCTION_RESULT_COUNT_ONE;
                }
                Ok(Some((function_pointer_record(payload1), children)))
            }
            InferredType::Named(spelling) => {
                match tables
                    .classes
                    .iter()
                    .find(|(known, _)| *known == spelling.as_bytes())
                {
                    // The inferred class is a module declaration: its row is
                    // the legal nominal target.
                    Some((_, ordinal)) => Ok(Some((
                        SemanticTypeRecord {
                            tag: SemanticTypeTag::Nominal,
                            payload0: 0,
                            payload1: 0,
                            text: None,
                            text2: None,
                            nominal: Some(NominalRef::Local(EntityId::new(*ordinal))),
                            children: ListSpan::new(0, 0),
                        },
                        Vec::new(),
                    ))),
                    None => Ok(None),
                }
            }
            InferredType::Any => Ok(None),
        }
    }

    /// One inferred container application over its optional element type.
    fn inferred_apply(
        &mut self,
        base: SemanticTypeRecord<'static>,
        element: Option<&InferredType>,
        tables: &TypeTables<'source>,
        anchor: u32,
    ) -> Result<Option<(SemanticTypeRecord<'source>, Vec<u32>)>, PythonCollectError> {
        let Some(base_row) = self.leaf_row(base, anchor)? else {
            return Ok(None);
        };
        let mut children = vec![base_row];
        if let Some(element) = element {
            match self.inferred_row(element, tables, anchor)? {
                Some(row) => children.push(row),
                None => return Ok(None),
            }
        }
        Ok(Some((apply_record(), children)))
    }

    /// Lowers one inferred type to a pooled row coordinate: a module class
    /// is its fact ordinal, every leaf or nested compound is an interned
    /// anonymous row, and an unhostable member stays `None`.
    fn inferred_row(
        &mut self,
        inferred: &InferredType,
        tables: &TypeTables<'source>,
        anchor: u32,
    ) -> Result<Option<u32>, PythonCollectError> {
        match inferred {
            InferredType::Integer => self.leaf_row(integer_record(), anchor),
            InferredType::Float => self.leaf_row(float64_record(), anchor),
            InferredType::Boolean => {
                self.leaf_row(primitive_record(PrimitiveShape::Bool, 0), anchor)
            }
            InferredType::Str => self.leaf_row(primitive_record(PrimitiveShape::Str, 0), anchor),
            InferredType::Bytes => self.leaf_row(builtin_record(b"bytes"), anchor),
            InferredType::Complex => self.leaf_row(builtin_record(b"complex"), anchor),
            InferredType::NoneType => self.leaf_row(none_record(), anchor),
            InferredType::Named(spelling) => match tables
                .classes
                .iter()
                .find(|(known, _)| *known == spelling.as_bytes())
            {
                Some((_, ordinal)) => Ok(Some(*ordinal)),
                None => Ok(None),
            },
            InferredType::List(_) | InferredType::Set(_) | InferredType::Dict(_) => {
                match self.inferred_root(inferred, tables)? {
                    Some((record, children)) => self.parent_row(record, &children, anchor),
                    None => Ok(None),
                }
            }
            InferredType::Tuple(_) | InferredType::Union(_) | InferredType::Callable { .. } => {
                match self.inferred_root(inferred, tables)? {
                    Some((record, children)) => self.parent_row(record, &children, anchor),
                    None => Ok(None),
                }
            }
            InferredType::Any => Ok(None),
        }
    }

    /// Streams every declaration docstring as borrowed doc fragments.
    fn emit_docs(&mut self) -> Result<(), PythonCollectError> {
        for (index, declaration) in self.module.declarations.iter().enumerate() {
            let Some(ordinal) = self.ordinals[index] else {
                // The module row is not a lane fact; its documentation has
                // no honest owner and is never synthesized onto another row.
                continue;
            };
            // Ruff's declaration row owns an explicit docstring field; `None`
            // is an authority-backed empty documentation value for this row.
            self.facts
                .mark_documentation_captured(ordinal)
                .map_err(lane_rejected)?;
            let Some(docstring) = &declaration.docstring else {
                continue;
            };
            for fragment in doc_fragments(self.source, docstring, &self.pushed)? {
                self.facts
                    .push_doc(ordinal, fragment)
                    .map_err(lane_rejected)?;
            }
        }
        Ok(())
    }
}

/// Maps any bounded-lane rejection onto the exact closed lowering terminal.
fn lane_rejected<F>(_fault: F) -> PythonCollectError {
    PythonCollectError::Lowering(LoweringUnsupported::NoSupportedDeclaration)
}

/// Maps a lexical-containment binding failure onto its exact closed terminal.
fn parentage_fault<F>(start: u32, end: u32, _fault: F) -> PythonCollectError {
    PythonCollectError::Projection(PythonProjectionFault::Containment { start, end })
}

/// Picks the innermost pushed row that owns an occurrence: the name must
/// match and the live declaration must fully contain the reference, so the
/// owner-relative span always lands inside its owner. Start-only matching
/// could re-home a reference from a shadowed (dropped) scope onto an earlier
/// same-name row that does not contain it, which the image build honestly
/// rejects as an escaping occurrence span; without a containing live owner
/// the reference has no honest lane owner and is skipped, never synthesized.
fn owner_row<'rows, 'source>(
    rows: &'rows [Pushed<'source>],
    occurrence: &OccurrenceFact,
) -> Option<&'rows Pushed<'source>> {
    let owner_bytes = occurrence.owner.as_bytes();
    rows.iter()
        .filter(|row| row.name == owner_bytes && span_contains(row.span, occurrence.span))
        .max_by_key(|row| row.span.start)
}

/// Projects an absolute occurrence span onto its owner's span start. The
/// candidate filter proved `owner.start <= occurrence.start`, and Ruff
/// ranges are ordered, so both subtractions stay in range.
fn owner_relative_span(owner: &Pushed<'_>, occurrence: &OccurrenceFact) -> RelSpan {
    let start = occurrence.span.start - owner.span.start;
    let end = occurrence.span.end - owner.span.start;
    RelSpan::new_trusted(start, end)
}

/// The source span carrying one occurrence's written target key. A bare-name
/// call spans exactly its name; a module-gated attribute call spans the whole
/// receiver-qualified callee (`pp.Word`), so its key spelling is the tail of
/// that span. When the tail cannot be proven inside the span, the whole span
/// is kept — the documented borrowed-spelling limitation — never a guessed
/// slice.
fn target_spelling_span(occurrence: &OccurrenceFact) -> Span {
    let target_width = u32::try_from(occurrence.target.len()).unwrap_or(u32::MAX);
    match occurrence.span.end.checked_sub(target_width) {
        Some(start) if start >= occurrence.span.start => Span {
            start,
            end: occurrence.span.end,
        },
        _ => occurrence.span,
    }
}

/// The total, name-preserving reference-kind mapping. The extractor's closed
/// call lattice and the lane's reference lattice share both categories, so
/// no two extractor kinds collapse and no lane kind is unreachable.
fn reference_kind(kind: backend_frontend_python::legacy::OccurrenceKind) -> ReferenceKind {
    match kind {
        backend_frontend_python::legacy::OccurrenceKind::FunctionCall => {
            ReferenceKind::FunctionCall
        }
        backend_frontend_python::legacy::OccurrenceKind::MethodCall => ReferenceKind::MethodCall,
        backend_frontend_python::legacy::OccurrenceKind::AttributeRead => {
            ReferenceKind::FieldAccess
        }
    }
}

/// The total, name-preserving parameter-convention mapping. The extractor's
/// closed parameter lattice and the lane's Python convention lattice share
/// every category, so no two conventions collapse and none is unreachable.
const fn parameter_convention(kind: ParameterKind) -> PythonParameterKind {
    match kind {
        ParameterKind::PositionalOnly => PythonParameterKind::PositionalOnly,
        ParameterKind::PositionalOrKeyword => PythonParameterKind::PositionalOrKeyword,
        ParameterKind::VarArgs => PythonParameterKind::VariadicPositional,
        ParameterKind::KeywordOnly => PythonParameterKind::KeywordOnly,
        ParameterKind::KwArgs => PythonParameterKind::VariadicKeyword,
    }
}

/// The declaration's dynamic-confidence tier for a Ruff-proven type state: a
/// resolved annotation is the lane's index tier; pure surface syntax stays
/// `Syntactic`. Checker-supplied facts use `Compiler` directly.
const fn lowered_tier(resolved: bool) -> Confidence {
    if resolved {
        Confidence::Indexed
    } else {
        Confidence::Syntactic
    }
}

/// The strongest tier of one declaration's parts: a checker-supplied part
/// dominates, then syntax-proven resolution, then pure surface syntax.
const fn combined_tier(any_checked: bool, any_resolved: bool) -> Confidence {
    if any_checked {
        Confidence::Compiler
    } else {
        lowered_tier(any_resolved)
    }
}

/// True when `outer` fully contains `inner`.
const fn span_contains(outer: Span, inner: Span) -> bool {
    outer.start <= inner.start && inner.end <= outer.end
}

/// Maximum base-class links followed while resolving one inherited member.
const MAX_INHERITED_BASE_LINKS: usize = 8;

/// One inherited-member lookup across same-file base classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InheritedMemberLookup {
    Absent,
    Unique(u32),
    Ambiguous,
}

/// One instance-attribute field-annotation lookup across same-file classes.
#[derive(Debug, Clone, PartialEq, Eq)]
enum InstanceAttributeClassLookup {
    Absent,
    Unique {
        field_index: usize,
        class_name: String,
    },
    Ambiguous,
    Unannotated,
}

/// The attribute token inside a receiver-qualified callee span (`Child.note`
/// → `note`). A span that is already the attribute token is unchanged.
fn attribute_token_span(
    text_bytes: &[u8],
    occurrence: &OccurrenceFact,
) -> Result<Span, PythonCollectError> {
    let text = core::str::from_utf8(text_bytes).map_err(|_| {
        PythonCollectError::Projection(PythonProjectionFault::ForeignSpellingUtf8 {
            start: occurrence.span.start,
            end: occurrence.span.end,
        })
    })?;
    let Some(dot) = text.rfind('.') else {
        return Ok(occurrence.span);
    };
    let right = &text[dot + 1..];
    let trimmed = right.trim();
    if trimmed != occurrence.target.as_str() {
        return Ok(occurrence.span);
    }
    let leading = right.len() - right.trim_start().len();
    let Ok(tail) = u32::try_from(dot + 1 + leading) else {
        return Ok(occurrence.span);
    };
    let Ok(width) = u32::try_from(trimmed.len()) else {
        return Ok(occurrence.span);
    };
    let start = occurrence.span.start.saturating_add(tail);
    let end = start.saturating_add(width);
    if end > occurrence.span.end || start < occurrence.span.start {
        return Ok(occurrence.span);
    }
    Ok(Span { start, end })
}

/// True when `name` is one undotted Python identifier.
fn simple_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

/// The simple undotted class name of one written base annotation, if any.
/// Quoted bases lower to `Name` rows with `span: None` and are ignored.
fn simple_base_name(annotation: &Annotation) -> Option<&str> {
    match annotation {
        Annotation::Name { name, span, .. }
            if !name.contains('.') && span.is_some() =>
        {
            Some(name.as_str())
        }
        Annotation::Generic { base, .. } => simple_base_name(base),
        Annotation::Name { .. }
        | Annotation::List(_)
        | Annotation::StringLiteral(_)
        | Annotation::Union(_)
        | Annotation::Literal(_)
        | Annotation::None
        | Annotation::Unknown(_) => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CallReturnMethodLookup {
    Absent,
    Unique(usize),
    Ambiguous,
}

fn merge_call_return_method_results(
    results: Vec<CallReturnMethodLookup>,
) -> CallReturnMethodLookup {
    let mut unique_index: Option<usize> = None;
    for result in results {
        match result {
            CallReturnMethodLookup::Absent => {}
            CallReturnMethodLookup::Ambiguous => return CallReturnMethodLookup::Ambiguous,
            CallReturnMethodLookup::Unique(index) => match unique_index {
                None => unique_index = Some(index),
                Some(existing) if existing == index => {}
                Some(_) => return CallReturnMethodLookup::Ambiguous,
            },
        }
    }
    unique_index.map_or(CallReturnMethodLookup::Absent, CallReturnMethodLookup::Unique)
}

/// Combines inherited-member results from sibling base classes.
fn merge_inherited_base_results(results: Vec<InheritedMemberLookup>) -> InheritedMemberLookup {
    let mut unique_ordinal: Option<u32> = None;
    for result in results {
        match result {
            InheritedMemberLookup::Absent => {}
            InheritedMemberLookup::Ambiguous => return InheritedMemberLookup::Ambiguous,
            InheritedMemberLookup::Unique(ordinal) => match unique_ordinal {
                None => unique_ordinal = Some(ordinal),
                Some(existing) if existing == ordinal => {}
                Some(_) => return InheritedMemberLookup::Ambiguous,
            },
        }
    }
    unique_ordinal.map_or(InheritedMemberLookup::Absent, InheritedMemberLookup::Unique)
}

/// Combines inherited field-annotation results from sibling base classes.
fn merge_instance_attribute_class_results(
    results: Vec<InstanceAttributeClassLookup>,
) -> InstanceAttributeClassLookup {
    let mut unique_field: Option<usize> = None;
    let mut unique_name: Option<String> = None;
    for result in results {
        match result {
            InstanceAttributeClassLookup::Absent => {}
            InstanceAttributeClassLookup::Ambiguous | InstanceAttributeClassLookup::Unannotated => {
                return result;
            }
            InstanceAttributeClassLookup::Unique {
                field_index,
                class_name,
            } => match unique_field {
                None => {
                    unique_field = Some(field_index);
                    unique_name = Some(class_name);
                }
                Some(existing) if existing == field_index => {}
                Some(_) => return InstanceAttributeClassLookup::Ambiguous,
            },
        }
    }
    match (unique_field, unique_name) {
        (Some(field_index), Some(class_name)) => InstanceAttributeClassLookup::Unique {
            field_index,
            class_name,
        },
        _ => InstanceAttributeClassLookup::Absent,
    }
}

/// Standard C3 merge of base MRO sequences plus the direct-base list.
fn c3_merge(mut sequences: Vec<Vec<usize>>, last: Vec<usize>) -> Option<Vec<usize>> {
    sequences.push(last);
    let mut result = Vec::new();
    loop {
        sequences.retain(|sequence| !sequence.is_empty());
        if sequences.is_empty() {
            break;
        }
        let mut chosen: Option<(usize, usize)> = None;
        'candidate: for (index, sequence) in sequences.iter().enumerate() {
            let head = sequence[0];
            for other in &sequences {
                if other.len() > 1 && other[1..].contains(&head) {
                    continue 'candidate;
                }
            }
            chosen = Some((index, head));
            break;
        }
        let (_, head) = chosen?;
        result.push(head);
        for sequence in &mut sequences {
            if sequence.first() == Some(&head) {
                sequence.remove(0);
            }
        }
    }
    Some(result)
}

/// `self` on an instance method and `cls` on a classmethod are receivers, not
/// parameters. Any other parameter with those names stays.
fn is_receiver_parameter(receiver: ReceiverKind, name: &str) -> bool {
    match receiver {
        ReceiverKind::Plain => name == "self",
        ReceiverKind::ClassMethod => name == "cls",
        ReceiverKind::StaticMethod | ReceiverKind::Property => false,
    }
}

enum LocalBinding {
    /// No local `AnnAssign` of this name in this function before the use.
    Absent,
    /// A local annotation exists but must not bind and must not fall through.
    Foreign,
    Unique(String),
}

/// A raw annotation for a subscripted name, before generic arguments are peeled.
enum RawReceiverAnnotation<'a> {
    /// A function-local `AnnAssign`. Its own store is not a shadow.
    Local(&'a Annotation),
    /// A parameter or enclosing binding. A later assignment of the name shadows it.
    Inherited(&'a Annotation),
    /// A binding exists, but it must not be subscripted and must not fall through.
    Blocked,
    /// No binding. A bare class name may still be `Child[int]`.
    Absent,
}

/// Whether one live module-level PEP 695 `type` alias names a spelling.
enum AliasValueLookup<'a> {
    Unique(&'a Annotation),
    Ambiguous,
    Absent,
}

/// Classification of one receiver parameter annotation for class-name binding.
enum ReceiverAnnotationName<'a> {
    /// No undotted class name is available for candidate lookup.
    Absent,
    /// One undotted class or alias name, including through peeled generics and
    /// a union of one class name with ignored `None` arms.
    Unique(&'a str),
    /// Two or more distinct class names, so binding must not pick one arm.
    Ambiguous,
}

fn optional_or_union_base(name: &str) -> Option<&'static str> {
    if name == "Optional" || name == "typing.Optional" {
        Some("Optional")
    } else if name == "Union" || name == "typing.Union" {
        Some("Union")
    } else {
        None
    }
}

fn is_none_annotation(annotation: &Annotation) -> bool {
    matches!(annotation, Annotation::Name { name, .. } if name == "None")
        || matches!(annotation, Annotation::None)
}

fn strip_optional_layers(annotation: &Annotation) -> &Annotation {
    match annotation {
        Annotation::Generic { base, args } => {
            if let Annotation::Name { name, .. } = base.as_ref() {
                if optional_or_union_base(name) == Some("Optional") && args.len() == 1 {
                    return strip_optional_layers(&args[0]);
                }
            }
            annotation
        }
        Annotation::Union(members) => {
            let mut flattened: Vec<&Annotation> = Vec::new();
            flatten_receiver_union_members(members, &mut flattened);
            let class_members: Vec<&&Annotation> = flattened
                .iter()
                .filter(|member| !is_none_annotation(member))
                .collect();
            if class_members.len() == 1 {
                return strip_optional_layers(class_members[0]);
            }
            annotation
        }
        _ => annotation,
    }
}

fn peel_indexes(annotation: &Annotation, count: usize) -> Option<Annotation> {
    let mut current = annotation.clone();
    for _ in 0..count {
        current = strip_optional_layers(&current).clone();
        match &current {
            Annotation::Generic { base, args } => {
                if let Annotation::Name { name, .. } = base.as_ref() {
                    if optional_or_union_base(name).is_some() {
                        return None;
                    }
                }
                if args.len() != 1 {
                    return None;
                }
                current = args[0].clone();
            }
            _ => return None,
        }
    }
    Some(strip_optional_layers(&current).clone())
}

fn is_subscript_index(step: &AttributeStep) -> bool {
    matches!(step, AttributeStep::Index | AttributeStep::NameIndex)
}

fn split_call_subscript_steps(steps: &[AttributeStep]) -> Option<(usize, Vec<(String, usize)>)> {
    let mut index = 0;
    while index < steps.len() && is_subscript_index(&steps[index]) {
        index += 1;
    }
    let leading_indexes = index;
    let mut groups = Vec::new();
    while index < steps.len() {
        let AttributeStep::Field(field) = &steps[index] else {
            return None;
        };
        index += 1;
        let mut index_count = 0;
        while index < steps.len() && is_subscript_index(&steps[index]) {
            index_count += 1;
            index += 1;
        }
        groups.push((field.clone(), index_count));
    }
    Some((leading_indexes, groups))
}

fn split_subscript_steps(
    root: &AttributeChainRoot,
    steps: &[AttributeStep],
) -> Option<(usize, Vec<(String, usize)>)> {
    let mut index = 0;
    while index < steps.len() && is_subscript_index(&steps[index]) {
        index += 1;
    }
    let leading_indexes = index;
    if matches!(root, AttributeChainRoot::Enclosing { .. }) && leading_indexes > 0 {
        return None;
    }
    let mut groups = Vec::new();
    while index < steps.len() {
        let AttributeStep::Field(field) = &steps[index] else {
            return None;
        };
        index += 1;
        let mut index_count = 0;
        while index < steps.len() && is_subscript_index(&steps[index]) {
            index_count += 1;
            index += 1;
        }
        groups.push((field.clone(), index_count));
    }
    if matches!(root, AttributeChainRoot::Enclosing { .. }) && groups.is_empty() {
        return None;
    }
    Some((leading_indexes, groups))
}

fn parameter_annotation_raw<'a>(
    function: &'a DeclarationFact,
    name: &str,
) -> Option<&'a Annotation> {
    for parameter in &function.parameters {
        if is_receiver_parameter(function.receiver, &parameter.name) {
            continue;
        }
        if parameter.name == name {
            return Some(&parameter.annotation);
        }
    }
    None
}

fn classify_receiver_annotation<'a>(annotation: &'a Annotation) -> ReceiverAnnotationName<'a> {
    match annotation {
        Annotation::None => ReceiverAnnotationName::Absent,
        Annotation::Name { name, .. } => {
            if name.contains('.') {
                ReceiverAnnotationName::Absent
            } else {
                ReceiverAnnotationName::Unique(name.as_str())
            }
        }
        Annotation::Generic { base, args } => {
            if let Annotation::Name { name, .. } = base.as_ref() {
                match optional_or_union_base(name) {
                    Some("Optional") => {
                        if args.len() == 1 {
                            classify_receiver_annotation(&args[0])
                        } else {
                            ReceiverAnnotationName::Absent
                        }
                    }
                    Some("Union") => classify_receiver_union_members(args),
                    _ => classify_receiver_annotation(base.as_ref()),
                }
            } else {
                classify_receiver_annotation(base.as_ref())
            }
        }
        Annotation::Union(members) => classify_receiver_union_members(members),
        Annotation::List(_)
        | Annotation::StringLiteral(_)
        | Annotation::Literal(_)
        | Annotation::Unknown(_) => ReceiverAnnotationName::Absent,
    }
}

fn classify_receiver_union_members<'a>(
    members: &'a [Annotation],
) -> ReceiverAnnotationName<'a> {
    let mut flattened: Vec<&Annotation> = Vec::new();
    flatten_receiver_union_members(members, &mut flattened);
    let mut unique_name: Option<&'a str> = None;
    for member in flattened {
        match classify_receiver_annotation(member) {
            ReceiverAnnotationName::Absent => {}
            ReceiverAnnotationName::Ambiguous => return ReceiverAnnotationName::Ambiguous,
            ReceiverAnnotationName::Unique(name) => {
                if let Some(existing) = unique_name {
                    if existing != name {
                        return ReceiverAnnotationName::Ambiguous;
                    }
                } else {
                    unique_name = Some(name);
                }
            }
        }
    }
    match unique_name {
        Some(name) => ReceiverAnnotationName::Unique(name),
        None => ReceiverAnnotationName::Absent,
    }
}

fn flatten_receiver_union_members<'a>(members: &'a [Annotation], out: &mut Vec<&'a Annotation>) {
    for member in members {
        if let Annotation::Union(inner) = member {
            flatten_receiver_union_members(inner, out);
        } else {
            out.push(member);
        }
    }
}

/// The non-receiver parameter whose name equals `receiver`, when its
/// annotation is a plain undotted name, peels to one through generics, or is
/// a union of one class name.
fn receiver_annotation_name<'a>(
    declaration: &'a DeclarationFact,
    receiver: &str,
) -> ReceiverAnnotationName<'a> {
    for parameter in &declaration.parameters {
        if is_receiver_parameter(declaration.receiver, &parameter.name) {
            continue;
        }
        if parameter.name != receiver {
            continue;
        }
        return classify_receiver_annotation(&parameter.annotation);
    }
    ReceiverAnnotationName::Absent
}

/// Borrowed source span of the module path in one import alias statement.
fn alias_import_module_span(source: &[u8], statement: Span) -> Option<Span> {
    let raw = source.get(statement.start as usize..statement.end as usize)?;
    let (lo, hi) = trim_ascii_bounds(raw);
    if lo >= hi {
        return None;
    }
    let stmt = raw.get(lo..hi)?;
    let base = statement.start + lo as u32;
    if stmt.starts_with(b"from ") {
        let rest = stmt.get(5..)?;
        let (rlo, rhi) = trim_ascii_bounds(rest);
        if rlo >= rhi {
            return None;
        }
        let rest = rest.get(rlo..rhi)?;
        let rest_base = base + 5 + rlo as u32;
        let import_pos = rest
            .windows(8)
            .position(|window| window == b" import ")?;
        let module = rest.get(..import_pos)?;
        let (mlo, mhi) = trim_ascii_bounds(module);
        if mlo >= mhi {
            return None;
        }
        let module = module.get(mlo..mhi)?;
        if module.is_empty() || module[0] == b'.' || module.contains(&b'\n') {
            return None;
        }
        return Some(Span {
            start: rest_base + mlo as u32,
            end: rest_base + mhi as u32,
        });
    }
    if stmt.starts_with(b"import ") {
        let rest = stmt.get(7..)?;
        let (rlo, rhi) = trim_ascii_bounds(rest);
        if rlo >= rhi {
            return None;
        }
        let rest = rest.get(rlo..rhi)?;
        let rest_base = base + 7 + rlo as u32;
        let mut parts: Vec<&[u8]> = Vec::new();
        for part in rest.split(|byte: &u8| byte.is_ascii_whitespace()) {
            if !part.is_empty() {
                parts.push(part);
            }
        }
        if parts.len() != 3 || parts[1] != b"as" {
            return None;
        }
        let module = parts[0];
        if module.is_empty() || module[0] == b'.' || module.contains(&b'\n') {
            return None;
        }
        return Some(Span {
            start: rest_base,
            end: rest_base + module.len() as u32,
        });
    }
    None
}

const fn trim_ascii_bounds(bytes: &[u8]) -> (usize, usize) {
    let mut start = 0;
    while start < bytes.len() && bytes[start].is_ascii_whitespace() {
        start += 1;
    }
    let mut end = bytes.len();
    while end > start && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    (start, end)
}

fn collect_import_bindings(source: &str, module_name: &str) -> (HashMap<String, String>, HashSet<String>) {
    let is_package = module_name.ends_with("__init__");
    let mut bound = HashMap::new();
    let mut ambiguous = HashSet::new();
    for line in source.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("from ") {
            if let Some((origin, names)) = rest.split_once(" import ") {
                let (level, module) = parse_relative_origin(origin);
                let origin = import_origin(module_name, is_package, level, module);
                for part in names.split(',') {
                    let part = part.trim();
                    if part.is_empty() || part == "*" { continue; }
                    let (imported, local) = parse_import_alias(part);
                    let target = if origin.is_empty() { imported.to_owned() } else { format!("{origin}.{imported}") };
                    record_import_target(&mut bound, &mut ambiguous, local, target);
                }
            }
        } else if let Some(rest) = line.strip_prefix("import ") {
            for part in rest.split(',') {
                let part = part.trim();
                if part.is_empty() { continue; }
                let (imported, local) = parse_import_alias(part);
                record_import_target(&mut bound, &mut ambiguous, local, imported.to_owned());
            }
        }
    }
    (bound, ambiguous)
}

fn parse_relative_origin(origin: &str) -> (u32, Option<&str>) {
    let dots = origin.chars().take_while(|c| *c == '.').count();
    let tail = origin[dots..].trim();
    (u32::try_from(dots).unwrap_or(0), if tail.is_empty() { None } else { Some(tail) })
}

fn parse_import_alias(part: &str) -> (&str, &str) {
    let mut words = part.split_whitespace();
    let imported = words.next().unwrap_or(part);
    if words.next() == Some("as") { (imported, words.next().unwrap_or(imported)) } else { (imported, imported.split('.').next().unwrap_or(imported)) }
}

fn import_origin(module_name: &str, is_package: bool, level: u32, module: Option<&str>) -> String {
    if level == 0 { return module.unwrap_or("").to_owned(); }
    let mut parts: Vec<&str> = module_name.split('.').filter(|p| !p.is_empty()).collect();
    if !is_package { parts.pop(); }
    let extra = (level as usize).saturating_sub(1);
    if extra >= parts.len() { parts.clear(); } else if extra > 0 { parts.truncate(parts.len() - extra); }
    let mut origin = parts.join(".");
    if let Some(module) = module.filter(|n| !n.is_empty()) {
        origin = if origin.is_empty() { module.to_owned() } else { format!("{origin}.{module}") };
    }
    origin
}

fn record_import_target(bound: &mut HashMap<String, String>, ambiguous: &mut HashSet<String>, local: &str, target: String) {
    if ambiguous.contains(local) { return; }
    match bound.get(local) {
        Some(existing) if existing == &target => {}
        Some(_) => { bound.remove(local); ambiguous.insert(local.to_owned()); }
        None => { bound.insert(local.to_owned(), target); }
    }
}

fn import_name_bindings(
    source: &str,
    module_name: &str,
    classes: &[(&[u8], u32)],
) -> HashMap<String, BindingResolution> {
    let (import_targets, ambiguous) = collect_import_bindings(source, module_name);
    let mut bound = HashMap::new();
    for (local, target) in import_targets {
        let resolution = import_binding_resolution(&target, classes);
        bound.insert(local, resolution);
    }
    for name in ambiguous {
        bound.insert(name, BindingResolution::Ambiguous);
    }
    bound
}

fn resolve_alias_target(
    bound: &HashMap<String, BindingResolution>,
    classes: &[(&[u8], u32)],
    target: &str,
) -> Option<BindingResolution> {
    match bound.get(target) {
        Some(BindingResolution::Nominal(ordinal)) => Some(BindingResolution::Nominal(*ordinal)),
        Some(BindingResolution::Ambiguous) => Some(BindingResolution::Ambiguous),
        Some(BindingResolution::External) => Some(BindingResolution::External),
        None => class_binding_for_name(classes, target),
    }
}

fn class_binding_for_name(classes: &[(&[u8], u32)], name: &str) -> Option<BindingResolution> {
    classes
        .iter()
        .find(|(known, _)| *known == name.as_bytes())
        .map(|(_, ordinal)| BindingResolution::Nominal(*ordinal))
}

fn apply_type_alias_binding(
    bound: &mut HashMap<String, BindingResolution>,
    locals: &mut HashMap<String, u32>,
    alias_name: &str,
    resolution: Option<BindingResolution>,
) {
    match resolution {
        Some(BindingResolution::Nominal(ordinal)) => {
            record_local_binding(bound, locals, alias_name.as_bytes(), ordinal);
        }
        Some(BindingResolution::Ambiguous) => {
            bound.insert(alias_name.to_owned(), BindingResolution::Ambiguous);
        }
        Some(BindingResolution::External) => {
            bound.insert(alias_name.to_owned(), BindingResolution::External);
        }
        None => {}
    }
}

fn record_local_binding(
    bound: &mut HashMap<String, BindingResolution>,
    locals: &mut HashMap<String, u32>,
    name: &[u8],
    ordinal: u32,
) {
    let name = std::str::from_utf8(name).unwrap_or("");
    if name.is_empty() {
        return;
    }
    if let Some(prev) = locals.get(name) {
        if *prev != ordinal {
            bound.insert(name.to_owned(), BindingResolution::Ambiguous);
        }
        return;
    }
    locals.insert(name.to_owned(), ordinal);
    bound.insert(name.to_owned(), BindingResolution::Nominal(ordinal));
}

fn import_binding_resolution(target: &str, classes: &[(&[u8], u32)]) -> BindingResolution {
    let simple = target.rsplit('.').next().unwrap_or(target);
    let matches: Vec<u32> = classes
        .iter()
        .filter(|(known, _)| *known == simple.as_bytes())
        .map(|(_, ordinal)| *ordinal)
        .collect();
    match matches.len() {
        0 => BindingResolution::External,
        1 => BindingResolution::Nominal(matches[0]),
        _ => BindingResolution::Ambiguous,
    }
}

fn annotation_root_name(annotation: &Annotation) -> Option<String> {
    match annotation { Annotation::Name { name, .. } => Some(name.clone()), _ => None }
}

/// Python legally rebinds a name in the same scope at runtime, but the syntax
/// extractor already drops later module-level rebindings and keeps overload
/// branches distinct. The identity model is coordinate-free, so two
/// byte-identical twins in one scope share one family and one structural
/// variant and the image build rejects the honest duplicate as
/// `DuplicateDeclarationIdentity`. This pass keeps the first live binding per
/// `(owner, kind, name)` signature group instead of minting coordinates into
/// identity.
///
/// Grouping is by lexical owner (the innermost enclosing class or function,
/// or the module root), declaration kind, and name, so two different scopes
/// reusing one name never merge. Within a group, bindings split by a
/// span-erased signature fingerprint that mirrors the variant inputs: arity,
/// parameter conventions, and normalized annotations for functions; the
/// normalized annotation for variables and type aliases; the structural
/// members for `TypedDict`/`Protocol` classes and nothing else for plain
/// classes (whose variant is the bare self-nominal). Distinct overloads keep
/// distinct fingerprints and all survive; byte-identical twins share one
/// fingerprint and only the later (larger `span.start`, ties by index) stays
/// live. A dead binding's whole subtree is dead too: inner declarations
/// owned by a shadowed class or function are dropped with it, as is every
/// parameter and result slot of a dropped function (those rows are only
/// emitted for live functions).
///
/// References resolve through the pushed (live-only) rows by name, so after
/// the collapse every occurrence points at the live binding. For identical
/// twins that redirection is type-identical; an earlier call that textually
/// precedes the shadowing definition therefore keeps its meaning while
/// naming the surviving row.
fn compute_live_set(module: &ModuleFacts) -> Vec<bool> {
    let count = module.declarations.len();
    let owners: Vec<Option<usize>> = (0..count)
        .map(|index| innermost_owner(module, index))
        .collect();
    let mut live = vec![true; count];
    // One group per (owner, kind, name): later bindings shadow earlier ones
    // only inside the same lexical scope.
    let mut groups: std::collections::HashMap<(Option<usize>, u8, &str), Vec<usize>> =
        std::collections::HashMap::new();
    for (index, declaration) in module.declarations.iter().enumerate() {
        if declaration.kind == DeclarationKind::Module {
            continue;
        }
        groups
            .entry((
                owners[index],
                declaration_kind_discriminant(declaration.kind),
                declaration.name.as_str(),
            ))
            .or_default()
            .push(index);
    }
    for indices in groups.values() {
        if indices.len() < 2 {
            continue;
        }
        // Subgroup by the span-erased signature: distinct overloads never
        // share a fingerprint, identical twins always do.
        let mut fingerprints: std::collections::HashMap<String, Vec<usize>> =
            std::collections::HashMap::new();
        for index in indices {
            fingerprints
                .entry(shadowing_fingerprint(module, *index))
                .or_default()
                .push(*index);
        }
        for twins in fingerprints.values() {
            if twins.len() < 2 {
                continue;
            }
            let mut ordered = twins.clone();
            ordered.sort_by_key(|index| (module.declarations[*index].span.start, *index));
            // Keep the first (live) binding; later twins are shadowed.
            for shadowed in &ordered[1..] {
                live[*shadowed] = false;
            }
        }
    }
    // A shadowed class or function takes its whole subtree with it: any
    // declaration owned (transitively) by a dead row is dead. Parents always
    // own strictly larger spans than their children, so one pass from the
    // largest span down propagates the full closure.
    let mut by_area: Vec<usize> = (0..count).collect();
    by_area.sort_by_key(|index| {
        let span = module.declarations[*index].span;
        u64::from(span.end.saturating_sub(span.start))
    });
    for index in by_area.into_iter().rev() {
        if live[index]
            && let Some(owner) = owners[index]
            && !live[owner]
        {
            live[index] = false;
        }
    }
    live
}

/// The innermost enclosing class or function of one declaration, by smallest
/// strictly containing span. `None` is the module root. This mirrors the
/// parentage pass exactly (strict containment, smallest area wins) so the
/// shadowing groups and the emitted parentage agree on every scope.
fn innermost_owner(module: &ModuleFacts, index: usize) -> Option<usize> {
    let span = module.declarations[index].span;
    let mut owner: Option<usize> = None;
    let mut owner_area = u64::MAX;
    for (candidate, declaration) in module.declarations.iter().enumerate() {
        if candidate == index {
            continue;
        }
        if !matches!(
            declaration.kind,
            DeclarationKind::Class | DeclarationKind::Function
        ) {
            continue;
        }
        if !span_contains(declaration.span, span) {
            continue;
        }
        if declaration.span.start == span.start && declaration.span.end == span.end {
            continue;
        }
        let area = u64::from(declaration.span.end.saturating_sub(declaration.span.start));
        if area < owner_area {
            owner_area = area;
            owner = Some(candidate);
        }
    }
    owner
}

/// Closed kind discriminant for the shadowing groups. `Field` and `Constant`
/// stay distinct kinds (they already map to distinct entity kinds), so a
/// field and a constant sharing one name never merge.
fn declaration_kind_discriminant(kind: DeclarationKind) -> u8 {
    match kind {
        DeclarationKind::Module => 0,
        DeclarationKind::Class => 1,
        DeclarationKind::Function => 2,
        DeclarationKind::Field => 3,
        DeclarationKind::Constant => 4,
        DeclarationKind::Alias => 5,
    }
}

/// The span-erased signature fingerprint of one declaration: equal
/// fingerprints imply equal structural variants, so only true twins collapse.
/// Spans never enter the key (twins live at different coordinates by
/// definition); parameter names, defaults, decorators, receivers, async
/// markers, and docstrings never enter either, exactly like the variant frame
/// ignores them.
fn shadowing_fingerprint(module: &ModuleFacts, index: usize) -> String {
    let declaration = &module.declarations[index];
    match declaration.kind {
        DeclarationKind::Function => function_fingerprint(module, index),
        DeclarationKind::Field | DeclarationKind::Constant => {
            match field_annotation_for(module, declaration) {
                Some(found) => format!("var:{:?}", normalized_annotation(&found.annotation)),
                None => "var:unannotated".to_string(),
            }
        }
        DeclarationKind::Alias => {
            let is_type_alias =
                declaration.value_span.is_none() && declaration.value_source.is_some();
            if is_type_alias && let Some(found) = alias_value_annotation_for(module, declaration) {
                format!("alias:{:?}", normalized_annotation(&found.annotation))
            } else {
                // Import bindings all lower to the same honest unknown row
                // regardless of the imported module spelling, so every same
                // name import shares one fingerprint and the later wins.
                "alias:import".to_string()
            }
        }
        DeclarationKind::Class => class_fingerprint(module, index),
        DeclarationKind::Module => "module".to_string(),
    }
}

/// Span-erased function signature: parameter conventions plus normalized
/// parameter and return annotations, in order. This mirrors the function
/// variant (parameter conventions plus type children) while ignoring the
/// parameter names the variant never frames.
fn function_fingerprint(module: &ModuleFacts, index: usize) -> String {
    let declaration = &module.declarations[index];
    let mut parts = Vec::with_capacity(declaration.parameters.len());
    for parameter in &declaration.parameters {
        parts.push(format!(
            "{:?}:{:?}",
            parameter.kind,
            normalized_annotation(&parameter.annotation)
        ));
    }
    let returns = return_annotation_for(module, declaration)
        .map(|found| format!("{:?}", normalized_annotation(&found.annotation)))
        .unwrap_or_else(|| "none".to_string());
    format!("fn:[{}] ret({})", parts.join(","), returns)
}

/// Span-erased class fingerprint. Plain classes lower to the bare
/// self-nominal, so every same-name plain class shares one fingerprint and
/// the later wins regardless of bases or decorators (which the variant never
/// frames). Structural classes carry their members as type children, so their
/// member signatures join the key and distinctly-shaped records survive.
fn class_fingerprint(module: &ModuleFacts, index: usize) -> String {
    let declaration = &module.declarations[index];
    match declaration.class_form {
        Some(ClassForm::TypedDict) | Some(ClassForm::Protocol) => {
            let mut bases = Vec::new();
            for base in &declaration.bases {
                bases.push(format!("{:?}", normalized_annotation(base)));
            }
            let mut members = Vec::new();
            for member_index in structural_member_indices_for(module, declaration) {
                let member = &module.declarations[member_index];
                let key = match member.kind {
                    DeclarationKind::Function => function_fingerprint(module, member_index),
                    DeclarationKind::Field | DeclarationKind::Constant => {
                        match field_annotation_for(module, member) {
                            Some(found) => {
                                format!("{:?}", normalized_annotation(&found.annotation))
                            }
                            None => "unannotated".to_string(),
                        }
                    }
                    _ => format!("{:?}:{}", member.kind, member.name),
                };
                members.push(format!("{}:{}", member.name, key));
            }
            format!(
                "structural:{:?}|bases:[{}]|total:{:?}|members:[{}]",
                declaration.class_form,
                bases.join(","),
                declaration.total,
                members.join(",")
            )
        }
        _ => "plain-class".to_string(),
    }
}

/// The direct structural members of one class, in declaration order, ignoring
/// nested classes: fields for a `TypedDict`, functions otherwise. Mirrors the
/// emitter's member walk so fingerprints and emitted records agree.
fn structural_member_indices_for(module: &ModuleFacts, class: &DeclarationFact) -> Vec<usize> {
    let wanted_kind = match class.class_form {
        Some(ClassForm::TypedDict) => DeclarationKind::Field,
        _ => DeclarationKind::Function,
    };
    module
        .declarations
        .iter()
        .enumerate()
        .filter(|(_, candidate)| {
            candidate.kind == wanted_kind
                && span_contains(class.span, candidate.span)
                && module.declarations.iter().all(|other| {
                    other.kind != DeclarationKind::Class
                        || other.span == class.span
                        || !span_contains(other.span, candidate.span)
                })
        })
        .map(|(index, _)| index)
        .collect()
}

/// One annotation with every source coordinate erased: leaf name spans become
/// `None` and unsupported-syntax reasons keep only their syntax kind. Twins
/// at different file offsets therefore fingerprint identically, while
/// structurally different annotations never do. A `Literal[...]` member
/// widens exactly like the lane lowers it (one literal to its base
/// primitive, several to their union), because the lane frames the widened
/// row: `Literal[True]` and a bare `bool` are one fingerprint, as they are
/// one variant. An unwidenable literal keeps its erased form.
fn normalized_annotation(annotation: &Annotation) -> Annotation {
    match annotation {
        Annotation::Name { name, .. } => {
            // The lane lowers each `typing.X` spelling exactly like its bare
            // `X` twin (`Any`, `Callable`, `Optional`, `Union`, `list`,
            // `dict`, `set`, `frozenset`, `tuple`, `NotRequired`,
            // `Required`), so fingerprints strip the prefix to mirror the
            // variant. Import aliases (`t.Any`) stay distinct spellings with
            // distinct variants, exactly like the lane treats them.
            let canonical = name.strip_prefix("typing.").unwrap_or(name);
            Annotation::Name {
                name: canonical.to_string(),
                span: None,
            }
        }
        Annotation::Generic { base, args } => Annotation::Generic {
            base: Box::new(normalized_annotation(base)),
            args: args.iter().map(normalized_annotation).collect(),
        },
        Annotation::List(items) => {
            Annotation::List(items.iter().map(normalized_annotation).collect())
        }
        Annotation::StringLiteral(value) => Annotation::StringLiteral(value.clone()),
        Annotation::Union(members) => {
            Annotation::Union(members.iter().map(normalized_annotation).collect())
        }
        Annotation::Literal(values) => widened_literal_fingerprint(values),
        Annotation::None => Annotation::None,
        Annotation::Unknown(reason) => Annotation::Unknown(normalized_reason(reason)),
    }
}

/// One `Literal[...]` annotation widened to its fingerprint form, mirroring
/// the lane's frozen widening law: every widenable member becomes its base
/// primitive (`str`, `int`, `float`, `complex`, `bool`, or `None`), distinct
/// members deduplicate, one member stays a leaf, and several become a union.
/// Ellipsis or unsupported members cannot widen, so the literal keeps its
/// span-erased form and only byte-identical twins merge.
fn widened_literal_fingerprint(values: &[LiteralValue]) -> Annotation {
    let mut widened: Vec<Annotation> = Vec::new();
    for value in values {
        let Some(mapped) = widened_literal_member(value) else {
            return Annotation::Literal(values.to_vec());
        };
        if !widened.contains(&mapped) {
            widened.push(mapped);
        }
    }
    match widened.as_slice() {
        [] => Annotation::Literal(values.to_vec()),
        [single] => single.clone(),
        _ => Annotation::Union(widened),
    }
}

/// One literal member widened to its base-primitive fingerprint, or `None`
/// when the widening law cannot name it.
fn widened_literal_member(value: &LiteralValue) -> Option<Annotation> {
    let name = match value {
        LiteralValue::String(_) => "str",
        LiteralValue::Integer(_) => "int",
        LiteralValue::Float { .. } => "float",
        LiteralValue::Complex { .. } => "complex",
        LiteralValue::Boolean(_) => "bool",
        LiteralValue::None => return Some(Annotation::None),
        LiteralValue::Ellipsis | LiteralValue::Unsupported(_) => return None,
    };
    Some(Annotation::Name {
        name: name.to_string(),
        span: None,
    })
}

/// One extractor reason with its span erased. `Unannotated` keeps its
/// position (a parameter and a field are never confused); unsupported syntax
/// keeps only its syntax kind.
fn normalized_reason(reason: &ExtractedReason) -> ExtractedReason {
    match reason {
        ExtractedReason::Unannotated { position } => ExtractedReason::Unannotated {
            position: *position,
        },
        ExtractedReason::UnsupportedSyntax { kind, .. } => ExtractedReason::UnsupportedSyntax {
            kind: *kind,
            span: Span { start: 0, end: 0 },
        },
        ExtractedReason::TruncatedAtDepthLimit => ExtractedReason::TruncatedAtDepthLimit,
    }
}

/// The return annotation of one function, by exact owner name and span
/// containment, so overloaded names never swap annotations. Free function
/// twin of the emitter method for use before the emitter exists.
fn return_annotation_for<'m>(
    module: &'m ModuleFacts,
    declaration: &DeclarationFact,
) -> Option<&'m AnnotationFact> {
    module.annotations.iter().find(|candidate| {
        candidate.position == AnnotationPosition::Return
            && candidate.owner == declaration.name
            && span_contains(declaration.span, candidate.span)
    })
}

/// The annotation of one field or constant, matched the same way.
fn field_annotation_for<'m>(
    module: &'m ModuleFacts,
    declaration: &DeclarationFact,
) -> Option<&'m AnnotationFact> {
    module.annotations.iter().find(|candidate| {
        candidate.position == AnnotationPosition::Field
            && candidate.owner == declaration.name
            && span_contains(declaration.span, candidate.span)
    })
}

/// The written value annotation of one PEP 695 `type` alias, matched by exact
/// owner name and span containment.
fn alias_value_annotation_for<'m>(
    module: &'m ModuleFacts,
    declaration: &DeclarationFact,
) -> Option<&'m AnnotationFact> {
    module.annotations.iter().find(|candidate| {
        candidate.position == AnnotationPosition::AliasValue
            && candidate.owner == declaration.name
            && span_contains(declaration.span, candidate.span)
    })
}

/// The integer row for a bare `int`: arbitrary precision, not a platform
/// word. Python's source spelling must never be rendered as `isize`/`nint`.
fn integer_record() -> SemanticTypeRecord<'static> {
    primitive_record(PrimitiveShape::ArbitraryInteger, 0)
}

/// The float row for a bare `float`: CPython's IEEE-754 double.
fn float64_record() -> SemanticTypeRecord<'static> {
    primitive_record(PrimitiveShape::Float, TypeWidth::Fixed(64).to_cell())
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

/// One builtin row whose text cell owns the exact spelling.
#[expect(
    clippy::as_conversions,
    reason = "the lattice freezes PrimitiveShape as a repr(u32) wire discriminant"
)]
fn builtin_record(name: &'static [u8]) -> SemanticTypeRecord<'static> {
    SemanticTypeRecord {
        tag: SemanticTypeTag::Primitive,
        payload0: PrimitiveShape::Builtin as u32,
        payload1: 0,
        text: Some(name),
        text2: None,
        nominal: None,
        children: ListSpan::new(0, 0),
    }
}

/// The `None` builtin row.
fn none_record() -> SemanticTypeRecord<'static> {
    builtin_record(b"None")
}

/// One tuple row over its ordered element coordinates.
fn tuple_record() -> SemanticTypeRecord<'static> {
    SemanticTypeRecord {
        tag: SemanticTypeTag::Tuple,
        payload0: 0,
        payload1: 0,
        text: None,
        text2: None,
        nominal: None,
        children: ListSpan::new(0, 0),
    }
}

/// One union row over its ordered member coordinates.
const fn union_record() -> SemanticTypeRecord<'static> {
    SemanticTypeRecord {
        tag: SemanticTypeTag::Union,
        payload0: 0,
        payload1: 0,
        text: None,
        text2: None,
        nominal: None,
        children: ListSpan::new(0, 0),
    }
}

/// One generic-application row over its base and argument coordinates.
const fn apply_record() -> SemanticTypeRecord<'static> {
    SemanticTypeRecord {
        tag: SemanticTypeTag::Apply,
        payload0: 0,
        payload1: 0,
        text: None,
        text2: None,
        nominal: None,
        children: ListSpan::new(0, 0),
    }
}

/// One callable row: parameters then the optional result child, with the
/// result-presence flag packed into the reserved payload cell.
const fn function_pointer_record(payload1: u32) -> SemanticTypeRecord<'static> {
    SemanticTypeRecord {
        tag: SemanticTypeTag::FunctionPointer,
        payload0: 0,
        payload1,
        text: None,
        text2: None,
        nominal: None,
        children: ListSpan::new(0, 0),
    }
}

/// One type-variable use row whose text cell owns the written spelling.
fn typevar_record<'source>(text: &'source [u8]) -> SemanticTypeRecord<'source> {
    SemanticTypeRecord {
        tag: SemanticTypeTag::TypeVar,
        payload0: 0,
        payload1: 0,
        text: Some(text),
        text2: None,
        nominal: None,
        children: ListSpan::new(0, 0),
    }
}

/// One honest unknown row whose reason carries no spelling.
#[expect(
    clippy::as_conversions,
    reason = "the lattice freezes TypeReason as a repr(u32) wire discriminant"
)]
fn unknown_record(reason: TypeReason) -> SemanticTypeRecord<'static> {
    SemanticTypeRecord {
        tag: SemanticTypeTag::Unknown,
        payload0: reason as u32,
        payload1: 0,
        text: None,
        text2: None,
        nominal: None,
        children: ListSpan::new(0, 0),
    }
}

/// One honest unknown row for a reason that demands its exact spelling.
#[expect(
    clippy::as_conversions,
    reason = "the lattice freezes TypeReason as a repr(u32) wire discriminant"
)]
fn spelled_unknown<'source>(
    reason: TypeReason,
    text: &'source [u8],
) -> SemanticTypeRecord<'source> {
    SemanticTypeRecord {
        tag: SemanticTypeTag::Unknown,
        payload0: reason as u32,
        payload1: 0,
        text: Some(text),
        text2: None,
        nominal: None,
        children: ListSpan::new(0, 0),
    }
}

/// The base-primitive row one literal member widens to, or `None` when the
/// widening law cannot name it (ellipsis, unsupported literal syntax).
fn widened_literal_record(value: &LiteralValue) -> Option<SemanticTypeRecord<'static>> {
    match value {
        LiteralValue::String(_) => Some(primitive_record(PrimitiveShape::Str, 0)),
        LiteralValue::Integer(_) => Some(integer_record()),
        LiteralValue::Float { .. } => Some(float64_record()),
        LiteralValue::Complex { .. } => Some(builtin_record(b"complex")),
        LiteralValue::Boolean(_) => Some(primitive_record(PrimitiveShape::Bool, 0)),
        LiteralValue::None => Some(none_record()),
        LiteralValue::Ellipsis | LiteralValue::Unsupported(_) => None,
    }
}

/// A foreign key under the module's `pypi` package lineage: the lineage
/// names the package's first path segment, the path keeps the dotted module
/// spelling (the lane borrows source bytes only, so no re-slashed copy is
/// synthesized), and the display is the local binding.
fn foreign_package<'source>(
    module_spelling: &'source [u8],
    display: &'source str,
    spelling_span: Span,
    kind: Option<EntityKind>,
) -> Result<OccurrenceTarget<'source>, PythonCollectError> {
    let path = core::str::from_utf8(module_spelling).map_err(|_| {
        PythonCollectError::Projection(PythonProjectionFault::ForeignSpellingUtf8 {
            start: spelling_span.start,
            end: spelling_span.end,
        })
    })?;
    let name = match path.split_once('.') {
        Some((head, _)) => head,
        None => path,
    };
    let lineage =
        PackageLineage::new("pypi", name).map_err(|cause| lineage_fault(cause, spelling_span))?;
    let key = ForeignKey::new(ForeignOrigin::Package(lineage), path, display, kind)
        .map_err(|cause| foreign_key_fault(cause, spelling_span))?;
    Ok(OccurrenceTarget::Foreign(key))
}

/// A foreign key outside every package, carrying the written spelling.
fn foreign_universe<'source>(
    written: &'source [u8],
    spelling_span: Span,
) -> Result<OccurrenceTarget<'source>, PythonCollectError> {
    let path = core::str::from_utf8(written).map_err(|_| {
        PythonCollectError::Projection(PythonProjectionFault::ForeignSpellingUtf8 {
            start: spelling_span.start,
            end: spelling_span.end,
        })
    })?;
    let key = ForeignKey::new(
        ForeignOrigin::Universe { ecosystem: "pypi" },
        path,
        path,
        None,
    )
    .map_err(|cause| foreign_key_fault(cause, spelling_span))?;
    Ok(OccurrenceTarget::Foreign(key))
}

/// The honest typed foreign key for one unresolved method call: the exact
/// written attribute spelling as a `pypi`-universe method target, never a
/// fabricated local.
fn foreign_method<'source>(
    written: &'source [u8],
    spelling_span: Span,
) -> Result<OccurrenceTarget<'source>, PythonCollectError> {
    foreign_entity(written, spelling_span, EntityKind::Function)
}

/// The honest typed foreign key for one unresolved attribute read: the exact
/// written attribute spelling as a `pypi`-universe field target, never a
/// fabricated local.
fn foreign_field<'source>(
    written: &'source [u8],
    spelling_span: Span,
) -> Result<OccurrenceTarget<'source>, PythonCollectError> {
    foreign_entity(written, spelling_span, EntityKind::Field)
}

fn foreign_entity<'source>(
    written: &'source [u8],
    spelling_span: Span,
    kind: EntityKind,
) -> Result<OccurrenceTarget<'source>, PythonCollectError> {
    let path = core::str::from_utf8(written).map_err(|_| {
        PythonCollectError::Projection(PythonProjectionFault::ForeignSpellingUtf8 {
            start: spelling_span.start,
            end: spelling_span.end,
        })
    })?;
    let key = ForeignKey::new(
        ForeignOrigin::Universe { ecosystem: "pypi" },
        path,
        path,
        Some(kind),
    )
    .map_err(|cause| foreign_key_fault(cause, spelling_span))?;
    Ok(OccurrenceTarget::Foreign(key))
}

/// Projects one validated foreign-key grammar rejection without replacing the
/// written package spelling by a universe fallback.
fn foreign_key_fault(
    cause: backend_semantic::ir::ForeignKeyFault,
    span: Span,
) -> PythonCollectError {
    PythonCollectError::Projection(PythonProjectionFault::ForeignKey {
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

/// Projects one exact package-lineage grammar rejection.
fn lineage_fault(
    cause: backend_semantic::ir::PackageLineageFault,
    span: Span,
) -> PythonCollectError {
    PythonCollectError::Projection(PythonProjectionFault::PackageLineage {
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

/// Strips the matching quote run from one docstring slice. A docstring whose
/// quotes do not close keeps its exact bytes rather than a guessed body.
fn docstring_content(raw: &[u8]) -> &[u8] {
    let Some(&first) = raw.first() else {
        return raw;
    };
    if first != b'"' && first != b'\'' {
        return raw;
    }
    let triple = raw.get(1) == Some(&first) && raw.get(2) == Some(&first);
    let quote_width = usize::from(triple) * 2 + 1;
    let Some(body_end) = raw.len().checked_sub(quote_width) else {
        return raw;
    };
    let Some(suffix) = raw.get(body_end..) else {
        return raw;
    };
    if suffix.iter().all(|byte| *byte == first) {
        // The quote run is proven to fit both ends, so the body borrow is
        // exactly the interior range.
        let body = raw.get(quote_width..body_end);
        return body.unwrap_or(raw);
    }
    raw
}

/// Splits one docstring into borrowed fragments: prose lines, fenced code
/// blocks, and whole-line Sphinx `:ref:`/`:doc:` links. Blank lines are
/// paragraph separators, not fragments; consecutive fragments join through
/// soft breaks. Link targets naming a module declaration resolve locally.
fn doc_fragments<'source>(
    source: &'source [u8],
    docstring: &backend_frontend_python::legacy::DocstringFact,
    rows: &[Pushed<'source>],
) -> Result<Vec<DocFragmentInput<'source>>, PythonCollectError> {
    let (raw_start, raw_end) = span_bounds(docstring.span)?;
    let raw = source.get(raw_start..raw_end).unwrap_or(&[]);
    let content = docstring_content(raw);
    #[expect(
        clippy::as_conversions,
        reason = "the content is a verified interior slice of `source`, so the pointer difference is its exact offset"
    )]
    let content_start = content.as_ptr() as usize - source.as_ptr() as usize;
    let content_end = content_start + content.len();
    let mut fragments: Vec<DocFragmentInput<'source>> = Vec::new();
    let mut inside_fence = false;
    let mut code: Option<(usize, usize)> = None;
    let mut cursor = content_start;
    loop {
        let newline = source
            .get(cursor..content_end)
            .and_then(|window| window.iter().position(|byte| *byte == b'\n'));
        let line_end = newline.map_or(content_end, |offset| cursor + offset);
        let (trimmed_start, trimmed_end) = trim_bounds(source, cursor, line_end);
        let fence_line = source
            .get(trimmed_start..trimmed_end)
            .is_some_and(|line| line.starts_with(b"```"));
        if fence_line {
            if inside_fence
                && let Some((start, end)) = code.take()
                && let Some(block) = source.get(start..end).filter(|block| !block.is_empty())
            {
                fragments.push(DocFragmentInput::Code(block));
            }
            inside_fence = !inside_fence;
        } else if inside_fence {
            match code.as_mut() {
                Some((_, end)) => *end = trimmed_end,
                None => code = Some((trimmed_start, trimmed_end)),
            }
        } else if let Some(line) = source.get(trimmed_start..trimmed_end) {
            match sphinx_link(line) {
                Some(link) => fragments.push(link),
                None if !line.is_empty() => fragments.push(DocFragmentInput::Text(line)),
                None => {}
            }
        }
        if newline.is_none() {
            break;
        }
        cursor = line_end + 1;
    }
    if let Some((start, end)) = code.take()
        && let Some(block) = source.get(start..end).filter(|block| !block.is_empty())
    {
        fragments.push(DocFragmentInput::Code(block));
    }
    let resolved: Vec<DocFragmentInput<'source>> = fragments
        .into_iter()
        .map(|fragment| resolve_link_target(fragment, rows))
        .collect();
    Ok(join_with_soft_breaks(resolved))
}

/// Byte bounds of one line with its surrounding whitespace trimmed.
fn trim_bounds(source: &[u8], start: usize, end: usize) -> (usize, usize) {
    let mut trimmed_start = start;
    let mut trimmed_end = end;
    while trimmed_start < trimmed_end
        && source
            .get(trimmed_start)
            .is_some_and(|byte| byte.is_ascii_whitespace())
    {
        trimmed_start += 1;
    }
    while trimmed_end > trimmed_start
        && source
            .get(trimmed_end - 1)
            .is_some_and(|byte| byte.is_ascii_whitespace())
    {
        trimmed_end -= 1;
    }
    (trimmed_start, trimmed_end)
}

/// Parses one whole-line Sphinx `:ref:`/`:doc:` link into a doc fragment
/// with its written target; [`resolve_link_target`] decides locality later.
fn sphinx_link(line: &[u8]) -> Option<DocFragmentInput<'_>> {
    const ROLES: [&[u8]; 2] = [b":ref:`", b":doc:`"];
    let role = ROLES.iter().find(|role| line.starts_with(role))?;
    if line.len() <= role.len() || !line.ends_with(b"`") {
        return None;
    }
    let inner = line.get(role.len()..line.len() - 1)?;
    let (label, target) = match inner.iter().position(|byte| *byte == b'<') {
        Some(open) if inner.last() == Some(&b'>') => {
            let label = &inner[..open];
            let target = inner.get(open + 1..inner.len() - 1)?;
            (label, target)
        }
        _ => (inner, inner),
    };
    if target.is_empty() {
        return None;
    }
    let label = if label.is_empty() { target } else { label };
    Some(DocFragmentInput::Link {
        label,
        target: DocLinkTarget::Foreign {
            ecosystem: b"pypi",
            path: target,
        },
    })
}

/// Rewrites a link target that names a pushed module declaration to the
/// exact local entity row; every other target keeps its written spelling.
fn resolve_link_target<'source>(
    fragment: DocFragmentInput<'source>,
    rows: &[Pushed<'source>],
) -> DocFragmentInput<'source> {
    let DocFragmentInput::Link { label, target } = fragment else {
        return fragment;
    };
    let DocLinkTarget::Foreign { path, .. } = target else {
        return DocFragmentInput::Link { label, target };
    };
    match rows.iter().find(|row| row.name == path) {
        Some(row) => DocFragmentInput::Link {
            label,
            target: DocLinkTarget::Local(EntityId::new(row.ordinal)),
        },
        None => DocFragmentInput::Link { label, target },
    }
}

/// Inserts a soft break between every pair of consecutive fragments.
fn join_with_soft_breaks(fragments: Vec<DocFragmentInput<'_>>) -> Vec<DocFragmentInput<'_>> {
    let mut joined = Vec::new();
    for fragment in fragments {
        if !joined.is_empty() {
            joined.push(DocFragmentInput::SoftBreak);
        }
        joined.push(fragment);
    }
    joined
}

/// Checked usize bounds of one source span.
fn span_bounds(span: Span) -> Result<(usize, usize), PythonCollectError> {
    match (usize::try_from(span.start), usize::try_from(span.end)) {
        (Ok(start), Ok(end)) if start <= end => Ok((start, end)),
        _ => Err(PythonCollectError::Span {
            start: span.start,
            end: span.end,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::{PythonCollectError, foreign_key_fault, foreign_universe, lineage_fault};
    use backend_frontend_python::legacy::Span;
    use backend_semantic::ir::{ForeignKeyFault, PackageLineageFault};
    use backend_semantic::vocabulary::{
        ProjectionForeignKeyFault, ProjectionLineagePart, ProjectionPackageLineageFault,
        PythonProjectionFault,
    };

    #[test]
    fn foreign_spelling_utf8_retains_the_occurrence_span() {
        let span = Span { start: 17, end: 21 };
        let Err(PythonCollectError::Projection(PythonProjectionFault::ForeignSpellingUtf8 {
            start,
            end,
        })) = foreign_universe(&[0xff], span)
        else {
            panic!("foreign UTF-8 rejection lost its source span");
        };
        assert_eq!((start, end), (17, 21));
    }

    #[test]
    fn foreign_key_and_lineage_faults_retain_their_exact_spans() {
        let key_span = Span { start: 5, end: 14 };
        let key_error = foreign_key_fault(ForeignKeyFault::BackslashInPath, key_span);
        let PythonCollectError::Projection(PythonProjectionFault::ForeignKey { start, end, cause }) =
            key_error
        else {
            panic!("foreign-key grammar fault was erased");
        };
        assert_eq!(
            (start, end, cause),
            (5, 14, ProjectionForeignKeyFault::BackslashInPath)
        );

        let lineage_span = Span { start: 22, end: 31 };
        let lineage_error =
            lineage_fault(PackageLineageFault::Backslash { segment: 9 }, lineage_span);
        let PythonCollectError::Projection(PythonProjectionFault::PackageLineage {
            start,
            end,
            cause:
                ProjectionPackageLineageFault::Backslash {
                    part: ProjectionLineagePart::Invalid { segment },
                },
        }) = lineage_error
        else {
            panic!("package-lineage grammar fault was erased");
        };
        assert_eq!((start, end, segment), (22, 31, 9));
    }
}
