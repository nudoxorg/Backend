//! Traverses one direct libclang translation unit into caller-bounded semantic fact slots.
//! The visitor retains declarations, recursive types, references, documentation, includes, and diagnostics.
//! Every capacity boundary is explicit and the traversal has no scanner or process fallback.

use core::{
    ffi::c_void,
    ptr,
    sync::atomic::{AtomicBool, Ordering},
};

use std::collections::HashMap;

use clang_sys::{
    CXChildVisitResult, CXCursor, CXCursorKind as NativeCursorKind, CXDiagnostic,
    CXDiagnosticSeverity as NativeDiagnosticSeverity, CXType, CXTypeKind,
};

use crate::legacy::{
    ClangInput, ClangScratch, CollectError, ScratchLane,
    facts::{
        BuiltinClass, ClangFacts, DeclarationFact, DeclarationId, DeclarationKind, DefinitionState,
        DiagnosticFact, DiagnosticSeverity, IncludeFact, IntegerRank, MethodVirtuality,
        OverrideFact, ReferenceFact, ReferenceKind, ReferenceTarget, SourceDependencyKind,
        SourceSpan, StorageClass, SymbolIdentity, TypeEdge, TypeFact, TypeId, TypeKind,
        TypeQualifiers, TypeRelation,
    },
    ffi::{self, TranslationUnit},
};

/// Maximum number of parameter cursors deferred until the owner walk completes.
///
/// The stash receives one entry per visited parameter cursor — prototypes and
/// definitions both defer, and a macro-generated header can declare more
/// parameter cursors than declaration slots (measured: `eigen`'s
/// `Eigen/src/misc/lapacke.h` alone visits 32,769+ parameter cursors). The
/// bound therefore uses the same historical four-times ratio as the recursive
/// type, type-edge, and reference lanes, so the transient deferral never
/// fires before the committed declaration lane it feeds.
const PARAMETER_STASH_CAPACITY: usize = 4 * crate::legacy::facts::MAX_CLANG_FACTS;

/// One parameter cursor deferred until its owner's committed definition state is final.
#[derive(Clone, Copy)]
struct StashedParameter {
    /// The semantic-parent identity shared by a prototype and its definition.
    owner: Option<SymbolIdentity>,
    /// Whether the visiting declaration is a definition. A prototype's
    /// parameters stash `false` even when a definition committed earlier.
    owner_is_definition: bool,
    /// The visiting declaration's extent. Distinct declarations never share
    /// one, so this separates duplicate-prototype sets that share an owner
    /// identity and flag.
    parent_span: Option<SourceSpan>,
    /// The native cursor replayed after the walk to emit the selected run only.
    cursor: CXCursor,
}

/// One maximal, owner-homogeneous run of stashed parameter cursors in visit order.
struct ParameterRun {
    /// The semantic-parent identity shared by every parameter in the run.
    owner: Option<SymbolIdentity>,
    /// The visiting declaration's extent shared by every parameter in the run.
    parent_span: Option<SourceSpan>,
    /// Inclusive first stashed index.
    start: usize,
    /// Exclusive last stashed index.
    end: usize,
    /// Whether the visiting declaration is a definition.
    is_definition: bool,
}

/// Collects complete direct libclang facts into the caller-provided typed scratch arrays.
///
/// # Errors
///
/// Returns an exact native, source-coordinate, or caller-capacity failure. Missing libclang is
/// never replaced with an alternative parser or a partial scanner result.
pub fn collect<'scratch>(
    input: ClangInput<'_>,
    scratch: ClangScratch<'scratch>,
) -> Result<ClangFacts<'scratch>, CollectError> {
    collect_inner(input, scratch, None)
}

/// Collects direct libclang facts while observing an exact caller-owned cancellation flag.
///
/// # Errors
///
/// Returns [`CollectError::Cancelled`] before native loading or at the next cursor boundary when
/// the caller has set `cancellation`; every other failure has the same meaning as [`collect`].
pub fn collect_cancellable<'scratch>(
    input: ClangInput<'_>,
    scratch: ClangScratch<'scratch>,
    cancellation: &'_ AtomicBool,
) -> Result<ClangFacts<'scratch>, CollectError> {
    collect_inner(input, scratch, Some(cancellation))
}

/// Runs one collector transaction with an optional caller-controlled cancellation authority.
fn collect_inner<'scratch>(
    input: ClangInput<'_>,
    scratch: ClangScratch<'scratch>,
    cancellation: Option<&AtomicBool>,
) -> Result<ClangFacts<'scratch>, CollectError> {
    preflight(input)?;
    if cancelled(cancellation) {
        return Err(CollectError::Cancelled);
    }
    let unit = ffi::parse(input)?;
    let mut collector = Collector::new(&unit, scratch, cancellation);
    collector.collect_diagnostics()?;
    let state = ptr::from_mut(&mut collector).cast::<c_void>();
    unit.visit(visit, state);
    collector.finish()
}

/// Rejects source facts that cannot safely reach the native authority before library discovery.
fn preflight(input: ClangInput<'_>) -> Result<(), CollectError> {
    if input.source().contains(&0) {
        return Err(CollectError::SourceContainsNul);
    }
    u32::try_from(input.source().len()).map_err(|_| CollectError::SourceLengthTooLarge {
        observed: input.source().len(),
    })?;
    Ok(())
}

/// Reads one optional cancellation flag with acquire ordering at a native work boundary.
fn cancelled(cancellation: Option<&AtomicBool>) -> bool {
    cancellation.is_some_and(|flag| flag.load(Ordering::Acquire))
}

