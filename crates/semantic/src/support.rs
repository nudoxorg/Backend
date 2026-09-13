//! Shared canonical encoding helpers for semantic modules.

use crate::{Deletion, FacetKind};
use backend_version::{
    Coverage, CoverageWitness, ObjectKey, ObjectVersion, Relation, Schema, StateRoot,
};

pub(crate) const SEMANTIC_DOMAIN: u8 = 0x53;

pub(crate) fn put_len(out: &mut Vec<u8>, len: usize) {
    let value = u64::try_from(len).unwrap_or(u64::MAX);
    out.extend_from_slice(&value.to_be_bytes());
}

pub(crate) fn frame(out: &mut Vec<u8>, bytes: &[u8]) {
    put_len(out, bytes.len());
    out.extend_from_slice(bytes);
}

pub(crate) fn text(out: &mut Vec<u8>, value: &str) {
    frame(out, value.as_bytes());
}

pub(crate) fn bytes(out: &mut Vec<u8>, value: &[u8]) {
    frame(out, value);
}

pub(crate) fn list<T>(out: &mut Vec<u8>, values: &[T], mut encode: impl FnMut(&T, &mut Vec<u8>)) {
    put_len(out, values.len());
    for value in values {
        let mut encoded = Vec::new();
        encode(value, &mut encoded);
        frame(out, &encoded);
    }
}

pub(crate) fn object_key<T: Schema>(out: &mut Vec<u8>, key: &ObjectKey<T>) {
    out.extend_from_slice(key.as_bytes());
}

pub(crate) fn object_version<T: Schema>(out: &mut Vec<u8>, version: &ObjectVersion<T>) {
    out.extend_from_slice(version.as_bytes());
}

pub(crate) fn state_root<R: Relation>(out: &mut Vec<u8>, root: &StateRoot<R>) {
    out.extend_from_slice(root.as_bytes());
}

pub(crate) fn witness(out: &mut Vec<u8>, coverage: CoverageWitness) {
    match coverage {
        CoverageWitness::Complete(value) => {
            out.push(1);
            out.extend_from_slice(value.scope_root().as_bytes());
            out.extend_from_slice(&value.producer_identity());
        }
        CoverageWitness::UntrustedComplete(value) => {
            out.push(5);
            out.extend_from_slice(value.scope_root().as_bytes());
        }
        CoverageWitness::Closed(value) => {
            out.push(6);
            out.extend_from_slice(value.scope_root().as_bytes());
        }
        CoverageWitness::Partial(value) => {
            out.push(2);
            out.extend_from_slice(value.scope_root().as_bytes());
            out.push(coverage_tag(Coverage::Partial));
        }
        CoverageWitness::Unavailable(value) => {
            out.push(3);
            out.extend_from_slice(value.scope_root().as_bytes());
        }
        CoverageWitness::Unsupported(value) => {
            out.push(4);
            out.extend_from_slice(value.scope_root().as_bytes());
        }
    }
}

pub(crate) fn coverage_tag(value: Coverage) -> u8 {
    match value {
        Coverage::Complete => 1,
        Coverage::Closed => 5,
        Coverage::Partial => 2,
        Coverage::Unavailable => 3,
        Coverage::Unsupported => 4,
    }
}

pub(crate) fn bool_tag(value: bool) -> u8 {
    u8::from(value)
}

pub(crate) const fn facet_tag(value: FacetKind) -> u16 {
    match value {
        FacetKind::Source => 1,
        FacetKind::Entity => 2,
        FacetKind::Facet => 3,
        FacetKind::Name => 4,
        FacetKind::Lexical => 5,
        FacetKind::Visibility => 6,
        FacetKind::Documentation => 7,
        FacetKind::Signature => 8,
        FacetKind::Attributes => 9,
        FacetKind::Generics => 10,
        FacetKind::Constraints => 11,
        FacetKind::Type => 12,
        FacetKind::Component => 13,
        FacetKind::Member => 14,
        FacetKind::Occurrence => 15,
        FacetKind::Edge => 16,
        FacetKind::Extension => 17,
        FacetKind::EmbeddingInput => 18,
        FacetKind::DocLink => 19,
        FacetKind::SourceSpan => 20,
        FacetKind::Authority => 21,
        FacetKind::Configuration => 22,
        FacetKind::GeneratedSource => 23,
    }
}

pub(crate) fn deletion_tag(value: Deletion) -> u8 {
    match value {
        Deletion::Live => 1,
        Deletion::CapturedEmpty => 2,
        Deletion::Absent => 3,
        Deletion::Deleted => 4,
    }
}
