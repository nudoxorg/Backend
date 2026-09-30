//! Portable V3 typed-history claims, locators, and durable catalog adapters.
//!
//! The V3 locator binds only the canonical manifest and semantic-to-FileStore
//! object bridge. It deliberately does not contain a closure, commit, or ref;
//! the root claim binds the locator identity and exact closure without a hash
//! cycle. Durable records use the shared history commit DAG, while ref
//! publication still requires proof tied to a same-store verifier pin and
//! selected input witness.

use super::catalog::validate_history_commit_node;
use super::codec::{
    append_commit_index, identify_history_record, prepare_history_layout,
    prepare_history_layout_typed,
};
use super::v2::{create_typed_v2_locator, decode_typed_v2_locator};
use super::*;
use backend_semantic::ir::SemanticPlaneImageKey;

const TYPED_V3_ROOT_DISCRIMINATOR: u8 = 3;
const TYPED_V3_ROOT_DOMAIN: &[u8] = b"backend.semantic.history-typed-v3-root-claim.v1\0";
const TYPED_V3_LOCATOR_TAG: u8 = 16;
const TYPED_V3_LOCATOR_DOMAIN: &[u8] = b"backend.semantic.history-typed-v3-locator.v1\0";
const MAX_TYPED_V3_LOCATOR_BYTES: usize = MAX_HISTORY_TYPED_V2_LOCATOR_BYTES + 128;
const MAX_TYPED_V3_ROOT_CLAIM_BYTES: usize = 1 + 4 * 32 + CHECKSUM_BYTES;
const MAX_TYPED_V3_PENDING_RECONCILE: usize = super::MAX_HISTORY_GC_BATCH_RECORDS / 2;

struct FencedSelectedGenerationSource<'fence> {
    fence: &'fence dyn crate::SelectedNativeImagePublicationFence,
}

impl SelectedGenerationSource for FencedSelectedGenerationSource<'_> {
    type Error = &'static str;

    fn current_selected_generation(&mut self) -> Result<SelectedGenerationStamp, Self::Error> {
        Ok(self.fence.selected_stamp())
    }

    fn selected_image_is_current(
        &mut self,
        expected_stamp: SelectedGenerationStamp,
        image: SemanticPlaneImageKey,
    ) -> Result<bool, Self::Error> {
        Ok(expected_stamp == self.fence.selected_stamp() && image == self.fence.selected_image())
    }
}

/// Portable identity of one immutable V3 manifest/object-bridge locator.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct HistoryTypedV3LocatorId([u8; 32]);

impl HistoryTypedV3LocatorId {
    /// Returns the fixed-width locator identity bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Wraps an untrusted locator identity claim.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

/// Portable, claim-only typed V3 semantic root.
///
/// This value records untrusted roots, one exact FileStore closure, and the
/// content-addressed V3 locator. It is not a `HistoryGenerationRoot` and does
/// not authorize history commit or ref publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HistoryTypedV3RootClaim {
    content_root: backend_semantic::ir::UntrustedSemanticContentRootV2,
    generation_root: backend_semantic::ir::UntrustedSemanticGenerationRootV2,
    closure: ArtifactClosureClaim,
    locator: HistoryTypedV3LocatorId,
}

impl std::hash::Hash for HistoryTypedV3RootClaim {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::hash::Hash::hash(&self.content_root, state);
        std::hash::Hash::hash(&self.generation_root, state);
        std::hash::Hash::hash(self.closure.as_bytes(), state);
        std::hash::Hash::hash(&self.locator, state);
    }
}

impl HistoryTypedV3RootClaim {
    pub(super) const fn from_claims(
        content_root: backend_semantic::ir::UntrustedSemanticContentRootV2,
        generation_root: backend_semantic::ir::UntrustedSemanticGenerationRootV2,
        closure: ArtifactClosureClaim,
        locator: HistoryTypedV3LocatorId,
    ) -> Self {
        Self {
            content_root,
            generation_root,
            closure,
            locator,
        }
    }

    pub(crate) fn from_verified(
        content: &backend_semantic::ir::VerifiedTypedPlaneHistoryContentV3,
        closure: ArtifactClosureClaim,
        locator: HistoryTypedV3LocatorId,
    ) -> Self {
        Self {
            content_root: backend_semantic::ir::UntrustedSemanticContentRootV2::from_wire_claim(
                *content.content_root().as_bytes(),
            ),
            generation_root:
                backend_semantic::ir::UntrustedSemanticGenerationRootV2::from_wire_claim(
                    *content.generation_root().as_bytes(),
                ),
            closure,
            locator,
        }
    }

    /// Returns the V3 content-root claim for cold verification.
    #[must_use]
    pub const fn content_root_claim(self) -> backend_semantic::ir::UntrustedSemanticContentRootV2 {
        self.content_root
    }

