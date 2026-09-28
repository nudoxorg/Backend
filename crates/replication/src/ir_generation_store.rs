//! Durable selected semantic-image generations for local IR consumers.
//!
//! Generation records are immutable canonical catalog/manifest pairs. The
//! small per-target head is the only mutable pointer and carries the authority
//! stamp separately from the content-derived generation identity.

use std::fs;
use std::path::{Path, PathBuf};

use backend_semantic::ir::{
    GenerationId, SemanticDeltaCursor, SemanticImageIdentity, SemanticManifestError,
    SemanticManifestRoot, SemanticPlaneCatalog, SemanticPlaneImageKey, SemanticPlaneManifest,
};

use crate::{SelectedGenerationSource, SelectedGenerationStamp, SemanticTargetKey};

const MAGIC: [u8; 4] = *b"SIRG";
const VERSION: u8 = 1;
const RECORD_TAG: u8 = 1;
const HEAD_TAG: u8 = 2;
const CHECKSUM_BYTES: usize = 32;
const MAX_TARGET_FIELD_BYTES: usize = 4 * 1024;
const MAX_CATALOG_BYTES: usize = 8 * 1024 * 1024;
const MAX_MANIFEST_BYTES: usize = 64 * 1024 * 1024;
const MAX_GENERATION_RECORD_BYTES: usize = 72 * 1024 * 1024;
const MAX_HEAD_BYTES: usize = 320;

/// Content identity of one admitted target/catalog/image-manifest tuple.
///
/// It deliberately excludes the selected authority revision. The authority
/// stamp belongs to the mutable head observation; identical semantic metadata
/// can therefore remain one immutable local generation across authority reads.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LocalSemanticGenerationId([u8; 32]);

impl LocalSemanticGenerationId {
    /// Returns the content-derived identity bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// One cold-reopenable local selected image generation.
///
/// The catalog and manifest are decoded from the exact canonical bytes saved
/// before the local head moved. Coverage authority is not recreated by decode;
/// consumers must obtain fresh selection evidence for authority-sensitive use.
#[derive(Debug)]
pub struct LocalSemanticGeneration {
    identity: LocalSemanticGenerationId,
    previous_identity: Option<LocalSemanticGenerationId>,
    previous_image: Option<SemanticPlaneImageKey>,
    previous_image_identity: Option<SemanticImageIdentity>,
    target: SemanticTargetKey,
    selected_stamp: SelectedGenerationStamp,
    catalog: SemanticPlaneCatalog,
    image: SemanticPlaneImageKey,
    image_identity: SemanticImageIdentity,
    manifest: SemanticPlaneManifest,
}

impl LocalSemanticGeneration {
    /// Returns this immutable local generation identity.
    #[must_use]
    pub const fn identity(&self) -> LocalSemanticGenerationId {
        self.identity
    }

    /// Returns the preceding local generation identity retained by the head.
    #[must_use]
    pub const fn previous_identity(&self) -> Option<LocalSemanticGenerationId> {
        self.previous_identity
    }

    /// Exact canonical image key retained as this head's local predecessor,
    /// when both immutable records remain available.
    #[must_use]
    pub const fn previous_image(&self) -> Option<SemanticPlaneImageKey> {
        self.previous_image
    }

    /// Content identity of the preceding complete NXFI, if it remains in the
    /// immutable generation record.
    #[must_use]
    pub const fn previous_image_identity(&self) -> Option<SemanticImageIdentity> {
        self.previous_image_identity
    }

    /// Returns the target whose catalog was admitted.
    #[must_use]
    pub const fn target(&self) -> &SemanticTargetKey {
        &self.target
    }

    /// Returns the authority stamp observed when this head was published.
    #[must_use]
    pub const fn selected_stamp(&self) -> SelectedGenerationStamp {
        self.selected_stamp
    }

    /// Returns the admitted canonical catalog.
    #[must_use]
    pub const fn catalog(&self) -> &SemanticPlaneCatalog {
        &self.catalog
    }

    /// Returns the exact catalog image whose manifest is retained.
    #[must_use]
    pub const fn image(&self) -> SemanticPlaneImageKey {
        self.image
    }

    /// Returns the typed content identity of the persisted complete NXFI image.
    #[must_use]
    pub const fn image_identity(&self) -> SemanticImageIdentity {
        self.image_identity
    }

    /// Returns the canonical semantic IR generation named by the image.
    #[must_use]
    pub const fn semantic_generation(&self) -> GenerationId {
        self.image.semantic_generation()
    }

    /// Returns the canonical selected image manifest.
    #[must_use]
    pub const fn manifest(&self) -> &SemanticPlaneManifest {
        &self.manifest
    }

