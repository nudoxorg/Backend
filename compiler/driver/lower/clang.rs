//! Defines lower clang behavior for `compiler-driver`, whose purpose is to run bounded native toolchains and lower their output into canonical IR.
//! This module owns the lower clang invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//!
//! Two collectors coexist during the scanner retirement window:
//!
//! - [`Authority`] admits the direct libclang authority's typed facts —
//!   declarations, type uses with their closed declared-type recipes,
//!   resolved references, and documentation — into the emission lanes.
//!   This is the production path for `Language::Clang`.
//! - The interim keyword scanner below remains the documented red-gate
//!   retirement target only; no production lowering reads it anymore.
#![allow(
    clippy::indexing_slicing,
    reason = "statement and field word runs are split from proven separator positions before indexing, and every run slot is admitted before it is read"
)]

use compiler_ir::PrimitiveType;
use compiler_ir_vocabulary::{Confidence, EntityKind, ReferenceKind, SemanticProductConstructor};

use crate::{
    lower::{
        FactSet, FactType, LEAF_PRODUCT, MAX_EMISSION_FACTS, MAX_EMISSION_OCCURRENCES,
        OccurrenceSlot, SemanticFact, UnsupportedDeclaration, UnsupportedLane, UnsupportedReason,
        push_fact,
        scanner::{DeclarationScanner, SyntaxToken},
    },
    native::clang::{
        ClangEmissionLane, ClangFact, ClangFailure, ClangReferenceKind, ClangSourceSpan,
        ClangTypeRecipe, ClangTypeUseResolution, SemanticKind,
    },
    types::LoweringUnsupported,
};

/// Dense staging cell of one authoritative declaration fact.
struct EntityStage<'source> {
    name: &'source str,
    kind: SemanticKind,
    span: ClangSourceSpan,
    owner: ClangSourceSpan,
}

impl EntityStage<'static> {
    const DEFAULT: Self = Self {
        name: "",
        kind: SemanticKind::Function,
        span: ClangSourceSpan { start: 0, end: 0 },
        owner: ClangSourceSpan { start: 0, end: 0 },
    };
}

/// Dense staging cell of one authoritative type-use fact.
struct UseStage<'source> {
    spelling: &'source str,
    resolved: bool,
    recipe: ClangTypeRecipe,
    span: ClangSourceSpan,
    owner: ClangSourceSpan,
}

impl UseStage<'static> {
    const DEFAULT: Self = Self {
        spelling: "",
        resolved: false,
        recipe: ClangTypeRecipe::None,
        span: ClangSourceSpan { start: 0, end: 0 },
        owner: ClangSourceSpan { start: 0, end: 0 },
    };
}

/// Dense staging cell of one authoritative reference fact.
struct RefStage<'source> {
    target: &'source str,
    kind: ClangReferenceKind,
    use_span: ClangSourceSpan,
}

impl RefStage<'static> {
    const DEFAULT: Self = Self {
        target: "",
        kind: ClangReferenceKind::VariableUse,
        use_span: ClangSourceSpan { start: 0, end: 0 },
    };
}