    /// Returns the V3 generation-root claim for cold verification.
    #[must_use]
    pub const fn generation_root_claim(
        self,
    ) -> backend_semantic::ir::UntrustedSemanticGenerationRootV2 {
        self.generation_root
    }

    /// Returns the exact FileStore closure claim bound by this root.
    #[must_use]
    pub const fn closure(self) -> ArtifactClosureClaim {
        self.closure
    }

    /// Returns the canonical V3 manifest/object-bridge locator identity.
    #[must_use]
    pub const fn locator(self) -> HistoryTypedV3LocatorId {
        self.locator
    }

    /// Encodes this claim with the reserved V3 root discriminator and a
    /// domain-separated checksum. Decoding it yields only portable claims;
    /// closure verification and selected-generation binding remain separate.
    pub fn encode_portable(self) -> Result<Vec<u8>, String> {
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(MAX_TYPED_V3_ROOT_CLAIM_BYTES)
            .map_err(|_| "typed V3 root claim allocation failed".to_owned())?;
        bytes.push(TYPED_V3_ROOT_DISCRIMINATOR);
        bytes.extend_from_slice(self.content_root.as_bytes());
        bytes.extend_from_slice(self.generation_root.as_bytes());
        bytes.extend_from_slice(self.closure.as_bytes());
        bytes.extend_from_slice(self.locator.as_bytes());
        let mut hasher = blake3::Hasher::new();
        hasher.update(TYPED_V3_ROOT_DOMAIN);
        hasher.update(&bytes);
        bytes.extend_from_slice(hasher.finalize().as_bytes());
        Ok(bytes)
    }

    /// Decodes an untrusted portable V3 claim. It does not validate the
    /// referenced closure or locator and cannot produce admission authority.
    pub fn decode_portable(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() != MAX_TYPED_V3_ROOT_CLAIM_BYTES
            || bytes.first().copied() != Some(TYPED_V3_ROOT_DISCRIMINATOR)
        {
            return Err("typed V3 root claim discriminator or length is invalid".to_owned());
        }
        let checksum_offset = bytes.len() - CHECKSUM_BYTES;
        let mut hasher = blake3::Hasher::new();
        hasher.update(TYPED_V3_ROOT_DOMAIN);
        hasher.update(&bytes[..checksum_offset]);
        if hasher.finalize().as_bytes() != &bytes[checksum_offset..] {
            return Err("typed V3 root claim checksum failed".to_owned());
        }
        let mut offset = 1;
        let mut take_root = || {
            let end = offset + 32;
            let root = bytes[offset..end]
                .try_into()
                .map_err(|_| "typed V3 root claim field has the wrong width".to_owned());
            offset = end;
            root
        };
        let content_root =
            backend_semantic::ir::UntrustedSemanticContentRootV2::from_wire_claim(take_root()?);
        let generation_root =
            backend_semantic::ir::UntrustedSemanticGenerationRootV2::from_wire_claim(take_root()?);
        let closure = ArtifactClosureClaim::from_bytes(take_root()?);
        let locator = HistoryTypedV3LocatorId(take_root()?);
        if offset != checksum_offset {
            return Err("typed V3 root claim has trailing fields".to_owned());
        }
        Ok(Self {
            content_root,
            generation_root,
            closure,
            locator,
        })
    }
}

/// Canonical V3 locator derived from a complete producer receipt set.
///
/// Its nested V2 bridge body intentionally reuses the existing canonical map
/// validator. The outer V3 tag and hash domain are distinct, so V3 metadata
/// cannot be mistaken for a V2 history locator.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TypedV3HistoryLocator {
    bridge: TypedV2HistoryLocator,
}

impl TypedV3HistoryLocator {
    /// Derives the exact locator from all durable receipts emitted by the
    /// complete typed V3 producer. Duplicate rope occurrences are reduced to
    /// one semantic object mapping; conflicting aliases fail closed.
    pub(crate) fn from_produced(
        produced: &crate::ProducedSemanticTypedPlaneV3,
    ) -> Result<Self, String> {
        Self::from_receipts(
            produced.manifest(),
            produced.segment_admissions(),
            produced.jumbo_admissions(),
        )
    }

    pub(crate) fn from_selected_native_history(
        produced: &crate::ir_producer_store::ProducedSelectedNativeTypedPlaneHistoryV3,
    ) -> Result<Self, String> {
        Self::from_receipts(
            produced.manifest(),
            produced.segment_admissions(),
            produced.jumbo_admissions(),
        )
    }

