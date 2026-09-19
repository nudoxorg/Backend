use crate::FacetKind;
use crate::read_manifest::ScopedReadObservation;
use backend_flow::WorkKey;
use backend_version::{ObjectKey, ObjectVersion, Schema, ScopeRoot};
use std::fmt;
use std::hash::Hash;
use std::sync::Arc;

/// Role marker for keys that identify retained semantic readers.
///
/// The reverse index is generic over this role so an execution-owned work key
/// can be supplied directly without converting it into the unrelated flow
/// `WorkKey` layout. Every execution-owned key must provide its own canonical
/// encoding; its `Hash` implementation remains an in-memory indexing detail.
pub trait SemanticReaderKey: Copy + fmt::Debug + Eq + Ord + Hash + Send + Sync + 'static {
    /// Encodes the execution-owned identity into a relation-key commitment.
    ///
    /// Implementations must write the complete, collision-resistant canonical
    /// identity. A `std::hash::Hash` value is only a map/treap aid and is not
    /// wide enough to identify persisted semantic state.
    fn encode_canonical(&self, out: &mut Vec<u8>);

    /// Returns the complete canonical identity bytes.
    ///
    /// This convenience method is intended for persistence adapters and
    /// diagnostics.  Callers must use the returned bytes as an identity
    /// commitment; the in-memory `Hash` implementation remains only a map
    /// optimization and is allowed to collide.
    fn canonical_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode_canonical(&mut out);
        out
    }
}

impl<S: Schema> SemanticReaderKey for ObjectVersion<S> {
    fn encode_canonical(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(b"backend.semantic.reader.object-version.v1\0");
        out.push(S::DOMAIN);
        out.extend_from_slice(&S::TYPE.to_be_bytes());
        out.push(S::VERSION);
        out.extend_from_slice(self.as_bytes());
    }
}

impl<S: Schema> SemanticReaderKey for ObjectKey<S> {
    fn encode_canonical(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(b"backend.semantic.reader.object-key.v1\0");
        out.push(S::DOMAIN);
        out.extend_from_slice(&S::TYPE.to_be_bytes());
        out.push(S::VERSION);
        out.extend_from_slice(self.as_bytes());
    }
}

impl SemanticReaderKey for WorkKey {
    fn encode_canonical(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(b"backend.semantic.reader.work-key.v1\0");
        out.extend_from_slice(&self.recipe.to_bytes());
        out.extend_from_slice(&self.input.to_bytes());
        out.extend_from_slice(&self.read.to_bytes());
        out.extend_from_slice(&self.authority.to_bytes());
        out.extend_from_slice(&self.equivalence.to_bytes());
    }
}

/// Default semantic reader-key role used by flow-facing callers.
///
/// Execution integrations should instantiate the reverse index with their
/// execution-owned work key and use [`SemanticReaderKeyAdapter`] at the
/// boundary. Keeping this alias explicit prevents the flow key's identity
/// role from being mistaken for an arbitrary recipe or input root.
pub type SemanticWorkKey = WorkKey;

/// Typed bridge from an execution identity to its retained-reader key.
///
/// An engine adapter implements this trait for its own identity type and
/// chooses the corresponding `RetainedReaders<K>` instantiation. The bridge
/// carries the role at the type boundary, so a recipe/input identity cannot be
/// passed where a reader key is expected by accident. Since the semantic
/// crate must not depend upward on `backend-execution`, the engine can define
/// a local wrapper around `VersionedWorkIdentity<R>` and implement this trait
/// for that wrapper, with `ReaderKey = backend_execution::WorkKey` and
/// `reader_key()` delegating to `work_key()`.
pub trait SemanticReaderKeyAdapter {
    /// Concrete key domain used by the semantic reverse index.
    type ReaderKey: SemanticReaderKey;

    /// Returns the exact key identifying this execution's reader shard.
    fn reader_key(&self) -> Self::ReaderKey;
}

/// A retained reader's bounded reverse-index registration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReaderRegistration<K: SemanticReaderKey = SemanticWorkKey> {
    /// Recipe/output shard that issued the read.
    pub reader: K,
    /// Exact witnessed dependency.
    pub observation: ScopedReadObservation,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::reuse) struct ReaderBucket {
    pub(in crate::reuse) facet: FacetKind,
    pub(in crate::reuse) scope: ScopeRoot,
}

impl Ord for ReaderBucket {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.facet
            .cmp(&other.facet)
            .then_with(|| self.scope.as_bytes().cmp(other.scope.as_bytes()))
    }
}

impl PartialOrd for ReaderBucket {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(in crate::reuse) struct RegistrationId<K: SemanticReaderKey = SemanticWorkKey> {
    pub(in crate::reuse) reader: K,
    /// Shared canonical selector bytes. Exact, interval, and observation
    /// arrangements all point at the same immutable key lane instead of
    /// retaining one allocation per arrangement.
    pub(in crate::reuse) selector: Arc<[u8]>,
}
