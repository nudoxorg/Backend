//! Stable source, declaration, authority, and provenance identities.

use crate::facets::VisibilityState;
use crate::schema::{
    AuthoritySchema, AuthorityValueSchema, EntitySchema, EntityValueSchema, SourceSchema,
    SourceValueSchema,
};
use crate::support::object_key;
use crate::{FacetCoverage, SemanticError};
use backend_version::{ObjectKey, ObjectVersion};

/// A source path identity. The identity is content independent and therefore
/// remains stable when source bytes change.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Source {
    workspace: u128,
    path: String,
}

impl Source {
    /// Admits a canonical workspace-relative source path.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::InvalidIdentity`] for an empty path or a path
    /// containing a NUL byte.
    pub fn new(workspace: u128, path: impl Into<String>) -> Result<Self, SemanticError> {
        let path = path.into();
        if path.is_empty() || path.as_bytes().contains(&0) {
            return Err(SemanticError::InvalidIdentity);
        }
        Ok(Self { workspace, path })
    }

    /// Returns the workspace namespace.
    #[must_use]
    pub const fn workspace(&self) -> u128 {
        self.workspace
    }

    /// Returns the canonical workspace-relative path.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }
}

/// Encoding of source bytes in the source value.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SourceEncoding {
    /// UTF-8 source.
    Utf8,
    /// UTF-16 source whose byte contract is supplied by the authority.
    Utf16,
    /// Opaque bytes.
    Binary,
}

pub(crate) fn source_encoding_tag(value: SourceEncoding) -> u8 {
    match value {
        SourceEncoding::Utf8 => 1,
        SourceEncoding::Utf16 => 2,
        SourceEncoding::Binary => 3,
    }
}

/// Complete source bytes and their source revision metadata.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SourceValue {
    bytes: Vec<u8>,
    encoding: SourceEncoding,
    revision: u64,
}

impl SourceValue {
    /// Admits source bytes under an explicit encoding contract.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::InvalidValue`] when UTF-8 is invalid or a
    /// UTF-16 payload does not contain complete code units.
    pub fn new(
        bytes: Vec<u8>,
        encoding: SourceEncoding,
        revision: u64,
    ) -> Result<Self, SemanticError> {
        match encoding {
            SourceEncoding::Utf8 if std::str::from_utf8(&bytes).is_err() => {
                return Err(SemanticError::InvalidValue);
            }
            SourceEncoding::Utf16 if !bytes.len().is_multiple_of(2) => {
                return Err(SemanticError::InvalidValue);
            }
            SourceEncoding::Utf8 | SourceEncoding::Utf16 | SourceEncoding::Binary => {}
        }
        Ok(Self {
            bytes,
            encoding,
            revision,
        })
    }

    /// Returns the exact source bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the source encoding contract.
    #[must_use]
    pub const fn encoding(&self) -> SourceEncoding {
        self.encoding
    }

    /// Returns the authority-owned source revision.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }
}

/// A source relation value, including coverage and provenance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceRecord {
    identity: Source,
    value: Option<SourceValue>,
    coverage: FacetCoverage,
    provenance: Provenance,
}

impl SourceRecord {
    /// Admits a source relation row with a self-consistent value state and
    /// source provenance.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::InvalidCoverageState`] when the value does
    /// not agree with the explicit coverage state, or
    /// [`SemanticError::InvalidProvenance`] when provenance names another
    /// source or value version.
    pub fn new(
        identity: Source,
        value: Option<SourceValue>,
        coverage: FacetCoverage,
        provenance: Provenance,
    ) -> Result<Self, SemanticError> {
        validate_value_state(value.is_some(), coverage)?;
        let source_id = ObjectKey::<SourceSchema>::from_value(&identity);
        if provenance.source() != source_id {
            return Err(SemanticError::InvalidProvenance);
        }
        if let Some(source) = &value {
            let version = ObjectVersion::<SourceValueSchema>::from_value(source);
            if provenance.source_version() != version {
                return Err(SemanticError::InvalidProvenance);
            }
        }
        Ok(Self {
            identity,
            value,
            coverage,
            provenance,
        })
    }

    /// Returns the source identity.
    #[must_use]
    pub const fn identity(&self) -> &Source {
        &self.identity
    }

    /// Returns the source value, when live.
    #[must_use]
    pub fn value(&self) -> Option<&SourceValue> {
        self.value.as_ref()
    }

    /// Returns the checked coverage/state.
    #[must_use]
    pub const fn coverage(&self) -> FacetCoverage {
        self.coverage
    }

    /// Returns the bound provenance.
    #[must_use]
    pub const fn provenance(&self) -> Provenance {
        self.provenance
    }
}

/// Stable source logical key.
pub type SourceId = ObjectKey<SourceSchema>;
/// Stable source content version.
pub type SourceVersion = ObjectVersion<SourceValueSchema>;