    pub(crate) fn from_receipts(
        produced_manifest: &backend_semantic::ir::SemanticTypedPlaneManifestV2,
        segment_receipts: &[crate::DurableSemanticObjectAdmission],
        jumbo_receipts: &[crate::DurableSemanticObjectAdmission],
    ) -> Result<Self, String> {
        use crate::{ProducedSemanticObjectIdentity, ProducedSemanticObjectKind};
        use std::collections::BTreeMap;

        let manifest = produced_manifest
            .canonical_bytes()
            .map_err(|error| format!("encode typed V3 history manifest: {error}"))?;
        let expected_segments = produced_manifest
            .resource_usage()
            .map_err(|error| format!("measure typed V3 history manifest: {error}"))?
            .segment_descriptors();
        TypedV2HistoryLocator::preflight_admission_counts(
            expected_segments,
            segment_receipts.len(),
            0,
        )?;

        let mut segments = Vec::new();
        segments
            .try_reserve_exact(segment_receipts.len())
            .map_err(|_| "typed V3 history segment map allocation failed".to_owned())?;
        for receipt in segment_receipts {
            let ProducedSemanticObjectIdentity::Segment { id, .. } = receipt.identity() else {
                return Err("typed V3 segment receipt has a non-segment identity".to_owned());
            };
            if receipt.identity().kind() != ProducedSemanticObjectKind::Segment {
                return Err("typed V3 segment receipt kind is inconsistent".to_owned());
            }
            segments.push(HistoryTypedV2SegmentObject::new(
                backend_semantic::ir::UntrustedSemanticSegmentId::from_raw(*id.as_bytes()),
                UntrustedObjectId::from_bytes(*receipt.object_id().as_bytes()),
                receipt.payload_bytes(),
            ));
        }

        let mut jumbo_by_semantic = BTreeMap::new();
        for receipt in jumbo_receipts {
            let (id, kind) = match receipt.identity() {
                ProducedSemanticObjectIdentity::JumboLeaf { id, .. } => {
                    (id, backend_semantic::ir::JumboRopeObjectKind::Leaf)
                }
                ProducedSemanticObjectIdentity::JumboInterior { id, .. } => {
                    (id, backend_semantic::ir::JumboRopeObjectKind::Interior)
                }
                ProducedSemanticObjectIdentity::Segment { .. } => {
                    return Err("typed V3 jumbo receipt has a segment identity".to_owned());
                }
            };
            let expected_kind = match kind {
                backend_semantic::ir::JumboRopeObjectKind::Leaf => {
                    ProducedSemanticObjectKind::JumboLeaf
                }
                backend_semantic::ir::JumboRopeObjectKind::Interior => {
                    ProducedSemanticObjectKind::JumboInterior
                }
            };
            if receipt.identity().kind() != expected_kind {
                return Err("typed V3 jumbo receipt kind is inconsistent".to_owned());
            }
            let mapping = HistoryTypedV2JumboObject::new(
                id,
                kind,
                UntrustedObjectId::from_bytes(*receipt.object_id().as_bytes()),
                receipt.payload_bytes(),
            );
            let key = (
                match kind {
                    backend_semantic::ir::JumboRopeObjectKind::Leaf => 0_u8,
                    backend_semantic::ir::JumboRopeObjectKind::Interior => 1_u8,
                },
                *id.as_bytes(),
            );
            if let Some(previous) = jumbo_by_semantic.insert(key, mapping) {
                if previous != mapping {
                    return Err(
                        "typed V3 repeated rope identity has conflicting durable receipts"
                            .to_owned(),
                    );
                }
            }
        }
        if segments.len().saturating_add(jumbo_by_semantic.len())
            > MAX_HISTORY_TYPED_V2_LOCATOR_OBJECTS
        {
            return Err("typed V3 history locator exceeds its object bound".to_owned());
        }
        let mut jumbo = Vec::new();
        jumbo
            .try_reserve_exact(jumbo_by_semantic.len())
            .map_err(|_| "typed V3 history rope map allocation failed".to_owned())?;
        jumbo.extend(jumbo_by_semantic.into_values());
        let bridge = TypedV2HistoryLocator::from_admission_parts(
            manifest,
            expected_segments,
            &segments,
            &jumbo,
            None,
        )?;
        if bridge.validate()? != *produced_manifest {
            return Err("typed V3 history locator differs from producer manifest".to_owned());
        }
        Ok(Self { bridge })
    }

    pub(crate) fn from_v2_bridge(bridge: TypedV2HistoryLocator) -> Result<Self, String> {
        if bridge.lineage_edge_set.is_some() || bridge.wire_revision != HISTORY_TYPED_V2_LOCATOR_TAG
        {
            return Err("typed V3 locator cannot carry V2-only lineage metadata".to_owned());
        }
        let _ = bridge.validate()?;
        Ok(Self { bridge })
    }

    pub(crate) fn bridge(&self) -> &TypedV2HistoryLocator {
        &self.bridge
    }