/// Mutable fact writers and their exact typed counts for one synchronous native traversal.
struct Collector<'unit, 'scratch> {
    unit: &'unit TranslationUnit,
    scratch: ClangScratch<'scratch>,
    declarations: usize,
    types: usize,
    type_edges: usize,
    references: usize,
    diagnostics: usize,
    includes: usize,
    overrides: usize,
    parameters: Vec<StashedParameter>,
    project_paths: Vec<Box<str>>,
    cancellation: Option<&'unit AtomicBool>,
    failure: Option<CollectError>,
}

impl<'unit, 'scratch> Collector<'unit, 'scratch> {
    /// Binds one live native unit to its caller-owned fact destination arrays.
    const fn new(
        unit: &'unit TranslationUnit,
        scratch: ClangScratch<'scratch>,
        cancellation: Option<&'unit AtomicBool>,
    ) -> Self {
        Self {
            unit,
            scratch,
            declarations: 0,
            types: 0,
            type_edges: 0,
            references: 0,
            diagnostics: 0,
            includes: 0,
            overrides: 0,
            parameters: Vec::new(),
            project_paths: Vec::new(),
            cancellation,
            failure: None,
        }
    }

    /// Streams native diagnostics before cursor traversal so syntax errors retain authority facts.
    fn collect_diagnostics(&mut self) -> Result<(), CollectError> {
        if cancelled(self.cancellation) {
            return Err(CollectError::Cancelled);
        }
        let unit = self.unit;
        unit.diagnostics(|diagnostic| self.record_diagnostic(diagnostic))
    }

    /// Records one traversal cursor and signals whether libclang should stop visiting children.
    fn observe(&mut self, cursor: CXCursor) -> bool {
        if cancelled(self.cancellation) {
            self.failure = Some(CollectError::Cancelled);
            return true;
        }
        if self.failure.is_some() {
            return true;
        }
        if let Err(failure) = self.record_cursor(cursor) {
            self.failure = Some(failure);
            return true;
        }
        false
    }

    /// Retains one cursor under exactly one semantic role determined by libclang's closed kind.
    fn record_cursor(&mut self, cursor: CXCursor) -> Result<(), CollectError> {
        let kind = TranslationUnit::cursor_kind(cursor);
        if kind == clang_sys::CXCursor_MacroDefinition {
            return self.record_declaration(cursor, DeclarationKind::Macro);
        }
        if let Some(dependency_kind) = source_dependency_kind(kind) {
            return self.record_source_dependency(cursor, dependency_kind);
        }
        if TranslationUnit::is_declaration(kind) {
            return self.record_declaration(cursor, declaration_kind(kind));
        }
        if let Some(reference_kind) = reference_kind(kind) {
            return self.record_reference(cursor, reference_kind);
        }
        Ok(())
    }

    /// Defers one parameter cursor, or commits one declaration fact with its direct type graph.
    fn record_declaration(
        &mut self,
        cursor: CXCursor,
        kind: DeclarationKind,
    ) -> Result<(), CollectError> {
        if kind == DeclarationKind::Parameter {
            return self.stash_parameter(cursor);
        }
        self.commit_declaration(cursor, kind)
    }

    /// Defers one parameter cursor until the walk completes, recording
    /// whether its visiting declaration is a definition.
    ///
    /// The flag comes from the lexical parent — the prototype or
    /// definition actually being walked — never from the committed owner
    /// state, so definition-before-prototype and duplicate-prototype
    /// orders attribute identically.
    fn stash_parameter(&mut self, cursor: CXCursor) -> Result<(), CollectError> {
        let owner = TranslationUnit::semantic_parent(cursor);
        let owner_is_definition = TranslationUnit::lexical_parent_is_definition(cursor);
        let parent_span = self.unit.lexical_parent_span(cursor)?;
        if self.parameters.len() >= PARAMETER_STASH_CAPACITY {
            return Err(CollectError::ScratchCapacity {
                lane: ScratchLane::Declarations,
                capacity: PARAMETER_STASH_CAPACITY,
                required: self.parameters.len().saturating_add(1),
            });
        }
        self.parameters.push(StashedParameter {
            owner,
            owner_is_definition,
            parent_span,
            cursor,
        });
        Ok(())
    }

