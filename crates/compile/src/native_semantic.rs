//! Typed language admission and semantic helper identity.

use crate::native_adapter::{NativeTemplate, native_executable_evidence, native_input};
use crate::{
    AuthorityError, AuthorityIdentity, DiscoverySnapshot, FactKeySchema, FactRecord,
    FactValueSchema, InputManifestId, NativeRequestInput, SessionKey,
};
use backend_semantic::{FacetKind, FacetValue, FacetValueSchema};
use backend_version::ObjectVersion;
use std::{fmt, path::Path};

/// Language-owned admission from hostile helper records into canonical facts.
pub trait NativeSemanticAdapter {
    /// Borrowed representation produced from one hostile native record.
    type Parsed<'record>
    where
        Self: 'record;
    /// Borrowed or owned record whose language invariants have been proved.
    type Admitted<'record>
    where
        Self: 'record;
    /// Concrete parse or admission failure retained by the language frontend.
    type Error: fmt::Display;

    /// Parses one bounded record without assigning it semantic authority.
    ///
    /// # Errors
    /// Returns the adapter's concrete parse failure for malformed payloads.
    fn parse<'record>(
        &'record self,
        record: &'record crate::NativeRecord,
    ) -> Result<Self::Parsed<'record>, Self::Error>;

    /// Validates a parsed record against the language schema and profile.
    ///
    /// # Errors
    /// Returns the concrete admission failure for an invalid semantic fact.
    fn admit<'record>(
        &'record self,
        parsed: Self::Parsed<'record>,
    ) -> Result<Self::Admitted<'record>, Self::Error>;

    /// Lowers an admitted language record into canonical fact storage.
    fn lower<'record>(
        &'record self,
        admitted: Self::Admitted<'record>,
    ) -> FactRecord<FactKeySchema, FactValueSchema>;
}

/// Manifest-bound inputs for one production semantic extraction.
pub struct NativeSemanticRequest<'request> {
    pub(super) identity: AuthorityIdentity,
    pub(super) language: &'request str,
    pub(super) snapshot: &'request DiscoverySnapshot,
    pub(super) key: SessionKey,
    pub(super) template: Option<&'request NativeTemplate>,
    pub(super) inputs: Vec<NativeRequestInput>,
    pub(super) current_manifest: InputManifestId,
}

impl<'request> NativeSemanticRequest<'request> {
    /// Binds semantic execution to the frontend's current input manifest.
    #[must_use]
    pub fn new(
        identity: AuthorityIdentity,
        language: &'request str,
        snapshot: &'request DiscoverySnapshot,
        key: SessionKey,
        template: Option<&'request NativeTemplate>,
        inputs: Vec<NativeRequestInput>,
        current_manifest: InputManifestId,
    ) -> Self {
        Self {
            identity,
            language,
            snapshot,
            key,
            template,
            inputs,
            current_manifest,
        }
    }
}

const NATIVE_FACETS: [FacetKind; 6] = [
    FacetKind::Source,
    FacetKind::Entity,
    FacetKind::Facet,
    FacetKind::Type,
    FacetKind::Edge,
    FacetKind::Configuration,
];

/// Returns the canonical semantic schema witness used by every native leaf.
#[must_use]
pub fn native_semantic_evidence() -> Vec<u8> {
    let mut bytes = Vec::with_capacity(2 + NATIVE_FACETS.len() * 2);
    bytes.push(backend_version::CANONICAL_VERSION);
    bytes.push(<FacetValueSchema as backend_version::Schema>::VERSION);
    for facet in NATIVE_FACETS {
        bytes.extend_from_slice(&facet.tag().to_be_bytes());
    }
    let value = FacetValue::new(FacetKind::Facet, bytes);
    ObjectVersion::<FacetValueSchema>::from_value(&value)
        .to_bytes()
        .to_vec()
}

/// Builds the semantic schema witness input shared by native requests.
///
/// # Errors
/// Returns an error if protocol bounds reject the fixed-size witness.
pub fn native_semantic_input() -> Result<NativeRequestInput, AuthorityError> {
    native_input("semantic-fact-schema", native_semantic_evidence())
}

/// Returns canonical content evidence for a helper file or source directory.
#[must_use]
pub fn native_helper_evidence(path: &Path) -> Vec<u8> {
    if path.is_file() {
        return native_executable_evidence(path);
    }
    directory_evidence(path).unwrap_or_else(|| b"NHELP\0unavailable".to_vec())
}

fn directory_evidence(root: &Path) -> Option<Vec<u8>> {
    const MAX_FILES: usize = 64;
    const MAX_BYTES: usize = 1024 * 1024;
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory).ok()? {
            let entry = entry.ok()?;
            let file_type = entry.file_type().ok()?;
            if file_type.is_symlink() {
                return None;
            }
            if file_type.is_dir() {
                pending.push(entry.path());
            } else if file_type.is_file() {
                files.push(entry.path());
                if files.len() > MAX_FILES {
                    return None;
                }
            }
        }
    }
    files.sort();
    let mut encoded = Vec::new();
    encoded.extend_from_slice(b"NHELP\0v1");
    for file in files {
        let relative = file.strip_prefix(root).ok()?.to_str()?;
        let bytes = std::fs::read(&file).ok()?;
        if encoded.len().checked_add(bytes.len())? > MAX_BYTES {
            return None;
        }
        encoded.extend_from_slice(&u32::try_from(relative.len()).ok()?.to_be_bytes());
        encoded.extend_from_slice(relative.as_bytes());
        encoded.extend_from_slice(blake3::hash(&bytes).as_bytes());
    }
    Some(encoded)
}