/// Caller-owned bounded staging lanes for one authority analysis.
///
/// Facts stream in traversal order while the analysis runs; [`Authority::seal`]
/// resolves owner extents against the completed entity set and admits the
/// ordered declaration and occurrence facts into the emission lanes.
pub(crate) struct Authority<'source> {
    entities: [EntityStage<'source>; MAX_EMISSION_FACTS],
    entity_len: usize,
    uses: [UseStage<'source>; MAX_EMISSION_OCCURRENCES],
    use_len: usize,
    refs: [RefStage<'source>; MAX_EMISSION_OCCURRENCES],
    ref_len: usize,
    rejected: Option<ClangFailure>,
    documentation: usize,
}

impl Default for Authority<'_> {
    fn default() -> Self {
        Self::new()
    }
}

impl<'source> Authority<'source> {
    pub(crate) fn new() -> Self {
        Self {
            entities: [EntityStage::DEFAULT; MAX_EMISSION_FACTS],
            entity_len: 0,
            uses: [UseStage::DEFAULT; MAX_EMISSION_OCCURRENCES],
            use_len: 0,
            refs: [RefStage::DEFAULT; MAX_EMISSION_OCCURRENCES],
            ref_len: 0,
            rejected: None,
            documentation: 0,
        }
    }

    /// Receives one typed fact from the direct libclang analysis.
    ///
    /// Lane overflow is retained as the exact typed rejection instead of
    /// truncating a proven fact; [`Authority::seal`] surfaces it.
    #[expect(
        clippy::indexing_slicing,
        reason = "each lane ordinal is admitted below its dense bound before the single fixed-capacity slot write"
    )]
    pub(crate) fn record(&mut self, fact: ClangFact<'source>) {
        if self.rejected.is_some() {
            return;
        }
        match fact {
            ClangFact::Entity(entity) => {
                if self.entity_len == MAX_EMISSION_FACTS {
                    self.rejected = Some(ClangFailure::EmissionLaneCapacity {
                        lane: ClangEmissionLane::Entity,
                        limit: MAX_EMISSION_FACTS,
                        observed: self.entity_len + 1,
                    });
                    return;
                }
                self.entities[self.entity_len] = EntityStage {
                    name: entity.name,
                    kind: entity.kind,
                    span: entity.span,
                    owner: entity.owner,
                };
                self.entity_len += 1;
            }
            ClangFact::TypeUse(use_fact) => {
                if self.use_len == MAX_EMISSION_OCCURRENCES {
                    self.rejected = Some(ClangFailure::EmissionLaneCapacity {
                        lane: ClangEmissionLane::TypeUse,
                        limit: MAX_EMISSION_OCCURRENCES,
                        observed: self.use_len + 1,
                    });
                    return;
                }
                self.uses[self.use_len] = UseStage {
                    spelling: use_fact.name,
                    resolved: matches!(
                        use_fact.resolution,
                        ClangTypeUseResolution::Declaration { .. }
                    ),
                    recipe: use_fact.recipe,
                    span: use_fact.span,
                    owner: use_fact.owner,
                };
                self.use_len += 1;
            }
            ClangFact::Reference(reference) => {
                if self.ref_len == MAX_EMISSION_OCCURRENCES {
                    self.rejected = Some(ClangFailure::EmissionLaneCapacity {
                        lane: ClangEmissionLane::Reference,
                        limit: MAX_EMISSION_OCCURRENCES,
                        observed: self.ref_len + 1,
                    });
                    return;
                }
                self.refs[self.ref_len] = RefStage {
                    target: reference.target,
                    kind: reference.kind,
                    use_span: reference.use_span,
                };
                self.ref_len += 1;
            }
            ClangFact::Documentation(_) => self.documentation += 1,
            ClangFact::Diagnostic(_) => {}
        }
    }

    /// Admits the staged authority facts into the emission lanes.
    ///
    /// Declarations are pushed in traversal order; every staged type use and
    /// reference becomes one owned occurrence whose relative span is measured
    /// from the innermost containing declaration's extent start. The closed
    /// declared-type recipe of each entity's own type use becomes the
    /// entity's declared-type fact.
    #[expect(
        clippy::indexing_slicing,
        reason = "every lane position is admitted below its dense bound before it is read or written"
    )]
    pub(crate) fn seal(&self, facts: &mut FactSet<'source>) -> Result<(), AuthoritySealFault> {
        if let Some(rejection) = &self.rejected {
            return Err(AuthoritySealFault::Clang(rejection.clone()));
        }
        for ordinal in 0..self.entity_len {
            let entity = &self.entities[ordinal];
            if entity.name.is_empty() {
                // An authority declaration without a provable name is
                // skipped, never guessed, matching the closed emission law.
                continue;
            }
            let fact_type = self.declared_type(ordinal);
            let pushed = facts.push(SemanticFact::new(
                entity_kind(entity.kind),
                entity.name.as_bytes(),
                fact_type,
                entity_constructor(entity.kind),
            ));
            if pushed.is_err() {
                return Err(AuthoritySealFault::Clang(
                    ClangFailure::EmissionEntityInvalid {
                        start: entity.span.start,
                        end: entity.span.end,
                    },
                ));
            }
        }
        for ordinal in 0..self.use_len {
            let use_stage = &self.uses[ordinal];
            let Some(owner) = self.owner_ordinal(use_stage.span) else {
                return Err(AuthoritySealFault::Clang(
                    ClangFailure::EmissionOwnerUnresolved {
                        start: use_stage.span.start,
                        end: use_stage.span.end,
                    },
                ));
            };
            let Some(span) = use_stage.span.relative_to(self.entities[owner].owner) else {
                return Err(AuthoritySealFault::Clang(
                    ClangFailure::EmissionOwnerUnresolved {
                        start: use_stage.span.start,
                        end: use_stage.span.end,
                    },
                ));
            };
            let pushed = facts.push_occurrence(OccurrenceSlot {
                owner: ordinal_to_owner(owner),
                ecosystem: "c",
                path: use_stage.spelling,
                display: use_stage.spelling,
                target_kind: None,
                kind: ReferenceKind::TypeReference,
                confidence: if use_stage.resolved {
                    Confidence::Oracle
                } else {
                    Confidence::Syntactic
                },
                span_start: span.start,
                span_end: span.end,
            });
            if let Err(fault) = pushed {
                return Err(AuthoritySealFault::occurrence_fault(
                    ClangEmissionLane::TypeUse,
                    use_stage.span,
                    fault,
                ));
            }
        }
        for ordinal in 0..self.ref_len {
            let ref_stage = &self.refs[ordinal];
            let Some(owner) = self.owner_ordinal(ref_stage.use_span) else {
                return Err(AuthoritySealFault::Clang(
                    ClangFailure::EmissionOwnerUnresolved {
                        start: ref_stage.use_span.start,
                        end: ref_stage.use_span.end,
                    },
                ));
            };
            let Some(span) = ref_stage.use_span.relative_to(self.entities[owner].owner) else {
                return Err(AuthoritySealFault::Clang(
                    ClangFailure::EmissionOwnerUnresolved {
                        start: ref_stage.use_span.start,
                        end: ref_stage.use_span.end,
                    },
                ));
            };
            let pushed = facts.push_occurrence(OccurrenceSlot {
                owner: ordinal_to_owner(owner),
                ecosystem: "c",
                path: ref_stage.target,
                display: ref_stage.target,
                target_kind: reference_target_kind(ref_stage.kind),
                kind: reference_kind(ref_stage.kind),
                confidence: Confidence::Oracle,
                span_start: span.start,
                span_end: span.end,
            });
            if let Err(fault) = pushed {
                return Err(AuthoritySealFault::occurrence_fault(
                    ClangEmissionLane::Reference,
                    ref_stage.use_span,
                    fault,
                ));
            }
        }
        Ok(())
    }

    /// The closed declared-type recipe of the entity's own type use: the
    /// latest type use that shares the entity's owner extent and precedes
    /// the entity's name.
    fn declared_type(&self, ordinal: usize) -> FactType {
        let entity = &self.entities[ordinal];
        let mut best: Option<&UseStage<'source>> = None;
        for use_stage in &self.uses[..self.use_len] {
            if use_stage.owner != entity.owner || use_stage.span.end > entity.span.start {
                continue;
            }
            best = match best {
                Some(current) if current.span.end > use_stage.span.end => Some(current),
                _ => Some(use_stage),
            };
        }
        match best.map(|use_stage| use_stage.recipe) {
            Some(ClangTypeRecipe::String) => FactType::Primitive(PrimitiveType::String),
            Some(ClangTypeRecipe::Bool) => FactType::Primitive(PrimitiveType::Bool),
            Some(ClangTypeRecipe::Integer) => FactType::Primitive(PrimitiveType::I32),
            Some(ClangTypeRecipe::None) | None => FactType::Opaque,
        }
    }

    /// The innermost declaration whose extent contains the fact's use span;
    /// ties resolve to the earliest declared ordinal.
    fn owner_ordinal(&self, span: ClangSourceSpan) -> Option<usize> {
        let mut best: Option<usize> = None;
        let mut best_extent = 0_u32;
        for ordinal in 0..self.entity_len {
            let entity = &self.entities[ordinal];
            if entity.owner.start > span.start || span.end > entity.owner.end {
                continue;
            }
            let extent = entity.owner.end - entity.owner.start;
            let wins = match best {
                None => true,
                Some(_) => extent < best_extent,
            };
            if wins {
                best = Some(ordinal);
                best_extent = extent;
            }
        }
        best
    }
}

