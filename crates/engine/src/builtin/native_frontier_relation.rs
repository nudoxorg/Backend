//! Exact, typed source dispositions for native compiler profiles.
//!
//! This relation is separate from structural source rows: a native candidate
//! can bind malformed-but-compiler-valid bytes without making them parsed
//! source facts or an available `ProductSourceRecord`.

use crate::workspace::{WorkspaceRelationError, WorkspaceRelationHandle, WorkspaceSnapshot};
use backend_semantic::vocabulary::{LanguageProfile, PackageUrl};
use backend_store::TypedObject;
use backend_version::{
    CanonicalRelation, ContentId, ObjectKey, Relation, RelationDecodeError, Schema, SchemaIdentity,
    SourceFactDomain, StateRoot,
};
use std::collections::{BTreeMap, BTreeSet};

const MAGIC: &[u8; 4] = b"PNF2";
const ROOT_KEY: [u8; 32] = [0x4e; 32];
const MAX_PATH_BYTES: usize = 4096;
/// Small relation values keep each retained content piece inside existing row budgets.
pub const NATIVE_SOURCE_CHUNK_BYTES: usize = 1024;
/// Reconstruction never exceeds the existing 64 MiB admitted compiler file ceiling.
pub const MAX_NATIVE_SOURCE_BYTES: usize = 64 * 1024 * 1024;

/// Persisted, content-bound dispositions for files in a native profile.
#[derive(Debug)]
pub struct ProductNativeFrontierRelation;

/// Closure pointer selecting the native frontier relation.
pub struct ProductNativeFrontierRootSchema;

impl Schema for ProductNativeFrontierRootSchema {
    const DOMAIN: u8 = 0x97;
    const TYPE: u16 = 11;
    const VERSION: u8 = 2;
    type Value = [u8; 32];

    fn encode(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(value);
    }
}

/// One exact per-path disposition or a complete profile manifest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProductNativeFrontierRecord {
    /// Commits the exact sorted disposition set for one project/profile.
    Profile {
        /// Stable digest of the owning project coordinate.
        project: [u8; 32],
        /// Compiler profile whose complete source set this record commits.
        profile: LanguageProfile,
        /// Exact compiler package coordinate selected by the product indexer.
        /// `None` is retained only for non-compilable legacy/test manifests;
        /// native candidate permits require this owner binding.
        package_target: Option<PackageUrl>,
        /// Number of normalized source paths represented by the manifest.
        path_count: u32,
        /// Digest of the sorted path/disposition pairs in the manifest.
        disposition_digest: [u8; 32],
    },
    /// States how one supported source path participates in the profile.
    Path {
        /// Stable digest of the owning project coordinate.
        project: [u8; 32],
        /// Compiler profile containing this path.
        profile: LanguageProfile,
        /// Normalized project-relative source path.
        path: String,
        /// Exact structural, compiler-only, or refusal state for this path.
        disposition: NativePathDisposition,
    },
    /// One bounded piece of exact malformed-source bytes, selected by the same root.
    ContentChunk {
        /// Stable digest of the owning project coordinate.
        project: [u8; 32],
        /// Compiler profile containing the source file.
        profile: LanguageProfile,
        /// Normalized project-relative path whose bytes are stored.
        path: String,
        /// Zero-based ordinal of this chunk within the path's byte sequence.
        ordinal: u32,
        /// Exact source bytes for this bounded chunk.
        bytes: Box<[u8]>,
    },
}

/// A disposition is either already represented by the structural compiler
/// path, a narrowly eligible compiler-only candidate, or explicitly refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativePathDisposition {
    /// Structurally parsed path with exact bytes and source-fact identities.
    Structural {
        /// Content-object digest for the exact source bytes.
        content: [u8; 32],
        /// Identity of the corresponding structural source fact.
        source_identity: ContentId<SourceFactDomain>,
    },
    /// Valid UTF-8 bytes refused only by the explicit Clang malformed-source
    /// baseline result; this does not claim structural declarations.
    ClangMalformed {
        /// Content-object digest for the exact source bytes.
        content: [u8; 32],
        /// Identity of the source bytes retained for this disposition.
        source_identity: ContentId<SourceFactDomain>,
        /// Exact source length in bytes.
        byte_length: u32,
        /// Number of bounded content chunks retained for reconstruction.
        chunk_count: u32,
    },
    /// Valid UTF-8 bytes refused only by the explicit C# malformed-source
    /// baseline result; this does not claim structural declarations.
    CSharpMalformed {
        /// Content-object digest for the exact source bytes.
        content: [u8; 32],
        /// Identity of the source bytes retained for this disposition.
        source_identity: ContentId<SourceFactDomain>,
        /// Exact source length in bytes.
        byte_length: u32,
        /// Number of bounded content chunks retained for reconstruction.
        chunk_count: u32,
    },
    /// The path is accounted for, but is not eligible for native compilation.
    Refused(NativePathRefusal),
}

/// Closed refusal reasons retained for an exact profile path.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativePathRefusal {
    /// Source bytes were not valid UTF-8.
    InvalidUtf8,
    /// The compiler refused the path for a reason other than malformed input.
    MalformedOther,
    /// The source could not be read during capture.
    ReadUnavailable,
    /// The source exceeded the configured native-source size ceiling.
    Oversized,
}

/// A profile frontier whose every current source path and manifest digest
/// were checked against one selected workspace relation root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedNativeFrontier {
    project: [u8; 32],
    profile: LanguageProfile,
    root: [u8; 32],
    package_target: Option<PackageUrl>,
    dispositions: Vec<(String, NativePathDisposition)>,
}

/// Unforgeable authorization for one exact malformed Clang path selected in a
/// checked current native frontier.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeClangCandidatePermit {
    project: [u8; 32],
    selected_root: [u8; 32],
    profile: LanguageProfile,
    package_target: PackageUrl,
    path: String,
    content: [u8; 32],
    source_identity: ContentId<SourceFactDomain>,
}

/// Unforgeable authorization for one exact malformed C# path selected in a
/// checked current native frontier.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeCSharpCandidatePermit {
    project: [u8; 32],
    selected_root: [u8; 32],
    profile: LanguageProfile,
    package_target: PackageUrl,
    path: String,
    content: [u8; 32],
    source_identity: ContentId<SourceFactDomain>,
}

impl NativeClangCandidatePermit {
    /// Exact path authorized by the checked selected frontier.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Exact Clang profile authorized by the checked selected frontier.
    #[must_use]
    pub const fn profile(&self) -> LanguageProfile {
        self.profile
    }

    /// Selected native-frontier root that issued this permit.
    #[must_use]
    pub const fn selected_root(&self) -> [u8; 32] {
        self.selected_root
    }

    /// Exact compiler package coordinate bound to the selected product frontier.
    #[must_use]
    pub const fn package_target(&self) -> &PackageUrl {
        &self.package_target
    }

    pub(crate) fn admits(
        &self,
        project: [u8; 32],
        selected_root: [u8; 32],
        package_target: &PackageUrl,
        relative_path: &str,
        profile: LanguageProfile,
        bytes: &[u8],
    ) -> bool {
        project == self.project
            && selected_root == self.selected_root
            && package_target == &self.package_target
            && relative_path == self.path
            && profile == self.profile
            && backend_compile::typed_of::<backend_compile::InputContentSchema>(bytes).to_bytes()
                == self.content
            && ContentId::<SourceFactDomain>::from_canonical_bytes(bytes) == self.source_identity
    }
}

