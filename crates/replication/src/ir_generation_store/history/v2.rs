//! Durable locators for typed V2 history commits.
//!
//! V1 history continues to name its local NXFI generation records. Typed V2
//! commits use the same commit DAG, ref catalog, and payload-root GC path, but
//! bind claim-only typed roots, an exact FileStore closure, and an immutable
//! locator containing the canonical V2 manifest plus semantic-to-physical
//! object bridges. A cold reader must verify that entire binding before it
//! receives a proof-bearing replay token.

use super::catalog::validate_history_commit_node;
use super::codec::{
    append_commit_index, identify_history_record, prepare_history_layout,
    read_history_payload_root, write_history_payload_root,
};
use super::*;

const TYPED_V2_LOCATOR_DOMAIN: &[u8] = b"backend.semantic.history-typed-v2-locator.v1\0";
const TYPED_V2_COMMIT_DOMAIN: &[u8] = b"backend.semantic.history-commit.typed-v2.v1\0";
const MAX_TYPED_V2_MANIFEST_BYTES: usize = backend_semantic::ir::MAX_TYPED_PLANE_MANIFEST_V2_BYTES;
const MAX_TYPED_V2_PENDING_RECONCILE: usize = super::MAX_HISTORY_GC_BATCH_RECORDS / 2;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TypedV2HistoryLocator {
    pub(crate) manifest: Vec<u8>,
    pub(crate) segments: Vec<HistoryTypedV2SegmentObject>,
    pub(crate) jumbo: Vec<HistoryTypedV2JumboObject>,
    /// Exact canonical candidate bytes, carried in the history metadata
    /// object so the existing commit-rooted locator ID binds them without a
    /// child-commit hash cycle or a change to the FileStore payload closure.
    pub(crate) lineage_edge_set: Option<Vec<u8>>,
    pub(crate) wire_revision: u8,
}

/// Immutable history metadata captured before cold V2 closure verification.
/// A caller can release the history state lock while scanning payload bytes,
/// then compare this complete snapshot again before publishing a ref.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TypedV2HistoryPublicationSnapshot {
    identity: HistoryCommitId,
    claim: HistoryTypedV2RootClaim,
    locator: TypedV2HistoryLocator,
    payload_root: HistoryPayloadRoot,
}

impl TypedV2HistoryPublicationSnapshot {
    pub(crate) const fn identity(&self) -> HistoryCommitId {
        self.identity
    }

    pub(crate) const fn claim(&self) -> HistoryTypedV2RootClaim {
        self.claim
    }

    pub(crate) fn locator(&self) -> &TypedV2HistoryLocator {
        &self.locator
    }
}

impl TypedV2HistoryLocator {
    pub(crate) fn preflight_admission_counts(
        expected_segments: usize,
        segment_count: usize,
        jumbo_count: usize,
    ) -> Result<(), String> {
        let object_count = segment_count
            .checked_add(jumbo_count)
            .ok_or_else(|| "typed V2 history locator object count overflows".to_owned())?;
        if expected_segments != segment_count
            || segment_count > backend_semantic::ir::MAX_TYPED_PLANE_SEGMENTS_V2
            || object_count > super::MAX_HISTORY_TYPED_V2_LOCATOR_OBJECTS
        {
            return Err("typed V2 history locator object count exceeds its bounds".to_owned());
        }
        Ok(())
    }

    pub(crate) fn from_admission_parts(
        manifest: Vec<u8>,
        expected_segments: usize,
        segments: &[HistoryTypedV2SegmentObject],
        jumbo: &[HistoryTypedV2JumboObject],
        lineage_edge_set: Option<&OwnedTypedLineageEdgeSetV1>,
    ) -> Result<Self, String> {
        Self::preflight_admission_counts(expected_segments, segments.len(), jumbo.len())?;
        if manifest.len() > MAX_TYPED_V2_MANIFEST_BYTES {
            return Err("typed V2 history manifest exceeds its byte bound".to_owned());
        }
        let mut segment_map = Vec::new();
        segment_map
            .try_reserve_exact(segments.len())
            .map_err(|_| "typed V2 history segment map allocation failed".to_owned())?;
        segment_map.extend_from_slice(segments);
        segment_map.sort_unstable_by(|left, right| {
            left.segment().as_bytes().cmp(right.segment().as_bytes())
        });

        let mut jumbo_map = Vec::new();
        jumbo_map
            .try_reserve_exact(jumbo.len())
            .map_err(|_| "typed V2 history rope map allocation failed".to_owned())?;
        jumbo_map.extend_from_slice(jumbo);
        jumbo_map.sort_unstable_by(|left, right| jumbo_order(*left).cmp(&jumbo_order(*right)));

        let lineage_edge_set = lineage_edge_set.map(|lineage| lineage.as_bytes().to_vec());
        let wire_revision = if lineage_edge_set.is_some() {
            HISTORY_TYPED_V2_LOCATOR_LINEAGE_TAG
        } else {
            HISTORY_TYPED_V2_LOCATOR_TAG
        };
        if let Some(bytes) = &lineage_edge_set {
            if bytes.len() > MAX_TYPED_LINEAGE_EDGE_SET_V1_BYTES {
                return Err("typed lineage edge set exceeds its byte bound".to_owned());
            }
            BorrowedTypedLineageEdgeSetV1::parse(bytes)
                .map_err(|error| format!("parse typed lineage edge set: {error}"))?;
        }
        let locator = Self {
            manifest,
            segments: segment_map,
            jumbo: jumbo_map,
            lineage_edge_set,
            wire_revision,
        };
        let _ = locator.validate()?;
        Ok(locator)
    }

