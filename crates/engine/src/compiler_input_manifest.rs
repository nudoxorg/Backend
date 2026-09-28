//! Canonical, content-addressed compiler invocation and immutable input snapshot.
//!
//! The manifest is one typed member of the exact input closure named by the
//! cluster Offer. File bytes remain separate CAS members and are referred to
//! only by untrusted IDs until the receiving store reopens and verifies them.

use backend_cluster_transport::ControlOffer;
use backend_execution::compiler_transfer_work_id;
use backend_semantic::vocabulary::{LanguageProfile, PackageUrl, Stage};
use backend_store::{ObjectId, TypedObject, UntrustedObjectId};
use backend_version::{ObjectKey, Schema};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

const MAGIC: &[u8; 8] = b"BKCINP01";
const MAX_PATH_BYTES: usize = 4096;
const MAX_MANIFEST_BYTES: usize = 32 * 1024 * 1024;
const MAX_ENTRIES: usize = crate::application::MAX_MANIFEST_ENTRIES;

/// Typed schema for one canonical compiler input manifest.
pub struct CompilerInputManifestSchema;

impl Schema for CompilerInputManifestSchema {
    const DOMAIN: u8 = 0xe7;
    const TYPE: u16 = 1;
    const VERSION: u8 = 1;
    type Value = [u8];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Completeness guarantee carried by an immutable compiler input snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputCompleteness {
    /// The closure captures the complete admissible workspace tree; worker execution must be
    /// sandboxed to this snapshot and the exact identified toolchain/environment.
    FullWorkspace,
    /// Every positive and negative compiler read is individually represented in the frontier.
    ProvenReadSet,
}

/// Role of a regular file in the workspace snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompilerInputFileRole {
    /// Source text admitted to the package compiler.
    Source,
    /// Build configuration that may affect compilation semantics.
    Configuration,
    /// Dependency resolution or lock data that may affect compilation semantics.
    Lock,
    /// Another file in the immutable admissible workspace tree.
    Other,
}

/// One normalized path in the complete workspace inventory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CompilerInputEntry {
    /// A directory present in the captured workspace tree.
    Directory {
        /// Slash-separated, normalized path relative to the workspace root.
        path: Box<str>,
    },
    /// A regular file whose payload is a separate typed CAS object.
    File {
        /// Slash-separated, normalized path relative to the workspace root.
        path: Box<str>,
        /// Semantic role of the file for the compiler adapter.
        role: CompilerInputFileRole,
        /// Untrusted object ID; the receiving store must reopen and verify it.
        object_id: UntrustedObjectId,
        /// Exact canonical payload length in bytes.
        length: u64,
    },
}

impl CompilerInputEntry {
    /// Returns the normalized workspace-relative path.
    #[must_use]
    pub fn path(&self) -> &str {
        match self {
            Self::Directory { path } | Self::File { path, .. } => path,
        }
    }
}

/// One exact positive or negative read observation from compiler input discovery.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CompilerReadClaim {
    /// The compiler read this exact regular file object and byte length.
    PresentFile {
        /// Slash-separated, normalized path relative to the workspace root.
        path: Box<str>,
        /// Exact CAS object ID observed by the source authority.
        object_id: UntrustedObjectId,
        /// Exact payload length observed by the source authority.
        length: u64,
    },
    /// The compiler observed that this exact workspace-relative path was absent.
    AbsentPath {
        /// Slash-separated, normalized path relative to the workspace root.
        path: Box<str>,
    },
    /// The compiler enumerated this directory and observed this exact listing digest.
    DirectoryListing {
        /// Slash-separated, normalized path relative to the workspace root.
        path: Box<str>,
        /// Digest of the canonical sorted direct-child listing.
        listing_digest: [u8; 32],
    },
}

impl CompilerReadClaim {
    /// Returns the normalized workspace-relative path.
    #[must_use]
    pub fn path(&self) -> &str {
        match self {
            Self::PresentFile { path, .. }
            | Self::AbsentPath { path }
            | Self::DirectoryListing { path, .. } => path,
        }
    }
}

