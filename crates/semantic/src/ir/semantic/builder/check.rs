//! Read-only admission of one semantic image.
//!
//! Interning only appends rows. Walking those rows for dangling identifiers,
//! illegal type shapes, and authority planes that disagree with owned facts
//! stays here, so the builder's mutation API does not own the admission rules.

use super::super::error::{BuildError, SemanticSpace};
use super::super::ids::{
    AtomListId, EntityListId, FreePredicateListId, ItemKind, LinkId, LinkOccurrenceId, TypeListId,
    TypeParameterListId,
};
use super::super::language_facts::SemanticImageAuthority;
use super::super::packed_types::{
    ArrayShape, CallableElementRole, ComputedType, ConcreteType, LiteralType, ObjectMember,
    PropertyKey, QualifiedSegments, TemplatePart, TupleElementKind, TypeParameterBound,
    TypeParameterKind, TypeQuery, VariadicForm, WildcardBound,
};
use super::super::relations::{
    DocFragment, EntityVersion, ExternalTarget, ForeignTargetOrigin, LinkTarget,
};
use super::super::type_model::{TypeExpr, Visibility};
use super::IrBuilder;
use crate::ir::{
    AtomId, AuthorityFactFault, AuthorityFactPlane, DeclarationIdentity, DenseId,
    EntityAuthorityFacts, EntityId, FactAvailability, ImageProvenance, ListId, ListInterner,
    ParentageAuthority, TextId, TypeId,
};
use crate::vocabulary::CompileRecipeFact;
use core::hash::Hash;

pub(in crate::ir::semantic) fn validate_type_parameters(
    builder: &IrBuilder,
    parameters: TypeParameterListId,
) -> Result<(), BuildError> {
    for (position, parameter) in list_or_dangling(
        &builder.type_parameters,
        parameters,
        SemanticSpace::TypeParameters,
    )?
    .iter()
    .enumerate()
    {
        atom(builder, parameter.name)?;
        for bound in list_or_dangling(
            &builder.type_parameter_bounds,
            parameter.bounds,
            SemanticSpace::TypeParameterBounds,
        )? {
            match bound {
                TypeParameterBound::Type(ty) => {
                    id(*ty, builder.types.len(), SemanticSpace::Type)?;
                }
                TypeParameterBound::Lifetime(name) => atom(builder, *name)?,
            }
        }
        if let TypeParameterKind::ConstValue { value_type } = parameter.kind {
            id(value_type, builder.types.len(), SemanticSpace::Type)?;
        }
        if !parameter.requirements.is_valid() {
            return Err(BuildError::TypeParameterRequirements {
                list: parameters.raw,
                position: u32::try_from(position).unwrap_or(u32::MAX),
                requirements: parameter.requirements,
            });
        }
        optional_id(parameter.default, builder.types.len(), SemanticSpace::Type)?;
    }
    Ok(())
}

pub(in crate::ir::semantic) fn validate_free_predicates(
    builder: &IrBuilder,
    predicates: FreePredicateListId,
) -> Result<(), BuildError> {
    for predicate in list_or_dangling(
        &builder.free_predicates,
        predicates,
        SemanticSpace::FreePredicates,
    )? {
        id(predicate.subject, builder.types.len(), SemanticSpace::Type)?;
        for bound in list_or_dangling(
            &builder.type_parameter_bounds,
            predicate.bounds,
            SemanticSpace::TypeParameterBounds,
        )? {
            match bound {
                TypeParameterBound::Type(ty) => {
                    id(*ty, builder.types.len(), SemanticSpace::Type)?;
                }
                TypeParameterBound::Lifetime(name) => atom(builder, *name)?,
            }
        }
    }
    Ok(())
}

pub(in crate::ir::semantic) fn validate_atom_list(
    builder: &IrBuilder,
    atoms: AtomListId,
) -> Result<(), BuildError> {
    for atom_id in list_or_dangling(&builder.atom_lists, atoms, SemanticSpace::AtomList)? {
        atom(builder, *atom_id)?;
    }
    Ok(())
}

