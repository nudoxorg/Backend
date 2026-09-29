//! Durable, resumable admission of complete canonical semantic images.
//!
//! Pages are written sequentially to a private staging file. A small checksummed
//! cursor is atomically replaced only after the corresponding page is synced.
//! Completed images are promoted to immutable content-addressed files and
//! reopened by the semantic crate's validated read-only mapping.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use backend_semantic::ir::{
    GenerationId, MappedSemanticImage, MappedSemanticImageError, SemanticImageIdentity,
    SemanticPlaneImageKey, load_semantic_image_mmap,
};

use crate::{ByteRange, MAX_RANGE_BYTES, MAX_SEMANTIC_IMAGE_BYTES, SemanticTargetKey};

const CHECKPOINT_MAGIC: [u8; 4] = *b"SIMG";
const CHECKPOINT_VERSION: u8 = 1;
const CHECKSUM_BYTES: usize = 32;
const CHECKPOINT_BYTES: usize = 4 + 1 + 4 + 32 + 32 + 32 + 8 + 8 + CHECKSUM_BYTES;
const MAX_IMAGE_STAGES: usize = 64;
const MAX_IMAGE_STAGE_SCAN: usize = 4096;
const MAX_IMAGE_MEMBERS: usize = MAX_IMAGE_STAGE_SCAN * 2;
const MAX_IMAGE_OBJECT_FILES: usize = 4096;
const MAX_IMAGE_TARGETS: usize = 4096;
const LOCATOR_BYTES: usize = 4 + 4 + 32 + 32 + 32 + CHECKSUM_BYTES;

/// Result class for reopening an image from the local semantic cache.
///
/// Only `CorruptContent` is eligible for cache repair, and callers must first
/// bind the request to a fresh selected-owner image page. Filesystem and path
/// failures stay hard errors; a content whose bytes hash correctly but name a
/// different canonical generation is a selection inconsistency, not a reason
/// to delete a potentially shared valid object.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SemanticImageCacheError {
    /// A locator or content object is incomplete or fails canonical admission.
    CorruptContent {
        /// Known content-address identity, if the damaged member named one.
        identity: Option<SemanticImageIdentity>,
        /// Bounded local diagnostic.
        detail: String,
    },
    /// Content identity is valid, but its canonical generation differs from
    /// the selected image key or selected owner-page claim.
    SelectionMismatch {
        /// Content identity observed in the local content-address path.
        identity: SemanticImageIdentity,
        /// Bounded local diagnostic.
        detail: String,
    },
    /// The staged owner response could not be admitted as the requested
    /// canonical image. This is a transfer/protocol failure, not a cache hit
    /// that can be repaired by deleting an existing object.
    TransferRejected(String),
    /// Filesystem, permissions, symlink, or transient read failure.
    Storage(String),
}

impl std::fmt::Display for SemanticImageCacheError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CorruptContent { detail, .. } => {
                write!(formatter, "local semantic image cache is corrupt: {detail}")
            }
            Self::SelectionMismatch { detail, .. } => {
                write!(
                    formatter,
                    "local semantic image selection mismatch: {detail}"
                )
            }
            Self::TransferRejected(detail) => {
                write!(
                    formatter,
                    "selected semantic image transfer was rejected: {detail}"
                )
            }
            Self::Storage(detail) => formatter.write_str(detail),
        }
    }
}

impl From<String> for SemanticImageCacheError {
    fn from(detail: String) -> Self {
        Self::Storage(detail)
    }
}

/// Cursor persisted after each synced page. A file-backed cache is a locator;
/// `open_image` rechecks its complete typed identities and NXFI grammar.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticImageResume {
    image: SemanticPlaneImageKey,
    identity: SemanticImageIdentity,
    total_length: u64,
    next_offset: u64,
}

impl SemanticImageResume {
    /// Exact catalog image key represented by this durable transfer.
    #[must_use]
    pub const fn image(&self) -> SemanticPlaneImageKey {
        self.image
    }

    /// Typed identity of the complete NXFI bytes returned by the owner.
    #[must_use]
    pub const fn identity(&self) -> SemanticImageIdentity {
        self.identity
    }

    /// Exact complete byte length admitted by the first page.
    #[must_use]
    pub const fn total_length(&self) -> u64 {
        self.total_length
    }

    /// Next byte offset accepted by the durable sequential cursor.
    #[must_use]
    pub const fn next_offset(&self) -> u64 {
        self.next_offset
    }
}

pub(super) struct LocalSemanticImageFiles {
    root: PathBuf,
}

impl LocalSemanticImageFiles {
    pub(super) fn open(state_root: &Path) -> Result<Self, String> {
        // macOS exposes the temporary directory through /var -> /private/var.
        // Resolve trusted ancestors once; the image directory and every
        // member below it still reject symlinks at their own boundary.
        let state_root = fs::canonicalize(state_root).map_err(display_io)?;
        let root = state_root.join("images");
        create_private_directory(&root)?;
        Ok(Self { root })
    }