/// Exact typed input to one package compilation, suitable for CAS storage and remote execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompilerInputManifestV1 {
    /// Canonical pinned package URL used to reconstruct the package request.
    pub package: PackageUrl,
    /// Authority-owned logical package lineage digest.
    pub package_lineage: [u8; 32],
    /// Closed language/profile discriminator from the compiler request.
    pub profile: LanguageProfile,
    /// Closed compiler stage discriminator from the compiler request.
    pub stage: Stage,
    /// Canonical target identity used by scheduler and coverage evidence.
    pub target: [u8; 32],
    /// Exact compile recipe identity, including source and toolchain authorities.
    pub recipe: [u8; 32],
    /// Exact executable/toolchain identity the worker must provide.
    pub toolchain: [u8; 32],
    /// Exact semantic environment identity the worker must provide.
    pub environment: [u8; 32],
    /// Exact target platform/sysroot identity the worker must provide.
    pub target_platform: [u8; 32],
    /// Exact immutable source/configuration input root.
    pub input_root: [u8; 32],
    /// Exact positive/negative read manifest identity.
    pub read_manifest: [u8; 32],
    /// Selected base generation, if the invocation is an advance.
    pub selected_base: Option<[u8; 32]>,
    /// Hard upper bound for output bytes from this assignment.
    pub max_output_bytes: u64,
    /// Declared completeness law for the captured inputs.
    pub completeness: InputCompleteness,
    /// Strictly path-sorted complete workspace inventory.
    pub entries: Vec<CompilerInputEntry>,
    /// Strictly path-sorted exact positive/negative observations when using a read set.
    pub read_frontier: Vec<CompilerReadClaim>,
}

/// Invalid, oversized, or noncanonical compiler input manifest.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum CompilerInputManifestError {
    /// The bytes do not have the supported manifest version header.
    #[error("compiler input manifest header is invalid")]
    Header,
    /// The manifest ended before a required field was complete.
    #[error("compiler input manifest is truncated")]
    Truncated,
    /// A field uses an unknown enum value or an invalid canonical value.
    #[error("compiler input manifest contains an invalid field")]
    InvalidField,
    /// Paths, entry order, workspace ancestry, or read claims are inconsistent.
    #[error("compiler input manifest is not a canonical complete input description")]
    NonCanonical,
    /// A manifest field or the full encoded object exceeds its fixed bound.
    #[error("compiler input manifest exceeds a fixed bound")]
    Limit,
    /// The supplied typed object uses another schema.
    #[error("typed object is not a compiler input manifest")]
    SchemaMismatch,
    /// The typed object's logical key does not bind its canonical bytes.
    #[error("compiler input manifest object key does not match its canonical bytes")]
    ObjectKeyMismatch,
    /// The package URL is not a supported canonical pinned package coordinate.
    #[error("compiler input manifest package URL is invalid")]
    PackageUrl,
    /// The canonical target identity is not the identity of the exact package URL.
    #[error("compiler input manifest target does not identify its package URL")]
    PackageTargetMismatch,
}

impl CompilerInputManifestV1 {
    /// Validates and constructs one exact compiler input manifest.
    pub fn new(
        package: PackageUrl,
        package_lineage: [u8; 32],
        profile: LanguageProfile,
        stage: Stage,
        target: [u8; 32],
        recipe: [u8; 32],
        toolchain: [u8; 32],
        environment: [u8; 32],
        target_platform: [u8; 32],
        input_root: [u8; 32],
        read_manifest: [u8; 32],
        selected_base: Option<[u8; 32]>,
        max_output_bytes: u64,
        completeness: InputCompleteness,
        entries: Vec<CompilerInputEntry>,
        read_frontier: Vec<CompilerReadClaim>,
    ) -> Result<Self, CompilerInputManifestError> {
        let manifest = Self {
            package,
            package_lineage,
            profile,
            stage,
            target,
            recipe,
            toolchain,
            environment,
            target_platform,
            input_root,
            read_manifest,
            selected_base,
            max_output_bytes,
            completeness,
            entries,
            read_frontier,
        };
        manifest.validate()?;
        Ok(manifest)
    }