impl NativeCSharpCandidatePermit {
    /// Exact path authorized by the checked selected frontier.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Exact C# profile authorized by the checked selected frontier.
    #[must_use]
    pub const fn profile(&self) -> LanguageProfile {
        self.profile
    }

    /// Selected native-frontier root that issued this permit.
    #[must_use]
    pub const fn selected_root(&self) -> [u8; 32] {
        self.selected_root
    }

    /// Exact compiler package coordinate bound to the selected product frontier.
    #[must_use]
    pub const fn package_target(&self) -> &PackageUrl {
        &self.package_target
    }

    pub(crate) fn admits(
        &self,
        project: [u8; 32],
        selected_root: [u8; 32],
        package_target: &PackageUrl,
        relative_path: &str,
        profile: LanguageProfile,
        bytes: &[u8],
    ) -> bool {
        project == self.project
            && selected_root == self.selected_root
            && package_target == &self.package_target
            && relative_path == self.path
            && profile == self.profile
            && backend_compile::typed_of::<backend_compile::InputContentSchema>(bytes).to_bytes()
                == self.content
            && ContentId::<SourceFactDomain>::from_canonical_bytes(bytes) == self.source_identity
    }
}

impl CheckedNativeFrontier {
    /// Exact project/profile covered by this selected frontier.
    #[must_use]
    pub const fn owner(&self) -> ([u8; 32], LanguageProfile) {
        (self.project, self.profile)
    }

    /// Exact selected relation root from which this frontier was checked.
    #[must_use]
    pub const fn selected_root(&self) -> [u8; 32] {
        self.root
    }

    /// Complete current path/disposition set, in normalized path order.
    #[must_use]
    pub fn dispositions(&self) -> &[(String, NativePathDisposition)] {
        &self.dispositions
    }

    /// Whether every current path has compiler authority rather than a typed refusal.
    #[must_use]
    pub fn is_compilable(&self) -> bool {
        !self.dispositions.is_empty()
            && self
                .dispositions
                .iter()
                .all(|(_, disposition)| !matches!(disposition, NativePathDisposition::Refused(_)))
    }

    /// Issues a native-only compiler permit for one exact malformed path.
    /// Structural and refused dispositions never produce a permit.
    #[must_use]
    pub fn candidate_permit(&self, path: &str) -> Option<NativeClangCandidatePermit> {
        let package_target = self.package_target.clone()?;
        if !matches!(
            self.profile,
            LanguageProfile::C(_) | LanguageProfile::Cxx(_)
        ) {
            return None;
        }
        let disposition = self
            .dispositions
            .binary_search_by(|(candidate, _)| candidate.as_str().cmp(path))
            .ok()?;
        let NativePathDisposition::ClangMalformed {
            content,
            source_identity,
            ..
        } = self.dispositions[disposition].1
        else {
            return None;
        };
        Some(NativeClangCandidatePermit {
            project: self.project,
            selected_root: self.root,
            profile: self.profile,
            package_target,
            path: path.to_owned(),
            content,
            source_identity,
        })
    }

    /// Issues a native-only compiler permit for one exact malformed C# path.
    /// Structural and refused dispositions never produce a permit.
    #[must_use]
    pub fn csharp_candidate_permit(&self, path: &str) -> Option<NativeCSharpCandidatePermit> {
        let package_target = self.package_target.clone()?;
        if !matches!(self.profile, LanguageProfile::CSharp(_)) {
            return None;
        }
        let disposition = self
            .dispositions
            .binary_search_by(|(candidate, _)| candidate.as_str().cmp(path))
            .ok()?;
        let NativePathDisposition::CSharpMalformed {
            content,
            source_identity,
            ..
        } = self.dispositions[disposition].1
        else {
            return None;
        };
        Some(NativeCSharpCandidatePermit {
            project: self.project,
            selected_root: self.root,
            profile: self.profile,
            package_target,
            path: path.to_owned(),
            content,
            source_identity,
        })
    }
}

impl ProductNativeFrontierRecord {
    /// Returns the owning project and profile.
    #[must_use]
    pub const fn owner(&self) -> ([u8; 32], LanguageProfile) {
        match self {
            Self::Profile {
                project, profile, ..
            }
            | Self::Path {
                project, profile, ..
            }
            | Self::ContentChunk {
                project, profile, ..
            } => (*project, *profile),
        }
    }

    /// Returns the exact path when this is a per-path row.
    #[must_use]
    pub fn path(&self) -> Option<&str> {
        match self {
            Self::Profile { .. } => None,
            Self::Path { path, .. } | Self::ContentChunk { path, .. } => Some(path),
        }
    }
}

/// Returns the domain-separated canonical key for a native frontier row.
pub fn product_native_frontier_record_key(
    record: &ProductNativeFrontierRecord,
) -> Result<[u8; 32], String> {
    match record {
        ProductNativeFrontierRecord::Profile {
            project, profile, ..
        } => Ok(product_native_frontier_profile_key(*project, *profile)),
        ProductNativeFrontierRecord::Path {
            project,
            profile,
            path,
            ..
        } => product_native_frontier_path_key(*project, *profile, path),
        ProductNativeFrontierRecord::ContentChunk {
            project,
            profile,
            path,
            ordinal,
            ..
        } => product_native_frontier_chunk_key(*project, *profile, path, *ordinal),
    }
}

/// Creates the canonical closure object selecting a checked native frontier.
#[must_use]
pub fn product_native_frontier_root_object(
    root: StateRoot<ProductNativeFrontierRelation>,
) -> TypedObject {
    let key = ObjectKey::<ProductNativeFrontierRootSchema>::from_value(&ROOT_KEY);
    TypedObject::from_value(&key, root.as_bytes())
}

/// Opens the native frontier selected by this exact immutable workspace.
pub fn product_native_frontier_relation(
    snapshot: &WorkspaceSnapshot,
) -> Result<Option<WorkspaceRelationHandle<ProductNativeFrontierRelation>>, WorkspaceRelationError>
{
    let key = ObjectKey::<ProductNativeFrontierRootSchema>::from_value(&ROOT_KEY);
    snapshot.auxiliary_relation::<ProductNativeFrontierRelation>(
        SchemaIdentity::new(
            ProductNativeFrontierRootSchema::DOMAIN,
            ProductNativeFrontierRootSchema::TYPE,
            ProductNativeFrontierRootSchema::VERSION,
        ),
        *key.as_bytes(),
    )
}

/// Deterministic relation key for the profile manifest.
#[must_use]
pub fn product_native_frontier_profile_key(
    project: [u8; 32],
    profile: LanguageProfile,
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"nudox.native-frontier.profile.v1\0");
    hasher.update(&project);
    hasher.update(&<[u8; 2]>::from(profile));
    *hasher.finalize().as_bytes()
}

/// Deterministic relation key for one normalized profile-relative path.
pub fn product_native_frontier_path_key(
    project: [u8; 32],
    profile: LanguageProfile,
    path: &str,
) -> Result<[u8; 32], String> {
    validate_path(path)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"nudox.native-frontier.path.v1\0");
    hasher.update(&project);
    hasher.update(&<[u8; 2]>::from(profile));
    hasher.update(&(path.len() as u32).to_be_bytes());
    hasher.update(path.as_bytes());
    Ok(*hasher.finalize().as_bytes())
}

