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
//!   nominal target is strictly backward.
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
    Confidence, DocFragmentInput, DocLinkTarget, EntityId, EntityKind, ForeignKey, ForeignOrigin,
    ListSpan, NominalRef, Occurrence, OccurrenceConfidence, OccurrenceTarget, PackageLineage,
    PrimitiveShape, ProductChildRole, PythonFacts, PythonParameterKind, ReferenceKind, RelSpan,
    SemanticProductConstructor, SemanticTypeRecord, SemanticTypeTag, TypeReason, TypeWidth,
};
use compiler_languages_python::{
    Annotation, AnnotationFact, AnnotationPosition, CheckerReport, DeclarationFact, DeclarationKind,
    ExtractionError, InferredType, ModuleFacts, OccurrenceFact, ParameterKind, Pyrefly, Span,
    SymbolOutcome, TypeReason as ExtractedReason, extract,
};
use compiler_vocabulary::PythonVersion;

use crate::{
    lower::{
        EmissionExtension, FactSet, LEAF_PRODUCT, MAX_TYPE_CHILDREN, SemanticFact, push_fact,
    },
    types::LoweringUnsupported,
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
    /// Canonical declaration admission rejected an exact borrowed fact.
    Lowering(LoweringUnsupported),
    /// Ruff returned a declaration span outside the exact caller source.
    Span { start: u32, end: u32 },
}

