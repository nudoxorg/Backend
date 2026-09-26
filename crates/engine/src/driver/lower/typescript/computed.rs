//! Checker-computed TypeScript type trees.
//!
//! Declaration facts are already pushed. This lane interns the checker's
//! observed type as computed rows, children first, so the pool stays
//! topologically backward. Member spellings are borrowed from the source
//! span that owns them.

use super::{
    FactRegistry, MAX_TYPE_DEPTH, SpellDomain, TypeScriptCollectError, UNSET, computed_fault,
    fault, find_sub, lane_rejection, unknown_record,
};
use crate::driver::lower::{FactSet, MAX_TYPE_CHILDREN};
use crate::driver::types::FactFault;
use backend_frontend_typescript::legacy::{
    MappedModifier as CheckerMappedModifier, TemplatePart, TypeTree,
};
use backend_semantic::ir::{
    AnonRecordForm, EntityId, ExternalEntityRef, ExternalFragmentId, LatticeMappedModifier,
    NominalRef, PrimitiveShape, SemanticTypeChild, SemanticTypeRecord, SemanticTypeTag, TypeReason,
    TypeWidth,
};

impl<'a, 'source> FactRegistry<'a, 'source> {
    /// Resolves one source position to the fact whose binding name starts
    /// exactly there.
    pub(super) fn fact_at_name_start(&self, start: u32) -> Option<u32> {
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
        let source_end = u32::try_from(self.source.len()).ok()?;
        match domain {
            SpellDomain::Owner => self
                .source_spelling_owner(name, owner)
                .or_else(|| self.source_spelling_in(name, 0, source_end)),
            SpellDomain::Range(start, end) => self
                .source_spelling_in(name, start, end)
                .or_else(|| self.source_spelling_owner(name, owner))
                .or_else(|| self.source_spelling_in(name, 0, source_end)),
        }
    }
}