pub(in crate::ir::semantic) fn validate_entity_list(
    builder: &IrBuilder,
    entities: EntityListId,
) -> Result<(), BuildError> {
    for entity_id in list_or_dangling(&builder.entity_lists, entities, SemanticSpace::EntityList)? {
        id(*entity_id, builder.items.len(), SemanticSpace::Entity)?;
    }
    Ok(())
}

/// Rejects dangling identifiers, illegal types, and authority planes that
/// disagree with the rows this builder owns.
pub(super) fn validate(builder: &IrBuilder) -> Result<(), BuildError> {
    if !builder.authority_facts.aligned(builder.items.len()) {
        return Err(BuildError::AuthorityRowCount {
            entities: builder.items.len(),
            authority_rows: builder.authority_facts.len(),
        });
    }
    if builder.occurrence_authority.len() != builder.link_occurrences.len() {
        return Err(BuildError::OccurrenceAuthorityRowCount {
            occurrences: builder.link_occurrences.len(),
            authority_rows: builder.occurrence_authority.len(),
        });
    }
    if let ImageProvenance::Captured {
        source,
        recipe,
        scope,
        ..
    } = builder.provenance
    {
        let expected = CompileRecipeFact::derive(
            recipe.profile,
            recipe.stage,
            recipe.tool,
            source.identity,
            recipe.toolchain,
        );
        if expected != recipe {
            return Err(BuildError::ImageProvenanceRecipe { source, recipe });
        }
        atom(builder, scope.ecosystem)?;
        atom(builder, scope.package)?;
        atom(builder, scope.path)?;
        if builder.authority != SemanticImageAuthority::Language(recipe.profile) {
            return Err(BuildError::LanguageProfileRebind {
                existing: builder.authority,
                requested: recipe.profile,
            });
        }
    }
    for raw in 0..builder.types.len() {
        let Some(ty) = builder.types.get(TypeId::new(raw as u32)) else {
            return Err(BuildError::Dangling {
                space: SemanticSpace::Type,
                raw: raw as u32,
            });
        };
        validate_type(builder, ty)?;
    }
    for target in builder.externals.as_slice() {
        match target {
            ExternalTarget::Stable { target } => {
                // A resolved endpoint is entirely typed identity; it has
                // no invented foreign spelling to validate.
                let _ = target;
            }
            ExternalTarget::Foreign(target) => {
                atom(builder, target.path)?;
                atom(builder, target.display)?;
                match target.origin {
                    ForeignTargetOrigin::Package { ecosystem, package } => {
                        atom(builder, ecosystem)?;
                        atom(builder, package)?;
                    }
                    ForeignTargetOrigin::Namespace {
                        ecosystem,
                        namespace,
                    } => {
                        atom(builder, ecosystem)?;
                        atom(builder, namespace)?;
                    }
                    ForeignTargetOrigin::Universe { ecosystem }
                    | ForeignTargetOrigin::Unspecified { ecosystem } => atom(builder, ecosystem)?,
                }
            }
            ExternalTarget::FragmentEntity { display, .. } => atom(builder, *display)?,
        }
    }
    for index in 0..builder.items.len() {
        let entity = EntityId::new(index as u32);
        atom(builder, builder.items.names[index])?;
        optional_id(
            builder.items.parents[index].get(),
            builder.items.len(),
            SemanticSpace::Entity,
        )?;
        optional_id(
            builder.items.semantic_types[index].get(),
            builder.types.len(),
            SemanticSpace::Type,
        )?;
        if let Some(source) = builder.sources.get(index) {
            atom(builder, source.file())?;
        }
        builder.extensions.validate_entity(builder, entity)?;
        let members = list_or_dangling(
            &builder.entity_lists,
            builder.items.members[index],
            SemanticSpace::EntityList,
        )?;
        for member in members {
            id(*member, builder.items.len(), SemanticSpace::Entity)?;
        }
        let attributes = list_or_dangling(
            &builder.atom_lists,
            builder.items.attributes[index],
            SemanticSpace::AtomList,
        )?;
        for attribute in attributes {
            atom(builder, *attribute)?;
        }
        let docs = list_or_dangling(
            &builder.docs,
            builder.items.docs[index],
            SemanticSpace::Docs,
        )?;
        for fragment in docs {
            validate_doc(builder, *fragment)?;
        }
        let facts = builder
            .authority_facts
            .facts(index)
            .ok_or(BuildError::AuthorityRowCount {
                entities: builder.items.len(),
                authority_rows: builder.authority_facts.len(),
            })?;
        let local_parent = builder.items.parents[index]
            .get()
            .and_then(|parent| builder.items.versions.get(parent.index()).copied())
            .map(EntityVersion::identity);
        validate_entity_authority(
            entity,
            facts,
            local_parent,
            builder.sources.get(index).is_some(),
            !members.is_empty(),
            builder.items.semantic_types[index].get().is_some(),
            !docs.is_empty(),
            builder.items.visibility[index] != Visibility::Unknown,
            !attributes.is_empty(),
            builder.extensions.has_entity(entity),
        )?;
    }
    for raw in 0..builder.links.len() {
        let Some(link) = builder.links.get(LinkId::new(raw as u32)) else {
            return Err(BuildError::Dangling {
                space: SemanticSpace::Entity,
                raw: raw as u32,
            });
        };
        id(link.from, builder.items.len(), SemanticSpace::Entity)?;
        validate_target(builder, link.target)?;
        if let Some(source) = link.source {
            atom(builder, source.file())?;
        }
    }
    for raw in 0..builder.link_occurrences.len() {
        let Some(occurrence) = builder
            .link_occurrences
            .get(LinkOccurrenceId::new(raw as u32))
        else {
            return Err(BuildError::Dangling {
                space: SemanticSpace::LinkOccurrence,
                raw: raw as u32,
            });
        };
        id(occurrence.link, builder.links.len(), SemanticSpace::Link)?;
        if let Some(source) = occurrence.source {
            atom(builder, source.file())?;
        }
        let authority = builder.occurrence_authority.get(raw).ok_or(
            BuildError::OccurrenceAuthorityRowCount {
                occurrences: builder.link_occurrences.len(),
                authority_rows: builder.occurrence_authority.len(),
            },
        )?;
        let present = occurrence.source.is_some();
        if matches!(authority.source, FactAvailability::Captured) != present {
            return Err(BuildError::OccurrenceAuthorityFacts {
                occurrence: LinkOccurrenceId::new(raw as u32),
                cause: AuthorityFactFault::Availability {
                    plane: AuthorityFactPlane::OccurrenceSource,
                    claimed: authority.source,
                    present,
                },
            });
        }
    }
    Ok(())
}
/// Validates one cold authority row against the corresponding owned entity
/// facts. List capture is intentionally not inferred from list cardinality:
/// captured-empty and unavailable-empty are distinct authority observations.
fn validate_entity_authority(
    entity: EntityId,
    facts: EntityAuthorityFacts,
    local_parent: Option<DeclarationIdentity>,
    has_source: bool,
    has_members: bool,
    has_semantic_type: bool,
    has_documentation: bool,
    has_visibility: bool,
    has_attributes: bool,
    has_extension: bool,
) -> Result<(), BuildError> {
    let parentage_matches = match facts.parentage {
        ParentageAuthority::Root => local_parent.is_none(),
        ParentageAuthority::Bound(parent) => local_parent == Some(parent),
        ParentageAuthority::UnrepresentedAuthorityOwner(_) | ParentageAuthority::Unavailable => {
            local_parent.is_none()
        }
    };
    if !parentage_matches {
        return Err(BuildError::AuthorityFacts {
            entity,
            cause: AuthorityFactFault::Parentage {
                claimed: facts.parentage,
                local_parent,
            },
        });
    }
    validate_authority_availability(entity, AuthorityFactPlane::Source, facts.source, has_source)?;
    if facts.source != facts.source_file {
        return Err(BuildError::AuthorityFacts {
            entity,
            cause: AuthorityFactFault::SourceFileWithoutSource {
                source: facts.source,
                source_file: facts.source_file,
            },
        });
    }
    validate_authority_availability(
        entity,
        AuthorityFactPlane::SourceFile,
        facts.source_file,
        has_source,
    )?;
    validate_authority_availability(
        entity,
        AuthorityFactPlane::SemanticType,
        facts.semantic_type,
        has_semantic_type,
    )?;
    validate_list_authority_availability(
        entity,
        AuthorityFactPlane::Members,
        facts.members,
        has_members,
    )?;
    // Documentation and attributes can be Captured with empty lists. A
    // nonempty value, however, cannot claim an unavailable authority plane.
    validate_list_authority_availability(
        entity,
        AuthorityFactPlane::Documentation,
        facts.documentation,
        has_documentation,
    )?;
    validate_list_authority_availability(
        entity,
        AuthorityFactPlane::Visibility,
        facts.visibility,
        has_visibility,
    )?;
    validate_list_authority_availability(
        entity,
        AuthorityFactPlane::Attributes,
        facts.attributes,
        has_attributes,
    )?;
    validate_list_authority_availability(
        entity,
        AuthorityFactPlane::LanguageExtension,
        facts.language_extension,
        has_extension,
    )
}