    pub(crate) fn validate(
        &self,
    ) -> Result<backend_semantic::ir::SemanticTypedPlaneManifestV2, String> {
        if self.manifest.len() > MAX_TYPED_V2_MANIFEST_BYTES {
            return Err("typed V2 history manifest exceeds its byte bound".to_owned());
        }
        let manifest = backend_semantic::ir::SemanticTypedPlaneManifestV2::decode(&self.manifest)
            .map_err(|error| format!("decode typed V2 history manifest: {error}"))?;
        if manifest
            .canonical_bytes()
            .map_err(|error| format!("encode typed V2 history manifest: {error}"))?
            != self.manifest
        {
            return Err("typed V2 history manifest is not canonical".to_owned());
        }
        let usage = manifest
            .resource_usage()
            .map_err(|error| format!("measure typed V2 history manifest: {error}"))?;
        let expected_segments = usage.segment_descriptors();
        if self.segments.len() != expected_segments
            || self.segments.len() > backend_semantic::ir::MAX_TYPED_PLANE_SEGMENTS_V2
            || self.segments.len().saturating_add(self.jumbo.len())
                > MAX_HISTORY_TYPED_V2_LOCATOR_OBJECTS
        {
            return Err("typed V2 history locator object count differs from its bounds".to_owned());
        }
        if self.segments.iter().any(|segment| segment.byte_length == 0)
            || self
                .segments
                .windows(2)
                .any(|pair| pair[0].segment.as_bytes() >= pair[1].segment.as_bytes())
        {
            return Err("typed V2 history segment map is not strictly canonical".to_owned());
        }
        if self.jumbo.iter().any(|object| object.byte_length == 0)
            || self
                .jumbo
                .windows(2)
                .any(|pair| jumbo_order(pair[0]) >= jumbo_order(pair[1]))
        {
            return Err("typed V2 history rope map is not strictly canonical".to_owned());
        }
        let mut manifest_segments = Vec::new();
        manifest_segments
            .try_reserve_exact(expected_segments)
            .map_err(|_| "typed V2 history manifest allocation failed".to_owned())?;
        for family in manifest.families() {
            for segment in family.segments() {
                manifest_segments.push((*segment.id_claim().as_bytes(), segment.byte_length()));
            }
        }
        manifest_segments.sort_unstable_by_key(|(id, _)| *id);
        for (mapped, (expected_id, expected_length)) in self.segments.iter().zip(&manifest_segments)
        {
            if mapped.segment.as_bytes() != expected_id || mapped.byte_length != *expected_length {
                return Err("typed V2 history segment map differs from its manifest".to_owned());
            }
        }
        if self.jumbo.iter().any(|object| match object.kind {
            backend_semantic::ir::JumboRopeObjectKind::Leaf => {
                object.byte_length > backend_semantic::ir::JUMBO_ROPE_MAX_LEAF_BYTES as u64
            }
            backend_semantic::ir::JumboRopeObjectKind::Interior => {
                object.byte_length != backend_semantic::ir::ROPE_NODE_WIRE_BYTES as u64
            }
        }) {
            return Err(
                "typed V2 history rope object exceeds its kind-specific length bound".to_owned(),
            );
        }
        if let Some(bytes) = &self.lineage_edge_set {
            let lineage = BorrowedTypedLineageEdgeSetV1::parse(bytes)
                .map_err(|error| format!("parse typed lineage edge set: {error}"))?;
            lineage
                .validate_structure()
                .map_err(|error| format!("validate typed lineage structure: {error}"))?;
            if lineage.child_generation_claim() != manifest.generation_root_claim().as_bytes() {
                return Err("typed lineage edge set names a different child generation".to_owned());
            }
        }
        Ok(manifest)
    }

    fn validate_lineage_parent_binding(
        &self,
        parent: HistoryCommitId,
        parent_generation: &[u8; 32],
    ) -> Result<(), String> {
        let Some(bytes) = &self.lineage_edge_set else {
            return Ok(());
        };
        let lineage = BorrowedTypedLineageEdgeSetV1::parse(bytes)
            .map_err(|error| format!("parse typed lineage edge set: {error}"))?;
        if lineage.parent_commit() != parent {
            return Err("typed lineage edge set names a different parent commit".to_owned());
        }
        if lineage.parent_generation_claim() != parent_generation {
            return Err("typed lineage edge set names a different parent generation".to_owned());
        }
        Ok(())
    }

