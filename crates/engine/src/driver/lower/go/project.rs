//! Runs the Go image projector passes against one validated authority image.
use super::*;

impl<'x, 'source> Projector<'x, 'source> {
    /// Opens one projector over a validated Go image and the shared fact lane.
    pub(super) fn new(image: GoImage<'source>, facts: &'x mut FactSet<'source>) -> Self {
        let declarations = image.declaration_count();
        Self {
            declaration_ordinals: vec![None; image.declaration_count()],
            method_ordinals: vec![None; image.method_count()],
            member_ordinals: vec![None; image.member_count()],
            anonymous: vec![None; image.type_count()],
            image,
            facts,
            names: Vec::new(),
            declaration_spans: vec![None; declarations],
            name_spans: vec![None; declarations],
            members: Vec::new(),
        }
    }

    /// Records one pushed declaration name for later resolution.
    fn record_name(&mut self, package: &'source [u8], name: &'source [u8], ordinal: u32) {
        self.names.push((package, name, ordinal));
    }

    /// Resolves one declared name to its pushed fact ordinal.
    fn lookup(&self, package: &[u8], name: &[u8]) -> Option<u32> {
        self.names
            .iter()
            .find(|(known_package, known, _)| *known_package == package && *known == name)
            .map(|(_, _, ordinal)| *ordinal)
    }

