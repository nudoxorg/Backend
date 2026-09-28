//! Bounded whole-closure capture without a precomputed closure root.
//!
//! Objects are verified and linked into the immutable CAS as each stream
//! finishes. The builder keeps only a bounded metadata spool and deliberately
//! exposes no closure identity until `seal` has constructed and verified the
//! complete membership tree. An abandoned builder leaves unreferenced CAS
//! objects for normal garbage collection.

use super::artifact::{
    OBJECT_IDENTITY_DOMAIN, OBJECT_IDENTITY_FIXED_BYTES, object_header_bytes, verify_closure_tree,
};
use super::{
    ArtifactBudget, ArtifactChunkReceipt, ArtifactObjectClaim, FileStore, Hash,
    PinnedStoredClosureReceipt, StoreError, StoredClosureReceipt, artifact_fs, io_error, nodes,
    relation_object_limit, sync_directory,
};
use crate::closure::ManifestRelation;
use crate::{ClosureId, ObjectId, UntrustedObjectId};
use backend_version::{
    IdContext, LazyTree, ObjectVersionHasher, PersistedTreeRoot, TreeChange, UntrustedId,
    admit_canonical_root_claim, canonical_empty,
};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    mem::size_of,
};

const METADATA_MAGIC: &[u8; 8] = b"SCMETA01";
const METADATA_RECORD_BYTES: usize = 1 + 2 + 1 + 32 + 32 + 8 + 32;
const MAX_CHUNK_CALLS_HARD_LIMIT: usize = 16_000_000;

/// Explicit resource limits for one streamed full-closure capture.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StreamingClosureBudget {
    /// Maximum number of object claims admitted by the builder.
    pub max_objects: usize,
    /// Maximum aggregate payload bytes in admitted objects.
    pub max_payload_bytes: u64,
    /// Maximum payload bytes in one object.
    pub max_object_bytes: usize,
    /// Maximum bytes accepted by one object-stream write call.
    pub max_chunk_bytes: usize,
    /// Maximum number of object-stream write calls across the closure.
    pub max_chunk_calls: usize,
    /// Maximum charged bytes for the sorted member records and canonical
    /// tree-change input vectors held while sealing. The persistent tree
    /// builder's transient nodes are separately bounded by `max_objects` but
    /// are not included in this byte charge.
    pub max_metadata_bytes: usize,
}

impl StreamingClosureBudget {
    /// Creates one explicit count-, byte-, frame-, and metadata-bounded policy.
    #[must_use]
    pub const fn new(
        max_objects: usize,
        max_payload_bytes: u64,
        max_object_bytes: usize,
        max_chunk_bytes: usize,
        max_chunk_calls: usize,
        max_metadata_bytes: usize,
    ) -> Self {
        Self {
            max_objects,
            max_payload_bytes,
            max_object_bytes,
            max_chunk_bytes,
            max_chunk_calls,
            max_metadata_bytes,
        }
    }

    /// Returns the explicit seal-input byte charge for `object_count` members.
    ///
    /// This accounts for the in-memory member records and canonical tree
    /// changes kept live together. It does not measure process RSS or include
    /// backend-version's transient canonical tree nodes.
    pub fn metadata_input_bytes_for(object_count: usize) -> Result<usize, StoreError> {
        let per_member = size_of::<MemberRecord>()
            .checked_add(size_of::<TreeChange<ManifestRelation>>())
            .ok_or(StoreError::Bounds)?;
        object_count
            .checked_mul(per_member)
            .ok_or(StoreError::Bounds)
    }

    fn validate(self) -> Result<Self, StoreError> {
        if self.max_objects == 0
            || self.max_payload_bytes == 0
            || self.max_object_bytes == 0
            || self.max_chunk_bytes == 0
            || self.max_chunk_calls == 0
            || self.max_metadata_bytes == 0
            || self.max_chunk_calls > MAX_CHUNK_CALLS_HARD_LIMIT
        {
            return Err(StoreError::Bounds);
        }
        Ok(self)
    }
}

/// Incrementally admits typed objects before their complete closure root is
/// known. A dedicated shared GC pin protects every admitted member until
/// `seal` succeeds or this builder is dropped.
pub struct StreamingClosureBuilder {
    store: FileStore,
    sink: super::ArtifactSink,
    budget: StreamingClosureBudget,
    gc_pin: Option<super::layout::GcPinLease>,
    staging_name: String,
    staging_parent: artifact_fs::ArtifactDirectory,
    staging_dir: artifact_fs::ArtifactDirectory,
    lease_file: Option<File>,
    metadata: File,
    object_count: usize,
    declared_payload_bytes: u64,
    payload_bytes: u64,
    bytes_written: u64,
    chunk_calls: usize,
    active_object: bool,
    failed: bool,
}

#[derive(Clone, Copy)]
struct MemberRecord {
    schema_domain: u8,
    schema_type: u16,
    schema_version: u8,
    key: Hash,
    version: Hash,
    length: u64,
    id: ObjectId,
}

impl MemberRecord {
    fn from_claim(claim: ArtifactObjectClaim, id: ObjectId) -> Self {
        let schema = claim.schema();
        Self {
            schema_domain: schema.domain(),
            schema_type: schema.ty(),
            schema_version: schema.version(),
            key: *claim.key(),
            version: *claim.version(),
            length: claim.length(),
            id,
        }
    }

    fn write_to(self, output: &mut File) -> Result<(), StoreError> {
        output
            .write_all(&[self.schema_domain])
            .and_then(|()| output.write_all(&self.schema_type.to_le_bytes()))
            .and_then(|()| output.write_all(&[self.schema_version]))
            .and_then(|()| output.write_all(&self.key))
            .and_then(|()| output.write_all(&self.version))
            .and_then(|()| output.write_all(&self.length.to_le_bytes()))
            .and_then(|()| output.write_all(self.id.as_bytes()))
            .map_err(|error| io_error(&error))
    }

