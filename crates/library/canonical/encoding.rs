//! Canonical preimages for relation roots and view versions.

use super::frontier::Frontier;
use super::schema::{
    ViewEntry, ViewEntryKey, ViewMetadata, ViewRecipeId, ViewRelation, ViewStateRoot,
};
use super::values::object_version;
use crate::{Basis, Coverage, Fragment, Row, RowId, RowState};
use backend_version::{canonical_empty, canonical_root};
use core::marker::PhantomData;

/// Computes a visible relation root from already ordered canonical entries.
///
/// Callers that require a complete authority witness should use the version
/// crate's checked [`backend_version::RelationState`] admission path. This
/// helper only computes the content root used by immutable view transitions.
#[must_use]
pub fn view_state_root(entries: &[(String, String)]) -> ViewStateRoot {
    let basis = Basis::new(
        canonical_empty::<ViewRelation>().commitment(),
        object_version(b"view-state-source"),
    );
    let entries = entries
        .iter()
        .map(|(key, value)| {
            (
                ViewEntryKey::Row(RowId::Object(object_version(key.as_bytes()))),
                ViewEntry::Row(Row::new(
                    RowId::Object(object_version(key.as_bytes())),
                    basis,
                    value.clone(),
                )),
            )
        })
        .collect::<Vec<_>>();
    canonical_root::<ViewRelation>(&entries).map_or_else(
        |_| canonical_empty::<ViewRelation>().commitment(),
        |node| node.commitment(),
    )
}

/// Returns the canonical empty view-relation preimage owned by this schema.
///
/// Transport adapters use this when certifying an empty basis, avoiding a
/// duplicate literal copy of the version tree ABI or cut-policy version.
#[must_use]
pub fn empty_view_relation_preimage() -> Vec<u8> {
    canonical_empty::<ViewRelation>().as_bytes().to_vec()
}

/// Returns the complete canonical preimage used for one immutable view
/// version. Producers place these bytes in a `WireClaim::Version`; receivers
/// re-admit the resulting object version before using it as a view identity.
#[must_use]
pub fn view_version_preimage(
    recipe: ViewRecipeId,
    basis: Basis,
    frontier: Frontier,
    root: ViewStateRoot,
    coverage: &[Coverage],
) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"VIEWVERSION\0");
    append_bytes(&mut bytes, recipe.as_bytes());
    append_bytes(&mut bytes, basis.root.as_bytes());
    append_bytes(&mut bytes, basis.object.as_bytes());
    append_bytes(&mut bytes, basis.branch.as_bytes());
    append_bytes(&mut bytes, basis.log.as_bytes());
    bytes.extend_from_slice(&basis.schema.to_be_bytes());
    append_frontier(&mut bytes, frontier);
    append_bytes(&mut bytes, root.as_bytes());
    append_coverage(&mut bytes, coverage);
    bytes
}

/// Returns the relation key for an accepted stable row identity.
#[must_use]
pub const fn relation_row_key(id: RowId) -> ViewEntryKey {
    ViewEntryKey::Row(id)
}

/// Encodes a complete visible row for the canonical view relation.
pub(super) fn encode_row(value: &Row, out: &mut Vec<u8>) {
    out.extend_from_slice(b"VIEWROW\0");
    append_row_id(out, value.id);
    append_bytes(out, value.basis.root.as_bytes());
    append_bytes(out, value.basis.object.as_bytes());
    append_bytes(out, value.basis.branch.as_bytes());
    append_bytes(out, value.basis.log.as_bytes());
    out.extend_from_slice(&value.basis.schema.to_be_bytes());
    out.push(match value.state {
        RowState::Ready => 0,
        RowState::Loading => 1,
        RowState::Failed => 2,
    });
    append_bytes(out, value.label.as_bytes());
    match value.score {
        Some(score) => {
            out.push(1);
            out.extend_from_slice(&score.to_be_bytes());
        }
        None => out.push(0),
    }
    match value.package {
        Some(package) => {
            out.push(1);
            append_bytes(out, package.as_bytes());
        }
        None => out.push(0),
    }
    match value.parent {
        Some(parent) => {
            out.push(1);
            append_bytes(out, parent.as_bytes());
        }
        None => out.push(0),
    }
    append_fragments(out, &value.document);
    match &value.signature {
        Some(signature) => {
            out.push(1);
            append_bytes(out, signature.as_bytes());
        }
        None => out.push(0),
    }
    match value.kind {
        Some(kind) => {
            out.push(1);
            out.push(kind.wire_tag());
        }
        None => out.push(0),
    }
    match &value.source {
        Some(source) => {
            out.push(1);
            append_bytes(out, source.path().as_bytes());
            out.extend_from_slice(&source.start_line().to_be_bytes());
        }
        None => out.push(0),
    }
}

