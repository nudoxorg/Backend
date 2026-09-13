//! Canonical encodings for semantic values and dependency inputs.

use crate::coverage::FacetCoverage;
use crate::facets::{
    Attribute, ComponentDescriptor, DocFragment, DocTarget, Edge, EmbeddingModel, ExtensionKey,
    LexicalFact, MemberKey, OccurrenceFact, OccurrenceKey, TypeExpr, TypeNode, attribute_kind_tag,
    edge_kind_tag, encode_type_operand, extension_kind_tag, lexical_kind_tag, type_state_tag,
    type_tag,
};
use crate::identity::{Authority, AuthorityValue, Entity, Provenance, Source, SourceSpan};
use crate::read_manifest::{
    ReadManifest, ReadObservation, ReadSelector, ScopedRead, ScopedReadManifest,
    ScopedReadObservation,
};
use crate::recipes::{Recipe, RecipeSpec};
use crate::support::{
    bool_tag, bytes, deletion_tag, frame, list, object_key, object_version, state_root, text,
    witness,
};

/// Canonical encoding for the legacy recursive expression grammar.
pub fn canonical_type(value: &TypeExpr, out: &mut Vec<u8>) {
    match value {
        TypeExpr::Named(name) => {
            out.push(1);
            text(out, name);
        }
        TypeExpr::Tuple(values) => {
            out.push(2);
            list(out, values, canonical_type);
        }
        TypeExpr::Function { args, result } => {
            out.push(3);
            list(out, args, canonical_type);
            let mut encoded = Vec::new();
            canonical_type(result, &mut encoded);
            frame(out, &encoded);
        }
        TypeExpr::Applied { base, args } => {
            out.push(4);
            let mut encoded = Vec::new();
            canonical_type(base, &mut encoded);
            frame(out, &encoded);
            list(out, args, canonical_type);
        }
        TypeExpr::Infer => out.push(5),
    }
}

/// Canonical encoding of one lexical fact.
#[must_use]
pub fn canonical_lexical(value: &LexicalFact) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(1);
    object_key(&mut out, &value.entity);
    out.push(lexical_kind_tag(value.kind));
    text(&mut out, &value.text);
    out
}

/// Canonical encoding of one occurrence fact.
#[must_use]
pub fn canonical_occurrence(value: &OccurrenceFact) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(1);
    object_key(&mut out, &value.target);
    object_key(&mut out, &value.source);
    canonical_occurrence_key(&mut out, &value.key);
    out.extend_from_slice(&value.multiplicity.to_be_bytes());
    out.extend_from_slice(&value.support.to_be_bytes());
    out
}

/// Canonical encoding of one derived edge fact.
#[must_use]
pub fn canonical_edge(value: &Edge) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(1);
    object_key(&mut out, &value.from);
    object_key(&mut out, &value.to);
    out.push(edge_kind_tag(value.kind));
    out.extend_from_slice(&value.multiplicity.to_be_bytes());
    out.extend_from_slice(&value.support.to_be_bytes());
    out
}

/// Canonical encoding of a complete SCC/component descriptor.
#[must_use]
pub fn canonical_component(value: &ComponentDescriptor) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(1);
    match value.graph_root() {
        Some(root) => {
            out.push(1);
            state_root(&mut out, &root);
        }
        None => out.push(0),
    }
    match value.recipe() {
        Some(recipe) => {
            out.push(1);
            object_version(&mut out, &recipe);
        }
        None => out.push(0),
    }
    // ComponentDescriptor admission sorts and de-duplicates all three
    // collections.  Re-sorting here used to allocate three full copies on
    // every version calculation, defeating immutable snapshot reuse.
    list(&mut out, value.members(), |member, encoded| {
        object_key(encoded, member);
    });
    list(&mut out, value.internal_edges(), |edge, encoded| {
        object_key(encoded, &edge.from());
        object_key(encoded, &edge.to());
        encoded.push(edge_kind_tag(edge.kind()));
    });
    list(&mut out, value.external_inputs(), |input, encoded| {
        object_key(encoded, &input.entity());
        encoded.extend_from_slice(&input.facet().tag().to_be_bytes());
        object_version(encoded, &input.version());
    });
    out
}