    pub(crate) fn validate(
        &self,
    ) -> Result<backend_semantic::ir::SemanticTypedPlaneManifestV2, String> {
        self.bridge.validate()
    }

    pub(crate) fn identity(&self) -> Result<HistoryTypedV3LocatorId, String> {
        Ok(typed_v3_locator_identity(&self.encode_body()?))
    }

    fn encode_body(&self) -> Result<Vec<u8>, String> {
        let _ = self.validate()?;
        let (_, bridge_bytes) = create_typed_v2_locator(self.bridge.clone())?;
        let mut writer = Writer::new(MAX_TYPED_V3_LOCATOR_BYTES);
        writer.header(TYPED_V3_LOCATOR_TAG)?;
        writer.sized_bytes(
            &bridge_bytes,
            MAX_HISTORY_TYPED_V2_LOCATOR_BYTES.saturating_add(64),
        )?;
        let body = writer.finish();
        if body.len() > MAX_TYPED_V3_LOCATOR_BYTES {
            return Err("typed V3 history locator exceeds its byte bound".to_owned());
        }
        Ok(body)
    }

    pub(crate) fn encode(&self) -> Result<Vec<u8>, String> {
        let body = self.encode_body()?;
        let identity = typed_v3_locator_identity(&body);
        let total = body
            .len()
            .checked_add(32 + CHECKSUM_BYTES)
            .ok_or_else(|| "typed V3 history locator length overflows".to_owned())?;
        if total > MAX_TYPED_V3_LOCATOR_BYTES + 32 + CHECKSUM_BYTES {
            return Err("typed V3 history locator exceeds its byte bound".to_owned());
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(total)
            .map_err(|_| "typed V3 history locator allocation failed".to_owned())?;
        bytes.extend_from_slice(identity.as_bytes());
        bytes.extend_from_slice(&body);
        bytes.extend_from_slice(blake3::hash(&bytes).as_bytes());
        Ok(bytes)
    }

    pub(crate) fn decode(bytes: &[u8], expected: HistoryTypedV3LocatorId) -> Result<Self, String> {
        let body = checked_body(bytes, MAX_TYPED_V3_LOCATOR_BYTES + 32 + CHECKSUM_BYTES)?;
        if body.len() < 32 || body[..32] != *expected.as_bytes() {
            return Err("typed V3 locator identity differs from its record prefix".to_owned());
        }
        let content = &body[32..];
        if typed_v3_locator_identity(content) != expected {
            return Err("typed V3 locator identity does not match its bytes".to_owned());
        }
        let mut reader = Reader::new(content);
        reader.header(TYPED_V3_LOCATOR_TAG)?;
        let bridge_bytes =
            reader.sized_bytes(MAX_HISTORY_TYPED_V2_LOCATOR_BYTES.saturating_add(64))?;
        reader.finish()?;
        if bridge_bytes.len() < 32 {
            return Err("typed V3 nested bridge locator is truncated".to_owned());
        }
        let bridge_id = HistoryTypedV2LocatorId(
            bridge_bytes[..32]
                .try_into()
                .map_err(|_| "typed V3 nested bridge ID has the wrong width".to_owned())?,
        );
        let locator = Self::from_v2_bridge(decode_typed_v2_locator(bridge_bytes, bridge_id)?)?;
        if locator.encode_body()?.as_slice() != content {
            return Err("typed V3 history locator is not canonically encoded".to_owned());
        }
        Ok(locator)
    }
}

/// Snapshot of immutable V3 metadata used while a cold publication scan runs
/// outside the history state lock.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TypedV3HistoryPublicationSnapshot {
    identity: HistoryCommitId,
    claim: HistoryTypedV3RootClaim,
    locator: TypedV3HistoryLocator,
    payload_root: HistoryPayloadRoot,
}

impl TypedV3HistoryPublicationSnapshot {
    pub(crate) const fn identity(&self) -> HistoryCommitId {
        self.identity
    }

    pub(crate) const fn claim(&self) -> HistoryTypedV3RootClaim {
        self.claim
    }

    pub(crate) fn locator(&self) -> &TypedV3HistoryLocator {
        &self.locator
    }
}

fn locator_path(target_root: &Path, commit: HistoryCommitId) -> PathBuf {
    target_root
        .join("history")
        .join("typed-v3-locators")
        .join(format!("{}.locator", hex(commit.as_bytes())))
}

fn pending_locator_directory(target_root: &Path) -> PathBuf {
    target_root
        .join("history")
        .join("typed-v3-locators")
        .join("pending")
}

fn pending_locator_path(target_root: &Path, commit: HistoryCommitId) -> PathBuf {
    pending_locator_directory(target_root).join(format!("{}.pending", hex(commit.as_bytes())))
}