    /// Plans a canonical manifest-segment delta from this durable base image.
    ///
    /// The iterator is the semantic crate's ordered versioned-plane merge; it
    /// reports reuse eligibility, fetch, and removal actions without copying
    /// or reimplementing VCS rules. A cold-reopened manifest retains claims
    /// but not coverage capabilities, so the canonical planner may conservatively
    /// return `Fetch` for a byte-identical range. The local CAS still reuses it
    /// after the target segment hash is independently checked. This is a segment
    /// plan, not an entity-level `SemanticDiff`, which requires complete
    /// materialized semantic readers. Callers must admit the target manifest
    /// against a fresh selected catalog before acting on the returned plan.
    pub fn semantic_segment_delta<'base, 'target>(
        &'base self,
        target: &'target SemanticPlaneManifest,
    ) -> Result<SemanticDeltaCursor<'base, 'target>, SemanticManifestError> {
        SemanticDeltaCursor::new(&self.manifest, target, self.manifest.root())
    }
}

/// Private per-target immutable-record and atomic-head file layout.
pub(super) struct LocalSemanticGenerationFiles {
    root: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LocalHead {
    current: LocalSemanticGenerationId,
    previous: Option<LocalSemanticGenerationId>,
    stamp: SelectedGenerationStamp,
}

struct GenerationRecord {
    identity: LocalSemanticGenerationId,
    target: SemanticTargetKey,
    image: SemanticPlaneImageKey,
    image_identity: SemanticImageIdentity,
    catalog: SemanticPlaneCatalog,
    manifest: SemanticPlaneManifest,
}

impl LocalSemanticGenerationFiles {
    pub(super) fn open(root: &Path) -> Result<Self, String> {
        let root = root.join("generations");
        create_private_directory(&root)?;
        set_private_directory(&root)?;
        Ok(Self { root })
    }

    pub(super) fn current(
        &self,
        target: &SemanticTargetKey,
    ) -> Result<Option<LocalSemanticGeneration>, String> {
        let target_root = self.target_root(target);
        if !ensure_optional_directory(&target_root)? {
            return Ok(None);
        }
        let records_root = target_root.join("records");
        let records_exist = ensure_optional_directory(&records_root)?;
        let head_path = target_root.join("HEAD");
        let Some(head_bytes) = read_optional_bounded(&head_path, MAX_HEAD_BYTES)? else {
            if records_exist {
                prune_records(&target_root, None, None)?;
            }
            return Ok(None);
        };
        if !records_exist {
            return Err("local semantic generation records directory is missing".to_owned());
        }
        let head = decode_head(&head_bytes)?;
        let record = load_record(&target_root, head.current, target)?;
        validate_record_selection(&record, head.stamp)?;
        let previous_record = head
            .previous
            .map(|previous| load_record(&target_root, previous, target))
            .transpose()?;
        let previous_image = previous_record.as_ref().map(|previous| previous.image);
        let previous_image_identity = previous_record
            .as_ref()
            .map(|previous| previous.image_identity);
        prune_records(&target_root, Some(head.current), head.previous)?;
        Ok(Some(LocalSemanticGeneration {
            identity: record.identity,
            previous_identity: head.previous,
            previous_image,
            previous_image_identity,
            target: record.target,
            selected_stamp: head.stamp,
            catalog: record.catalog,
            image: record.image,
            image_identity: record.image_identity,
            manifest: record.manifest,
        }))
    }

    pub(super) fn commit<S: SelectedGenerationSource>(
        &self,
        target: &SemanticTargetKey,
        stamp: SelectedGenerationStamp,
        catalog: &SemanticPlaneCatalog,
        image: SemanticPlaneImageKey,
        image_identity: SemanticImageIdentity,
        manifest: &SemanticPlaneManifest,
        source: &mut S,
    ) -> Result<LocalSemanticGeneration, String> {
        let record_bytes =
            encode_generation_record(target, catalog, image, image_identity, manifest)?;
        let record = decode_generation_record(&record_bytes)?;
        validate_record_selection(&record, stamp)?;
        let target_root = self.target_root(target);
        let records_root = target_root.join("records");
        create_private_directory(&target_root)?;
        create_private_directory(&records_root)?;
        set_private_directory(&target_root)?;
        set_private_directory(&records_root)?;

        require_current(source, stamp, image)?;
        let head_path = target_root.join("HEAD");
        let prior = read_optional_bounded(&head_path, MAX_HEAD_BYTES)?
            .as_deref()
            .map(decode_head)
            .transpose()?;
        if let Some(prior) = prior {
            let prior_record = load_record(&target_root, prior.current, target)?;
            validate_record_selection(&prior_record, prior.stamp)?;
            if let Some(previous) = prior.previous {
                let _previous_record = load_record(&target_root, previous, target)?;
            }
        }
        let next = advance_head(prior, record.identity, stamp)?;

        // Persist the immutable record first. If the process stops here, HEAD
        // still identifies the prior generation and the orphan is reclaimed
        // by the next open or commit.
        let immutable_path = record_path(&target_root, record.identity);
        match fs::read(&immutable_path) {
            Ok(existing) if existing == record_bytes => {}
            Ok(_) => return Err("immutable semantic generation identity collision".to_owned()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                backend_platform::durable::write_private_atomic(&immutable_path, &record_bytes)
                    .map_err(display_io)?;
            }
            Err(error) => return Err(display_io(error)),
        }

        // Recheck immediately before the atomic pointer change. A stale
        // candidate can leave an immutable orphan, but it cannot move HEAD.
        if let Err(error) = require_current(source, stamp, image) {
            let keep_current = prior.map(|head| head.current);
            let keep_previous = prior.and_then(|head| head.previous);
            prune_records(&target_root, keep_current, keep_previous)?;
            return Err(error);
        }
        let head_bytes = encode_head(next)?;
        backend_platform::durable::write_private_atomic(&head_path, &head_bytes)
            .map_err(display_io)?;
        let previous_record = next
            .previous
            .map(|previous| load_record(&target_root, previous, target))
            .transpose()?;
        let previous_image = previous_record.as_ref().map(|previous| previous.image);
        let previous_image_identity = previous_record
            .as_ref()
            .map(|previous| previous.image_identity);
        prune_records(&target_root, Some(next.current), next.previous)?;

        Ok(LocalSemanticGeneration {
            identity: record.identity,
            previous_identity: next.previous,
            previous_image,
            previous_image_identity,
            target: record.target,
            selected_stamp: stamp,
            catalog: record.catalog,
            image: record.image,
            image_identity: record.image_identity,
            manifest: record.manifest,
        })
    }