/// Canonical encoding of the compact dependency manifest.
#[must_use]
pub fn canonical_read_manifest(value: &ReadManifest) -> Vec<u8> {
    canonical_read_manifest_reads(value.reads())
}

/// Canonical encoding of compact read rows used while admitting a manifest.
/// Keeping this slice based avoids constructing a temporary manifest merely
/// to seed its cached encoding.
pub(crate) fn canonical_read_manifest_reads(reads: &[crate::read_manifest::Read]) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(1);
    list(&mut out, reads, |read, encoded| {
        encoded.extend_from_slice(&read.facet().tag().to_be_bytes());
        encoded.extend_from_slice(read.scope_root().as_bytes());
        encoded.extend_from_slice(&read.range_token().to_be_bytes());
        encoded.push(bool_tag(read.is_negative()));
    });
    out
}

pub(crate) fn canonical_selector(out: &mut Vec<u8>, selector: &ReadSelector) {
    match selector {
        ReadSelector::Exact(key) => {
            out.push(1);
            bytes(out, key);
        }
        ReadSelector::Range(range) => {
            out.push(2);
            bytes(out, range.start());
            bytes(out, range.end());
        }
        ReadSelector::Prefix(prefix) => {
            out.push(3);
            bytes(out, prefix);
        }
    }
}

pub(crate) fn canonical_scoped_read(value: &ScopedRead) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(1);
    out.extend_from_slice(&value.facet().tag().to_be_bytes());
    out.extend_from_slice(value.scope_root().as_bytes());
    canonical_selector(&mut out, value.selector());
    out.push(bool_tag(value.is_negative()));
    out
}

pub(crate) fn canonical_scoped_observation(out: &mut Vec<u8>, value: &ScopedReadObservation) {
    let read = canonical_scoped_read(value.read());
    frame(out, &read);
    match value.version() {
        Some(version) => {
            out.push(1);
            object_version(out, &version);
        }
        None => out.push(0),
    }
    witness(out, value.coverage());
}

pub(crate) fn canonical_scoped_manifest(value: &ScopedReadManifest) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(1);
    // ScopedReadManifest admission establishes this canonical order once.
    // Rebuilding and sorting the full observation set for every hash was a
    // hidden O(n log n) allocation on every planner/cache lookup.
    list(&mut out, value.observations(), |observation, encoded| {
        canonical_scoped_observation(encoded, observation);
    });
    out
}

/// Canonical encoding of one typed recipe.
#[must_use]
pub fn canonical_recipe(value: &RecipeSpec) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(1);
    out.push(recipe_tag(value.kind()));
    out.extend_from_slice(&value.version().to_be_bytes());
    bytes(&mut out, value.parameters());
    frame(&mut out, value.reads().canonical_bytes_ref());
    out
}

fn recipe_tag(value: Recipe) -> u8 {
    match value {
        Recipe::Names => 1,
        Recipe::LexicalFacts => 2,
        Recipe::ReferencesForward => 3,
        Recipe::ReferencesReverse => 4,
        Recipe::EmbeddingInput => 5,
        Recipe::Documents => 6,
        Recipe::Components => 7,
    }
}

pub(crate) fn canonical_source(value: &Source, out: &mut Vec<u8>) {
    out.extend_from_slice(&value.workspace().to_be_bytes());
    text(out, value.path());
}

pub(crate) fn canonical_source_span(value: &SourceSpan, out: &mut Vec<u8>) {
    canonical_source(value.source(), out);
    out.extend_from_slice(&value.start().to_be_bytes());
    out.extend_from_slice(&value.end().to_be_bytes());
}

pub(crate) fn canonical_occurrence_key(out: &mut Vec<u8>, value: &OccurrenceKey) {
    canonical_source_span(&value.span, out);
    out.extend_from_slice(&value.ordinal.to_be_bytes());
}

pub(crate) fn canonical_entity(value: &Entity, out: &mut Vec<u8>) {
    canonical_source(value.source(), out);
    text(out, value.local_key());
    match value.span() {
        Some(span) => {
            out.push(1);
            canonical_source_span(span, out);
        }
        None => out.push(0),
    }
}

pub(crate) fn canonical_authority(value: &Authority, out: &mut Vec<u8>) {
    text(out, value.namespace());
    text(out, value.capability());
}