pub(super) fn remove_typed_v3_locator_for_commit(
    target_root: &Path,
    commit: HistoryCommitId,
) -> Result<(), String> {
    remove_file(&locator_path(target_root, commit))?;
    remove_file(&pending_locator_path(target_root, commit))
}

fn ensure_typed_v3_locator_directories(target_root: &Path) -> Result<(), String> {
    prepare_history_layout(target_root)?;
    let directory = target_root.join("history").join("typed-v3-locators");
    create_private_directory(&directory)?;
    set_private_directory(&directory)?;
    let pending = pending_locator_directory(target_root);
    create_private_directory(&pending)?;
    set_private_directory(&pending)?;
    Ok(())
}

fn stage_typed_v3_locator_bytes(
    target_root: &Path,
    commit: HistoryCommitId,
    bytes: &[u8],
) -> Result<(), String> {
    ensure_typed_v3_locator_directories(target_root)?;
    let pending = pending_locator_path(target_root, commit);
    match fs::symlink_metadata(&pending) {
        Ok(_) => {
            ensure_regular_file(&pending)?;
            if fs::metadata(&pending).map_err(display_io)?.len() != 0 {
                return Err("typed V3 pending locator marker is not empty".to_owned());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            backend_platform::durable::write_private_atomic(&pending, &[]).map_err(display_io)?;
        }
        Err(error) => return Err(display_io(error)),
    }
    let path = locator_path(target_root, commit);
    match read_optional_bounded(&path, MAX_TYPED_V3_LOCATOR_BYTES + 32 + CHECKSUM_BYTES)? {
        Some(existing) if existing == bytes => Ok(()),
        Some(_) => Err("typed V3 history commit locator changed".to_owned()),
        None => backend_platform::durable::write_private_atomic(&path, bytes).map_err(display_io),
    }
}

fn decode_pending_locator_name(path: &Path) -> Result<HistoryCommitId, String> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "typed V3 pending locator name is not UTF-8".to_owned())?;
    let encoded = name
        .strip_suffix(".pending")
        .ok_or_else(|| "typed V3 pending locator has an unexpected file name".to_owned())?;
    if encoded.len() != 64 {
        return Err("typed V3 pending locator name has the wrong length".to_owned());
    }
    let mut identity = [0_u8; 32];
    for (index, pair) in encoded.as_bytes().chunks_exact(2).enumerate() {
        let digit = |byte| match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            _ => None,
        };
        let high = digit(pair[0])
            .ok_or_else(|| "typed V3 pending locator name is not lowercase hex".to_owned())?;
        let low = digit(pair[1])
            .ok_or_else(|| "typed V3 pending locator name is not lowercase hex".to_owned())?;
        identity[index] = (high << 4) | low;
    }
    let commit = HistoryCommitId(identity);
    if encoded != hex(commit.as_bytes()) {
        return Err("typed V3 pending locator name is not canonical".to_owned());
    }
    Ok(commit)
}

fn reconcile_one_pending_locator(
    target_root: &Path,
    target: &SemanticTargetKey,
    pending_path: &Path,
) -> Result<(), String> {
    ensure_regular_file(pending_path)?;
    if fs::metadata(pending_path).map_err(display_io)?.len() != 0 {
        return Err("typed V3 pending locator marker is not empty".to_owned());
    }
    let commit = decode_pending_locator_name(pending_path)?;
    let commit_path = history_commit_path(&target_root.join("history").join("commits"), commit);
    match fs::symlink_metadata(&commit_path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            remove_file(&locator_path(target_root, commit))?;
            remove_file(&history_payload_root_path(target_root, commit))?;
            remove_file(pending_path)
        }
        Err(error) => Err(display_io(error)),
        Ok(_) => {
            ensure_regular_file(&commit_path)?;
            let record = validate_history_commit_node(
                target_root,
                target,
                &target_root.join("history").join("commits"),
                commit,
            )?;
            if record.identity != commit
                || !matches!(record.generation_root, HistoryGenerationRoot::TypedV3(_))
            {
                return Err("typed V3 pending locator names a different commit body".to_owned());
            }
            append_commit_index(target_root, commit)?;
            remove_file(pending_path)
        }
    }
}

pub(super) fn reconcile_pending_typed_v3_locators(
    target_root: &Path,
    target: &SemanticTargetKey,
) -> Result<(usize, bool), String> {
    let pending_root = pending_locator_directory(target_root);
    if !ensure_optional_directory(&pending_root)? {
        return Ok((0, false));
    }
    let mut entries = fs::read_dir(&pending_root).map_err(display_io)?;
    let mut processed = 0_usize;
    loop {
        if processed == MAX_TYPED_V3_PENDING_RECONCILE {
            let more = entries.next().transpose().map_err(display_io)?.is_some();
            return Ok((processed, more));
        }
        let Some(entry) = entries.next() else {
            return Ok((processed, false));
        };
        let entry = entry.map_err(display_io)?;
        reconcile_one_pending_locator(target_root, target, &entry.path())?;
        processed += 1;
    }
}