    /// Retains one declaration and its complete direct recursive type graph when it is local.
    fn commit_declaration(
        &mut self,
        cursor: CXCursor,
        kind: DeclarationKind,
    ) -> Result<(), CollectError> {
        if kind == DeclarationKind::Record
            && TranslationUnit::template_cursor_kind(cursor) == clang_sys::CXCursor_ClassTemplate
        {
            return Ok(());
        }
        let canonical_identity = TranslationUnit::canonical_identity(cursor);
        let existing = canonical_identity.and_then(|identity| {
            self.scratch.declarations[..self.declarations]
                .iter()
                .position(|fact| fact.identity == Some(identity))
        });
        // A name-less foreign value-authority row exists only so a later
        // value reference can read declaration kind across a header. It must
        // never become the slot a main-source definition overwrites.
        let existing = existing.and_then(|index| {
            if foreign_value_authority_stub(&self.scratch.declarations[index]) {
                None
            } else {
                Some(index)
            }
        });
        if existing.is_some_and(|index| {
            !TranslationUnit::is_definition(cursor)
                || self.scratch.declarations[index].definition == DefinitionState::Definition
        }) {
            return Ok(());
        }
        let Some(span) = self.unit.cursor_span(cursor)? else {
            return Ok(());
        };
        let identity = TranslationUnit::cursor_identity(cursor);
        let type_root = self.collect_type(TranslationUnit::cursor_type(cursor))?;
        let enum_underlying = (kind == DeclarationKind::Enumeration)
            .then(|| self.collect_type(TranslationUnit::enum_underlying_type(cursor)))
            .transpose()?
            .flatten();
        let virtuality = if kind == DeclarationKind::Method {
            let (is_virtual, is_pure_virtual) = TranslationUnit::method_virtuality(cursor);
            if is_pure_virtual {
                MethodVirtuality::PureVirtual
            } else if is_virtual {
                MethodVirtuality::Virtual
            } else {
                MethodVirtuality::NonVirtual
            }
        } else {
            MethodVirtuality::NonVirtual
        };
        if kind == DeclarationKind::Method {
            self.record_overrides(cursor)?;
        }
        let id = match existing {
            Some(index) => self.scratch.declarations[index].id,
            None => DeclarationId {
                raw: u32::try_from(self.declarations).map_err(|_| {
                    CollectError::SlotOrdinalTooLarge {
                        lane: ScratchLane::Declarations,
                        observed: self.declarations,
                    }
                })?,
            },
        };
        let name = (!TranslationUnit::cursor_spelling_is_empty(cursor))
            .then(|| self.unit.name_span(cursor))
            .transpose()?
            .flatten();
        // An anonymous record or enumeration reports its own keyword (`struct`,
        // `union`, `class`, `enum`) as the spelling range at the declaration
        // start. That keyword is not a declared name: admitting it would mint
        // one shared `enum` identity for every anonymous enumeration in the
        // translation unit. Only a name strictly inside the extent counts.
        let name = (!matches!(kind, DeclarationKind::Record | DeclarationKind::Enumeration)
            || name.is_none_or(|name| name.start > span.start))
        .then_some(name)
        .flatten();
        let fact = DeclarationFact {
            id,
            kind,
            definition: definition_state(TranslationUnit::is_definition(cursor)),
            virtuality,
            identity,
            span,
            name,
            owner: TranslationUnit::semantic_parent(cursor),
            documentation: self.unit.documentation_span(cursor)?,
            storage: storage_class(TranslationUnit::storage_class(cursor)),
            type_root,
            enum_underlying,
        };
        if let Some(index) = existing {
            self.scratch.declarations[index] = fact;
            Ok(())
        } else {
            self.push_declaration(fact)
        }
    }

    /// Streams distinct overridden USR identities while the native array guard remains live.
    fn record_overrides(&mut self, cursor: CXCursor) -> Result<(), CollectError> {
        let Some(source) = TranslationUnit::cursor_identity(cursor) else {
            return Ok(());
        };
        let overridden = TranslationUnit::overridden_cursors(cursor);
        for target_cursor in overridden.as_slice() {
            let Some(target) = TranslationUnit::cursor_identity(*target_cursor) else {
                continue;
            };
            if self.scratch.overrides[..self.overrides]
                .iter()
                .any(|fact| fact.source == source && fact.target == target)
            {
                continue;
            }
            self.push_override(OverrideFact {
                source,
                target,
                target_file: self.unit.cursor_file_identity(*target_cursor),
            })?;
        }
        Ok(())
    }

    /// Retains a direct native reference with a local, foreign, or unresolved target identity.
    ///
    /// The retained span is the written spelling of the referenced entity at
    /// the use site — the callee token of a call, the member token of a
    /// member access, the identifier of a declaration reference — so a
    /// site's bytes are exactly the name it resolves. The reference-name
    /// range carries that token for every site class with a written name,
    /// except a call expression: there libclang degenerates to the whole
    /// expression extent (probe: `measure(buffer)` returned verbatim), so a
    /// degenerate range — empty or identical to the extent — falls back to
    /// the cursor's spelling-name range, which for a call is exactly the
    /// callee token. Only a site with neither range — the written name is
    /// produced inside a macro definition and maps outside the main source,
    /// a class with no name token at all — keeps the whole use extent; a
    /// reference is never dropped or truncated by the narrowing.
    fn record_reference(
        &mut self,
        cursor: CXCursor,
        kind: ReferenceKind,
    ) -> Result<(), CollectError> {
        let Some(extent) = self.unit.cursor_span(cursor)? else {
            return Ok(());
        };
        let span = match self
            .unit
            .reference_name_span(cursor)?
            .filter(|name| name.end > name.start && *name != extent)
        {
            Some(name) => name,
            None => self
                .unit
                .name_span(cursor)?
                .filter(|name| name.end > name.start)
                .unwrap_or(extent),
        };
        if kind == ReferenceKind::Value {
            self.ensure_foreign_value_authority(cursor)?;
        }
        let target = self.reference_target(TranslationUnit::referenced(cursor))?;
        self.push_reference(ReferenceFact {
            kind,
            span,
            owner: TranslationUnit::semantic_parent(cursor),
            target,
        })
    }

    /// Retains one foreign variable, enumerator, function, or method authority
    /// row when a value reference resolves across a project header. The row
    /// carries identity and kind only; it is not a main-source declaration
    /// fact and never becomes a pushed entity.
    fn ensure_foreign_value_authority(&mut self, cursor: CXCursor) -> Result<(), CollectError> {
        let referenced = TranslationUnit::referenced(cursor);
        if TranslationUnit::is_null_cursor(referenced) {
            return Ok(());
        }
        if self.unit.is_local(referenced)? {
            return Ok(());
        }
        let kind = declaration_kind(TranslationUnit::cursor_kind(referenced));
        if !matches!(
            kind,
            DeclarationKind::Variable
                | DeclarationKind::Enumerator
                | DeclarationKind::Function
                | DeclarationKind::Method
        ) {
            return Ok(());
        }
        let Some(identity) = TranslationUnit::cursor_identity(referenced) else {
            return Ok(());
        };
        if self.scratch.declarations[..self.declarations]
            .iter()
            .any(|fact| fact.identity == Some(identity))
        {
            return Ok(());
        }
        let id = DeclarationId {
            raw: u32::try_from(self.declarations).map_err(|_| CollectError::SlotOrdinalTooLarge {
                lane: ScratchLane::Declarations,
                observed: self.declarations,
            })?,
        };
        self.push_declaration(DeclarationFact {
            id,
            kind,
            definition: DefinitionState::Declaration,
            virtuality: MethodVirtuality::NonVirtual,
            identity: Some(identity),
            span: SourceSpan { start: 0, end: 0 },
            name: None,
            owner: None,
            documentation: None,
            storage: StorageClass::None,
            type_root: None,
            enum_underlying: None,
        })
    }