    /// Reports whether one unqualified identifier already names a declaration
    /// row in the authority image.
    fn image_declared(&self, name: &[u8]) -> Result<bool, GoCollectError> {
        for index in 0..self.image.declaration_count() {
            let declaration = self
                .image
                .declaration(index)
                .map_err(GoCollectError::Image)?;
            if declaration.name == name {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Emits one package-level alias per unresolved-cgo name whose
    /// unqualified identifier is not already a declaration in this image.
    pub(super) fn unresolved_cgo(&mut self) -> Result<(), GoCollectError> {
        for index in 0..self.image.unresolved_cgo_count() {
            let spelling = self
                .image
                .unresolved_cgo(index)
                .map_err(GoCollectError::Image)?;
            let unqualified = match spelling.iter().rposition(|byte| *byte == b'.') {
                Some(index) => &spelling[index + 1..],
                None => spelling,
            };
            if self.image_declared(unqualified)? {
                continue;
            }
            let fact = RootType::leaf(unknown_record(
                TypeReason::UnresolvedExternal,
                Some(spelling),
            ))
            .attach(SemanticFact::new(
                EntityKind::Alias,
                spelling,
                constructor(EntityKind::Alias),
            ));
            let ordinal = push(self.facts, fact)?;
            self.facts
                .mark_parentage_root(ordinal)
                .map_err(|fault| lane_terminal_ordinal(ordinal, spelling.len(), fault))?;
        }
        Ok(())
    }

    /// Resolves one receiver-qualified member spelling to its pushed fact
    /// ordinal and class.
    fn lookup_member(
        &self,
        package: &[u8],
        type_name: &[u8],
        member: &[u8],
    ) -> Option<(u32, bool)> {
        self.members
            .iter()
            .find(|key| {
                key.package == package && key.type_name == type_name && key.member == member
            })
            .map(|key| (key.ordinal, key.is_field))
    }

    /// Records one image declaration's primary-source facts: its fact's
    /// source span (the full authority-bound declaration extent, the exact
    /// basis every owned occurrence's relative span is measured from) and
    /// its NAME-TOKEN extent for satisfaction anchoring. Unbound rows —
    /// declarations that declare in a sibling source file — stay
    /// source-less rather than borrowing another file's coordinates.
    fn record_declaration_spans(
        &mut self,
        index: usize,
        declaration: &Declaration<'source>,
        ordinal: u32,
    ) -> Result<(), GoCollectError> {
        if !declaration.bound {
            return Ok(());
        }
        if let Some((start, end)) = declaration.span {
            self.declaration_spans[index] = Some((start, end));
            let staged = StagedSourceSpan::new(start, end).ok_or_else(|| {
                terminal(ProjectionFault::Span {
                    row: u32::try_from(index).unwrap_or(u32::MAX),
                    start,
                    end,
                })
            })?;
            self.facts
                .attach_source_span(ordinal, staged)
                .map_err(|fault| lane_terminal_ordinal(ordinal, declaration.name.len(), fault))?;
        }
        if let Some((start, end)) = declaration.name_span {
            self.name_spans[index] = Some((start, end));
        }
        Ok(())
    }

    /// Records one image method's primary-source span: the declaring
    /// `func` extent, the exact basis every method-owned occurrence's
    /// relative span is measured from. Unbound methods stay source-less.
    fn record_method_span(
        &mut self,
        method: backend_frontend_go::legacy::MethodRow<'source>,
        ordinal: u32,
    ) -> Result<(), GoCollectError> {
        if !method.bound {
            return Ok(());
        }
        let Some((start, end)) = method.span else {
            return Ok(());
        };
        let staged = StagedSourceSpan::new(start, end).ok_or_else(|| {
            terminal(ProjectionFault::Span {
                row: method.owner,
                start,
                end,
            })
        })?;
        self.facts
            .attach_source_span(ordinal, staged)
            .map_err(|fault| lane_terminal_ordinal(ordinal, method.name.len(), fault))
    }

    /// The pending fact that owns anonymous rows being built immediately
    /// before its push. `FactSet` records this reserved coordinate and proves
    /// it becomes valid when the caller admits that exact next fact.
    fn anchor(&self) -> Result<u32, ProjectionFault> {
        let length = self.facts.len();
        u32::try_from(length).map_err(|_| ProjectionFault::IndexCapacity {
            phase: GoProjectionIndexPhase::FactOrdinal,
            observed: length as u64,
        })
    }

    /// Pass one: one named type with its recursive diagonal self-nominal.
    /// The kind follows the underlying shape: an interface root row is the
    /// canonical trait, every other defined type is a record.
    pub(super) fn named_type(
        &mut self,
        index: usize,
        declaration: &Declaration<'source>,
    ) -> Result<(), GoCollectError> {
        let own = u32::try_from(self.facts.len()).map_err(|_| {
            terminal(ProjectionFault::IndexCapacity {
                phase: GoProjectionIndexPhase::FactOrdinal,
                observed: self.facts.len() as u64,
            })
        })?;
        let kind = self.named_type_kind(declaration)?;
        let fact =
            SemanticFact::new(kind, declaration.name, constructor(kind)).typed(nominal_record(own));
        let ordinal = push(self.facts, fact)?;
        self.facts
            .mark_parentage_root(ordinal)
            .map_err(|fault| lane_terminal_ordinal(ordinal, declaration.name.len(), fault))?;
        self.declaration_ordinals[index] = Some(ordinal);
        self.record_name(declaration.package, declaration.name, ordinal);
        self.record_declaration_spans(index, declaration, ordinal)?;
        Ok(())
    }

    /// Pass one: one alias whose declared type is its projected target.
    /// Targets referencing later declarations fold to typed unknowns
    /// because the lane admits strictly backward references only.
    pub(super) fn alias(
        &mut self,
        index: usize,
        declaration: &Declaration<'source>,
    ) -> Result<(), GoCollectError> {
        let root = self.root(declaration.type_root, TypeReason::OracleGap)?;
        let fact = root.attach(SemanticFact::new(
            EntityKind::Alias,
            declaration.name,
            constructor(EntityKind::Alias),
        ));
        let ordinal = push(self.facts, fact)?;
        self.facts
            .mark_parentage_root(ordinal)
            .map_err(|fault| lane_terminal_ordinal(ordinal, declaration.name.len(), fault))?;
        self.declaration_ordinals[index] = Some(ordinal);
        self.record_name(declaration.package, declaration.name, ordinal);
        self.record_declaration_spans(index, declaration, ordinal)?;
        Ok(())
    }

    /// Pass two: one named type's member planes — its type parameters, its
    /// struct fields or interface method signatures, its methods, and the
    /// Go extension row binding them to the type fact.
    pub(super) fn type_family(
        &mut self,
        index: usize,
        declaration: &Declaration<'source>,
    ) -> Result<(), GoCollectError> {
        let Some(type_ordinal) = self.declaration_ordinals[index] else {
            let owner = u32::try_from(index).map_err(|_| {
                terminal(ProjectionFault::IndexCapacity {
                    phase: GoProjectionIndexPhase::Declaration,
                    observed: index as u64,
                })
            })?;
            return Err(terminal(ProjectionFault::OrphanOwner { owner }));
        };
        let parameter_start = u32::try_from(self.facts.type_parameter_len).map_err(|_| {
            terminal(ProjectionFault::IndexCapacity {
                phase: GoProjectionIndexPhase::TypeParameter,
                observed: self.facts.type_parameter_len as u64,
            })
        })?;
        self.type_parameters(index)?;
        let type_parameters = self
            .facts
            .type_parameter_range(parameter_start)
            .map_err(|fault| lane_terminal(self.facts.len(), declaration.name.len(), fault))?;
        let mut fields = Vec::new();
        let mut interface_methods = Vec::new();
        if let Some(root_cell) = declaration.type_root {
            let row = self
                .image
                .type_row(index_of(root_cell))
                .map_err(GoCollectError::Image)?;
            match row.kind {
                TypeRowKind::Struct => self.fields(
                    &row,
                    &mut fields,
                    type_ordinal,
                    declaration.package,
                    declaration.name,
                )?,
                TypeRowKind::Interface => self.interface_methods(
                    &row,
                    &mut interface_methods,
                    type_ordinal,
                    declaration.package,
                    declaration.name,
                )?,
                _ => {}
            }
        }
        let mut methods = Vec::new();
        let mut method_names = Vec::new();
        for method_index in 0..self.image.method_count() {
            let method = self
                .image
                .method(method_index)
                .map_err(GoCollectError::Image)?;
            if usize::try_from(method.owner).is_ok_and(|owner| owner != index) {
                continue;
            }
            let receiver_start = u32::try_from(self.facts.type_parameter_len).map_err(|_| {
                terminal(ProjectionFault::IndexCapacity {
                    phase: GoProjectionIndexPhase::TypeParameter,
                    observed: self.facts.type_parameter_len as u64,
                })
            })?;
            for name in blank_separated(method.receiver_type_params) {
                self.facts
                    .push_type_parameter(name, None, None)
                    .map_err(|fault| lane_terminal(self.facts.len(), method.name.len(), fault))?;
            }
            let ordinal = self.executable(method.name, method.type_root, receiver_start)?;
            self.facts
                .attach_parent(ordinal, type_ordinal)
                .map_err(|fault| lane_terminal_ordinal(ordinal, method.name.len(), fault))?;
            self.record_method_span(method, ordinal)?;
            self.members.push(MemberKey {
                package: declaration.package,
                type_name: declaration.name,
                member: method.name,
                ordinal,
                is_field: false,
            });
            methods.push(ordinal);
            method_names.push(method.name);
            self.method_ordinals[method_index] = Some(ordinal);
        }
        for (ordinal, name) in interface_methods {
            methods.push(ordinal);
            method_names.push(name);
        }
        for method_set_index in 0..self.image.method_set_count() {
            let method_set = self
                .image
                .method_set(method_set_index)
                .map_err(GoCollectError::Image)?;
            // Owner is the interface type-row index, not the declaration
            // index. The declaring package may be this package, a foreign
            // import path, or empty (universe); the name still belongs to
            // this interface's method set.
            if declaration.type_root != Some(method_set.owner)
                || method_names.contains(&method_set.name)
            {
                continue;
            }
            let ordinal =
                self.executable(method_set.name, method_set.type_root, parameter_start)?;
            self.facts
                .attach_parent(ordinal, type_ordinal)
                .map_err(|fault| lane_terminal_ordinal(ordinal, method_set.name.len(), fault))?;
            self.members.push(MemberKey {
                package: declaration.package,
                type_name: declaration.name,
                member: method_set.name,
                ordinal,
                is_field: false,
            });
            methods.push(ordinal);
            method_names.push(method_set.name);
        }
        let fields_list = self.entity_list(&fields, type_ordinal, GoProjectionListPhase::Entity)?;
        let method_set = self.entity_list(&methods, type_ordinal, GoProjectionListPhase::Entity)?;
        self.facts
            .attach_extension_with_type_parameters(
                usize::try_from(type_ordinal).map_err(|_| {
                    terminal(ProjectionFault::IndexCapacity {
                        phase: GoProjectionIndexPhase::FactOrdinal,
                        observed: type_ordinal as u64,
                    })
                })?,
                EmissionExtension::Go(GoFacts {
                    signature: GoSignature {
                        parameters: TypeListId::new(0),
                        results: TypeListId::new(0),
                        variadic: false,
                    },
                    type_parameters: TypeParameterListId::new(parameter_start),
                    fields: fields_list,
                    method_set,
                    build_constraints: AtomListId::new(0),
                    constant_value: AtomListId::new(0),
                    constant_group: 0,
                    constant_flags: 0,
                }),
                type_parameters,
            )
            .map_err(|fault| lane_terminal_ordinal(type_ordinal, declaration.name.len(), fault))?;
        Ok(())
    }

    /// Pushes the pooled type-parameter rows of one declaration. A
    /// constraint resolves to its fact ordinal only when it is a pure local
    /// named row; inline interface constraints have no fact to name and
    /// stay empty on the pooled row.
    fn type_parameters(&mut self, index: usize) -> Result<(), GoCollectError> {
        let own_package = self
            .image
            .declaration(index)
            .map_err(GoCollectError::Image)?
            .package;
        for parameter_index in 0..self.image.type_parameter_count() {
            let row = self
                .image
                .type_parameter(parameter_index)
                .map_err(GoCollectError::Image)?;
            if usize::try_from(row.owner).is_ok_and(|owner| owner != index) {
                continue;
            }
            let constraint = row
                .constraint
                .and_then(|root| self.image.type_row(index_of(root)).ok())
                .filter(|row| {
                    matches!(row.kind, TypeRowKind::Named | TypeRowKind::Alias)
                        && row.children.1 == 0
                        && row.package == own_package
                })
                .and_then(|row| self.lookup(row.package, row.name));
            self.facts
                .push_type_parameter(row.name, constraint, None)
                .map_err(|fault| lane_terminal(self.facts.len(), row.name.len(), fault))?;
        }
        Ok(())
    }

    /// Pushes one fact per struct field, projecting each field type as the
    /// field fact's root record, and registers each field under its
    /// receiver type's spelling for member-target resolution.
    fn fields(
        &mut self,
        row: &backend_frontend_go::legacy::TypeRow<'source>,
        fields: &mut Vec<u32>,
        owner: u32,
        package: &'source [u8],
        type_name: &'source [u8],
    ) -> Result<(), GoCollectError> {
        let mut field_index = 0usize;
        for member_index in member_run(row) {
            let member = self
                .image
                .member(member_index)
                .map_err(GoCollectError::Image)?;
            if member.kind != MemberKind::Field {
                continue;
            }
            let root = self.root(member.type_root, TypeReason::OracleGap)?;
            let mut fact = root.attach(SemanticFact::new(
                EntityKind::Field,
                member.name,
                LEAF_PRODUCT,
            ));
            if is_unbound_name(member.name) {
                fact = fact.with_identity_discriminator(unbound_discriminator(field_index));
            }
            field_index += 1;
            let ordinal = push(self.facts, fact)?;
            self.facts
                .attach_parent(ordinal, owner)
                .map_err(|fault| lane_terminal_ordinal(ordinal, member.name.len(), fault))?;
            self.members.push(MemberKey {
                package,
                type_name,
                member: member.name,
                ordinal,
                is_field: true,
            });
            fields.push(ordinal);
            self.member_ordinals[member_index] = Some(ordinal);
        }
        Ok(())
    }

    /// Pushes one function fact per interface method signature, registering
    /// each under its interface type's spelling for member-target
    /// resolution.
    fn interface_methods(
        &mut self,
        row: &backend_frontend_go::legacy::TypeRow<'source>,
        methods: &mut Vec<(u32, &'source [u8])>,
        owner: u32,
        package: &'source [u8],
        type_name: &'source [u8],
    ) -> Result<(), GoCollectError> {
        for member_index in member_run(row) {
            let member = self
                .image
                .member(member_index)
                .map_err(GoCollectError::Image)?;
            if member.kind != MemberKind::Method {
                continue;
            }
            let start = u32::try_from(self.facts.type_parameter_len).map_err(|_| {
                terminal(ProjectionFault::IndexCapacity {
                    phase: GoProjectionIndexPhase::TypeParameter,
                    observed: self.facts.type_parameter_len as u64,
                })
            })?;
            let ordinal = self.executable(member.name, member.type_root, start)?;
            self.facts
                .attach_parent(ordinal, owner)
                .map_err(|fault| lane_terminal_ordinal(ordinal, member.name.len(), fault))?;
            self.members.push(MemberKey {
                package,
                type_name,
                member: member.name,
                ordinal,
                is_field: false,
            });
            methods.push((ordinal, member.name));
            self.member_ordinals[member_index] = Some(ordinal);
        }
        Ok(())
    }

    /// Pass two: one package-level function.
    pub(super) fn function(
        &mut self,
        index: usize,
        declaration: &Declaration<'source>,
    ) -> Result<(), GoCollectError> {
        let parameter_start = u32::try_from(self.facts.type_parameter_len).map_err(|_| {
            terminal(ProjectionFault::IndexCapacity {
                phase: GoProjectionIndexPhase::TypeParameter,
                observed: self.facts.type_parameter_len as u64,
            })
        })?;
        self.type_parameters(index)?;
        let ordinal = self.executable(declaration.name, declaration.type_root, parameter_start)?;
        self.facts
            .mark_parentage_root(ordinal)
            .map_err(|fault| lane_terminal_ordinal(ordinal, declaration.name.len(), fault))?;
        self.declaration_ordinals[index] = Some(ordinal);
        self.record_name(declaration.package, declaration.name, ordinal);
        self.record_declaration_spans(index, declaration, ordinal)?;
        Ok(())
    }

    /// Pass two: one constant or variable with its projected declared type.
    pub(super) fn value(
        &mut self,
        index: usize,
        declaration: &Declaration<'source>,
    ) -> Result<(), GoCollectError> {
        let kind = entity_kind(declaration.kind);
        let root = self.root(declaration.type_root, TypeReason::Unannotated)?;
        let (constant_value, constant_group, constant_flags) = if kind == EntityKind::Constant {
            if declaration.value.is_empty() {
                (
                    AtomListId::new(0),
                    declaration.const_group,
                    u32::from(declaration.iota),
                )
            } else {
                let atom = self.facts.intern_atom(declaration.value).map_err(|fault| {
                    lane_terminal(self.facts.len(), declaration.name.len(), fault)
                })?;
                let list = self
                    .facts
                    .intern_atom_list(core::slice::from_ref(&atom))
                    .map_err(|fault| {
                        lane_terminal(self.facts.len(), declaration.name.len(), fault)
                    })?;
                (list, declaration.const_group, u32::from(declaration.iota))
            }
        } else {
            (AtomListId::new(0), 0, 0)
        };
        let empty_type_parameters = u32::try_from(self.facts.type_parameter_len).map_err(|_| {
            terminal(ProjectionFault::IndexCapacity {
                phase: GoProjectionIndexPhase::TypeParameter,
                observed: self.facts.type_parameter_len as u64,
            })
        })?;
        let mut fact = root
            .attach(SemanticFact::new(kind, declaration.name, constructor(kind)))
            .with_extension(EmissionExtension::Go(GoFacts {
                signature: GoSignature {
                    parameters: TypeListId::new(0),
                    results: TypeListId::new(0),
                    variadic: false,
                },
                type_parameters: TypeParameterListId::new(empty_type_parameters),
                fields: EntityListId::new(0),
                method_set: EntityListId::new(0),
                build_constraints: AtomListId::new(0),
                constant_value,
                constant_group,
                constant_flags,
            }));
        if declaration.kind == DeclarationKind::Static && is_unbound_name(declaration.name) {
            fact = fact.with_identity_discriminator(unbound_discriminator(index));
        }
        let ordinal = push(self.facts, fact)?;
        self.facts
            .mark_parentage_root(ordinal)
            .map_err(|fault| lane_terminal_ordinal(ordinal, declaration.name.len(), fault))?;
        self.declaration_ordinals[index] = Some(ordinal);
        if !is_unbound_name(declaration.name) {
            self.record_name(declaration.package, declaration.name, ordinal);
        }
        self.record_declaration_spans(index, declaration, ordinal)?;
        Ok(())
    }

    /// Pass three: one fact per declaration excluded by a build constraint,
    /// carrying the normalized constraint expression on its Go extension
    /// row.
    pub(super) fn constraints(&mut self) -> Result<(), GoCollectError> {
        for index in 0..self.image.constraint_count() {
            let row = self
                .image
                .constraint(index)
                .map_err(GoCollectError::Image)?;
            let owner = go_u32(index, GoProjectionIndexPhase::Constraint).map_err(
                |(phase, observed)| terminal(ProjectionFault::IndexCapacity { phase, observed }),
            )?;
            let exported = parse_constraint_blob(row.exported, row.exported_count)
                .ok_or_else(|| terminal(ProjectionFault::OrphanOwner { owner }))?;
            let atom = self
                .facts
                .intern_atom(row.constraint)
                .map_err(|fault| lane_terminal(self.facts.len(), 0, fault))?;
            // The excluded file is an authority owner the lane emits no entity
            // for, and it is the only thing that keeps mutually-exclusive
            // declarations distinct: `colorable_windows.go` and
            // `colorable_appengine.go` both spell `NewColorableStdout`. The
            // normalized build-constraint expression is coordinate-free and,
            // because two files that share an expression can never both
            // declare one name, a stable digest of it is a collision-free
            // owner identity.
            let mut hasher = Sha256::new();
            hasher.update(b"nudox.go.build-constraint-owner.v1\0");
            hasher.update(row.constraint);
            let mut owner_identity = [0_u8; 16];
            owner_identity.copy_from_slice(&hasher.finalize()[..16]);
            let list = self
                .facts
                .intern_atom_list(core::slice::from_ref(&atom))
                .map_err(|fault| lane_terminal(self.facts.len(), 0, fault))?;
            let empty_type_parameters =
                u32::try_from(self.facts.type_parameter_len).map_err(|_| {
                    terminal(ProjectionFault::IndexCapacity {
                        phase: GoProjectionIndexPhase::TypeParameter,
                        observed: self.facts.type_parameter_len as u64,
                    })
                })?;
            for declaration in exported {
                let kind = entity_kind(declaration.kind);
                let fact = SemanticFact::new(kind, declaration.name, constructor(kind))
                    .typed(unknown_record(TypeReason::OracleGap, None))
                    .with_extension(EmissionExtension::Go(GoFacts {
                        signature: GoSignature {
                            parameters: TypeListId::new(0),
                            results: TypeListId::new(0),
                            variadic: false,
                        },
                        type_parameters: TypeParameterListId::new(empty_type_parameters),
                        fields: EntityListId::new(0),
                        method_set: EntityListId::new(0),
                        build_constraints: list,
                        constant_value: AtomListId::new(0),
                        constant_group: 0,
                        constant_flags: 0,
                    }));
                let ordinal = push(self.facts, fact)?;
                self.facts
                    .mark_unrepresented_parent(ordinal, owner_identity)
                    .map_err(|fault| {
                        lane_terminal_ordinal(ordinal, declaration.name.len(), fault)
                    })?;
            }
        }
        Ok(())
    }

    /// Pass four: one text run per documentation line, soft-break
    /// separated, owned by the pushed fact of the row's owner. Package-level
    /// doc rows have no lane owner — the fact lane owns no package entity —
    /// so they stay image-only facts like the receiver spellings.
    pub(super) fn docs(&mut self) -> Result<(), GoCollectError> {
        // The validated image owns the complete declaration/method/member
        // documentation plane. Mark every source declaration before rows are
        // decoded so an empty doc list remains captured truth, while callable
        // carrier facts stay outside this source-declaration relation.
        for owner in self
            .declaration_ordinals
            .iter()
            .chain(self.method_ordinals.iter())
            .chain(self.member_ordinals.iter())
            .filter_map(|ordinal| *ordinal)
        {
            self.facts
                .mark_documentation_captured(owner)
                .map_err(|fault| lane_terminal_ordinal(owner, 0, fault))?;
        }
        for index in 0..self.image.doc_count() {
            let row = self.image.doc(index).map_err(GoCollectError::Image)?;
            let owner = match row.owner_kind {
                DocOwner::Package => continue,
                DocOwner::Declaration => self
                    .declaration_ordinals
                    .get(index_of(row.owner))
                    .copied()
                    .flatten(),
                DocOwner::Method => self
                    .method_ordinals
                    .get(index_of(row.owner))
                    .copied()
                    .flatten(),
                DocOwner::Member => self
                    .member_ordinals
                    .get(index_of(row.owner))
                    .copied()
                    .flatten(),
            }
            .ok_or_else(|| terminal(ProjectionFault::OrphanOwner { owner: row.owner }))?;
            push_doc_lines(self.facts, owner, row.text)
                .map_err(|fault| lane_terminal_ordinal(owner, 0, fault))?;
        }
        Ok(())
    }

    /// Pass five: one oracle-confidence occurrence per reference row. The
    /// image's reference plane carries every go/types use of a keyable
    /// object, each with its closed use kind, the used object's closed
    /// class, and the receiver type name for method and field targets.
    /// Resolution keeps the call-graph law: a same-package target resolves
    /// to its local fact when the lane carries one (package scope by name,
    /// members by receiver type and name); everything else — every foreign
    /// package, every promoted or otherwise unlocalizable member — stays a
    /// typed foreign `go` lineage key. Owners lift relative spans over the
    /// authority-bound source spans attached in passes one and two, so the
    /// shared containment law places every site in the exact source bytes
    /// of the used identifier.
    pub(super) fn occurrences(&mut self) -> Result<(), GoCollectError> {
        for index in 0..self.image.reference_count() {
            let reference_index =
                go_u32(index, GoProjectionIndexPhase::Reference).map_err(|(phase, observed)| {
                    terminal(ProjectionFault::IndexCapacity { phase, observed })
                })?;
            let row = self.image.reference(index).map_err(GoCollectError::Image)?;
            let owner = if row.owner_is_declaration {
                self.declaration_ordinals
                    .get(index_of(row.owner_row))
                    .copied()
                    .flatten()
            } else {
                self.method_ordinals
                    .get(index_of(row.owner_row))
                    .copied()
                    .flatten()
            }
            .ok_or_else(|| {
                terminal(ProjectionFault::OrphanOwner {
                    owner: row.owner_row,
                })
            })?;
            let kind = occurrence_kind(row.use_kind, row.target_class);
            // The package whose lineage keys an unresolvable target: the
            // target's own declaring package when it declares elsewhere,
            // else the owner's package.
            let owner_package = if row.owner_is_declaration {
                self.image
                    .declaration(index_of(row.owner_row))
                    .map_err(GoCollectError::Image)?
                    .package
            } else {
                let method = self
                    .image
                    .method(index_of(row.owner_row))
                    .map_err(GoCollectError::Image)?;
                self.image
                    .declaration(index_of(method.owner))
                    .map_err(GoCollectError::Image)?
                    .package
            };
            let target = if row.target_class == ReferenceTargetClass::Pkg {
                // An imported package binding: the used object is the
                // package itself, keyed by its import path under the `go`
                // ecosystem. The lane owns no package entities, so an
                // import use is always a typed foreign key.
                let package = if row.target_package.is_empty() {
                    owner_package
                } else {
                    row.target_package
                };
                foreign_target(reference_index, package, row.target, EntityKind::Module)?
            } else if row.target_package.is_empty() {
                let local = self
                    .lookup(owner_package, row.target)
                    .map(|ordinal| OccurrenceTarget::Local(EntityId::new(ordinal)))
                    .or_else(|| {
                        self.lookup_member(owner_package, row.recv_type, row.target)
                            .map(|(ordinal, _)| OccurrenceTarget::Local(EntityId::new(ordinal)))
                    });
                match local {
                    Some(target) => target,
                    None => {
                        // Same-package target with no local fact: promoted
                        // members, blank-keyed fields, or build-excluded
                        // declarations. The key keeps the exact spelling
                        // under the declaring package's lineage.
                        foreign_target(
                            reference_index,
                            owner_package,
                            row.target,
                            foreign_entity_kind(row.target_class),
                        )?
                    }
                }
            } else {
                let local = self
                    .lookup(row.target_package, row.target)
                    .map(|ordinal| OccurrenceTarget::Local(EntityId::new(ordinal)))
                    .or_else(|| {
                        self.lookup_member(row.target_package, row.recv_type, row.target)
                            .map(|(ordinal, _)| OccurrenceTarget::Local(EntityId::new(ordinal)))
                    });
                match local {
                    Some(target) => target,
                    None => foreign_target(
                        reference_index,
                        row.target_package,
                        row.target,
                        foreign_entity_kind(row.target_class),
                    )?,
                }
            };
            let span = RelSpan::new(row.relative.0, row.relative.1)
                .map_err(|fault| match fault {
                    RelSpanFault::Inverted { start, end } => ProjectionFault::Span {
                        row: reference_index,
                        start,
                        end,
                    },
                })
                .map_err(terminal)?;
            self.facts
                .push_occurrence(
                    owner,
                    Occurrence {
                        target,
                        kind,
                        confidence: OccurrenceConfidence::Oracle,
                        span,
                    },
                )
                .map_err(|fault| lane_terminal_ordinal(owner, 0, fault))?;
        }
        Ok(())
    }

    /// Pass six: one oracle-confidence type-reference occurrence per
    /// interface-satisfaction edge, owned by the satisfying type's fact.
    /// In-package targets resolve to their local nominal ordinals;
    /// cross-package targets fold to foreign `go` lineage keys. Go never
    /// writes the satisfaction relation, so the occurrence anchors on the
    /// subject's declared identifier: when the image carries the subject's
    /// authority-bound NAME-TOKEN extent, the occurrence's owner-relative
    /// span is exactly that extent — a real extent whose lifted site
    /// verifies against the declared identifier's source bytes; without
    /// one, the occurrence keeps the position-free zero-width spelling at
    /// the owner's start.
    pub(super) fn satisfactions(&mut self) -> Result<(), GoCollectError> {
        for index in 0..self.image.satisfaction_count() {
            let reference_index = go_u32(index, GoProjectionIndexPhase::Satisfaction).map_err(
                |(phase, observed)| terminal(ProjectionFault::IndexCapacity { phase, observed }),
            )?;
            let row = self
                .image
                .satisfaction(index)
                .map_err(GoCollectError::Image)?;
            let subject_index = index_of(row.subject);
            let subject = self
                .declaration_ordinals
                .get(subject_index)
                .copied()
                .flatten()
                .ok_or_else(|| terminal(ProjectionFault::OrphanOwner { owner: row.subject }))?;
            // Satisfaction targets are declared in the target package, not
            // necessarily in the satisfying subject's package. An absent
            // target package means the subject's declaration package.
            let subject_package = self
                .image
                .declaration(subject_index)
                .map_err(GoCollectError::Image)?
                .package;
            let target_package = if row.target_package.is_empty() {
                subject_package
            } else {
                row.target_package
            };
            let target = if let Some(ordinal) = self.lookup(target_package, row.target) {
                OccurrenceTarget::Local(EntityId::new(ordinal))
            } else {
                // A satisfaction target is an interface by the plane's
                // construction, so its honest foreign kind is the trait —
                // the same kind a local interface projects.
                foreign_target(
                    reference_index,
                    row.target_package,
                    row.target,
                    EntityKind::Trait,
                )?
            };
            // The NAME-TOKEN extent relative to the declaration-extent basis
            // the subject's source span was attached with. Both spans come
            // from one bound row and the image proves the identifier sits
            // inside its declaration, so the relative span is ordered and
            // contained; any other shape is a typed span fault.
            let span = match (
                self.declaration_spans[subject_index],
                self.name_spans[subject_index],
            ) {
                (Some((base, _)), Some((name_start, name_end))) => {
                    let start = name_start.checked_sub(base).ok_or_else(|| {
                        terminal(ProjectionFault::Span {
                            row: reference_index,
                            start: name_start,
                            end: base,
                        })
                    })?;
                    let end = name_end.checked_sub(base).ok_or_else(|| {
                        terminal(ProjectionFault::Span {
                            row: reference_index,
                            start: name_start,
                            end: base,
                        })
                    })?;
                    RelSpan::new(start, end)
                        .map_err(|fault| match fault {
                            RelSpanFault::Inverted { start, end } => ProjectionFault::Span {
                                row: reference_index,
                                start,
                                end,
                            },
                        })
                        .map_err(terminal)?
                }
                _ => RelSpan::new(0, 0)
                    .map_err(|fault| match fault {
                        RelSpanFault::Inverted { start, end } => ProjectionFault::Span {
                            row: reference_index,
                            start,
                            end,
                        },
                    })
                    .map_err(terminal)?,
            };
            self.facts
                .push_occurrence(
                    subject,
                    Occurrence {
                        target,
                        kind: ReferenceKind::TypeReference,
                        confidence: OccurrenceConfidence::Oracle,
                        span,
                    },
                )
                .map_err(|fault| lane_terminal_ordinal(subject, 0, fault))?;
        }
        Ok(())
    }

    /// Pushes one executable fact: parameter and result carrier facts
    /// first, then the function fact whose product and function-pointer
    /// children are exactly those carriers, with its signature on the Go
    /// extension row.
    fn executable(
        &mut self,
        name: &'source [u8],
        signature: Option<u32>,
        parameter_start: u32,
    ) -> Result<u32, GoCollectError> {
        let mut carriers: Vec<TypeChild<'source>> = Vec::new();
        let mut parameters = Vec::new();
        let mut results = Vec::new();
        let mut variadic = false;
        let record = match signature {
            None => unknown_record(TypeReason::OracleGap, None),
            Some(row_index) => {
                let row = self
                    .image
                    .type_row(index_of(row_index))
                    .map_err(GoCollectError::Image)?;
                variadic = row.variadic;
                let children = self.row_children(&row)?;
                let param_count = usize::try_from(row.param_count)
                    .map_err(|_| {
                        terminal(ProjectionFault::IndexCapacity {
                            phase: GoProjectionIndexPhase::TypeRow,
                            observed: u64::from(row.param_count),
                        })
                    })?
                    .min(children.len());
                for (ordinal_in_signature, child) in children.iter().take(param_count).enumerate() {
                    let name = self.signature_parameter_name(row_index, ordinal_in_signature)?;
                    let ordinal = self.carrier(*child, name)?;
                    parameters.push(ordinal);
                    carriers.push(TypeChild {
                        target: ordinal,
                        name: None,
                        flags: 0,
                    });
                }
                for (ordinal_in_signature, child) in children.iter().skip(param_count).enumerate() {
                    let ordinal_in_signature = param_count + ordinal_in_signature;
                    let name = self.signature_parameter_name(row_index, ordinal_in_signature)?;
                    let ordinal = self.carrier(*child, name)?;
                    results.push(ordinal);
                    // Only a source-named result labels its callable slot;
                    // the positional `_`/`_1` carrier spelling is identity,
                    // not a name the declaration wrote.
                    let label = self.source_parameter_name(row_index, ordinal_in_signature)?;
                    carriers.push(TypeChild {
                        target: ordinal,
                        name: label,
                        flags: 0,
                    });
                }
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::FunctionPointer);
                record.payload1 = u32::try_from(results.len()).map_err(|_| {
                    terminal(ProjectionFault::IndexCapacity {
                        phase: GoProjectionIndexPhase::TypeRow,
                        observed: results.len() as u64,
                    })
                })?;
                if variadic {
                    let final_parameter = parameters.len().checked_sub(1).ok_or_else(|| {
                        terminal(ProjectionFault::VariadicWithoutParameter {
                            signature: row_index,
                        })
                    })?;
                    let parameter = carriers.get_mut(final_parameter).ok_or_else(|| {
                        terminal(ProjectionFault::VariadicWithoutParameter {
                            signature: row_index,
                        })
                    })?;
                    record.payload0 = SemanticTypeRecord::FUNCTION_TYPED_VARIADIC_FLAG;
                    parameter.flags |= SemanticTypeChild::FLAG_REST;
                }
                record
            }
        };
        let parameter_list = self
            .facts
            .intern_type_list(&parameters)
            .map_err(|fault| lane_terminal(self.facts.len(), name.len(), fault))?;
        let result_list = self
            .facts
            .intern_type_list(&results)
            .map_err(|fault| lane_terminal(self.facts.len(), name.len(), fault))?;
        let arity = u32::try_from(parameters.len()).map_err(|_| {
            terminal(ProjectionFault::IndexCapacity {
                phase: GoProjectionIndexPhase::TypeRow,
                observed: parameters.len() as u64,
            })
        })?;
        let results_count = u32::try_from(results.len()).map_err(|_| {
            terminal(ProjectionFault::IndexCapacity {
                phase: GoProjectionIndexPhase::TypeRow,
                observed: results.len() as u64,
            })
        })?;
        let mut fact = SemanticFact::new(
            EntityKind::Function,
            name,
            SemanticProductConstructor::function(arity, results_count),
        )
        .typed(record)
        .with_extension(EmissionExtension::Go(GoFacts {
            signature: GoSignature {
                parameters: parameter_list,
                results: result_list,
                variadic,
            },
            type_parameters: TypeParameterListId::new(parameter_start),
            fields: EntityListId::new(0),
            method_set: EntityListId::new(0),
            build_constraints: AtomListId::new(0),
            constant_value: AtomListId::new(0),
            constant_group: 0,
            constant_flags: 0,
        }));
        for ordinal in &parameters {
            fact = fact.child(ProductChildRole::FunctionParameter, *ordinal);
        }
        for ordinal in &results {
            fact = fact.child(ProductChildRole::FunctionResult, *ordinal);
        }
        for carrier in carriers {
            fact = fact.type_child(carrier.target, carrier.name, carrier.flags);
        }
        let ordinal = push(self.facts, fact)?;
        // Signature carriers share names and types across functions; bind
        // each to its executable so identical carriers stay distinct.
        for carrier in parameters.iter().chain(results.iter()) {
            self.facts
                .attach_parent(*carrier, ordinal)
                .map_err(|fault| lane_terminal_ordinal(*carrier, name.len(), fault))?;
        }
        Ok(ordinal)
    }

    /// Pushes one parameter or result carrier fact whose root record is the
    /// projected parameter type.
    fn signature_parameter_name(
        &self,
        owner: u32,
        ordinal: usize,
    ) -> Result<&'source [u8], GoCollectError> {
        // An unnamed carrier is absent from the image; a source may also spell
        // one with Go's blank identifier. Both are blanks, so both take the
        // positional spelling: `_` at position zero and `_1`, `_2`, … after
        // it. Without this, a signature such as
        // `filter(_ *state, _ reflect.Type, _, _ reflect.Value)` frames four
        // byte-identical carrier identities and collides as a duplicate.
        Ok(self
            .source_parameter_name(owner, ordinal)?
            .unwrap_or_else(|| absent_name(ordinal)))
    }

    /// The carrier name exactly as the source wrote it; `None` for an
    /// unnamed carrier or Go's blank identifier.
    fn source_parameter_name(
        &self,
        owner: u32,
        ordinal: usize,
    ) -> Result<Option<&'source [u8]>, GoCollectError> {
        let parameter = self
            .image
            .signature_parameters()
            .enumerate()
            .find_map(|(_, row)| match row {
                Ok(row) if row.owner == owner && row.ordinal == ordinal as u32 => {
                    Some(Ok(row.name))
                }
                Ok(_) => None,
                Err(error) => Some(Err(GoCollectError::Image(error))),
            })
            .transpose()?
            .unwrap_or(&[]);
        Ok((!parameter.is_empty() && parameter != b"_").then_some(parameter))
    }

    fn carrier(&mut self, row_index: u32, name: &'source [u8]) -> Result<u32, GoCollectError> {
        let root = self.root(Some(row_index), TypeReason::OracleGap)?;
        let fact = root.attach(SemanticFact::new(EntityKind::Parameter, name, LEAF_PRODUCT));
        push(self.facts, fact)
    }

    /// Projects one optional type-row coordinate as a fact's root record.
    /// A missing coordinate folds to the caller's typed unknown reason.
    fn root(
        &mut self,
        cell: Option<u32>,
        missing: TypeReason,
    ) -> Result<RootType<'source>, GoCollectError> {
        let Some(row_index) = cell else {
            return Ok(RootType::leaf(unknown_record(missing, None)));
        };
        self.root_row(row_index, DEPTH_LIMIT)
    }

