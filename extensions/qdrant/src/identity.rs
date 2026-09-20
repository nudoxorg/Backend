//! Stable relation, schema, candidate, and binding identities for the vector extension.

use crate::Error;
use backend_version::{ObjectVersion, Relation, Schema, StateRoot, WorkspaceRoot};

/// The schema marker for the vector candidate relation.
#[derive(Debug, Eq, PartialEq)]
pub struct CandidateRelation;

impl Relation for CandidateRelation {
    const DOMAIN: u8 = 0x71;
    const TYPE: u16 = 1;
    type Key = u64;
    type Value = Vec<u8>;

    fn encode_key(key: &Self::Key, out: &mut Vec<u8>) {
        out.extend_from_slice(&key.to_be_bytes());
    }

    fn encode_value(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(&(value.len() as u64).to_be_bytes());
        out.extend_from_slice(value);
    }
}

/// Versioned candidate relation root.
pub type Root = StateRoot<CandidateRelation>;

/// Schema marker for an ANN recipe value.
#[derive(Debug, Eq, PartialEq)]
pub struct RecipeSchema;

impl Schema for RecipeSchema {
    const DOMAIN: u8 = 0x71;
    const TYPE: u16 = 2;
    type Value = [u8; 32];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Schema marker for an authority/capability value.
#[derive(Debug, Eq, PartialEq)]
pub struct AuthoritySchema;

impl Schema for AuthoritySchema {
    const DOMAIN: u8 = 0x71;
    const TYPE: u16 = 3;
    type Value = [u8; 32];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Schema marker for the exact read/dependency manifest used by a query.
#[derive(Debug, Eq, PartialEq)]
pub struct ReadManifestSchema;

impl Schema for ReadManifestSchema {
    const DOMAIN: u8 = 0x71;
    const TYPE: u16 = 4;
    type Value = [u8];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// ANN recipe version.
pub type Recipe = ObjectVersion<RecipeSchema>;
/// Authority/capability version.
pub type Authority = ObjectVersion<AuthoritySchema>;
/// Exact positive/negative/range dependency manifest version.
pub type ReadManifest = ObjectVersion<ReadManifestSchema>;

/// Schema marker for the completed frontier selected for vector results.
#[derive(Debug, Eq, PartialEq)]
pub struct FrontierSchema;

impl Schema for FrontierSchema {
    const DOMAIN: u8 = 0x71;
    const TYPE: u16 = 5;
    type Value = [u8; 32];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Exact completed execution frontier identity.
pub type Frontier = ObjectVersion<FrontierSchema>;

/// Schema marker for the immutable embedding/model revision used by ANN.
#[derive(Debug, Eq, PartialEq)]
pub struct ModelSchema;

impl Schema for ModelSchema {
    const DOMAIN: u8 = 0x71;
    const TYPE: u16 = 6;
    type Value = [u8; 32];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Immutable model/provider revision used to build an ANN materialization.
pub type ModelVersion = ObjectVersion<ModelSchema>;

/// The schema tag expected at this adapter boundary.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SchemaVersion {
    /// Domain byte.
    pub domain: u8,
    /// Relation type number.
    pub type_id: u16,
    /// Canonical encoding version.
    pub version: u8,
}

impl SchemaVersion {
    /// The current candidate page schema.
    pub const CURRENT: Self = Self {
        domain: CandidateRelation::DOMAIN,
        type_id: CandidateRelation::TYPE,
        version: CandidateRelation::VERSION,
    };

    /// Creates a schema tag received from a provider.
    #[must_use]
    pub const fn new(domain: u8, type_id: u16, version: u8) -> Self {
        Self {
            domain,
            type_id,
            version,
        }
    }
}

/// Stable logical candidate identity.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CandidateId(pub u64);

impl CandidateId {
    /// Creates a candidate identity. Zero is reserved for malformed provider
    /// records and is therefore rejected.
    ///
    /// # Errors
    ///
    /// Returns [`Error::MalformedInput`] when `value` is zero.
    pub const fn new(value: u64) -> Result<Self, Error> {
        if value == 0 {
            Err(Error::MalformedInput)
        } else {
            Ok(Self(value))
        }
    }