/// Exact failure of one authority seal, retaining the typed cause.
#[derive(Debug)]
pub(crate) enum AuthoritySealFault {
    /// The analysis authority rejected or could not hold a proven fact.
    Clang(ClangFailure),
}

impl AuthoritySealFault {
    fn occurrence_fault(
        lane: ClangEmissionLane,
        span: ClangSourceSpan,
        fault: crate::lower::OccurrenceFault,
    ) -> Self {
        match fault {
            crate::lower::OccurrenceFault::Capacity { limit, observed } => {
                Self::Clang(ClangFailure::EmissionLaneCapacity {
                    lane,
                    limit,
                    observed,
                })
            }
            crate::lower::OccurrenceFault::Owner { .. }
            | crate::lower::OccurrenceFault::Span { .. } => {
                Self::Clang(ClangFailure::EmissionOwnerUnresolved {
                    start: span.start,
                    end: span.end,
                })
            }
            crate::lower::OccurrenceFault::Key(_) => {
                Self::Clang(ClangFailure::EmissionTargetPath {
                    start: span.start,
                    end: span.end,
                })
            }
        }
    }
}

/// Maps the closed authority kind registry onto the canonical entity kinds.
/// The registries share one discriminant set by construction.
const fn entity_kind(kind: SemanticKind) -> EntityKind {
    match kind {
        SemanticKind::Function => EntityKind::Function,
        SemanticKind::Constant => EntityKind::Constant,
        SemanticKind::Record => EntityKind::Record,
        SemanticKind::Module => EntityKind::Module,
        SemanticKind::Field => EntityKind::Field,
        SemanticKind::Alias => EntityKind::Alias,
        SemanticKind::Trait => EntityKind::Trait,
        SemanticKind::Implementation => EntityKind::Implementation,
        SemanticKind::Enum => EntityKind::Enum,
        SemanticKind::Variant => EntityKind::Variant,
        SemanticKind::Static => EntityKind::Static,
        SemanticKind::Reexport => EntityKind::Reexport,
        SemanticKind::Parameter => EntityKind::Parameter,
    }
}