fn validate_authority_availability(
    entity: EntityId,
    plane: AuthorityFactPlane,
    claimed: FactAvailability,
    present: bool,
) -> Result<(), BuildError> {
    let matches = matches!(claimed, FactAvailability::Captured) == present;
    if matches {
        return Ok(());
    }
    Err(BuildError::AuthorityFacts {
        entity,
        cause: AuthorityFactFault::Availability {
            plane,
            claimed,
            present,
        },
    })
}

fn validate_list_authority_availability(
    entity: EntityId,
    plane: AuthorityFactPlane,
    claimed: FactAvailability,
    present: bool,
) -> Result<(), BuildError> {
    if !present || claimed == FactAvailability::Captured {
        return Ok(());
    }
    Err(BuildError::AuthorityFacts {
        entity,
        cause: AuthorityFactFault::Availability {
            plane,
            claimed,
            present,
        },
    })
}

fn validate_type(builder: &IrBuilder, ty: TypeExpr) -> Result<(), BuildError> {
    match ty {
        TypeExpr::Concrete(concrete) => validate_concrete_type(builder, concrete),
        TypeExpr::Computed(computed) => validate_computed_type(builder, computed),
        TypeExpr::Unknown(unknown) => unknown
            .spelling
            .map(|spelling| atom(builder, spelling))
            .transpose()
            .map(|_| ()),
    }
}