/// Interns one checker computed type as a computed type row owned by
/// `owner`, interning every child row first so the pooled lane stays
/// topologically backward. Returns the row's lane coordinate
/// (`COMPUTED_ROW_BASE` plus its pool ordinal).
///
/// `spell` names the source span that owns the tree's member spellings:
/// the owner's declaring span for declaration-computed rows, or the exact
/// assignment site for narrowing rows (whose shapes are spelled at the
/// site, not at the declaration).
pub(super) fn intern_computed_tree<'source>(
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
        TypeTree::This => match registry.source_spelling(spell, b"this", owner) {
            Some(spelling) => {
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::SelfType);
                record.text = Some(spelling);
                intern_computed_leaf(facts, record, owner)
            }
            None => intern_computed_leaf(facts, unknown_record(TypeReason::OracleGap), owner),
        },
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
        TypeTree::Union { members } => intern_computed_associative(
            registry,
            facts,
            members,
            owner,
            depth,
            spell,
            SemanticTypeTag::Union,
        ),
        TypeTree::Intersection { members } => intern_computed_associative(
            registry,
            facts,
            members,
            owner,
            depth,
            spell,
            SemanticTypeTag::Intersection,
        ),
        TypeTree::Conditional {
            check,
            extends,
            then_type,
            else_type,
        } => {
            let children = [
                check.as_ref().clone(),
                extends.as_ref().clone(),
                then_type.as_ref().clone(),
                else_type.as_ref().clone(),
            ];
            let children =
                intern_computed_children(registry, facts, &children, owner, depth, spell)?;
            intern_computed_row(
                registry,
                facts,
                SemanticTypeRecord::leaf(SemanticTypeTag::Conditional),
                owner,
                &children[..4],
            )
        }
        TypeTree::Mapped {
            parameter,
            constraint,
            name_as,
            value,
            readonly,
            optional,
        } => {
            let constraint =
                intern_computed_tree(registry, facts, constraint, owner, depth, spell)?;
            let name_as = name_as
                .as_deref()
                .map(|name_as| intern_computed_tree(registry, facts, name_as, owner, depth, spell))
                .transpose()?;
            let value = intern_computed_tree(registry, facts, value, owner, depth, spell)?;
            let mut children = [constraint, value, 0];
            let len = match name_as {
                Some(name_as) => {
                    children = [constraint, name_as, value];
                    3
                }
                None => 2,
            };
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Mapped);
            record.text = registry.source_spelling(spell, parameter.as_bytes(), owner);
            record.payload0 = checker_mapped_modifier(*readonly);
            record.payload1 = checker_mapped_modifier(*optional);
            intern_computed_row(registry, facts, record, owner, &children[..len])
        }
        TypeTree::TemplateLiteral { parts } => {
            // The checker report owns strings, while staging deliberately
            // borrows only the entered source lease. Resolve every literal
            // segment before appending anything so an absent source spelling
            // becomes one truthful OracleGap row rather than a half-built
            // computed-child transaction.
            let mut text_parts = [None; MAX_TYPE_CHILDREN];
            if parts.len() > MAX_TYPE_CHILDREN {
                return Err(computed_fault(
                    registry,
                    owner,
                    FactFault::TypeChildCapacity,
                ));
            }
            for (position, part) in parts.iter().enumerate() {
                if let TemplatePart::Text { text } = part {
                    let Some(source_text) = registry.source_spelling(spell, text.as_bytes(), owner)
                    else {
                        return intern_computed_leaf(
                            facts,
                            unknown_record(TypeReason::OracleGap),
                            owner,
                        );
                    };
                    text_parts[position] = Some(source_text);
                }
            }
            let record = SemanticTypeRecord::leaf(SemanticTypeTag::TemplateLiteral);
            for (position, part) in parts.iter().enumerate() {
                match part {
                    TemplatePart::Text { .. } => {
                        let text = text_parts[position].ok_or_else(|| {
                            computed_fault(registry, owner, FactFault::TypeChildCapacity)
                        })?;
                        facts
                            .computed_type_text_child(text)
                            .map_err(|cause| computed_fault(registry, owner, cause))?;
                    }
                    TemplatePart::Type { r#type } => {
                        let child = intern_computed_tree(
                            registry,
                            facts,
                            r#type,
                            owner,
                            depth.saturating_add(1),
                            spell,
                        )?;
                        facts
                            .computed_type_child(child, None, 0)
                            .map_err(|cause| computed_fault(registry, owner, cause))?;
                    }
                }
            }
            facts
                .intern_computed_type_row(owner, record)
                .map_err(|cause| computed_fault(registry, owner, cause))
        }
        TypeTree::Tuple { elements } => {
            let children =
                intern_computed_children(registry, facts, elements, owner, depth, spell)?;
            intern_computed_row(
                registry,
                facts,
                SemanticTypeRecord::leaf(SemanticTypeTag::Tuple),
                owner,
                &children[..elements.len()],
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
            let record = SemanticTypeRecord::leaf(SemanticTypeTag::ArraySequence);
            intern_computed_row(registry, facts, record, owner, &children[..1])
        }
        TypeTree::Function { parameters, result } => {
            let mut children = [0_u32; MAX_TYPE_CHILDREN];
            let mut len = 0_usize;
            if parameters.len() >= MAX_TYPE_CHILDREN {
                return Err(computed_fault(
                    registry,
                    owner,
                    FactFault::TypeChildCapacity,
                ));
            }
            for parameter in parameters {
                let slot = children
                    .get_mut(len)
                    .ok_or_else(|| computed_fault(registry, owner, FactFault::TypeChildCapacity))?;
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
            let slot = children
                .get_mut(len)
                .ok_or_else(|| computed_fault(registry, owner, FactFault::TypeChildCapacity))?;
            *slot = result_row;
            len += 1;
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::FunctionPointer);
            record.payload1 = SemanticTypeRecord::FUNCTION_RESULT_COUNT_ONE;
            intern_computed_row(registry, facts, record, owner, &children[..len])
        }
        TypeTree::Object { members } => {
            // Members carry names and flags, so each member's row is
            // interned first and then linked with its spelling-domain
            // source spelling.
            let mut rows: Vec<(u32, Option<&'source [u8]>, u8)> = Vec::with_capacity(members.len());
            for (position, member) in members.iter().enumerate() {
                let row = intern_computed_tree(
                    registry,
                    facts,
                    &member.member_type,
                    owner,
                    depth.saturating_add(1),
                    spell,
                )?;
                let mut flags = 0_u8;
                if member.optional {
                    flags |= SemanticTypeChild::FLAG_OPTIONAL;
                }
                if member.readonly {
                    flags |= SemanticTypeChild::FLAG_READONLY;
                }
                // A checker-synthesized member whose name carries its internal
                // `__@` marker (`__@UNDEFINED_VOID_ONLY@9`, `__@iterator`, ...)
                // is not a source declaration. The anonymous-record grammar
                // requires a source-backed name, and fabricating one would
                // mint a member the source never wrote, so the synthetic
                // member is omitted while every spelled member stays. Any
                // other unspelled name stays the exact typed rejection.
                let Some(spelling) = registry.source_spelling(spell, member.name.as_bytes(), owner)
                else {
                    if member.name.starts_with("__@") {
                        continue;
                    }
                    return Err(computed_fault(
                        registry,
                        owner,
                        FactFault::TypeChild {
                            position,
                            fault: backend_semantic::ir::SemanticTypeFault::ChildNameRequired {
                                tag: SemanticTypeTag::AnonymousRecord,
                                position: position as u32,
                            },
                        },
                    ));
                };
                rows.push((row, Some(spelling), flags));
            }
            // A checker-synthesized namespace type (`typeof Ns` for a module
            // with more exported members than the lane holds per row) legally
            // exceeds the per-row bound. The member run therefore folds into
            // MAX_TYPE_CHILDREN-wide anonymous-record rows exactly as wide
            // unions fold: no member is lost, none nests deeper than the fold
            // requires, and every run at or under the bound stays unchanged.
            while rows.len() > MAX_TYPE_CHILDREN {
                let mut next: Vec<(u32, Option<&'source [u8]>, u8)> =
                    Vec::with_capacity(rows.len().div_ceil(MAX_TYPE_CHILDREN));
                for chunk in rows.chunks(MAX_TYPE_CHILDREN) {
                    for (target, name, flags) in chunk {
                        facts
                            .computed_type_child(*target, *name, *flags)
                            .map_err(|cause| computed_fault(registry, owner, cause))?;
                    }
                    let mut chunk_record =
                        SemanticTypeRecord::leaf(SemanticTypeTag::AnonymousRecord);
                    chunk_record.payload0 = u32::from(AnonRecordForm::Interface);
                    let folded = facts
                        .intern_computed_type_row(owner, chunk_record)
                        .map_err(|cause| computed_fault(registry, owner, cause))?;
                    // The anonymous-record child law names every member from
                    // source, so each folded row is named by the exact member
                    // that closes its chunk — the rightmost-member naming the
                    // pair fold used elsewhere in the lane.
                    let closes = chunk.last().and_then(|(_, name, _)| *name);
                    next.push((folded, closes, 0));
                }
                rows = next;
            }
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::AnonymousRecord);
            record.payload0 = u32::from(AnonRecordForm::Interface);
            for (target, name, flags) in &rows {
                facts
                    .computed_type_child(*target, *name, *flags)
                    .map_err(|cause| computed_fault(registry, owner, cause))?;
            }
            facts
                .intern_computed_type_row(owner, record)
                .map_err(|cause| computed_fault(registry, owner, cause))
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

/// Interns one computed reference. Foreign bases retain a typed external
/// nominal row, so applying one keeps the constructor rather than decaying
/// to an unknown record.
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
    let local_fact = local;
    let base = match local_fact {
        Some(fact) => fact,
        None => {
            // A checker name the declaration never spells (a synthesized
            // `__type`, a printed tuple, or a lib-internal spelling) cannot
            // back a spelling-bearing unknown row and cannot become a
            // nominal-external row, whose owned-IR conversion rejects an
            // absent text cell as a dangling atom. It stays an honest
            // oracle-gap unknown, whose reason owns no text cell. The
            // foreign occurrence link still retains the exact endpoint.
            let Some(text) = registry.source_spelling(spell, name.as_bytes(), owner) else {
                return intern_computed_leaf(facts, unknown_record(TypeReason::OracleGap), owner);
            };
            if module.is_none() {
                let mut record = unknown_record(TypeReason::UnresolvedExternal);
                record.text = Some(text);
                return intern_computed_leaf(facts, record, owner);
            }
            let module_bytes = module.map_or(name.as_bytes(), str::as_bytes);
            let fragment = ExternalFragmentId::from_canonical_bytes(module_bytes);
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Nominal);
            record.nominal = Some(NominalRef::External(ExternalEntityRef::bind(fragment, 0)));
            record.text = Some(text);
            intern_computed_leaf(facts, record, owner)?
        }
    };
    if args.is_empty() {
        if local.is_none() {
            // A foreign base already owns the honest unknown row above; it is
            // the computed root, not an entity ordinal for a nominal cell.
            return Ok(base);
        }
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
        registry,
        facts,
        SemanticTypeRecord::leaf(SemanticTypeTag::Apply),
        owner,
        &children[..len],
    )
}

/// Folds an arbitrarily wide union or intersection into binary rows. Every
/// source member remains reachable while each row obeys the fixed child lane.
fn intern_computed_associative<'source>(
    registry: &FactRegistry<'_, 'source>,
    facts: &mut FactSet<'source>,
    members: &[TypeTree],
    owner: u32,
    depth: u8,
    spell: SpellDomain,
    tag: SemanticTypeTag,
) -> Result<u32, TypeScriptCollectError> {
    if members.len() <= MAX_TYPE_CHILDREN {
        let children = intern_computed_children(registry, facts, members, owner, depth, spell)?;
        return intern_computed_row(
            registry,
            facts,
            SemanticTypeRecord::leaf(tag),
            owner,
            &children[..members.len()],
        );
    }
    // A wide union/intersection folds into MAX_TYPE_CHILDREN-wide rows rather
    // than binary pairs: the tag's child law is unbounded, so a 3000-member
    // union needs ~48 rows instead of 2999, staying inside the fixed computed
    // row lane without losing any member or nesting it deeper than necessary.
    let mut rows: Vec<u32> = Vec::with_capacity(members.len());
    for member in members {
        rows.push(intern_computed_tree(
            registry,
            facts,
            member,
            owner,
            depth.saturating_add(1),
            spell,
        )?);
    }
    while rows.len() > MAX_TYPE_CHILDREN {
        let mut next: Vec<u32> = Vec::with_capacity(rows.len().div_ceil(MAX_TYPE_CHILDREN));
        for chunk in rows.chunks(MAX_TYPE_CHILDREN) {
            next.push(intern_computed_row(
                registry,
                facts,
                SemanticTypeRecord::leaf(tag),
                owner,
                chunk,
            )?);
        }
        rows = next;
    }
    intern_computed_row(registry, facts, SemanticTypeRecord::leaf(tag), owner, &rows)
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
    registry: &FactRegistry<'_, 'source>,
    facts: &mut FactSet<'source>,
    record: SemanticTypeRecord<'source>,
    owner: u32,
    children: &[u32],
) -> Result<u32, TypeScriptCollectError> {
    for child in children {
        facts
            .computed_type_child(*child, None, 0)
            .map_err(|cause| computed_fault(registry, owner, cause))?;
    }
    facts
        .intern_computed_type_row(owner, record)
        .map_err(|cause| computed_fault(registry, owner, cause))
}