/// The closed structural constructor of one canonical declaration kind.
const fn entity_constructor(kind: SemanticKind) -> SemanticProductConstructor {
    match kind {
        SemanticKind::Function => SemanticProductConstructor::function(0, 0),
        SemanticKind::Record => SemanticProductConstructor::PRODUCT,
        SemanticKind::Enum => SemanticProductConstructor::UNION,
        SemanticKind::Trait => SemanticProductConstructor::INTERSECTION,
        _ => LEAF_PRODUCT,
    }
}

/// The closed occurrence category of one authoritative reference.
const fn reference_kind(kind: ClangReferenceKind) -> ReferenceKind {
    match kind {
        ClangReferenceKind::FunctionCall => ReferenceKind::FunctionCall,
        ClangReferenceKind::MethodCall => ReferenceKind::MethodCall,
        ClangReferenceKind::VariableUse => ReferenceKind::VariableUse,
        ClangReferenceKind::FieldAccess => ReferenceKind::FieldAccess,
        ClangReferenceKind::MacroInvocation => ReferenceKind::MacroInvocation,
    }
}

/// The expected declaration kind of one authoritative reference target,
/// when the reference role proves one.
const fn reference_target_kind(kind: ClangReferenceKind) -> Option<EntityKind> {
    match kind {
        ClangReferenceKind::FunctionCall | ClangReferenceKind::MethodCall => {
            Some(EntityKind::Function)
        }
        ClangReferenceKind::FieldAccess => Some(EntityKind::Field),
        ClangReferenceKind::VariableUse => Some(EntityKind::Static),
        ClangReferenceKind::MacroInvocation => None,
    }
}

/// Converts an admitted entity ordinal into its owner coordinate.
#[expect(
    clippy::as_conversions,
    reason = "the entity ordinal is bounded by MAX_EMISSION_FACTS and widens totally to the owner coordinate width"
)]
const fn ordinal_to_owner(ordinal: usize) -> u32 {
    ordinal as u32
}

/// Word-run marker for one pointer declarator or parameter-list opener.
const STAR: &[u8] = b"*";
/// Word-run marker for one parenthesized group.
const PAREN: &[u8] = b"(";
/// Word-run marker for one array declarator bracket.
const BRACKET: &[u8] = b"[";

