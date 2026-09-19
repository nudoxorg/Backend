//! Schema-marked semantic relations and versioned transitions.

use crate::canonical::{
    canonical_attribute, canonical_component, canonical_coverage, canonical_edge, canonical_entity,
    canonical_extension_key, canonical_occurrence, canonical_provenance, canonical_source,
    canonical_type_node,
};
use crate::facets::{
    AttributeId, AttributeRecord, ComponentDescriptor, ComponentRecord, ComponentVersion,
    DocumentationRecord, EdgeId, EdgeRecord, EmbeddingInputId, EmbeddingInputRecord, ExtensionId,
    ExtensionRecord, FacetKey, FacetRecord, MemberId, MemberRecord, OccurrenceId, OccurrenceRecord,
    TypeId, TypeIdentity, TypeRecord,
};
use crate::identity::{Entity, EntityId, EntityRecord, Source, SourceId, SourceRecord};
use crate::schema::{
    encode_doc_value_schema, encode_embedding_input_value_schema, encode_entity_value_schema,
    encode_extension_value_schema, encode_facet_schema, encode_member_value_schema,
    encode_source_value_schema, encode_type_schema,
};
use crate::support::{SEMANTIC_DOMAIN, deletion_tag, object_key, object_version, witness};
use backend_version::{
    AdmittedProducerObservation, AuthorityScopeClaim, AuthorizedCompleteCoverage,
    Delta as VersionDelta, ObjectKey, ObjectVersion, Relation, RelationState,
};

/// Schema-bound source relation.
pub struct Sources;
/// Schema-bound entity relation.
pub struct Entities;
/// Schema-bound facet relation.
pub struct Facets;
/// Schema-bound occurrence relation.
pub struct Occurrences;
/// Schema-bound derived edge relation.
pub struct Edges;
/// Schema-bound type relation.
pub struct Types;
/// Schema-bound component relation.
pub struct Components;
/// Schema-bound documentation relation.
pub struct Documents;
/// Schema-bound ordered member relation.
pub struct Members;
/// Schema-bound attribute relation.
pub struct Attributes;
/// Schema-bound extension relation.
pub struct Extensions;
/// Schema-bound embedding input relation.
pub struct EmbeddingInputs;

macro_rules! relation_impl {
    ($name:ty, $ty:expr, $key:ty, $value:ty, $key_encode:path, $value_encode:path) => {
        impl Relation for $name {
            const DOMAIN: u8 = SEMANTIC_DOMAIN;
            const TYPE: u16 = $ty;
            type Key = $key;
            type Value = $value;
            fn encode_key(value: &Self::Key, out: &mut Vec<u8>) {
                $key_encode(value, out);
            }
            fn encode_value(value: &Self::Value, out: &mut Vec<u8>) {
                $value_encode(value, out);
            }
        }
    };
}

