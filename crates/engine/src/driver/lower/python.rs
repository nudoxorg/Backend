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
    Annotation, AnnotationFact, AnnotationPosition, CheckerError, CheckerReport, ClassForm,
    DeclarationFact, DeclarationKind, ExtractionError, InferredType, LiteralValue, ModuleFacts,
    OccurrenceFact, OccurrenceReceiver, ParameterKind, Pyrefly, ReceiverKind, Span, SymbolOutcome,
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

mod emit;

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

/// `self` on an instance method and `cls` on a classmethod are receivers, not
/// parameters. Any other parameter with those names stays.
fn is_receiver_parameter(receiver: ReceiverKind, name: &str) -> bool {
    match receiver {
        ReceiverKind::Plain => name == "self",
        ReceiverKind::ClassMethod => name == "cls",
        ReceiverKind::StaticMethod | ReceiverKind::Property => false,
    }
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
    let key = ForeignKey::new(ForeignOrigin::Package(lineage), path, display, None)
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
        Some(EntityKind::Function),
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