    /// Retains one include directive or module import with resolved opaque native authority.
    fn record_source_dependency(
        &mut self,
        cursor: CXCursor,
        kind: SourceDependencyKind,
    ) -> Result<(), CollectError> {
        let Some(span) = self.unit.cursor_span(cursor)? else {
            return Ok(());
        };
        self.push_include(IncludeFact {
            kind,
            span,
            resolved: match kind {
                SourceDependencyKind::Include => TranslationUnit::included_file_identity(cursor),
                SourceDependencyKind::ModuleImport => {
                    TranslationUnit::imported_module_identity(cursor)
                }
            },
        })
    }

    /// Retains one structured native diagnostic without copying its potentially large message text.
    fn record_diagnostic(&mut self, diagnostic: CXDiagnostic) -> Result<(), CollectError> {
        self.push_diagnostic(DiagnosticFact {
            severity: diagnostic_severity(TranslationUnit::diagnostic_severity(diagnostic)),
            location: self
                .unit
                .location_span(TranslationUnit::diagnostic_location(diagnostic))?,
            category: TranslationUnit::diagnostic_category(diagnostic),
            message: TranslationUnit::diagnostic_identity(diagnostic),
        })
    }

    /// Builds a recursive type graph directly from libclang type operations.
    fn collect_type(&mut self, type_: CXType) -> Result<Option<TypeId>, CollectError> {
        let native_kind = TranslationUnit::type_kind(type_);
        if native_kind == clang_sys::CXType_Invalid {
            return Ok(None);
        }
        let declaration = TranslationUnit::type_declaration(type_);
        let kind = type_kind(native_kind, declaration);
        let canonical_kind = TranslationUnit::type_kind(TranslationUnit::canonical_type(type_));
        let id = TypeId {
            raw: u32::try_from(self.types).map_err(|_| CollectError::SlotOrdinalTooLarge {
                lane: ScratchLane::Types,
                observed: self.types,
            })?,
        };
        let (is_const, is_volatile, is_restrict) = TranslationUnit::type_qualifiers(type_);
        self.push_type(TypeFact {
            id,
            kind,
            qualifiers: TypeQualifiers {
                is_const,
                is_volatile,
                is_restrict,
            },
            declaration,
            array_len: matches!(kind, TypeKind::Array)
                .then(|| TranslationUnit::array_len(type_))
                .flatten(),
            // A named type that canonicalizes to a builtin (a `uint16_t`
            // typedef, say) retains that closed underlying class. The
            // declaration identity still records the alias, but the measured
            // canonical form is what distinguishes two overloads whose only
            // difference is such a typedef.
            builtin: builtin_class(native_kind, canonical_kind).or_else(|| {
                matches!(kind, TypeKind::Named)
                    .then(|| builtin_class(canonical_kind, canonical_kind))
                    .flatten()
            }),
            size_bits: measured_bits(TranslationUnit::type_size_of(type_)),
            align_bits: measured_bits(TranslationUnit::type_align_of(type_)),
            is_variadic: kind == TypeKind::Function && TranslationUnit::function_is_variadic(type_),
        })?;
        self.collect_type_children(id, kind, type_)?;
        Ok(Some(id))
    }

    /// Adds every direct recursive child relation that libclang exposes for one type fact.
    fn collect_type_children(
        &mut self,
        parent: TypeId,
        kind: TypeKind,
        type_: CXType,
    ) -> Result<(), CollectError> {
        match kind {
            TypeKind::Pointer | TypeKind::BlockPointer => self.collect_type_child(
                parent,
                TypeRelation::Pointee,
                TranslationUnit::pointee_type(type_),
            )?,
            TypeKind::MemberPointer => {
                // A dependent member pointer (`T::*` under a template
                // parameter) has no concrete class operand to admit: the
                // direct libclang query crashes on exactly those types in
                // the loaded libclang, and the dependence signal is the
                // documented negative layout error. The MemberOwner edge is
                // omitted for the dependent form; the Pointee edge stays
                // because `clang_getPointeeType` reads the stored pointee
                // without touching the class operand.
                if !TranslationUnit::member_pointer_class_is_dependent(type_) {
                    self.collect_type_child(
                        parent,
                        TypeRelation::MemberOwner,
                        TranslationUnit::member_pointer_class_type(type_),
                    )?;
                }
                self.collect_type_child(
                    parent,
                    TypeRelation::Pointee,
                    TranslationUnit::pointee_type(type_),
                )?;
            }
            TypeKind::LvalueReference | TypeKind::RvalueReference => {
                self.collect_type_child(
                    parent,
                    TypeRelation::Referent,
                    TranslationUnit::pointee_type(type_),
                )?;
            }
            TypeKind::Array => self.collect_type_child(
                parent,
                TypeRelation::Element,
                TranslationUnit::array_element_type(type_),
            )?,
            TypeKind::Function => {
                self.collect_type_child(
                    parent,
                    TypeRelation::Result,
                    TranslationUnit::function_result_type(type_),
                )?;
                if let Some(count) = TranslationUnit::function_argument_count(type_) {
                    for index in 0..count {
                        self.collect_type_child(
                            parent,
                            TypeRelation::Parameter,
                            TranslationUnit::function_argument_type(type_, index),
                        )?;
                    }
                }
            }
            TypeKind::Named => {
                if let Some(count) = TranslationUnit::template_argument_count(type_) {
                    for index in 0..count {
                        self.collect_type_child(
                            parent,
                            TypeRelation::TemplateArgument,
                            TranslationUnit::template_argument_type(type_, index),
                        )?;
                    }
                }
            }
            TypeKind::Unknown | TypeKind::Builtin => {}
        }
        Ok(())
    }