/// Streams the complete Python semantic plane into the canonical lane.
///
/// Ruff owns spans, declarations, and written annotations. pyrefly runs as
/// the peer type authority: its typed report fills unannotated constants and
/// parameters, upgrades resolved call sites to `Import`/`Oracle` evidence,
/// and lifts dynamic-confidence tiers. When the checker is unavailable or
/// fails, the lane degrades to its proven syntax tiers — it never invents a
/// dynamic fact the checker did not supply.
pub(crate) fn collect<'source>(
    profile: PythonVersion,
    source: &'source [u8],
    facts: &mut FactSet<'source>,
) -> Result<(), PythonCollectError> {
    let module = extract(source, profile).map_err(PythonCollectError::Authority)?;
    let checker = match checked_report(profile, source, &module) {
        Ok(report) => report,
        // A missing or failing authority is honest absence, not a lane fault.
        Err(_) => None,
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
/// `Ok(None)` is the honest unavailable answer: the adapter does not resolve
/// on this system. `Err` carries the exact [`compiler_languages_python::CheckerError`]
/// with every operand intact for callers that must observe the failure.
pub(crate) fn checked_report(
    profile: PythonVersion,
    source: &[u8],
    module: &ModuleFacts,
) -> Result<Option<CheckerReport>, compiler_languages_python::CheckerError> {
    let adapter = Pyrefly::from_env();
    if !adapter.is_available() {
        return Ok(None);
    }
    adapter.analyze(source, profile, module).map(Some)
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
    checker: Option<&'a CheckerReport>,
}

/// The interned name tables annotation lowering resolves against.
struct TypeTables<'source> {
    /// Module classes: exact name bytes to their already-pushed ordinal.
    classes: Vec<(&'source [u8], u32)>,
    /// Names bound by a `TypeVar(...)` assignment in this module.
    typevars: Vec<&'source [u8]>,
}

/// The bit shift that packs a width cell above the integer signedness bit,
/// frozen by the lattice's integer-row encoding.
const INTEGER_WIDTH_SHIFT: u32 = 1;

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
            checker,
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
    /// any later annotation can name any class in the module.
    fn emit_classes(&mut self) -> Result<(), PythonCollectError> {
        let indices: Vec<usize> = self
            .module
            .declarations
            .iter()
            .enumerate()
            .filter(|(_, declaration)| declaration.kind == DeclarationKind::Class)
            .map(|(index, _)| index)
            .collect();
        for index in indices {
            let declaration = &self.module.declarations[index];
            let name = self.slice(declaration.name_span)?;
            // The self-nominal names the row being pushed: the one closed
            // diagonal case the lane's backward law admits.
            let own_ordinal = Self::coordinate(declaration.name_span, self.facts.len())?;
            let record = SemanticTypeRecord {
                tag: SemanticTypeTag::Nominal,
                payload0: 0,
                payload1: 0,
                text: None,
                text2: None,
                nominal: Some(NominalRef::Local(EntityId::new(own_ordinal))),
                children: ListSpan::new(0, 0),
            };
            let extension = self.python_extension(
                &declaration.decorator_spans,
                None,
                Confidence::Syntactic,
            )?;
            let fact = SemanticFact::new(
                EntityKind::Record,
                name,
                SemanticProductConstructor::PRODUCT,
            )
            .typed(record)
            .with_extension(EmissionExtension::Python(extension));
            let ordinal = push_fact(self.facts, fact).map_err(PythonCollectError::Lowering)?;
            self.record_pushed(index, ordinal, name, declaration);
        }
        Ok(())
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
                DeclarationKind::Alias => self.emit_alias(index)?,
            }
        }
        Ok(())
    }

    /// Interns the class and `TypeVar` name tables annotations resolve
    /// against. Classes are all pushed by pass one, so every nominal target
    /// they name is already in the lane.
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
            let ordinal = push_fact(self.facts, fact).map_err(PythonCollectError::Lowering)?;
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
            // annotation's record.
            let fact = SemanticFact::new(EntityKind::Parameter, name, LEAF_PRODUCT)
                .typed(lowered.record)
                .with_extension(EmissionExtension::Python(extension));
            let ordinal = push_fact(self.facts, fact).map_err(PythonCollectError::Lowering)?;
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
            payload1 = SemanticTypeRecord::RESULT_FLAG;
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
        let ordinal = push_fact(self.facts, fact).map_err(PythonCollectError::Lowering)?;
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
                (lowered.record, lowered.children, lowered_tier(lowered.resolved))
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
        let ordinal = push_fact(self.facts, fact).map_err(PythonCollectError::Lowering)?;
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
            .and_then(|report| report.inference_at(site))
            .filter(|inferred| **inferred != InferredType::Any)
    }

    /// Lowers one import binding. Its declared type is honestly unwritten.
    fn emit_alias(&mut self, index: usize) -> Result<(), PythonCollectError> {
        let declaration = &self.module.declarations[index];
        let name = self.slice(declaration.name_span)?;
        let extension = self.python_extension(&[], None, Confidence::Syntactic)?;
        let fact = SemanticFact::new(EntityKind::Alias, name, LEAF_PRODUCT)
            .typed(unknown_record(TypeReason::Unannotated))
            .with_extension(EmissionExtension::Python(extension));
        let ordinal = push_fact(self.facts, fact).map_err(PythonCollectError::Lowering)?;
        self.record_pushed(index, ordinal, name, declaration);
        Ok(())
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
            Annotation::Name(name) => self.lower_name(name, spelling, tables),
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
            Annotation::Literal(_) => self.unrepresentable(spelling),
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
                let is_union_base = matches!(base.as_ref(), Annotation::Name(name) if name == "Union" || name == "typing.Union");
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
            Annotation::Name(name) => Some(name.as_str()),
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
                    payload1 = SemanticTypeRecord::RESULT_FLAG;
                }
                Ok(Some((function_pointer_record(payload1), children)))
            }
            Some("Optional") | Some("typing.Optional") => {
                let mut members = args.clone();
                members.push(Annotation::None);
                let children = self.row_children(&members, tables, anchor)?;
                Ok(children.map(|children| (union_record(), children)))
            }
            Some("list") | Some("set") | Some("frozenset") | Some("dict")
            | Some("typing.List") | Some("typing.Set") | Some("typing.FrozenSet")
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
            Annotation::Name(name) => {
                if let Some((_, ordinal)) = tables
                    .classes
                    .iter()
                    .find(|(known, _)| *known == name.as_bytes())
                {
                    return Ok(Some(*ordinal));
                }
                if tables.typevars.contains(&name.as_bytes()) {
                    let text = self.spelling_bytes(spelling)?;
                    let record = if text.is_empty() {
                        unknown_record(TypeReason::OracleGap)
                    } else {
                        typevar_record(text)
                    };
                    return self.leaf_row(record, anchor);
                }
                match name.as_str() {
                    "int" => self.leaf_row(integer_arch_signed_record(), anchor),
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
            Annotation::Generic { .. } => match self.compound_root(annotation, spelling, tables)? {
                Some((record, children)) => {
                    self.parent_row(record, &children, anchor)
                }
                None => Ok(None),
            },
            Annotation::Literal(_)
            | Annotation::StringLiteral(_)
            | Annotation::Unknown(_) => Ok(None),
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
            let admitted = self
                .facts
                .anonymous_type_child(*child, None, 0)
                .is_ok();
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
        Ok(self.facts.intern_anonymous_type_row(anchor, record).ok())
    }

    /// The anchor fact for this declaration's anonymous type rows: the first
    /// already-pushed declaration, per the established lane convention that
    /// the pooled row lane only admits already-pushed owners. `None` means
    /// nothing is pushed yet, so no compound can be hosted for this fact.
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
            "int" => return leaf(integer_arch_signed_record(), true),
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
    ) -> Result<Option<(OccurrenceTarget<'source>, OccurrenceConfidence)>, PythonCollectError>
    {
        let checked = self
            .checker
            .and_then(|report| report.symbol_at(occurrence.span))
            .map(|symbol| &symbol.outcome);
        let matched = rows
            .iter()
            .find(|row| row.name == occurrence.target.as_bytes());
        let Some(row) = matched else {
            // The extractor only records targets matched against module
            // names; an unmatched row still carries its written spelling.
            let written = self.slice(occurrence.span)?;
            return Ok(Some((foreign_universe(written)?, OccurrenceConfidence::Index)));
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
            InferredType::Integer => Ok(Some((integer_arch_signed_record(), Vec::new()))),
            InferredType::Float => Ok(Some((float64_record(), Vec::new()))),
            InferredType::Boolean => Ok(Some((primitive_record(PrimitiveShape::Bool, 0), Vec::new()))),
            InferredType::Str => Ok(Some((primitive_record(PrimitiveShape::Str, 0), Vec::new()))),
            InferredType::Bytes => Ok(Some((builtin_record(b"bytes"), Vec::new()))),
            InferredType::Complex => Ok(Some((builtin_record(b"complex"), Vec::new()))),
            InferredType::NoneType => Ok(Some((none_record(), Vec::new()))),
            InferredType::List(element) => {
                let (base, element) = (
                    builtin_record(b"list"),
                    element.as_deref(),
                );
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
                    payload1 = SemanticTypeRecord::RESULT_FLAG;
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
            InferredType::Integer => self.leaf_row(integer_arch_signed_record(), anchor),
            InferredType::Float => self.leaf_row(float64_record(), anchor),
            InferredType::Boolean => self.leaf_row(primitive_record(PrimitiveShape::Bool, 0), anchor),
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
            InferredType::Tuple(_)
            | InferredType::Union(_)
            | InferredType::Callable { .. } => match self.inferred_root(inferred, tables)? {
                Some((record, children)) => self.parent_row(record, &children, anchor),
                None => Ok(None),
            },
            InferredType::Any => Ok(None),
        }
    }

    /// Streams every declaration docstring as borrowed doc fragments.
    fn emit_docs(&mut self) -> Result<(), PythonCollectError> {
        for (index, declaration) in self.module.declarations.iter().enumerate() {
            let Some(docstring) = &declaration.docstring else {
                continue;
            };
            let Some(ordinal) = self.ordinals[index] else {
                // The module row is not a lane fact; its documentation has
                // no honest owner and is never synthesized onto another row.
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

/// The integer row for a bare `int`: architecture-width and signed, packed
/// per the lattice's frozen integer-cell encoding.
fn integer_arch_signed_record() -> SemanticTypeRecord<'static> {
    primitive_record(
        PrimitiveShape::Integer,
        (TypeWidth::Arch.to_cell() << INTEGER_WIDTH_SHIFT)
            | SemanticTypeRecord::INTEGER_SIGNED_FLAG,
    )
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
mod tests {
    use compiler_ir::{
        DecodedDocFact, DecodedOccurrence, DecodedTypeFact, EntityKind, FragmentView,
        LanguageExtensionWireFact, OccurrenceTarget, PrimitiveShape, SemanticTypeTag,
        SourceIdentity, TypeReason,
    };
    use compiler_vocabulary::{
        CompileRecipeFact, LanguageProfile, NativeTool, PythonVersion, Stage,
    };
    use heart_identity::{ContentId, SourceFactDomain, ToolchainDomain};
    use thiserror::Error;

    use super::{
        FactSet, PythonCollectError, collect_syntax_only, collect_with_checker,
    };

    use compiler_languages_python::{CheckerReport, Pyrefly};

    const SOURCE_BYTES: &[u8] = b"python-semantic-lane-source";
    const OUTPUT_CAPACITY: usize = 65_536;

    #[derive(Debug, Error)]
    enum TestError {
        #[error("fixture source length does not fit the source identity")]
        Source(#[from] core::num::TryFromIntError),
        #[error("the python authority rejected the valid fixture: {0:?}")]
        Collect(PythonCollectError),
        #[error("the extraction authority rejected the valid fixture: {0:?}")]
        CollectAuth(compiler_languages_python::ExtractionError),
        #[error("{label} faulted: {fault:?}")]
        Admission {
            label: &'static str,
            fault: super::super::AdmissionFault,
        },
        #[error("the fragment failed validation")]
        Validate(#[from] compiler_ir::FragmentError),
        #[error("the fragment output tail changed")]
        Tail,
        #[error("entity {ordinal} differed from the expected fact row")]
        Entity { ordinal: usize },
        #[error("the fragment committed {actual} rows, not {expected}")]
        Count { expected: usize, actual: usize },
        #[error("the python extension row for entity {ordinal} was absent or undecodable")]
        Extension { ordinal: usize },
        #[error("expected the fixture to be rejected")]
        ExpectedRejection,
        #[error("the pyrefly authority failed: {0:?}")]
        Checker(compiler_languages_python::CheckerError),
    }

    fn identity() -> Result<SourceIdentity, TestError> {
        Ok(SourceIdentity {
            identity: ContentId::<SourceFactDomain>::from_canonical_bytes(SOURCE_BYTES),
            byte_len: u32::try_from(SOURCE_BYTES.len())?,
        })
    }

    fn recipe() -> CompileRecipeFact {
        let profile = LanguageProfile::Python(PythonVersion::Python314);
        CompileRecipeFact::derive(
            profile,
            Stage::LowerIr,
            NativeTool::Python,
            ContentId::<SourceFactDomain>::from_canonical_bytes(SOURCE_BYTES),
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"python-semantic-lane-toolchain"),
        )
    }

    /// Collects one fixture into the lane and writes its validated fragment,
    /// proving the untouched output tail stayed unchanged. The syntax-only
    /// entry point pins these laws deterministically, with or without a
    /// provisioned pyrefly.
    fn write(source: &[u8]) -> Result<Vec<u8>, TestError> {
        write_with(source, None)
    }

    /// Collects one fixture with a caller-supplied type-authority report.
    fn write_with(
        source: &[u8],
        checker: Option<&CheckerReport>,
    ) -> Result<Vec<u8>, TestError> {
        let mut facts = FactSet::new();
        collect_with_checker(
            &super::extract(source, PythonVersion::Python314).map_err(TestError::CollectAuth)?,
            source,
            &mut facts,
            checker,
        )
        .map_err(TestError::Collect)?;
        let mut output = vec![0xa5_u8; OUTPUT_CAPACITY];
        let length = {
            let written =
                super::super::admit(&facts, identity()?, recipe(), recipe().profile, &mut output)
                    .map_err(|fault| TestError::Admission {
                    label: "admission",
                    fault,
                })?;
            written.len()
        };
        if !output[length..].iter().all(|byte| *byte == 0xa5) {
            return Err(TestError::Tail);
        }
        output.truncate(length);
        Ok(output)
    }

    /// Validates one written fragment.
    fn view_of(bytes: &[u8]) -> Result<FragmentView<'_>, TestError> {
        FragmentView::validate(bytes).map_err(TestError::from)
    }

    /// Decodes the entity rows of one validated fragment: each row is the
    /// resolved name atom bytes and the entity kind.
    #[expect(
        clippy::as_conversions,
        reason = "fragment validation proved every atom coordinate fits the test address space before this usize projection"
    )]
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
        let Some(mut cursor) = view.type_facts() else {
            return Err(TestError::Tail);
        };
        let mut rows = Vec::new();
        for row in cursor.by_ref() {
            rows.push(row.map_err(|fault| TestError::Admission {
                label: "type facts",
                fault: super::super::AdmissionFault::Prepare(
                    compiler_ir::PrepareError::TypeFacts { fault },
                ),
            })?);
        }
        Ok(rows)
    }

    /// Decodes every occurrence fact from the validated fragment.
    fn occurrences<'fragment>(
        view: &FragmentView<'fragment>,
    ) -> Result<Vec<DecodedOccurrence<'fragment>>, TestError> {
        let Some(mut cursor) = view.occurrences() else {
            return Err(TestError::Tail);
        };
        let mut rows = Vec::new();
        for row in cursor.by_ref() {
            rows.push(row.map_err(|_| TestError::Tail)?);
        }
        Ok(rows)
    }

    /// Decodes every documentation fact from the validated fragment.
    fn docs<'fragment>(
        view: &FragmentView<'fragment>,
    ) -> Result<Vec<DecodedDocFact<'fragment>>, TestError> {
        let Some(mut cursor) = view.docs() else {
            return Err(TestError::Tail);
        };
        let mut rows = Vec::new();
        for row in cursor.by_ref() {
            rows.push(row.map_err(|_| TestError::Tail)?);
        }
        Ok(rows)
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

    /// Decodes the python extension row of one entity from the raw
    /// language-extension section: a 16-byte header, one 20-byte directory
    /// per plane (python is the fifth), then the row table and fact pool.
    fn python_extension(
        view: &FragmentView<'_>,
        ordinal: usize,
    ) -> Result<compiler_ir::PythonFacts, TestError> {
        let payload = view
            .language_extension_payload()
            .ok_or(TestError::Extension { ordinal })?;
        let directory = 16 + 4 * 20;
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
            offset + rows * 4 + usize::try_from(fact_ordinal).map_err(|_| TestError::Tail)? * 12;
        compiler_ir::PythonFacts::decode(payload, at).ok_or(TestError::Extension { ordinal })
    }

    /// Reads one pooled atom list from the extension-pool payload: a u32
    /// type-parameter count (zero in this lane), then the atom-list lane of
    /// (length, u32 words) rows.
    fn pooled_atom_list(pool: &[u8], index: usize) -> Result<Vec<u32>, TestError> {
        if word(pool, 0)? != 0 {
            return Err(TestError::Tail);
        }
        let mut cursor = 4;
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
        Err(TestError::Tail)
    }

    /// Parses the pooled local targets of the last type-fact row. Every
    /// child in this lane carries an empty name cell, so each child is
    /// exactly one target tag byte, one u32 coordinate, one empty name
    /// cell, and one flags byte.
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
        let mut cursor = 4usize;
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
        // Local tag + u32 coordinate + empty name cell + flags byte.
        let width = 1 + 4 + 1 + 1;
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

    /// A signature lowers to parameter rows, an annotated-return result
    /// slot, and a function row whose constructor and FunctionPointer type
    /// record name exactly those rows at the indexed confidence tier.
    #[test]
    fn annotated_signature_lowers_parameter_result_and_function_pointer_rows()
    -> Result<(), TestError> {
        let source = b"def scale(x: int) -> str:\n    return \"\"\n";
        let bytes = write(source)?;
        let view = view_of(&bytes)?;
        let entities = entity_rows(&view);
        if entities.len() != 3 {
            return Err(TestError::Count {
                expected: 3,
                actual: entities.len(),
            });
        }
        if entities[0] != (&b"x"[..], EntityKind::Parameter)
            || entities[1].1 != EntityKind::Parameter
            || entities[2].1 != EntityKind::Function
        {
            return Err(TestError::Entity { ordinal: 0 });
        }
        let rows = type_facts(&view)?;
        if rows.len() != 3 {
            return Err(TestError::Count {
                expected: 3,
                actual: rows.len(),
            });
        }
        // Row 0: bare `int` is the architecture-width signed integer.
        let integer = &rows[0].record;
        let expected_width = (compiler_ir::TypeWidth::Arch.to_cell() << 1)
            | compiler_ir::SemanticTypeRecord::INTEGER_SIGNED_FLAG;
        if integer.tag != SemanticTypeTag::Primitive
            || integer.payload0 != u32::from(PrimitiveShape::Integer)
            || integer.payload1 != expected_width
        {
            return Err(TestError::Entity { ordinal: 0 });
        }
        // Row 1: the result slot carries `str`.
        if rows[1].record.tag != SemanticTypeTag::Primitive
            || rows[1].record.payload0 != u32::from(PrimitiveShape::Str)
        {
            return Err(TestError::Entity { ordinal: 1 });
        }
        // Row 2: the function is the FunctionPointer with a result flag.
        if rows[2].record.tag != SemanticTypeTag::FunctionPointer
            || rows[2].record.payload1 != compiler_ir::SemanticTypeRecord::RESULT_FLAG
        {
            return Err(TestError::Entity { ordinal: 2 });
        }
        for ordinal in 0..3 {
            if python_extension(&view, ordinal)?.dynamic_confidence
                != compiler_ir::Confidence::Indexed
            {
                return Err(TestError::Extension { ordinal });
            }
        }
        Ok(())
    }

    /// An unannotated function is exactly `Unknown(Unannotated)` on its
    /// parameter and stays on the syntactic confidence tier.
    #[test]
    fn unannotated_function_stays_exactly_unknown_unannotated() -> Result<(), TestError> {
        let source = b"def f(a):\n    pass\n";
        let bytes = write(source)?;
        let view = view_of(&bytes)?;
        let entities = entity_rows(&view);
        if entities.len() != 2 {
            return Err(TestError::Count {
                expected: 2,
                actual: entities.len(),
            });
        }
        let rows = type_facts(&view)?;
        let parameter = &rows[0].record;
        if parameter.tag != SemanticTypeTag::Unknown
            || parameter.payload0 != u32::from(TypeReason::Unannotated)
            || parameter.text.is_some()
        {
            return Err(TestError::Entity { ordinal: 0 });
        }
        if rows[1].record.tag != SemanticTypeTag::FunctionPointer || rows[1].record.payload1 != 0 {
            return Err(TestError::Entity { ordinal: 1 });
        }
        let extension = python_extension(&view, 0)?;
        if extension.dynamic_confidence != compiler_ir::Confidence::Syntactic
            || extension.parameter_kind != compiler_ir::PythonParameterKind::PositionalOrKeyword
        {
            return Err(TestError::Extension { ordinal: 0 });
        }
        Ok(())
    }

    /// The quoted recursive annotation closes on the class's diagonal
    /// self-nominal, and the field names the same row.
    #[test]
    fn recursive_string_annotation_closes_on_the_self_nominal() -> Result<(), TestError> {
        let source = b"class Node:\n    next: \"Node\"\n";
        let bytes = write(source)?;
        let view = view_of(&bytes)?;
        let entities = entity_rows(&view);
        if entities.len() != 2
            || entities[0].1 != EntityKind::Record
            || entities[1].1 != EntityKind::Field
        {
            return Err(TestError::Entity { ordinal: 0 });
        }
        let rows = type_facts(&view)?;
        let self_nominal = Some(compiler_ir::NominalRef::Local(compiler_ir::EntityId::new(
            0,
        )));
        if rows[0].record.nominal != self_nominal || rows[1].record.nominal != self_nominal {
            return Err(TestError::Entity { ordinal: 0 });
        }
        Ok(())
    }

    /// An imported annotation keeps the exact written spelling as
    /// `Unknown(UnresolvedExternal)`, while `Any` is the dynamic reason.
    #[test]
    fn imported_annotation_is_unresolved_external_with_the_written_spelling()
    -> Result<(), TestError> {
        let source = b"from json import loads\n\ndef f(x: loads):\n    pass\n\nfrom typing import Any\n\ndef g(v: Any):\n    pass\n";
        let bytes = write(source)?;
        let view = view_of(&bytes)?;
        let entities = entity_rows(&view);
        // loads (Alias), x, f, Any (Alias), v, g
        if entities.len() != 6 {
            return Err(TestError::Count {
                expected: 6,
                actual: entities.len(),
            });
        }
        let rows = type_facts(&view)?;
        let imported = &rows[1].record;
        if imported.tag != SemanticTypeTag::Unknown
            || imported.payload0 != u32::from(TypeReason::UnresolvedExternal)
            || imported.text != Some(&b"loads"[..])
        {
            return Err(TestError::Entity { ordinal: 1 });
        }
        let dynamic = &rows[4].record;
        if dynamic.tag != SemanticTypeTag::Unknown
            || dynamic.payload0 != u32::from(TypeReason::DynamicallyTyped)
            || dynamic.text.is_some()
        {
            return Err(TestError::Entity { ordinal: 4 });
        }
        Ok(())
    }

    /// Every parameter convention survives into its extension row, and the
    /// recursive call resolves locally at the index tier with an
    /// owner-relative span.
    #[test]
    fn parameter_conventions_and_call_occurrence_round_trip() -> Result<(), TestError> {
        let source =
            b"def f(a: int, /, b=2, *args: str, c: bool = 1, **kwargs):\n    return f(a)\n";
        let bytes = write(source)?;
        let view = view_of(&bytes)?;
        let entities = entity_rows(&view);
        if entities.len() != 6 {
            return Err(TestError::Count {
                expected: 6,
                actual: entities.len(),
            });
        }
        let expected_kinds = [
            compiler_ir::PythonParameterKind::PositionalOnly,
            compiler_ir::PythonParameterKind::PositionalOrKeyword,
            compiler_ir::PythonParameterKind::VariadicPositional,
            compiler_ir::PythonParameterKind::KeywordOnly,
            compiler_ir::PythonParameterKind::VariadicKeyword,
        ];
        for (ordinal, expected) in expected_kinds.iter().enumerate() {
            if python_extension(&view, ordinal)?.parameter_kind != *expected {
                return Err(TestError::Extension { ordinal });
            }
        }
        let rows = occurrences(&view)?;
        if rows.len() != 1 {
            return Err(TestError::Count {
                expected: 1,
                actual: rows.len(),
            });
        }
        let occurrence = &rows[0];
        if occurrence.owner.raw != 5
            || occurrence.occurrence.kind != compiler_ir::ReferenceKind::FunctionCall
            || occurrence.occurrence.confidence != compiler_ir::OccurrenceConfidence::Index
        {
            return Err(TestError::Entity { ordinal: 5 });
        }
        let OccurrenceTarget::Local(target) = occurrence.occurrence.target else {
            return Err(TestError::Entity { ordinal: 5 });
        };
        if target.raw != 5 {
            return Err(TestError::Entity { ordinal: 5 });
        }
        // The owner (`f`) starts at byte 0, so the relative span is the call
        // site's absolute offset of the callee name inside `f(a)`.
        let call_at = source
            .windows(4)
            .position(|window| window == b"f(a)")
            .ok_or(TestError::Tail)?;
        let call_at = u32::try_from(call_at).map_err(|_| TestError::Tail)?;
        if occurrence.occurrence.span.start != call_at
            || occurrence.occurrence.span.end != call_at + 1
        {
            return Err(TestError::Entity { ordinal: 5 });
        }
        Ok(())
    }

    /// A decorator pair is interned once as extension atoms and named by the
    /// declaration's pooled decorator list.
    #[test]
    fn decorator_pair_interns_atoms_and_extension_row() -> Result<(), TestError> {
        let source = b"@staticmethod\n@cache\ndef f():\n    pass\n";
        let bytes = write(source)?;
        let view = FragmentView::validate(&bytes)?;
        let atoms: Vec<&[u8]> = view.atoms().map(|atom| atom.bytes).collect();
        if !atoms.contains(&&b"staticmethod"[..]) || !atoms.contains(&&b"cache"[..]) {
            return Err(TestError::Tail);
        }
        let extension = python_extension(&view, 0)?;
        if extension.dynamic_confidence != compiler_ir::Confidence::Syntactic {
            return Err(TestError::Extension { ordinal: 0 });
        }
        let pool = view.extension_pool_payload().ok_or(TestError::Tail)?;
        let list = pooled_atom_list(
            pool,
            usize::try_from(extension.decorators.raw).map_err(|_| TestError::Tail)?,
        )?;
        let expected: [&[u8]; 2] = [b"staticmethod", b"cache"];
        if list.len() != expected.len() {
            return Err(TestError::Extension { ordinal: 0 });
        }
        for (position, word) in list.iter().enumerate() {
            let coordinate = usize::try_from(*word).map_err(|_| TestError::Tail)?;
            if atoms.get(coordinate).copied() != Some(expected[position]) {
                return Err(TestError::Extension { ordinal: 0 });
            }
        }
        Ok(())
    }

    /// Docstrings lower to prose text, fenced code, and Sphinx links whose
    /// targets name module declarations locally; fragments join through
    /// soft breaks.
    #[test]
    fn docstring_prose_code_and_sphinx_links_become_fragments() -> Result<(), TestError> {
        let source = b"def guide():\n    \"\"\"One line.\n\n    :ref:`guide`\n    :doc:`Other <guide>`\n    ```\n    code_block()\n    ```\n    Tail line.\n    \"\"\"\n";
        let bytes = write(source)?;
        let view = view_of(&bytes)?;
        let entities = entity_rows(&view);
        if entities.len() != 1 {
            return Err(TestError::Count {
                expected: 1,
                actual: entities.len(),
            });
        }
        let rows = docs(&view)?;
        let expected: [(&[u8], bool); 5] = [
            (b"One line.", false),
            (b"guide", true),
            (b"Other ", true),
            (b"code_block()", false),
            (b"Tail line.", false),
        ];
        if rows.len() != expected.len() * 2 - 1 {
            return Err(TestError::Count {
                expected: expected.len() * 2 - 1,
                actual: rows.len(),
            });
        }
        let mut ordinal = 0;
        for (position, (bytes, is_link)) in expected.iter().enumerate() {
            if position > 0 {
                if rows[ordinal].fragment != compiler_ir::DocFragmentInput::SoftBreak {
                    return Err(TestError::Tail);
                }
                ordinal += 1;
            }
            let matched = match (&rows[ordinal].fragment, *is_link) {
                (compiler_ir::DocFragmentInput::Text(text), false) => *text == *bytes,
                (compiler_ir::DocFragmentInput::Code(code), false) => *code == *bytes,
                (
                    compiler_ir::DocFragmentInput::Link {
                        label,
                        target: compiler_ir::DocLinkTarget::Local(target),
                    },
                    true,
                ) => *label == *bytes && target.raw == 0,
                _ => false,
            };
            if !matched {
                return Err(TestError::Tail);
            }
            ordinal += 1;
        }
        Ok(())
    }

    /// Two assignments to one name produce two facts with the same key; the
    /// lane's rank disambiguation is the only difference.
    #[test]
    fn rebinds_share_the_key_and_both_facts_survive() -> Result<(), TestError> {
        let source = b"x = 1\nx = 2\n";
        let bytes = write(source)?;
        let view = view_of(&bytes)?;
        let entities = entity_rows(&view);
        if entities.len() != 2 {
            return Err(TestError::Count {
                expected: 2,
                actual: entities.len(),
            });
        }
        if entities[0] != (&b"x"[..], EntityKind::Static)
            || entities[1] != (&b"x"[..], EntityKind::Static)
        {
            return Err(TestError::Entity { ordinal: 0 });
        }
        let rows = type_facts(&view)?;
        if rows.len() != 2
            || rows[0].record.tag != SemanticTypeTag::Unknown
            || rows[0].record.payload0 != u32::from(TypeReason::Unannotated)
        {
            return Err(TestError::Entity { ordinal: 0 });
        }
        Ok(())
    }

    /// A union of two module classes lowers to a union row whose children
    /// are exactly those backward class rows.
    #[test]
    fn union_of_module_classes_lowers_union_children() -> Result<(), TestError> {
        let source = b"class A:\n    pass\n\nclass B:\n    pass\n\nvalue: A | B = 1\n";
        let bytes = write(source)?;
        let view = view_of(&bytes)?;
        let entities = entity_rows(&view);
        if entities.len() != 3 || entities[2].1 != EntityKind::Static {
            return Err(TestError::Entity { ordinal: 2 });
        }
        let rows = type_facts(&view)?;
        if rows[2].record.tag != SemanticTypeTag::Union {
            return Err(TestError::Entity { ordinal: 2 });
        }
        let payload = view.type_fact_payload().ok_or(TestError::Tail)?;
        if last_type_children(payload, &rows)? != vec![0, 1] {
            return Err(TestError::Tail);
        }
        if python_extension(&view, 2)?.dynamic_confidence != compiler_ir::Confidence::Indexed {
            return Err(TestError::Extension { ordinal: 2 });
        }
        Ok(())
    }


    /// Extracts one live-test fixture under the canonical profile.
    fn fixture(source: &[u8]) -> Result<compiler_languages_python::ModuleFacts, TestError> {
        super::extract(source, PythonVersion::Python314).map_err(TestError::CollectAuth)
    }

    /// A source beyond the lane's 128-fact bound is the exact typed lowering
    /// rejection, never a truncated emission.
    #[test]
    fn module_capacity_beyond_128_is_the_exact_lowering_rejection() -> Result<(), TestError> {
        let mut source = String::new();
        for ordinal in 0..129 {
            source.push_str(&format!("value_{ordinal} = {ordinal}\n"));
        }
        let mut facts = FactSet::new();
        match collect_syntax_only(PythonVersion::Python314, source.as_bytes(), &mut facts) {
            Err(PythonCollectError::Lowering(_)) => Ok(()),
            Err(other) => Err(TestError::Collect(other)),
            Ok(()) => Err(TestError::ExpectedRejection),
        }
    }

    /// Holds source authority failures without an assertion panic.
    #[derive(Debug, thiserror::Error)]
    enum LegacyTestError {
        /// Ruff or fact admission rejected the valid source fixture.
        #[error("Ruff declaration authority rejected the fixture: {0:?}")]
        Collect(PythonCollectError),
        /// The class declaration was not admitted from the Ruff AST.
        #[error("Ruff class declaration was absent from the canonical fact lane")]
        Class,
        /// A mutable Python module binding was admitted as a constant.
        #[error("Ruff assignment binding did not retain mutable static semantics")]
        Static,
    }

    #[test]
    fn ruff_class_declaration_flows_into_the_shared_fact_lane() -> Result<(), LegacyTestError> {
        let mut facts = FactSet::new();
        collect_syntax_only(
            PythonVersion::Python313,
            b"class RuffAuthority:\n    pass\n",
            &mut facts,
        )
        .map_err(LegacyTestError::Collect)?;
        if facts.kind_at(0) == Some(EntityKind::Record) {
            Ok(())
        } else {
            Err(LegacyTestError::Class)
        }
    }

    #[test]
    fn ruff_assignment_is_not_admitted_as_a_constant() -> Result<(), LegacyTestError> {
        let mut facts = FactSet::new();
        collect_syntax_only(PythonVersion::Python313, b"value = 1\n", &mut facts)
            .map_err(LegacyTestError::Collect)?;
        if facts.kind_at(0) == Some(EntityKind::Static) {
            Ok(())
        } else {
            Err(LegacyTestError::Static)
        }
    }

    #[derive(Debug, thiserror::Error)]
    enum LegacyErrorKind {
        #[error("Ruff assignment binding did not retain mutable static semantics")]
        Static,
    }

    /// `list[int]` lowers to an `Apply` row over the `list` builtin row and
    /// the integer row, at the indexed confidence tier.
    #[test]
    fn list_annotation_lowers_apply_over_builtin_and_element_rows() -> Result<(), TestError> {
        let source = b"value = 0\nitems: list[int] = 1\n";
        let bytes = write(source)?;
        let view = view_of(&bytes)?;
        let rows = type_facts(&view)?;
        // Rows: the `list` builtin leaf, the integer leaf, the plain anchor
        // fact, then the `items` fact.
        if rows.len() != 4 || rows[3].record.tag != SemanticTypeTag::Apply {
            return Err(TestError::Entity { ordinal: 3 });
        }
        let payload = view.type_fact_payload().ok_or(TestError::Tail)?;
        if last_type_children(payload, &rows)? != vec![0, 1] {
            return Err(TestError::Tail);
        }
        let extension = python_extension(&view, 1)?;
        if extension.dynamic_confidence != compiler_ir::Confidence::Indexed {
            return Err(TestError::Extension { ordinal: 0 });
        }
        Ok(())
    }

    /// `dict[str, int]` lowers to an `Apply` row over the `dict` builtin row
    /// plus its key and value rows.
    #[test]
    fn dict_annotation_lowers_apply_over_key_and_value_rows() -> Result<(), TestError> {
        let source = b"value = 0\nlookup: dict[str, int] = 1\n";
        let bytes = write(source)?;
        let view = view_of(&bytes)?;
        let rows = type_facts(&view)?;
        if rows.len() != 5 || rows[4].record.tag != SemanticTypeTag::Apply {
            return Err(TestError::Entity { ordinal: 4 });
        }
        let payload = view.type_fact_payload().ok_or(TestError::Tail)?;
        if last_type_children(payload, &rows)? != vec![2, 0, 1] {
            return Err(TestError::Tail);
        }
        Ok(())
    }

    /// `tuple[int, str]` lowers to a `Tuple` row over its element rows.
    #[test]
    fn tuple_annotation_lowers_tuple_row_over_element_rows() -> Result<(), TestError> {
        let source = b"value = 0\npair: tuple[int, str] = 1\n";
        let bytes = write(source)?;
        let view = view_of(&bytes)?;
        let rows = type_facts(&view)?;
        if rows.len() != 4 || rows[3].record.tag != SemanticTypeTag::Tuple {
            return Err(TestError::Entity { ordinal: 3 });
        }
        let payload = view.type_fact_payload().ok_or(TestError::Tail)?;
        if last_type_children(payload, &rows)? != vec![0, 1] {
            return Err(TestError::Tail);
        }
        Ok(())
    }

    /// `Callable[[int], str]` lowers to a `FunctionPointer` row over the
    /// parameter row and the result row, with the result flag committed.
    #[test]
    fn callable_annotation_lowers_function_pointer_row() -> Result<(), TestError> {
        let source = b"value = 0\nhandler: Callable[[int], str] = None\n";
        let bytes = write(source)?;
        let view = view_of(&bytes)?;
        let rows = type_facts(&view)?;
        if rows.len() != 4 || rows[3].record.tag != SemanticTypeTag::FunctionPointer {
            return Err(TestError::Entity { ordinal: 3 });
        }
        if rows[3].record.payload1 != compiler_ir::SemanticTypeRecord::RESULT_FLAG {
            return Err(TestError::Entity { ordinal: 3 });
        }
        let payload = view.type_fact_payload().ok_or(TestError::Tail)?;
        if last_type_children(payload, &rows)? != vec![0, 1] {
            return Err(TestError::Tail);
        }
        Ok(())
    }

    /// `Optional[int]` lowers to a union row over the element row and the
    /// `None` builtin row.
    #[test]
    fn optional_annotation_lowers_union_with_none() -> Result<(), TestError> {
        let source = b"value = 0\nmaybe: Optional[int] = None\n";
        let bytes = write(source)?;
        let view = view_of(&bytes)?;
        let rows = type_facts(&view)?;
        if rows.len() != 4 || rows[3].record.tag != SemanticTypeTag::Union {
            return Err(TestError::Entity { ordinal: 3 });
        }
        let payload = view.type_fact_payload().ok_or(TestError::Tail)?;
        if last_type_children(payload, &rows)? != vec![0, 1] {
            return Err(TestError::Tail);
        }
        Ok(())
    }

    /// A generic class application lowers to an `Apply` row whose base is
    /// the class's backward fact row.
    #[test]
    fn generic_class_annotation_lowers_apply_over_nominal_row() -> Result<(), TestError> {
        let source = b"class Box:\n    pass\n\nboxed: Box[int] = 1\n";
        let bytes = write(source)?;
        let view = view_of(&bytes)?;
        let rows = type_facts(&view)?;
        // Rows: the integer leaf, the fact row of `boxed`, ... the class row
        // is fact ordinal 0 and the anonymous pool starts at 128.
        if rows[2].record.tag != SemanticTypeTag::Apply {
            return Err(TestError::Entity { ordinal: 2 });
        }
        let payload = view.type_fact_payload().ok_or(TestError::Tail)?;
        if last_type_children(payload, &rows)? != vec![0, 128] {
            return Err(TestError::Tail);
        }
        Ok(())
    }

    /// A nested compound (`dict[str, list[int]]`) expresses the inner list
    /// as one pooled anonymous row and reuses it as the outer child.
    #[test]
    fn nested_compound_annotation_lowers_pooled_inner_row() -> Result<(), TestError> {
        let source = b"value = 0\ndeep: dict[str, list[int]] = 1\n";
        let bytes = write(source)?;
        let view = view_of(&bytes)?;
        let rows = type_facts(&view)?;
        if rows.len() != 6 || rows[5].record.tag != SemanticTypeTag::Apply {
            return Err(TestError::Entity { ordinal: 5 });
        }
        let payload = view.type_fact_payload().ok_or(TestError::Tail)?;
        // Children: the dict builtin, the str leaf, and the pooled inner
        // `list[int]` application row.
        if last_type_children(payload, &rows)? != vec![3, 0, 2] {
            return Err(TestError::Tail);
        }
        Ok(())
    }

    /// The live pyrefly authority fills the inferred constant, upgrades the
    /// local call to `Oracle` and the resolved import call to `Import`, and
    /// leaves the checker-proved-nothing parameter exactly `Unannotated`.
    #[test]
    fn checker_report_fills_inference_and_upgrades_occurrences() -> Result<(), TestError> {
        let source = b"from json import dumps\n\ncounter = 42\n\ndef scale(factor):\n    return factor * 2\n\nscale(counter)\ndumps(counter)\n";
        let facts = fixture(source)?;
        let adapter = Pyrefly::uvx();
        if !adapter.is_available() {
            return Ok(());
        }
        let report = match adapter.analyze(source, PythonVersion::Python314, &facts) {
            Ok(report) => report,
            Err(
                compiler_languages_python::CheckerError::Spawn { .. }
                | compiler_languages_python::CheckerError::OutputLimit { .. }
                | compiler_languages_python::CheckerError::Exit { .. },
            ) => return Ok(()),
            Err(fault) => return Err(TestError::Checker(fault)),
        };
        let bytes = write_with(source, Some(&report))?;
        let view = view_of(&bytes)?;

        // Fact ordinals: dumps alias = 0, counter = 1, factor param = 2,
        // scale = 3. No compound rows, so type row index equals the ordinal.
        let rows = type_facts(&view)?;
        let counter = &rows[1].record;
        if counter.tag != SemanticTypeTag::Primitive
            || counter.payload0 != u32::from(PrimitiveShape::Integer)
        {
            return Err(TestError::Entity { ordinal: 1 });
        }
        if python_extension(&view, 1)?.dynamic_confidence != compiler_ir::Confidence::Compiler {
            return Err(TestError::Extension { ordinal: 1 });
        }
        let factor = &rows[2].record;
        if factor.tag != SemanticTypeTag::Unknown
            || factor.payload0 != u32::from(TypeReason::Unannotated)
        {
            return Err(TestError::Entity { ordinal: 2 });
        }
        if python_extension(&view, 2)?.dynamic_confidence != compiler_ir::Confidence::Syntactic {
            return Err(TestError::Extension { ordinal: 2 });
        }

        // The local call resolves at `Oracle`; the resolved import call is a
        // foreign `pypi` package key at `Import`.
        let occurrences = occurrences(&view)?;
        if occurrences.len() != 2 {
            return Err(TestError::Count {
                expected: 2,
                actual: occurrences.len(),
            });
        }
        let local = &occurrences[0];
        if local.occurrence.confidence != compiler_ir::OccurrenceConfidence::Oracle
            || !matches!(
                local.occurrence.target,
                OccurrenceTarget::Local(target) if target.raw == 3
            )
        {
            return Err(TestError::Entity { ordinal: 3 });
        }
        let imported = &occurrences[1];
        let compiler_ir::OccurrenceTarget::Foreign(key) = &imported.occurrence.target else {
            return Err(TestError::Entity { ordinal: 0 });
        };
        if imported.occurrence.confidence != compiler_ir::OccurrenceConfidence::Import
            || key.path != "json"
            || key.display != "dumps"
        {
            return Err(TestError::Tail);
        }
        Ok(())
    }

    /// A checker-inferred class instance names that class's nominal row.
    #[test]
    fn checker_inferred_instance_names_the_class_row() -> Result<(), TestError> {
        let source = b"class Node:\n    pass\n\nnode = Node()\n";
        let facts = fixture(source)?;
        let adapter = Pyrefly::uvx();
        if !adapter.is_available() {
            return Ok(());
        }
        let report = match adapter.analyze(source, PythonVersion::Python314, &facts) {
            Ok(report) => report,
            Err(
                compiler_languages_python::CheckerError::Spawn { .. }
                | compiler_languages_python::CheckerError::OutputLimit { .. }
                | compiler_languages_python::CheckerError::Exit { .. },
            ) => return Ok(()),
            Err(fault) => return Err(TestError::Checker(fault)),
        };
        let bytes = write_with(source, Some(&report))?;
        let view = view_of(&bytes)?;
        let rows = type_facts(&view)?;
        // node = fact 1; its inferred `Node` names the class row 0.
        let expected = Some(compiler_ir::NominalRef::Local(compiler_ir::EntityId::new(0)));
        if rows[1].record.tag != SemanticTypeTag::Nominal || rows[1].record.nominal != expected {
            return Err(TestError::Entity { ordinal: 1 });
        }
        if python_extension(&view, 1)?.dynamic_confidence != compiler_ir::Confidence::Compiler {
            return Err(TestError::Extension { ordinal: 1 });
        }
        Ok(())
    }
}