    fn read_from(input: &mut File) -> Result<Self, StoreError> {
        let mut schema = [0; 4];
        let mut key = [0; 32];
        let mut version = [0; 32];
        let mut length = [0; 8];
        let mut id = [0; 32];
        input
            .read_exact(&mut schema)
            .and_then(|()| input.read_exact(&mut key))
            .and_then(|()| input.read_exact(&mut version))
            .and_then(|()| input.read_exact(&mut length))
            .and_then(|()| input.read_exact(&mut id))
            .map_err(|error| io_error(&error))?;
        Ok(Self {
            schema_domain: schema[0],
            schema_type: u16::from_le_bytes([schema[1], schema[2]]),
            schema_version: schema[3],
            key,
            version,
            length: u64::from_le_bytes(length),
            id: ObjectId::from_bytes(id),
        })
    }

    fn claim_order(self) -> (u8, u16, u8, Hash, Hash) {
        (
            self.schema_domain,
            self.schema_type,
            self.schema_version,
            self.key,
            self.version,
        )
    }
}

impl FileStore {
    /// Starts bounded full-closure capture with no caller-supplied target root.
    ///
    /// A shared GC pin is held while objects stream so a concurrent
    /// mark-and-sweep cannot remove a newly admitted member before sealing.
    /// The store mutation lock is held only while creating staging state and
    /// while installing the final closure descriptor.
    ///
    /// # Errors
    /// Returns [`StoreError::Bounds`] for invalid limits and filesystem errors
    /// from session creation or stale-session recovery.
    pub fn begin_streaming_closure(
        &self,
        budget: StreamingClosureBudget,
    ) -> Result<StreamingClosureBuilder, StoreError> {
        let budget = budget.validate()?;
        let gc_pin = self.acquire_gc_pin()?;
        let _process_lock = self.acquire_process_lock()?;
        let staging_root = artifact_fs::prepare_staging_root(self)?;
        artifact_fs::reap_stale_sessions(self, &staging_root)?;
        let staging = artifact_fs::create_session(&staging_root)?;
        let mut metadata = artifact_fs::create_stage_file(&staging.directory, "object-0.tmp")?;
        metadata
            .write_all(METADATA_MAGIC)
            .map_err(|error| io_error(&error))?;
        let sink_budget = ArtifactBudget::new(
            budget.max_objects,
            budget.max_objects,
            budget.max_payload_bytes,
            budget.max_chunk_bytes,
            budget.max_chunk_calls,
        );
        let sink = self.artifact_sink(sink_budget);
        Ok(StreamingClosureBuilder {
            store: self.clone(),
            sink,
            budget,
            gc_pin: Some(gc_pin),
            staging_name: staging.name,
            staging_parent: staging.parent,
            staging_dir: staging.directory,
            lease_file: Some(staging.lease),
            metadata,
            object_count: 0,
            declared_payload_bytes: 0,
            payload_bytes: 0,
            bytes_written: 0,
            chunk_calls: 0,
            active_object: false,
            failed: false,
        })
    }
}

impl StreamingClosureBuilder {
    /// Number of fully admitted objects in this still-unsealed capture.
    #[must_use]
    pub const fn object_count(&self) -> usize {
        self.object_count
    }