fn encode_source_id(value: &SourceId, out: &mut Vec<u8>) {
    object_key(out, value);
}
fn encode_source_record(value: &SourceRecord, out: &mut Vec<u8>) {
    canonical_source(value.identity(), out);
    match value.value() {
        Some(source) => {
            out.push(1);
            encode_source_value_schema(source, out);
        }
        None => out.push(0),
    }
    canonical_coverage(out, value.coverage());
    canonical_provenance(&value.provenance(), out);
}
fn encode_entity_id(value: &EntityId, out: &mut Vec<u8>) {
    object_key(out, value);
}
fn encode_entity_record(value: &EntityRecord, out: &mut Vec<u8>) {
    canonical_entity(value.identity(), out);
    match value.value() {
        Some(entity) => {
            out.push(1);
            encode_entity_value_schema(entity, out);
        }
        None => out.push(0),
    }
    canonical_coverage(out, value.coverage());
    canonical_provenance(&value.provenance(), out);
}
fn encode_facet_record(value: &FacetRecord, out: &mut Vec<u8>) {
    encode_facet_schema(&value.key(), out);
    match value.value_version() {
        Some(version) => {
            out.push(1);
            object_version(out, &version);
        }
        None => out.push(0),
    }
    out.push(deletion_tag(value.deletion()));
    witness(out, value.coverage().witness());
    canonical_provenance(&value.provenance(), out);
}
fn encode_occurrence_id(value: &OccurrenceId, out: &mut Vec<u8>) {
    object_key(out, value);
}
fn encode_occurrence_record(value: &OccurrenceRecord, out: &mut Vec<u8>) {
    match value.value() {
        Some(occurrence) => {
            out.push(1);
            out.extend_from_slice(&canonical_occurrence(occurrence));
        }
        None => out.push(0),
    }
    canonical_coverage(out, value.coverage());
    canonical_provenance(&value.provenance(), out);
}
fn encode_edge_id(value: &EdgeId, out: &mut Vec<u8>) {
    object_key(out, value);
}
fn encode_edge_record(value: &EdgeRecord, out: &mut Vec<u8>) {
    match value.value() {
        Some(edge) => {
            out.push(1);
            out.extend_from_slice(&canonical_edge(edge));
        }
        None => out.push(0),
    }
    canonical_coverage(out, value.coverage());
    canonical_provenance(&value.provenance(), out);
}
fn encode_type_id(value: &TypeId, out: &mut Vec<u8>) {
    object_key(out, value);
}
fn encode_type_record(value: &TypeRecord, out: &mut Vec<u8>) {
    encode_type_schema(value.identity(), out);
    match value.value() {
        Some(node) => {
            out.push(1);
            canonical_type_node(out, node);
        }
        None => out.push(0),
    }
    canonical_coverage(out, value.coverage());
    canonical_provenance(&value.provenance(), out);
}
fn encode_component_version(value: &ComponentVersion, out: &mut Vec<u8>) {
    object_version(out, value);
}
fn encode_component_record(value: &ComponentRecord, out: &mut Vec<u8>) {
    match value.descriptor() {
        Some(descriptor) => {
            out.push(1);
            out.extend_from_slice(&canonical_component(descriptor));
        }
        None => out.push(0),
    }
    canonical_coverage(out, value.coverage());
    canonical_provenance(&value.provenance(), out);
}
fn encode_document_id(value: &EntityId, out: &mut Vec<u8>) {
    object_key(out, value);
}
fn encode_document_record(value: &DocumentationRecord, out: &mut Vec<u8>) {
    object_key(out, &value.entity());
    match value.value() {
        Some(document) => {
            out.push(1);
            encode_doc_value_schema(document, out);
        }
        None => out.push(0),
    }
    canonical_coverage(out, value.coverage());
    canonical_provenance(&value.provenance(), out);
}
fn encode_member_id(value: &MemberId, out: &mut Vec<u8>) {
    object_key(out, value);
}
fn encode_member_record_relation(value: &MemberRecord, out: &mut Vec<u8>) {
    encode_member_value_schema(value, out);
}
fn encode_attribute_id(value: &AttributeId, out: &mut Vec<u8>) {
    object_key(out, value);
}
fn encode_attribute_record_relation(value: &AttributeRecord, out: &mut Vec<u8>) {
    let key: AttributeId = ObjectKey::from_value(&value.key());
    object_key(out, &key);
    match value.value() {
        Some(attribute) => {
            out.push(1);
            canonical_attribute(out, attribute);
        }
        None => out.push(0),
    }
    canonical_coverage(out, value.coverage());
    canonical_provenance(&value.provenance(), out);
}
fn encode_extension_id(value: &ExtensionId, out: &mut Vec<u8>) {
    object_key(out, value);
}
fn encode_extension_record_relation(value: &ExtensionRecord, out: &mut Vec<u8>) {
    canonical_extension_key(out, &value.key());
    match value.value() {
        Some(extension) => {
            out.push(1);
            encode_extension_value_schema(extension, out);
        }
        None => out.push(0),
    }
    canonical_coverage(out, value.coverage());
    canonical_provenance(&value.provenance(), out);
}
fn encode_embedding_id(value: &EmbeddingInputId, out: &mut Vec<u8>) {
    object_key(out, value);
}
fn encode_embedding_record(value: &EmbeddingInputRecord, out: &mut Vec<u8>) {
    match value.value() {
        Some(input) => {
            out.push(1);
            encode_embedding_input_value_schema(input, out);
        }
        None => out.push(0),
    }
    canonical_coverage(out, value.coverage());
    canonical_provenance(&value.provenance(), out);
}