pub(super) fn validate_typed_v3_locator_binding(
    target_root: &Path,
    record: &HistoryCommitRecord,
) -> Result<(), String> {
    let HistoryGenerationRoot::TypedV3(claim) = record.generation_root else {
        return Ok(());
    };
    let locator = load_typed_v3_history_locator(target_root, record.identity, claim.locator())?;
    let manifest = locator.validate()?;
    if manifest.content_root_claim().as_bytes() != claim.content_root_claim().as_bytes()
        || manifest.generation_root_claim().as_bytes() != claim.generation_root_claim().as_bytes()
    {
        return Err("typed V3 history commit roots differ from its locator manifest".to_owned());
    }
    Ok(())
}

impl LocalSemanticGenerationFiles {
    pub(crate) fn typed_v3_locator_identity(
        &self,
        locator: &TypedV3HistoryLocator,
    ) -> Result<HistoryTypedV3LocatorId, String> {
        locator.identity()
    }

    pub(crate) fn stage_typed_v3_locator(
        &self,
        target: &SemanticTargetKey,
        commit: HistoryCommitId,
        locator: &TypedV3HistoryLocator,
    ) -> Result<HistoryTypedV3LocatorId, String> {
        let identity = locator.identity()?;
        let bytes = locator.encode()?;
        stage_typed_v3_locator_bytes(&self.target_root(target), commit, &bytes)?;
        Ok(identity)
    }

    pub(crate) fn finish_typed_v3_locator_admission(
        &self,
        target: &SemanticTargetKey,
        commit: HistoryCommitId,
    ) -> Result<(), String> {
        remove_file(&pending_locator_path(&self.target_root(target), commit))
    }

    pub(crate) fn reconcile_pending_typed_v3_locators(
        &self,
        target: &SemanticTargetKey,
    ) -> Result<(usize, bool), String> {
        reconcile_pending_typed_v3_locators(&self.target_root(target), target)
    }

    pub(crate) fn reconcile_typed_v3_locator_admission(
        &self,
        target: &SemanticTargetKey,
        commit: HistoryCommitId,
    ) -> Result<(), String> {
        let target_root = self.target_root(target);
        reconcile_one_pending_locator(
            &target_root,
            target,
            &pending_locator_path(&target_root, commit),
        )
    }

    pub(crate) fn typed_v3_locator(
        &self,
        target: &SemanticTargetKey,
        commit: HistoryCommitId,
        identity: HistoryTypedV3LocatorId,
    ) -> Result<TypedV3HistoryLocator, String> {
        load_typed_v3_history_locator(&self.target_root(target), commit, identity)
    }

    pub(crate) fn typed_v3_publication_snapshot(
        &self,
        target: &SemanticTargetKey,
        identity: HistoryCommitId,
    ) -> Result<TypedV3HistoryPublicationSnapshot, String> {
        let commit = self.history_commit(target, identity)?;
        let claim = commit
            .generation_root()
            .typed_v3_claim()
            .ok_or_else(|| "history commit does not name a typed V3 generation".to_owned())?;
        let target_root = self.target_root(target);
        let locator = load_typed_v3_history_locator(&target_root, identity, claim.locator())?;
        let manifest = locator.validate()?;
        if manifest.content_root_claim().as_bytes() != claim.content_root_claim().as_bytes()
            || manifest.generation_root_claim().as_bytes()
                != claim.generation_root_claim().as_bytes()
        {
            return Err("typed V3 commit roots differ from its cold manifest".to_owned());
        }
        let selected = super::super::load_record(&target_root, commit.generation(), target)?;
        if manifest.build() != selected.manifest.build()
            || manifest.input_claim()
                != backend_semantic::ir::SemanticInputClaimV2::from_witness(
                    &selected.manifest.input(),
                )
        {
            return Err(
                "typed V3 manifest differs from its persisted selected-generation build or input"
                    .to_owned(),
            );
        }
        let payload_root = super::codec::read_history_payload_root(&target_root, identity)?
            .ok_or_else(|| "typed V3 history payload root is missing".to_owned())?;
        if payload_root.closure.as_bytes() != claim.closure().as_bytes() {
            return Err("typed V3 history payload root differs from its commit".to_owned());
        }
        Ok(TypedV3HistoryPublicationSnapshot {
            identity,
            claim,
            locator,
            payload_root,
        })
    }

