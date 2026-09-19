//! Stable relation, schema, and input identities for the lexical extension.

use crate::Error;
use backend_semantic::EntityId;
use backend_version::{ObjectVersion, Relation, Schema, StateRoot, WorkspaceRoot};

/// Canonical relation represented by the lexical materialization.
#[derive(Debug, Eq, PartialEq)]
pub struct IndexRelation;

impl Relation for IndexRelation {
    const DOMAIN: u8 = 0x74;
    const TYPE: u16 = 1;
    type Key = EntityId;
    type Value = Vec<(String, String)>;

    fn encode_key(key: &Self::Key, out: &mut Vec<u8>) {
        out.extend_from_slice(key.as_bytes());
    }

    fn encode_value(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(&(value.len() as u64).to_be_bytes());
        for (field, text) in value {
            out.extend_from_slice(&(field.len() as u64).to_be_bytes());
            out.extend_from_slice(field.as_bytes());
            out.extend_from_slice(&(text.len() as u64).to_be_bytes());
            out.extend_from_slice(text.as_bytes());
        }
    }
}

/// Versioned visible document root.
pub type Root = StateRoot<IndexRelation>;

/// Versioned lexical recipe identity.
#[derive(Debug, Eq, PartialEq)]
pub struct RecipeSchema;

impl Schema for RecipeSchema {
    const DOMAIN: u8 = 0x74;
    const TYPE: u16 = 2;
    type Value = [u8; 32];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Versioned authority identity.
#[derive(Debug, Eq, PartialEq)]
pub struct AuthoritySchema;

impl Schema for AuthoritySchema {
    const DOMAIN: u8 = 0x74;
    const TYPE: u16 = 3;
    type Value = [u8; 32];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Versioned exact read/dependency manifest identity.
#[derive(Debug, Eq, PartialEq)]
pub struct ReadManifestSchema;

impl Schema for ReadManifestSchema {
    const DOMAIN: u8 = 0x74;
    const TYPE: u16 = 4;
    type Value = [u8];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Versioned query term set identity used to bind cursors.
#[derive(Debug, Eq, PartialEq)]
pub struct QuerySchema;

impl Schema for QuerySchema {
    const DOMAIN: u8 = 0x74;
    const TYPE: u16 = 5;
    type Value = [u8];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Schema marker for an execution frontier closed by a lexical result.
#[derive(Debug, Eq, PartialEq)]
pub struct FrontierSchema;

impl Schema for FrontierSchema {
    const DOMAIN: u8 = 0x74;
    const TYPE: u16 = 6;
    type Value = [u8; 32];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Lexical recipe version.
pub type Recipe = ObjectVersion<RecipeSchema>;
/// Authority version.
pub type Authority = ObjectVersion<AuthoritySchema>;
/// Exact read-manifest version.
pub type ReadManifest = ObjectVersion<ReadManifestSchema>;
/// Canonical query terms version.
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
    /// The current lexical page schema.
    pub const CURRENT: Self = Self {
        domain: IndexRelation::DOMAIN,
        type_id: IndexRelation::TYPE,
        version: IndexRelation::VERSION,
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

/// Bounded lexical materialization and query limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    /// Maximum documents admitted by one delta or rebuild work batch.
    pub max_delta_documents: usize,
    /// Maximum fields in one document.
    pub max_fields_per_document: usize,
    /// Maximum UTF-8 bytes in one field name or value.
    pub max_field_bytes: usize,
    /// Maximum text bytes admitted by one document or delta batch.
    pub max_total_text_bytes: usize,
    /// Maximum terms in one query.
    pub max_terms: usize,
    /// Maximum page size.
    pub max_page: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_delta_documents: 4096,
            max_fields_per_document: 128,
            max_field_bytes: 16 * 1024,
            max_total_text_bytes: 16 * 1024 * 1024,
            max_terms: 16,
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
        if self.max_delta_documents == 0
            || self.max_fields_per_document == 0
            || self.max_field_bytes == 0
            || self.max_total_text_bytes == 0
            || self.max_terms == 0
            || self.max_page == 0
        {
            Err(Error::InvalidLimits)
        } else {
            Ok(self)
        }
    }
}

/// Exact inputs that identify one lexical materialization.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Binding {
    /// Atomic authoritative workspace root.
    pub workspace: WorkspaceRoot,
    /// Exact visible document relation root.
    pub root: Root,
    /// Lexical recipe version.
    pub recipe: Recipe,
    /// Authority version.
    pub authority: Authority,
    /// Exact dependency/read-manifest version.
    pub read_manifest: ReadManifest,
    /// Exact completed execution frontier for the derived view.
    pub frontier: Frontier,
}

impl Binding {
    /// Creates a complete lexical binding.
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