    /// Starts the only live object stream. Dropping it before `finish` poisons
    /// the builder, so partial objects cannot be silently skipped.
    pub fn begin_object(
        &mut self,
        claim: ArtifactObjectClaim,
    ) -> Result<ObjectStream<'_>, StoreError> {
        if self.failed || self.active_object || self.object_count >= self.budget.max_objects {
            self.failed = true;
            return Err(StoreError::Bounds);
        }
        let payload_bytes = self
            .declared_payload_bytes
            .checked_add(claim.length())
            .ok_or(StoreError::Bounds)?;
        if claim.length()
            > u64::try_from(self.budget.max_object_bytes).map_err(|_| StoreError::Bounds)?
            || payload_bytes > self.budget.max_payload_bytes
        {
            self.failed = true;
            return Err(StoreError::Bounds);
        }
        let envelope_bytes = object_header_bytes()
            .checked_add(usize::try_from(claim.length()).map_err(|_| StoreError::Bounds)?)
            .ok_or(StoreError::Bounds)?;
        if envelope_bytes > self.store.object_envelope_limit()? {
            self.failed = true;
            return Err(StoreError::Bounds);
        }
        if claim
            .expected_object_id()
            .is_some_and(|id| id.as_bytes() == &[0; 32])
        {
            self.failed = true;
            return Err(StoreError::Corrupt);
        }
        let projected_count = self.object_count.checked_add(1).ok_or(StoreError::Bounds)?;
        check_metadata_budget(projected_count, self.budget.max_metadata_bytes)?;
        let name = format!("object-{}.tmp", projected_count);
        let mut file = artifact_fs::create_stage_file(&self.staging_dir, &name)?;
        self.failed = true;
        write_object_header(&mut file, claim, [0; 32])?;
        let schema = claim.schema();
        let payload_len = usize::try_from(claim.length()).map_err(|_| StoreError::Bounds)?;
        let relation = self.store.relation_registry.contains_schema(schema);
        let version_hasher = if relation {
            if envelope_bytes > relation_object_limit()? {
                self.failed = true;
                return Err(StoreError::Bounds);
            }
            None
        } else {
            Some(ObjectVersionHasher::new(schema, payload_len).map_err(|_| StoreError::Bounds)?)
        };
        let relation_bytes = if relation {
            let mut bytes = Vec::new();
            bytes
                .try_reserve_exact(payload_len)
                .map_err(|_| StoreError::Bounds)?;
            Some(bytes)
        } else {
            None
        };
        let mut object_hasher = blake3::Hasher::new();
        object_hasher.update(OBJECT_IDENTITY_DOMAIN);
        object_hasher.update(
            &OBJECT_IDENTITY_FIXED_BYTES
                .checked_add(claim.length())
                .ok_or(StoreError::Bounds)?
                .to_le_bytes(),
        );
        object_hasher.update(&[schema.domain()]);
        object_hasher.update(&schema.ty().to_le_bytes());
        object_hasher.update(&[schema.version()]);
        object_hasher.update(claim.key());
        object_hasher.update(claim.version());
        object_hasher.update(&claim.length().to_le_bytes());
        self.active_object = true;
        self.declared_payload_bytes = payload_bytes;
        self.failed = false;
        Ok(ObjectStream {
            builder: self,
            claim,
            file: Some(file),
            name,
            written: 0,
            version_hasher,
            object_hasher,
            relation_bytes,
        })
    }

    /// Seals the canonical membership tree after checking every member. No
    /// closure ID or receipt exists before this operation succeeds.
    pub fn seal(mut self) -> Result<StoredClosureReceipt, StoreError> {
        let receipt = self.seal_inner()?;
        self.gc_pin.take();
        Ok(receipt)
    }

    /// Seals the closure and transfers its shared GC pin into the receipt.
    ///
    /// Keep the returned value alive while composing or selecting a closure
    /// that includes these members. This closes the gap between the capture
    /// pin and the next operation's pin acquisition.
    pub fn seal_pinned(mut self) -> Result<PinnedStoredClosureReceipt, StoreError> {
        let receipt = self.seal_inner()?;
        let gc_pin = self.gc_pin.take().ok_or(StoreError::Corrupt)?;
        Ok(PinnedStoredClosureReceipt::from_parts(receipt, gc_pin))
    }

    fn seal_inner(&mut self) -> Result<StoredClosureReceipt, StoreError> {
        if self.failed || self.active_object {
            return Err(StoreError::Corrupt);
        }
        self.metadata.sync_all().map_err(|error| io_error(&error))?;
        self.metadata
            .seek(SeekFrom::Start(0))
            .map_err(|error| io_error(&error))?;
        let expected_metadata_bytes = u64::try_from(METADATA_MAGIC.len())
            .map_err(|_| StoreError::Bounds)?
            .checked_add(
                u64::try_from(self.object_count)
                    .map_err(|_| StoreError::Bounds)?
                    .checked_mul(
                        u64::try_from(METADATA_RECORD_BYTES).map_err(|_| StoreError::Bounds)?,
                    )
                    .ok_or(StoreError::Bounds)?,
            )
            .ok_or(StoreError::Bounds)?;
        if self
            .metadata
            .metadata()
            .map_err(|error| io_error(&error))?
            .len()
            != expected_metadata_bytes
        {
            return Err(StoreError::Corrupt);
        }
        let mut members = Vec::new();
        members
            .try_reserve_exact(self.object_count)
            .map_err(|_| StoreError::Bounds)?;
        let mut magic = [0; METADATA_MAGIC.len()];
        self.metadata
            .read_exact(&mut magic)
            .map_err(|error| io_error(&error))?;
        if &magic != METADATA_MAGIC {
            return Err(StoreError::Corrupt);
        }
        for _ in 0..self.object_count {
            members.push(MemberRecord::read_from(&mut self.metadata)?);
        }
        members.sort_unstable_by_key(|member| member.claim_order());
        for pair in members.windows(2) {
            if pair[0].claim_order() == pair[1].claim_order() {
                return Err(StoreError::MalformedDelta);
            }
        }
        members.sort_unstable_by_key(|member| member.id);
        let mut tree_changes = Vec::new();
        tree_changes
            .try_reserve_exact(members.len())
            .map_err(|_| StoreError::Bounds)?;
        let mut prior: Option<MemberRecord> = None;
        for member in &members {
            if let Some(previous) = prior {
                if previous.id == member.id {
                    if !same_claim(previous, *member) {
                        return Err(StoreError::Corrupt);
                    }
                    continue;
                }
            }
            tree_changes.push(TreeChange {
                key: *member.id.as_bytes(),
                after: Some(()),
            });
            prior = Some(*member);
        }
        drop(members);

        let loader = self.store.owned_relation_node_loader();
        let empty = canonical_empty::<ManifestRelation>();
        let empty_claim = UntrustedId::<ManifestRelation>::from_wire(
            &empty.commitment().to_bytes(),
            IdContext::relation::<ManifestRelation>(),
        )
        .map_err(|_| StoreError::Corrupt)?;
        let checked_empty = admit_canonical_root_claim(empty_claim, empty.as_bytes())
            .map_err(|_| StoreError::Corrupt)?;
        let tree = LazyTree::from_admitted(&loader, PersistedTreeRoot::from_checked(checked_empty));
        let update = tree
            .prepare_update(&tree_changes)
            .map_err(|_| StoreError::Corrupt)?;
        let closure = ClosureId::from_bytes(*update.target().root().as_bytes());
        let object_count = update.target().node().row_count();
        if object_count > u64::try_from(self.budget.max_objects).map_err(|_| StoreError::Bounds)? {
            return Err(StoreError::Bounds);
        }
        let relation_write = self.store.write_lazy_relation_update(&update)?;
        let facts = verify_closure_tree(
            &self.store,
            update.target_root(),
            object_count,
            self.budget.max_objects,
        )?;
        if facts.object_count != object_count {
            return Err(StoreError::Corrupt);
        }
        #[cfg(test)]
        if take_test_fault(11) {
            return Err(StoreError::Io(
                "injected interruption after manifest write".to_owned(),
            ));
        }
        let descriptor = nodes::encode_manifest_descriptor(
            closure,
            relation_write.root(),
            usize::try_from(object_count).map_err(|_| StoreError::Bounds)?,
        )?;
        let descriptor_bytes = u64::try_from(descriptor.len()).map_err(|_| StoreError::Bounds)?;
        let relation_bytes =
            u64::try_from(relation_write.bytes_written).map_err(|_| StoreError::Bounds)?;
        let _process_lock = self.store.acquire_process_lock()?;
        let descriptor_created = artifact_fs::write_closure_descriptor(
            &self.staging_dir,
            &self.store,
            closure,
            &descriptor,
        )?;
        let bytes_written = self
            .bytes_written
            .checked_add(relation_bytes)
            .and_then(|bytes| bytes.checked_add(u64::from(descriptor_created) * descriptor_bytes))
            .ok_or(StoreError::Bounds)?;
        sync_directory(&self.store.root.join("objects"))?;
        self.staging_dir.sync_all()?;
        let receipt = StoredClosureReceipt::from_verified(
            closure,
            object_count,
            self.payload_bytes,
            bytes_written,
            facts.payload_bytes,
        );
        Ok(receipt)
    }

    fn record_member(
        &mut self,
        claim: ArtifactObjectClaim,
        id: ObjectId,
    ) -> Result<(), StoreError> {
        MemberRecord::from_claim(claim, id).write_to(&mut self.metadata)?;
        self.object_count = self.object_count.checked_add(1).ok_or(StoreError::Bounds)?;
        self.payload_bytes = self
            .payload_bytes
            .checked_add(claim.length())
            .ok_or(StoreError::Bounds)?;
        Ok(())
    }

    fn commit_streamed_object(
        &mut self,
        claim: ArtifactObjectClaim,
        name: &str,
        mut file: File,
        object_id: ObjectId,
    ) -> Result<ObjectId, StoreError> {
        let expected = ArtifactObjectClaim::new(
            claim.schema(),
            *claim.key(),
            *claim.version(),
            claim.length(),
        );
        let existing = self
            .sink
            .open_object(UntrustedObjectId::from_bytes(*object_id.as_bytes()))?;
        let mut created = false;
        if let Some(existing) = existing {
            if existing.id() != object_id
                || existing.schema() != expected.schema()
                || existing.key() != expected.key()
                || existing.version() != expected.version()
                || existing.payload_len() != expected.length()
            {
                return Err(StoreError::Corrupt);
            }
            artifact_fs::unlink_stage_file(&self.staging_dir, name)?;
            self.staging_dir.sync_all()?;
        } else {
            let linked = artifact_fs::link_stage_object(
                &self.staging_dir,
                name,
                &file,
                &self.store,
                object_id,
            )?;
            if linked {
                created = true;
            } else {
                let existing = self
                    .sink
                    .open_object(UntrustedObjectId::from_bytes(*object_id.as_bytes()))?
                    .ok_or(StoreError::Corrupt)?;
                if existing.id() != object_id
                    || existing.schema() != expected.schema()
                    || existing.key() != expected.key()
                    || existing.version() != expected.version()
                    || existing.payload_len() != expected.length()
                {
                    return Err(StoreError::Corrupt);
                }
                artifact_fs::unlink_stage_file(&self.staging_dir, name)?;
                self.staging_dir.sync_all()?;
            }
        }
        drop(file);
        let relation_bytes = self.store.index_streamed_relation_reference(
            claim.schema(),
            claim.version(),
            object_id,
        )?;
        if created {
            let encoded = u64::try_from(object_header_bytes())
                .map_err(|_| StoreError::Bounds)?
                .checked_add(claim.length())
                .ok_or(StoreError::Bounds)?;
            self.bytes_written = self
                .bytes_written
                .checked_add(encoded)
                .ok_or(StoreError::Bounds)?;
        }
        self.bytes_written = self
            .bytes_written
            .checked_add(relation_bytes)
            .ok_or(StoreError::Bounds)?;
        self.record_member(claim, object_id)?;
        self.active_object = false;
        Ok(object_id)
    }
}