    /// Reopens the immutable generation metadata bound by one history commit.
    /// This snapshot is history evidence only; no local transfer-cache HEAD
    /// participates in selection.
    pub(crate) fn typed_v3_history_generation(
        &self,
        target: &SemanticTargetKey,
        identity: HistoryCommitId,
    ) -> Result<LocalSemanticGeneration, String> {
        let commit = self.history_commit(target, identity)?;
        let target_root = self.target_root(target);
        let record = super::super::load_record(&target_root, commit.generation(), target)?;
        super::catalog::validate_commit_generation(&commit.record, &record)?;
        super::catalog::generation_from_record(record, commit.selected_stamp())
    }

    pub(crate) fn revalidate_typed_v3_publication_snapshot(
        &self,
        target: &SemanticTargetKey,
        expected: &TypedV3HistoryPublicationSnapshot,
    ) -> Result<(), String> {
        let actual = self.typed_v3_publication_snapshot(target, expected.identity)?;
        if actual != *expected {
            return Err(
                "typed V3 publication history metadata changed during cold verification".to_owned(),
            );
        }
        Ok(())
    }

    pub(crate) fn propose_typed_v3_history_commit(
        &self,
        target: &SemanticTargetKey,
        generation: &LocalSemanticGeneration,
        parents: &[HistoryCommitId],
        provenance: [u8; 32],
        content: &backend_semantic::ir::VerifiedTypedPlaneHistoryContentV3,
        locator: &TypedV3HistoryLocator,
        closure: ArtifactClosureClaim,
        locator_id: HistoryTypedV3LocatorId,
    ) -> Result<UnpublishedHistoryProposal, HistoryProposalError> {
        if parents.len() > MAX_HISTORY_PARENTS {
            return Err(HistoryProposalError::Storage(
                "semantic history commit exceeds its parent bound".to_owned(),
            ));
        }
        if parents.len() == 2 {
            return Err(HistoryProposalError::UnsupportedMergePayloadClosure);
        }
        if generation.target() != target {
            return Err(HistoryProposalError::Storage(
                "typed V3 history snapshot belongs to another target".to_owned(),
            ));
        }
        let manifest = locator.validate().map_err(HistoryProposalError::Storage)?;
        let selected_input = backend_semantic::ir::SemanticInputClaimV2::from_witness(
            &generation.manifest().input(),
        );
        if manifest.build() != generation.manifest().build()
            || manifest.input_claim() != selected_input
            || !manifest
                .content_root_claim()
                .matches(content.content_root())
            || !manifest
                .generation_root_claim()
                .matches(content.generation_root())
        {
            return Err(HistoryProposalError::Storage(
                "typed V3 manifest is not bound to the exact selected generation input and build"
                    .to_owned(),
            ));
        }
        if locator.identity().map_err(HistoryProposalError::Storage)? != locator_id {
            return Err(HistoryProposalError::Storage(
                "typed V3 locator identity differs from its canonical bytes".to_owned(),
            ));
        }
        let target_root = self.target_root(target);
        let commits_root =
            prepare_history_layout_typed(&target_root).map_err(|error| match error {
                HistoryMutationError::RetryableAvailability(detail) => {
                    HistoryProposalError::RetryableStorage(detail)
                }
                HistoryMutationError::CompareAndSwapMismatch => HistoryProposalError::Storage(
                    "unexpected ref-tip mismatch while preparing a typed history proposal"
                        .to_owned(),
                ),
                HistoryMutationError::Refused(detail) => HistoryProposalError::Storage(detail),
            })?;
        let mut depth = 0_u32;
        for (index, parent) in parents.iter().enumerate() {
            let record = validate_history_commit_node(&target_root, target, &commits_root, *parent)
                .map_err(HistoryProposalError::Storage)?;
            if index == 0 {
                depth = record.first_parent_depth.checked_add(1).ok_or_else(|| {
                    HistoryProposalError::Storage("semantic history depth overflows".to_owned())
                })?;
            }
        }
        let root_claim = HistoryTypedV3RootClaim::from_verified(content, closure, locator_id);
        let record = HistoryCommitRecord {
            identity: HistoryCommitId([0; 32]),
            target: target.clone(),
            parents: parents.to_vec(),
            generation: generation.identity(),
            generation_root: HistoryGenerationRoot::TypedV3(root_claim),
            manifest_root: generation.manifest().root(),
            stamp: generation.selected_stamp(),
            provenance,
            first_parent_depth: depth,
            checkpoint: parents.is_empty() || depth % HISTORY_CHECKPOINT_INTERVAL == 0,
        };
        let (record, identity) =
            identify_history_record(record).map_err(HistoryProposalError::Storage)?;
        Ok(UnpublishedHistoryProposal { record, identity })
    }