    fn target_root(&self, target: &SemanticTargetKey) -> PathBuf {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"backend.semantic.local-generation-target.v1\0");
        hash_target(&mut hasher, target);
        self.root.join(hex(hasher.finalize().as_bytes()))
    }
}

fn require_current<S: SelectedGenerationSource>(
    source: &mut S,
    expected: SelectedGenerationStamp,
    image: SemanticPlaneImageKey,
) -> Result<(), String> {
    let current = source
        .current_selected_generation()
        .map_err(|error| format!("read current selected semantic generation: {error}"))?;
    if current != expected
        || !source
            .selected_image_is_current(expected, image)
            .map_err(|error| format!("check selected semantic image: {error}"))?
    {
        return Err(
            "selected semantic generation became stale before local head commit".to_owned(),
        );
    }
    Ok(())
}

fn advance_head(
    prior: Option<LocalHead>,
    identity: LocalSemanticGenerationId,
    stamp: SelectedGenerationStamp,
) -> Result<LocalHead, String> {
    let Some(prior) = prior else {
        return Ok(LocalHead {
            current: identity,
            previous: None,
            stamp,
        });
    };
    if prior.stamp.namespace() != stamp.namespace()
        || prior.stamp.profile() != stamp.profile()
        || prior.stamp.source_coordinate() != stamp.source_coordinate()
    {
        return Err("local semantic head belongs to another authority scope".to_owned());
    }
    match stamp
        .selection_revision()
        .cmp(&prior.stamp.selection_revision())
    {
        std::cmp::Ordering::Less => {
            return Err("stale selected authority revision cannot replace local head".to_owned());
        }
        std::cmp::Ordering::Equal => {
            if stamp != prior.stamp || identity != prior.current {
                return Err(
                    "same-revision semantic head conflicts with the selected root".to_owned(),
                );
            }
            return Ok(prior);
        }
        std::cmp::Ordering::Greater => {}
    }
    if identity == prior.current {
        Ok(LocalHead {
            current: identity,
            previous: prior.previous,
            stamp,
        })
    } else {
        Ok(LocalHead {
            current: identity,
            previous: Some(prior.current),
            stamp,
        })
    }
}

fn validate_record_selection(
    record: &GenerationRecord,
    stamp: SelectedGenerationStamp,
) -> Result<(), String> {
    if record.target.profile() != stamp.profile()
        || record.catalog.root() != stamp.catalog_root()
        || record.manifest.root() != record.image.manifest_root()
        || record.manifest.semantic_generation() != record.image.semantic_generation()
        || !record
            .catalog
            .entries()
            .iter()
            .any(|entry| entry.image() == record.image)
    {
        return Err("local semantic generation record differs from its authority head".to_owned());
    }
    Ok(())
}

fn encode_generation_record(
    target: &SemanticTargetKey,
    catalog: &SemanticPlaneCatalog,
    image: SemanticPlaneImageKey,
    image_identity: SemanticImageIdentity,
    manifest: &SemanticPlaneManifest,
) -> Result<Vec<u8>, String> {
    if target.profile() != manifest.build().profile()
        || image.manifest_root() != manifest.root()
        || image.semantic_generation() != manifest.semantic_generation()
    {
        return Err("semantic generation metadata has inconsistent target or image".to_owned());
    }
    let catalog_bytes = catalog.encode().map_err(display_error)?;
    let manifest_bytes = manifest.encode().map_err(display_error)?;
    if catalog_bytes.len() > MAX_CATALOG_BYTES || manifest_bytes.len() > MAX_MANIFEST_BYTES {
        return Err("semantic generation metadata exceeds its storage bound".to_owned());
    }
    let entry = catalog
        .entries()
        .iter()
        .find(|entry| entry.image() == image)
        .ok_or_else(|| "semantic image is absent from its admitted catalog".to_owned())?;
    if usize::try_from(entry.manifest_length()).ok() != Some(manifest_bytes.len()) {
        return Err("semantic manifest length differs from its catalog entry".to_owned());
    }
    let mut writer = Writer::new(MAX_GENERATION_RECORD_BYTES - CHECKSUM_BYTES - 32);
    writer.header(RECORD_TAG)?;
    writer.target(target)?;
    writer.u32(image.artifact_ordinal())?;
    writer.fixed(image.semantic_generation().as_bytes())?;
    writer.fixed(image.manifest_root().as_bytes())?;
    writer.fixed(image_identity.as_ref())?;
    writer.sized_bytes(&catalog_bytes, MAX_CATALOG_BYTES)?;
    writer.sized_bytes(&manifest_bytes, MAX_MANIFEST_BYTES)?;
    let body = writer.finish();
    let identity = generation_identity(&body);
    let mut output = Vec::with_capacity(body.len() + 32 + CHECKSUM_BYTES);
    output.extend_from_slice(&identity.0);
    output.extend_from_slice(&body);
    append_checksum(&mut output)?;
    Ok(output)
}

fn decode_generation_record(bytes: &[u8]) -> Result<GenerationRecord, String> {
    let body = checked_body(bytes, MAX_GENERATION_RECORD_BYTES)?;
    if body.len() < 32 {
        return Err("semantic generation record is truncated".to_owned());
    }
    let identity = LocalSemanticGenerationId(
        body[..32]
            .try_into()
            .map_err(|_| "semantic generation identity is truncated".to_owned())?,
    );
    let content = &body[32..];
    if generation_identity(content) != identity {
        return Err("semantic generation identity does not match its record".to_owned());
    }
    let mut reader = Reader::new(content);
    reader.header(RECORD_TAG)?;
    let target = reader.target()?;
    let ordinal = reader.u32()?;
    let semantic_generation = GenerationId::from_raw(reader.fixed()?);
    let manifest_root = SemanticManifestRoot::from_wire_claim(reader.fixed()?);
    let image_identity = SemanticImageIdentity::try_from(reader.fixed()?).map_err(display_error)?;
    let catalog_bytes = reader.sized_bytes(MAX_CATALOG_BYTES)?;
    let manifest_bytes = reader.sized_bytes(MAX_MANIFEST_BYTES)?;
    reader.finish()?;
    let catalog = SemanticPlaneCatalog::decode(catalog_bytes).map_err(display_error)?;
    let manifest = SemanticPlaneManifest::decode(manifest_bytes).map_err(display_error)?;
    let image = SemanticPlaneImageKey::new(ordinal, semantic_generation, manifest_root);
    if manifest.root() != manifest_root
        || manifest.semantic_generation() != semantic_generation
        || !catalog.entries().iter().any(|entry| entry.image() == image)
        || catalog
            .entries()
            .iter()
            .find(|entry| entry.image() == image)
            .is_none_or(|entry| entry.manifest_length() as usize != manifest_bytes.len())
    {
        return Err(
            "semantic generation record contains inconsistent canonical metadata".to_owned(),
        );
    }
    Ok(GenerationRecord {
        identity,
        target,
        image,
        image_identity,
        catalog,
        manifest,
    })
}

fn encode_head(head: LocalHead) -> Result<Vec<u8>, String> {
    let mut writer = Writer::new(MAX_HEAD_BYTES - CHECKSUM_BYTES);
    writer.header(HEAD_TAG)?;
    writer.fixed(&head.current.0)?;
    match head.previous {
        Some(previous) => {
            writer.u8(1)?;
            writer.fixed(&previous.0)?;
        }
        None => writer.u8(0)?,
    }
    writer.stamp(head.stamp)?;
    let mut bytes = writer.finish();
    append_checksum(&mut bytes)?;
    Ok(bytes)
}

fn decode_head(bytes: &[u8]) -> Result<LocalHead, String> {
    let body = checked_body(bytes, MAX_HEAD_BYTES)?;
    let mut reader = Reader::new(body);
    reader.header(HEAD_TAG)?;
    let current = LocalSemanticGenerationId(reader.fixed()?);
    let previous = match reader.u8()? {
        0 => None,
        1 => Some(LocalSemanticGenerationId(reader.fixed()?)),
        _ => return Err("local semantic head has an invalid predecessor marker".to_owned()),
    };
    let stamp = reader.stamp()?;
    reader.finish()?;
    if previous == Some(current) {
        return Err("local semantic head repeats its current generation".to_owned());
    }
    Ok(LocalHead {
        current,
        previous,
        stamp,
    })
}

fn prune_records(
    target_root: &Path,
    current: Option<LocalSemanticGenerationId>,
    previous: Option<LocalSemanticGenerationId>,
) -> Result<(), String> {
    let records = target_root.join("records");
    let mut entries = Vec::new();
    for entry in fs::read_dir(&records).map_err(display_io)? {
        let entry = entry.map_err(display_io)?;
        let path = entry.path();
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "semantic generation filename is not UTF-8".to_owned())?;
        let Some(stem) = name.strip_suffix(".record") else {
            if name.ends_with(".tmp") {
                ensure_regular_file(&path)?;
                entries.push((path, None));
                if entries.len() > 64 {
                    return Err("semantic generation recovery scan exceeds its bound".to_owned());
                }
                continue;
            }
            return Err("semantic generation directory contains an unknown member".to_owned());
        };
        if !is_hex_digest(stem) {
            return Err("semantic generation filename is malformed".to_owned());
        }
        ensure_regular_file(&path)?;
        entries.push((path, Some(stem.to_owned())));
        if entries.len() > 64 {
            return Err("semantic generation recovery scan exceeds its bound".to_owned());
        }
    }
    for (path, stem) in entries {
        let Some(stem) = stem else {
            remove_file(&path)?;
            continue;
        };
        let keep = current.is_some_and(|id| stem == hex(&id.0))
            || previous.is_some_and(|id| stem == hex(&id.0));
        if !keep {
            remove_file(&path)?;
        }
    }
    Ok(())
}

