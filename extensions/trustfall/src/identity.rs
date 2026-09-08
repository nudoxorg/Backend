//! Stable relation, schema, and input identities for the graph extension.

use crate::Error;
use backend_version::{ObjectVersion, Relation, Schema, StateRoot, WorkspaceRoot};

/// Relation identity for semantic graph rows.
#[derive(Debug, Eq, PartialEq)]
pub struct SemanticRelation;

impl Relation for SemanticRelation {
    const DOMAIN: u8 = 0x75;
    const TYPE: u16 = 1;
    type Key = u64;
    type Value = Vec<String>;

    fn encode_key(key: &Self::Key, out: &mut Vec<u8>) {
        out.extend_from_slice(&key.to_be_bytes());
    }

    fn encode_value(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(&(value.len() as u64).to_be_bytes());
        for field in value {
            out.extend_from_slice(&(field.len() as u64).to_be_bytes());
            out.extend_from_slice(field.as_bytes());
        }
    }
}

/// Canonical semantic graph relation root.
pub type Root = StateRoot<SemanticRelation>;

/// Schema marker for a graph-query recipe.
#[derive(Debug, Eq, PartialEq)]
pub struct RecipeSchema;

impl Schema for RecipeSchema {
    const DOMAIN: u8 = 0x75;
    const TYPE: u16 = 2;
    type Value = [u8; 32];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Schema marker for a semantic authority revision.
#[derive(Debug, Eq, PartialEq)]
pub struct AuthoritySchema;

impl Schema for AuthoritySchema {
    const DOMAIN: u8 = 0x75;
    const TYPE: u16 = 3;
    type Value = [u8; 32];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Schema marker for an exact graph read manifest.
#[derive(Debug, Eq, PartialEq)]
pub struct ReadManifestSchema;

impl Schema for ReadManifestSchema {
    const DOMAIN: u8 = 0x75;
    const TYPE: u16 = 4;
    type Value = [u8];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Schema marker for a canonical graph query/read-set identity.
#[derive(Debug, Eq, PartialEq)]
pub struct QuerySchema;

impl Schema for QuerySchema {
    const DOMAIN: u8 = 0x75;
    const TYPE: u16 = 5;
    type Value = [u8];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Schema marker for an execution frontier closed by a graph result.
#[derive(Debug, Eq, PartialEq)]
pub struct FrontierSchema;

impl Schema for FrontierSchema {
    const DOMAIN: u8 = 0x75;
    const TYPE: u16 = 6;
    type Value = [u8; 32];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Graph-query recipe version.
pub type Recipe = ObjectVersion<RecipeSchema>;
/// Semantic authority version.
pub type Authority = ObjectVersion<AuthoritySchema>;
/// Exact read-manifest version.
pub type ReadManifest = ObjectVersion<ReadManifestSchema>;
/// Canonical graph query/read-set version.
pub type QueryVersion = ObjectVersion<QuerySchema>;
/// Exact completed execution frontier identity.
pub type Frontier = ObjectVersion<FrontierSchema>;

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
    /// The current graph page schema.
    pub const CURRENT: Self = Self {
        domain: SemanticRelation::DOMAIN,
        type_id: SemanticRelation::TYPE,
        version: SemanticRelation::VERSION,
    };

    /// Creates a schema tag received from a source.
    #[must_use]
    pub const fn new(domain: u8, type_id: u16, version: u8) -> Self {
        Self {
            domain,
            type_id,
            version,
        }
    }
}

/// Bounded graph projection limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    /// Maximum graph rows in one state/result.
    pub max_rows: usize,
    /// Maximum scalar fields per row.
    pub max_fields_per_row: usize,
    /// Maximum UTF-8 bytes per scalar field.
    pub max_field_bytes: usize,
    /// Maximum total bytes in a state/result.
    pub max_total_bytes: usize,
    /// Maximum declared reads.
    pub max_reads: usize,
    /// Maximum page size.
    pub max_page: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_rows: 4096,
            max_fields_per_row: 128,
            max_field_bytes: 16 * 1024,
            max_total_bytes: 16 * 1024 * 1024,
            max_reads: 256,
            max_page: 256,
        }
    }
}

impl Limits {
    /// Validates limits before they drive allocation or source work.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidLimits`] for zero or inconsistent limits.
    pub const fn validate(self) -> Result<Self, Error> {
        if self.max_rows == 0
            || self.max_fields_per_row == 0
            || self.max_field_bytes == 0
            || self.max_total_bytes == 0
            || self.max_reads == 0
            || self.max_page == 0
            || self.max_page > self.max_rows
        {
            Err(Error::InvalidLimits)
        } else {
            Ok(self)
        }
    }
}

/// Exact graph-query inputs shared by source, cursor, and result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Binding {
    /// Atomic authoritative workspace root.
    pub workspace: WorkspaceRoot,
    /// Exact semantic relation root.
    pub root: Root,
    /// Graph recipe version.
    pub recipe: Recipe,
    /// Semantic authority version.
    pub authority: Authority,
    /// Exact read/dependency manifest version.
    pub read_manifest: ReadManifest,
    /// Exact completed execution frontier for the derived view.
    pub frontier: Frontier,
}

impl Binding {
    /// Creates a complete graph-query binding.
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