    /// Recursively retains one child type and emits its typed directed parent relation.
    fn collect_type_child(
        &mut self,
        parent: TypeId,
        relation: TypeRelation,
        child: CXType,
    ) -> Result<(), CollectError> {
        let Some(target) = self.collect_type(child)? else {
            return Ok(());
        };
        self.push_type_edge(TypeEdge {
            source: parent,
            relation,
            target,
        })
    }

    /// Resolves one referenced cursor into a local, foreign, or exact unresolved fact.
    fn reference_target(&mut self, cursor: CXCursor) -> Result<ReferenceTarget, CollectError> {
        if TranslationUnit::is_null_cursor(cursor) {
            return Ok(ReferenceTarget::Unresolved);
        }
        let Some(identity) = TranslationUnit::cursor_identity(cursor) else {
            return Ok(ReferenceTarget::Unresolved);
        };
        if self.unit.is_local(cursor)? {
            Ok(ReferenceTarget::Local(identity))
        } else {
            let path = self
                .unit
                .cursor_relative_path(cursor)
                .and_then(|relative| {
                    backend_semantic::ir::PackageLineage::new("c", &relative).ok()?;
                    let slot = u32::try_from(self.project_paths.len()).ok()?;
                    self.project_paths.push(relative.into_boxed_str());
                    Some(slot)
                });
            Ok(ReferenceTarget::Foreign {
                identity,
                file: self.unit.cursor_file_identity(cursor),
                path,
            })
        }
    }

    /// Writes one declaration fact into the next exact caller-provided declaration slot.
    fn push_declaration(&mut self, fact: DeclarationFact) -> Result<(), CollectError> {
        push(
            self.scratch.declarations,
            &mut self.declarations,
            ScratchLane::Declarations,
            fact,
        )
    }

    /// Writes one type fact into the next exact caller-provided type slot.
    fn push_type(&mut self, fact: TypeFact) -> Result<(), CollectError> {
        push(
            self.scratch.types,
            &mut self.types,
            ScratchLane::Types,
            fact,
        )
    }

    /// Writes one type edge into the next exact caller-provided edge slot.
    fn push_type_edge(&mut self, fact: TypeEdge) -> Result<(), CollectError> {
        push(
            self.scratch.type_edges,
            &mut self.type_edges,
            ScratchLane::TypeEdges,
            fact,
        )
    }

    /// Writes one reference fact into the next exact caller-provided reference slot.
    fn push_reference(&mut self, fact: ReferenceFact) -> Result<(), CollectError> {
        push(
            self.scratch.references,
            &mut self.references,
            ScratchLane::References,
            fact,
        )
    }

    /// Writes one diagnostic fact into the next exact caller-provided diagnostic slot.
    fn push_diagnostic(&mut self, fact: DiagnosticFact) -> Result<(), CollectError> {
        push(
            self.scratch.diagnostics,
            &mut self.diagnostics,
            ScratchLane::Diagnostics,
            fact,
        )
    }

    /// Writes one include fact into the next exact caller-provided include slot.
    fn push_include(&mut self, fact: IncludeFact) -> Result<(), CollectError> {
        push(
            self.scratch.includes,
            &mut self.includes,
            ScratchLane::Includes,
            fact,
        )
    }

    fn push_override(&mut self, fact: OverrideFact) -> Result<(), CollectError> {
        push(
            self.scratch.overrides,
            &mut self.overrides,
            ScratchLane::Overrides,
            fact,
        )
    }

    /// Emits only the parameter run owned by each declaration's definition.
    ///
    /// Each owner's stashed parameters are split into maximal contiguous runs
    /// keyed by owner and visiting-declaration definition flag, so a
    /// prototype and its definition never merge into one run however they
    /// order. The definition run wins when present; otherwise the first run
    /// survives. Owner-less runs (function-pointer typedef parameters) never
    /// compete: without an identity there is nothing to supersede against,
    /// so every such run is emitted.
    fn emit_parameters(&mut self) -> Result<(), CollectError> {
        let parameters = core::mem::take(&mut self.parameters);
        let mut runs: Vec<ParameterRun> = Vec::new();
        for (index, parameter) in parameters.iter().enumerate() {
            match runs.last_mut() {
                Some(run)
                    if run.owner == parameter.owner
                        && run.parent_span == parameter.parent_span
                        && run.is_definition == parameter.owner_is_definition =>
                {
                    run.end = index + 1
                }
                _ => runs.push(ParameterRun {
                    owner: parameter.owner,
                    parent_span: parameter.parent_span,
                    start: index,
                    end: index + 1,
                    is_definition: parameter.owner_is_definition,
                }),
            }
        }
        let mut chosen: HashMap<Option<SymbolIdentity>, usize> = HashMap::new();
        for (index, run) in runs.iter().enumerate() {
            if run.owner.is_none() {
                continue;
            }
            match chosen.get_mut(&run.owner) {
                Some(selected) => {
                    if run.is_definition && !runs[*selected].is_definition {
                        *selected = index;
                    }
                }
                None => {
                    chosen.insert(run.owner, index);
                }
            }
        }
        for (index, run) in runs.iter().enumerate() {
            // Owner-less runs never compete; every one is emitted.
            if run.owner.is_some() && chosen.get(&run.owner) != Some(&index) {
                continue;
            }
            for parameter in &parameters[run.start..run.end] {
                self.commit_declaration(parameter.cursor, DeclarationKind::Parameter)?;
            }
        }
        Ok(())
    }