    /// Recovers every bounded target staging area after the caller acquires
    /// the store-wide state lock. Stages beyond the retention cap are evicted
    /// by age, then digest order, so a restart cannot inherit a permanent
    /// over-cap wedge.
    pub(super) fn recover_stages(&self) -> Result<(), String> {
        let mut targets = Vec::new();
        for entry in fs::read_dir(&self.root).map_err(display_io)? {
            let entry = entry.map_err(display_io)?;
            let path = entry.path();
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| "semantic-image target directory is not UTF-8".to_owned())?;
            if !is_hex_digest(&name) {
                return Err("semantic-image target directory name is malformed".to_owned());
            }
            let metadata = fs::symlink_metadata(&path).map_err(display_io)?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err("semantic-image target path is not a directory".to_owned());
            }
            targets.push(path);
            if targets.len() > MAX_IMAGE_TARGETS {
                return Err("semantic-image target recovery scan exceeds its bound".to_owned());
            }
        }
        for target in targets {
            let staging = target.join("staging");
            match fs::symlink_metadata(&staging) {
                Ok(metadata) => {
                    if !metadata.is_dir() || metadata.file_type().is_symlink() {
                        return Err("semantic-image staging path is not a directory".to_owned());
                    }
                    recover_stage_directory_on_open(&staging)?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(display_io(error)),
            }
        }
        Ok(())
    }

    /// Looks up an exact selected image key, then independently reopens and
    /// verifies the content object. The locator itself grants no authority.
    pub(super) fn find_generation(
        &self,
        target: &SemanticTargetKey,
        image: SemanticPlaneImageKey,
    ) -> Result<Option<MappedSemanticImage>, SemanticImageCacheError> {
        let target_root = self.target_root(target);
        create_private_directory(&target_root).map_err(SemanticImageCacheError::Storage)?;
        let generations = target_root.join("generations");
        create_private_directory(&generations).map_err(SemanticImageCacheError::Storage)?;
        let locator = generations.join(format!("{}.ref", image_key_hash(image)));
        let Some(identity) = read_locator_for_lookup(&locator, image)? else {
            return Ok(None);
        };
        self.open_identity_for_lookup(target, identity, image.semantic_generation())
            .map(Some)
    }

    /// Reuses bytes by exact content identity after a selected first page has
    /// supplied that identity. This supports reuse across manifest roots while
    /// never treating an old selection locator as proof of the new selection.
    pub(super) fn find_identity(
        &self,
        target: &SemanticTargetKey,
        identity: SemanticImageIdentity,
        generation: GenerationId,
    ) -> Result<Option<MappedSemanticImage>, SemanticImageCacheError> {
        let object = self
            .target_root(target)
            .join("objects")
            .join(format!("{}.image", hex(identity.as_ref())));
        match fs::symlink_metadata(&object) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => self
                .open_identity_for_lookup(target, identity, generation)
                .map(Some),
            Ok(_) => Err(SemanticImageCacheError::Storage(
                "local semantic image object path is not a regular file".to_owned(),
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(SemanticImageCacheError::Storage(display_io(error))),
        }
    }

    pub(super) fn bind_selected_key(
        &self,
        target: &SemanticTargetKey,
        image: SemanticPlaneImageKey,
        identity: SemanticImageIdentity,
        retained: &[(SemanticPlaneImageKey, SemanticImageIdentity)],
    ) -> Result<MappedSemanticImage, SemanticImageCacheError> {
        let mapped =
            self.open_identity_for_lookup(target, identity, image.semantic_generation())?;
        self.write_generation_locator(target, image, identity)
            .map_err(SemanticImageCacheError::Storage)?;
        let mut keep = retained.to_vec();
        keep.push((image, identity));
        self.prune(target, keep)
            .map_err(SemanticImageCacheError::Storage)?;
        Ok(mapped)
    }

    /// Removes one corrupt content-addressed object only after a fresh initial
    /// page from the selected owner has bound the same target, image, and
    /// authority stamp. A healthy object or a generation mismatch is preserved.
    pub(super) fn repair_corrupt_identity(
        &self,
        target: &SemanticTargetKey,
        image: SemanticPlaneImageKey,
        selected_stamp: crate::SelectedGenerationStamp,
        owner_page: &crate::SelectedSemanticImageChunk,
    ) -> Result<(), SemanticImageCacheError> {
        if owner_page.target != *target
            || owner_page.image != image
            || owner_page.selected_stamp != selected_stamp
            || owner_page.byte_range.start != 0
            || owner_page.byte_range.len != 1
            || owner_page.payload.len() != 1
            || owner_page.total_length == 0
            || owner_page.total_length > MAX_SEMANTIC_IMAGE_BYTES
        {
            return Err(SemanticImageCacheError::Storage(
                "semantic-image repair lacks an exact selected owner first-page witness".to_owned(),
            ));
        }

        let identity = owner_page.image_identity;
        match self.open_identity_for_lookup(target, identity, image.semantic_generation()) {
            Ok(_) => return Ok(()),
            Err(SemanticImageCacheError::SelectionMismatch { identity, detail }) => {
                return Err(SemanticImageCacheError::SelectionMismatch { identity, detail });
            }
            Err(SemanticImageCacheError::Storage(detail)) => {
                return Err(SemanticImageCacheError::Storage(detail));
            }
            Err(SemanticImageCacheError::CorruptContent { .. }) => {}
            Err(error @ SemanticImageCacheError::TransferRejected(_)) => return Err(error),
        }

        let target_root = self.target_root(target);
        create_private_directory(&target_root).map_err(SemanticImageCacheError::Storage)?;
        let generations = target_root.join("generations");
        create_private_directory(&generations).map_err(SemanticImageCacheError::Storage)?;
        let objects = target_root.join("objects");
        create_private_directory(&objects).map_err(SemanticImageCacheError::Storage)?;
        let locator = generations.join(format!("{}.ref", image_key_hash(image)));
        remove_optional_regular_file(&locator).map_err(SemanticImageCacheError::Storage)?;
        let object = objects.join(format!("{}.image", hex(identity.as_ref())));
        remove_optional_regular_file(&object).map_err(SemanticImageCacheError::Storage)?;
        Ok(())
    }

    /// Returns one resumable sequential transfer, if a checked cursor exists.
    pub(super) fn resume(
        &self,
        target: &SemanticTargetKey,
        image: SemanticPlaneImageKey,
        retained: &[SemanticPlaneImageKey],
    ) -> Result<Option<SemanticImageResume>, String> {
        let stage_dir = self.stage_directory(target)?;
        let mut protected = retained.to_vec();
        protected.push(image);
        recover_stage_directory(&stage_dir, &protected, None)?;
        let (partial, checkpoint) = self.stage_paths(&stage_dir, image);
        let Some(bytes) = read_optional_bounded(&checkpoint, CHECKPOINT_BYTES)? else {
            return Ok(None);
        };
        let resume = decode_checkpoint(&bytes)?;
        if resume.image != image {
            return Err("semantic-image stage belongs to another image key".to_owned());
        }
        ensure_regular_file(&partial)?;
        let metadata = fs::metadata(&partial).map_err(display_io)?;
        if metadata.len() != resume.total_length {
            return Err("semantic-image stage length differs from its checkpoint"
                .to_owned()
                .into());
        }
        Ok(Some(resume))
    }

    /// Discards one exact-key interrupted transfer after a newer selected
    /// owner page proves its saved identity or length no longer applies.
    pub(super) fn discard_stage(
        &self,
        target: &SemanticTargetKey,
        image: SemanticPlaneImageKey,
    ) -> Result<(), String> {
        let stage_dir = self.stage_directory(target)?;
        let (partial, checkpoint) = self.stage_paths(&stage_dir, image);
        remove_optional_regular_file(&partial)?;
        remove_optional_regular_file(&checkpoint)
    }

    /// Writes one page at the durable cursor or accepts an identical replay.
    /// Gaps, mixed identities, and conflicting duplicate bytes fail closed.
    pub(super) fn stage_page(
        &self,
        target: &SemanticTargetKey,
        image: SemanticPlaneImageKey,
        identity: SemanticImageIdentity,
        total_length: u64,
        byte_range: ByteRange,
        payload: &[u8],
        retained: &[SemanticPlaneImageKey],
    ) -> Result<SemanticImageResume, String> {
        let end = byte_range
            .start
            .checked_add(byte_range.len)
            .ok_or_else(|| "semantic-image page range overflow".to_owned())?;
        if total_length == 0
            || total_length > MAX_SEMANTIC_IMAGE_BYTES
            || byte_range.len == 0
            || byte_range.len > MAX_RANGE_BYTES as u64
            || end > total_length
            || usize::try_from(byte_range.len).ok() != Some(payload.len())
        {
            return Err("semantic-image page exceeds its checked bounds".to_owned());
        }

        let stage_dir = self.stage_directory(target)?;
        let mut protected = retained.to_vec();
        protected.push(image);
        recover_stage_directory(&stage_dir, &protected, None)?;
        let (partial, checkpoint) = self.stage_paths(&stage_dir, image);
        let prior_bytes = read_optional_bounded(&checkpoint, CHECKPOINT_BYTES)?;
        if prior_bytes.is_none() {
            if byte_range.start != 0 {
                return Err("semantic-image stage must begin at byte zero".to_owned());
            }
            // Reserve capacity only after proving this is a valid new transfer,
            // so a malformed nonzero first page cannot evict useful stages.
            recover_stage_directory(&stage_dir, &protected, Some(image))?;
        }
        let prior = prior_bytes
            .map(|bytes| decode_checkpoint(&bytes))
            .transpose()?;
        let new_stage = prior.is_none();
        let mut file = match prior {
            None => {
                if byte_range.start != 0 {
                    return Err("semantic-image stage must begin at byte zero".to_owned());
                }
                let mut file = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create_new(true)
                    .open(&partial)
                    .map_err(display_io)?;
                set_private_file(&file)?;
                file.set_len(total_length).map_err(display_io)?;
                file
            }
            Some(prior) => {
                if prior.image != image
                    || prior.identity != identity
                    || prior.total_length != total_length
                {
                    return Err("semantic-image page conflicts with its durable stage".to_owned());
                }
                ensure_regular_file(&partial)?;
                OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&partial)
                    .map_err(display_io)?
            }
        };

        let next_offset = match prior {
            Some(prior) if byte_range.start < prior.next_offset => {
                if end > prior.next_offset {
                    return Err("semantic-image replay overlaps the durable cursor".to_owned());
                }
                file.seek(SeekFrom::Start(byte_range.start))
                    .map_err(display_io)?;
                let mut existing = [0; MAX_RANGE_BYTES];
                file.read_exact(&mut existing[..payload.len()])
                    .map_err(display_io)?;
                if &existing[..payload.len()] != payload {
                    return Err("semantic-image replay differs from its durable bytes".to_owned());
                }
                return Ok(prior);
            }
            Some(prior) if byte_range.start > prior.next_offset => {
                return Err("semantic-image page skips the durable cursor".to_owned());
            }
            Some(_) | None => end,
        };
        file.seek(SeekFrom::Start(byte_range.start))
            .map_err(display_io)?;
        file.write_all(payload).map_err(display_io)?;
        file.sync_all().map_err(display_io)?;
        if new_stage {
            backend_platform::durable::sync_parent(&partial).map_err(display_io)?;
        }
        let resume = SemanticImageResume {
            image,
            identity,
            total_length,
            next_offset,
        };
        write_checkpoint(&checkpoint, &resume)?;
        Ok(resume)
    }

    /// Verifies and atomically promotes a complete staged image. The returned
    /// anonymous mapping remains a stable borrowed reader after the file rename.
    pub(super) fn finish(
        &self,
        target: &SemanticTargetKey,
        resume: SemanticImageResume,
        retained: &[(SemanticPlaneImageKey, SemanticImageIdentity)],
    ) -> Result<MappedSemanticImage, SemanticImageCacheError> {
        if resume.next_offset != resume.total_length {
            return Err("semantic-image stage is incomplete".to_owned().into());
        }
        let stage_dir = self.stage_directory(target)?;
        let mut protected = retained.iter().map(|(image, _)| *image).collect::<Vec<_>>();
        protected.push(resume.image);
        recover_stage_directory(&stage_dir, &protected, None)?;
        let (partial, checkpoint) = self.stage_paths(&stage_dir, resume.image);
        let checkpoint_bytes = read_optional_bounded(&checkpoint, CHECKPOINT_BYTES)?
            .ok_or_else(|| "semantic-image stage checkpoint is missing".to_owned())?;
        if decode_checkpoint(&checkpoint_bytes)? != resume {
            return Err("semantic-image stage checkpoint changed before admission"
                .to_owned()
                .into());
        }
        ensure_regular_file(&partial)?;
        let metadata = fs::metadata(&partial)
            .map_err(display_io)
            .map_err(SemanticImageCacheError::Storage)?;
        if metadata.len() != resume.total_length {
            return Err(SemanticImageCacheError::Storage(
                "semantic-image stage length differs from its checkpoint".to_owned(),
            ));
        }
        let target_root = self.target_root(target);
        let objects = target_root.join("objects");
        create_private_directory(&objects)?;
        let object = objects.join(format!("{}.image", hex(resume.identity.as_ref())));
        let mut keep = retained.to_vec();
        keep.push((resume.image, resume.identity));
        self.prune(target, keep.iter().copied())?;
        match fs::symlink_metadata(&object) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                // A pre-existing content object can be corrupted after an
                // earlier lookup. Preserve the completed stage and return a
                // typed cache error so the client can bind a fresh owner page,
                // invalidate this exact identity, and retry admission.
                let mapped = self.open_identity_for_lookup(
                    target,
                    resume.identity,
                    resume.image.semantic_generation(),
                )?;
                remove_file(&partial)?;
                remove_file(&checkpoint)?;
                self.write_generation_locator(target, resume.image, resume.identity)?;
                return Ok(mapped);
            }
            Ok(_) => {
                return Err(SemanticImageCacheError::Storage(
                    "local semantic image object path is not a regular file".to_owned(),
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(display_io(error).into()),
        }

        // The safe semantic loader copies directly into an anonymous mapping,
        // validates full grammar and identities, and avoids a full heap image.
        let mapped = match load_semantic_image_mmap(
            &partial,
            resume.identity,
            resume.image.semantic_generation(),
            MAX_SEMANTIC_IMAGE_BYTES as usize,
        ) {
            Ok(mapped) => mapped,
            Err(error) => {
                // I/O failures can be transient and should remain resumable.
                // Identity, generation, size, and grammar failures prove that
                // this completed transfer can never be admitted under its
                // current claims; discard it so a fresh owner page can restart.
                if !matches!(&error, MappedSemanticImageError::Io { .. }) {
                    remove_file(&partial)?;
                    remove_file(&checkpoint)?;
                }
                return Err(match error {
                    MappedSemanticImageError::Identity { .. }
                    | MappedSemanticImageError::Generation { .. }
                    | MappedSemanticImageError::Grammar(_)
                    | MappedSemanticImageError::EmptyFile
                    | MappedSemanticImageError::FileTooLarge { .. } => {
                        SemanticImageCacheError::TransferRejected(display_error(error))
                    }
                    MappedSemanticImageError::Io { .. }
                    | MappedSemanticImageError::FileLength { .. } => {
                        SemanticImageCacheError::Storage(display_error(error))
                    }
                });
            }
        };
        fs::rename(&partial, &object).map_err(display_io)?;
        backend_platform::durable::sync_parent(&object).map_err(display_io)?;
        self.write_generation_locator(target, resume.image, resume.identity)?;
        remove_file(&checkpoint)?;
        Ok(mapped)
    }

    /// Reopens an already admitted content object by its typed identity.
    pub(super) fn open_identity(
        &self,
        target: &SemanticTargetKey,
        identity: SemanticImageIdentity,
        generation: GenerationId,
    ) -> Result<MappedSemanticImage, String> {
        self.open_identity_for_lookup(target, identity, generation)
            .map_err(|error| error.to_string())
    }

    fn open_identity_for_lookup(
        &self,
        target: &SemanticTargetKey,
        identity: SemanticImageIdentity,
        generation: GenerationId,
    ) -> Result<MappedSemanticImage, SemanticImageCacheError> {
        let target_root = self.target_root(target);
        create_private_directory(&target_root).map_err(SemanticImageCacheError::Storage)?;
        let objects = target_root.join("objects");
        create_private_directory(&objects).map_err(SemanticImageCacheError::Storage)?;
        let object = objects.join(format!("{}.image", hex(identity.as_ref())));
        let metadata = match fs::symlink_metadata(&object) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => metadata,
            Ok(_) => {
                return Err(SemanticImageCacheError::Storage(
                    "local semantic image object path is not a regular file".to_owned(),
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(SemanticImageCacheError::CorruptContent {
                    identity: Some(identity),
                    detail: "content-addressed semantic image is missing".to_owned(),
                });
            }
            Err(error) => {
                return Err(SemanticImageCacheError::Storage(display_io(error)));
            }
        };
        let length = metadata.len();
        if length == 0 || length > MAX_SEMANTIC_IMAGE_BYTES {
            return Err(SemanticImageCacheError::CorruptContent {
                identity: Some(identity),
                detail: "content-addressed semantic image has an invalid length".to_owned(),
            });
        }
        load_semantic_image_mmap(
            &object,
            identity,
            generation,
            MAX_SEMANTIC_IMAGE_BYTES as usize,
        )
        .map_err(|error| {
            let detail = display_error(&error);
            match &error {
                MappedSemanticImageError::Identity { .. }
                | MappedSemanticImageError::Grammar(_)
                | MappedSemanticImageError::EmptyFile
                | MappedSemanticImageError::FileTooLarge { .. } => {
                    SemanticImageCacheError::CorruptContent {
                        identity: Some(identity),
                        detail,
                    }
                }
                MappedSemanticImageError::Generation { .. } => {
                    SemanticImageCacheError::SelectionMismatch { identity, detail }
                }
                MappedSemanticImageError::Io { .. }
                | MappedSemanticImageError::FileLength { .. } => {
                    SemanticImageCacheError::Storage(detail)
                }
            }
        })
    }

    /// Removes unreferenced content images after the caller has selected its
    /// retained generation identities. The scan rejects over-bound stores first.
    pub(super) fn prune(
        &self,
        target: &SemanticTargetKey,
        keep: impl IntoIterator<Item = (SemanticPlaneImageKey, SemanticImageIdentity)>,
    ) -> Result<(), String> {
        let target_root = self.target_root(target);
        let objects = target_root.join("objects");
        let metadata = match fs::symlink_metadata(&objects) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(display_io(error)),
        };
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err("local semantic image object path is not a directory".to_owned());
        }
        let keep = keep.into_iter().collect::<std::collections::BTreeSet<_>>();
        let keep_identities = keep
            .iter()
            .map(|(_, identity)| hex(identity.as_ref()))
            .collect::<std::collections::BTreeSet<_>>();
        let mut entries = Vec::new();
        for entry in fs::read_dir(&objects).map_err(display_io)? {
            let entry = entry.map_err(display_io)?;
            let path = entry.path();
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| "local semantic image filename is not UTF-8".to_owned())?;
            ensure_regular_file(&path)?;
            let stem = name.strip_suffix(".image").ok_or_else(|| {
                "local semantic image directory contains an unknown member".to_owned()
            })?;
            if !is_hex_digest(stem) {
                return Err("local semantic image filename is malformed".to_owned());
            }
            entries.push((path, stem.to_owned()));
            if entries.len() > MAX_IMAGE_OBJECT_FILES {
                return Err("local semantic image recovery scan exceeds its bound".to_owned());
            }
        }
        let generations = target_root.join("generations");
        let mut locators = Vec::new();
        match fs::symlink_metadata(&generations) {
            Ok(metadata) => {
                if !metadata.is_dir() || metadata.file_type().is_symlink() {
                    return Err("local semantic image locator path is not a directory".to_owned());
                }
                for entry in fs::read_dir(&generations).map_err(display_io)? {
                    let entry = entry.map_err(display_io)?;
                    let path = entry.path();
                    let name = entry
                        .file_name()
                        .into_string()
                        .map_err(|_| "semantic-image locator filename is not UTF-8".to_owned())?;
                    let stem = name.strip_suffix(".ref").ok_or_else(|| {
                        "semantic-image locator directory contains an unknown member".to_owned()
                    })?;
                    if !is_hex_digest(stem) {
                        return Err("semantic-image locator filename is malformed".to_owned());
                    }
                    ensure_regular_file(&path)?;
                    let (image, identity) = read_locator_any(&path)?.ok_or_else(|| {
                        "semantic-image locator disappeared during scan".to_owned()
                    })?;
                    if image_key_hash(image) != stem {
                        return Err(
                            "semantic-image locator filename differs from its image key".to_owned()
                        );
                    }
                    locators.push((path, image, identity));
                    if locators.len() > MAX_IMAGE_OBJECT_FILES {
                        return Err("semantic-image locator scan exceeds its bound".to_owned());
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(display_io(error)),
        }
        for (path, stem) in entries {
            if !keep_identities.contains(&stem) {
                remove_file(&path)?;
            }
        }
        for (path, image, identity) in locators {
            if !keep.contains(&(image, identity)) {
                remove_file(&path)?;
            }
        }
        Ok(())
    }

    fn write_generation_locator(
        &self,
        target: &SemanticTargetKey,
        image: SemanticPlaneImageKey,
        identity: SemanticImageIdentity,
    ) -> Result<(), String> {
        let target_root = self.target_root(target);
        create_private_directory(&target_root)?;
        let directory = target_root.join("generations");
        create_private_directory(&directory)?;
        let mut bytes = Vec::with_capacity(4 + 4 + 32 + 32 + 32 + CHECKSUM_BYTES);
        bytes.extend_from_slice(b"SIGR");
        bytes.extend_from_slice(&image.artifact_ordinal().to_be_bytes());
        bytes.extend_from_slice(image.semantic_generation().as_bytes());
        bytes.extend_from_slice(image.manifest_root().as_bytes());
        bytes.extend_from_slice(identity.as_ref());
        let checksum = blake3::hash(&bytes);
        bytes.extend_from_slice(checksum.as_bytes());
        let path = directory.join(format!("{}.ref", image_key_hash(image)));
        backend_platform::durable::write_private_atomic(&path, &bytes).map_err(display_io)
    }

    fn target_root(&self, target: &SemanticTargetKey) -> PathBuf {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"backend.semantic.local-generation-target.v1\0");
        hasher.update(&(target.package().len() as u64).to_be_bytes());
        hasher.update(target.package().as_bytes());
        hasher.update(&(target.coordinate().len() as u64).to_be_bytes());
        hasher.update(target.coordinate().as_bytes());
        hasher.update(&<[u8; 2]>::from(target.profile()));
        self.root.join(hex(hasher.finalize().as_bytes()))
    }

    fn stage_directory(&self, target: &SemanticTargetKey) -> Result<PathBuf, String> {
        let target_root = self.target_root(target);
        create_private_directory(&target_root)?;
        let directory = target_root.join("staging");
        create_private_directory(&directory)?;
        Ok(directory)
    }

    fn stage_paths(&self, directory: &Path, image: SemanticPlaneImageKey) -> (PathBuf, PathBuf) {
        let key = stage_key_hash(image);
        (
            directory.join(format!("{key}.partial")),
            directory.join(format!("{key}.resume")),
        )
    }
}