pub(crate) fn canonical_authority_value(value: &AuthorityValue, out: &mut Vec<u8>) {
    canonical_authority(value.identity(), out);
    out.extend_from_slice(&value.revision().to_be_bytes());
    bytes(out, value.basis());
}

pub(crate) fn canonical_provenance(value: &Provenance, out: &mut Vec<u8>) {
    object_key(out, &value.authority());
    object_version(out, &value.authority_version());
    object_key(out, &value.source());
    object_version(out, &value.source_version());
}

pub(crate) fn canonical_coverage(out: &mut Vec<u8>, value: FacetCoverage) {
    witness(out, value.witness());
    out.push(deletion_tag(value.deletion()));
}

pub(crate) fn canonical_doc_target(out: &mut Vec<u8>, value: &DocTarget) {
    match value {
        DocTarget::Local(key) => {
            out.push(1);
            object_key(out, key);
        }
        DocTarget::Foreign {
            namespace,
            identity,
            variant,
        } => {
            out.push(2);
            text(out, namespace);
            text(out, identity);
            match variant {
                Some(value) => {
                    out.push(1);
                    bytes(out, value);
                }
                None => out.push(0),
            }
        }
        DocTarget::Unknown => out.push(3),
    }
}

pub(crate) fn canonical_doc_fragment(out: &mut Vec<u8>, value: &DocFragment) {
    match value {
        DocFragment::Text(value) => {
            out.push(1);
            bytes(out, value);
        }
        DocFragment::Code(value) => {
            out.push(2);
            bytes(out, value);
        }
        DocFragment::Link(value) => {
            out.push(3);
            bytes(out, &value.label);
            canonical_doc_target(out, &value.target);
        }
        DocFragment::Break => out.push(4),
    }
}

pub(crate) fn canonical_type_node(out: &mut Vec<u8>, value: &TypeNode) {
    out.push(type_tag(value.tag));
    list(out, &value.roles, |role, encoded| text(encoded, role));
    list(out, &value.operands, |operand, encoded| {
        encode_type_operand(encoded, operand);
    });
    list(out, &value.qualifiers, |qualifier, encoded| {
        text(encoded, qualifier);
    });
    match &value.literal {
        Some(literal) => {
            out.push(1);
            bytes(out, literal);
        }
        None => out.push(0),
    }
    list(out, &value.bounds, |bound, encoded| {
        object_key(encoded, bound);
    });
    out.push(type_state_tag(value.state));
}

pub(crate) fn canonical_embedding_model(out: &mut Vec<u8>, value: &EmbeddingModel) {
    text(out, value.provider());
    text(out, value.revision());
    out.extend_from_slice(&value.dimensions().to_be_bytes());
    bytes(out, value.metric());
}

pub(crate) fn canonical_read_observation(out: &mut Vec<u8>, value: &ReadObservation) {
    let read = value.read();
    out.extend_from_slice(&read.facet().tag().to_be_bytes());
    out.extend_from_slice(read.scope_root().as_bytes());
    out.push(bool_tag(read.is_negative()));
    out.extend_from_slice(&read.range_token().to_be_bytes());
    match value.version() {
        Some(version) => {
            out.push(1);
            object_version(out, &version);
        }
        None => out.push(0),
    }
    witness(out, value.coverage());
}

/// Canonical encoding helper used before an [`ExactReadManifest`] has been
/// assembled with its cached bytes.
pub(crate) fn canonical_exact_read_manifest_observations(
    observations: &[ReadObservation],
) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(1);
    list(&mut out, observations, |observation, encoded| {
        canonical_read_observation(encoded, observation);
    });
    out
}

pub(crate) fn canonical_member_key(out: &mut Vec<u8>, value: &MemberKey) {
    object_key(out, &value.owner());
    object_key(out, &value.member());
    out.extend_from_slice(&value.ordinal().to_be_bytes());
}

pub(crate) fn canonical_attribute(out: &mut Vec<u8>, value: &Attribute) {
    out.push(attribute_kind_tag(value.name));
    bytes(out, &value.value);
}

pub(crate) fn canonical_extension_key(out: &mut Vec<u8>, value: &ExtensionKey) {
    object_key(out, &value.entity());
    out.push(extension_kind_tag(value.language()));
    out.extend_from_slice(&value.tag().to_be_bytes());
}
