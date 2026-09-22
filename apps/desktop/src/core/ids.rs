//! Stable product identities used by the desktop boundary.
//!
//! The service has several identity families.  Keeping them as distinct
//! newtypes makes it impossible for a package coordinate to accidentally be
//! used as a local project path or for a view route to smuggle an arbitrary
//! string through the UI.

use std::fmt;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use backend_platform::NativePath;

/// A non-empty, validated local workspace identity.
#[derive(Clone, Debug)]
pub struct LocalProjectId {
    native: NativePath,
    coordinate: Arc<str>,
    key: backend_library::SemanticObject,
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
    /// The native path cannot be represented by the persisted UTF-8 identity
    /// contract without lossy conversion.
    UnsupportedPlatformEncoding,
}

impl fmt::Display for IdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("identity must not be empty"),
            Self::ControlCharacter => f.write_str("identity contains a control character"),
            Self::Invalid => f.write_str("identity is not a valid package reference"),
            Self::UnsupportedPlatformEncoding => {
                f.write_str("the selected path uses an unsupported platform encoding")
            }
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
    /// Creates a local identity from native path units.
    pub fn from_path(path: &Path) -> Result<Self, IdentityError> {
        let native = NativePath::from_path(path).map_err(map_native_path_error)?;
        Ok(Self::from_native(native))
    }

    /// Creates a local identity from an already selected spelling.
    pub fn new(value: &str) -> Result<Self, IdentityError> {
        let coordinate = validate(value)?;
        let native =
            NativePath::from_path(Path::new(coordinate.as_ref())).map_err(map_native_path_error)?;
        Ok(Self::from_native_with_coordinate(native, coordinate))
    }

    fn from_native(native: NativePath) -> Self {
        let coordinate = native
            .to_str()
            .map(Arc::<str>::from)
            .unwrap_or_else(|_| Arc::from("<native path>"));
        Self::from_native_with_coordinate(native, coordinate)
    }

    fn from_native_with_coordinate(native: NativePath, coordinate: Arc<str>) -> Self {
        let key = backend_library::object_version(&native.key().as_bytes());
        Self {
            key,
            native,
            coordinate,
        }
    }

    /// Returns the persisted spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.coordinate
    }

    /// Returns the exact native path.
    #[must_use]
    pub fn path(&self) -> PathBuf {
        self.native.as_path().to_path_buf()
    }

    /// Returns the exact shared native path value for typed owner/client
    /// boundaries. Callers must not lower this to UTF-8 for indexing.
    #[must_use]
    pub fn native_path(&self) -> &NativePath {
        &self.native
    }

    /// Returns a presentation-only spelling for labels and diagnostics.
    /// Identity and service admission continue to use [`Self::native_path`].
    #[must_use]
    pub fn display_lossy(&self) -> String {
        self.native.display_lossy()
    }

    /// Returns the reversible native identity used by durable state.
    #[must_use]
    pub fn native_wire(&self) -> Result<backend_platform::NativePathWire, IdentityError> {
        self.native.to_wire().map_err(map_native_path_error)
    }

    /// Returns whether the identity has display text that is also a lossless
    /// UTF-8 service argument.
    #[must_use]
    pub fn has_utf8_spelling(&self) -> bool {
        self.native.to_str().is_ok()
    }

    /// Returns the exact UTF-8 coordinate accepted by the legacy service
    /// index command. The native path remains the authority; callers must
    /// handle this error instead of substituting the display spelling.
    pub fn service_coordinate(&self) -> Result<&str, IdentityError> {
        self.native
            .to_str()
            .map_err(|_| IdentityError::UnsupportedPlatformEncoding)
    }

    /// Returns the stable native-unit key.
    #[must_use]
    pub const fn key(&self) -> backend_library::SemanticObject {
        self.key
    }

    /// Restores a local identity from the shared native persistence value.
    pub fn from_native_wire(
        value: &backend_platform::NativePathWire,
    ) -> Result<Self, IdentityError> {
        let native = NativePath::from_wire(value).map_err(map_native_path_error)?;
        Ok(Self::from_native(native))
    }
}

fn map_native_path_error(error: backend_platform::NativePathError) -> IdentityError {
    match error {
        backend_platform::NativePathError::Empty => IdentityError::Empty,
        backend_platform::NativePathError::Nul => IdentityError::ControlCharacter,
        _ => IdentityError::UnsupportedPlatformEncoding,
    }
}

impl PartialEq for LocalProjectId {
    fn eq(&self, other: &Self) -> bool {
        self.native.key() == other.native.key()
    }
}

impl Eq for LocalProjectId {}

impl Hash for LocalProjectId {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.native.key().hash(state);
    }
}

impl PartialOrd for LocalProjectId {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for LocalProjectId {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.native.key().cmp(&other.native.key())
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

/// The producer-issued authority for one immutable view root.
///
/// The fields are private so the desktop cannot pair a root with a fabricated
/// cursor or advance producer order locally. A value enters the shell only
/// through [`VersionedRoot::from_revision`] at the service adapter boundary.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ProducerAuthority {
    root: backend_library::ViewStateRoot,
    producer_epoch: u64,
    cursor: backend_library::Cursor,
}

impl ProducerAuthority {
    /// Returns the certified immutable view root.
    #[must_use]
    pub const fn root(self) -> backend_library::ViewStateRoot {
        self.root
    }

