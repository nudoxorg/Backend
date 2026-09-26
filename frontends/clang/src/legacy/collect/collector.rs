//! Walks one libclang translation unit into caller-bounded fact slots.

use super::*;

impl<'unit, 'scratch> Collector<'unit, 'scratch> {
    /// Binds one live native unit to its caller-owned fact destination arrays.
    pub(super) const fn new(
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
            cancellation,
            failure: None,
        }
    }

    /// Streams native diagnostics before cursor traversal so syntax errors retain authority facts.
    pub(super) fn collect_diagnostics(&mut self) -> Result<(), CollectError> {
        if cancelled(self.cancellation) {
            return Err(CollectError::Cancelled);
        }
        let unit = self.unit;
        unit.diagnostics(|diagnostic| self.record_diagnostic(diagnostic))
    }

    /// Records one traversal cursor and signals whether libclang should stop visiting children.
    pub(super) fn observe(&mut self, cursor: CXCursor) -> bool {
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
        let target = self.reference_target(TranslationUnit::referenced(cursor))?;
        self.push_reference(ReferenceFact {
            kind,
            span,
            owner: TranslationUnit::semantic_parent(cursor),
            target,
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
    fn reference_target(&self, cursor: CXCursor) -> Result<ReferenceTarget, CollectError> {
        if TranslationUnit::is_null_cursor(cursor) {
            return Ok(ReferenceTarget::Unresolved);
        }
        let Some(identity) = TranslationUnit::cursor_identity(cursor) else {
            return Ok(ReferenceTarget::Unresolved);
        };
        if self.unit.is_local(cursor)? {
            Ok(ReferenceTarget::Local(identity))
        } else {
            Ok(ReferenceTarget::Foreign {
                identity,
                file: self.unit.cursor_file_identity(cursor),
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
    pub(super) fn finish(mut self) -> Result<ClangFacts<'scratch>, CollectError> {
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
        })
    }
}