struct RecoveredStage {
    stem: String,
    image: SemanticPlaneImageKey,
    partial: PathBuf,
    checkpoint: PathBuf,
    modified: SystemTime,
}

fn recover_stage_directory(
    directory: &Path,
    protected: &[SemanticPlaneImageKey],
    reserve_candidate: Option<SemanticPlaneImageKey>,
) -> Result<(), String> {
    recover_stage_directory_with_policy(directory, protected, reserve_candidate, false)
}

/// A malformed durable cursor should not prevent the whole local store from
/// reopening. Startup recovery discards only that regular-file pair; direct
/// resume/stage operations continue using strict recovery and report the same
/// malformed cursor to their caller.
fn recover_stage_directory_on_open(directory: &Path) -> Result<(), String> {
    recover_stage_directory_with_policy(directory, &[], None, true)
}

fn recover_stage_directory_with_policy(
    directory: &Path,
    protected: &[SemanticPlaneImageKey],
    reserve_candidate: Option<SemanticPlaneImageKey>,
    discard_corrupt_pairs: bool,
) -> Result<(), String> {
    let mut members = BTreeMap::<String, (Option<PathBuf>, Option<PathBuf>)>::new();
    for entry in fs::read_dir(directory).map_err(display_io)? {
        let entry = entry.map_err(display_io)?;
        let path = entry.path();
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "semantic-image staging filename is not UTF-8".to_owned())?;
        let (stem, is_partial) = if let Some(stem) = name.strip_suffix(".partial") {
            (stem, true)
        } else if let Some(stem) = name.strip_suffix(".resume") {
            (stem, false)
        } else {
            return Err("semantic-image staging directory contains an unknown member".to_owned());
        };
        if !is_hex_digest(stem) {
            return Err("semantic-image stage filename is malformed".to_owned());
        }
        ensure_regular_file(&path)?;
        let member = members.entry(stem.to_owned()).or_default();
        if is_partial {
            if member.0.replace(path).is_some() {
                return Err("duplicate semantic-image stage file".to_owned());
            }
        } else if member.1.replace(path).is_some() {
            return Err("duplicate semantic-image resume record".to_owned());
        }
        if members.len() > MAX_IMAGE_STAGE_SCAN || members.len() * 2 > MAX_IMAGE_MEMBERS {
            return Err("semantic-image staging scan exceeds its bound".to_owned());
        }
    }

    let mut stages = Vec::new();
    for (stem, (partial, checkpoint)) in members {
        let (partial, checkpoint) = match (partial, checkpoint) {
            (Some(partial), Some(checkpoint)) => (partial, checkpoint),
            (partial, checkpoint) => {
                if let Some(path) = partial {
                    remove_file(&path)?;
                }
                if let Some(path) = checkpoint {
                    remove_file(&path)?;
                }
                continue;
            }
        };
        let checkpoint_metadata = fs::symlink_metadata(&checkpoint).map_err(display_io)?;
        if !checkpoint_metadata.is_file() || checkpoint_metadata.file_type().is_symlink() {
            return Err("semantic-image member is not a regular file".to_owned());
        }
        if checkpoint_metadata.len() > u64::try_from(CHECKPOINT_BYTES).unwrap_or(u64::MAX) {
            if discard_corrupt_pairs {
                discard_stage_pair(&partial, &checkpoint)?;
                continue;
            }
            return Err("semantic-image metadata member is not a bounded regular file".to_owned());
        }
        let mut bytes = Vec::with_capacity(CHECKPOINT_BYTES);
        File::open(&checkpoint)
            .map_err(display_io)?
            .take(u64::try_from(CHECKPOINT_BYTES.saturating_add(1)).unwrap_or(u64::MAX))
            .read_to_end(&mut bytes)
            .map_err(display_io)?;
        if bytes.len() > CHECKPOINT_BYTES {
            if discard_corrupt_pairs {
                discard_stage_pair(&partial, &checkpoint)?;
                continue;
            }
            return Err("semantic-image metadata member is not a bounded regular file".to_owned());
        }
        let resume = match decode_checkpoint(&bytes) {
            Ok(resume) => resume,
            Err(_) if discard_corrupt_pairs => {
                discard_stage_pair(&partial, &checkpoint)?;
                continue;
            }
            Err(error) => return Err(error),
        };
        if stage_key_hash(resume.image) != stem {
            if discard_corrupt_pairs {
                discard_stage_pair(&partial, &checkpoint)?;
                continue;
            }
            return Err("semantic-image stage filename differs from its image key".to_owned());
        }
        let metadata = fs::metadata(&partial).map_err(display_io)?;
        if metadata.len() != resume.total_length {
            if discard_corrupt_pairs {
                discard_stage_pair(&partial, &checkpoint)?;
                continue;
            }
            return Err("semantic-image stage length differs from its checkpoint".to_owned());
        }
        let modified = checkpoint_metadata.modified().map_err(display_io)?;
        stages.push(RecoveredStage {
            stem,
            image: resume.image,
            partial,
            checkpoint,
            modified,
        });
    }

    stages.sort_by(|left, right| {
        left.modified
            .cmp(&right.modified)
            .then_with(|| left.stem.cmp(&right.stem))
    });
    let protected = protected.iter().copied().collect::<BTreeSet<_>>();
    let candidate_needs_slot = reserve_candidate
        .is_some_and(|candidate| !stages.iter().any(|stage| stage.image == candidate));
    let required_slots = usize::from(candidate_needs_slot);
    while stages.len().saturating_add(required_slots) > MAX_IMAGE_STAGES {
        let Some(index) = stages.iter().position(|stage| {
            !protected.contains(&stage.image)
                && reserve_candidate.is_none_or(|candidate| candidate != stage.image)
        }) else {
            return Err("semantic-image staging retention set exceeds its bound".to_owned());
        };
        let stage = stages.remove(index);
        remove_file(&stage.partial)?;
        remove_file(&stage.checkpoint)?;
    }
    Ok(())
}

