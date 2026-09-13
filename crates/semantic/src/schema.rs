//! Version schemas for semantic values.

use crate::canonical::{
    canonical_attribute, canonical_authority, canonical_authority_value, canonical_component,
    canonical_coverage, canonical_doc_fragment, canonical_embedding_model, canonical_entity,
    canonical_extension_key, canonical_member_key, canonical_occurrence_key, canonical_provenance,
    canonical_recipe, canonical_source, canonical_type_node,
};
use crate::facets::{
    Attribute, AttributeKey, ComponentDescriptor, DocumentationValue, Edge, EdgeKey,
    EmbeddingInput, EmbeddingInputValue, EmbeddingModel, ExtensionFact, ExtensionKey, FacetKey,
    FacetValue, MemberKey, MemberRecord, OccurrenceAddress, OccurrenceFact, TypeIdentity, TypeNode,
    VisibilityState, edge_kind_tag, extension_kind_tag,
};
use crate::identity::{
    Authority, AuthorityValue, Entity, EntityValue, Source, SourceValue, encode_parent,
    entity_kind_tag, source_encoding_tag,
};
use crate::read_manifest::{AuthorityScope, ExactReadManifest, ReadManifest, ScopedReadManifest};
use crate::recipes::RecipeSpec;
use crate::support::{SEMANTIC_DOMAIN, bytes, frame, list, object_key, object_version, text};
use backend_version::Schema;

/// Schema marker for source identities.
pub struct SourceSchema;
/// Schema marker for source values.
pub struct SourceValueSchema;
/// Schema marker for authority identities.
pub struct AuthoritySchema;
/// Schema marker for authority values.
pub struct AuthorityValueSchema;
/// Schema marker for entity identities.
pub struct EntitySchema;
/// Schema marker for entity values.
pub struct EntityValueSchema;
/// Schema marker for facet keys.
pub struct FacetSchema;
/// Schema marker for facet payloads.
pub struct FacetValueSchema;
/// Schema marker for occurrence identities.
pub struct OccurrenceSchema;
/// Schema marker for occurrence values.
pub struct OccurrenceValueSchema;
/// Schema marker for edge identities.
pub struct EdgeSchema;
/// Schema marker for edge values.
pub struct EdgeValueSchema;
/// Schema marker for type identities.
pub struct TypeSchema;
/// Schema marker for type values.
pub struct TypeValueSchema;
/// Schema marker for SCC/component values.
pub struct ComponentSchema;
/// Schema marker for documentation values.
pub struct DocumentationValueSchema;
/// Schema marker for member identities.
pub struct MemberSchema;
/// Schema marker for member values.
pub struct MemberValueSchema;
/// Schema marker for attribute identities.
pub struct AttributeSchema;
/// Schema marker for attribute values.
pub struct AttributeValueSchema;
/// Schema marker for extension identities.
pub struct ExtensionSchema;
/// Schema marker for extension values.
pub struct ExtensionValueSchema;
/// Schema marker for embedding models.
pub struct EmbeddingModelSchema;
/// Schema marker for embedding input identities.
pub struct EmbeddingInputSchema;
/// Schema marker for embedding input values.
pub struct EmbeddingInputValueSchema;
/// Schema marker for typed recipes.
pub struct RecipeSchema;
/// Schema marker for compact read manifests.
pub struct ReadManifestSchema;
/// Schema marker for witnessed read manifests.
pub struct ExactReadManifestSchema;
/// Schema marker for full-width scoped read manifests.
pub struct ScopedReadManifestSchema;
/// Schema marker for authority scopes.
pub struct AuthorityScopeSchema;

macro_rules! impl_schema {
    ($name:ty, $ty:expr, $value:ty, $encode:path) => {
        impl Schema for $name {
            const DOMAIN: u8 = SEMANTIC_DOMAIN;
            const TYPE: u16 = $ty;
            type Value = $value;
            fn encode(value: &Self::Value, out: &mut Vec<u8>) {
                $encode(value, out);
            }
        }
    };
}