fn same_claim(left: MemberRecord, right: MemberRecord) -> bool {
    left.schema_domain == right.schema_domain
        && left.schema_type == right.schema_type
        && left.schema_version == right.schema_version
        && left.key == right.key
        && left.version == right.version
        && left.length == right.length
}

impl Drop for StreamingClosureBuilder {
    fn drop(&mut self) {
        let _ = artifact_fs::cleanup_session(
            &self.store,
            &self.staging_parent,
            &self.staging_name,
            &self.staging_dir,
        );
        self.lease_file.take();
        self.gc_pin.take();
    }
}

/// A single sequential object stream borrowing its parent closure builder.
/// This typestate prevents interleaved or partially finished object writes.
pub struct ObjectStream<'a> {
    builder: &'a mut StreamingClosureBuilder,
    claim: ArtifactObjectClaim,
    file: Option<File>,
    name: String,
    written: u64,
    version_hasher: Option<ObjectVersionHasher>,
    object_hasher: blake3::Hasher,
    relation_bytes: Option<Vec<u8>>,
}

impl ObjectStream<'_> {
    /// Writes the next contiguous bounded payload frame.
    pub fn write(&mut self, bytes: &[u8]) -> Result<ArtifactChunkReceipt, StoreError> {
        if self.builder.failed
            || bytes.is_empty()
            || bytes.len() > self.builder.budget.max_chunk_bytes
            || self.builder.chunk_calls >= self.builder.budget.max_chunk_calls
        {
            self.builder.failed = true;
            return Err(StoreError::Bounds);
        }
        let end = self
            .written
            .checked_add(u64::try_from(bytes.len()).map_err(|_| StoreError::Bounds)?)
            .ok_or(StoreError::Bounds)?;
        if end > self.claim.length() {
            self.builder.failed = true;
            return Err(StoreError::Corrupt);
        }
        let write_result = self
            .file
            .as_mut()
            .ok_or(StoreError::Corrupt)?
            .write_all(bytes);
        if let Err(error) = write_result {
            self.builder.failed = true;
            return Err(io_error(&error));
        }
        if let Some(hasher) = &mut self.version_hasher {
            if hasher.update(bytes).is_err() {
                self.builder.failed = true;
                return Err(StoreError::Corrupt);
            }
        }
        if let Some(relation_bytes) = &mut self.relation_bytes {
            relation_bytes.extend_from_slice(bytes);
        }
        self.object_hasher.update(bytes);
        self.written = end;
        self.builder.chunk_calls = self
            .builder
            .chunk_calls
            .checked_add(1)
            .ok_or(StoreError::Bounds)?;
        Ok(ArtifactChunkReceipt {
            duplicate: false,
            object_complete: false,
            bytes_accepted: u64::try_from(bytes.len()).map_err(|_| StoreError::Bounds)?,
            object_id: None,
        })
    }

    /// Verifies and durably admits this exact object into the local CAS.
    pub fn finish(mut self) -> Result<ObjectId, StoreError> {
        if self.builder.failed || self.written != self.claim.length() {
            self.builder.failed = true;
            return Err(StoreError::Corrupt);
        }
        if self
            .version_hasher
            .take()
            .map(|hasher| hasher.finish().map_err(|_| StoreError::Corrupt))
            .transpose()?
            .is_some_and(|version| version != *self.claim.version())
        {
            self.builder.failed = true;
            return Err(StoreError::Corrupt);
        }
        if let Some(payload) = self.relation_bytes.as_deref() {
            self.builder.store.relation_registry.admit_relation_object(
                self.claim.schema(),
                self.claim.key(),
                self.claim.version(),
                payload,
            )?;
        }
        let id_bytes = *self.object_hasher.finalize().as_bytes();
        if id_bytes == [0; 32]
            || self
                .claim
                .expected_object_id()
                .is_some_and(|expected| expected.as_bytes() != &id_bytes)
        {
            self.builder.failed = true;
            return Err(StoreError::Corrupt);
        }
        let id = ObjectId::from_bytes(id_bytes);
        let mut file = self.file.take().ok_or(StoreError::Corrupt)?;
        file.seek(SeekFrom::Start(
            u64::try_from(super::OBJECT_MAGIC.len()).map_err(|_| StoreError::Bounds)?,
        ))
        .and_then(|_| file.write_all(&id_bytes))
        .and_then(|()| file.sync_all())
        .map_err(|error| io_error(&error))?;
        let id = self
            .builder
            .commit_streamed_object(self.claim, &self.name, file, id)?;
        self.builder.active_object = false;
        Ok(id)
    }
}