/// A half-open source byte span.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SourceSpan {
    source: Source,
    start: u32,
    end: u32,
}

impl SourceSpan {
    /// Constructs a valid half-open span.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::InvalidSpan`] when `start` is greater than
    /// `end`.
    pub fn new(source: Source, start: u32, end: u32) -> Result<Self, SemanticError> {
        if start > end {
            Err(SemanticError::InvalidSpan)
        } else {
            Ok(Self { source, start, end })
        }
    }

    /// Returns the source identity containing the span.
    #[must_use]
    pub const fn source(&self) -> &Source {
        &self.source
    }

    /// Returns the inclusive start byte offset.
    #[must_use]
    pub const fn start(&self) -> u32 {
        self.start
    }

    /// Returns the exclusive end byte offset.
    #[must_use]
    pub const fn end(&self) -> u32 {
        self.end
    }
}

/// Stable declaration identity established by an authority.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Entity {
    source: Source,
    local_key: String,
    span: Option<SourceSpan>,
}

impl Entity {
    /// Admits a declaration identity and optional source evidence.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::InvalidIdentity`] for an empty local key or
    /// when a supplied span belongs to another source.
    pub fn new(
        source: Source,
        local_key: impl Into<String>,
        span: Option<SourceSpan>,
    ) -> Result<Self, SemanticError> {
        let local_key = local_key.into();
        if local_key.is_empty()
            || span
                .as_ref()
                .is_some_and(|candidate| candidate.source() != &source)
        {
            return Err(SemanticError::InvalidIdentity);
        }
        Ok(Self {
            source,
            local_key,
            span,
        })
    }

    /// Returns the source identity.
    #[must_use]
    pub const fn source(&self) -> &Source {
        &self.source
    }

    /// Returns the authority-scoped local identity.
    #[must_use]
    pub fn local_key(&self) -> &str {
        &self.local_key
    }

    /// Returns optional source evidence.
    #[must_use]
    pub const fn span(&self) -> Option<&SourceSpan> {
        self.span.as_ref()
    }
}

/// Declaration kind carried by the entity header facet.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum EntityKind {
    /// Module/package namespace.
    Module,
    /// Type/class/struct declaration.
    Type,
    /// Function/method declaration.
    Function,
    /// Field/property declaration.
    Field,
    /// Variable/constant declaration.
    Variable,
    /// Parameter declaration.
    Parameter,
    /// Unknown authority-specific kind.
    Other,
}

pub(crate) fn entity_kind_tag(value: EntityKind) -> u8 {
    match value {
        EntityKind::Module => 1,
        EntityKind::Type => 2,
        EntityKind::Function => 3,
        EntityKind::Field => 4,
        EntityKind::Variable => 5,
        EntityKind::Parameter => 6,
        EntityKind::Other => 7,
    }
}

/// Parent/containment observation that keeps unknown distinct from root.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Parent {
    /// A logical parent declaration.
    Parent(EntityId),
    /// An explicitly observed root declaration.
    Root,
    /// The authority observed the declaration but does not represent it.
    Unrepresented,
    /// Parentage was unavailable.
    Unavailable,
}

pub(crate) fn encode_parent(out: &mut Vec<u8>, value: &Parent) {
    match value {
        Parent::Parent(key) => {
            out.push(1);
            object_key(out, key);
        }
        Parent::Root => out.push(2),
        Parent::Unrepresented => out.push(3),
        Parent::Unavailable => out.push(4),
    }
}

/// Complete declaration header and containment value.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct EntityValue {
    name: Option<String>,
    kind: EntityKind,
    visibility: VisibilityState,
    parent: Parent,
}

impl EntityValue {
    /// Creates a complete declaration header value.
    #[must_use]
    pub fn new(
        name: Option<String>,
        kind: EntityKind,
        visibility: VisibilityState,
        parent: Parent,
    ) -> Self {
        Self {
            name,
            kind,
            visibility,
            parent,
        }
    }

    /// Returns the canonical declaration name, when captured.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Returns the declaration kind.
    #[must_use]
    pub const fn kind(&self) -> EntityKind {
        self.kind
    }

    /// Returns the exact visibility state.
    #[must_use]
    pub const fn visibility(&self) -> VisibilityState {
        self.visibility
    }

    /// Returns the parent/containment state.
    #[must_use]
    pub const fn parent(&self) -> &Parent {
        &self.parent
    }
}

/// Entity relation value with authority evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EntityRecord {
    identity: Entity,
    value: Option<EntityValue>,
    coverage: FacetCoverage,
    provenance: Provenance,
}