    /// Projects one image type row in fact-root context: the row's record
    /// becomes the fact's own record and its children may name fact
    /// ordinals as well as anonymous rows.
    fn root_row(
        &mut self,
        row_index: u32,
        depth: usize,
    ) -> Result<RootType<'source>, GoCollectError> {
        if depth == 0 {
            return Err(terminal(ProjectionFault::Depth {
                type_row: row_index,
            }));
        }
        let row = self
            .image
            .type_row(index_of(row_index))
            .map_err(GoCollectError::Image)?;
        match row.kind {
            TypeRowKind::Basic => Ok(RootType::leaf(self.basic_leaf(&row))),
            TypeRowKind::Named | TypeRowKind::Alias => self.named_root(&row),
            TypeRowKind::TypeParam => {
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::TypeVar);
                record.text = Some(row.name);
                Ok(RootType::leaf(record))
            }
            TypeRowKind::Invalid => Ok(RootType::leaf(unknown_record(TypeReason::OracleGap, None))),
            TypeRowKind::Pointer => {
                let children = self.row_children(&row)?;
                self.unary_root(
                    SemanticTypeRecord::leaf(SemanticTypeTag::Primitive)
                        .with_shape(SHAPE_MUT_POINTER),
                    children.first().copied(),
                    depth,
                )
            }
            TypeRowKind::Slice => {
                let children = self.row_children(&row)?;
                self.unary_root(
                    SemanticTypeRecord::leaf(SemanticTypeTag::Slice),
                    children.first().copied(),
                    depth,
                )
            }
            TypeRowKind::Array => {
                let children = self.row_children(&row)?;
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::ArrayFixed);
                let length = u64::try_from(row.length).map_err(|_| {
                    terminal(ProjectionFault::Image(ImageError::ArrayLength {
                        index: index_of(row_index),
                        length: row.length,
                    }))
                })?;
                let bytes = length.to_le_bytes();
                record.payload0 = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                record.payload1 = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
                self.unary_root(record, children.first().copied(), depth)
            }
            TypeRowKind::Map => {
                let children = self.row_children(&row)?;
                let mut projected = RootType {
                    record: SemanticTypeRecord::leaf(SemanticTypeTag::Map),
                    children: Vec::new(),
                };
                for child in children {
                    let target = self.coordinate(Some(child), depth - 1)?;
                    projected.children.push(TypeChild {
                        target,
                        name: None,
                        flags: 0,
                    });
                }
                Ok(projected)
            }
            TypeRowKind::Chan => {
                let children = self.row_children(&row)?;
                let mut projected = RootType {
                    record: {
                        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Channel);
                        record.payload0 = match row.dir {
                            ChanDir::Both => ChannelDirection::Both as u32,
                            ChanDir::Send => ChannelDirection::Send as u32,
                            ChanDir::Recv => ChannelDirection::Receive as u32,
                        };
                        record
                    },
                    children: Vec::new(),
                };
                for child in children {
                    let target = self.coordinate(Some(child), depth - 1)?;
                    projected.children.push(TypeChild {
                        target,
                        name: None,
                        flags: 0,
                    });
                }
                Ok(projected)
            }
            TypeRowKind::Func => {
                let children = self.row_children(&row)?;
                let param_count = usize::try_from(row.param_count)
                    .map_err(|_| {
                        terminal(ProjectionFault::IndexCapacity {
                            phase: GoProjectionIndexPhase::TypeRow,
                            observed: u64::from(row.param_count),
                        })
                    })?
                    .min(children.len());
                let mut projected = RootType {
                    record: {
                        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::FunctionPointer);
                        record.payload1 =
                            u32::try_from(children.len() - param_count).map_err(|_| {
                                lane_terminal(self.facts.len(), 0, FactFault::TypeChildCapacity)
                            })?;
                        if row.variadic {
                            record.payload0 = SemanticTypeRecord::FUNCTION_TYPED_VARIADIC_FLAG;
                        }
                        record
                    },
                    children: Vec::new(),
                };
                for (position, child) in children.into_iter().enumerate() {
                    let target = self.coordinate(Some(child), depth - 1)?;
                    projected.children.push(TypeChild {
                        target,
                        name: None,
                        flags: if row.variadic && position + 1 == param_count {
                            SemanticTypeChild::FLAG_REST
                        } else {
                            0
                        },
                    });
                }
                Ok(projected)
            }
            TypeRowKind::Struct => {
                let mut projected = RootType {
                    record: anonymous_record(ANON_STRUCT),
                    children: Vec::new(),
                };
                for member_index in member_run(&row) {
                    let member = self
                        .image
                        .member(member_index)
                        .map_err(GoCollectError::Image)?;
                    let target = self.coordinate(member.type_root, depth - 1)?;
                    projected.children.push(TypeChild {
                        target,
                        name: Some(member.name),
                        flags: 0,
                    });
                }
                Ok(projected)
            }
            TypeRowKind::Interface => {
                if row.members.1 == 0 && row.children.1 == 0 {
                    return Ok(RootType::leaf(SemanticTypeRecord::leaf(
                        SemanticTypeTag::Any,
                    )));
                }
                let mut projected = RootType {
                    record: anonymous_record(ANON_INTERFACE),
                    children: Vec::new(),
                };
                for embedded in embedded_run(self.image, &row) {
                    let embedded = embedded.map_err(GoCollectError::Image)?;
                    let target = self.coordinate(Some(embedded), depth - 1)?;
                    let embedded_row = self
                        .image
                        .type_row(index_of(embedded))
                        .map_err(GoCollectError::Image)?;
                    projected.children.push(TypeChild {
                        target,
                        name: Some(embedded_name(&embedded_row)),
                        flags: 0,
                    });
                }
                for member_index in member_run(&row) {
                    let member = self
                        .image
                        .member(member_index)
                        .map_err(GoCollectError::Image)?;
                    if member.kind != MemberKind::Method {
                        continue;
                    }
                    let target = self.coordinate(member.type_root, depth - 1)?;
                    projected.children.push(TypeChild {
                        target,
                        name: Some(member.name),
                        flags: 0,
                    });
                }
                Ok(projected)
            }
            TypeRowKind::Union | TypeRowKind::Tuple => {
                let tag = match row.kind {
                    TypeRowKind::Union => SemanticTypeTag::Union,
                    _ => SemanticTypeTag::Tuple,
                };
                let children = self.row_children(&row)?;
                let mut projected = RootType {
                    record: SemanticTypeRecord::leaf(tag),
                    children: Vec::new(),
                };
                // The lattice's union children own no approximation cell,
                // so the image's tilde flags stay behind; the term shapes
                // are exact.
                for child in children {
                    let target = self.coordinate(Some(child), depth - 1)?;
                    projected.children.push(TypeChild {
                        target,
                        name: None,
                        flags: 0,
                    });
                }
                Ok(projected)
            }
        }
    }

    /// Projects one unary root: exactly one child coordinate or the typed
    /// depth-less unknown when the image row lost its element.
    fn unary_root(
        &mut self,
        record: SemanticTypeRecord<'source>,
        child: Option<u32>,
        depth: usize,
    ) -> Result<RootType<'source>, GoCollectError> {
        let mut projected = RootType {
            record,
            children: Vec::new(),
        };
        let target = self.coordinate(child, depth - 1)?;
        projected.children.push(TypeChild {
            target,
            name: None,
            flags: 0,
        });
        Ok(projected)
    }

    /// Projects one named or alias row in fact-root context: a pure local
    /// name becomes its nominal fact ordinal, a local application carries
    /// the base fact and its projected arguments, and every foreign or
    /// universe name folds to the typed unknown that retains its spelling.
    fn named_root(
        &mut self,
        row: &backend_frontend_go::legacy::TypeRow<'source>,
    ) -> Result<RootType<'source>, GoCollectError> {
        if !row.package.is_empty()
            && let Some(base) = self.lookup(row.package, row.name)
        {
            if row.children.1 == 0 {
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Nominal);
                record.nominal = Some(NominalRef::Local(EntityId::new(base)));
                return Ok(RootType::leaf(record));
            }
            let children = self.row_children(row)?;
            let mut projected = RootType {
                record: SemanticTypeRecord::leaf(SemanticTypeTag::Apply),
                children: vec![TypeChild {
                    target: base,
                    name: None,
                    flags: 0,
                }],
            };
            for child in children {
                let target = self.coordinate(Some(child), DEPTH_LIMIT - 1)?;
                projected.children.push(TypeChild {
                    target,
                    name: None,
                    flags: 0,
                });
            }
            return Ok(projected);
        }
        let reason = if row.package.is_empty() {
            if row.name == b"any" {
                return Ok(RootType::leaf(SemanticTypeRecord::leaf(
                    SemanticTypeTag::Any,
                )));
            }
            // A universe named type (`error`, `comparable`) is a builtin
            // with no lattice row, never an unresolved external reference.
            TypeReason::NoIrRepresentation
        } else {
            TypeReason::UnresolvedExternal
        };
        Ok(RootType::leaf(unknown_record(reason, Some(row.name))))
    }

    /// Projects one optional type-row coordinate as a child coordinate: a
    /// local named reference stays a fact ordinal, everything else lives in
    /// the anonymous pool.
    fn coordinate(&mut self, cell: Option<u32>, depth: usize) -> Result<u32, GoCollectError> {
        let Some(row_index) = cell else {
            let anchor = self.anchor().map_err(terminal)?;
            return self.intern_anonymous(
                anchor,
                &AnonRow {
                    record: unknown_record(TypeReason::OracleGap, None),
                    children: Vec::new(),
                },
            );
        };
        if depth == 0 {
            return Err(terminal(ProjectionFault::Depth {
                type_row: row_index,
            }));
        }
        let row = self
            .image
            .type_row(index_of(row_index))
            .map_err(GoCollectError::Image)?;
        if matches!(row.kind, TypeRowKind::Named | TypeRowKind::Alias)
            && !row.package.is_empty()
            && row.children.1 == 0
            && let Some(local) = self.lookup(row.package, row.name)
        {
            return Ok(local);
        }
        self.project_anonymous(index_of(row_index), depth)
    }

    /// Projects one image type row in anonymous context, memoized per row:
    /// the result is an anonymous row coordinate whose subtree references
    /// only earlier anonymous rows, because the frozen wire orders the pool
    /// before the fact rows and rejects forward child targets.
    fn project_anonymous(&mut self, row_index: usize, depth: usize) -> Result<u32, GoCollectError> {
        let type_row =
            go_u32(row_index, GoProjectionIndexPhase::TypeRow).map_err(|(phase, observed)| {
                terminal(ProjectionFault::IndexCapacity { phase, observed })
            })?;
        if depth == 0 {
            return Err(terminal(ProjectionFault::Depth { type_row }));
        }
        let anchor = self.anchor().map_err(terminal)?;
        if let Some(memoized) = self.anonymous.get(row_index).copied().flatten() {
            if memoized.owner == anchor {
                return Ok(memoized.coordinate);
            }
        }
        let row = self
            .image
            .type_row(row_index)
            .map_err(GoCollectError::Image)?;
        let built = match row.kind {
            TypeRowKind::Basic => AnonRow {
                record: self.basic_leaf(&row),
                children: Vec::new(),
            },
            TypeRowKind::Named | TypeRowKind::Alias => AnonRow {
                record: unknown_record(TypeReason::NoIrRepresentation, Some(row.name)),
                children: Vec::new(),
            },
            TypeRowKind::TypeParam => {
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::TypeVar);
                record.text = Some(row.name);
                AnonRow {
                    record,
                    children: Vec::new(),
                }
            }
            TypeRowKind::Invalid => AnonRow {
                record: unknown_record(TypeReason::OracleGap, None),
                children: Vec::new(),
            },
            TypeRowKind::Pointer => {
                let children = self.row_children(&row)?;
                let mut built = AnonRow {
                    record: SemanticTypeRecord::leaf(SemanticTypeTag::Primitive)
                        .with_shape(SHAPE_MUT_POINTER),
                    children: Vec::new(),
                };
                if let Some(child) = children.first() {
                    let target = self.project_anonymous(index_of(*child), depth - 1)?;
                    built.children.push(TypeChild {
                        target,
                        name: None,
                        flags: 0,
                    });
                }
                built
            }
            TypeRowKind::Slice => {
                let children = self.row_children(&row)?;
                let mut built = AnonRow {
                    record: SemanticTypeRecord::leaf(SemanticTypeTag::Slice),
                    children: Vec::new(),
                };
                if let Some(child) = children.first() {
                    let target = self.project_anonymous(index_of(*child), depth - 1)?;
                    built.children.push(TypeChild {
                        target,
                        name: None,
                        flags: 0,
                    });
                }
                built
            }
            TypeRowKind::Array => {
                let children = self.row_children(&row)?;
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::ArrayFixed);
                let length = u64::try_from(row.length).map_err(|_| {
                    terminal(ProjectionFault::Image(ImageError::ArrayLength {
                        index: row_index,
                        length: row.length,
                    }))
                })?;
                let bytes = length.to_le_bytes();
                record.payload0 = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                record.payload1 = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
                let mut built = AnonRow {
                    record,
                    children: Vec::new(),
                };
                if let Some(child) = children.first() {
                    let target = self.project_anonymous(index_of(*child), depth - 1)?;
                    built.children.push(TypeChild {
                        target,
                        name: None,
                        flags: 0,
                    });
                }
                built
            }
            TypeRowKind::Map => {
                let children = self.row_children(&row)?;
                let mut built = AnonRow {
                    record: SemanticTypeRecord::leaf(SemanticTypeTag::Map),
                    children: Vec::new(),
                };
                for child in children {
                    let target = self.project_anonymous(index_of(child), depth - 1)?;
                    built.children.push(TypeChild {
                        target,
                        name: None,
                        flags: 0,
                    });
                }
                built
            }
            TypeRowKind::Chan => {
                let children = self.row_children(&row)?;
                let mut built = AnonRow {
                    record: {
                        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Channel);
                        record.payload0 = match row.dir {
                            ChanDir::Both => ChannelDirection::Both as u32,
                            ChanDir::Send => ChannelDirection::Send as u32,
                            ChanDir::Recv => ChannelDirection::Receive as u32,
                        };
                        record
                    },
                    children: Vec::new(),
                };
                for child in children {
                    let target = self.project_anonymous(index_of(child), depth - 1)?;
                    built.children.push(TypeChild {
                        target,
                        name: None,
                        flags: 0,
                    });
                }
                built
            }
            TypeRowKind::Func => {
                let children = self.row_children(&row)?;
                let param_count = usize::try_from(row.param_count)
                    .map_err(|_| {
                        terminal(ProjectionFault::IndexCapacity {
                            phase: GoProjectionIndexPhase::TypeRow,
                            observed: u64::from(row.param_count),
                        })
                    })?
                    .min(children.len());
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::FunctionPointer);
                record.payload1 = u32::try_from(children.len() - param_count).map_err(|_| {
                    lane_terminal(self.facts.len(), 0, FactFault::TypeChildCapacity)
                })?;
                if row.variadic {
                    record.payload0 = SemanticTypeRecord::FUNCTION_TYPED_VARIADIC_FLAG;
                }
                let mut built = AnonRow {
                    record,
                    children: Vec::new(),
                };
                for (position, child) in children.into_iter().enumerate() {
                    let target = self.project_anonymous(index_of(child), depth - 1)?;
                    built.children.push(TypeChild {
                        target,
                        name: None,
                        flags: if row.variadic && position + 1 == param_count {
                            SemanticTypeChild::FLAG_REST
                        } else {
                            0
                        },
                    });
                }
                built
            }
            TypeRowKind::Struct => {
                let mut built = AnonRow {
                    record: anonymous_record(ANON_STRUCT),
                    children: Vec::new(),
                };
                for member_index in member_run(&row) {
                    let member = self
                        .image
                        .member(member_index)
                        .map_err(GoCollectError::Image)?;
                    let target = match member.type_root {
                        Some(root) => self.project_anonymous(index_of(root), depth - 1)?,
                        None => self.coordinate(None, depth - 1)?,
                    };
                    built.children.push(TypeChild {
                        target,
                        name: Some(member.name),
                        flags: 0,
                    });
                }
                built
            }
            TypeRowKind::Interface => {
                if row.members.1 == 0 && row.children.1 == 0 {
                    AnonRow {
                        record: SemanticTypeRecord::leaf(SemanticTypeTag::Any),
                        children: Vec::new(),
                    }
                } else {
                    let mut built = AnonRow {
                        record: anonymous_record(ANON_INTERFACE),
                        children: Vec::new(),
                    };
                    for embedded in embedded_run(self.image, &row) {
                        let embedded = embedded.map_err(GoCollectError::Image)?;
                        let target = self.project_anonymous(index_of(embedded), depth - 1)?;
                        let embedded_row = self
                            .image
                            .type_row(index_of(embedded))
                            .map_err(GoCollectError::Image)?;
                        built.children.push(TypeChild {
                            target,
                            name: Some(embedded_name(&embedded_row)),
                            flags: 0,
                        });
                    }
                    for member_index in member_run(&row) {
                        let member = self
                            .image
                            .member(member_index)
                            .map_err(GoCollectError::Image)?;
                        if member.kind != MemberKind::Method {
                            continue;
                        }
                        let target = match member.type_root {
                            Some(root) => self.project_anonymous(index_of(root), depth - 1)?,
                            None => self.coordinate(None, depth - 1)?,
                        };
                        built.children.push(TypeChild {
                            target,
                            name: Some(member.name),
                            flags: 0,
                        });
                    }
                    built
                }
            }
            TypeRowKind::Union | TypeRowKind::Tuple => {
                let tag = match row.kind {
                    TypeRowKind::Union => SemanticTypeTag::Union,
                    _ => SemanticTypeTag::Tuple,
                };
                let children = self.row_children(&row)?;
                let mut built = AnonRow {
                    record: SemanticTypeRecord::leaf(tag),
                    children: Vec::new(),
                };
                for child in children {
                    let target = self.project_anonymous(index_of(child), depth - 1)?;
                    built.children.push(TypeChild {
                        target,
                        name: None,
                        flags: 0,
                    });
                }
                built
            }
        };
        let coordinate = self.intern_anonymous(anchor, &built)?;
        if let Some(slot) = self.anonymous.get_mut(row_index) {
            *slot = Some(AnonymousMemo {
                owner: anchor,
                coordinate,
            });
        }
        Ok(coordinate)
    }

    /// Appends one anonymous row's children into the pooled lane and interns
    /// the row under the given owner fact.
    fn intern_anonymous(
        &mut self,
        anchor: u32,
        row: &AnonRow<'source>,
    ) -> Result<u32, GoCollectError> {
        for child in &row.children {
            self.facts
                .anonymous_type_child(child.target, child.name, child.flags)
                .map_err(|fault| lane_terminal(self.facts.len(), 0, fault))?;
        }
        let fact_count = u32::try_from(self.facts.len()).map_err(|_| {
            terminal(ProjectionFault::IndexCapacity {
                phase: GoProjectionIndexPhase::FactOrdinal,
                observed: self.facts.len() as u64,
            })
        })?;
        if anchor < fact_count {
            self.facts
                .intern_anonymous_type_row(anchor, row.record)
                .map_err(|fault| lane_terminal(self.facts.len(), 0, fault))
        } else {
            self.facts
                .intern_reserved_anchor_type_row(anchor, row.record)
                .map_err(|fault| lane_terminal(self.facts.len(), 0, fault))
        }
    }

    /// The exact lattice cells of one basic row: exact integer and float
    /// widths, `byte`/`rune` aliases folded to their underlying widths,
    /// complex spellings on the builtin shape, and the untyped constant
    /// kinds with an exact lattice equivalent (Go's untyped integer and rune
    /// constants are exact arbitrary-precision integers). Every other
    /// universe basic (`untyped float`, `untyped nil`, `unsafe.Pointer`) is
    /// a builtin the lattice cannot represent, never an unresolved external,
    /// so it keeps its spelling under `NoIrRepresentation`.
    fn basic_leaf(
        &self,
        row: &backend_frontend_go::legacy::TypeRow<'source>,
    ) -> SemanticTypeRecord<'source> {
        let signed = |width: u32| {
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
            record.payload0 = SHAPE_INTEGER;
            record.payload1 = (width << INTEGER_WIDTH_SHIFT) | INTEGER_SIGNED_FLAG;
            record
        };
        let unsigned = |width: u32| {
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
            record.payload0 = SHAPE_INTEGER;
            record.payload1 = width << INTEGER_WIDTH_SHIFT;
            record
        };
        match row.name {
            b"bool" | b"untyped bool" => {
                SemanticTypeRecord::leaf(SemanticTypeTag::Primitive).with_shape(SHAPE_BOOL)
            }
            b"string" | b"untyped string" => {
                SemanticTypeRecord::leaf(SemanticTypeTag::Primitive).with_shape(SHAPE_STR)
            }
            b"untyped int" | b"untyped rune" => {
                SemanticTypeRecord::leaf(SemanticTypeTag::Primitive)
                    .with_shape(SHAPE_ARBITRARY_INTEGER)
            }
            b"int" => SemanticTypeRecord::leaf(SemanticTypeTag::Primitive)
                .with_shape(SHAPE_NATIVE_SIGNED_INTEGER),
            b"uint" => SemanticTypeRecord::leaf(SemanticTypeTag::Primitive)
                .with_shape(SHAPE_NATIVE_UNSIGNED_INTEGER),
            b"uintptr" => SemanticTypeRecord::leaf(SemanticTypeTag::Primitive)
                .with_shape(SHAPE_POINTER_ADDRESS_INTEGER),
            b"int8" => signed(8),
            b"int16" => signed(16),
            b"int32" | b"rune" => signed(32),
            b"int64" => signed(64),
            b"uint8" | b"byte" => unsigned(8),
            b"uint16" => unsigned(16),
            b"uint32" => unsigned(32),
            b"uint64" => unsigned(64),
            b"float32" => {
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
                record.payload0 = SHAPE_FLOAT;
                record.payload1 = TypeWidth::Fixed(32).to_cell();
                record
            }
            b"float64" => {
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
                record.payload0 = SHAPE_FLOAT;
                record.payload1 = TypeWidth::Fixed(64).to_cell();
                record
            }
            b"complex64" | b"complex128" => {
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
                record.payload0 = SHAPE_BUILTIN;
                record.text = Some(row.name);
                record
            }
            _ => unknown_record(TypeReason::NoIrRepresentation, Some(row.name)),
        }
    }

    /// The pooled child-run row indices of one type row, image order.
    fn row_children(
        &self,
        row: &backend_frontend_go::legacy::TypeRow<'source>,
    ) -> Result<Vec<u32>, GoCollectError> {
        let start = index_of(row.children.0);
        let count = row.children.1 as usize;
        let mut children = Vec::with_capacity(count);
        for offset in 0..count {
            let (target, _) = self
                .image
                .type_child(start + offset)
                .map_err(GoCollectError::Image)?;
            children.push(target);
        }
        Ok(children)
    }

    /// Interns one pooled entity list under its bounded width.
    fn entity_list(
        &mut self,
        ordinals: &[u32],
        owner: u32,
        phase: GoProjectionListPhase,
    ) -> Result<EntityListId, GoCollectError> {
        if ordinals.len() > MAX_REF_LIST_ELEMENTS {
            return Err(terminal(ProjectionFault::ListCapacity { owner, phase }));
        }
        self.facts
            .intern_entity_list(ordinals)
            .map_err(|fault| lane_terminal_ordinal(owner, 0, fault))
    }
}