    /// Returns only the initialized prefixes after emitting deferred parameters and preserving
    /// any traversal failure exactly.
    fn finish(mut self) -> Result<ClangFacts<'scratch>, CollectError> {
        if let Some(failure) = self.failure {
            return Err(failure);
        }
        self.emit_parameters()?;
        Ok(ClangFacts {
            declarations: prefix(
                self.scratch.declarations,
                self.declarations,
                ScratchLane::Declarations,
            )?,
            types: prefix(self.scratch.types, self.types, ScratchLane::Types)?,
            type_edges: prefix(
                self.scratch.type_edges,
                self.type_edges,
                ScratchLane::TypeEdges,
            )?,
            references: prefix(
                self.scratch.references,
                self.references,
                ScratchLane::References,
            )?,
            diagnostics: prefix(
                self.scratch.diagnostics,
                self.diagnostics,
                ScratchLane::Diagnostics,
            )?,
            includes: prefix(self.scratch.includes, self.includes, ScratchLane::Includes)?,
            overrides: prefix(
                self.scratch.overrides,
                self.overrides,
                ScratchLane::Overrides,
            )?,
            project_paths: self.project_paths,
        })
    }
}

/// C callback that never permits a Rust failure to cross the native ABI boundary.
extern "C" fn visit(cursor: CXCursor, _parent: CXCursor, data: *mut c_void) -> CXChildVisitResult {
    let stop = ffi::callback_state(data, |collector: &mut Collector<'_, '_>| {
        collector.observe(cursor)
    })
    .unwrap_or(true);
    if stop {
        clang_sys::CXChildVisit_Break
    } else {
        clang_sys::CXChildVisit_Recurse
    }
}

/// Appends one fact to a caller-provided typed region while preserving exact capacity facts.
fn push<Fact>(
    slots: &mut [Fact],
    used: &mut usize,
    lane: ScratchLane,
    fact: Fact,
) -> Result<(), CollectError> {
    let Some(slot) = slots.get_mut(*used) else {
        let required = used.checked_add(1).ok_or(CollectError::SlotCountOverflow {
            lane,
            capacity: slots.len(),
        })?;
        return Err(CollectError::ScratchCapacity {
            lane,
            capacity: slots.len(),
            required,
        });
    };
    *slot = fact;
    *used = used.checked_add(1).ok_or(CollectError::SlotCountOverflow {
        lane,
        capacity: slots.len(),
    })?;
    Ok(())
}

/// True when one declaration row was minted by
/// [`Collector::ensure_foreign_value_authority`] for a cross-header value
/// reference. The row carries identity and kind only and must never be
/// upgraded into a named main-source declaration.
const fn foreign_value_authority_stub(fact: &DeclarationFact) -> bool {
    fact.name.is_none()
        && fact.span.start == 0
        && fact.span.end == 0
        && matches!(
            fact.kind,
            DeclarationKind::Function | DeclarationKind::Method
        )
}

/// Borrows an initialized prefix while retaining an impossible internal discrepancy as typed data.
fn prefix<Fact>(slots: &[Fact], used: usize, lane: ScratchLane) -> Result<&[Fact], CollectError> {
    slots.get(..used).ok_or(CollectError::ScratchCapacity {
        lane,
        capacity: slots.len(),
        required: used,
    })
}

/// Classifies libclang's direct declaration cursor kinds without name or token heuristics.
const fn declaration_kind(kind: NativeCursorKind) -> DeclarationKind {
    match kind {
        clang_sys::CXCursor_Namespace => DeclarationKind::Namespace,
        clang_sys::CXCursor_MacroDefinition => DeclarationKind::Macro,
        clang_sys::CXCursor_StructDecl
        | clang_sys::CXCursor_UnionDecl
        | clang_sys::CXCursor_ClassDecl => DeclarationKind::Record,
        clang_sys::CXCursor_EnumDecl => DeclarationKind::Enumeration,
        clang_sys::CXCursor_EnumConstantDecl => DeclarationKind::Enumerator,
        clang_sys::CXCursor_FunctionDecl => DeclarationKind::Function,
        clang_sys::CXCursor_CXXMethod | clang_sys::CXCursor_ConversionFunction => {
            DeclarationKind::Method
        }
        clang_sys::CXCursor_Constructor => DeclarationKind::Constructor,
        clang_sys::CXCursor_Destructor => DeclarationKind::Destructor,
        clang_sys::CXCursor_FieldDecl => DeclarationKind::Field,
        clang_sys::CXCursor_VarDecl => DeclarationKind::Variable,
        clang_sys::CXCursor_ParmDecl => DeclarationKind::Parameter,
        clang_sys::CXCursor_TemplateTypeParameter
        | clang_sys::CXCursor_NonTypeTemplateParameter
        | clang_sys::CXCursor_TemplateTemplateParameter => DeclarationKind::TemplateParameter,
        clang_sys::CXCursor_TypedefDecl | clang_sys::CXCursor_TypeAliasDecl => {
            DeclarationKind::TypeAlias
        }
        clang_sys::CXCursor_FunctionTemplate
        | clang_sys::CXCursor_ClassTemplate
        | clang_sys::CXCursor_ClassTemplatePartialSpecialization => DeclarationKind::Template,
        _ => DeclarationKind::Unknown,
    }
}