pub(crate) fn encode_source_schema(value: &Source, out: &mut Vec<u8>) {
    canonical_source(value, out);
}
pub(crate) fn encode_source_value_schema(value: &SourceValue, out: &mut Vec<u8>) {
    bytes(out, value.bytes());
    out.push(source_encoding_tag(value.encoding()));
    out.extend_from_slice(&value.revision().to_be_bytes());
}
pub(crate) fn encode_authority_schema(value: &Authority, out: &mut Vec<u8>) {
    canonical_authority(value, out);
}
pub(crate) fn encode_authority_value_schema(value: &AuthorityValue, out: &mut Vec<u8>) {
    canonical_authority_value(value, out);
}
pub(crate) fn encode_entity_schema(value: &Entity, out: &mut Vec<u8>) {
    canonical_entity(value, out);
}
pub(crate) fn encode_entity_value_schema(value: &EntityValue, out: &mut Vec<u8>) {
    match value.name() {
        Some(name) => {
            out.push(1);
            text(out, name);
        }
        None => out.push(0),
    }
    out.push(entity_kind_tag(value.kind()));
    out.push(match value.visibility() {
        VisibilityState::Public => 1,
        VisibilityState::Private => 2,
        VisibilityState::Unavailable => 3,
    });
    encode_parent(out, value.parent());
}
pub(crate) fn encode_facet_schema(value: &FacetKey, out: &mut Vec<u8>) {
    object_key(out, &value.entity());
    out.extend_from_slice(&value.facet().tag().to_be_bytes());
}
pub(crate) fn encode_facet_value_schema(value: &FacetValue, out: &mut Vec<u8>) {
    out.extend_from_slice(&value.facet().tag().to_be_bytes());
    bytes(out, value.bytes());
}
pub(crate) fn encode_occurrence_schema(value: &OccurrenceAddress, out: &mut Vec<u8>) {
    object_key(out, &value.source);
    object_key(out, &value.target);
    canonical_occurrence_key(out, &value.key);
}
pub(crate) fn encode_occurrence_value_schema(value: &OccurrenceFact, out: &mut Vec<u8>) {
    out.extend_from_slice(&value.multiplicity.to_be_bytes());
    out.extend_from_slice(&value.support.to_be_bytes());
}
pub(crate) fn encode_edge_schema(value: &EdgeKey, out: &mut Vec<u8>) {
    object_key(out, &value.from);
    object_key(out, &value.to);
    out.push(edge_kind_tag(value.kind));
}
pub(crate) fn encode_edge_value_schema(value: &Edge, out: &mut Vec<u8>) {
    out.extend_from_slice(&value.multiplicity.to_be_bytes());
    out.extend_from_slice(&value.support.to_be_bytes());
}
pub(crate) fn encode_type_schema(value: &TypeIdentity, out: &mut Vec<u8>) {
    object_key(out, &value.owner());
    text(out, value.local_key());
}
pub(crate) fn encode_type_value_schema(value: &TypeNode, out: &mut Vec<u8>) {
    canonical_type_node(out, value);
}
pub(crate) fn encode_component_schema(value: &ComponentDescriptor, out: &mut Vec<u8>) {
    out.extend_from_slice(&canonical_component(value));
}
pub(crate) fn encode_doc_value_schema(value: &DocumentationValue, out: &mut Vec<u8>) {
    list(out, &value.fragments, |fragment, encoded| {
        canonical_doc_fragment(encoded, fragment);
    });
}
pub(crate) fn encode_member_schema(value: &MemberKey, out: &mut Vec<u8>) {
    canonical_member_key(out, value);
}
pub(crate) fn encode_member_value_schema(value: &MemberRecord, out: &mut Vec<u8>) {
    canonical_member_key(out, &value.key());
    canonical_coverage(out, value.coverage());
    canonical_provenance(&value.provenance(), out);
}
pub(crate) fn encode_attribute_schema(value: &AttributeKey, out: &mut Vec<u8>) {
    object_key(out, &value.entity());
    out.extend_from_slice(&value.ordinal().to_be_bytes());
}
pub(crate) fn encode_attribute_value_schema(value: &Attribute, out: &mut Vec<u8>) {
    canonical_attribute(out, value);
}
pub(crate) fn encode_extension_schema(value: &ExtensionKey, out: &mut Vec<u8>) {
    canonical_extension_key(out, value);
}
pub(crate) fn encode_extension_value_schema(value: &ExtensionFact, out: &mut Vec<u8>) {
    object_key(out, &value.entity);
    out.push(extension_kind_tag(value.language));
    out.extend_from_slice(&value.tag.to_be_bytes());
    bytes(out, &value.payload);
}
pub(crate) fn encode_embedding_model_schema(value: &EmbeddingModel, out: &mut Vec<u8>) {
    canonical_embedding_model(out, value);
}
pub(crate) fn encode_embedding_input_schema(value: &EmbeddingInput, out: &mut Vec<u8>) {
    object_key(out, &value.entity());
    object_version(out, &value.input_version());
    out.extend_from_slice(&value.model().to_be_bytes());
}
pub(crate) fn encode_embedding_input_value_schema(value: &EmbeddingInputValue, out: &mut Vec<u8>) {
    object_key(out, &value.entity());
    object_version(out, &value.input_version());
    object_version(out, &value.model_version());
    frame(out, value.reads().canonical_bytes_ref());
}
pub(crate) fn encode_recipe_schema(value: &RecipeSpec, out: &mut Vec<u8>) {
    out.extend_from_slice(&canonical_recipe(value));
}
pub(crate) fn encode_read_manifest_schema(value: &ReadManifest, out: &mut Vec<u8>) {
    out.extend_from_slice(value.canonical_bytes_ref());
}
pub(crate) fn encode_exact_read_manifest_schema(value: &ExactReadManifest, out: &mut Vec<u8>) {
    out.extend_from_slice(value.canonical_bytes_ref());
}
pub(crate) fn encode_scoped_read_manifest_schema(value: &ScopedReadManifest, out: &mut Vec<u8>) {
    out.extend_from_slice(value.canonical_bytes_ref());
}
pub(crate) fn encode_authority_scope_schema(value: &AuthorityScope, out: &mut Vec<u8>) {
    out.extend_from_slice(&value.canonical_bytes());
}

