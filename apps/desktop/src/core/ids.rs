//! Stable product identities used by the desktop boundary.
//!
//! The service has several identity families.  Keeping them as distinct
//! newtypes makes it impossible for a package coordinate to accidentally be
//! used as a local project path or for a view route to smuggle an arbitrary
//! string through the UI.

use std::fmt;
use std::path::Path;
use std::sync::Arc;

/// A non-empty, validated local workspace identity.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LocalProjectId {
    coordinate: Arc<str>,
    key: backend_library::PackageKey,
}

/// A non-empty, validated package coordinate.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PackageId {
    reference: backend_library::PackageReference,
    key: backend_library::PackageKey,
}

/// A project identity supplied by the service, wrapping the canonical library
/// project ID rather than recreating a desktop string key.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ProjectId(backend_library::ProjectId);

/// A stable identity for a document opened in the viewport.
///
/// The document symbol and the immutable view root come from the producer;
/// the desktop shell never invents a document number.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DocumentId {
    symbol: backend_library::SymbolKey,
    root: backend_library::ViewStateRoot,
}

/// A stable identity for source and document rows, wrapping the producer's
/// canonical row identity.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RowId(backend_library::RowId);

/// Error returned when an identity would be empty or contain a control value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IdentityError {
    /// The spelling had no meaningful characters.
    Empty,
    /// The spelling contained a newline or NUL and cannot be persisted safely.
    ControlCharacter,
    /// The spelling did not satisfy the backend package admission grammar.
    Invalid,
}

impl fmt::Display for IdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("identity must not be empty"),
            Self::ControlCharacter => f.write_str("identity contains a control character"),
            Self::Invalid => f.write_str("identity is not a valid package reference"),
        }
    }
}

impl std::error::Error for IdentityError {}

fn validate(value: &str) -> Result<Arc<str>, IdentityError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(IdentityError::Empty);
    }
    if value.chars().any(char::is_control) {
        return Err(IdentityError::ControlCharacter);
    }
    Ok(Arc::<str>::from(value))
}

impl LocalProjectId {
    /// Creates a local identity from a path spelling.
    pub fn from_path(path: &Path) -> Result<Self, IdentityError> {
        Self::new(path.to_string_lossy().as_ref())
    }

    /// Creates a local identity from an already selected spelling.
    pub fn new(value: &str) -> Result<Self, IdentityError> {
        validate(value).map(|coordinate| Self {
            key: backend_library::package_key(&coordinate),
            coordinate,
        })
    }

    /// Returns the persisted spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.coordinate
    }

    /// Returns the producer-compatible canonical package key.
    #[must_use]
    pub const fn key(&self) -> backend_library::PackageKey {
        self.key
    }
}

impl PackageId {
    /// Wraps a package reference already admitted by the backend boundary.
    #[must_use]
    pub fn from_backend(reference: backend_library::PackageReference) -> Self {
        let key = backend_library::package_key(reference.as_str());
        Self { reference, key }
    }

    /// Admits a producer package coordinate only when its complete spelling
    /// hashes to the separately transported stable key.
    ///
    /// # Errors
    /// Returns [`IdentityError::Invalid`] when the coordinate is malformed or
    /// when a display label is presented as the preimage for another package.
    pub fn try_from_backend(
        key: backend_library::PackageKey,
        coordinate: &str,
    ) -> Result<Self, IdentityError> {
        let reference = backend_library::PackageReference::parse(coordinate)
            .map_err(|_| IdentityError::Invalid)?;
        if backend_library::package_key(reference.as_str()) != key {
            return Err(IdentityError::Invalid);
        }
        Ok(Self { reference, key })
    }

    /// Creates a package identity from a canonical coordinate.
    pub fn new(value: &str) -> Result<Self, IdentityError> {
        let coordinate = validate(value)?;
        let reference = backend_library::PackageReference::parse(coordinate.as_ref())
            .map_err(|_| IdentityError::Invalid)?;
        Ok(Self::from_backend(reference))
    }

    /// Returns the persisted spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.reference.as_str()
    }

    /// Returns the canonical producer package key.
    #[must_use]
    pub const fn key(&self) -> backend_library::PackageKey {
        self.key
    }
}

impl ProjectId {
    /// Wraps a producer-issued project identity.
    #[must_use]
    pub const fn from_backend(value: backend_library::ProjectId) -> Self {
        Self(value)
    }

    /// Returns the producer's stable numeric identity.
    #[must_use]
    pub const fn get(&self) -> backend_library::ProjectId {
        self.0
    }

    /// Creates a deterministic project identity for unit tests only.
    #[must_use]
    #[cfg(test)]
    pub fn test(value: u64) -> Option<Self> {
        std::num::NonZeroU64::new(value)
            .map(|value| Self::from_backend(backend_library::ProjectId::new(value)))
    }
}

impl DocumentId {
    /// Creates a document identity from the producer's symbol and root.
    #[must_use]
    pub const fn from_backend(
        symbol: backend_library::SymbolKey,
        root: backend_library::ViewStateRoot,
    ) -> Self {
        Self { symbol, root }
    }

    /// Returns the producer's symbol identity.
    #[must_use]
    pub const fn symbol(self) -> backend_library::SymbolKey {
        self.symbol
    }

    /// Returns the exact immutable root that qualified this document.
    #[must_use]
    pub const fn root(self) -> backend_library::ViewStateRoot {
        self.root
    }