fn discard_stage_pair(partial: &Path, checkpoint: &Path) -> Result<(), String> {
    // Both paths were scanned and checked as regular files before reaching
    // this helper. Remove the data first so a crash leaves at most a harmless
    // checkpoint-only orphan for the next bounded recovery pass.
    remove_file(partial)?;
    remove_file(checkpoint)
}

fn encode_checkpoint(resume: SemanticImageResume) -> [u8; CHECKPOINT_BYTES] {
    let mut bytes = [0; CHECKPOINT_BYTES];
    let mut offset = 0;
    copy_into(&mut bytes, &mut offset, &CHECKPOINT_MAGIC);
    copy_into(&mut bytes, &mut offset, &[CHECKPOINT_VERSION]);
    copy_into(
        &mut bytes,
        &mut offset,
        &resume.image.artifact_ordinal().to_be_bytes(),
    );
    copy_into(
        &mut bytes,
        &mut offset,
        resume.image.semantic_generation().as_bytes(),
    );
    copy_into(
        &mut bytes,
        &mut offset,
        resume.image.manifest_root().as_bytes(),
    );
    copy_into(&mut bytes, &mut offset, resume.identity.as_ref());
    copy_into(&mut bytes, &mut offset, &resume.total_length.to_be_bytes());
    copy_into(&mut bytes, &mut offset, &resume.next_offset.to_be_bytes());
    let checksum = blake3::hash(&bytes[..offset]);
    copy_into(&mut bytes, &mut offset, checksum.as_bytes());
    bytes
}

