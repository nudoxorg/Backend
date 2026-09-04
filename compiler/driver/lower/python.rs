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

use compiler_ir::{
    AnonRecordForm, Confidence, DocFragmentInput, DocLinkTarget, EntityId, EntityKind, ForeignKey,
    ForeignOrigin, ListSpan, NominalRef, Occurrence, OccurrenceConfidence, OccurrenceTarget,
    PackageLineage, PrimitiveShape, ProductChildRole, PythonFacts, PythonParameterKind,
    ReferenceKind, RelSpan, SemanticProductConstructor, SemanticTypeChild, SemanticTypeRecord,
    SemanticTypeTag, TypeReason, TypeWidth,
};
use compiler_languages_python::{
    Annotation, AnnotationFact, AnnotationPosition, CheckerError, CheckerReport, ClassForm,
    DeclarationFact, DeclarationKind, ExtractionError, InferredType, LiteralValue, ModuleFacts,
    OccurrenceFact, ParameterKind, Pyrefly, ReceiverKind, Span, SymbolOutcome,
    TypeReason as ExtractedReason, extract,
};
use compiler_vocabulary::PythonVersion;

use crate::{
    lower::{EmissionExtension, FactSet, LEAF_PRODUCT, MAX_TYPE_CHILDREN, SemanticFact, push_fact},
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
    /// Pushed declaration rows in lane order, built while emitting.
    pushed: Vec<Pushed<'source>>,
    /// The pyrefly authority report, when the checker answered.
    checker: Option<CheckerIndex<'a>>,
    /// Reserved owner while the first structural class is being lowered.
    reserved_anchor: Option<u32>,
}