impl_schema!(SourceSchema, 1, Source, encode_source_schema);
impl_schema!(
    SourceValueSchema,
    2,
    SourceValue,
    encode_source_value_schema
);
impl_schema!(AuthoritySchema, 3, Authority, encode_authority_schema);
impl_schema!(
    AuthorityValueSchema,
    4,
    AuthorityValue,
    encode_authority_value_schema
);
impl_schema!(EntitySchema, 5, Entity, encode_entity_schema);
impl_schema!(
    EntityValueSchema,
    6,
    EntityValue,
    encode_entity_value_schema
);
impl_schema!(FacetSchema, 7, FacetKey, encode_facet_schema);
impl_schema!(FacetValueSchema, 8, FacetValue, encode_facet_value_schema);
impl_schema!(
    OccurrenceSchema,
    9,
    OccurrenceAddress,
    encode_occurrence_schema
);
impl_schema!(
    OccurrenceValueSchema,
    10,
    OccurrenceFact,
    encode_occurrence_value_schema
);
impl_schema!(EdgeSchema, 11, EdgeKey, encode_edge_schema);
impl_schema!(EdgeValueSchema, 12, Edge, encode_edge_value_schema);
impl_schema!(TypeSchema, 13, TypeIdentity, encode_type_schema);
impl_schema!(TypeValueSchema, 14, TypeNode, encode_type_value_schema);
impl_schema!(
    ComponentSchema,
    15,
    ComponentDescriptor,
    encode_component_schema
);
impl_schema!(
    DocumentationValueSchema,
    16,
    DocumentationValue,
    encode_doc_value_schema
);
impl_schema!(MemberSchema, 17, MemberKey, encode_member_schema);
impl_schema!(
    MemberValueSchema,
    18,
    MemberRecord,
    encode_member_value_schema
);
impl_schema!(AttributeSchema, 19, AttributeKey, encode_attribute_schema);
impl_schema!(
    AttributeValueSchema,
    20,
    Attribute,
    encode_attribute_value_schema
);
impl_schema!(ExtensionSchema, 21, ExtensionKey, encode_extension_schema);
impl_schema!(
    ExtensionValueSchema,
    22,
    ExtensionFact,
    encode_extension_value_schema
);
impl_schema!(
    EmbeddingModelSchema,
    23,
    EmbeddingModel,
    encode_embedding_model_schema
);
impl_schema!(
    EmbeddingInputSchema,
    24,
    EmbeddingInput,
    encode_embedding_input_schema
);
impl_schema!(
    EmbeddingInputValueSchema,
    25,
    EmbeddingInputValue,
    encode_embedding_input_value_schema
);
impl_schema!(RecipeSchema, 26, RecipeSpec, encode_recipe_schema);
impl_schema!(
    ReadManifestSchema,
    27,
    ReadManifest,
    encode_read_manifest_schema
);
impl_schema!(
    ExactReadManifestSchema,
    28,
    ExactReadManifest,
    encode_exact_read_manifest_schema
);
impl_schema!(
    ScopedReadManifestSchema,
    30,
    ScopedReadManifest,
    encode_scoped_read_manifest_schema
);
impl_schema!(
    AuthorityScopeSchema,
    29,
    AuthorityScope,
    encode_authority_scope_schema
);
