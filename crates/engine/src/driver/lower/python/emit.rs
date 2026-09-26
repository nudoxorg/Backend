//! Emits canonical Python facts from one extracted module.

use super::*;

impl<'a, 'source> Emitter<'a, 'source> {
    /// Builds one emitter over a module and an optional checker report.
    pub(super) fn new(
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
    pub(super) fn emit_classes(&mut self) -> Result<(), PythonCollectError> {
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
    pub(super) fn emit_non_class_declarations(&mut self) -> Result<(), PythonCollectError> {
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
    pub(super) fn emit_parentage(&mut self) -> Result<(), PythonCollectError> {
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
        Ok(TypeTables {
            classes,
            typevars,
            bindings,
        })
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
            apply_type_alias_binding(&mut bound, &mut locals, &declaration.name, resolution);
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
                let Some(children) = self.admit_flat_children(tuple_record(), children, anchor)?
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
        self.module
            .declarations
            .iter()
            .enumerate()
            .any(|(index, declaration)| {
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
    pub(super) fn emit_occurrences(&mut self) -> Result<(), PythonCollectError> {
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
                    let target = foreign_package(module_spelling, binding, imported)?;
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
                    // The attribute resolves to no live method of the class
                    // (an inherited or unknown method): an honest typed
                    // foreign method key, never a fabricated local.
                    None => Ok(Some((
                        foreign_method(self.slice(occurrence.span)?, occurrence.span)?,
                        OccurrenceConfidence::Index,
                    ))),
                }
            }
            OccurrenceReceiver::Foreign { receiver } => {
                // A receiver that names an import binding resolves through
                // that binding's own package key; any other receiver stays
                // an honest typed foreign method key.
                if let Some(receiver) = receiver {
                    if let Some(row) = rows
                        .iter()
                        .find(|row| row.name == receiver.as_bytes() && row.imported.is_some())
                    {
                        let imported = row.imported.expect("imported span proven non-None above");
                        let module_spelling = self.slice(imported)?;
                        let binding = self.slice(occurrence.span)?;
                        let binding = core::str::from_utf8(binding).map_err(|_| {
                            PythonCollectError::Projection(
                                PythonProjectionFault::ForeignSpellingUtf8 {
                                    start: occurrence.span.start,
                                    end: occurrence.span.end,
                                },
                            )
                        })?;
                        let target = foreign_package(module_spelling, binding, imported)?;
                        let confidence = match checked {
                            Some(SymbolOutcome::Foreign { .. }) => OccurrenceConfidence::Import,
                            _ => OccurrenceConfidence::Index,
                        };
                        return Ok(Some((target, confidence)));
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
                let Some(children) = self.admit_flat_children(tuple_record(), children, anchor)?
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
                let Some(children) = self.admit_flat_children(union_record(), children, anchor)?
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
    pub(super) fn emit_docs(&mut self) -> Result<(), PythonCollectError> {
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