    /// Returns the producer epoch that issued this cursor.
    #[must_use]
    pub const fn producer_epoch(self) -> u64 {
        self.producer_epoch
    }

    /// Returns the complete producer-issued cursor.
    #[must_use]
    pub const fn cursor(self) -> backend_library::Cursor {
        self.cursor
    }

    /// Returns the producer sequence carried by the cursor.
    #[must_use]
    pub const fn generation(self) -> u64 {
        self.cursor.sequence()
    }
}

/// A versioned state root. The producer root, epoch, and cursor are one
/// admitted authority; observation is UI metadata and is never used to admit
/// a result.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct VersionedRoot {
    authority: ProducerAuthority,
    /// UI observation sequence. This is diagnostic metadata only.
    observation: u64,
}

impl VersionedRoot {
    /// Creates a root key from the producer's exact revision cursor.
    ///
    /// The cursor sequence is the daemon-issued producer order. The desktop
    /// never increments it or turns a wall-clock observation into authority.
    #[must_use]
    pub const fn from_revision(
        producer_epoch: u64,
        cursor: backend_library::Cursor,
        observation: u64,
    ) -> Self {
        Self {
            authority: ProducerAuthority {
                root: cursor.root(),
                producer_epoch,
                cursor,
            },
            observation,
        }
    }

    /// Creates a synthetic authority for reducer/model fixtures.
    ///
    /// Production code must use [`Self::from_revision`]. Keeping this
    /// constructor test-only prevents a fixture generation from becoming a
    /// served workspace authority.
    #[must_use]
    #[cfg(test)]
    pub fn synthetic(root: backend_library::ViewStateRoot, producer_epoch: u64) -> Self {
        Self {
            authority: ProducerAuthority {
                root,
                producer_epoch,
                cursor: backend_library::Cursor::at(root, 0),
            },
            observation: 0,
        }
    }

    /// Binds a synthetic fixture generation. Production code cannot call this
    /// constructor because it is compiled only for the test support surface.
    #[must_use]
    #[cfg(test)]
    pub fn with_generation(mut self, generation: u64) -> Self {
        self.authority.cursor = backend_library::Cursor::at(self.root(), generation);
        self
    }

    /// Returns the complete producer authority.
    #[must_use]
    pub const fn authority(self) -> ProducerAuthority {
        self.authority
    }

    /// Returns the certified immutable view root.
    #[must_use]
    pub const fn root(self) -> backend_library::ViewStateRoot {
        self.authority.root()
    }

    /// Returns the producer epoch that issued this root.
    #[must_use]
    pub const fn producer_epoch(self) -> u64 {
        self.authority.producer_epoch()
    }

    /// Returns the producer generation carried by the admitted cursor.
    #[must_use]
    pub const fn generation(self) -> u64 {
        self.authority.generation()
    }

    /// Returns the UI observation sequence carried alongside the authority.
    #[must_use]
    pub const fn observation(self) -> u64 {
        self.observation
    }

    /// Returns the exact producer cursor that issued this root.
    #[must_use]
    pub const fn revision(self) -> backend_library::Cursor {
        self.authority.cursor()
    }

    /// Adds a UI observation sequence without changing producer authority.
    #[must_use]
    #[cfg(test)]
    pub const fn observed_at(mut self, observation: u64) -> Self {
        self.observation = observation;
        self
    }

    /// Returns whether two keys name the same producer epoch/generation.
    #[must_use]
    pub const fn same_producer_generation(self, other: Self) -> bool {
        self.producer_epoch() == other.producer_epoch() && self.revision() == other.revision()
    }

    /// Returns whether two keys describe the same producer authority. The UI
    /// observation counter is intentionally ignored.
    #[must_use]
    pub fn same_authority(self, other: Self) -> bool {
        self.authority == other.authority
    }

    /// Returns whether this key is older in producer order. Observation order
    /// is never consulted for admission.
    #[must_use]
    pub const fn is_older_authority(self, other: Self) -> bool {
        if self.producer_epoch() == other.producer_epoch() {
            return self.revision() < other.revision();
        }
        self.producer_epoch() < other.producer_epoch()
    }
}

impl fmt::Display for VersionedRoot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}@{}:{}:{}",
            hex_digest(self.root().as_bytes()),
            self.producer_epoch(),
            self.generation(),
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

    #[cfg(unix)]
    #[test]
    fn local_project_identity_retains_non_utf8_native_units() {
        use std::ffi::OsString;
        use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};

        let os = OsString::from_vec(b"/tmp/nudox-\xff".to_vec());
        let path = Path::new(&os);
        let identity = LocalProjectId::from_path(path).expect("native path identity");
        assert!(!identity.has_utf8_spelling());
        assert_eq!(
            identity.path().as_os_str().as_bytes(),
            path.as_os_str().as_bytes()
        );
        assert_eq!(
            identity,
            LocalProjectId::from_path(&identity.path()).expect("round trip")
        );
        assert_eq!(
            identity,
            LocalProjectId::from_native_wire(&identity.native_wire().expect("wire"))
                .expect("wire round trip")
        );
    }
}
