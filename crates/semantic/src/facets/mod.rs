//! Typed semantic facet values and evidence records.

use crate::SemanticError;
use crate::canonical::canonical_component;
use crate::coverage::{Deletion, FacetCoverage};
use crate::identity::{EntityId, EntityVersion, Provenance, SourceSpan, validate_value_state};
use crate::read_manifest::ReadManifest;
use crate::recipes::RecipeVersion;
use crate::relations::Types;
use crate::schema::{
    AttributeSchema, AttributeValueSchema, ComponentSchema, DocumentationValueSchema, EdgeSchema,
    EdgeValueSchema, EmbeddingInputSchema, EmbeddingInputValueSchema, EmbeddingModelSchema,
    ExtensionSchema, ExtensionValueSchema, FacetValueSchema, MemberSchema, MemberValueSchema,
    OccurrenceSchema, OccurrenceValueSchema, TypeSchema, TypeValueSchema,
};
use crate::support::{bytes, facet_tag, object_key, text};
use backend_version::AuthorizedCompleteCoverage;
use backend_version::{ObjectKey, ObjectVersion, StateRoot};
use std::sync::Arc;

// Each child owns one semantic family.  The parent retains the canonical
// imports and re-exports the checked public vocabulary as one stable facade;
// callers never need to know which storage/encoding boundary owns a type.
// The family modules intentionally consume this shared vocabulary through a
// wildcard import; keep the lint exception scoped to these declarations.
#[allow(clippy::wildcard_imports)]
mod availability;
#[allow(clippy::wildcard_imports)]
mod docs;
#[allow(clippy::wildcard_imports)]
mod embedding;
#[allow(clippy::wildcard_imports)]
mod entity;
#[allow(clippy::wildcard_imports)]
mod extension;
#[allow(clippy::wildcard_imports)]
mod graph;
#[allow(clippy::wildcard_imports)]
mod manifest;
#[allow(clippy::wildcard_imports)]
mod member;
#[allow(clippy::wildcard_imports)]
mod occurrence_edge;
#[allow(clippy::wildcard_imports)]
mod type_facts;

pub use availability::*;
pub use docs::*;
pub use embedding::*;
pub use entity::*;
pub use extension::*;
pub use graph::*;
pub use manifest::*;
pub use member::*;
pub use occurrence_edge::*;
pub use type_facts::*;

pub(crate) use availability::lexical_kind_tag;
pub(crate) use extension::extension_kind_tag;
pub(crate) use member::attribute_kind_tag;
pub(crate) use occurrence_edge::edge_kind_tag;
pub(crate) use type_facts::encode_type_operand;
pub(crate) use type_facts::{type_state_tag, type_tag};