/// Deterministic key for one path-bound native source content chunk.
pub fn product_native_frontier_chunk_key(
    project: [u8; 32],
    profile: LanguageProfile,
    path: &str,
    ordinal: u32,
) -> Result<[u8; 32], String> {
    validate_path(path)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"nudox.native-frontier.chunk.v1\0");
    hasher.update(&project);
    hasher.update(&<[u8; 2]>::from(profile));
    hasher.update(&(path.len() as u32).to_be_bytes());
    hasher.update(path.as_bytes());
    hasher.update(&ordinal.to_be_bytes());
    Ok(*hasher.finalize().as_bytes())
}

/// Hashes a complete sorted path/disposition set for the profile manifest.
pub fn product_native_frontier_digest<'a>(
    records: impl IntoIterator<Item = (&'a str, NativePathDisposition)>,
) -> Result<([u8; 32], u32), String> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"nudox.native-frontier.dispositions.v1\0");
    let mut previous: Option<&str> = None;
    let mut count = 0_u32;
    for (path, disposition) in records {
        validate_path(path)?;
        if previous.is_some_and(|previous| previous >= path) {
            return Err("native frontier paths are not strictly sorted".to_owned());
        }
        previous = Some(path);
        hasher.update(&(path.len() as u32).to_be_bytes());
        hasher.update(path.as_bytes());
        encode_disposition(disposition, &mut |bytes: &[u8]| {
            hasher.update(bytes);
        });
        count = count
            .checked_add(1)
            .ok_or_else(|| "native frontier path count exceeds u32".to_owned())?;
    }
    Ok((*hasher.finalize().as_bytes(), count))
}

/// Builds the profile manifest and exact per-path relation rows.
pub fn build_native_frontier_records(
    project: [u8; 32],
    profile: LanguageProfile,
    dispositions: Vec<(String, NativePathDisposition)>,
) -> Result<Vec<([u8; 32], ProductNativeFrontierRecord)>, String> {
    build_native_frontier_records_with_content(project, profile, dispositions, Vec::new())
}

/// Builds profile/path rows and the exact bounded chunks for malformed candidates.
pub fn build_native_frontier_records_with_content(
    project: [u8; 32],
    profile: LanguageProfile,
    mut dispositions: Vec<(String, NativePathDisposition)>,
    content: Vec<(String, Vec<u8>)>,
) -> Result<Vec<([u8; 32], ProductNativeFrontierRecord)>, String> {
    dispositions.sort_by(|left, right| left.0.cmp(&right.0));
    let mut content_by_path = BTreeMap::new();
    for (path, bytes) in content {
        validate_path(&path)?;
        if content_by_path.insert(path.clone(), bytes).is_some() {
            return Err(format!("duplicate native content payload path: {path}"));
        }
    }
    for (path, disposition) in &mut dispositions {
        let malformed = match disposition {
            NativePathDisposition::ClangMalformed {
                content,
                source_identity,
                byte_length,
                chunk_count,
            }
            | NativePathDisposition::CSharpMalformed {
                content,
                source_identity,
                byte_length,
                chunk_count,
            } => Some((content, source_identity, byte_length, chunk_count)),
            _ => None,
        };
        if let Some((expected_content, source_identity, byte_length, chunk_count)) = malformed {
            let bytes = content_by_path
                .get(path)
                .ok_or_else(|| format!("native malformed path lacks retained bytes: {path}"))?;
            if bytes.is_empty()
                || bytes.len() > MAX_NATIVE_SOURCE_BYTES
                || std::str::from_utf8(bytes).is_err()
                || backend_compile::typed_of::<backend_compile::InputContentSchema>(bytes)
                    .to_bytes()
                    != *expected_content
                || ContentId::<SourceFactDomain>::from_canonical_bytes(bytes) != *source_identity
            {
                return Err(format!(
                    "native malformed bytes differ from disposition: {path}"
                ));
            }
            *byte_length = u32::try_from(bytes.len())
                .map_err(|_| "native malformed source length exceeds u32".to_owned())?;
            *chunk_count = u32::try_from(bytes.len().div_ceil(NATIVE_SOURCE_CHUNK_BYTES))
                .map_err(|_| "native malformed source chunk count exceeds u32".to_owned())?;
        }
    }
    let malformed_paths = dispositions
        .iter()
        .filter_map(|(path, disposition)| {
            matches!(
                disposition,
                NativePathDisposition::ClangMalformed { .. }
                    | NativePathDisposition::CSharpMalformed { .. }
            )
            .then_some(path.as_str())
        })
        .collect::<BTreeSet<_>>();
    if content_by_path
        .keys()
        .any(|path| !malformed_paths.contains(path.as_str()))
    {
        return Err("native content bytes name a path without malformed disposition".to_owned());
    }
    if dispositions
        .iter()
        .any(|(_, disposition)| matches!(disposition, NativePathDisposition::ClangMalformed { .. }))
        && !matches!(profile, LanguageProfile::C(_) | LanguageProfile::Cxx(_))
    {
        return Err("Clang malformed-source disposition requires a Clang profile".to_owned());
    }
    if dispositions.iter().any(|(_, disposition)| {
        matches!(disposition, NativePathDisposition::CSharpMalformed { .. })
    }) && !matches!(profile, LanguageProfile::CSharp(_))
    {
        return Err("C# malformed-source disposition requires a C# profile".to_owned());
    }
    let (disposition_digest, path_count) = product_native_frontier_digest(
        dispositions
            .iter()
            .map(|(path, disposition)| (path.as_str(), *disposition)),
    )?;
    if path_count == 0 {
        return Err("native profile frontier cannot be empty".to_owned());
    }
    let mut records = Vec::with_capacity(dispositions.len().saturating_add(1));
    records.push((
        product_native_frontier_profile_key(project, profile),
        ProductNativeFrontierRecord::Profile {
            project,
            profile,
            package_target: None,
            path_count,
            disposition_digest,
        },
    ));
    for (path, disposition) in dispositions {
        let key = product_native_frontier_path_key(project, profile, &path)?;
        records.push((
            key,
            ProductNativeFrontierRecord::Path {
                project,
                profile,
                path,
                disposition,
            },
        ));
    }
    for (path, bytes) in content_by_path {
        for (ordinal, chunk) in bytes.chunks(NATIVE_SOURCE_CHUNK_BYTES).enumerate() {
            let ordinal = u32::try_from(ordinal)
                .map_err(|_| "native content chunk ordinal exceeds u32".to_owned())?;
            let key = product_native_frontier_chunk_key(project, profile, &path, ordinal)?;
            records.push((
                key,
                ProductNativeFrontierRecord::ContentChunk {
                    project,
                    profile,
                    path: path.clone(),
                    ordinal,
                    bytes: chunk.into(),
                },
            ));
        }
    }
    Ok(records)
}