fn record_path(target_root: &Path, identity: LocalSemanticGenerationId) -> PathBuf {
    target_root
        .join("records")
        .join(format!("{}.record", hex(&identity.0)))
}

fn load_record(
    target_root: &Path,
    identity: LocalSemanticGenerationId,
    target: &SemanticTargetKey,
) -> Result<GenerationRecord, String> {
    let path = record_path(target_root, identity);
    let bytes = read_optional_bounded(&path, MAX_GENERATION_RECORD_BYTES)?
        .ok_or_else(|| "local semantic generation head references a missing record".to_owned())?;
    let record = decode_generation_record(&bytes)?;
    if record.identity != identity || &record.target != target {
        return Err("local semantic generation head references another target".to_owned());
    }
    Ok(record)
}

fn generation_identity(content: &[u8]) -> LocalSemanticGenerationId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.semantic.local-generation.v1\0");
    hasher.update(content);
    LocalSemanticGenerationId(*hasher.finalize().as_bytes())
}

fn hash_target(hasher: &mut blake3::Hasher, target: &SemanticTargetKey) {
    hasher.update(&(target.package().len() as u64).to_be_bytes());
    hasher.update(target.package().as_bytes());
    hasher.update(&(target.coordinate().len() as u64).to_be_bytes());
    hasher.update(target.coordinate().as_bytes());
    hasher.update(&<[u8; 2]>::from(target.profile()));
}