    /// Returns the canonical versioned byte encoding of this manifest.
    pub fn encode(&self) -> Result<Vec<u8>, CompilerInputManifestError> {
        self.validate()?;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        put_str(&mut bytes, self.package.as_ref())?;
        bytes.extend_from_slice(&self.package_lineage);
        bytes.extend_from_slice(&<[u8; 2]>::from(self.profile));
        bytes.push(u8::from(self.stage));
        bytes.extend_from_slice(&self.target);
        bytes.extend_from_slice(&self.recipe);
        bytes.extend_from_slice(&self.toolchain);
        bytes.extend_from_slice(&self.environment);
        bytes.extend_from_slice(&self.target_platform);
        bytes.extend_from_slice(&self.input_root);
        bytes.extend_from_slice(&self.read_manifest);
        match self.selected_base {
            Some(base) => {
                bytes.push(1);
                bytes.extend_from_slice(&base);
            }
            None => bytes.push(0),
        }
        bytes.extend_from_slice(&self.max_output_bytes.to_be_bytes());
        bytes.push(match self.completeness {
            InputCompleteness::FullWorkspace => 1,
            InputCompleteness::ProvenReadSet => 2,
        });
        put_count(&mut bytes, self.entries.len())?;
        for entry in &self.entries {
            match entry {
                CompilerInputEntry::Directory { path } => {
                    bytes.push(1);
                    put_str(&mut bytes, path)?;
                }
                CompilerInputEntry::File {
                    path,
                    role,
                    object_id,
                    length,
                } => {
                    bytes.push(2);
                    put_str(&mut bytes, path)?;
                    bytes.push(match role {
                        CompilerInputFileRole::Source => 1,
                        CompilerInputFileRole::Configuration => 2,
                        CompilerInputFileRole::Lock => 3,
                        CompilerInputFileRole::Other => 4,
                    });
                    bytes.extend_from_slice(object_id.as_bytes());
                    bytes.extend_from_slice(&length.to_be_bytes());
                }
            }
        }
        put_count(&mut bytes, self.read_frontier.len())?;
        for claim in &self.read_frontier {
            match claim {
                CompilerReadClaim::PresentFile {
                    path,
                    object_id,
                    length,
                } => {
                    bytes.push(1);
                    put_str(&mut bytes, path)?;
                    bytes.extend_from_slice(object_id.as_bytes());
                    bytes.extend_from_slice(&length.to_be_bytes());
                }
                CompilerReadClaim::AbsentPath { path } => {
                    bytes.push(2);
                    put_str(&mut bytes, path)?;
                }
                CompilerReadClaim::DirectoryListing {
                    path,
                    listing_digest,
                } => {
                    bytes.push(3);
                    put_str(&mut bytes, path)?;
                    bytes.extend_from_slice(listing_digest);
                }
            }
        }
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(CompilerInputManifestError::Limit);
        }
        Ok(bytes)
    }

    /// Decodes only the supported canonical encoding and revalidates every cross-reference.
    pub fn decode(bytes: &[u8]) -> Result<Self, CompilerInputManifestError> {
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(CompilerInputManifestError::Limit);
        }
        let mut input = Reader::new(bytes);
        if input.take(MAGIC.len())? != MAGIC {
            return Err(CompilerInputManifestError::Header);
        }
        let package_text = input.string()?;
        let package =
            PackageUrl::parse(package_text).map_err(|_| CompilerInputManifestError::PackageUrl)?;
        let package_lineage = input.array32()?;
        let profile = LanguageProfile::try_from(input.array2()?)
            .map_err(|_| CompilerInputManifestError::InvalidField)?;
        let stage =
            Stage::try_from(input.byte()?).map_err(|_| CompilerInputManifestError::InvalidField)?;
        let target = input.array32()?;
        let recipe = input.array32()?;
        let toolchain = input.array32()?;
        let environment = input.array32()?;
        let target_platform = input.array32()?;
        let input_root = input.array32()?;
        let read_manifest = input.array32()?;
        let selected_base = match input.byte()? {
            0 => None,
            1 => Some(input.array32()?),
            _ => return Err(CompilerInputManifestError::InvalidField),
        };
        let max_output_bytes = input.u64()?;
        let completeness = match input.byte()? {
            1 => InputCompleteness::FullWorkspace,
            2 => InputCompleteness::ProvenReadSet,
            _ => return Err(CompilerInputManifestError::InvalidField),
        };
        let entry_count = input.count()?;
        let mut entries = Vec::with_capacity(entry_count);
        for _ in 0..entry_count {
            let entry = match input.byte()? {
                1 => CompilerInputEntry::Directory {
                    path: input.string()?.into_boxed_str(),
                },
                2 => {
                    let path = input.string()?.into_boxed_str();
                    let role = match input.byte()? {
                        1 => CompilerInputFileRole::Source,
                        2 => CompilerInputFileRole::Configuration,
                        3 => CompilerInputFileRole::Lock,
                        4 => CompilerInputFileRole::Other,
                        _ => return Err(CompilerInputManifestError::InvalidField),
                    };
                    let object_id = UntrustedObjectId::from_bytes(input.array32()?);
                    let length = input.u64()?;
                    CompilerInputEntry::File {
                        path,
                        role,
                        object_id,
                        length,
                    }
                }
                _ => return Err(CompilerInputManifestError::InvalidField),
            };
            entries.push(entry);
        }
        let frontier_count = input.count()?;
        let mut read_frontier = Vec::with_capacity(frontier_count);
        for _ in 0..frontier_count {
            let claim = match input.byte()? {
                1 => CompilerReadClaim::PresentFile {
                    path: input.string()?.into_boxed_str(),
                    object_id: UntrustedObjectId::from_bytes(input.array32()?),
                    length: input.u64()?,
                },
                2 => CompilerReadClaim::AbsentPath {
                    path: input.string()?.into_boxed_str(),
                },
                3 => CompilerReadClaim::DirectoryListing {
                    path: input.string()?.into_boxed_str(),
                    listing_digest: input.array32()?,
                },
                _ => return Err(CompilerInputManifestError::InvalidField),
            };
            read_frontier.push(claim);
        }
        if !input.is_empty() {
            return Err(CompilerInputManifestError::NonCanonical);
        }
        let manifest = Self::new(
            package,
            package_lineage,
            profile,
            stage,
            target,
            recipe,
            toolchain,
            environment,
            target_platform,
            input_root,
            read_manifest,
            selected_base,
            max_output_bytes,
            completeness,
            entries,
            read_frontier,
        )?;
        if manifest.encode()?.as_slice() != bytes {
            return Err(CompilerInputManifestError::NonCanonical);
        }
        Ok(manifest)
    }

    /// Encodes this manifest as a typed immutable backend-store object.
    pub fn typed_object(&self) -> Result<TypedObject, CompilerInputManifestError> {
        let bytes = self.encode()?;
        let key = ObjectKey::<CompilerInputManifestSchema>::from_value(bytes.as_slice());
        Ok(TypedObject::from_value(&key, bytes.as_slice()))
    }

    /// Reopens a stored typed object and verifies its schema, key, and canonical manifest bytes.
    pub fn from_typed_object(object: &TypedObject) -> Result<Self, CompilerInputManifestError> {
        let schema = object.schema();
        if schema.domain() != CompilerInputManifestSchema::DOMAIN
            || schema.ty() != CompilerInputManifestSchema::TYPE
            || schema.version() != CompilerInputManifestSchema::VERSION
        {
            return Err(CompilerInputManifestError::SchemaMismatch);
        }
        let manifest = Self::decode(object.bytes())?;
        let key = ObjectKey::<CompilerInputManifestSchema>::from_value(object.bytes());
        if object.key() != &key.to_bytes() {
            return Err(CompilerInputManifestError::ObjectKeyMismatch);
        }
        Ok(manifest)
    }

    /// Returns the deterministic checked object ID for this manifest.
    pub fn object_id(&self) -> Result<ObjectId, CompilerInputManifestError> {
        Ok(self.typed_object()?.id())
    }

    /// Returns this manifest's typed object ID in the untrusted wire-claim form.
    pub fn object_id_claim(&self) -> Result<UntrustedObjectId, CompilerInputManifestError> {
        Ok(untrusted_object_id(self.object_id()?))
    }

    /// Checks all wire-visible work identity fields and the shared scheduler work-ID grammar.
    #[must_use]
    pub fn matches_offer(&self, offer: &ControlOffer) -> bool {
        self.package_lineage == offer.package_lineage
            && self.target == offer.target
            && self.recipe == offer.recipe
            && self.input_root == offer.input_root
            && self.read_manifest == offer.read_manifest
            && self.selected_base == offer.selected_base
            && self.max_output_bytes == offer.max_output_bytes
            && compiler_transfer_work_id(
                self.package_lineage,
                self.target,
                self.recipe,
                self.read_manifest,
                self.input_root,
                self.selected_base,
                self.max_output_bytes,
            ) == offer.scope.work_id
    }

    fn validate(&self) -> Result<(), CompilerInputManifestError> {
        if self.package.identity.as_ref() != &self.target {
            return Err(CompilerInputManifestError::PackageTargetMismatch);
        }
        if self.profile.language() != self.package.ecosystem.language()
            || self.entries.is_empty()
            || self.entries.len() > MAX_ENTRIES
            || self.read_frontier.len() > MAX_ENTRIES
            || self.max_output_bytes == 0
        {
            return Err(CompilerInputManifestError::NonCanonical);
        }
        let mut files = BTreeMap::new();
        let mut directories = BTreeSet::new();
        let mut portable_paths = BTreeSet::new();
        let mut previous_path: Option<&str> = None;
        for entry in &self.entries {
            let path = entry.path();
            validate_compiler_input_path(path)?;
            if previous_path.is_some_and(|previous| previous >= path)
                || !portable_paths.insert(path.to_ascii_lowercase())
            {
                return Err(CompilerInputManifestError::NonCanonical);
            }
            previous_path = Some(path);
            match entry {
                CompilerInputEntry::Directory { .. } => {
                    directories.insert(path);
                }
                CompilerInputEntry::File {
                    object_id, length, ..
                } => {
                    if *length == 0 {
                        return Err(CompilerInputManifestError::NonCanonical);
                    }
                    files.insert(path, (*object_id, *length));
                }
            }
        }
        for entry in &self.entries {
            let path = entry.path();
            for ancestor in ancestors(path) {
                if files.contains_key(ancestor) || !directories.contains(ancestor) {
                    return Err(CompilerInputManifestError::NonCanonical);
                }
            }
        }
        if self.completeness == InputCompleteness::FullWorkspace {
            if !self.read_frontier.is_empty() {
                return Err(CompilerInputManifestError::NonCanonical);
            }
        } else if self.read_frontier.is_empty() {
            return Err(CompilerInputManifestError::NonCanonical);
        }
        let mut previous_claim: Option<&str> = None;
        let mut portable_claims = BTreeSet::new();
        for claim in &self.read_frontier {
            validate_compiler_input_path(claim.path())?;
            if previous_claim.is_some_and(|previous| previous >= claim.path()) {
                return Err(CompilerInputManifestError::NonCanonical);
            }
            if !portable_claims.insert(claim.path().to_ascii_lowercase()) {
                return Err(CompilerInputManifestError::NonCanonical);
            }
            previous_claim = Some(claim.path());
            match claim {
                CompilerReadClaim::PresentFile {
                    path,
                    object_id,
                    length,
                } => {
                    if files.get(path.as_ref()) != Some(&(*object_id, *length)) {
                        return Err(CompilerInputManifestError::NonCanonical);
                    }
                }
                CompilerReadClaim::AbsentPath { path } => {
                    if files.contains_key(path.as_ref())
                        || directories.contains(path.as_ref())
                        || portable_paths.contains(&path.to_ascii_lowercase())
                    {
                        return Err(CompilerInputManifestError::NonCanonical);
                    }
                }
                CompilerReadClaim::DirectoryListing { path, .. } => {
                    if files.contains_key(path.as_ref()) || !directories.contains(path.as_ref()) {
                        return Err(CompilerInputManifestError::NonCanonical);
                    }
                }
            }
        }
        Ok(())
    }
}