fn intern_computed_leaf<'a, 'source>(
    facts: &mut FactSet<'source>,
    record: SemanticTypeRecord<'source>,
    owner: u32,
) -> Result<u32, TypeScriptCollectError> {
    facts.intern_computed_type_row(owner, record).map_err(fault)
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
    base: backend_frontend_typescript::legacy::LiteralBase,
) -> SemanticTypeRecord<'static> {
    match base {
        backend_frontend_typescript::legacy::LiteralBase::Number => {
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
            record.payload0 = u32::from(PrimitiveShape::Integer);
            record.payload1 = (32_u32 << 1) | SemanticTypeRecord::INTEGER_SIGNED_FLAG;
            record
        }
        backend_frontend_typescript::legacy::LiteralBase::String => {
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
            record.payload0 = u32::from(PrimitiveShape::Str);
            record
        }
        backend_frontend_typescript::legacy::LiteralBase::Boolean => {
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
            record.payload0 = u32::from(PrimitiveShape::Bool);
            record
        }
        backend_frontend_typescript::legacy::LiteralBase::Bigint => {
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
            record.payload0 = u32::from(PrimitiveShape::Builtin);
            record.text = Some(&b"bigint"[..]);
            record
        }
    }
}

/// Maps the checker protocol's independently ordered modifier vocabulary
/// into the frozen semantic lattice.  Never cast its Rust discriminant: the
/// checker uses Preserve/Add/Remove while the wire uses Add/Remove/Absent.
fn checker_mapped_modifier(modifier: CheckerMappedModifier) -> u32 {
    match modifier {
        CheckerMappedModifier::Preserve => u32::from(LatticeMappedModifier::Absent),
        CheckerMappedModifier::Add => u32::from(LatticeMappedModifier::Add),
        CheckerMappedModifier::Remove => u32::from(LatticeMappedModifier::Remove),
    }
}