    pub(crate) fn admit_typed_v3_history_proposal<F: crate::SelectedNativeImagePublicationFence>(
        &self,
        proposal: UnpublishedHistoryProposal,
        payload_root: AdmittedHistoryPayloadRoot,
        selection_fence: &F,
    ) -> Result<HistoryAdmissionReceipt, String> {
        let HistoryGenerationRoot::TypedV3(claim) = proposal.record.generation_root else {
            return Err("typed V3 history proposal carries another generation root".to_owned());
        };
        if claim.closure().as_bytes() != payload_root.closure.as_bytes() {
            return Err("typed V3 commit and payload closure roots differ".to_owned());
        }
        if selection_fence.selected_target() != &proposal.record.target
            || selection_fence.selected_stamp() != proposal.record.stamp
        {
            return Err("typed V3 proposal differs from its selected-owner fence".to_owned());
        }
        let target_root = self.target_root(&proposal.record.target);
        let _ = load_typed_v3_history_locator(&target_root, proposal.identity, claim.locator())?;
        let selected = super::super::load_record(
            &target_root,
            proposal.record.generation,
            &proposal.record.target,
        )?;
        super::catalog::validate_commit_generation(&proposal.record, &selected)?;
        if selection_fence.selected_image() != selected.image
            || selection_fence.selected_image_identity() != selected.image_identity
        {
            return Err("typed V3 generation differs from its selected-owner fence".to_owned());
        }
        // Persist the payload root before the commit/index. If interrupted,
        // the durable pending-locator marker removes both unreachable sidecars.
        super::codec::write_history_payload_root(&target_root, proposal.identity, payload_root)?;
        let mut source = FencedSelectedGenerationSource {
            fence: selection_fence,
        };
        self.admit_history_proposal(proposal, &mut source)
    }
}

pub(super) fn load_typed_v3_history_locator(
    target_root: &Path,
    commit: HistoryCommitId,
    identity: HistoryTypedV3LocatorId,
) -> Result<TypedV3HistoryLocator, String> {
    let path = locator_path(target_root, commit);
    let Some(bytes) =
        read_optional_bounded(&path, MAX_TYPED_V3_LOCATOR_BYTES + 32 + CHECKSUM_BYTES)?
    else {
        return Err("typed V3 history locator is missing".to_owned());
    };
    TypedV3HistoryLocator::decode(&bytes, identity)
}

fn typed_v3_locator_identity(body: &[u8]) -> HistoryTypedV3LocatorId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(TYPED_V3_LOCATOR_DOMAIN);
    hasher.update(&u64::try_from(body.len()).unwrap_or(u64::MAX).to_be_bytes());
    hasher.update(body);
    HistoryTypedV3LocatorId(*hasher.finalize().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v3_locator_has_a_distinct_wire_tag_and_cold_round_trips_c007_bridge() {
        let fixture = crate::ir_hydration_store::positive_v2_history_fixture_for_test();
        let locator = TypedV3HistoryLocator::from_v2_bridge(fixture.locator)
            .expect("canonical c007 bridge is portable under the V3 locator tag");
        let identity = locator.identity().expect("hash V3 locator body");
        let bytes = locator.encode().expect("encode V3 locator envelope");
        assert_eq!(bytes.get(32 + 5), Some(&TYPED_V3_LOCATOR_TAG));
        let decoded = TypedV3HistoryLocator::decode(&bytes, identity)
            .expect("cold decode V3 locator envelope");
        assert_eq!(decoded, locator);
        assert_eq!(
            decoded.validate().expect("validate V3 bridge"),
            fixture.manifest
        );

        let mut swapped_identity = *identity.as_bytes();
        swapped_identity[0] ^= 0x80;
        assert!(
            TypedV3HistoryLocator::decode(
                &bytes,
                HistoryTypedV3LocatorId::from_bytes(swapped_identity),
            )
            .is_err()
        );
    }

    #[test]
    fn standalone_v3_root_claim_round_trips_and_fails_closed_on_mutation() {
        let claim = HistoryTypedV3RootClaim {
            content_root: backend_semantic::ir::UntrustedSemanticContentRootV2::from_wire_claim(
                [0x31; 32],
            ),
            generation_root:
                backend_semantic::ir::UntrustedSemanticGenerationRootV2::from_wire_claim([0x42; 32]),
            closure: ArtifactClosureClaim::from_bytes([0x53; 32]),
            locator: HistoryTypedV3LocatorId::from_bytes([0x64; 32]),
        };
        let bytes = claim.encode_portable().expect("encode portable root claim");
        assert_eq!(bytes[0], TYPED_V3_ROOT_DISCRIMINATOR);
        assert_eq!(HistoryTypedV3RootClaim::decode_portable(&bytes), Ok(claim));

        let mut mutated = bytes;
        mutated[1] ^= 1;
        assert!(HistoryTypedV3RootClaim::decode_portable(&mutated).is_err());
        mutated[0] = 2;
        assert!(HistoryTypedV3RootClaim::decode_portable(&mutated).is_err());
    }
}