fn decode_checkpoint(bytes: &[u8]) -> Result<SemanticImageResume, String> {
    if bytes.len() != CHECKPOINT_BYTES
        || bytes.get(..4) != Some(CHECKPOINT_MAGIC.as_slice())
        || bytes[4] != CHECKPOINT_VERSION
    {
        return Err("semantic-image resume record header is invalid".to_owned());
    }
    let checksum_offset = CHECKPOINT_BYTES - CHECKSUM_BYTES;
    let expected = blake3::hash(&bytes[..checksum_offset]);
    if expected.as_bytes() != &bytes[checksum_offset..] {
        return Err("semantic-image resume checksum is invalid".to_owned());
    }
    let ordinal = u32::from_be_bytes(bytes[5..9].try_into().map_err(display_error)?);
    let generation = GenerationId::from_raw(bytes[9..41].try_into().map_err(display_error)?);
    let root = backend_semantic::ir::SemanticManifestRoot::from_wire_claim(
        bytes[41..73].try_into().map_err(display_error)?,
    );
    let identity = SemanticImageIdentity::try_from(&bytes[73..105]).map_err(display_error)?;
    let total_length = u64::from_be_bytes(bytes[105..113].try_into().map_err(display_error)?);
    let next_offset = u64::from_be_bytes(bytes[113..121].try_into().map_err(display_error)?);
    if total_length == 0
        || total_length > MAX_SEMANTIC_IMAGE_BYTES
        || next_offset == 0
        || next_offset > total_length
    {
        return Err("semantic-image resume cursor is outside its bounds".to_owned());
    }
    Ok(SemanticImageResume {
        image: SemanticPlaneImageKey::new(ordinal, generation, root),
        identity,
        total_length,
        next_offset,
    })
}

fn read_locator(
    path: &Path,
    expected_image: SemanticPlaneImageKey,
) -> Result<Option<SemanticImageIdentity>, String> {
    let Some((image, identity)) = read_locator_any_optional(path)? else {
        return Ok(None);
    };
    if image != expected_image {
        return Err("semantic-image locator key differs from its selected image".to_owned());
    }
    Ok(Some(identity))
}

fn read_locator_for_lookup(
    path: &Path,
    expected_image: SemanticPlaneImageKey,
) -> Result<Option<SemanticImageIdentity>, SemanticImageCacheError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(SemanticImageCacheError::Storage(display_io(error))),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(SemanticImageCacheError::Storage(
            "semantic-image locator path is not a regular file".to_owned(),
        ));
    }
    if metadata.len() > u64::try_from(LOCATOR_BYTES).unwrap_or(u64::MAX) {
        return Err(SemanticImageCacheError::CorruptContent {
            identity: None,
            detail: "semantic-image locator exceeds its bound".to_owned(),
        });
    }
    let bytes =
        fs::read(path).map_err(|error| SemanticImageCacheError::Storage(display_io(error)))?;
    let corrupt = |detail: &str| SemanticImageCacheError::CorruptContent {
        identity: None,
        detail: detail.to_owned(),
    };
    if bytes.len() != LOCATOR_BYTES || &bytes[..4] != b"SIGR" {
        return Err(corrupt("semantic-image locator header is invalid"));
    }
    let checksum_offset = bytes.len() - CHECKSUM_BYTES;
    if blake3::hash(&bytes[..checksum_offset]).as_bytes() != &bytes[checksum_offset..] {
        return Err(corrupt("semantic-image locator checksum is invalid"));
    }
    let ordinal = u32::from_be_bytes(
        bytes[4..8]
            .try_into()
            .map_err(|_| corrupt("semantic-image locator ordinal is invalid"))?,
    );
    let generation = GenerationId::from_raw(
        bytes[8..40]
            .try_into()
            .map_err(|_| corrupt("semantic-image locator generation is invalid"))?,
    );
    let root = backend_semantic::ir::SemanticManifestRoot::from_wire_claim(
        bytes[40..72]
            .try_into()
            .map_err(|_| corrupt("semantic-image locator root is invalid"))?,
    );
    let identity = SemanticImageIdentity::try_from(&bytes[72..104])
        .map_err(|_| corrupt("semantic-image locator content identity is invalid"))?;
    if SemanticPlaneImageKey::new(ordinal, generation, root) != expected_image {
        return Err(corrupt(
            "semantic-image locator differs from the selected image key",
        ));
    }
    Ok(Some(identity))
}

fn read_locator_any(
    path: &Path,
) -> Result<Option<(SemanticPlaneImageKey, SemanticImageIdentity)>, String> {
    let Some((image, identity)) = read_locator_any_optional(path)? else {
        return Ok(None);
    };
    Ok(Some((image, identity)))
}

fn read_locator_any_optional(
    path: &Path,
) -> Result<Option<(SemanticPlaneImageKey, SemanticImageIdentity)>, String> {
    const LOCATOR_BYTES: usize = 4 + 4 + 32 + 32 + 32 + CHECKSUM_BYTES;
    let Some(bytes) = read_optional_bounded(path, LOCATOR_BYTES)? else {
        return Ok(None);
    };
    if bytes.len() != LOCATOR_BYTES || &bytes[..4] != b"SIGR" {
        return Err("semantic-image locator header is invalid".to_owned());
    }
    let checksum_offset = bytes.len() - CHECKSUM_BYTES;
    if blake3::hash(&bytes[..checksum_offset]).as_bytes() != &bytes[checksum_offset..] {
        return Err("semantic-image locator checksum is invalid".to_owned());
    }
    let ordinal = u32::from_be_bytes(bytes[4..8].try_into().map_err(display_error)?);
    let generation = GenerationId::from_raw(bytes[8..40].try_into().map_err(display_error)?);
    let root = backend_semantic::ir::SemanticManifestRoot::from_wire_claim(
        bytes[40..72].try_into().map_err(display_error)?,
    );
    let identity = SemanticImageIdentity::try_from(&bytes[72..104]).map_err(display_error)?;
    Ok(Some((
        SemanticPlaneImageKey::new(ordinal, generation, root),
        identity,
    )))
}