/// Converts a checked CAS object identity into the explicit untrusted form used at wire edges.
#[must_use]
pub const fn untrusted_object_id(object_id: ObjectId) -> UntrustedObjectId {
    UntrustedObjectId::from_bytes(*object_id.as_bytes())
}

/// Validates a portable normalized relative path before a worker creates it on disk.
///
/// The ASCII-only rule removes Unicode normalization aliases; manifest validation additionally
/// rejects case-fold collisions across all entries.
pub fn validate_compiler_input_path(path: &str) -> Result<(), CompilerInputManifestError> {
    if path.is_empty()
        || path.len() > MAX_PATH_BYTES
        || !path.is_ascii()
        || path.contains('\\')
        || path.contains('\0')
        || path.starts_with('/')
        || path.split('/').any(|part| {
            part.is_empty()
                || part == "."
                || part == ".."
                || part.ends_with('.')
                || part.ends_with(' ')
                || part.chars().any(|character| {
                    character.is_control()
                        || matches!(character, '<' | '>' | ':' | '"' | '|' | '?' | '*')
                })
                || is_windows_device_name(part)
        })
    {
        return Err(CompilerInputManifestError::NonCanonical);
    }
    Ok(())
}

fn is_windows_device_name(component: &str) -> bool {
    let basename = component
        .split_once('.')
        .map_or(component, |(basename, _)| basename)
        .to_ascii_uppercase();
    matches!(basename.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ["COM", "LPT"].iter().any(|prefix| {
            basename.strip_prefix(prefix).is_some_and(|suffix| {
                suffix.len() == 1
                    && suffix
                        .as_bytes()
                        .first()
                        .is_some_and(|digit| (b'1'..=b'9').contains(digit))
            })
        })
}

fn ancestors(path: &str) -> impl Iterator<Item = &str> {
    path.match_indices('/').map(|(index, _)| &path[..index])
}

fn put_count(output: &mut Vec<u8>, count: usize) -> Result<(), CompilerInputManifestError> {
    if count > MAX_ENTRIES {
        return Err(CompilerInputManifestError::Limit);
    }
    let count = u32::try_from(count).map_err(|_| CompilerInputManifestError::Limit)?;
    output.extend_from_slice(&count.to_be_bytes());
    Ok(())
}

fn put_str(output: &mut Vec<u8>, text: &str) -> Result<(), CompilerInputManifestError> {
    if text.len() > MAX_PATH_BYTES.max(1024) {
        return Err(CompilerInputManifestError::Limit);
    }
    let length = u32::try_from(text.len()).map_err(|_| CompilerInputManifestError::Limit)?;
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(text.as_bytes());
    Ok(())
}

struct Reader<'bytes> {
    bytes: &'bytes [u8],
    offset: usize,
}