impl EntityRecord {
    /// Admits an entity row with matching source provenance and value state.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::InvalidCoverageState`] when the value does
    /// not agree with coverage, or [`SemanticError::InvalidProvenance`] when
    /// provenance names another source.
    pub fn new(
        identity: Entity,
        value: Option<EntityValue>,
        coverage: FacetCoverage,
        provenance: Provenance,
    ) -> Result<Self, SemanticError> {
        validate_value_state(value.is_some(), coverage)?;
        let source_id = ObjectKey::<SourceSchema>::from_value(identity.source());
        if provenance.source() != source_id {
            return Err(SemanticError::InvalidProvenance);
        }
        Ok(Self {
            identity,
            value,
            coverage,
            provenance,
        })
    }

    /// Returns the entity identity.
    #[must_use]
    pub const fn identity(&self) -> &Entity {
        &self.identity
    }

    /// Returns the entity value, when live.
    #[must_use]
    pub fn value(&self) -> Option<&EntityValue> {
        self.value.as_ref()
    }

    /// Returns the checked coverage/state.
    #[must_use]
    pub const fn coverage(&self) -> FacetCoverage {
        self.coverage
    }

    /// Returns the bound provenance.
    #[must_use]
    pub const fn provenance(&self) -> Provenance {
        self.provenance
    }
}

/// Stable entity logical key.
pub type EntityId = ObjectKey<EntitySchema>;
/// Complete entity value version, independent from [`EntityId`].
pub type EntityVersion = ObjectVersion<EntityValueSchema>;

/// Authority identity used by provenance and replacement scopes.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Authority {
    namespace: String,
    capability: String,
}

impl Authority {
    /// Admits a non-empty authority namespace and capability.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::InvalidIdentity`] when either component is
    /// empty.
    pub fn new(
        namespace: impl Into<String>,
        capability: impl Into<String>,
    ) -> Result<Self, SemanticError> {
        let namespace = namespace.into();
        let capability = capability.into();
        if namespace.is_empty() || capability.is_empty() {
            return Err(SemanticError::InvalidIdentity);
        }
        Ok(Self {
            namespace,
            capability,
        })
    }

    /// Returns the stable authority namespace.
    #[must_use]
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// Returns the capability/protocol name.
    #[must_use]
    pub fn capability(&self) -> &str {
        &self.capability
    }
}

/// Authority revision and immutable basis value.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct AuthorityValue {
    identity: Authority,
    revision: u64,
    basis: Vec<u8>,
}

impl AuthorityValue {
    /// Creates an immutable authority revision value.
    #[must_use]
    pub fn new(identity: Authority, revision: u64, basis: Vec<u8>) -> Self {
        Self {
            identity,
            revision,
            basis,
        }
    }

    /// Returns the authority identity.
    #[must_use]
    pub const fn identity(&self) -> &Authority {
        &self.identity
    }

    /// Returns the authority-owned revision.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Returns canonical profile/toolchain basis bytes.
    #[must_use]
    pub fn basis(&self) -> &[u8] {
        &self.basis
    }
}

/// Stable authority key.
pub type AuthorityId = ObjectKey<AuthoritySchema>;
/// Authority revision value version.
pub type AuthorityVersion = ObjectVersion<AuthorityValueSchema>;

/// Immutable source and authority provenance attached to an admitted fact.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Provenance {
    authority: AuthorityId,
    authority_version: AuthorityVersion,
    source: SourceId,
    source_version: SourceVersion,
}

impl Provenance {
    /// Constructs provenance from the concrete identities and values it
    /// binds, preventing cross-authority/source version mixing.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::InvalidProvenance`] when the authority value
    /// names another authority.
    pub fn new(
        authority: Authority,
        authority_value: AuthorityValue,
        source: Source,
        source_value: SourceValue,
    ) -> Result<Self, SemanticError> {
        if authority_value.identity() != &authority {
            return Err(SemanticError::InvalidProvenance);
        }
        let result = Self {
            authority: ObjectKey::from_value(&authority),
            authority_version: ObjectVersion::from_value(&authority_value),
            source: ObjectKey::from_value(&source),
            source_version: ObjectVersion::from_value(&source_value),
        };
        drop((authority, authority_value, source, source_value));
        Ok(result)
    }

    /// Returns the producing authority identity.
    #[must_use]
    pub const fn authority(&self) -> AuthorityId {
        self.authority
    }

    /// Returns the producing authority revision version.
    #[must_use]
    pub const fn authority_version(&self) -> AuthorityVersion {
        self.authority_version
    }

    /// Returns the source identity read by the authority.
    #[must_use]
    pub const fn source(&self) -> SourceId {
        self.source
    }

    /// Returns the exact source value version read by the authority.
    #[must_use]
    pub const fn source_version(&self) -> SourceVersion {
        self.source_version
    }
}

pub(crate) fn validate_value_state(
    has_value: bool,
    coverage: FacetCoverage,
) -> Result<(), SemanticError> {
    if has_value && coverage.deletion() == crate::Deletion::Live {
        return Ok(());
    }
    if !has_value && coverage.deletion() != crate::Deletion::Live && coverage.is_complete() {
        return Ok(());
    }
    Err(SemanticError::InvalidCoverageState)
}