/// Classifies native reference cursors without token spelling or source-text inference.
const fn reference_kind(kind: NativeCursorKind) -> Option<ReferenceKind> {
    match kind {
        clang_sys::CXCursor_DeclRefExpr | clang_sys::CXCursor_VariableRef => {
            Some(ReferenceKind::Value)
        }
        clang_sys::CXCursor_TypeRef => Some(ReferenceKind::Type),
        clang_sys::CXCursor_TemplateRef => Some(ReferenceKind::Template),
        clang_sys::CXCursor_MemberRef | clang_sys::CXCursor_MemberRefExpr => {
            Some(ReferenceKind::Member)
        }
        clang_sys::CXCursor_CallExpr => Some(ReferenceKind::Call),
        clang_sys::CXCursor_MacroExpansion => Some(ReferenceKind::MacroExpansion),
        _ => None,
    }
}

/// Classifies direct include and module-import cursor authority without source-text inspection.
const fn source_dependency_kind(kind: NativeCursorKind) -> Option<SourceDependencyKind> {
    match kind {
        clang_sys::CXCursor_InclusionDirective => Some(SourceDependencyKind::Include),
        clang_sys::CXCursor_ModuleImportDecl => Some(SourceDependencyKind::ModuleImport),
        _ => None,
    }
}

/// Classifies direct native type kinds while retaining unsupported kinds as `Unknown` facts.
const fn type_kind(kind: CXTypeKind, declaration: Option<SymbolIdentity>) -> TypeKind {
    match kind {
        clang_sys::CXType_Pointer => TypeKind::Pointer,
        clang_sys::CXType_BlockPointer => TypeKind::BlockPointer,
        clang_sys::CXType_MemberPointer => TypeKind::MemberPointer,
        clang_sys::CXType_LValueReference => TypeKind::LvalueReference,
        clang_sys::CXType_RValueReference => TypeKind::RvalueReference,
        clang_sys::CXType_ConstantArray
        | clang_sys::CXType_IncompleteArray
        | clang_sys::CXType_VariableArray
        | clang_sys::CXType_DependentSizedArray => TypeKind::Array,
        clang_sys::CXType_FunctionNoProto | clang_sys::CXType_FunctionProto => TypeKind::Function,
        clang_sys::CXType_Record
        | clang_sys::CXType_Enum
        | clang_sys::CXType_Typedef
        | clang_sys::CXType_Elaborated => TypeKind::Named,
        clang_sys::CXType_Unexposed if declaration.is_some() => TypeKind::Named,
        clang_sys::CXType_Void
        | clang_sys::CXType_Bool
        | clang_sys::CXType_Char_U
        | clang_sys::CXType_UChar
        | clang_sys::CXType_Char16
        | clang_sys::CXType_Char32
        | clang_sys::CXType_UShort
        | clang_sys::CXType_UInt
        | clang_sys::CXType_ULong
        | clang_sys::CXType_ULongLong
        | clang_sys::CXType_UInt128
        | clang_sys::CXType_Char_S
        | clang_sys::CXType_SChar
        | clang_sys::CXType_WChar
        | clang_sys::CXType_Short
        | clang_sys::CXType_Int
        | clang_sys::CXType_Long
        | clang_sys::CXType_LongLong
        | clang_sys::CXType_Int128
        | clang_sys::CXType_Float
        | clang_sys::CXType_Double
        | clang_sys::CXType_LongDouble
        | clang_sys::CXType_NullPtr
        | clang_sys::CXType_Complex
        | clang_sys::CXType_Vector => TypeKind::Builtin,
        _ => TypeKind::Unknown,
    }
}

/// Projects libclang's declaration-definition predicate into its closed fact type.
const fn definition_state(is_definition: bool) -> DefinitionState {
    if is_definition {
        DefinitionState::Definition
    } else {
        DefinitionState::Declaration
    }
}

/// Projects libclang's closed storage class into the canonical lattice.
/// Private-extern and OpenCL work-group-local declarations keep static
/// storage because both bind internal-linkage storage; `ThreadLocal` has no
/// native query on the enabled symbol surface and is therefore unreachable.
const fn storage_class(class: clang_sys::CX_StorageClass) -> StorageClass {
    match class {
        clang_sys::CX_SC_Auto => StorageClass::Auto,
        clang_sys::CX_SC_Static
        | clang_sys::CX_SC_PrivateExtern
        | clang_sys::CX_SC_OpenCLWorkGroupLocal => StorageClass::Static,
        clang_sys::CX_SC_Extern => StorageClass::Extern,
        clang_sys::CX_SC_Register => StorageClass::Register,
        // `Invalid` and every newer native class keep no storage fact.
        _ => StorageClass::None,
    }
}

/// Wraps one native integer scalar with its exact signedness and C rank.
const fn integer_class(signed: bool, rank: IntegerRank) -> Option<BuiltinClass> {
    Some(BuiltinClass::Integer { signed, rank })
}