fn write_object_header(
    output: &mut File,
    claim: ArtifactObjectClaim,
    id: Hash,
) -> Result<(), StoreError> {
    let schema = claim.schema();
    output
        .write_all(super::OBJECT_MAGIC)
        .and_then(|()| output.write_all(&id))
        .and_then(|()| output.write_all(&[schema.domain()]))
        .and_then(|()| output.write_all(&schema.ty().to_le_bytes()))
        .and_then(|()| output.write_all(&[schema.version()]))
        .and_then(|()| output.write_all(claim.key()))
        .and_then(|()| output.write_all(claim.version()))
        .and_then(|()| output.write_all(&claim.length().to_le_bytes()))
        .map_err(|error| io_error(&error))
}

fn check_metadata_budget(count: usize, maximum_bytes: usize) -> Result<(), StoreError> {
    let required = StreamingClosureBudget::metadata_input_bytes_for(count)?;
    if required > maximum_bytes {
        return Err(StoreError::Bounds);
    }
    Ok(())
}

#[cfg(test)]
use std::sync::Mutex;

#[cfg(test)]
static TEST_FAULT: Mutex<Vec<(std::thread::ThreadId, u8)>> = Mutex::new(Vec::new());

#[cfg(test)]
pub(super) fn set_test_fault(point: u8) {
    if let Ok(mut faults) = TEST_FAULT.lock() {
        let owner = std::thread::current().id();
        faults.retain(|(thread, _)| *thread != owner);
        faults.push((owner, point));
    }
    artifact_fs::set_test_fault(point);
}