/// Emits every provable top-level C declaration: functions, struct records
/// with their named fields, enums with their enumerators, typedefs, and
/// closed constants.
///
/// Pointer, array, record, and compound object types are recorded exactly as
/// unsupported; a declaration is never guessed into a closed recipe.
pub(super) fn collect<'source>(
    source: &'source [u8],
    facts: &mut FactSet<'source>,
    unsupported: &mut UnsupportedLane<'source>,
) -> Result<(), LoweringUnsupported> {
    let mut scanner = DeclarationScanner::new(source);
    // One reused scratch run per top-level statement, field, or enumerator.
    let mut run: Vec<&'source [u8]> = Vec::new();
    let mut mode = Mode::Top;
    while let Some(scanned) = scanner.next() {
        match (mode, scanned.brace_depth, scanned.token) {
            (Mode::Top, 0, SyntaxToken::Word(word)) => run.push(word),
            (Mode::Top, 0, SyntaxToken::Symbol(b'*')) => run.push(STAR),
            (Mode::Top, 0, SyntaxToken::Symbol(b'(')) => run.push(PAREN),
            (Mode::Top, 0, SyntaxToken::Symbol(b'['))
            | (Mode::Top, 0, SyntaxToken::Symbol(b']')) => {
                run.push(BRACKET);
            }
            (Mode::Top, 0, SyntaxToken::Symbol(b';')) => {
                top_statement(&run, facts, unsupported);
                run.clear();
            }
            (Mode::Top, 0, SyntaxToken::Symbol(b'{')) => {
                mode = body_mode(&run);
                top_statement(&run, facts, unsupported);
                run.clear();
            }
            (Mode::Fields, 1, SyntaxToken::Word(word)) => run.push(word),
            (Mode::Fields, 1, SyntaxToken::Symbol(b'*')) => run.push(STAR),
            (Mode::Fields, 1, SyntaxToken::Symbol(b'['))
            | (Mode::Fields, 1, SyntaxToken::Symbol(b']')) => {
                run.push(BRACKET);
            }
            (Mode::Fields, 1, SyntaxToken::Symbol(b'{')) => {
                // A nested record definition's header words never describe
                // the enclosing record's next field.
                run.clear();
            }
            (Mode::Fields, 1, SyntaxToken::Symbol(b';')) => {
                field(&run, facts);
                run.clear();
            }
            (Mode::Enumerators, 1, SyntaxToken::Word(word)) => run.push(word),
            (Mode::Enumerators, 1, SyntaxToken::Symbol(b',')) => {
                enumerator(&run, facts);
                run.clear();
            }
            // Any deeper brace group (function bodies, nested records) is
            // structurally invisible to this top-level scan.
            _ => {}
        }
    }
    Ok(())
}

/// Scan mode after one top-level `{`.
#[derive(Clone, Copy, Eq, PartialEq)]
enum Mode {
    Top,
    /// Named struct fields, one `;`-terminated declaration each.
    Fields,
    /// Enumerators, one `,`-separated declaration each.
    Enumerators,
}

fn body_mode(run: &[&[u8]]) -> Mode {
    match run.first().copied() {
        Some(b"struct") => Mode::Fields,
        Some(b"enum") => Mode::Enumerators,
        _ => Mode::Top,
    }
}

/// Lowers one complete top-level statement or body opener.
fn top_statement<'source>(
    run: &[&'source [u8]],
    facts: &mut FactSet<'source>,
    unsupported: &mut UnsupportedLane<'source>,
) {
    let Some(last) = run.last().copied() else {
        return;
    };
    match run.first().copied() {
        Some(b"typedef") => {
            // A struct/enum typedef's alias name follows its body, outside
            // this run, so no compact alias fact is provable here.
            if run.len() >= 3
                && run.get(1) != Some(&b"struct".as_slice())
                && run.get(1) != Some(&b"enum".as_slice())
            {
                let _ = push_fact(
                    facts,
                    SemanticFact::new(EntityKind::Alias, last, typedef_type(run), LEAF_PRODUCT),
                );
            }
        }
        Some(b"struct") | Some(b"enum") => {
            let (kind, constructor) = if run.first() == Some(&b"enum".as_slice()) {
                (EntityKind::Enum, SemanticProductConstructor::UNION)
            } else {
                (EntityKind::Record, SemanticProductConstructor::PRODUCT)
            };
            if let Some(name) = run.get(1).copied().filter(|word| *word != STAR) {
                let _ = push_fact(
                    facts,
                    SemanticFact::new(kind, name, FactType::Opaque, constructor),
                );
            }
        }
        Some(b"const") => {
            if let Some(name) = object_name(run) {
                match const_type(run) {
                    Some(primitive) => {
                        let _ = push_fact(
                            facts,
                            SemanticFact::new(
                                EntityKind::Constant,
                                name,
                                FactType::Primitive(primitive),
                                LEAF_PRODUCT,
                            ),
                        );
                    }
                    None => unsupported.record(UnsupportedDeclaration {
                        name,
                        reason: UnsupportedReason::ClosedValueType,
                    }),
                }
            }
        }
        _ => {
            if let Some(paren) = run.iter().position(|word| *word == PAREN) {
                // A pointer declarator directly after the parameter-list
                // opener is a function-pointer object, not a function.
                if run.get(paren + 1) == Some(&STAR) {
                    if let Some(name) = object_name(run) {
                        unsupported.record(UnsupportedDeclaration {
                            name,
                            reason: UnsupportedReason::NeedsFrontend,
                        });
                    }
                    return;
                }
                // A function definition or prototype: its name is the word
                // before the parameter list, and its return type is the
                // directly preceding single word.
                let mut name_end = paren;
                while name_end > 0 && run[name_end - 1] == STAR {
                    name_end -= 1;
                }
                let Some(name) = run
                    .get(name_end.wrapping_sub(1))
                    .copied()
                    .filter(|word| *word != PAREN)
                else {
                    return;
                };
                let mut fact_type = FactType::Opaque;
                if name_end == 2 {
                    if let Some(ret) = run.first() {
                        fact_type = match *ret {
                            b"int" => FactType::Primitive(PrimitiveType::I32),
                            b"bool" | b"_Bool" => FactType::Primitive(PrimitiveType::Bool),
                            _ => FactType::Opaque,
                        };
                    }
                }
                let _ = push_fact(
                    facts,
                    SemanticFact::new(
                        EntityKind::Function,
                        name,
                        fact_type,
                        SemanticProductConstructor::function(0, 0),
                    ),
                );
                return;
            }
            // A plain object definition with a proven name: object storage
            // stays outside this compact slice and is recorded exactly.
            if let Some(name) = object_name(run) {
                unsupported.record(UnsupportedDeclaration {
                    name,
                    reason: UnsupportedReason::NeedsFrontend,
                });
            }
        }
    }
}