relation_impl!(
    Sources,
    101,
    SourceId,
    SourceRecord,
    encode_source_id,
    encode_source_record
);
relation_impl!(
    Entities,
    102,
    EntityId,
    EntityRecord,
    encode_entity_id,
    encode_entity_record
);
relation_impl!(
    Facets,
    103,
    FacetKey,
    FacetRecord,
    encode_facet_schema,
    encode_facet_record
);
relation_impl!(
    Occurrences,
    104,
    OccurrenceId,
    OccurrenceRecord,
    encode_occurrence_id,
    encode_occurrence_record
);
relation_impl!(
    Edges,
    105,
    EdgeId,
    EdgeRecord,
    encode_edge_id,
    encode_edge_record
);
relation_impl!(
    Types,
    106,
    TypeId,
    TypeRecord,
    encode_type_id,
    encode_type_record
);
relation_impl!(
    Components,
    107,
    ComponentVersion,
    ComponentRecord,
    encode_component_version,
    encode_component_record
);
relation_impl!(
    Documents,
    108,
    EntityId,
    DocumentationRecord,
    encode_document_id,
    encode_document_record
);
relation_impl!(
    Members,
    109,
    MemberId,
    MemberRecord,
    encode_member_id,
    encode_member_record_relation
);
relation_impl!(
    Attributes,
    110,
    AttributeId,
    AttributeRecord,
    encode_attribute_id,
    encode_attribute_record_relation
);
relation_impl!(
    Extensions,
    111,
    ExtensionId,
    ExtensionRecord,
    encode_extension_id,
    encode_extension_record_relation
);
relation_impl!(
    EmbeddingInputs,
    112,
    EmbeddingInputId,
    EmbeddingInputRecord,
    encode_embedding_id,
    encode_embedding_record
);

/// Version alias for a source relation state.
pub type SourceState = RelationState<Sources>;
/// Version alias for an entity relation state.
pub type EntityState = RelationState<Entities>;
/// Version alias for a facet relation state.
pub type FacetStateRelation = RelationState<Facets>;
/// Version alias for an occurrence relation state.
pub type OccurrenceState = RelationState<Occurrences>;
/// Version alias for an edge relation state.
pub type EdgeState = RelationState<Edges>;
/// Version alias for a type relation state.
pub type TypeStateRelation = RelationState<Types>;
/// Version alias for a component relation state.
pub type ComponentState = RelationState<Components>;
/// Version alias for a documentation relation state.
pub type DocumentationState = RelationState<Documents>;
/// Version alias for a member relation state.
pub type MemberState = RelationState<Members>;
/// Version alias for an attribute relation state.
pub type AttributeState = RelationState<Attributes>;
/// Version alias for an extension relation state.
pub type ExtensionState = RelationState<Extensions>;
/// Version alias for an embedding relation state.
pub type EmbeddingInputState = RelationState<EmbeddingInputs>;

/// Version-bound source relation transition.
pub type SourceDelta = VersionDelta<Sources>;
/// Version-bound entity relation transition.
pub type EntityDelta = VersionDelta<Entities>;
/// Version-bound facet relation transition.
pub type FacetDelta = VersionDelta<Facets>;
/// Version-bound occurrence relation transition.
pub type OccurrenceDelta = VersionDelta<Occurrences>;
/// Version-bound edge relation transition.
pub type EdgeDelta = VersionDelta<Edges>;
/// Version-bound type relation transition.
pub type TypeDelta = VersionDelta<Types>;

/// Compact source schema encoding helper for tests and adapters.
#[must_use]
pub fn source_key(value: &Source) -> SourceId {
    ObjectKey::from_value(value)
}

/// Compact entity schema encoding helper for tests and adapters.
#[must_use]
pub fn entity_key(value: &Entity) -> EntityId {
    ObjectKey::from_value(value)
}

/// Compact type schema encoding helper for tests and adapters.
#[must_use]
pub fn type_key(value: &TypeIdentity) -> TypeId {
    ObjectKey::from_value(value)
}

/// Compact component identity helper.
#[must_use]
pub fn component_version(value: &ComponentDescriptor) -> ComponentVersion {
    ObjectVersion::from_value(value)
}

/// Admits an authority scope claim from an independently verified producer
/// observation.
///
/// # Errors
///
/// Returns [`backend_version::CoverageAdmissionError::ScopeMismatch`] when
/// the declared and observed roots differ.
pub fn admit_complete_scope(
    declared: AuthorityScopeClaim,
    producer: AdmittedProducerObservation,
) -> Result<AuthorizedCompleteCoverage, backend_version::CoverageAdmissionError> {
    backend_version::admit_complete_scope(declared, producer)
}