    /// Returns whether this identity is valid at the adapter boundary.
    #[must_use]
    pub const fn is_valid(self) -> bool {
        self.0 != 0
    }
}

/// Bounded resource limits for provider pages and candidate deltas.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    /// Maximum candidates in one accepted result.
    pub max_candidates: usize,
    /// Maximum tombstones in one request.
    pub max_tombstones: usize,
    /// Maximum bytes in one candidate payload.
    pub max_payload_bytes: usize,
    /// Maximum candidate payload bytes in one relation state.
    pub max_total_payload_bytes: usize,
    /// Maximum page size requested from a provider.
    pub max_page: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_candidates: 4096,
            max_tombstones: 4096,
            max_payload_bytes: 4096,
            max_total_payload_bytes: 4 * 1024 * 1024,
            max_page: 256,
        }
    }
}

impl Limits {
    /// Validates limits before they can drive allocation or provider work.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidLimits`] when a limit is zero or the page limit
    /// exceeds the candidate limit.
    pub const fn validate(self) -> Result<Self, Error> {
        if self.max_candidates == 0
            || self.max_tombstones == 0
            || self.max_payload_bytes == 0
            || self.max_total_payload_bytes == 0
            || self.max_page == 0
            || self.max_page > self.max_candidates
        {
            Err(Error::InvalidLimits)
        } else {
            Ok(self)
        }
    }
}

/// Tombstones from the exact visible candidate relation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Tombstones(pub Vec<CandidateId>);

impl Tombstones {
    /// Admits and canonicalizes tombstones under the supplied bound.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidLimits`], [`Error::MalformedInput`], or
    /// [`Error::SizeLimit`] when the supplied values are not admissible.
    pub fn new(mut ids: Vec<CandidateId>, limits: Limits) -> Result<Self, Error> {
        limits.validate()?;
        if ids.iter().any(|id| !id.is_valid()) {
            return Err(Error::MalformedInput);
        }
        ids.sort_unstable();
        ids.dedup();
        if ids.len() > limits.max_tombstones {
            return Err(Error::SizeLimit);
        }
        Ok(Self(ids))
    }

    /// Returns canonical tombstone identities.
    #[must_use]
    pub fn ids(&self) -> &[CandidateId] {
        &self.0
    }

    pub(crate) fn contains(&self, id: CandidateId) -> bool {
        self.0.binary_search(&id).is_ok()
    }
}

/// Exact inputs that identify one selected vector query/materialization.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Binding {
    /// Atomic authoritative workspace root.
    pub workspace: WorkspaceRoot,
    /// Exact vector-facts relation root.
    pub root: Root,
    /// Immutable ANN recipe version.
    pub recipe: Recipe,
    /// Authority/capability revision.
    pub authority: Authority,
    /// Exact dependency/read manifest version.
    pub read_manifest: ReadManifest,
    /// Exact completed execution frontier for the derived vector view.
    pub frontier: Frontier,
}

impl Binding {
    /// Creates a complete query binding.
    #[must_use]
    pub fn new(
        workspace: WorkspaceRoot,
        root: Root,
        recipe: Recipe,
        authority: Authority,
        read_manifest: ReadManifest,
    ) -> Self {
        Self {
            workspace,
            root,
            recipe,
            authority,
            read_manifest,
            frontier: Frontier::from_value(&[0; 32]),
        }
    }

    /// Returns a binding with an explicit completed execution frontier.
    #[must_use]
    pub const fn with_frontier(mut self, frontier: Frontier) -> Self {
        self.frontier = frontier;
        self
    }

    /// Returns the exact completed execution frontier.
    #[must_use]
    pub const fn frontier(self) -> Frontier {
        self.frontier
    }
}