fn validate_concrete_type(builder: &IrBuilder, ty: ConcreteType) -> Result<(), BuildError> {
    match ty {
        ConcreteType::Builtin(_) => Ok(()),
        ConcreteType::Literal(literal) => match literal {
            LiteralType::String(value)
            | LiteralType::Number(value)
            | LiteralType::BigInt(value) => atom(builder, value),
            LiteralType::Boolean(_) | LiteralType::Null | LiteralType::Undefined => Ok(()),
        },
        ConcreteType::Nominal(entity) => id(entity, builder.items.len(), SemanticSpace::Entity),
        ConcreteType::External(external) => id(
            external,
            builder.externals.as_slice().len(),
            SemanticSpace::External,
        ),
        ConcreteType::Parameter(atom_id) => atom(builder, atom_id),
        ConcreteType::Applied {
            constructor,
            arguments,
        } => {
            id(constructor, builder.types.len(), SemanticSpace::Type)?;
            validate_type_list(builder, arguments)
        }
        ConcreteType::Tuple(list) => {
            for element in
                list_or_dangling(&builder.tuple_elements, list, SemanticSpace::TupleElements)?
            {
                if let Some(label) = element.label {
                    atom(builder, label)?;
                }
                id(element.ty, builder.types.len(), SemanticSpace::Type)?;
            }
            Ok(())
        }
        ConcreteType::Object(list) => {
            for member in
                list_or_dangling(&builder.object_members, list, SemanticSpace::ObjectMembers)?
            {
                validate_object_member(builder, *member)?;
            }
            Ok(())
        }
        ConcreteType::Union(list)
        | ConcreteType::Intersection(list)
        | ConcreteType::ImplTrait(list)
        | ConcreteType::DynTrait(list) => validate_type_list(builder, list),
        ConcreteType::Function {
            parameters,
            results,
            abi,
            variadic,
            ..
        } => {
            let parameters = list_or_dangling(
                &builder.tuple_elements,
                parameters,
                SemanticSpace::TupleElements,
            )?;
            let mut typed_rest = false;
            for (position, parameter) in parameters.iter().copied().enumerate() {
                if let Some(label) = parameter.label {
                    atom(builder, label)?;
                }
                id(parameter.ty, builder.types.len(), SemanticSpace::Type)?;
                if parameter.kind == TupleElementKind::Rest {
                    let is_final = position + 1 == parameters.len();
                    if variadic != VariadicForm::TypedLast || !is_final {
                        return Err(BuildError::CallableElement {
                            role: CallableElementRole::Parameter,
                            position,
                            kind: parameter.kind,
                        });
                    }
                    typed_rest = true;
                }
            }
            if variadic == VariadicForm::TypedLast && !typed_rest {
                return Err(BuildError::MissingTypedVariadicParameter {
                    parameter_count: parameters.len(),
                });
            }
            for (position, result) in list_or_dangling(
                &builder.tuple_elements,
                results,
                SemanticSpace::TupleElements,
            )?
            .iter()
            .copied()
            .enumerate()
            {
                if let Some(label) = result.label {
                    atom(builder, label)?;
                }
                id(result.ty, builder.types.len(), SemanticSpace::Type)?;
                if result.kind != TupleElementKind::Required {
                    return Err(BuildError::CallableElement {
                        role: CallableElementRole::Result,
                        position,
                        kind: result.kind,
                    });
                }
            }
            if let Some(abi) = abi {
                atom(builder, abi)?;
            }
            Ok(())
        }
        ConcreteType::Reference {
            target, lifetime, ..
        } => {
            id(target, builder.types.len(), SemanticSpace::Type)?;
            if let Some(lifetime) = lifetime {
                atom(builder, lifetime)?;
            }
            Ok(())
        }
        ConcreteType::CxxReference { target, .. }
        | ConcreteType::CPointer { target }
        | ConcreteType::CBlockPointer { target } => {
            id(target, builder.types.len(), SemanticSpace::Type)
        }
        ConcreteType::NativeCharacter { .. } => Ok(()),
        ConcreteType::CxxMemberPointer { owner, member } => {
            id(owner, builder.types.len(), SemanticSpace::Type)?;
            id(member, builder.types.len(), SemanticSpace::Type)?;
            if !is_cxx_record_owner(builder, owner) {
                return Err(BuildError::IllegalCxxMemberPointerOwner { owner });
            }
            Ok(())
        }
        ConcreteType::CQualified { target, qualifiers } => {
            if qualifiers.is_empty() {
                return Err(BuildError::EmptyCxxQualification);
            }
            id(target, builder.types.len(), SemanticSpace::Type)?;
            let target_type = builder.types.get(target).ok_or(BuildError::Dangling {
                space: SemanticSpace::Type,
                raw: target.raw,
            })?;
            let legal = match target_type.concrete() {
                Some(ConcreteType::CPointer { .. }) => true,
                Some(ConcreteType::Function { .. }) | Some(ConcreteType::CxxReference { .. }) => {
                    !qualifiers.const_ && !qualifiers.volatile && !qualifiers.restrict
                }
                _ => !qualifiers.restrict,
            };
            if !legal {
                return Err(BuildError::IllegalCQualifierTarget { target, qualifiers });
            }
            Ok(())
        }
        ConcreteType::Pointer { target, .. }
        | ConcreteType::Slice(target)
        | ConcreteType::Optional(target) => id(target, builder.types.len(), SemanticSpace::Type),
        ConcreteType::Array { element, shape } => {
            id(element, builder.types.len(), SemanticSpace::Type)?;
            if let ArrayShape::ConstExpression(expression) = shape {
                atom(builder, expression)?;
            }
            Ok(())
        }
        ConcreteType::Wildcard(WildcardBound::Unbounded) => Ok(()),
        ConcreteType::Wildcard(WildcardBound::Extends(bound))
        | ConcreteType::Wildcard(WildcardBound::Super(bound)) => {
            id(bound, builder.types.len(), SemanticSpace::Type)
        }
        ConcreteType::Annotated { target, .. } => {
            id(target, builder.types.len(), SemanticSpace::Type)
        }
        ConcreteType::Inferred(spelling) => spelling
            .map(|spelling| atom(builder, spelling))
            .transpose()
            .map(|_| ()),
        ConcreteType::QualifiedPath {
            self_type,
            trait_type,
            segments,
            spelling,
        } => {
            id(self_type, builder.types.len(), SemanticSpace::Type)?;
            optional_id(trait_type, builder.types.len(), SemanticSpace::Type)?;
            if let QualifiedSegments::Captured(segments) = segments {
                let segments =
                    list_or_dangling(&builder.atom_lists, segments, SemanticSpace::AtomList)?;
                if segments.is_empty() {
                    return Err(BuildError::EmptyQualifiedPath);
                }
                for segment in segments {
                    atom(builder, *segment)?;
                }
            }
            atom(builder, spelling)
        }
        ConcreteType::Map { key, value } => {
            id(key, builder.types.len(), SemanticSpace::Type)?;
            id(value, builder.types.len(), SemanticSpace::Type)
        }
        ConcreteType::Channel { element, .. } => {
            id(element, builder.types.len(), SemanticSpace::Type)
        }
    }
}