fn write_checkpoint(path: &Path, resume: &SemanticImageResume) -> Result<(), String> {
    backend_platform::durable::write_private_atomic(path, &encode_checkpoint(*resume))
        .map_err(display_io)
}

fn read_optional_bounded(path: &Path, max: usize) -> Result<Option<Vec<u8>>, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(display_io(error)),
    };
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > u64::try_from(max).map_err(display_error)?
    {
        return Err("semantic-image metadata member is not a bounded regular file".to_owned());
    }
    fs::read(path).map(Some).map_err(display_io)
}

fn create_private_directory(path: &Path) -> Result<(), String> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if !metadata.is_dir() || metadata.file_type().is_symlink() {
                    return Err("semantic-image storage path is not a directory".to_owned());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&current).map_err(display_io)?;
                let metadata = fs::symlink_metadata(&current).map_err(display_io)?;
                if !metadata.is_dir() || metadata.file_type().is_symlink() {
                    return Err("semantic-image storage path is not a directory".to_owned());
                }
            }
            Err(error) => return Err(display_io(error)),
        }
    }
    let metadata = fs::symlink_metadata(path).map_err(display_io)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut permissions = metadata.permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(path, permissions).map_err(display_io)?;
    }
    Ok(())
}

fn ensure_regular_file(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(display_io)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("semantic-image member is not a regular file".to_owned());
    }
    Ok(())
}

fn set_private_file(file: &File) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut permissions = file.metadata().map_err(display_io)?.permissions();
        permissions.set_mode(0o600);
        file.set_permissions(permissions).map_err(display_io)?;
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

fn remove_optional_regular_file(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            remove_file(path)
        }
        Ok(_) => Err("semantic-image repair target is not a regular file".to_owned()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(display_io(error)),
    }
}

fn copy_into<const N: usize>(destination: &mut [u8; N], offset: &mut usize, bytes: &[u8]) {
    let end = *offset + bytes.len();
    destination[*offset..end].copy_from_slice(bytes);
    *offset = end;
}

fn is_hex_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn image_key_hash(image: SemanticPlaneImageKey) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.semantic.local-image-locator.v1\0");
    hasher.update(&image.artifact_ordinal().to_be_bytes());
    hasher.update(image.semantic_generation().as_bytes());
    hasher.update(image.manifest_root().as_bytes());
    hasher.finalize().to_hex().to_string()
}

fn stage_key_hash(image: SemanticPlaneImageKey) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.semantic.local-image-stage.v1\0");
    hasher.update(&image.artifact_ordinal().to_be_bytes());
    hasher.update(image.semantic_generation().as_bytes());
    hasher.update(image.manifest_root().as_bytes());
    hasher.finalize().to_hex().to_string()
}

fn display_io(error: std::io::Error) -> String {
    error.to_string()
}