/// Loads and validates a complete profile frontier against the current
/// normalized path set. Missing or mismatched rows never fall back to a
/// weaker path-only comparison.
pub fn admit_native_frontier(
    relation: &WorkspaceRelationHandle<ProductNativeFrontierRelation>,
    project: [u8; 32],
    profile: LanguageProfile,
    current_paths: &BTreeSet<String>,
) -> Result<CheckedNativeFrontier, String> {
    let manifest = relation
        .lookup(&product_native_frontier_profile_key(project, profile))
        .map_err(|error| format!("read native frontier manifest: {error}"))?
        .ok_or_else(|| "native profile frontier manifest is missing".to_owned())?;
    let ProductNativeFrontierRecord::Profile {
        project: manifest_project,
        profile: manifest_profile,
        package_target,
        path_count,
        disposition_digest,
    } = manifest
    else {
        return Err("native profile frontier key names a path row".to_owned());
    };
    if manifest_project != project || manifest_profile != profile {
        return Err("native profile frontier owner differs from its key".to_owned());
    }
    if path_count == 0 || current_paths.is_empty() {
        return Err("native profile frontier cannot be vacuously complete".to_owned());
    }
    let expected_count = u32::try_from(current_paths.len())
        .map_err(|_| "current profile path count exceeds u32".to_owned())?;
    if path_count != expected_count {
        return Err("native profile frontier path count is stale".to_owned());
    }
    let mut dispositions = Vec::with_capacity(current_paths.len());
    for path in current_paths {
        let key = product_native_frontier_path_key(project, profile, path)?;
        let record = relation
            .lookup(&key)
            .map_err(|error| format!("read native frontier path row: {error}"))?
            .ok_or_else(|| format!("native profile frontier omits {path}"))?;
        let ProductNativeFrontierRecord::Path {
            project: row_project,
            profile: row_profile,
            path: row_path,
            disposition,
        } = record
        else {
            return Err(format!(
                "native frontier path key names a profile row: {path}"
            ));
        };
        if row_project != project || row_profile != profile || row_path != *path {
            return Err(format!(
                "native frontier path row does not match its key: {path}"
            ));
        }
        dispositions.push((path.clone(), disposition));
    }
    let (actual_digest, actual_count) = product_native_frontier_digest(
        dispositions
            .iter()
            .map(|(path, disposition)| (path.as_str(), *disposition)),
    )?;
    if actual_count != path_count || actual_digest != disposition_digest {
        return Err("native profile frontier digest is stale".to_owned());
    }
    for (path, disposition) in &dispositions {
        let malformed = match disposition {
            NativePathDisposition::ClangMalformed {
                content,
                source_identity,
                byte_length,
                chunk_count,
            }
            | NativePathDisposition::CSharpMalformed {
                content,
                source_identity,
                byte_length,
                chunk_count,
            } => Some((*content, *source_identity, *byte_length, *chunk_count)),
            _ => None,
        };
        if let Some((content, source_identity, byte_length, chunk_count)) = malformed {
            let bytes =
                reconstruct_content(relation, project, profile, path, byte_length, chunk_count)?;
            if backend_compile::typed_of::<backend_compile::InputContentSchema>(&bytes).to_bytes()
                != content
                || ContentId::<SourceFactDomain>::from_canonical_bytes(&bytes) != source_identity
            {
                return Err(format!(
                    "native content chunks do not match path identity: {path}"
                ));
            }
        }
    }
    Ok(CheckedNativeFrontier {
        project,
        profile,
        root: *relation.root().as_bytes(),
        package_target,
        dispositions,
    })
}