/// A C++ member-pointer owner is a class/record nominal or one generic
/// application whose constructor is such a nominal. This deliberately does
/// not accept a source spelling, enum, pointer, or unrelated type variable.
fn is_cxx_record_owner(builder: &IrBuilder, owner: TypeId) -> bool {
    let Some(TypeExpr::Concrete(owner)) = builder.types.get(owner) else {
        return false;
    };
    let nominal = match owner {
        ConcreteType::Nominal(entity) => Some(entity),
        ConcreteType::Applied { constructor, .. } => match builder.types.get(constructor) {
            Some(TypeExpr::Concrete(ConcreteType::Nominal(entity))) => Some(entity),
            _ => None,
        },
        _ => None,
    };
    nominal.is_some_and(|entity| {
        builder
            .items
            .kinds
            .get(entity.index())
            .is_some_and(|kind| *kind == ItemKind::Record)
    })
}

fn validate_computed_type(builder: &IrBuilder, ty: ComputedType) -> Result<(), BuildError> {
    let type_len = builder.types.len();
    match ty {
        ComputedType::KeyOf(ty) | ComputedType::Awaited(ty) => {
            id(ty, type_len, SemanticSpace::Type)
        }
        ComputedType::TypeOf(query) => match query {
            TypeQuery::Entity(entity) => id(entity, builder.items.len(), SemanticSpace::Entity),
            TypeQuery::Path(path) => {
                for component in
                    list_or_dangling(&builder.atom_lists, path, SemanticSpace::AtomList)?
                {
                    atom(builder, *component)?;
                }
                Ok(())
            }
            TypeQuery::External(external) => id(
                external,
                builder.externals.as_slice().len(),
                SemanticSpace::External,
            ),
        },
        ComputedType::IndexedAccess { object, index } => {
            id(object, type_len, SemanticSpace::Type)?;
            id(index, type_len, SemanticSpace::Type)
        }
        ComputedType::Conditional {
            check,
            extends,
            then_type,
            else_type,
            ..
        } => {
            for ty in [check, extends, then_type, else_type] {
                id(ty, type_len, SemanticSpace::Type)?;
            }
            Ok(())
        }
        ComputedType::Mapped {
            parameter,
            constraint,
            name_as,
            value,
            ..
        } => {
            atom(builder, parameter)?;
            id(constraint, type_len, SemanticSpace::Type)?;
            optional_id(name_as, type_len, SemanticSpace::Type)?;
            id(value, type_len, SemanticSpace::Type)
        }
        ComputedType::Infer {
            parameter,
            constraint,
        } => {
            atom(builder, parameter)?;
            optional_id(constraint, type_len, SemanticSpace::Type)
        }
        ComputedType::TemplateLiteral(parts) => {
            for part in
                list_or_dangling(&builder.template_parts, parts, SemanticSpace::TemplateParts)?
            {
                match *part {
                    TemplatePart::Bytes(bytes) => atom(builder, bytes)?,
                    TemplatePart::Placeholder(ty) => id(ty, type_len, SemanticSpace::Type)?,
                }
            }
            Ok(())
        }
        ComputedType::Import {
            specifier,
            qualifier,
            arguments,
        } => {
            atom(builder, specifier)?;
            for component in
                list_or_dangling(&builder.atom_lists, qualifier, SemanticSpace::AtomList)?
            {
                atom(builder, *component)?;
            }
            validate_type_list(builder, arguments)
        }
        ComputedType::This => Ok(()),
    }
}