struct Writer {
    bytes: Vec<u8>,
    maximum: usize,
}

impl Writer {
    fn new(maximum: usize) -> Self {
        Self {
            bytes: Vec::new(),
            maximum,
        }
    }

    fn push(&mut self, bytes: &[u8]) -> Result<(), String> {
        let total = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .ok_or_else(|| "semantic generation record length overflows".to_owned())?;
        if total > self.maximum {
            return Err("semantic generation record exceeds its bound".to_owned());
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    fn header(&mut self, tag: u8) -> Result<(), String> {
        self.push(&MAGIC)?;
        self.u8(VERSION)?;
        self.u8(tag)
    }

    fn target(&mut self, target: &SemanticTargetKey) -> Result<(), String> {
        self.sized_bytes(target.package().as_bytes(), MAX_TARGET_FIELD_BYTES)?;
        self.sized_bytes(target.coordinate().as_bytes(), MAX_TARGET_FIELD_BYTES)?;
        self.push(&<[u8; 2]>::from(target.profile()))
    }

    fn stamp(&mut self, stamp: SelectedGenerationStamp) -> Result<(), String> {
        self.push(stamp.namespace())?;
        self.push(&<[u8; 2]>::from(stamp.profile()))?;
        self.push(stamp.source_coordinate())?;
        self.push(&stamp.selection_revision().to_be_bytes())?;
        self.push(stamp.selected_root())?;
        self.push(stamp.closure_id())?;
        self.push(stamp.catalog_root().as_bytes())
    }

    fn u8(&mut self, value: u8) -> Result<(), String> {
        self.push(&[value])
    }

    fn u32(&mut self, value: u32) -> Result<(), String> {
        self.push(&value.to_be_bytes())
    }

    fn fixed(&mut self, value: &[u8]) -> Result<(), String> {
        self.push(value)
    }

    fn sized_bytes(&mut self, value: &[u8], maximum: usize) -> Result<(), String> {
        if value.len() > maximum {
            return Err("semantic generation field exceeds its bound".to_owned());
        }
        self.u64(u64::try_from(value.len()).map_err(display_error)?)?;
        self.push(value)
    }

    fn u64(&mut self, value: u64) -> Result<(), String> {
        self.push(&value.to_be_bytes())
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], String> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or_else(|| "semantic generation offset overflows".to_owned())?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or_else(|| "semantic generation record is truncated".to_owned())?;
        self.offset = end;
        Ok(value)
    }

    fn header(&mut self, expected_tag: u8) -> Result<(), String> {
        if self.take(4)? != MAGIC || self.u8()? != VERSION || self.u8()? != expected_tag {
            return Err("semantic generation record header is invalid".to_owned());
        }
        Ok(())
    }

    fn target(&mut self) -> Result<SemanticTargetKey, String> {
        let package = std::str::from_utf8(self.sized_bytes(MAX_TARGET_FIELD_BYTES)?)
            .map_err(|_| "semantic target package is not UTF-8".to_owned())?;
        let coordinate = std::str::from_utf8(self.sized_bytes(MAX_TARGET_FIELD_BYTES)?)
            .map_err(|_| "semantic target coordinate is not UTF-8".to_owned())?;
        let profile = backend_semantic::ir::LanguageProfile::try_from(self.fixed()?)
            .map_err(display_error)?;
        SemanticTargetKey::new(package, coordinate, profile).map_err(display_error)
    }

    fn stamp(&mut self) -> Result<SelectedGenerationStamp, String> {
        let namespace = self.fixed()?;
        let profile = backend_semantic::ir::LanguageProfile::try_from(self.fixed()?)
            .map_err(display_error)?;
        let source_coordinate = self.fixed()?;
        let selection_revision = self.u64()?;
        let selected_root = self.fixed()?;
        let closure_id = self.fixed()?;
        let catalog_root =
            backend_semantic::ir::SemanticPlaneCatalogRoot::from_wire_claim(self.fixed()?);
        SelectedGenerationStamp::checked(
            namespace,
            profile,
            source_coordinate,
            selection_revision,
            selected_root,
            closure_id,
            catalog_root,
        )
        .map_err(display_error)
    }

    fn sized_bytes(&mut self, maximum: usize) -> Result<&'a [u8], String> {
        let length = usize::try_from(self.u64()?)
            .map_err(|_| "semantic generation field exceeds address space".to_owned())?;
        if length > maximum {
            return Err("semantic generation field exceeds its bound".to_owned());
        }
        self.take(length)
    }