    /// Creates a deterministic document identity for unit tests only.
    #[must_use]
    #[cfg(test)]
    pub fn test(value: u64) -> Self {
        Self::from_backend(
            backend_library::symbol_key(&format!("test::document::{value}")),
            backend_library::view_state_root(&[("test".to_owned(), value.to_string())]),
        )
    }
}

impl RowId {
    /// Wraps a canonical producer row identity.
    #[must_use]
    pub const fn from_backend(value: backend_library::RowId) -> Self {
        Self(value)
    }

    /// Returns the canonical producer row identity.
    #[must_use]
    pub const fn backend(self) -> backend_library::RowId {
        self.0
    }

    /// Creates a deterministic row identity for unit tests only.
    #[must_use]
    #[cfg(test)]
    pub fn test(value: u64) -> Self {
        Self::from_backend(backend_library::RowId::Object(
            backend_library::object_version(&value.to_be_bytes()),
        ))
    }
}

impl fmt::Display for LocalProjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.coordinate)
    }
}

impl fmt::Display for PackageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.reference.as_str())
    }
}

impl fmt::Display for ProjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.get().fmt(f)
    }
}

impl fmt::Display for DocumentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}@{}",
            backend_library::encode_id(self.symbol.as_bytes()),
            hex_digest(self.root.as_bytes())
        )
    }
}

impl fmt::Display for RowId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A producer-owned identity that may be selected by a route.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ResourceIdentity {
    /// A filesystem-backed workspace.
    Local(LocalProjectId),
    /// A package coordinate from a registry.
    Package(PackageId),
    /// A service-owned project.
    Project(ProjectId),
}

/// A versioned state root. The producer root digest plus epoch/generation bind
/// authority; the observation is UI metadata and is never used to admit a
/// result.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct VersionedRoot {
    /// The certified immutable view root.
    pub root: backend_library::ViewStateRoot,
    /// Producer epoch that issued this root.
    pub producer_epoch: u64,
    /// Producer generation within the epoch.
    pub generation: u64,
    /// UI observation sequence. This is diagnostic metadata only.
    pub observation: u64,
}

impl VersionedRoot {
    /// Creates a root key at a producer epoch. Generation and observation are
    /// initially zero and can only be advanced by a runtime adapter.
    #[must_use]
    pub const fn new(root: backend_library::ViewStateRoot, producer_epoch: u64) -> Self {
        Self {
            root,
            producer_epoch,
            generation: 0,
            observation: 0,
        }
    }

    /// Binds a producer generation to this root.
    #[must_use]
    pub const fn with_generation(mut self, generation: u64) -> Self {
        self.generation = generation;
        self
    }

    /// Adds a UI observation sequence without changing producer authority.
    #[must_use]
    pub const fn observed_at(mut self, observation: u64) -> Self {
        self.observation = observation;
        self
    }

    /// Returns whether two keys name the same producer epoch/generation.
    #[must_use]
    pub const fn same_producer_generation(self, other: Self) -> bool {
        self.producer_epoch == other.producer_epoch && self.generation == other.generation
    }

    /// Returns whether two keys describe the same producer authority. The UI
    /// observation counter is intentionally ignored.
    #[must_use]
    pub fn same_authority(self, other: Self) -> bool {
        self.root == other.root && self.same_producer_generation(other)
    }

    /// Returns whether this key is older in producer order. Observation order
    /// is never consulted for admission.
    #[must_use]
    pub const fn is_older_authority(self, other: Self) -> bool {
        self.producer_epoch < other.producer_epoch
            || (self.producer_epoch == other.producer_epoch && self.generation < other.generation)
    }
}

impl fmt::Display for VersionedRoot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}@{}:{}:{}",
            hex_digest(self.root.as_bytes()),
            self.producer_epoch,
            self.generation,
            self.observation
        )
    }
}

fn hex_digest(bytes: &[u8; 32]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use fmt::Write as _;
        let _ = write!(text, "{byte:02x}");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(test)]
    fn test_root() -> backend_library::ViewStateRoot {
        backend_library::view_state_root(&[("test".to_owned(), "root".to_owned())])
    }

    #[test]
    fn document_and_row_ids_wrap_backend_values() {
        let symbol = backend_library::symbol_key("pkg::Thing");
        let document = DocumentId::from_backend(symbol, test_root());
        assert_eq!(document.symbol(), symbol);
        assert_eq!(document.root(), test_root());
        let row = RowId::from_backend(backend_library::RowId::Symbol(symbol));
        assert_eq!(row.backend(), backend_library::RowId::Symbol(symbol));
    }

    #[test]
    fn identity_families_do_not_accept_empty_values() {
        assert_eq!(PackageId::new(" "), Err(IdentityError::Empty));
        assert!(LocalProjectId::new("workspace").is_ok());
        assert_eq!(ProjectId::test(0), None);
        assert!(ProjectId::test(1).is_some());
    }

    #[test]
    fn package_identity_rejects_a_display_label_for_another_key() {
        let key = backend_library::package_key("pkg:cargo/serde@1.0.228");
        let admitted = PackageId::try_from_backend(key, "pkg:cargo/serde@1.0.228")
            .expect("matching package preimage");
        assert_eq!(admitted.key(), key);
        assert_eq!(
            PackageId::try_from_backend(key, "serde"),
            Err(IdentityError::Invalid)
        );
    }
}