    fn encode_body(&self) -> Result<Vec<u8>, String> {
        let mut writer = Writer::new(MAX_HISTORY_TYPED_V2_LOCATOR_BYTES);
        let tag = match self.wire_revision {
            HISTORY_TYPED_V2_LOCATOR_TAG => HISTORY_TYPED_V2_LOCATOR_TAG,
            HISTORY_TYPED_V2_LOCATOR_LINEAGE_TAG => HISTORY_TYPED_V2_LOCATOR_LINEAGE_TAG,
            _ => return Err("typed V2 history locator wire revision is invalid".to_owned()),
        };
        if tag == HISTORY_TYPED_V2_LOCATOR_TAG && self.lineage_edge_set.is_some() {
            return Err("legacy typed V2 locator cannot carry lineage bytes".to_owned());
        }
        writer.header(tag)?;
        writer.sized_bytes(&self.manifest, MAX_TYPED_V2_MANIFEST_BYTES)?;
        writer.u32(u32::try_from(self.segments.len()).map_err(|error| error.to_string())?)?;
        for segment in &self.segments {
            writer.fixed(segment.segment.as_bytes())?;
            writer.fixed(segment.object.as_bytes())?;
            writer.u64(segment.byte_length)?;
        }
        writer.u32(u32::try_from(self.jumbo.len()).map_err(|error| error.to_string())?)?;
        for object in &self.jumbo {
            writer.u8(match object.kind {
                backend_semantic::ir::JumboRopeObjectKind::Leaf => 0,
                backend_semantic::ir::JumboRopeObjectKind::Interior => 1,
            })?;
            writer.fixed(&object.id)?;
            writer.fixed(object.object.as_bytes())?;
            writer.u64(object.byte_length)?;
        }
        if tag == HISTORY_TYPED_V2_LOCATOR_LINEAGE_TAG {
            match &self.lineage_edge_set {
                None => writer.u8(0)?,
                Some(bytes) => {
                    writer.u8(1)?;
                    writer.sized_bytes(bytes, MAX_TYPED_LINEAGE_EDGE_SET_V1_BYTES)?;
                }
            }
        }
        let bytes = writer.finish();
        if bytes.len() > MAX_HISTORY_TYPED_V2_LOCATOR_BYTES {
            return Err("typed V2 history locator exceeds its byte bound".to_owned());
        }
        Ok(bytes)
    }
}

fn jumbo_order(object: HistoryTypedV2JumboObject) -> (u8, [u8; 32]) {
    (
        match object.kind {
            backend_semantic::ir::JumboRopeObjectKind::Leaf => 0,
            backend_semantic::ir::JumboRopeObjectKind::Interior => 1,
        },
        object.id,
    )
}

pub(super) fn typed_v2_locator_identity(body: &[u8]) -> HistoryTypedV2LocatorId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(TYPED_V2_LOCATOR_DOMAIN);
    hasher.update(&u64::try_from(body.len()).unwrap_or(u64::MAX).to_be_bytes());
    hasher.update(body);
    HistoryTypedV2LocatorId(*hasher.finalize().as_bytes())
}

fn locator_path(target_root: &Path, commit: HistoryCommitId) -> PathBuf {
    target_root
        .join("history")
        .join("typed-v2-locators")
        .join(format!("{}.locator", hex(commit.as_bytes())))
}

fn pending_locator_directory(target_root: &Path) -> PathBuf {
    target_root
        .join("history")
        .join("typed-v2-locators")
        .join("pending")
}

fn pending_locator_path(target_root: &Path, commit: HistoryCommitId) -> PathBuf {
    pending_locator_directory(target_root).join(format!("{}.pending", hex(commit.as_bytes())))
}

pub(super) fn remove_typed_v2_locator_for_commit(
    target_root: &Path,
    commit: HistoryCommitId,
) -> Result<(), String> {
    remove_file(&locator_path(target_root, commit))?;
    remove_file(&pending_locator_path(target_root, commit))
}

fn ensure_typed_v2_locator_directories(target_root: &Path) -> Result<(), String> {
    prepare_history_layout(target_root)?;
    let directory = target_root.join("history").join("typed-v2-locators");
    create_private_directory(&directory)?;
    set_private_directory(&directory)?;
    let pending = pending_locator_directory(target_root);
    create_private_directory(&pending)?;
    set_private_directory(&pending)?;
    Ok(())
}