#[cfg(test)]
fn take_test_fault(point: u8) -> bool {
    let Ok(mut faults) = TEST_FAULT.lock() else {
        return false;
    };
    let owner = std::thread::current().id();
    let Some(index) = faults
        .iter()
        .position(|(thread, pending)| *thread == owner && *pending == point)
    else {
        return false;
    };
    faults.remove(index);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ArtifactClosureClaim, ArtifactPlan, ClosureManifest, GcLimits, GcRoots, TypedObject,
    };
    use backend_version::{ObjectKey, Schema};
    use std::{
        fs,
        path::PathBuf,
        process::Command,
        sync::atomic::{AtomicU64, Ordering},
        sync::mpsc,
        thread,
        time::Duration,
    };

    static NEXT_STORE: AtomicU64 = AtomicU64::new(0);

    struct BytesSchema;

    impl Schema for BytesSchema {
        const DOMAIN: u8 = 0xf4;
        const TYPE: u16 = 27;
        const VERSION: u8 = 1;
        type Value = [u8];

        fn encode(value: &Self::Value, output: &mut Vec<u8>) {
            output.extend_from_slice(value);
        }
    }

    struct TestStore {
        path: PathBuf,
        store: FileStore,
    }

    impl TestStore {
        fn new() -> Self {
            let ordinal = NEXT_STORE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "backend-store-streamed-closure-{}-{ordinal}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&path);
            let store = FileStore::open(&path, 64 * 1024).expect("open test store");
            Self { path, store }
        }
    }

    impl Drop for TestStore {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn object(seed: u8, length: usize) -> TypedObject {
        let bytes = (0..length)
            .map(|index| seed.wrapping_add(u8::try_from(index % 251).expect("bounded index")))
            .collect::<Vec<_>>();
        let key = ObjectKey::<BytesSchema>::from_value(bytes.as_slice());
        TypedObject::from_value(&key, bytes.as_slice())
    }

    fn budget(max_objects: usize) -> StreamingClosureBudget {
        StreamingClosureBudget::new(
            max_objects,
            2 * 1024 * 1024,
            256 * 1024,
            16 * 1024,
            4096,
            16 * 1024 * 1024,
        )
    }

    fn stream_object(builder: &mut StreamingClosureBuilder, item: &TypedObject) -> ObjectId {
        let claim = object_claim(item);
        let mut stream = builder.begin_object(claim).expect("begin object stream");
        for chunk in item.bytes().chunks(4096) {
            stream.write(chunk).expect("write object chunk");
        }
        stream.finish().expect("admit complete object")
    }

    fn object_claim(item: &TypedObject) -> ArtifactObjectClaim {
        ArtifactObjectClaim::new(
            item.schema(),
            *item.key(),
            *item.version(),
            u64::try_from(item.bytes().len()).expect("bounded test object"),
        )
        .with_object_id(UntrustedObjectId::from_bytes(*item.id().as_bytes()))
    }

    fn expected_closure(items: &[TypedObject]) -> ClosureId {
        let mut objects = items.to_vec();
        objects.sort_by_key(|item| (item.schema(), *item.key(), *item.version()));
        ClosureManifest::new(objects)
            .expect("construct expected closure")
            .id()
    }

    #[test]
    fn one_and_two_object_streams_seal_and_reopen_cold() {
        let test = TestStore::new();
        for items in [vec![object(1, 37)], vec![object(9, 51), object(4, 29)]] {
            let expected = expected_closure(&items);
            let mut builder = test
                .store
                .begin_streaming_closure(budget(8))
                .expect("begin rootless closure capture");
            for item in items.iter().rev() {
                stream_object(&mut builder, item);
            }
            let receipt = builder.seal().expect("seal exact closure membership");
            assert_eq!(receipt.closure(), expected);
            let reopened = test
                .store
                .reopen_stored_closure(
                    super::super::ArtifactClosureClaim::from_id(expected),
                    ArtifactBudget::new(8, 8, 2 * 1024 * 1024, 16 * 1024, 4096),
                )
                .expect("cold reopen checked closure");
            assert_eq!(reopened.closure(), expected);
            assert_eq!(reopened.object_count(), u64::try_from(items.len()).unwrap());
        }
    }

    #[test]
    fn empty_streamed_closure_has_canonical_root() {
        let test = TestStore::new();
        let expected = ClosureManifest::new(Vec::new())
            .expect("construct empty closure")
            .id();
        let receipt = test
            .store
            .begin_streaming_closure(budget(1))
            .expect("begin empty capture")
            .seal()
            .expect("seal empty capture");
        assert_eq!(receipt.closure(), expected);
        test.store
            .collect_garbage(&GcRoots::new(), GcLimits::default())
            .expect("sweep the unselected empty closure and relation node");
        let rebuilt = test
            .store
            .begin_streaming_closure(budget(1))
            .expect("begin empty rebuild after GC");
        let rebuilt_receipt = rebuilt.seal().expect("rebuild dangling relation ref");
        assert_eq!(rebuilt_receipt.closure(), expected);
        test.store
            .reopen_stored_closure(
                super::super::ArtifactClosureClaim::from_id(expected),
                ArtifactBudget::new(1, 1, 2 * 1024 * 1024, 16 * 1024, 4096),
            )
            .expect("cold reopen empty closure");
    }

    #[test]
    fn zero_byte_object_finishes_without_a_payload_frame() {
        let test = TestStore::new();
        let item = object(7, 0);
        let mut builder = test
            .store
            .begin_streaming_closure(budget(1))
            .expect("begin zero-byte closure");
        let id = builder
            .begin_object(object_claim(&item))
            .expect("begin empty object")
            .finish()
            .expect("finish zero-byte object without write calls");
        assert_eq!(id, item.id());
        let receipt = builder.seal().expect("seal zero-byte object closure");
        assert_eq!(
            receipt.closure(),
            expected_closure(std::slice::from_ref(&item))
        );
        assert_eq!(receipt.payload_bytes(), 0);
        assert_eq!(receipt.object_count(), 1);
    }

    #[test]
    fn duplicate_member_claims_are_rejected_instead_of_silently_deduplicated() {
        let test = TestStore::new();
        let item = object(12, 41);
        let mut builder = test
            .store
            .begin_streaming_closure(budget(2))
            .expect("begin duplicate-member capture");
        stream_object(&mut builder, &item);
        stream_object(&mut builder, &item);
        assert!(matches!(builder.seal(), Err(StoreError::MalformedDelta)));
    }

    #[test]
    fn explicit_metadata_input_budget_accepts_31_members_plus_envelope_slot() {
        let test = TestStore::new();
        let items = (0..32)
            .map(|seed| object(u8::try_from(seed).expect("bounded seed"), 13))
            .collect::<Vec<_>>();
        let required = StreamingClosureBudget::metadata_input_bytes_for(32)
            .expect("bounded metadata input charge");
        let mut exact_budget = budget(32);
        exact_budget.max_metadata_bytes = required;
        let mut builder = test
            .store
            .begin_streaming_closure(exact_budget)
            .expect("begin exact 32-object budget");
        for item in &items {
            stream_object(&mut builder, item);
        }
        let receipt = builder.seal().expect("seal exact metadata budget");
        assert_eq!(receipt.object_count(), 32);

        let mut short_budget = budget(32);
        short_budget.max_metadata_bytes = required - 1;
        let mut builder = test
            .store
            .begin_streaming_closure(short_budget)
            .expect("begin one-byte-short metadata budget");
        for item in items.iter().take(31) {
            stream_object(&mut builder, item);
        }
        assert!(matches!(
            builder.begin_object(object_claim(&items[31])),
            Err(StoreError::Bounds)
        ));
    }

    #[test]
    fn interrupted_seal_leaves_only_collectable_unselected_objects() {
        let test = TestStore::new();
        let item = object(19, 83);
        let expected = expected_closure(std::slice::from_ref(&item));
        let mut builder = test
            .store
            .begin_streaming_closure(budget(4))
            .expect("begin capture");
        let id = stream_object(&mut builder, &item);
        set_test_fault(11);
        assert!(matches!(builder.seal(), Err(StoreError::Io(_))));
        assert!(test.store.contains_object(id).expect("object metadata"));
        assert!(
            test.store
                .reopen_stored_closure(
                    super::super::ArtifactClosureClaim::from_id(expected),
                    ArtifactBudget::new(4, 4, 2 * 1024 * 1024, 16 * 1024, 4096),
                )
                .is_err()
        );
        test.store
            .collect_garbage(&GcRoots::new(), GcLimits::default())
            .expect("collect failed-seal orphans");
        assert!(!test.store.contains_object(id).expect("object metadata"));
    }

    #[test]
    fn simulated_process_crash_child() {
        let (Ok(path), Ok(point)) = (
            std::env::var("BACKEND_STORE_TEST_CRASH_PATH"),
            std::env::var("BACKEND_STORE_TEST_CRASH_POINT"),
        ) else {
            return;
        };
        let point = point.parse::<u8>().expect("test crash point");
        let store = FileStore::open(path, 64 * 1024).expect("open child store");
        let item = object(19, 83);
        let mut builder = store
            .begin_streaming_closure(budget(4))
            .expect("begin child capture");
        let mut stream = builder
            .begin_object(object_claim(&item))
            .expect("begin child object");
        for chunk in item.bytes().chunks(4096) {
            stream.write(chunk).expect("write child payload");
        }
        if point == 12 {
            set_test_fault(point);
        }
        stream.finish().expect("finish child object");
        if point == 13 || point == 14 {
            set_test_fault(point);
        }
        let _ = builder.seal();
        panic!("crash point {point} was not reached");
    }

    #[test]
    fn hard_process_exit_recovers_object_relation_and_descriptor_boundaries() {
        const CHILD_TEST: &str = "durable::streaming_closure::tests::simulated_process_crash_child";
        let item = object(19, 83);
        let expected = expected_closure(std::slice::from_ref(&item));
        for point in [12_u8, 14, 13] {
            let test = TestStore::new();
            let status = Command::new(std::env::current_exe().expect("test executable"))
                .arg("--exact")
                .arg(CHILD_TEST)
                .env("BACKEND_STORE_TEST_CRASH_PATH", &test.path)
                .env("BACKEND_STORE_TEST_CRASH_POINT", point.to_string())
                .output()
                .expect("start crash-boundary child");
            assert_eq!(
                status.status.code(),
                Some(80 + i32::from(point)),
                "child missed crash point {point}: {}",
                String::from_utf8_lossy(&status.stderr)
            );

            let reopened =
                FileStore::open(&test.path, 64 * 1024).expect("cold-open store after child exit");
            if point == 13 {
                let receipt = reopened
                    .reopen_stored_closure(
                        super::super::ArtifactClosureClaim::from_id(expected),
                        ArtifactBudget::new(4, 4, 2 * 1024 * 1024, 16 * 1024, 4096),
                    )
                    .expect("cold-open closure after descriptor link crash");
                assert_eq!(receipt.closure(), expected);
            }

            let mut followup = reopened
                .begin_streaming_closure(budget(4))
                .expect("reap child session before later capture");
            stream_object(&mut followup, &item);
            let receipt = followup.seal().expect("seal after crash recovery");
            assert_eq!(receipt.closure(), expected);
            reopened
                .reopen_stored_closure(
                    super::super::ArtifactClosureClaim::from_id(expected),
                    ArtifactBudget::new(4, 4, 2 * 1024 * 1024, 16 * 1024, 4096),
                )
                .expect("cold-open closure after follow-up capture");
        }
    }

    #[test]
    fn alias_preflight_keeps_session_lease_after_two_cold_reap_attempts() {
        const CHILD_TEST: &str = "durable::streaming_closure::tests::simulated_process_crash_child";
        let test = TestStore::new();
        let item = object(19, 83);
        let status = Command::new(std::env::current_exe().expect("test executable"))
            .arg("--exact")
            .arg(CHILD_TEST)
            .env("BACKEND_STORE_TEST_CRASH_PATH", &test.path)
            .env("BACKEND_STORE_TEST_CRASH_POINT", "12")
            .output()
            .expect("start object-link crash child");
        assert_eq!(status.status.code(), Some(92));

        let artifact_sessions = test.path.join("staging").join("artifacts");
        let session = fs::read_dir(&artifact_sessions)
            .expect("read stale sessions")
            .map(|entry| entry.expect("read session entry").path())
            .find(|path| path.is_dir())
            .expect("crashed session remains");
        let hostile_alias = session.join("object-2.tmp");
        fs::hard_link(test.store.object_path(item.id()), &hostile_alias)
            .expect("make the stale inode fail the exact two-link rule");

        for _ in 0..2 {
            let cold = FileStore::open(&test.path, 64 * 1024).expect("cold-open failed reap");
            assert!(matches!(
                cold.begin_streaming_closure(budget(4)),
                Err(StoreError::Corrupt)
            ));
            assert!(session.join("ACTIVE.lock").is_file());
        }

        fs::remove_file(&hostile_alias).expect("remove test-only extra alias");
        let cold = FileStore::open(&test.path, 64 * 1024).expect("cold-open repaired store");
        let mut followup = cold
            .begin_streaming_closure(budget(4))
            .expect("repair one valid link alias after two failed attempts");
        stream_object(&mut followup, &item);
        let receipt = followup.seal().expect("seal after stale-session repair");
        assert_eq!(
            receipt.closure(),
            expected_closure(std::slice::from_ref(&item))
        );
    }

    #[test]
    fn capture_gc_pin_does_not_block_head_or_unrelated_object_io() {
        let test = TestStore::new();
        let item = object(31, 73);
        let mut builder = test
            .store
            .begin_streaming_closure(budget(4))
            .expect("begin capture");
        let id = stream_object(&mut builder, &item);

        assert!(test.store.head().expect("read selected head").is_none());
        let unrelated = object(55, 67);
        test.store
            .write_object(&unrelated)
            .expect("admit unrelated immutable object while capture is open");
        assert_eq!(test.store.read_object(unrelated.id()).unwrap(), unrelated);

        let gc_store = test.store.clone();
        let (started_tx, started_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let collector = thread::spawn(move || {
            started_tx.send(()).expect("signal GC start");
            let result = gc_store.collect_garbage(&GcRoots::new(), GcLimits::default());
            done_tx.send(result).expect("send GC result");
        });
        started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("collector thread started");
        assert!(done_rx.recv_timeout(Duration::from_millis(100)).is_err());
        assert!(
            test.store
                .contains_object(id)
                .expect("capture member remains")
        );

        drop(builder);
        done_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("GC resumes after capture drop")
            .expect("collect abandoned member");
        collector.join().expect("join GC thread");
        assert!(
            !test
                .store
                .contains_object(id)
                .expect("capture member swept")
        );
    }

    #[test]
    fn sealed_capture_transfers_gc_pin_into_receipt() {
        let test = TestStore::new();
        let item = object(43, 79);
        let mut builder = test
            .store
            .begin_streaming_closure(budget(4))
            .expect("begin capture");
        let id = stream_object(&mut builder, &item);
        let held = builder.seal_pinned().expect("seal and transfer GC pin");
        assert_eq!(held.receipt().closure(), expected_closure(&[item]));

        let gc_store = test.store.clone();
        let (started_tx, started_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let collector = thread::spawn(move || {
            started_tx.send(()).expect("signal GC start");
            let result = gc_store.collect_garbage(&GcRoots::new(), GcLimits::default());
            done_tx.send(result).expect("send GC result");
        });
        started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("collector thread started");
        assert!(done_rx.recv_timeout(Duration::from_millis(100)).is_err());
        assert!(test.store.contains_object(id).expect("member stays pinned"));

        drop(held);
        done_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("GC resumes after receipt drop")
            .expect("collect unselected member");
        collector.join().expect("join GC thread");
        assert!(!test.store.contains_object(id).expect("member swept"));
    }

    #[test]
    fn legacy_artifact_session_pins_streamed_members_until_drop() {
        let test = TestStore::new();
        let item = object(37, 73);
        let id = item.id();
        let closure = expected_closure(std::slice::from_ref(&item));
        let sink =
            test.store
                .artifact_sink(ArtifactBudget::new(4, 4, 2 * 1024 * 1024, 16 * 1024, 4096));
        let mut session = sink
            .begin(ArtifactPlan::new(
                None,
                ArtifactClosureClaim::from_id(closure),
                vec![object_claim(&item)],
                Vec::new(),
            ))
            .expect("begin legacy closure session");
        session
            .put(0, 0, item.bytes())
            .expect("admit complete member before closure finish");

        let gc_store = test.store.clone();
        let (started_tx, started_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let collector = thread::spawn(move || {
            started_tx.send(()).expect("signal GC start");
            let result = gc_store.collect_garbage(&GcRoots::new(), GcLimits::default());
            done_tx.send(result).expect("send GC result");
        });
        started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("legacy-session collector thread started");
        assert!(done_rx.recv_timeout(Duration::from_millis(100)).is_err());
        assert!(
            test.store
                .contains_object(id)
                .expect("member remains pinned")
        );

        drop(session);
        done_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("GC resumes when unfinished legacy session drops")
            .expect("collect unfinished closure member");
        collector.join().expect("join legacy-session GC thread");
        assert!(
            !test
                .store
                .contains_object(id)
                .expect("member swept after drop")
        );
    }

    #[test]
    fn readers_retry_the_short_cas_hardlink_publication_window() {
        let test = TestStore::new();
        let item = object(44, 127);
        let writer_item = item.clone();
        let writer_store = test.store.clone();
        let (reached_tx, reached_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let writer = thread::spawn(move || {
            let mut builder = writer_store
                .begin_streaming_closure(budget(2))
                .expect("begin writer closure");
            artifact_fs::set_test_link_barrier(reached_tx, release_rx);
            let id = stream_object(&mut builder, &writer_item);
            let receipt = builder.seal().expect("seal writer closure");
            (id, receipt)
        });
        reached_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("writer paused after durable CAS link");

        let release = thread::spawn(move || {
            thread::sleep(Duration::from_millis(10));
            release_tx.send(()).expect("release link window");
        });
        let mut reader = test
            .store
            .artifact_sink(ArtifactBudget::new(2, 2, 2 * 1024 * 1024, 16 * 1024, 4096))
            .open_object(UntrustedObjectId::from_bytes(*item.id().as_bytes()))
            .expect("bounded retry succeeds while stage alias settles")
            .expect("object becomes visible after publication");
        let mut payload = vec![0; item.bytes().len()];
        let read = reader
            .read_payload_range(0, &mut payload)
            .expect("read verified object payload");
        assert_eq!(read, item.bytes().len());
        assert_eq!(payload, item.bytes());
        release.join().expect("join release thread");
        let (id, receipt) = writer.join().expect("join writer");
        assert_eq!(id, item.id());
        assert_eq!(
            receipt.closure(),
            expected_closure(std::slice::from_ref(&item))
        );
    }
}