impl<'bytes> Reader<'bytes> {
    const fn new(bytes: &'bytes [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'bytes [u8], CompilerInputManifestError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(CompilerInputManifestError::Limit)?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or(CompilerInputManifestError::Truncated)?;
        self.offset = end;
        Ok(value)
    }

    fn byte(&mut self) -> Result<u8, CompilerInputManifestError> {
        Ok(self.take(1)?[0])
    }

    fn array2(&mut self) -> Result<[u8; 2], CompilerInputManifestError> {
        self.take(2)?
            .try_into()
            .map_err(|_| CompilerInputManifestError::Truncated)
    }

    fn array32(&mut self) -> Result<[u8; 32], CompilerInputManifestError> {
        self.take(32)?
            .try_into()
            .map_err(|_| CompilerInputManifestError::Truncated)
    }

    fn u32(&mut self) -> Result<u32, CompilerInputManifestError> {
        Ok(u32::from_be_bytes(
            self.take(4)?
                .try_into()
                .map_err(|_| CompilerInputManifestError::Truncated)?,
        ))
    }

    fn u64(&mut self) -> Result<u64, CompilerInputManifestError> {
        Ok(u64::from_be_bytes(
            self.take(8)?
                .try_into()
                .map_err(|_| CompilerInputManifestError::Truncated)?,
        ))
    }