fn validate_object_member(builder: &IrBuilder, member: ObjectMember) -> Result<(), BuildError> {
    let type_len = builder.types.len();
    match member {
        ObjectMember::Property { key, ty, .. } => {
            validate_property_key(builder, key)?;
            id(ty, type_len, SemanticSpace::Type)
        }
        ObjectMember::Method { key, signature, .. } => {
            validate_property_key(builder, key)?;
            id(signature, type_len, SemanticSpace::Type)
        }
        ObjectMember::Index {
            parameter,
            key,
            value,
            ..
        } => {
            atom(builder, parameter)?;
            id(key, type_len, SemanticSpace::Type)?;
            id(value, type_len, SemanticSpace::Type)
        }
        ObjectMember::Call(signature) | ObjectMember::Construct(signature) => {
            id(signature, type_len, SemanticSpace::Type)
        }
    }
}

fn validate_property_key(builder: &IrBuilder, key: PropertyKey) -> Result<(), BuildError> {
    match key {
        PropertyKey::Named(atom_id)
        | PropertyKey::Private(atom_id)
        | PropertyKey::Numeric(atom_id) => atom(builder, atom_id),
        PropertyKey::Computed(ty) => id(ty, builder.types.len(), SemanticSpace::Type),
    }
}

pub(in crate::ir::semantic) fn validate_type_list(
    builder: &IrBuilder,
    list: TypeListId,
) -> Result<(), BuildError> {
    for ty in list_or_dangling(&builder.type_lists, list, SemanticSpace::TypeList)? {
        id(*ty, builder.types.len(), SemanticSpace::Type)?;
    }
    Ok(())
}