pub(super) fn reconstruct_content(
    relation: &WorkspaceRelationHandle<ProductNativeFrontierRelation>,
    project: [u8; 32],
    profile: LanguageProfile,
    path: &str,
    byte_length: u32,
    chunk_count: u32,
) -> Result<Vec<u8>, String> {
    let length = usize::try_from(byte_length)
        .map_err(|_| "native source length exceeds address space".to_owned())?;
    if length == 0 || length > MAX_NATIVE_SOURCE_BYTES {
        return Err("native source length is outside the existing file limit".to_owned());
    }
    let expected_count = u32::try_from(length.div_ceil(NATIVE_SOURCE_CHUNK_BYTES))
        .map_err(|_| "native source chunk count exceeds u32".to_owned())?;
    if expected_count != chunk_count {
        return Err("native source chunk count differs from byte length".to_owned());
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .map_err(|_| "native source reconstruction allocation failed".to_owned())?;
    for ordinal in 0..chunk_count {
        let key = product_native_frontier_chunk_key(project, profile, path, ordinal)?;
        let record = relation
            .lookup(&key)
            .map_err(|error| format!("read native source chunk: {error}"))?
            .ok_or_else(|| format!("native source chunk {ordinal} is missing: {path}"))?;
        let ProductNativeFrontierRecord::ContentChunk {
            project: chunk_project,
            profile: chunk_profile,
            path: chunk_path,
            ordinal: chunk_ordinal,
            bytes: chunk,
        } = record
        else {
            return Err("native source chunk key names a non-chunk row".to_owned());
        };
        let expected_len = (length - bytes.len()).min(NATIVE_SOURCE_CHUNK_BYTES);
        if chunk_project != project
            || chunk_profile != profile
            || chunk_path != path
            || chunk_ordinal != ordinal
            || chunk.len() != expected_len
        {
            return Err(format!("native source chunk identity is invalid: {path}"));
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.len() != length {
        return Err("native source chunks do not cover declared byte length".to_owned());
    }
    Ok(bytes)
}

fn validate_path(path: &str) -> Result<(), String> {
    if path.is_empty()
        || path.len() > MAX_PATH_BYTES
        || path.starts_with('/')
        || path.contains('\\')
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err("native frontier path is not a normalized project-relative path".to_owned());
    }
    Ok(())
}

fn encode_disposition(disposition: NativePathDisposition, mut push: impl FnMut(&[u8])) {
    match disposition {
        NativePathDisposition::Structural {
            content,
            source_identity,
        } => {
            push(&[1]);
            push(&content);
            push(source_identity.as_ref());
        }
        NativePathDisposition::ClangMalformed {
            content,
            source_identity,
            byte_length,
            chunk_count,
        } => {
            push(&[2]);
            push(&content);
            push(source_identity.as_ref());
            push(&byte_length.to_be_bytes());
            push(&chunk_count.to_be_bytes());
        }
        NativePathDisposition::CSharpMalformed {
            content,
            source_identity,
            byte_length,
            chunk_count,
        } => {
            push(&[4]);
            push(&content);
            push(source_identity.as_ref());
            push(&byte_length.to_be_bytes());
            push(&chunk_count.to_be_bytes());
        }
        NativePathDisposition::Refused(reason) => push(&[3, reason as u8]),
    }
}

impl Relation for ProductNativeFrontierRelation {
    const DOMAIN: u8 = 0x97;
    const TYPE: u16 = 10;
    const VERSION: u8 = 2;
    type Key = [u8; 32];
    type Value = ProductNativeFrontierRecord;

    fn encode_key(value: &Self::Key, output: &mut Vec<u8>) {
        output.extend_from_slice(value);
    }

    fn encode_value(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(MAGIC);
        match value {
            ProductNativeFrontierRecord::Profile {
                project,
                profile,
                package_target,
                path_count,
                disposition_digest,
            } => {
                output.push(1);
                output.extend_from_slice(project);
                output.extend_from_slice(&<[u8; 2]>::from(*profile));
                match package_target {
                    Some(package_target) => {
                        output.push(1);
                        let bytes = package_target.as_str().as_bytes();
                        output.extend_from_slice(
                            &u32::try_from(bytes.len())
                                .expect("PackageUrl is bounded to u32 bytes")
                                .to_be_bytes(),
                        );
                        output.extend_from_slice(bytes);
                    }
                    None => output.push(0),
                }
                output.extend_from_slice(&path_count.to_be_bytes());
                output.extend_from_slice(disposition_digest);
            }
            ProductNativeFrontierRecord::Path {
                project,
                profile,
                path,
                disposition,
            } => {
                output.push(2);
                output.extend_from_slice(project);
                output.extend_from_slice(&<[u8; 2]>::from(*profile));
                output.extend_from_slice(&(path.len() as u32).to_be_bytes());
                output.extend_from_slice(path.as_bytes());
                encode_disposition(*disposition, &mut |bytes: &[u8]| {
                    output.extend_from_slice(bytes);
                });
            }
            ProductNativeFrontierRecord::ContentChunk {
                project,
                profile,
                path,
                ordinal,
                bytes,
            } => {
                output.push(3);
                output.extend_from_slice(project);
                output.extend_from_slice(&<[u8; 2]>::from(*profile));
                output.extend_from_slice(&(path.len() as u32).to_be_bytes());
                output.extend_from_slice(path.as_bytes());
                output.extend_from_slice(&ordinal.to_be_bytes());
                output.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
                output.extend_from_slice(bytes);
            }
        }
    }
}

impl CanonicalRelation for ProductNativeFrontierRelation {
    fn decode_key(bytes: &[u8]) -> Result<Self::Key, RelationDecodeError> {
        bytes.try_into().map_err(|_| RelationDecodeError::Malformed)
    }

    fn decode_value(bytes: &[u8]) -> Result<Self::Value, RelationDecodeError> {
        decode_record(bytes).map_err(|_| RelationDecodeError::Malformed)
    }
}

fn decode_record(bytes: &[u8]) -> Result<ProductNativeFrontierRecord, ()> {
    if bytes.get(..4) != Some(MAGIC) {
        return Err(());
    }
    let mut cursor = 4;
    let tag = take(bytes, &mut cursor, 1)?[0];
    let project = take(bytes, &mut cursor, 32)?.try_into().map_err(|_| ())?;
    let profile_code: [u8; 2] = take(bytes, &mut cursor, 2)?.try_into().map_err(|_| ())?;
    let profile = LanguageProfile::try_from(profile_code).map_err(|_| ())?;
    let record = match tag {
        1 => {
            let package_target = match take(bytes, &mut cursor, 1)?[0] {
                0 => None,
                1 => {
                    let length = u32::from_be_bytes(
                        take(bytes, &mut cursor, 4)?.try_into().map_err(|_| ())?,
                    ) as usize;
                    if length > backend_semantic::vocabulary::MAX_PACKAGE_URL_BYTES {
                        return Err(());
                    }
                    let text =
                        std::str::from_utf8(take(bytes, &mut cursor, length)?).map_err(|_| ())?;
                    let package_target = PackageUrl::parse(text.to_owned()).map_err(|_| ())?;
                    if package_target.package_type().language() != profile.language() {
                        return Err(());
                    }
                    Some(package_target)
                }
                _ => return Err(()),
            };
            let path_count =
                u32::from_be_bytes(take(bytes, &mut cursor, 4)?.try_into().map_err(|_| ())?);
            let disposition_digest = take(bytes, &mut cursor, 32)?.try_into().map_err(|_| ())?;
            ProductNativeFrontierRecord::Profile {
                project,
                profile,
                package_target,
                path_count,
                disposition_digest,
            }
        }
        2 => {
            let path_len =
                u32::from_be_bytes(take(bytes, &mut cursor, 4)?.try_into().map_err(|_| ())?)
                    as usize;
            if path_len > MAX_PATH_BYTES {
                return Err(());
            }
            let path = std::str::from_utf8(take(bytes, &mut cursor, path_len)?)
                .map_err(|_| ())?
                .to_owned();
            validate_path(&path).map_err(|_| ())?;
            let tag = take(bytes, &mut cursor, 1)?[0];
            let disposition = match tag {
                1 | 2 | 4 => {
                    let content = take(bytes, &mut cursor, 32)?.try_into().map_err(|_| ())?;
                    let identity =
                        ContentId::<SourceFactDomain>::try_from(take(bytes, &mut cursor, 32)?)
                            .map_err(|_| ())?;
                    if tag == 1 {
                        NativePathDisposition::Structural {
                            content,
                            source_identity: identity,
                        }
                    } else {
                        let byte_length = u32::from_be_bytes(
                            take(bytes, &mut cursor, 4)?.try_into().map_err(|_| ())?,
                        );
                        let chunk_count = u32::from_be_bytes(
                            take(bytes, &mut cursor, 4)?.try_into().map_err(|_| ())?,
                        );
                        let byte_length_usize = usize::try_from(byte_length).map_err(|_| ())?;
                        if byte_length_usize == 0
                            || byte_length_usize > MAX_NATIVE_SOURCE_BYTES
                            || usize::try_from(chunk_count).map_err(|_| ())?
                                != byte_length_usize.div_ceil(NATIVE_SOURCE_CHUNK_BYTES)
                        {
                            return Err(());
                        }
                        if tag == 2 {
                            NativePathDisposition::ClangMalformed {
                                content,
                                source_identity: identity,
                                byte_length,
                                chunk_count,
                            }
                        } else {
                            NativePathDisposition::CSharpMalformed {
                                content,
                                source_identity: identity,
                                byte_length,
                                chunk_count,
                            }
                        }
                    }
                }
                3 => {
                    let reason = match take(bytes, &mut cursor, 1)?[0] {
                        1 => NativePathRefusal::InvalidUtf8,
                        2 => NativePathRefusal::MalformedOther,
                        3 => NativePathRefusal::ReadUnavailable,
                        4 => NativePathRefusal::Oversized,
                        _ => return Err(()),
                    };
                    NativePathDisposition::Refused(reason)
                }
                _ => return Err(()),
            };
            ProductNativeFrontierRecord::Path {
                project,
                profile,
                path,
                disposition,
            }
        }
        3 => {
            let path_len =
                u32::from_be_bytes(take(bytes, &mut cursor, 4)?.try_into().map_err(|_| ())?)
                    as usize;
            if path_len > MAX_PATH_BYTES {
                return Err(());
            }
            let path = std::str::from_utf8(take(bytes, &mut cursor, path_len)?)
                .map_err(|_| ())?
                .to_owned();
            validate_path(&path).map_err(|_| ())?;
            let ordinal =
                u32::from_be_bytes(take(bytes, &mut cursor, 4)?.try_into().map_err(|_| ())?);
            let byte_len =
                u16::from_be_bytes(take(bytes, &mut cursor, 2)?.try_into().map_err(|_| ())?)
                    as usize;
            if byte_len == 0 || byte_len > NATIVE_SOURCE_CHUNK_BYTES {
                return Err(());
            }
            ProductNativeFrontierRecord::ContentChunk {
                project,
                profile,
                path,
                ordinal,
                bytes: take(bytes, &mut cursor, byte_len)?.into(),
            }
        }
        _ => return Err(()),
    };
    if matches!(
        record,
        ProductNativeFrontierRecord::Path {
            profile,
            disposition: NativePathDisposition::ClangMalformed { .. },
            ..
        } if !matches!(profile, LanguageProfile::C(_) | LanguageProfile::Cxx(_))
    ) {
        return Err(());
    }
    if matches!(
        record,
        ProductNativeFrontierRecord::Path {
            profile,
            disposition: NativePathDisposition::CSharpMalformed { .. },
            ..
        } if !matches!(profile, LanguageProfile::CSharp(_))
    ) {
        return Err(());
    }
    if matches!(record, ProductNativeFrontierRecord::ContentChunk { profile, .. }
        if !matches!(profile, LanguageProfile::C(_) | LanguageProfile::Cxx(_) | LanguageProfile::CSharp(_)))
    {
        return Err(());
    }
    if cursor != bytes.len() {
        return Err(());
    }
    Ok(record)
}

fn take<'a>(bytes: &'a [u8], cursor: &mut usize, count: usize) -> Result<&'a [u8], ()> {
    let end = cursor.checked_add(count).ok_or(())?;
    let result = bytes.get(*cursor..end).ok_or(())?;
    *cursor = end;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_semantic::vocabulary::{CSharpVersion, CStandard, CxxStandard};

    #[test]
    fn native_frontier_records_round_trip_exact_profile_and_disposition() {
        let project = [0x37; 32];
        let profile = LanguageProfile::Cxx(CxxStandard::Cxx23);
        let package_target =
            PackageUrl::parse("pkg:generic/native-frontier-round-trip@1.0.0".to_owned()).unwrap();
        let identity = ContentId::<SourceFactDomain>::from_canonical_bytes(b"captured bytes");
        let records = [
            ProductNativeFrontierRecord::Profile {
                project,
                profile,
                package_target: Some(package_target),
                path_count: 2,
                disposition_digest: [0x62; 32],
            },
            ProductNativeFrontierRecord::Path {
                project,
                profile,
                path: "src/parsed.cpp".to_owned(),
                disposition: NativePathDisposition::Structural {
                    content: [0x18; 32],
                    source_identity: identity,
                },
            },
            ProductNativeFrontierRecord::Path {
                project,
                profile: LanguageProfile::CSharp(CSharpVersion::CSharp14),
                path: "src/raw.cs".to_owned(),
                disposition: NativePathDisposition::CSharpMalformed {
                    content: [0x39; 32],
                    source_identity: identity,
                    byte_length: 13,
                    chunk_count: 1,
                },
            },
            ProductNativeFrontierRecord::Path {
                project,
                profile,
                path: "src/raw.hpp".to_owned(),
                disposition: NativePathDisposition::ClangMalformed {
                    content: [0x29; 32],
                    source_identity: identity,
                    byte_length: 13,
                    chunk_count: 1,
                },
            },
        ];
        for record in records {
            let mut encoded = Vec::new();
            ProductNativeFrontierRelation::encode_value(&record, &mut encoded);
            assert_eq!(
                ProductNativeFrontierRelation::decode_value(&encoded),
                Ok(record)
            );
        }

        let invalid_target =
            PackageUrl::parse("pkg:nuget/native-frontier-wrong-language@1.0.0".to_owned()).unwrap();
        let invalid_record = ProductNativeFrontierRecord::Profile {
            project,
            profile,
            package_target: Some(invalid_target),
            path_count: 0,
            disposition_digest: [0; 32],
        };
        let mut encoded = Vec::new();
        ProductNativeFrontierRelation::encode_value(&invalid_record, &mut encoded);
        assert!(decode_record(&encoded).is_err());
        assert_eq!(
            LanguageProfile::try_from(<[u8; 2]>::from(LanguageProfile::C(CStandard::C23))),
            Ok(LanguageProfile::C(CStandard::C23))
        );
    }

    #[test]
    fn native_frontier_rejects_noncanonical_paths_and_unsorted_manifest_inputs() {
        for path in ["", "/src/a.cpp", "src/../a.cpp", "src//a.cpp", "src\\a.cpp"] {
            assert!(
                product_native_frontier_path_key([0; 32], LanguageProfile::C(CStandard::C23), path)
                    .is_err()
            );
        }
        assert!(
            product_native_frontier_digest([
                (
                    "src/z.cpp",
                    NativePathDisposition::Refused(NativePathRefusal::InvalidUtf8)
                ),
                (
                    "src/a.cpp",
                    NativePathDisposition::Refused(NativePathRefusal::InvalidUtf8)
                ),
            ])
            .is_err()
        );
        assert!(
            build_native_frontier_records(
                [0; 32],
                LanguageProfile::Python(backend_semantic::vocabulary::PythonVersion::Python314),
                vec![(
                    "src/raw.py".to_owned(),
                    NativePathDisposition::ClangMalformed {
                        content: [1; 32],
                        source_identity: ContentId::<SourceFactDomain>::from_canonical_bytes(b"py"),
                        byte_length: 2,
                        chunk_count: 1,
                    },
                )],
            )
            .is_err()
        );
        assert!(
            build_native_frontier_records(
                [0; 32],
                LanguageProfile::C(CStandard::C23),
                vec![(
                    "src/raw.cs".to_owned(),
                    NativePathDisposition::CSharpMalformed {
                        content: [1; 32],
                        source_identity: ContentId::<SourceFactDomain>::from_canonical_bytes(
                            b"csharp"
                        ),
                        byte_length: 6,
                        chunk_count: 1,
                    },
                )],
            )
            .is_err()
        );
        assert!(
            build_native_frontier_records([0; 32], LanguageProfile::C(CStandard::C23), Vec::new(),)
                .is_err()
        );
    }

    #[test]
    fn profile_frontier_is_content_bound_and_refusals_remain_non_compilable() {
        let project = [0x51; 32];
        let profile = LanguageProfile::C(CStandard::C23);
        let candidate_identity =
            ContentId::<SourceFactDomain>::from_canonical_bytes(b"raw C bytes");
        let candidate_bytes = b"raw C bytes".to_vec();
        let candidate_content =
            backend_compile::typed_of::<backend_compile::InputContentSchema>(&candidate_bytes)
                .to_bytes();
        let parsed_identity =
            ContentId::<SourceFactDomain>::from_canonical_bytes(b"parsed C bytes");
        let rows = build_native_frontier_records_with_content(
            project,
            profile,
            vec![
                (
                    "src/a.c".to_owned(),
                    NativePathDisposition::ClangMalformed {
                        content: candidate_content,
                        source_identity: candidate_identity,
                        byte_length: 0,
                        chunk_count: 0,
                    },
                ),
                (
                    "src/b.c".to_owned(),
                    NativePathDisposition::Structural {
                        content: [0x22; 32],
                        source_identity: parsed_identity,
                    },
                ),
            ],
            vec![("src/a.c".to_owned(), candidate_bytes.clone())],
        )
        .expect("bounded profile rows");
        assert_eq!(rows.len(), 4);
        assert_eq!(
            rows[0].0,
            product_native_frontier_profile_key(project, profile)
        );
        assert_ne!(rows[1].0, rows[2].0);

        let (digest, count) = product_native_frontier_digest([
            (
                "src/a.c",
                NativePathDisposition::ClangMalformed {
                    content: candidate_content,
                    source_identity: candidate_identity,
                    byte_length: 11,
                    chunk_count: 1,
                },
            ),
            (
                "src/b.c",
                NativePathDisposition::Refused(NativePathRefusal::ReadUnavailable),
            ),
        ])
        .expect("complete disposition digest");
        assert_eq!(count, 2);
        let ProductNativeFrontierRecord::Path {
            disposition: NativePathDisposition::ClangMalformed { content, .. },
            ..
        } = rows[1].1.clone()
        else {
            panic!("first path is the malformed Clang candidate");
        };
        assert_eq!(content, candidate_content);
        // The second profile has an explicit refusal, so it can be complete
        // as an observation but cannot claim complete compiler coverage.
        let refused_rows = build_native_frontier_records_with_content(
            project,
            profile,
            vec![
                (
                    "src/a.c".to_owned(),
                    NativePathDisposition::ClangMalformed {
                        content: candidate_content,
                        source_identity: candidate_identity,
                        byte_length: 0,
                        chunk_count: 0,
                    },
                ),
                (
                    "src/b.c".to_owned(),
                    NativePathDisposition::Refused(NativePathRefusal::ReadUnavailable),
                ),
            ],
            vec![("src/a.c".to_owned(), candidate_bytes)],
        )
        .expect("explicit refusal rows");
        assert_eq!(
            refused_rows[0].1,
            ProductNativeFrontierRecord::Profile {
                project,
                profile,
                package_target: None,
                path_count: count,
                disposition_digest: digest,
            }
        );
    }

    #[test]
    fn malformed_content_chunks_reject_duplicate_and_unbound_payloads() {
        let bytes = b"int main(void) { return 0; }".to_vec();
        let content =
            backend_compile::typed_of::<backend_compile::InputContentSchema>(&bytes).to_bytes();
        let identity = ContentId::<SourceFactDomain>::from_canonical_bytes(&bytes);
        let disposition = || NativePathDisposition::ClangMalformed {
            content,
            source_identity: identity,
            byte_length: 0,
            chunk_count: 0,
        };
        assert!(
            build_native_frontier_records_with_content(
                [1; 32],
                LanguageProfile::C(CStandard::C23),
                vec![("src/a.c".to_owned(), disposition())],
                vec![
                    ("src/a.c".to_owned(), bytes.clone()),
                    ("src/a.c".to_owned(), bytes.clone()),
                ],
            )
            .is_err()
        );
        assert!(
            build_native_frontier_records_with_content(
                [1; 32],
                LanguageProfile::C(CStandard::C23),
                vec![("src/a.c".to_owned(), disposition())],
                vec![("src/other.c".to_owned(), bytes)],
            )
            .is_err()
        );
    }

    #[test]
    fn native_clang_permit_rejects_wrong_project_root_profile_and_bytes() {
        let project = [0x31; 32];
        let root = [0x42; 32];
        let profile = LanguageProfile::C(CStandard::C23);
        let bytes = b"int x = 1;";
        let package_target =
            PackageUrl::parse("pkg:generic/native-owner-a@1.0.0".to_owned()).unwrap();
        let frontier = CheckedNativeFrontier {
            project,
            profile,
            root,
            package_target: Some(package_target.clone()),
            dispositions: vec![(
                "src/x.c".to_owned(),
                NativePathDisposition::ClangMalformed {
                    content: backend_compile::typed_of::<backend_compile::InputContentSchema>(
                        bytes,
                    )
                    .to_bytes(),
                    source_identity: ContentId::<SourceFactDomain>::from_canonical_bytes(bytes),
                    byte_length: bytes.len() as u32,
                    chunk_count: 1,
                },
            )],
        };
        let permit = frontier
            .candidate_permit("src/x.c")
            .expect("malformed permit");
        assert!(permit.admits(project, root, &package_target, "src/x.c", profile, bytes));
        assert!(!permit.admits([0x32; 32], root, &package_target, "src/x.c", profile, bytes));
        assert!(!permit.admits(
            project,
            [0x43; 32],
            &package_target,
            "src/x.c",
            profile,
            bytes
        ));
        let other_target =
            PackageUrl::parse("pkg:generic/native-owner-b@1.0.0".to_owned()).unwrap();
        assert!(!permit.admits(project, root, &other_target, "src/x.c", profile, bytes));
        assert!(!permit.admits(
            project,
            root,
            &package_target,
            "src/x.c",
            LanguageProfile::C(CStandard::C17),
            bytes
        ));
        assert!(!permit.admits(
            project,
            root,
            &package_target,
            "src/x.c",
            profile,
            b"int x = 2;"
        ));
    }

    #[test]
    fn native_csharp_permit_is_profile_specific_and_binds_exact_bytes_and_root() {
        let project = [0x71; 32];
        let root = [0x82; 32];
        let profile = LanguageProfile::CSharp(CSharpVersion::CSharp14);
        let bytes = b"class A {";
        let content =
            backend_compile::typed_of::<backend_compile::InputContentSchema>(bytes).to_bytes();
        let identity = ContentId::<SourceFactDomain>::from_canonical_bytes(bytes);
        let package_target =
            PackageUrl::parse("pkg:nuget/native-owner-cs-a@1.0.0".to_owned()).unwrap();
        let frontier = CheckedNativeFrontier {
            project,
            profile,
            root,
            package_target: Some(package_target.clone()),
            dispositions: vec![(
                "src/a.cs".to_owned(),
                NativePathDisposition::CSharpMalformed {
                    content,
                    source_identity: identity,
                    byte_length: bytes.len() as u32,
                    chunk_count: 1,
                },
            )],
        };
        let permit = frontier
            .csharp_candidate_permit("src/a.cs")
            .expect("C# malformed permit");
        assert_eq!(permit.path(), "src/a.cs");
        assert_eq!(permit.profile(), profile);
        assert_eq!(permit.selected_root(), root);
        assert!(permit.admits(project, root, &package_target, "src/a.cs", profile, bytes));
        assert!(!permit.admits(
            [0x72; 32],
            root,
            &package_target,
            "src/a.cs",
            profile,
            bytes
        ));
        assert!(!permit.admits(
            project,
            [0x83; 32],
            &package_target,
            "src/a.cs",
            profile,
            bytes
        ));
        assert!(!permit.admits(project, root, &package_target, "src/b.cs", profile, bytes));
        let other_target =
            PackageUrl::parse("pkg:nuget/native-owner-cs-b@1.0.0".to_owned()).unwrap();
        assert!(!permit.admits(project, root, &other_target, "src/a.cs", profile, bytes));
        assert!(!permit.admits(
            project,
            root,
            &package_target,
            "src/a.cs",
            LanguageProfile::CSharp(CSharpVersion::CSharp13),
            bytes
        ));
        assert!(!permit.admits(
            project,
            root,
            &package_target,
            "src/a.cs",
            profile,
            b"class B {"
        ));
        assert!(frontier.candidate_permit("src/a.cs").is_none());

        let wrong_language = CheckedNativeFrontier {
            project,
            profile: LanguageProfile::C(CStandard::C23),
            root,
            package_target: Some(
                PackageUrl::parse("pkg:generic/native-owner-cs-wrong-profile@1.0.0".to_owned())
                    .unwrap(),
            ),
            dispositions: vec![(
                "src/a.cs".to_owned(),
                NativePathDisposition::CSharpMalformed {
                    content,
                    source_identity: identity,
                    byte_length: bytes.len() as u32,
                    chunk_count: 1,
                },
            )],
        };
        assert!(wrong_language.csharp_candidate_permit("src/a.cs").is_none());
    }

    #[test]
    fn native_candidate_constructors_reject_a_permit_for_another_package_url() {
        use crate::application::{OwnedPackageSource, OwnedPackageSourceSet};
        use backend_library::interface::{CorrelationId, GenerateTarget, PackageCompileRequest};
        use backend_semantic::vocabulary::{CxxStandard, Stage};
        use std::path::PathBuf;

        let project = [0xa1; 32];
        let root = [0xb2; 32];
        let source_root = PathBuf::from(std::env::temp_dir());
        let source = "class Broken {\n";

        let csharp_profile = LanguageProfile::CSharp(CSharpVersion::CSharp14);
        let csharp_target =
            PackageUrl::parse("pkg:nuget/permit-owner-cs-a@1.0.0".to_owned()).unwrap();
        let csharp_frontier = CheckedNativeFrontier {
            project,
            profile: csharp_profile,
            root,
            package_target: Some(csharp_target.clone()),
            dispositions: vec![(
                "src/broken.cs".to_owned(),
                NativePathDisposition::CSharpMalformed {
                    content: backend_compile::typed_of::<backend_compile::InputContentSchema>(
                        source.as_bytes(),
                    )
                    .to_bytes(),
                    source_identity: ContentId::<SourceFactDomain>::from_canonical_bytes(
                        source.as_bytes(),
                    ),
                    byte_length: source.len() as u32,
                    chunk_count: 1,
                },
            )],
        };
        let csharp_permit = csharp_frontier
            .csharp_candidate_permit("src/broken.cs")
            .unwrap();
        let csharp_request = |url: PackageUrl| {
            PackageCompileRequest::new(
                GenerateTarget {
                    correlation: CorrelationId(0x51),
                    profile: csharp_profile,
                    stage: Stage::LowerIr,
                },
                url,
            )
            .unwrap()
        };
        assert!(
            OwnedPackageSourceSet::new_native_csharp_candidate(
                csharp_request(csharp_target),
                source_root.clone(),
                vec![OwnedPackageSource::new("src/broken.cs", source).unwrap()].into_boxed_slice(),
                project,
                root,
                vec![csharp_permit.clone()].into_boxed_slice(),
            )
            .is_ok()
        );
        let csharp_request_b = csharp_request(
            PackageUrl::parse("pkg:nuget/permit-owner-cs-b@1.0.0".to_owned()).unwrap(),
        );
        assert!(!csharp_permit.admits(
            project,
            root,
            csharp_request_b.as_ref(),
            "src/broken.cs",
            csharp_profile,
            source.as_bytes(),
        ));
        assert!(matches!(
            OwnedPackageSourceSet::new_native_csharp_candidate(
                csharp_request_b,
                source_root.clone(),
                vec![OwnedPackageSource::new("src/broken.cs", source).unwrap()].into_boxed_slice(),
                project,
                root,
                vec![csharp_permit].into_boxed_slice(),
            ),
            Err(crate::application::PackageSourceSetError::NativeCSharpProfileMismatch)
        ));

        let clang_profile = LanguageProfile::Cxx(CxxStandard::Cxx23);
        let clang_target =
            PackageUrl::parse("pkg:generic/permit-owner-cxx-a@1.0.0".to_owned()).unwrap();
        let clang_source = "class Broken {\n";
        let clang_frontier = CheckedNativeFrontier {
            project,
            profile: clang_profile,
            root,
            package_target: Some(clang_target.clone()),
            dispositions: vec![(
                "src/broken.cpp".to_owned(),
                NativePathDisposition::ClangMalformed {
                    content: backend_compile::typed_of::<backend_compile::InputContentSchema>(
                        clang_source.as_bytes(),
                    )
                    .to_bytes(),
                    source_identity: ContentId::<SourceFactDomain>::from_canonical_bytes(
                        clang_source.as_bytes(),
                    ),
                    byte_length: clang_source.len() as u32,
                    chunk_count: 1,
                },
            )],
        };
        let clang_permit = clang_frontier.candidate_permit("src/broken.cpp").unwrap();
        let clang_request = |url: PackageUrl| {
            PackageCompileRequest::new(
                GenerateTarget {
                    correlation: CorrelationId(0x52),
                    profile: clang_profile,
                    stage: Stage::LowerIr,
                },
                url,
            )
            .unwrap()
        };
        assert!(
            OwnedPackageSourceSet::new_native_clang_candidate(
                clang_request(clang_target),
                source_root.clone(),
                vec![OwnedPackageSource::new("src/broken.cpp", clang_source).unwrap()]
                    .into_boxed_slice(),
                project,
                root,
                vec![clang_permit.clone()].into_boxed_slice(),
            )
            .is_ok()
        );
        let clang_request_b = clang_request(
            PackageUrl::parse("pkg:generic/permit-owner-cxx-b@1.0.0".to_owned()).unwrap(),
        );
        assert!(!clang_permit.admits(
            project,
            root,
            clang_request_b.as_ref(),
            "src/broken.cpp",
            clang_profile,
            clang_source.as_bytes(),
        ));
        assert!(matches!(
            OwnedPackageSourceSet::new_native_clang_candidate(
                clang_request_b,
                source_root,
                vec![OwnedPackageSource::new("src/broken.cpp", clang_source).unwrap()]
                    .into_boxed_slice(),
                project,
                root,
                vec![clang_permit].into_boxed_slice(),
            ),
            Err(crate::application::PackageSourceSetError::NativeClangProfileMismatch)
        ));
    }

    #[test]
    fn csharp_malformed_payload_requires_utf8_and_exact_bounded_chunks() {
        let project = [0x91; 32];
        let profile = LanguageProfile::CSharp(CSharpVersion::CSharp14);
        let bytes = vec![b'x'; NATIVE_SOURCE_CHUNK_BYTES + 7];
        let identity = ContentId::<SourceFactDomain>::from_canonical_bytes(&bytes);
        let content =
            backend_compile::typed_of::<backend_compile::InputContentSchema>(&bytes).to_bytes();
        let rows = build_native_frontier_records_with_content(
            project,
            profile,
            vec![(
                "src/raw.cs".to_owned(),
                NativePathDisposition::CSharpMalformed {
                    content,
                    source_identity: identity,
                    byte_length: 0,
                    chunk_count: 0,
                },
            )],
            vec![("src/raw.cs".to_owned(), bytes.clone())],
        )
        .expect("bounded C# candidate content");
        assert_eq!(rows.len(), 4, "manifest, path, and two exact chunks");
        assert!(rows.iter().any(|(_, record)| matches!(
            record,
            ProductNativeFrontierRecord::ContentChunk {
                profile: row_profile,
                path,
                ordinal: 1,
                bytes,
                ..
            } if *row_profile == profile && path == "src/raw.cs" && bytes.len() == 7
        )));

        let invalid_utf8 = vec![0xff];
        assert!(
            build_native_frontier_records_with_content(
                project,
                profile,
                vec![(
                    "src/raw.cs".to_owned(),
                    NativePathDisposition::CSharpMalformed {
                        content: backend_compile::typed_of::<backend_compile::InputContentSchema>(
                            &invalid_utf8,
                        )
                        .to_bytes(),
                        source_identity: ContentId::<SourceFactDomain>::from_canonical_bytes(
                            &invalid_utf8,
                        ),
                        byte_length: 0,
                        chunk_count: 0,
                    },
                )],
                vec![("src/raw.cs".to_owned(), invalid_utf8)],
            )
            .is_err()
        );
    }
}