/// Lowers one `;`-terminated struct field: its name is the final word and its
/// closed type comes from the leading type words. An array declarator never
/// commits its element primitive.
fn field<'source>(run: &[&'source [u8]], facts: &mut FactSet<'source>) {
    if run.contains(&PAREN) {
        // Function-pointer fields need the real frontend's type authority.
        return;
    }
    let Some(name) = object_name(run) else {
        return;
    };
    let fact_type = if run.contains(&BRACKET) {
        FactType::Opaque
    } else {
        match run.first().copied() {
            Some(b"int") if !run.contains(&STAR) => FactType::Primitive(PrimitiveType::I32),
            Some(b"bool") | Some(b"_Bool") => FactType::Primitive(PrimitiveType::Bool),
            Some(b"char") if run.contains(&STAR) => FactType::Primitive(PrimitiveType::String),
            _ => FactType::Opaque,
        }
    };
    let _ = push_fact(
        facts,
        SemanticFact::new(EntityKind::Field, name, fact_type, LEAF_PRODUCT),
    );
}

/// Lowers one enumerator name: the run's first word, which precedes any
/// explicit value assignment.
fn enumerator<'source>(run: &[&'source [u8]], facts: &mut FactSet<'source>) {
    if let Some(name) = run
        .first()
        .copied()
        .filter(|word| *word != STAR && *word != PAREN)
    {
        let _ = push_fact(
            facts,
            SemanticFact::new(EntityKind::Variant, name, FactType::Opaque, LEAF_PRODUCT),
        );
    }
}

/// The declared object name of a word run: its final plain word.
fn object_name<'run>(run: &[&'run [u8]]) -> Option<&'run [u8]> {
    run.last()
        .copied()
        .filter(|word| *word != STAR && *word != PAREN)
}

/// Classifies a `const` word run: `const char *NAME` text, `const int`
/// integers, `const bool` booleans; every other type, including array
/// declarators, stays unsupported.
fn const_type(run: &[&[u8]]) -> Option<PrimitiveType> {
    if run.contains(&BRACKET) {
        return None;
    }
    match run.get(1).copied() {
        Some(b"int") if !run.contains(&STAR) => Some(PrimitiveType::I32),
        Some(b"bool") | Some(b"_Bool") => Some(PrimitiveType::Bool),
        Some(b"char") => Some(PrimitiveType::String),
        _ => None,
    }
}

/// Classifies a typedef target: a single spelled closed primitive keeps its
/// type; every other target, including array declarators, stays opaque.
fn typedef_type(run: &[&[u8]]) -> FactType {
    if run.contains(&BRACKET) {
        return FactType::Opaque;
    }
    match run.get(1).copied() {
        Some(b"int") if !run.contains(&STAR) => FactType::Primitive(PrimitiveType::I32),
        Some(b"bool") | Some(b"_Bool") => FactType::Primitive(PrimitiveType::Bool),
        _ => FactType::Opaque,
    }
}