fn stage_typed_v2_locator_bytes(
    target_root: &Path,
    commit: HistoryCommitId,
    bytes: &[u8],
) -> Result<(), String> {
    ensure_typed_v2_locator_directories(target_root)?;
    let pending = pending_locator_path(target_root, commit);
    match fs::symlink_metadata(&pending) {
        Ok(_) => {
            ensure_regular_file(&pending)?;
            if fs::metadata(&pending).map_err(display_io)?.len() != 0 {
                return Err("typed V2 pending locator marker is not empty".to_owned());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            backend_platform::durable::write_private_atomic(&pending, &[]).map_err(display_io)?;
        }
        Err(error) => return Err(display_io(error)),
    }
    let path = locator_path(target_root, commit);
    match read_optional_bounded(&path, MAX_HISTORY_TYPED_V2_LOCATOR_BYTES + 64)? {
        Some(existing) if existing == bytes => Ok(()),
        Some(_) => Err("typed V2 history commit locator changed".to_owned()),
        None => backend_platform::durable::write_private_atomic(&path, bytes).map_err(display_io),
    }
}

fn decode_pending_typed_v2_locator_name(path: &Path) -> Result<HistoryCommitId, String> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "typed V2 pending locator name is not UTF-8".to_owned())?;
    let encoded = name
        .strip_suffix(".pending")
        .ok_or_else(|| "typed V2 pending locator has an unexpected file name".to_owned())?;
    if encoded.len() != 64 {
        return Err("typed V2 pending locator name has the wrong length".to_owned());
    }
    let mut identity = [0_u8; 32];
    for (index, pair) in encoded.as_bytes().chunks_exact(2).enumerate() {
        let digit = |byte| match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            _ => None,
        };
        let high = digit(pair[0])
            .ok_or_else(|| "typed V2 pending locator name is not lowercase hex".to_owned())?;
        let low = digit(pair[1])
            .ok_or_else(|| "typed V2 pending locator name is not lowercase hex".to_owned())?;
        identity[index] = (high << 4) | low;
    }
    let commit = HistoryCommitId(identity);
    if encoded != hex(commit.as_bytes()) {
        return Err("typed V2 pending locator name is not canonical".to_owned());
    }
    Ok(commit)
}

fn reconcile_one_pending_typed_v2_locator(
    target_root: &Path,
    target: &SemanticTargetKey,
    pending_path: &Path,
) -> Result<(), String> {
    ensure_regular_file(pending_path)?;
    if fs::metadata(pending_path).map_err(display_io)?.len() != 0 {
        return Err("typed V2 pending locator marker is not empty".to_owned());
    }
    let commit = decode_pending_typed_v2_locator_name(pending_path)?;
    let commit_path = history_commit_path(&target_root.join("history").join("commits"), commit);
    match fs::symlink_metadata(&commit_path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            remove_file(&locator_path(target_root, commit))?;
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
                || !matches!(record.generation_root, HistoryGenerationRoot::TypedV2(_))
            {
                return Err("typed V2 pending locator names a different commit body".to_owned());
            }
            append_commit_index(target_root, commit)?;
            remove_file(pending_path)
        }
    }
}

pub(super) fn reconcile_pending_typed_v2_locators(
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
        if processed == MAX_TYPED_V2_PENDING_RECONCILE {
            let more = entries.next().transpose().map_err(display_io)?.is_some();
            return Ok((processed, more));
        }
        let Some(entry) = entries.next() else {
            return Ok((processed, false));
        };
        let entry = entry.map_err(display_io)?;
        let path = entry.path();
        reconcile_one_pending_typed_v2_locator(target_root, target, &path)?;
        processed += 1;
    }
}

pub(super) fn validate_typed_v2_locator_binding(
    target_root: &Path,
    record: &HistoryCommitRecord,
) -> Result<(), String> {
    let HistoryGenerationRoot::TypedV2(claim) = record.generation_root else {
        return Ok(());
    };
    let locator = load_typed_v2_history_locator(target_root, record.identity, claim.locator)?;
    let manifest = locator.validate()?;
    if manifest.content_root_claim().as_bytes() != claim.content_root.as_bytes()
        || manifest.generation_root_claim().as_bytes() != claim.generation_root.as_bytes()
    {
        return Err("typed V2 history commit roots differ from its locator manifest".to_owned());
    }
    if locator.lineage_edge_set.is_some() {
        let parent = record
            .parents
            .first()
            .copied()
            .ok_or_else(|| "root typed V2 commit cannot carry parent lineage".to_owned())?;
        let parent_record =
            load_history_commit(&target_root.join("history").join("commits"), parent)?;
        let HistoryGenerationRoot::TypedV2(parent_claim) = parent_record.generation_root else {
            return Err("typed lineage edge set parent is not a V2 commit".to_owned());
        };
        locator.validate_lineage_parent_binding(parent, parent_claim.generation_root.as_bytes())?;
    }
    Ok(())
}

pub(super) fn history_commit_identity(root: HistoryGenerationRoot, body: &[u8]) -> HistoryCommitId {
    let domain = match root {
        HistoryGenerationRoot::NxfiV1(_) => HISTORY_COMMIT_DOMAIN,
        HistoryGenerationRoot::TypedV2(_) => TYPED_V2_COMMIT_DOMAIN,
    };
    let mut hasher = blake3::Hasher::new();
    hasher.update(domain);
    hasher.update(&u64::try_from(body.len()).unwrap_or(u64::MAX).to_be_bytes());
    hasher.update(body);
    HistoryCommitId(*hasher.finalize().as_bytes())
}