fn display_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_semantic::vocabulary::{LanguageProfile, RustEdition};
    use backend_store::FileStore;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn create() -> Self {
            static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(1);
            let path = std::env::temp_dir().join(format!(
                "backend-semantic-image-store-{}-{}",
                std::process::id(),
                NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed),
            ));
            fs::create_dir_all(&path).expect("create test storage root");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn fixture() -> (
        SemanticTargetKey,
        SemanticPlaneImageKey,
        SemanticImageIdentity,
    ) {
        let target = SemanticTargetKey::new(
            "pkg:cargo/image-test@1.0.0",
            "pkg:cargo/image-test@1.0.0",
            LanguageProfile::Rust(RustEdition::Rust2024),
        )
        .expect("target");
        let image = SemanticPlaneImageKey::new(
            0,
            GenerationId::from_raw([3; 32]),
            backend_semantic::ir::SemanticManifestRoot::from_wire_claim([4; 32]),
        );
        let identity = SemanticImageIdentity::from_encoded_bytes(b"not an NXFI image");
        (target, image, identity)
    }

    fn canonical_image_bytes() -> Vec<u8> {
        let image = backend_semantic::ir::IrBuilder::new()
            .finish()
            .expect("empty canonical IR builds");
        let length =
            backend_semantic::ir::full_semantic_image_len(&image).expect("canonical image length");
        let mut bytes = vec![0; length];
        backend_semantic::ir::encode_full_semantic_image(&image, &mut bytes)
            .expect("canonical image encodes");
        bytes
    }

    fn image_key(index: u8) -> SemanticPlaneImageKey {
        SemanticPlaneImageKey::new(
            u32::from(index),
            GenerationId::from_raw([index; 32]),
            backend_semantic::ir::SemanticManifestRoot::from_wire_claim(
                [index.wrapping_add(1); 32],
            ),
        )
    }

    fn stage_count(directory: &Path) -> usize {
        fs::read_dir(directory)
            .expect("list stages")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".resume"))
            .count()
    }

    fn write_interrupted_stage(
        files: &LocalSemanticImageFiles,
        target: &SemanticTargetKey,
        image: SemanticPlaneImageKey,
        identity: SemanticImageIdentity,
        marker: u8,
    ) {
        let stage_dir = files.stage_directory(target).expect("stage directory");
        let (partial, checkpoint) = files.stage_paths(&stage_dir, image);
        let mut file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&partial)
            .expect("create interrupted partial");
        set_private_file(&file).expect("protect partial");
        file.set_len(4).expect("reserve interrupted transfer");
        file.seek(SeekFrom::Start(0)).expect("seek partial");
        file.write_all(&[marker]).expect("write interrupted byte");
        file.sync_all().expect("sync interrupted byte");
        backend_platform::durable::sync_parent(&partial).expect("sync stage directory");
        write_checkpoint(
            &checkpoint,
            &SemanticImageResume {
                image,
                identity,
                total_length: 4,
                next_offset: 1,
            },
        )
        .expect("write interrupted checkpoint");
    }

    #[test]
    fn locators_distinguish_artifacts_with_one_vcs_generation() {
        let directory = TestDirectory::create();
        let (target, first_image, first_identity) = fixture();
        let second_image = SemanticPlaneImageKey::new(
            1,
            first_image.semantic_generation(),
            backend_semantic::ir::SemanticManifestRoot::from_wire_claim([5; 32]),
        );
        let second_identity = SemanticImageIdentity::from_encoded_bytes(b"another invalid image");
        let files = LocalSemanticImageFiles::open(&directory.0).expect("open image files");
        files
            .write_generation_locator(&target, first_image, first_identity)
            .expect("write first image-key locator");
        files
            .write_generation_locator(&target, second_image, second_identity)
            .expect("write second image-key locator");

        let directory = files.target_root(&target).join("generations");
        let first_path = directory.join(format!("{}.ref", image_key_hash(first_image)));
        let second_path = directory.join(format!("{}.ref", image_key_hash(second_image)));
        assert_ne!(first_path, second_path);
        assert_eq!(
            read_locator(&first_path, first_image).expect("first locator"),
            Some(first_identity)
        );
        assert_eq!(
            read_locator(&second_path, second_image).expect("second locator"),
            Some(second_identity)
        );
        assert!(
            read_locator(&first_path, second_image)
                .expect_err("a locator cannot be rebound to another image key")
                .contains("differs")
        );

        let objects = files.target_root(&target).join("objects");
        create_private_directory(&objects).expect("create object directory");
        let first_object = objects.join(format!("{}.image", hex(first_identity.as_ref())));
        let second_object = objects.join(format!("{}.image", hex(second_identity.as_ref())));
        fs::write(&first_object, b"first").expect("create first object");
        fs::write(&second_object, b"second").expect("create second object");
        files
            .prune(&target, [(second_image, second_identity)])
            .expect("prune unretained image identity");
        assert!(!first_path.exists());
        assert!(second_path.exists());
        assert!(!first_object.exists());
        assert!(second_object.exists());
    }

    #[test]
    fn staged_pages_cold_reopen_and_identical_replay_preserve_cursor() {
        let directory = TestDirectory::create();
        let (target, image, identity) = fixture();
        let files = LocalSemanticImageFiles::open(&directory.0).expect("open image files");
        let first = files
            .stage_page(
                &target,
                image,
                identity,
                4,
                ByteRange::new(0, 2).expect("first range"),
                b"ab",
                &[],
            )
            .expect("stage first page");
        assert_eq!(first.next_offset(), 2);
        drop(files);

        let reopened = LocalSemanticImageFiles::open(&directory.0).expect("cold reopen");
        assert_eq!(
            reopened.resume(&target, image, &[]).expect("read cursor"),
            Some(first)
        );
        assert_eq!(
            reopened
                .stage_page(
                    &target,
                    image,
                    identity,
                    4,
                    ByteRange::new(0, 2).expect("replayed range"),
                    b"ab",
                    &[],
                )
                .expect("identical replay"),
            first
        );
        assert!(
            reopened
                .stage_page(
                    &target,
                    image,
                    identity,
                    4,
                    ByteRange::new(0, 2).expect("conflicting range"),
                    b"zz",
                    &[],
                )
                .expect_err("conflicting duplicate fails closed")
                .contains("differs")
        );
        assert!(
            reopened
                .stage_page(
                    &target,
                    image,
                    identity,
                    4,
                    ByteRange::new(3, 1).expect("gapped range"),
                    b"d",
                    &[],
                )
                .expect_err("gap fails closed")
                .contains("skips")
        );
    }

    #[test]
    fn crash_between_page_sync_and_cursor_commit_discards_only_orphan_stage() {
        let directory = TestDirectory::create();
        let (target, image, identity) = fixture();
        let files = LocalSemanticImageFiles::open(&directory.0).expect("open image files");
        let _resume = files
            .stage_page(
                &target,
                image,
                identity,
                4,
                ByteRange::new(0, 2).expect("first range"),
                b"ab",
                &[],
            )
            .expect("stage first page");
        let stage_dir = files.stage_directory(&target).expect("stage directory");
        let (_, checkpoint) = files.stage_paths(&stage_dir, image);
        fs::remove_file(checkpoint).expect("simulate stop before cursor publication");

        assert_eq!(
            files.resume(&target, image, &[]).expect("bounded recovery"),
            None
        );
        let member_count = fs::read_dir(stage_dir)
            .expect("list recovered stage directory")
            .count();
        assert_eq!(member_count, 0);
    }

    #[test]
    fn corrupt_resume_checksum_is_rejected_without_advancing_stage() {
        let directory = TestDirectory::create();
        let (target, image, identity) = fixture();
        let files = LocalSemanticImageFiles::open(&directory.0).expect("open image files");
        let _resume = files
            .stage_page(
                &target,
                image,
                identity,
                4,
                ByteRange::new(0, 2).expect("first range"),
                b"ab",
                &[],
            )
            .expect("stage first page");
        let stage_dir = files.stage_directory(&target).expect("stage directory");
        let (_, checkpoint) = files.stage_paths(&stage_dir, image);
        let mut bytes = fs::read(&checkpoint).expect("read resume record");
        bytes[8] ^= 0x80;
        fs::write(&checkpoint, bytes).expect("corrupt resume record");

        assert!(
            files
                .resume(&target, image, &[])
                .expect_err("corrupt resume checksum fails closed")
                .contains("checksum")
        );
    }

    #[test]
    fn cold_store_reopen_discards_corrupt_stage_and_preserves_unrelated_resume() {
        let directory = TestDirectory::create();
        let cas_root = directory.0.join("cas");
        let state_root = cas_root.join("semantic-hydration");
        let (target, corrupt_image, corrupt_identity) = fixture();
        let valid_image = image_key(11);
        let valid_identity = SemanticImageIdentity::from_encoded_bytes(b"valid stage");
        {
            let cas = FileStore::open(&cas_root, 16 * 1024 * 1024).expect("open FileStore");
            let range_store = crate::FileSemanticRangeStore::open(
                cas,
                crate::TransportLimits {
                    max_frame: 32 * 1024,
                    max_chunk: crate::MAX_RANGE_BYTES,
                    ..crate::TransportLimits::default()
                },
            )
            .expect("initial semantic store open");
            let files = LocalSemanticImageFiles::open(&state_root).expect("open image files");
            write_interrupted_stage(&files, &target, corrupt_image, corrupt_identity, 1);
            write_interrupted_stage(&files, &target, valid_image, valid_identity, 2);
            let stage_dir = files.stage_directory(&target).expect("stage directory");
            let (_, corrupt_checkpoint) = files.stage_paths(&stage_dir, corrupt_image);
            let mut checkpoint = fs::read(&corrupt_checkpoint).expect("read corrupt-stage cursor");
            checkpoint[8] ^= 0x80;
            fs::write(&corrupt_checkpoint, checkpoint).expect("corrupt one cursor checksum");
            drop(files);
            drop(range_store);
        }

        let cas = FileStore::open(&cas_root, 16 * 1024 * 1024).expect("cold-open FileStore");
        let reopened = crate::FileSemanticRangeStore::open(
            cas,
            crate::TransportLimits {
                max_frame: 32 * 1024,
                max_chunk: crate::MAX_RANGE_BYTES,
                ..crate::TransportLimits::default()
            },
        )
        .expect("corrupt checkpoint does not wedge store startup");
        let valid = reopened
            .resume_semantic_image_transfer(&target, valid_image)
            .expect("valid unrelated stage remains readable")
            .expect("valid unrelated stage was preserved");
        assert_eq!(valid.identity(), valid_identity);
        assert_eq!(valid.next_offset(), 1);
        assert_eq!(
            reopened
                .resume_semantic_image_transfer(&target, corrupt_image)
                .expect("discarded corrupt stage is absent"),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn startup_recovery_still_fails_closed_on_symlink_stage_member() {
        use std::os::unix::fs::symlink;

        let directory = TestDirectory::create();
        let (target, valid_image, valid_identity) = fixture();
        let linked_image = image_key(12);
        let linked_identity = SemanticImageIdentity::from_encoded_bytes(b"linked stage");
        let files = LocalSemanticImageFiles::open(&directory.0).expect("open image files");
        write_interrupted_stage(&files, &target, valid_image, valid_identity, 1);
        write_interrupted_stage(&files, &target, linked_image, linked_identity, 2);
        let stage_dir = files.stage_directory(&target).expect("stage directory");
        let (linked_partial, linked_checkpoint) = files.stage_paths(&stage_dir, linked_image);
        fs::remove_file(&linked_checkpoint).expect("remove regular checkpoint");
        let sentinel = directory.0.join("sentinel");
        fs::write(&sentinel, b"external").expect("write symlink target");
        symlink(&sentinel, &linked_checkpoint).expect("replace checkpoint with symlink");

        assert!(
            files
                .recover_stages()
                .expect_err("symlink member fails closed")
                .contains("regular file")
        );
        let (valid_partial, valid_checkpoint) = files.stage_paths(&stage_dir, valid_image);
        assert!(valid_partial.exists() && valid_checkpoint.exists());
        assert!(linked_partial.exists() && linked_checkpoint.is_symlink());
        assert_eq!(fs::read(&sentinel).expect("sentinel remains"), b"external");
    }

    #[test]
    fn cold_reopen_evicts_over_cap_interrupted_stages_and_preserves_active_keys() {
        let directory = TestDirectory::create();
        let (target, _, _) = fixture();
        let files = LocalSemanticImageFiles::open(&directory.0).expect("open image files");
        let mut keys = Vec::new();
        for index in 1..=65_u8 {
            let image = image_key(index);
            let identity = SemanticImageIdentity::from_encoded_bytes(&[index]);
            write_interrupted_stage(&files, &target, image, identity, index);
            keys.push((image, identity));
        }
        drop(files);

        let reopened = LocalSemanticImageFiles::open(&directory.0).expect("cold reopen");
        reopened.recover_stages().expect("bounded cold recovery");
        let stage_dir = reopened.stage_directory(&target).expect("stage directory");
        assert_eq!(stage_count(&stage_dir), MAX_IMAGE_STAGES);

        // Target-scoped recovery receives the currently selected generation,
        // predecessor, and in-flight candidate from the owning store.
        let retained = keys
            .iter()
            .copied()
            .filter(|(image, _)| {
                reopened
                    .resume(&target, *image, &[])
                    .expect("inspect recovered candidate")
                    .is_some()
            })
            .take(2)
            .collect::<Vec<_>>();
        assert_eq!(retained.len(), 2);
        let current = retained[0].0;
        let predecessor = retained[1].0;
        let candidate = image_key(100);
        let candidate_identity = SemanticImageIdentity::from_encoded_bytes(b"candidate");
        let (partial, checkpoint) = reopened.stage_paths(&stage_dir, candidate);
        assert!(!partial.exists() && !checkpoint.exists());
        recover_stage_directory(
            &stage_dir,
            &[current, predecessor, candidate],
            Some(candidate),
        )
        .expect("keep selected, predecessor, and reserve candidate slot");
        assert_eq!(stage_count(&stage_dir), MAX_IMAGE_STAGES - 1);
        for (protected, identity) in retained {
            assert_eq!(
                reopened
                    .resume(&target, protected, &[current, predecessor])
                    .expect("preserved resume")
                    .expect("selected stage retained")
                    .identity(),
                identity
            );
        }
        let staged = reopened
            .stage_page(
                &target,
                candidate,
                candidate_identity,
                4,
                ByteRange::new(0, 1).expect("candidate range"),
                b"c",
                &[current, predecessor],
            )
            .expect("new candidate uses reserved stage slot");
        assert_eq!(staged.next_offset(), 1);
        assert_eq!(stage_count(&stage_dir), MAX_IMAGE_STAGES);
        drop(reopened);

        let cold_again = LocalSemanticImageFiles::open(&directory.0).expect("second reopen");
        cold_again
            .recover_stages()
            .expect("second bounded recovery");
        assert_eq!(
            cold_again
                .resume(&target, candidate, &[current, predecessor, candidate])
                .expect("candidate resumes after restart")
                .expect("candidate stage remains")
                .next_offset(),
            1
        );
    }

    #[test]
    fn rejected_complete_image_is_discarded_and_retry_starts_at_zero() {
        let directory = TestDirectory::create();
        let (target, _, _) = fixture();
        let files = LocalSemanticImageFiles::open(&directory.0).expect("open image files");
        let canonical = canonical_image_bytes();
        let generation = GenerationId::from_canonical_bytes(&canonical);
        let image = SemanticPlaneImageKey::new(
            0,
            generation,
            backend_semantic::ir::SemanticManifestRoot::from_wire_claim([4; 32]),
        );
        let mut corrupted = canonical.clone();
        let last = corrupted.len() - 1;
        corrupted[last] ^= 1;
        let corrupted_identity = SemanticImageIdentity::from_encoded_bytes(&corrupted);
        let corrupted_resume = files
            .stage_page(
                &target,
                image,
                corrupted_identity,
                corrupted.len() as u64,
                ByteRange::new(0, corrupted.len() as u64).expect("corrupt full range"),
                &corrupted,
                &[],
            )
            .expect("stage corrupt complete image");
        let error = files
            .finish(&target, corrupted_resume, &[])
            .err()
            .expect("corrupt canonical image is rejected");
        assert!(
            error.to_string().contains("generation") || error.to_string().contains("image"),
            "unexpected image rejection: {error}"
        );
        assert_eq!(
            files
                .resume(&target, image, &[])
                .expect("rejected stage was removed"),
            None
        );

        let identity = SemanticImageIdentity::from_encoded_bytes(&canonical);
        let resume = files
            .stage_page(
                &target,
                image,
                identity,
                canonical.len() as u64,
                ByteRange::new(0, canonical.len() as u64).expect("valid full range"),
                &canonical,
                &[],
            )
            .expect("retry begins from byte zero with owner bytes");
        assert_eq!(resume.next_offset(), canonical.len() as u64);
        let mapped = files
            .finish(&target, resume, &[])
            .expect("correct owner image is admitted");
        assert_eq!(mapped.identity(), identity);
        assert_eq!(mapped.generation(), generation);
    }

    fn cold_reopen_repairs_corrupt_object(with_locator: bool) {
        let directory = TestDirectory::create();
        let (target, _, _) = fixture();
        let bytes = canonical_image_bytes();
        let total_length = u64::try_from(bytes.len()).expect("image length fits wire range");
        let identity = SemanticImageIdentity::from_encoded_bytes(&bytes);
        let generation = GenerationId::from_canonical_bytes(&bytes);
        let image = SemanticPlaneImageKey::new(
            0,
            generation,
            backend_semantic::ir::SemanticManifestRoot::from_wire_claim([7; 32]),
        );
        let files = LocalSemanticImageFiles::open(&directory.0).expect("open image files");
        let resume = files
            .stage_page(
                &target,
                image,
                identity,
                total_length,
                ByteRange::new(0, total_length).expect("complete image range"),
                &bytes,
                &[],
            )
            .expect("stage canonical image");
        drop(
            files
                .finish(&target, resume, &[])
                .expect("admit canonical image"),
        );
        let target_root = files.target_root(&target);
        let object = target_root
            .join("objects")
            .join(format!("{}.image", hex(identity.as_ref())));
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&object)
            .expect("open immutable object for corruption fixture");
        file.seek(SeekFrom::Start(0)).expect("seek object");
        file.write_all(&[bytes[0] ^ 0x80])
            .expect("corrupt cached image byte");
        file.sync_all().expect("sync corrupted object");
        let locator = target_root
            .join("generations")
            .join(format!("{}.ref", image_key_hash(image)));
        if !with_locator {
            fs::remove_file(&locator).expect("remove selected-image locator");
        }
        drop(files);

        let reopened = LocalSemanticImageFiles::open(&directory.0).expect("cold reopen");
        if with_locator {
            assert!(matches!(
                reopened.find_generation(&target, image),
                Err(SemanticImageCacheError::CorruptContent {
                    identity: Some(found),
                    ..
                }) if found == identity
            ));
        } else {
            assert!(matches!(reopened.find_generation(&target, image), Ok(None)));
        }

        let catalog_root = backend_semantic::ir::SemanticPlaneCatalogRoot::from_wire_claim([8; 32]);
        let selected_stamp = crate::SelectedGenerationStamp::checked(
            [1; 16],
            target.profile(),
            [2; 32],
            1,
            [3; 32],
            [4; 32],
            catalog_root,
        )
        .expect("selected stamp");
        let owner_page = crate::SelectedSemanticImageChunk {
            request_id: 1,
            target: target.clone(),
            selected_stamp,
            image,
            image_identity: identity,
            total_length,
            byte_range: ByteRange::new(0, 1).expect("owner first page range"),
            payload: bytes[..1].to_vec(),
        };
        assert!(matches!(
            reopened.find_identity(&target, identity, generation),
            Err(SemanticImageCacheError::CorruptContent {
                identity: Some(found),
                ..
            }) if found == identity
        ));
        let retry = reopened
            .stage_page(
                &target,
                image,
                identity,
                total_length,
                ByteRange::new(0, total_length).expect("retry image range"),
                &bytes,
                &[],
            )
            .expect("stage canonical owner bytes");
        assert!(matches!(
            reopened.finish(&target, retry, &[]),
            Err(SemanticImageCacheError::CorruptContent {
                identity: Some(found),
                ..
            }) if found == identity
        ));
        reopened
            .repair_corrupt_identity(&target, image, selected_stamp, &owner_page)
            .expect("owner-bound cache repair");
        assert!(matches!(
            reopened.find_identity(&target, identity, generation),
            Ok(None)
        ));
        drop(
            reopened
                .finish(&target, retry, &[])
                .expect("admit staged canonical owner bytes"),
        );
        assert!(matches!(
            reopened.find_generation(&target, image),
            Ok(Some(_))
        ));
    }

    #[test]
    fn cold_reopen_repairs_corrupt_selected_image_object_after_owner_page() {
        cold_reopen_repairs_corrupt_object(true);
    }

    #[test]
    fn cold_reopen_repairs_corrupt_unbound_image_object_after_owner_page() {
        cold_reopen_repairs_corrupt_object(false);
    }
}