/// The interned name tables annotation lowering resolves against.
struct TypeTables<'source> {
    /// Module classes: exact name bytes to their already-pushed ordinal.
    classes: Vec<(&'source [u8], u32)>,
    /// Names bound by a `TypeVar(...)` assignment in this module.
    typevars: Vec<&'source [u8]>,
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
        Self {
            source,
            module,
            facts,
            ordinals,
            pushed: Vec::new(),
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
            .filter(|(_, declaration)| declaration.kind == DeclarationKind::Class)
            .map(|(index, _)| index)
            .collect();
        let mut tables = TypeTables {
            classes: Vec::new(),
            typevars: self.module_typevar_names()?,
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
            .filter(|(_, candidate)| {
                candidate.kind == wanted_kind
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
    /// against, independent of any pushed row.
    fn module_typevar_names(&self) -> Result<Vec<&'source [u8]>, PythonCollectError> {
        let mut typevars = Vec::new();
        for declaration in &self.module.declarations {
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
    /// variables and aliases, all in source order.
    fn emit_non_class_declarations(&mut self) -> Result<(), PythonCollectError> {
        let tables = self.type_tables()?;
        for index in 0..self.module.declarations.len() {
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
        for declaration in &self.module.declarations {
            for parameter in &declaration.type_parameters {
                typevars.push(self.slice(*parameter)?);
            }
        }
        Ok(TypeTables { classes, typevars })
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
        let mut any_resolved = false;
        let mut any_checked = false;
        for parameter in &declaration.parameters {
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
            for ordinal in children {
                fact = fact.type_child(ordinal, None, 0);
            }
            let fact = fact.with_extension(EmissionExtension::Python(extension));
            let ordinal = push_fact(self.facts, fact).map_err(PythonCollectError::Rejected)?;
            parameter_ordinals.push(Self::coordinate(parameter.name_span, ordinal)?);
        }
        let returns = self.return_annotation(declaration);
        let mut result_ordinal = None;
        let mut return_resolved = false;
        if let Some(annotation) = returns {
            let lowered =
                self.lower_annotation(&annotation.annotation, Some(annotation.span), tables)?;
            return_resolved = lowered.resolved;
            let extension = self.python_extension(&[], None, lowered_tier(lowered.resolved))?;
            // The result slot belongs to the function's key family: it is a
            // `Parameter` fact named by the function, carrying the return
            // annotation's record and its ordered type children exactly like
            // every other annotation fact.
            let mut fact =
                SemanticFact::new(EntityKind::Parameter, name, LEAF_PRODUCT).typed(lowered.record);
            for ordinal in lowered.children {
                fact = fact.type_child(ordinal, None, 0);
            }
            let fact = fact.with_extension(EmissionExtension::Python(extension));
            let ordinal = push_fact(self.facts, fact).map_err(PythonCollectError::Rejected)?;
            result_ordinal = Some(Self::coordinate(declaration.name_span, ordinal)?);
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
                let children = self.row_children(args, tables, anchor)?;
                let Some(children) = children else {
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

    /// Lowers every member of one written union to its row coordinate.
    fn row_children(
        &mut self,
        members: &[Annotation],
        tables: &TypeTables<'source>,
        anchor: u32,
    ) -> Result<Option<Vec<u32>>, PythonCollectError> {
        if members.len() > MAX_TYPE_CHILDREN {
            return Ok(None);
        }
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
        // The written spelling is the exact evidence this lane can carry:
        // imported names resolve outside the module and everything else is a
        // local name one resolution pass away from a type.
        let reason = if self.is_imported_name(name) {
            TypeReason::UnresolvedExternal
        } else {
            TypeReason::UnresolvedLocalName
        };
        Ok(LoweredType {
            record: spelled_unknown(reason, self.spelling_bytes(spelling)?),
            children: Vec::new(),
            resolved: false,
        })
    }

    /// True when the name is an import binding of this module.
    fn is_imported_name(&self, name: &str) -> bool {
        self.module.declarations.iter().any(|declaration| {
            declaration.kind == DeclarationKind::Alias && declaration.name == name
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
        match self.row_children(members, tables, anchor)? {
            Some(children) => Ok(LoweredType {
                record: union_record(),
                children,
                resolved: true,
            }),
            None => self.unrepresentable(spelling),
        }
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

    /// Resolves one occurrence target with its evidence tier: the earliest
    /// pushed row with the same name. Import bindings become foreign `pypi`
    /// package keys, since the referenced declaration lives outside this
    /// fragment; the checker's resolution decides the tier.
    fn occurrence_target(
        &self,
        rows: &[Pushed<'source>],
        occurrence: &OccurrenceFact,
    ) -> Result<Option<(OccurrenceTarget<'source>, OccurrenceConfidence)>, PythonCollectError> {
        let checked = self
            .checker
            .as_ref()
            .and_then(|report| report.symbol_at(occurrence.span));
        let matched = rows
            .iter()
            .find(|row| row.name == occurrence.target.as_bytes());
        let Some(row) = matched else {
            // The extractor only records targets matched against module
            // names; an unmatched row still carries its written spelling.
            let written = self.slice(occurrence.span)?;
            return Ok(Some((
                foreign_universe(written)?,
                OccurrenceConfidence::Index,
            )));
        };
        if let Some(imported) = row.imported {
            let module_spelling = self.slice(imported)?;
            let binding = core::str::from_utf8(row.name).unwrap_or("");
            let target = foreign_package(module_spelling, binding)?;
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

    /// Lowers one checker-inferred type to its root record and the ordered
    /// row coordinates of its children. The root record sits directly on
    /// the owning fact; nested compounds consume pooled anonymous rows.
    /// A `Named` spelling that resolves to a module class becomes that
    /// class's nominal row; any other named spelling has no borrowed bytes
    /// to own, so the honest gap names the oracle.
    fn inferred_root(
        &mut self,
        inferred: &InferredType,
        tables: &TypeTables<'source>,
    ) -> Result<Option<(SemanticTypeRecord<'source>, Vec<u32>)>, PythonCollectError> {
        let Some(anchor) = self.anchor() else {
            return Ok(None);
        };
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
                let (base, element) = (builtin_record(b"list"), element.as_deref());
                self.inferred_apply(base, element, tables, anchor)
            }
            InferredType::Set(element) => {
                let (base, element) = (builtin_record(b"set"), element.as_deref());
                self.inferred_apply(base, element, tables, anchor)
            }
            InferredType::Dict(pair) => {
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
                let mut children = Vec::new();
                for element in elements.as_ref() {
                    match self.inferred_row(element, tables, anchor)? {
                        Some(row) => children.push(row),
                        None => return Ok(None),
                    }
                }
                Ok(Some((tuple_record(), children)))
            }
            InferredType::Union(members) => {
                let mut children = Vec::new();
                for member in members.as_ref() {
                    match self.inferred_row(member, tables, anchor)? {
                        Some(row) => children.push(row),
                        None => return Ok(None),
                    }
                }
                Ok(Some((union_record(), children)))
            }
            InferredType::Callable { params, result } => {
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

/// Picks the innermost pushed row that owns an occurrence: the name must
/// match and the declaration must start no later than the reference, so the
/// owner-relative span is always ordered.
fn owner_row<'rows, 'source>(
    rows: &'rows [Pushed<'source>],
    occurrence: &OccurrenceFact,
) -> Option<&'rows Pushed<'source>> {
    let owner_bytes = occurrence.owner.as_bytes();
    rows.iter()
        .filter(|row| row.name == owner_bytes && row.span.start <= occurrence.span.start)
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

/// The total, name-preserving reference-kind mapping. The extractor's closed
/// call lattice and the lane's reference lattice share both categories, so
/// no two extractor kinds collapse and no lane kind is unreachable.
fn reference_kind(kind: compiler_languages_python::OccurrenceKind) -> ReferenceKind {
    match kind {
        compiler_languages_python::OccurrenceKind::FunctionCall => ReferenceKind::FunctionCall,
        compiler_languages_python::OccurrenceKind::MethodCall => ReferenceKind::MethodCall,
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
) -> Result<OccurrenceTarget<'source>, PythonCollectError> {
    let path = core::str::from_utf8(module_spelling).ok();
    if let Some(path) = path.filter(|path| !path.is_empty()) {
        let name = match path.split_once('.') {
            Some((head, _)) => head,
            None => path,
        };
        if let Ok(key) = ForeignKey::new(
            ForeignOrigin::Package(PackageLineage {
                ecosystem: "pypi",
                name,
            }),
            path,
            display,
            None,
        ) {
            return Ok(OccurrenceTarget::Foreign(key));
        }
    }
    foreign_universe(module_spelling)
}

/// A foreign key outside every package, carrying the written spelling.
fn foreign_universe<'source>(
    written: &'source [u8],
) -> Result<OccurrenceTarget<'source>, PythonCollectError> {
    let path = core::str::from_utf8(written).unwrap_or("");
    let usable = if path.is_empty() { "unresolved" } else { path };
    let key = ForeignKey::new(
        ForeignOrigin::Universe { ecosystem: "pypi" },
        usable,
        path,
        None,
    )
    .map_err(lane_rejected)?;
    Ok(OccurrenceTarget::Foreign(key))
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
    docstring: &compiler_languages_python::DocstringFact,
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
mod tests {} // diagnostic stub in throwaway worktree