pub(super) fn create_typed_v2_locator(
    locator: TypedV2HistoryLocator,
) -> Result<(HistoryTypedV2LocatorId, Vec<u8>), String> {
    let _ = locator.validate()?;
    let body = locator.encode_body()?;
    let identity = typed_v2_locator_identity(&body);
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(body.len().saturating_add(64))
        .map_err(|_| "typed V2 history locator allocation failed".to_owned())?;
    bytes.extend_from_slice(identity.as_bytes());
    bytes.extend_from_slice(&body);
    bytes.extend_from_slice(blake3::hash(&bytes).as_bytes());
    Ok((identity, bytes))
}

pub(super) fn decode_typed_v2_locator(
    bytes: &[u8],
    expected: HistoryTypedV2LocatorId,
) -> Result<TypedV2HistoryLocator, String> {
    #[cfg(test)]
    super::super::count_history_locator_decode();
    let body = checked_body(bytes, MAX_HISTORY_TYPED_V2_LOCATOR_BYTES + 64)?;
    if body.len() < 32 || body[..32] != *expected.as_bytes() {
        return Err("typed V2 history locator identity differs from its filename".to_owned());
    }
    let content = &body[32..];
    if typed_v2_locator_identity(content) != expected {
        return Err("typed V2 history locator identity does not match its bytes".to_owned());
    }
    let mut reader = Reader::new(content);
    let wire_revision = match content.get(5).copied() {
        Some(HISTORY_TYPED_V2_LOCATOR_TAG) => HISTORY_TYPED_V2_LOCATOR_TAG,
        Some(HISTORY_TYPED_V2_LOCATOR_LINEAGE_TAG) => HISTORY_TYPED_V2_LOCATOR_LINEAGE_TAG,
        _ => return Err("typed V2 history locator header is invalid".to_owned()),
    };
    reader.header(wire_revision)?;
    let manifest = reader.sized_bytes(MAX_TYPED_V2_MANIFEST_BYTES)?.to_vec();
    let segment_count = usize::try_from(reader.u32()?).map_err(|error| error.to_string())?;
    if segment_count > backend_semantic::ir::MAX_TYPED_PLANE_SEGMENTS_V2
        || segment_count > MAX_HISTORY_TYPED_V2_LOCATOR_OBJECTS
    {
        return Err("typed V2 history segment map exceeds its count bound".to_owned());
    }
    let mut segments = Vec::new();
    segments
        .try_reserve_exact(segment_count)
        .map_err(|_| "typed V2 history segment map allocation failed".to_owned())?;
    for _ in 0..segment_count {
        segments.push(HistoryTypedV2SegmentObject {
            segment: backend_semantic::ir::UntrustedSemanticSegmentId::from_raw(reader.fixed()?),
            object: UntrustedObjectId::from_bytes(reader.fixed()?),
            byte_length: reader.u64()?,
        });
    }
    let jumbo_count = usize::try_from(reader.u32()?).map_err(|error| error.to_string())?;
    if segment_count.saturating_add(jumbo_count) > MAX_HISTORY_TYPED_V2_LOCATOR_OBJECTS {
        return Err("typed V2 history rope map exceeds its count bound".to_owned());
    }
    let mut jumbo = Vec::new();
    jumbo
        .try_reserve_exact(jumbo_count)
        .map_err(|_| "typed V2 history rope map allocation failed".to_owned())?;
    for _ in 0..jumbo_count {
        let kind = match reader.u8()? {
            0 => backend_semantic::ir::JumboRopeObjectKind::Leaf,
            1 => backend_semantic::ir::JumboRopeObjectKind::Interior,
            _ => return Err("typed V2 history rope kind is invalid".to_owned()),
        };
        let id = reader.fixed()?;
        let object = UntrustedObjectId::from_bytes(reader.fixed()?);
        let byte_length = reader.u64()?;
        jumbo.push(HistoryTypedV2JumboObject {
            id,
            kind,
            object,
            byte_length,
        });
    }
    let lineage_edge_set = if wire_revision == HISTORY_TYPED_V2_LOCATOR_LINEAGE_TAG {
        match reader.u8()? {
            0 => None,
            1 => Some(
                reader
                    .sized_bytes(MAX_TYPED_LINEAGE_EDGE_SET_V1_BYTES)?
                    .to_vec(),
            ),
            _ => return Err("typed V2 locator lineage marker is invalid".to_owned()),
        }
    } else {
        None
    };
    reader.finish()?;
    let locator = TypedV2HistoryLocator {
        manifest,
        segments,
        jumbo,
        lineage_edge_set,
        wire_revision,
    };
    let _ = locator.validate()?;
    if locator.encode_body()?.as_slice() != content {
        return Err("typed V2 history locator is not canonically encoded".to_owned());
    }
    Ok(locator)
}