/// Classifies libclang's closed builtin type kinds without name or spelling
/// inspection, retaining exotic builtins as `Other`.
const fn builtin_class(kind: CXTypeKind, canonical_kind: CXTypeKind) -> Option<BuiltinClass> {
    match kind {
        clang_sys::CXType_Void => Some(BuiltinClass::Void),
        clang_sys::CXType_Bool => Some(BuiltinClass::Bool),
        clang_sys::CXType_Char_U => Some(BuiltinClass::PlainCharUnsigned),
        clang_sys::CXType_Char_S => Some(BuiltinClass::PlainCharSigned),
        clang_sys::CXType_SChar => Some(BuiltinClass::SignedChar),
        clang_sys::CXType_UChar => Some(BuiltinClass::UnsignedChar),
        clang_sys::CXType_Char16 => Some(BuiltinClass::Utf16CodeUnit),
        clang_sys::CXType_Char32 => Some(BuiltinClass::Utf32CodeUnit),
        clang_sys::CXType_WChar => match canonical_kind {
            clang_sys::CXType_SChar
            | clang_sys::CXType_Short
            | clang_sys::CXType_Int
            | clang_sys::CXType_Long
            | clang_sys::CXType_LongLong
            | clang_sys::CXType_Int128 => Some(BuiltinClass::WideCharSigned),
            clang_sys::CXType_UChar
            | clang_sys::CXType_UShort
            | clang_sys::CXType_UInt
            | clang_sys::CXType_ULong
            | clang_sys::CXType_ULongLong
            | clang_sys::CXType_UInt128 => Some(BuiltinClass::WideCharUnsigned),
            _ => Some(BuiltinClass::WideCharSignednessUnavailable),
        },
        clang_sys::CXType_Short => integer_class(true, IntegerRank::Short),
        clang_sys::CXType_UShort => integer_class(false, IntegerRank::Short),
        clang_sys::CXType_Int => integer_class(true, IntegerRank::Int),
        clang_sys::CXType_UInt => integer_class(false, IntegerRank::Int),
        clang_sys::CXType_Long => integer_class(true, IntegerRank::Long),
        clang_sys::CXType_ULong => integer_class(false, IntegerRank::Long),
        clang_sys::CXType_LongLong => integer_class(true, IntegerRank::LongLong),
        clang_sys::CXType_ULongLong => integer_class(false, IntegerRank::LongLong),
        clang_sys::CXType_Int128 => integer_class(true, IntegerRank::Int128),
        clang_sys::CXType_UInt128 => integer_class(false, IntegerRank::Int128),
        clang_sys::CXType_Float | clang_sys::CXType_Double | clang_sys::CXType_LongDouble => {
            Some(BuiltinClass::Float)
        }
        clang_sys::CXType_NullPtr | clang_sys::CXType_Complex | clang_sys::CXType_Vector => {
            Some(BuiltinClass::Other)
        }
        _ => None,
    }
}

/// Narrows one native byte measurement into exact bit facts. Negative
/// results are libclang's incomplete, dependent, and invalid layout errors,
/// which keep the cell honestly empty.
fn measured_bits(bytes: i64) -> Option<u32> {
    if bytes <= 0 {
        return None;
    }
    let bits = bytes.checked_mul(8)?;
    u32::try_from(bits).ok()
}

/// Projects libclang's closed diagnostic severity without converting it through strings.
const fn diagnostic_severity(severity: NativeDiagnosticSeverity) -> DiagnosticSeverity {
    match severity {
        clang_sys::CXDiagnostic_Ignored => DiagnosticSeverity::Ignored,
        clang_sys::CXDiagnostic_Note => DiagnosticSeverity::Note,
        clang_sys::CXDiagnostic_Warning => DiagnosticSeverity::Warning,
        clang_sys::CXDiagnostic_Error => DiagnosticSeverity::Error,
        clang_sys::CXDiagnostic_Fatal => DiagnosticSeverity::Fatal,
        _ => DiagnosticSeverity::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CollectError, DeclarationKind, ScratchLane, TypeKind, declaration_kind, push, type_kind,
    };
    use crate::legacy::facts::SymbolIdentity;

    /// Holds the exact result category required by one deterministic collector unit test.
    #[derive(Debug, thiserror::Error)]
    enum TestError {
        /// A collector result differed from the required closed error fact.
        #[error("unexpected collector result: {0:?}")]
        Unexpected(CollectError),
        /// A fact classifier differed from the direct native kind law.
        #[error("native classifier did not preserve the required fact")]
        Classification,
    }

    #[test]
    fn capacity_error_preserves_exact_fact_lane_and_required_slot() -> Result<(), TestError> {
        let mut slots = [0_u8];
        let mut used = 0;
        push(&mut slots, &mut used, ScratchLane::Types, 1).map_err(TestError::Unexpected)?;
        match push(&mut slots, &mut used, ScratchLane::Types, 2) {
            Err(
                error @ CollectError::ScratchCapacity {
                    lane: ScratchLane::Types,
                    capacity: 1,
                    required: 2,
                },
            ) => {
                let _ = error;
                Ok(())
            }
            Err(error) => Err(TestError::Unexpected(error)),
            Ok(()) => Err(TestError::Classification),
        }
    }

    #[test]
    fn native_kind_classifiers_preserve_templates_and_recursive_types() -> Result<(), TestError> {
        let template = declaration_kind(clang_sys::CXCursor_FunctionTemplate);
        let pointer = type_kind(clang_sys::CXType_Pointer, None);
        let block_pointer = type_kind(clang_sys::CXType_BlockPointer, None);
        let member_pointer = type_kind(clang_sys::CXType_MemberPointer, None);
        let named = type_kind(
            clang_sys::CXType_Unexposed,
            Some(SymbolIdentity {
                bytes: [1; crate::legacy::SYMBOL_IDENTITY_BYTES],
            }),
        );
        if template == DeclarationKind::Template
            && pointer == TypeKind::Pointer
            && block_pointer == TypeKind::BlockPointer
            && member_pointer == TypeKind::MemberPointer
            && named == TypeKind::Named
        {
            Ok(())
        } else {
            Err(TestError::Classification)
        }
    }
}