    fn fixed<const N: usize>(&mut self) -> Result<[u8; N], String> {
        self.take(N)?
            .try_into()
            .map_err(|_| "semantic generation field has the wrong width".to_owned())
    }

    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.fixed::<1>()?[0])
    }

    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_be_bytes(self.fixed()?))
    }

    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_be_bytes(self.fixed()?))
    }

    fn finish(&self) -> Result<(), String> {
        if self.offset == self.bytes.len() {
            Ok(())
        } else {
            Err("semantic generation record has trailing bytes".to_owned())
        }
    }
}

fn checked_body(bytes: &[u8], maximum: usize) -> Result<&[u8], String> {
    if bytes.len() < CHECKSUM_BYTES + 6 || bytes.len() > maximum {
        return Err("semantic generation record length is invalid".to_owned());
    }
    let body_length = bytes.len() - CHECKSUM_BYTES;
    if blake3::hash(&bytes[..body_length]).as_bytes() != &bytes[body_length..] {
        return Err("semantic generation record checksum failed".to_owned());
    }
    Ok(&bytes[..body_length])
}

fn append_checksum(bytes: &mut Vec<u8>) -> Result<(), String> {
    if bytes.len().saturating_add(CHECKSUM_BYTES) > MAX_GENERATION_RECORD_BYTES {
        return Err("semantic generation record exceeds its bound".to_owned());
    }
    let checksum = blake3::hash(bytes);
    bytes.extend_from_slice(checksum.as_bytes());
    Ok(())
}

fn read_optional_bounded(path: &Path, maximum: usize) -> Result<Option<Vec<u8>>, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(display_io(error)),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > maximum as u64 {
        return Err("semantic generation state member is not a bounded regular file".to_owned());
    }
    fs::read(path).map(Some).map_err(display_io)
}

fn ensure_directory(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(display_io)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("semantic generation state path is not a directory".to_owned());
    }
    Ok(())
}

fn ensure_optional_directory(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => {
            ensure_directory(path)?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(display_io(error)),
    }
}

fn create_private_directory(path: &Path) -> Result<(), String> {
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => ensure_directory(path),
        Err(error) => Err(display_io(error)),
    }
}

fn ensure_regular_file(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(display_io)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("semantic generation state member is not a regular file".to_owned());
    }
    Ok(())
}

fn set_private_directory(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut permissions = fs::metadata(path).map_err(display_io)?.permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(path, permissions).map_err(display_io)?;
    }
    Ok(())
}

fn remove_file(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => backend_platform::durable::sync_parent(path).map_err(display_io),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(display_io(error)),
    }
}

fn is_hex_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn display_io(error: std::io::Error) -> String {
    error.to_string()
}