impl LocalSemanticGenerationFiles {
    pub(crate) fn typed_v2_publication_snapshot(
        &self,
        target: &SemanticTargetKey,
        identity: HistoryCommitId,
    ) -> Result<TypedV2HistoryPublicationSnapshot, String> {
        let commit = self.history_commit(target, identity)?;
        let claim = commit
            .generation_root()
            .typed_v2_claim()
            .ok_or_else(|| "history commit does not name a typed V2 generation".to_owned())?;
        let locator =
            load_typed_v2_history_locator(&self.target_root(target), identity, claim.locator())?;
        let manifest = locator.validate()?;
        if manifest.content_root_claim().as_bytes() != claim.content_root_claim().as_bytes()
            || manifest.generation_root_claim().as_bytes()
                != claim.generation_root_claim().as_bytes()
        {
            return Err("typed V2 commit roots differ from its cold manifest".to_owned());
        }
        let payload_root = read_history_payload_root(&self.target_root(target), identity)?
            .ok_or_else(|| "typed V2 history payload root is missing".to_owned())?;
        if payload_root.closure.as_bytes() != claim.closure().as_bytes() {
            return Err("typed V2 history payload root differs from its commit".to_owned());
        }
        Ok(TypedV2HistoryPublicationSnapshot {
            identity,
            claim,
            locator,
            payload_root,
        })
    }

    pub(crate) fn revalidate_typed_v2_publication_snapshot(
        &self,
        target: &SemanticTargetKey,
        expected: &TypedV2HistoryPublicationSnapshot,
    ) -> Result<(), String> {
        let actual = self.typed_v2_publication_snapshot(target, expected.identity)?;
        if actual != *expected {
            return Err(
                "typed V2 publication history metadata changed during cold verification".to_owned(),
            );
        }
        Ok(())
    }

    pub(crate) fn typed_v2_locator_identity(
        &self,
        locator: &TypedV2HistoryLocator,
    ) -> Result<HistoryTypedV2LocatorId, String> {
        let (identity, _) = create_typed_v2_locator(locator.clone())?;
        Ok(identity)
    }

    pub(crate) fn stage_typed_v2_locator(
        &self,
        target: &SemanticTargetKey,
        commit: HistoryCommitId,
        locator: TypedV2HistoryLocator,
    ) -> Result<HistoryTypedV2LocatorId, String> {
        let (identity, bytes) = create_typed_v2_locator(locator)?;
        stage_typed_v2_locator_bytes(&self.target_root(target), commit, &bytes)?;
        Ok(identity)
    }

    pub(crate) fn finish_typed_v2_locator_admission(
        &self,
        target: &SemanticTargetKey,
        commit: HistoryCommitId,
    ) -> Result<(), String> {
        remove_file(&pending_locator_path(&self.target_root(target), commit))
    }

    pub(crate) fn reconcile_pending_typed_v2_locators(
        &self,
        target: &SemanticTargetKey,
    ) -> Result<(usize, bool), String> {
        reconcile_pending_typed_v2_locators(&self.target_root(target), target)
    }

    pub(crate) fn reconcile_typed_v2_locator_admission(
        &self,
        target: &SemanticTargetKey,
        commit: HistoryCommitId,
    ) -> Result<(), String> {
        let target_root = self.target_root(target);
        reconcile_one_pending_typed_v2_locator(
            &target_root,
            target,
            &pending_locator_path(&target_root, commit),
        )
    }

    pub(crate) fn typed_v2_locator(
        &self,
        target: &SemanticTargetKey,
        commit: HistoryCommitId,
        identity: HistoryTypedV2LocatorId,
    ) -> Result<TypedV2HistoryLocator, String> {
        load_typed_v2_history_locator(&self.target_root(target), commit, identity)
    }

    pub(crate) fn propose_typed_v2_history_commit(
        &self,
        target: &SemanticTargetKey,
        parents: &[HistoryCommitId],
        provenance: [u8; 32],
        content: &backend_semantic::ir::VerifiedTypedPlaneContentV2,
        closure: ArtifactClosureClaim,
        locator: HistoryTypedV2LocatorId,
    ) -> Result<UnpublishedHistoryProposal, HistoryProposalError> {
        if parents.len() > MAX_HISTORY_PARENTS {
            return Err(HistoryProposalError::Storage(
                "semantic history commit exceeds its parent bound".to_owned(),
            ));
        }
        if parents.len() == 2 {
            return Err(HistoryProposalError::UnsupportedMergePayloadClosure);
        }
        let generation = self
            .current(target)
            .map_err(HistoryProposalError::Storage)?
            .ok_or_else(|| {
                HistoryProposalError::Storage(
                    "no admitted current generation is available for typed V2 history".to_owned(),
                )
            })?;
        let target_root = self.target_root(target);
        let commits_root =
            prepare_history_layout(&target_root).map_err(HistoryProposalError::Storage)?;
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
        let record = HistoryCommitRecord {
            identity: HistoryCommitId([0; 32]),
            target: target.clone(),
            parents: parents.to_vec(),
            generation: generation.identity,
            generation_root: HistoryGenerationRoot::typed_v2(content, closure, locator),
            manifest_root: generation.manifest.root(),
            stamp: generation.selected_stamp,
            provenance,
            first_parent_depth: depth,
            checkpoint: parents.is_empty() || depth % HISTORY_CHECKPOINT_INTERVAL == 0,
        };
        let (record, identity) =
            identify_history_record(record).map_err(HistoryProposalError::Storage)?;
        Ok(UnpublishedHistoryProposal { record, identity })
    }