    fn count(&mut self) -> Result<usize, CompilerInputManifestError> {
        let count = usize::try_from(self.u32()?).map_err(|_| CompilerInputManifestError::Limit)?;
        if count > MAX_ENTRIES {
            return Err(CompilerInputManifestError::Limit);
        }
        Ok(count)
    }

    fn string(&mut self) -> Result<String, CompilerInputManifestError> {
        let length = usize::try_from(self.u32()?).map_err(|_| CompilerInputManifestError::Limit)?;
        if length > MAX_PATH_BYTES.max(1024) {
            return Err(CompilerInputManifestError::Limit);
        }
        std::str::from_utf8(self.take(length)?)
            .map(str::to_owned)
            .map_err(|_| CompilerInputManifestError::InvalidField)
    }

    fn is_empty(&self) -> bool {
        self.offset == self.bytes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_semantic::vocabulary::{LanguageProfile, PackageUrl, RustEdition};

    fn manifest() -> CompilerInputManifestV1 {
        let package = PackageUrl::parse("pkg:cargo/fixture@1.0.0".to_owned()).expect("package URL");
        let target = *package.identity.as_ref();
        CompilerInputManifestV1::new(
            package,
            [1; 32],
            LanguageProfile::Rust(RustEdition::Rust2024),
            Stage::LowerIr,
            target,
            [3; 32],
            [4; 32],
            [5; 32],
            [6; 32],
            [7; 32],
            [8; 32],
            None,
            8 * 1024 * 1024,
            InputCompleteness::FullWorkspace,
            vec![
                CompilerInputEntry::Directory { path: "src".into() },
                CompilerInputEntry::File {
                    path: "src/lib.rs".into(),
                    role: CompilerInputFileRole::Source,
                    object_id: UntrustedObjectId::from_bytes([9; 32]),
                    length: 64,
                },
            ],
            Vec::new(),
        )
        .expect("valid manifest")
    }

    #[test]
    fn canonical_typed_object_roundtrips_and_rejects_wrong_key() {
        let manifest = manifest();
        let object = manifest.typed_object().expect("typed object");
        assert_eq!(
            CompilerInputManifestV1::from_typed_object(&object),
            Ok(manifest.clone())
        );
        assert_eq!(
            manifest.object_id_claim(),
            Ok(untrusted_object_id(object.id()))
        );
    }

    #[test]
    fn full_workspace_requires_directory_inventory_and_canonical_paths() {
        let mut missing_directory = manifest();
        missing_directory.entries.remove(0);
        assert_eq!(
            missing_directory.encode(),
            Err(CompilerInputManifestError::NonCanonical)
        );

        let mut invalid_path = manifest();
        invalid_path.entries[1] = CompilerInputEntry::File {
            path: "src/../lib.rs".into(),
            role: CompilerInputFileRole::Source,
            object_id: UntrustedObjectId::from_bytes([9; 32]),
            length: 64,
        };
        assert_eq!(
            invalid_path.encode(),
            Err(CompilerInputManifestError::NonCanonical)
        );
    }

    #[test]
    fn trailing_bytes_are_rejected() {
        let mut bytes = manifest().encode().expect("encoding");
        bytes.push(0);
        assert_eq!(
            CompilerInputManifestV1::decode(&bytes),
            Err(CompilerInputManifestError::NonCanonical)
        );
    }

    #[test]
    fn a_valid_package_url_cannot_be_swapped_while_retaining_the_target() {
        let mut manifest = manifest();
        manifest.package = PackageUrl::parse("pkg:cargo/other@1.0.0".to_owned())
            .expect("second package URL is valid");

        assert_eq!(
            manifest.encode(),
            Err(CompilerInputManifestError::PackageTargetMismatch)
        );
    }

    #[test]
    fn portable_paths_reject_aliases_and_host_specific_names() {
        for path in [
            "C:/outside.rs",
            "src/NUL.txt",
            "src/COM1.rs",
            "src/name.",
            "src/name ",
            "src/file.rs:stream",
            "src/cafe\u{301}.rs",
        ] {
            assert_eq!(
                validate_compiler_input_path(path),
                Err(CompilerInputManifestError::NonCanonical),
                "path should be rejected: {path}"
            );
        }
        let mut manifest = manifest();
        manifest
            .entries
            .push(CompilerInputEntry::Directory { path: "Src".into() });
        manifest
            .entries
            .sort_by(|left, right| left.path().cmp(right.path()));
        assert_eq!(
            manifest.encode(),
            Err(CompilerInputManifestError::NonCanonical)
        );
    }
}