fn display_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use backend_semantic::ir::{
        GenerationId, LanguageProfile, RustEdition, SemanticBuildIdentity, SemanticInputWitness,
        SemanticIrPlane, SemanticPlane, SemanticPlaneCatalogEntry, SemanticPlaneKind,
        SemanticPlaneSegment,
    };
    use backend_semantic::vocabulary::Stage;
    use backend_version::{Coverage, ScopeRoot};

    use super::*;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn create() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "backend-semantic-generation-store-{}-{nonce}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).expect("create unique generation fixture directory");
            set_private_directory(&path).expect("private generation fixture directory");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    struct Fixture {
        target: SemanticTargetKey,
        catalog: SemanticPlaneCatalog,
        image: SemanticPlaneImageKey,
        image_identity: SemanticImageIdentity,
        manifest: SemanticPlaneManifest,
        stamp: SelectedGenerationStamp,
    }

    fn fixture(payload: &[u8], revision: u64, root: u8) -> Fixture {
        let profile = LanguageProfile::Rust(RustEdition::Rust2024);
        let target = SemanticTargetKey::new(
            "pkg:cargo/tentpole-app@1.2.3",
            "pkg:cargo/tentpole-app@1.2.3",
            profile,
        )
        .expect("fixture target");
        let kind = SemanticPlaneKind::Ir(SemanticIrPlane::Core);
        let segment = SemanticPlaneSegment::from_payload(kind, [1; 32], [1; 32], 1, payload)
            .expect("fixture segment");
        let plane =
            SemanticPlane::claimed(kind, vec![segment], Coverage::Complete).expect("fixture plane");
        let manifest = SemanticPlaneManifest::new(
            GenerationId::from_canonical_bytes(payload),
            SemanticBuildIdentity::new(
                [1; 32],
                [2; 32],
                profile,
                Stage::LowerIr,
                [3; 32],
                [4; 32],
                [5; 32],
                [6; 32],
            ),
            SemanticInputWitness::claimed([7; 32], ScopeRoot::from_bytes([8; 32])),
            vec![plane],
        )
        .expect("fixture manifest");
        let image = SemanticPlaneImageKey::from_manifest(0, &manifest);
        let image_identity = SemanticImageIdentity::from_encoded_bytes(payload);
        let manifest_length = u32::try_from(manifest.encode().expect("manifest encoding").len())
            .expect("small manifest length");
        let catalog = SemanticPlaneCatalog::new(vec![
            SemanticPlaneCatalogEntry::new(image, manifest_length).expect("catalog entry"),
        ])
        .expect("fixture catalog");
        let stamp = SelectedGenerationStamp::checked(
            [9; 16],
            profile,
            [10; 32],
            revision,
            [root; 32],
            [11; 32],
            catalog.root(),
        )
        .expect("fixture selected stamp");
        Fixture {
            target,
            catalog,
            image,
            image_identity,
            manifest,
            stamp,
        }
    }

    struct TestAuthority {
        observations: VecDeque<SelectedGenerationStamp>,
        current: SelectedGenerationStamp,
        images: Vec<SemanticPlaneImageKey>,
    }

    impl TestAuthority {
        fn new(
            observations: impl IntoIterator<Item = SelectedGenerationStamp>,
            images: impl IntoIterator<Item = SemanticPlaneImageKey>,
        ) -> Self {
            let mut observations = observations.into_iter().collect::<VecDeque<_>>();
            let current = observations
                .pop_front()
                .expect("at least one selected stamp");
            Self {
                observations,
                current,
                images: images.into_iter().collect(),
            }
        }
    }

    impl SelectedGenerationSource for TestAuthority {
        type Error = &'static str;

        fn current_selected_generation(&mut self) -> Result<SelectedGenerationStamp, Self::Error> {
            if let Some(next) = self.observations.pop_front() {
                self.current = next;
            }
            Ok(self.current)
        }

        fn selected_image_is_current(
            &mut self,
            expected_stamp: SelectedGenerationStamp,
            image: SemanticPlaneImageKey,
        ) -> Result<bool, Self::Error> {
            Ok(self.current == expected_stamp && self.images.contains(&image))
        }
    }

    fn commit(
        files: &LocalSemanticGenerationFiles,
        fixture: &Fixture,
        stamps: impl IntoIterator<Item = SelectedGenerationStamp>,
    ) -> Result<LocalSemanticGeneration, String> {
        files.commit(
            &fixture.target,
            fixture.stamp,
            &fixture.catalog,
            fixture.image,
            fixture.image_identity,
            &fixture.manifest,
            &mut TestAuthority::new(stamps, [fixture.image]),
        )
    }

    #[test]
    fn selected_generation_cold_reopens_from_its_checksummed_record() {
        let directory = TestDirectory::create();
        let fixture = fixture(b"full semantic generation one", 1, 12);
        let files = LocalSemanticGenerationFiles::open(&directory.0).expect("open store");
        let committed = commit(&files, &fixture, [fixture.stamp, fixture.stamp])
            .expect("commit selected generation");
        let local_identity = committed.identity();
        let semantic_generation = committed.semantic_generation();
        assert_ne!(local_identity.as_bytes(), semantic_generation.as_bytes());

        drop(files);
        let reopened_files =
            LocalSemanticGenerationFiles::open(&directory.0).expect("reopen store");
        let reopened = reopened_files
            .current(&fixture.target)
            .expect("read durable head")
            .expect("head exists");
        assert_eq!(reopened.identity(), local_identity);
        assert_eq!(reopened.semantic_generation(), semantic_generation);
        assert_eq!(reopened.selected_stamp(), fixture.stamp);
        assert_eq!(reopened.catalog().root(), fixture.catalog.root());
        assert_eq!(reopened.manifest().root(), fixture.manifest.root());
    }

    #[test]
    fn orphaned_immutable_record_from_interrupted_commit_is_reclaimed() {
        let directory = TestDirectory::create();
        let fixture = fixture(b"generation interrupted before HEAD", 1, 13);
        let files = LocalSemanticGenerationFiles::open(&directory.0).expect("open store");
        let target_root = files.target_root(&fixture.target);
        let records_root = target_root.join("records");
        fs::create_dir_all(&records_root).expect("create record directory");
        set_private_directory(&target_root).expect("private target directory");
        set_private_directory(&records_root).expect("private record directory");

        let encoded = encode_generation_record(
            &fixture.target,
            &fixture.catalog,
            fixture.image,
            fixture.image_identity,
            &fixture.manifest,
        )
        .expect("encode immutable record");
        let record = decode_generation_record(&encoded).expect("decode immutable record");
        let orphan = record_path(&target_root, record.identity);
        backend_platform::durable::write_private_atomic(&orphan, &encoded)
            .expect("persist record before simulated crash");
        let temporary = records_root.join("interrupted.tmp");
        fs::write(&temporary, b"partial temporary member").expect("write interrupted temp");

        assert!(
            files
                .current(&fixture.target)
                .expect("recover absent HEAD")
                .is_none()
        );
        assert!(!orphan.exists());
        assert!(!temporary.exists());
    }

    #[test]
    fn same_generation_reuses_immutable_identity_while_advancing_authority_stamp() {
        let directory = TestDirectory::create();
        let first = fixture(b"unchanged canonical metadata", 1, 14);
        let second = fixture(b"unchanged canonical metadata", 2, 15);
        let files = LocalSemanticGenerationFiles::open(&directory.0).expect("open store");
        let first_head =
            commit(&files, &first, [first.stamp, first.stamp]).expect("commit initial head");
        let second_head = commit(&files, &second, [second.stamp, second.stamp])
            .expect("advance authority for same content");

        assert_eq!(second_head.identity(), first_head.identity());
        assert_eq!(second_head.previous_identity(), None);
        assert_eq!(second_head.selected_stamp(), second.stamp);
        let reopened = files
            .current(&second.target)
            .expect("read current head")
            .expect("current head exists");
        assert_eq!(reopened.identity(), first_head.identity());
        assert_eq!(reopened.selected_stamp(), second.stamp);
    }

    #[test]
    fn changed_generation_keeps_one_base_and_rejects_stale_or_same_revision_fork() {
        let directory = TestDirectory::create();
        let base = fixture(b"semantic generation base", 1, 16);
        let target = fixture(b"semantic generation target", 2, 17);
        let stale = fixture(b"stale authority candidate", 1, 18);
        let fork = fixture(b"same revision fork", 2, 19);
        let files = LocalSemanticGenerationFiles::open(&directory.0).expect("open store");

        let base_head = commit(&files, &base, [base.stamp, base.stamp]).expect("commit base");
        let target_head = commit(&files, &target, [target.stamp, target.stamp])
            .expect("commit changed generation");
        assert_ne!(target_head.identity(), base_head.identity());
        assert_eq!(target_head.previous_identity(), Some(base_head.identity()));

        let stale_result = commit(&files, &stale, [stale.stamp, stale.stamp]);
        assert!(
            stale_result
                .expect_err("lower authority revision is stale")
                .contains("stale selected authority revision")
        );
        let fork_result = commit(&files, &fork, [fork.stamp, fork.stamp]);
        assert!(
            fork_result
                .expect_err("same-revision changed root is a conflict")
                .contains("same-revision semantic head conflicts")
        );

        let current = files
            .current(&target.target)
            .expect("read unchanged current head")
            .expect("current head exists");
        assert_eq!(current.identity(), target_head.identity());
        assert_eq!(current.previous_identity(), Some(base_head.identity()));

        let missing_base = record_path(&files.target_root(&target.target), base_head.identity());
        fs::remove_file(missing_base).expect("remove retained base to test fail-closed reopen");
        assert!(
            files
                .current(&target.target)
                .expect_err("missing predecessor must fail closed")
                .contains("missing record")
        );
    }

    #[test]
    fn recovery_bound_is_checked_before_any_record_is_removed() {
        let directory = TestDirectory::create();
        let fixture = fixture(b"bounded recovery", 1, 20);
        let files = LocalSemanticGenerationFiles::open(&directory.0).expect("open store");
        let target_root = files.target_root(&fixture.target);
        let records_root = target_root.join("records");
        fs::create_dir_all(&records_root).expect("create record directory");
        for index in 0..=64 {
            let path = records_root.join(format!("{:064x}.record", index));
            fs::write(path, b"orphan").expect("write synthetic orphan");
        }

        assert!(
            prune_records(&target_root, None, None)
                .expect_err("recovery scan exceeds its bound")
                .contains("exceeds its bound")
        );
        assert_eq!(
            fs::read_dir(records_root)
                .expect("enumerate records")
                .count(),
            65
        );
    }
}