    pub(crate) fn admit_typed_v2_history_proposal<S: SelectedGenerationSource>(
        &self,
        proposal: UnpublishedHistoryProposal,
        payload_root: AdmittedHistoryPayloadRoot,
        source: &mut S,
    ) -> Result<HistoryAdmissionReceipt, String> {
        let HistoryGenerationRoot::TypedV2(claim) = proposal.record.generation_root else {
            return Err("typed V2 history proposal carries a V1 generation root".to_owned());
        };
        if claim.closure.as_bytes() != payload_root.closure.as_bytes() {
            return Err("typed V2 commit and payload closure roots differ".to_owned());
        }
        let target_root = self.target_root(&proposal.record.target);
        let _ = load_typed_v2_history_locator(&target_root, proposal.identity, claim.locator)?;
        let admission = self.admit_history_proposal(proposal, source)?;
        write_history_payload_root(&target_root, admission.commit.identity(), payload_root)?;
        Ok(admission)
    }
}

fn load_typed_v2_history_locator(
    target_root: &Path,
    commit: HistoryCommitId,
    identity: HistoryTypedV2LocatorId,
) -> Result<TypedV2HistoryLocator, String> {
    let path = locator_path(target_root, commit);
    let Some(bytes) = read_optional_bounded(&path, MAX_HISTORY_TYPED_V2_LOCATOR_BYTES + 64)? else {
        return Err("typed V2 history locator is missing".to_owned());
    };
    decode_typed_v2_locator(&bytes, identity)
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_semantic::ir::{
        ImageProvenance, LanguageProfile, RustEdition, SemanticBuildIdentity,
        SemanticImageAuthority, SemanticImageFacts, SemanticInputClaimV2, SemanticIrPlane,
        SemanticPlaneSegmentBoundaryPolicy, SemanticTypedPlaneFamilyDescriptorV2,
        SemanticTypedPlaneManifestV2, UntrustedSemanticContentRootV2,
        UntrustedSemanticGenerationRootV2,
    };
    use backend_semantic::vocabulary::Stage;
    use backend_version::{Coverage, ScopeRoot};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn create() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "backend-typed-v2-history-locator-{}-{nonce}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create locator fixture directory");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn target() -> SemanticTargetKey {
        SemanticTargetKey::new(
            "pkg:history/typed-v2@1.0.0",
            "pkg:history/typed-v2@1.0.0",
            LanguageProfile::Rust(RustEdition::Rust2024),
        )
        .expect("fixture target")
    }

    fn empty_manifest() -> SemanticTypedPlaneManifestV2 {
        let profile = LanguageProfile::Rust(RustEdition::Rust2024);
        let build = SemanticBuildIdentity::new(
            [1; 32],
            [2; 32],
            profile,
            Stage::LowerIr,
            [3; 32],
            [4; 32],
            [5; 32],
            [6; 32],
        );
        let families = [
            SemanticIrPlane::Core,
            SemanticIrPlane::Types,
            SemanticIrPlane::Relations,
            SemanticIrPlane::Occurrences,
            SemanticIrPlane::Documentation,
            SemanticIrPlane::SourceProvenance,
            SemanticIrPlane::LanguageExtensions(profile),
        ]
        .map(|family| {
            SemanticTypedPlaneFamilyDescriptorV2::from_untrusted_claims(
                family,
                0,
                SemanticPlaneSegmentBoundaryPolicy::stable_key_hash_ramp(20, 4096, 1_048_576)
                    .expect("fixture boundary policy"),
                Vec::new(),
            )
            .expect("empty family is canonical")
        });
        SemanticTypedPlaneManifestV2::from_untrusted_claims(
            build,
            SemanticImageFacts {
                authority: SemanticImageAuthority::Shared,
                provenance: ImageProvenance::Unavailable,
            },
            SemanticInputClaimV2::from_untrusted_claims(
                [7; 32],
                ScopeRoot::from_bytes([8; 32]),
                Coverage::Complete,
            ),
            UntrustedSemanticContentRootV2::from_wire_claim([9; 32]),
            UntrustedSemanticGenerationRootV2::from_wire_claim([10; 32]),
            families,
        )
        .expect("claim-only V2 manifest is structurally canonical")
    }

    #[test]
    fn typed_locator_survives_cold_reopen_and_gc_cleanup_is_idempotent() {
        let directory = TestDirectory::create();
        let target = target();
        let commit = HistoryCommitId::from_bytes([0x44; 32]);
        let files =
            LocalSemanticGenerationFiles::open(&directory.0).expect("open generation files");
        let manifest = empty_manifest();
        let empty_lineage = |parent: u8| {
            let mut bytes = Vec::with_capacity(108);
            bytes.extend_from_slice(b"IRLEDGE1");
            bytes.extend_from_slice(&[parent; 32]);
            bytes.extend_from_slice(&[0x66; 32]);
            bytes.extend_from_slice(&[10; 32]);
            bytes.extend_from_slice(&0_u32.to_be_bytes());
            bytes
        };
        let locator = TypedV2HistoryLocator {
            manifest: manifest.canonical_bytes().expect("encode manifest"),
            segments: Vec::new(),
            jumbo: Vec::new(),
            lineage_edge_set: Some(empty_lineage(0x55)),
            wire_revision: HISTORY_TYPED_V2_LOCATOR_LINEAGE_TAG,
        };
        let changed_lineage = TypedV2HistoryLocator {
            lineage_edge_set: Some(empty_lineage(0x56)),
            ..locator.clone()
        };
        let identity = files
            .typed_v2_locator_identity(&locator)
            .expect("identify canonical locator");
        assert!(
            locator
                .validate_lineage_parent_binding(
                    HistoryCommitId::from_bytes([0x55; 32]),
                    &[0x66; 32],
                )
                .is_ok()
        );
        assert_ne!(
            identity,
            files
                .typed_v2_locator_identity(&changed_lineage)
                .expect("lineage bytes affect the committed locator root")
        );
        let mut wrong_child_root = empty_lineage(0x55);
        wrong_child_root[72] ^= 1;
        let wrong_child_locator = TypedV2HistoryLocator {
            lineage_edge_set: Some(wrong_child_root),
            ..locator.clone()
        };
        assert!(wrong_child_locator.validate().is_err());

        let wrong_parent_locator = TypedV2HistoryLocator {
            lineage_edge_set: Some(empty_lineage(0x56)),
            ..locator.clone()
        };
        assert!(
            wrong_parent_locator
                .validate_lineage_parent_binding(
                    HistoryCommitId::from_bytes([0x55; 32]),
                    &[0x66; 32],
                )
                .is_err()
        );

        let mut tampered_body = locator.encode_body().expect("encode tag-15 body");
        let lineage_offset = tampered_body
            .windows(b"IRLEDGE1".len())
            .position(|window| window == b"IRLEDGE1")
            .expect("tag-15 payload is present");
        tampered_body[lineage_offset + 8] ^= 1;
        let tampered_identity = typed_v2_locator_identity(&tampered_body);
        let mut tampered_bytes = Vec::new();
        tampered_bytes.extend_from_slice(tampered_identity.as_bytes());
        tampered_bytes.extend_from_slice(&tampered_body);
        let tampered_checksum = blake3::hash(&tampered_bytes);
        tampered_bytes.extend_from_slice(tampered_checksum.as_bytes());
        assert!(decode_typed_v2_locator(&tampered_bytes, identity).is_err());
        assert_eq!(
            files
                .stage_typed_v2_locator(&target, commit, locator.clone())
                .expect("stage locator"),
            identity
        );
        assert_eq!(
            files
                .stage_typed_v2_locator(&target, commit, locator.clone())
                .expect("retry locator staging"),
            identity
        );
        let legacy_locator = TypedV2HistoryLocator {
            manifest: locator.manifest.clone(),
            segments: Vec::new(),
            jumbo: Vec::new(),
            lineage_edge_set: None,
            wire_revision: HISTORY_TYPED_V2_LOCATOR_TAG,
        };
        let legacy_body = legacy_locator.encode_body().expect("encode legacy locator");
        let legacy_identity = typed_v2_locator_identity(&legacy_body);
        let mut legacy_bytes = Vec::new();
        legacy_bytes.extend_from_slice(legacy_identity.as_bytes());
        legacy_bytes.extend_from_slice(&legacy_body);
        let legacy_checksum = blake3::hash(&legacy_bytes);
        legacy_bytes.extend_from_slice(legacy_checksum.as_bytes());
        assert_eq!(
            decode_typed_v2_locator(&legacy_bytes, legacy_identity)
                .expect("decode legacy locator")
                .encode_body()
                .expect("re-encode legacy locator"),
            legacy_body
        );
        drop(files);

        let reopened = LocalSemanticGenerationFiles::open(&directory.0).expect("cold reopen");
        assert_eq!(
            reopened
                .typed_v2_locator(&target, commit, identity)
                .expect("reopen typed locator"),
            locator
        );
        assert_eq!(
            reopened
                .reconcile_pending_typed_v2_locators(&target)
                .expect("reconcile orphan locator after reopen"),
            (1, false)
        );
        assert!(
            reopened
                .typed_v2_locator(&target, commit, identity)
                .expect_err("orphan locator is unavailable after recovery")
                .contains("missing")
        );
    }

    #[test]
    fn typed_locator_admission_preflights_exact_and_checked_object_counts() {
        assert!(TypedV2HistoryLocator::preflight_admission_counts(1, 0, 0).is_err());
        assert!(
            TypedV2HistoryLocator::preflight_admission_counts(0, usize::MAX, 1)
                .expect_err("overflow must reject before copying")
                .contains("overflows")
        );
        assert!(
            TypedV2HistoryLocator::preflight_admission_counts(
                0,
                0,
                MAX_HISTORY_TYPED_V2_LOCATOR_OBJECTS + 1,
            )
            .is_err()
        );
        assert!(TypedV2HistoryLocator::preflight_admission_counts(0, 0, 0).is_ok());
    }
}