fn append_fragments(out: &mut Vec<u8>, fragments: &[Fragment]) {
    out.extend_from_slice(&(fragments.len() as u64).to_be_bytes());
    for fragment in fragments {
        match fragment {
            Fragment::Text(text) => {
                out.push(0);
                append_bytes(out, text.as_bytes());
            }
            Fragment::Code(text) => {
                out.push(1);
                append_bytes(out, text.as_bytes());
            }
            Fragment::Link { label, target } => {
                out.push(2);
                append_bytes(out, label.as_bytes());
                append_bytes(out, target.as_bytes());
            }
            Fragment::Break => out.push(3),
        }
    }
}

pub(super) fn encode_metadata(value: &ViewMetadata, out: &mut Vec<u8>) {
    out.extend_from_slice(b"VIEWMETA\0");
    append_bytes(out, value.basis.root.as_bytes());
    append_bytes(out, value.basis.object.as_bytes());
    append_bytes(out, value.basis.branch.as_bytes());
    append_bytes(out, value.basis.log.as_bytes());
    out.extend_from_slice(&value.basis.schema.to_be_bytes());
    append_bytes(out, value.frontier.branch.as_bytes());
    append_bytes(out, value.frontier.log.as_bytes());
    out.extend_from_slice(&value.frontier.schema.to_be_bytes());
    append_bytes(out, value.frontier.root.as_bytes());
    out.extend_from_slice(&value.frontier.sequence.to_be_bytes());
    out.extend_from_slice(&(value.coverage.len() as u64).to_be_bytes());
    for coverage in &value.coverage {
        match coverage {
            Coverage::Complete => out.push(0),
            Coverage::Partial { completed, total } => {
                out.push(1);
                out.extend_from_slice(&completed.to_be_bytes());
                out.extend_from_slice(&total.to_be_bytes());
            }
            Coverage::Unavailable { lane, reason } => {
                out.push(2);
                out.push(match lane {
                    crate::Lane::Exact => 0,
                    crate::Lane::Names => 1,
                    crate::Lane::Graph => 2,
                    crate::Lane::Semantic => 3,
                });
                out.push(match reason {
                    crate::Reason::NoIndex => 0,
                    crate::Reason::Unconfigured => 1,
                    crate::Reason::Offline => 2,
                    crate::Reason::Cancelled => 3,
                    crate::Reason::Incomplete => 4,
                });
            }
        }
    }
}

pub(super) fn append_row_id(out: &mut Vec<u8>, id: RowId) {
    match id {
        RowId::Package(value) => {
            out.push(0);
            append_bytes(out, value.as_bytes());
        }
        RowId::Symbol(value) => {
            out.push(1);
            append_bytes(out, value.as_bytes());
        }
        RowId::Object(value) => {
            out.push(2);
            append_bytes(out, value.as_bytes());
        }
    }
}

fn append_bytes(out: &mut Vec<u8>, value: &[u8]) {
    out.extend_from_slice(&(value.len() as u64).to_be_bytes());
    out.extend_from_slice(value);
}

fn append_frontier(out: &mut Vec<u8>, frontier: Frontier) {
    append_bytes(out, frontier.branch.as_bytes());
    append_bytes(out, frontier.log.as_bytes());
    out.extend_from_slice(&frontier.schema.to_be_bytes());
    append_bytes(out, frontier.root.as_bytes());
    out.extend_from_slice(&frontier.sequence.to_be_bytes());
}

fn append_coverage(out: &mut Vec<u8>, coverage: &[Coverage]) {
    out.extend_from_slice(&(coverage.len() as u64).to_be_bytes());
    for value in coverage {
        match value {
            Coverage::Complete => out.push(0),
            Coverage::Partial { completed, total } => {
                out.push(1);
                out.extend_from_slice(&completed.to_be_bytes());
                out.extend_from_slice(&total.to_be_bytes());
            }
            Coverage::Unavailable { lane, reason } => {
                out.push(2);
                out.push(match lane {
                    crate::Lane::Exact => 0,
                    crate::Lane::Names => 1,
                    crate::Lane::Graph => 2,
                    crate::Lane::Semantic => 3,
                });
                out.push(match reason {
                    crate::Reason::NoIndex => 0,
                    crate::Reason::Unconfigured => 1,
                    crate::Reason::Offline => 2,
                    crate::Reason::Cancelled => 3,
                    crate::Reason::Incomplete => 4,
                });
            }
        }
    }
}

/// Hashes length-delimited recipe/source parts into one view identity.
#[must_use]
pub fn view_identity_bytes(parts: &[&[u8]]) -> ViewRecipeId {
    let mut bytes = Vec::new();
    for part in parts {
        bytes.extend_from_slice(&(part.len() as u64).to_be_bytes());
        bytes.extend_from_slice(part);
    }
    ViewRecipeId::from_value(&bytes)
}

/// Marker used to retain a generic type parameter in APIs that store a wire
/// claim before deciding whether it is a key, version, root, or delta.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IdentityMarker<T>(PhantomData<fn() -> T>);