fn validate_doc(builder: &IrBuilder, doc: DocFragment) -> Result<(), BuildError> {
    match doc {
        DocFragment::Text(text) | DocFragment::Code(text) => text_id(builder, text),
        DocFragment::Link { label, target } => {
            text_id(builder, label)?;
            validate_target(builder, target)
        }
        DocFragment::SoftBreak | DocFragment::HardBreak => Ok(()),
    }
}

fn validate_target(builder: &IrBuilder, target: LinkTarget) -> Result<(), BuildError> {
    match target {
        LinkTarget::Local(entity) => id(entity, builder.items.len(), SemanticSpace::Entity),
        LinkTarget::External(external) => id(
            external,
            builder.externals.as_slice().len(),
            SemanticSpace::External,
        ),
    }
}

pub(in crate::ir::semantic) fn atom(builder: &IrBuilder, atom: AtomId) -> Result<(), BuildError> {
    builder
        .atoms
        .get(atom)
        .map(|_| ())
        .ok_or(BuildError::Dangling {
            space: SemanticSpace::Atom,
            raw: atom.raw,
        })
}

fn text_id(builder: &IrBuilder, text: TextId) -> Result<(), BuildError> {
    builder
        .atoms
        .text(text)
        .map(|_| ())
        .ok_or(BuildError::Dangling {
            space: SemanticSpace::Text,
            raw: text.raw,
        })
}

fn id<T: Copy>(id: DenseId<T>, len: usize, space: SemanticSpace) -> Result<(), BuildError> {
    (id.index() < len)
        .then_some(())
        .ok_or(BuildError::Dangling { space, raw: id.raw })
}

pub(in crate::ir::semantic) fn optional_id<T: Copy>(
    id_: Option<DenseId<T>>,
    len: usize,
    space: SemanticSpace,
) -> Result<(), BuildError> {
    id_.map_or(Ok(()), |id_| id(id_, len, space))
}

fn list_or_dangling<T: Copy + Eq + Hash>(
    lists: &ListInterner<T>,
    id: ListId<T>,
    space: SemanticSpace,
) -> Result<&[T], BuildError> {
    lists
        .get(id)
        .ok_or(BuildError::Dangling { space, raw: id.raw })
}
