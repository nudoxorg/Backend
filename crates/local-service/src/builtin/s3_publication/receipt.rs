use super::*;

impl S3ClosurePublisher {
    pub(super) fn persist_receipt(
        &self,
        receipt: &ExactS3ClosureReceipt,
    ) -> Result<(), PublicationError> {
        let encoded = receipt.encode()?;
        fs::create_dir_all(&self.receipt_root).map_err(|_| PublicationError::ReceiptIo)?;
        let metadata =
            fs::symlink_metadata(&self.receipt_root).map_err(|_| PublicationError::ReceiptIo)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(PublicationError::ReceiptIo);
        }
        let target = self.receipt_path(receipt);
        if let Ok(existing) = read_receipt_file(&target) {
            let recovered = ExactS3ClosureReceipt::decode(&existing)?;
            if recovered == *receipt {
                File::open(&target)
                    .and_then(|file| file.sync_all())
                    .and_then(|()| {
                        backend_platform::durability::open_directory(&self.receipt_root)?
                            .sync_all()
                    })
                    .map_err(|_| PublicationError::ReceiptIo)?;
                return Ok(());
            }
            return Err(PublicationError::Receipt);
        }
        let unique = NEXT_RECEIPT_TEMP.fetch_add(1, Ordering::Relaxed);
        let temp = self
            .receipt_root
            .join(format!(".tmp-{}-{unique}", std::process::id()));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|_| PublicationError::ReceiptIo)?;
        let result = (|| {
            file.write_all(&encoded)
                .map_err(|_| PublicationError::ReceiptIo)?;
            file.sync_all().map_err(|_| PublicationError::ReceiptIo)?;
            drop(file);
            match fs::hard_link(&temp, &target) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    let current = read_receipt_file(&target)?;
                    if current != encoded {
                        return Err(PublicationError::Receipt);
                    }
                }
                Err(_) => return Err(PublicationError::ReceiptIo),
            }
            fs::remove_file(&temp).map_err(|_| PublicationError::ReceiptIo)?;
            backend_platform::durability::open_directory(&self.receipt_root)
                .and_then(|directory| directory.sync_all())
                .map_err(|_| PublicationError::ReceiptIo)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result
    }

    fn receipt_path(&self, receipt: &ExactS3ClosureReceipt) -> PathBuf {
        self.receipt_root.join(format!(
            "v2-{}-{}-{}-{}.receipt",
            hex(&receipt.closure),
            hex(&receipt.publication_fence.attempt_id),
            receipt.publication_fence.epoch,
            hex(&receipt.publication_fence.attempt_fence),
        ))
    }

    /// Reopens the exact durable receipt for a live Turso selection without
    /// consulting the hydration cache. GC uses this path because an in-memory
    /// receipt must never authorize eviction after its durable record has been
    /// removed or replaced.
    pub(super) fn durable_selected_receipt(
        &self,
        store: &FileStore,
        selected: RemoteClosureSelection,
    ) -> Result<Option<ExactS3ClosureReceipt>, PublicationError> {
        match fs::symlink_metadata(&self.receipt_root) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                // Continue below with the exact immutable receipt path.
            }
            Ok(_) => return Err(PublicationError::ReceiptIo),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(PublicationError::ReceiptIo),
        }
        let path = self.receipt_root.join(format!(
            "v2-{}-{}-{}-{}.receipt",
            hex(&selected.closure),
            hex(&selected.attempt_id),
            selected.epoch,
            hex(&selected.attempt_fence),
        ));
        let Some(encoded) = read_receipt_file_if_exists(&path)? else {
            return Ok(None);
        };
        let receipt = match ExactS3ClosureReceipt::decode(&encoded) {
            Ok(receipt) => receipt,
            Err(PublicationError::Receipt) => return Ok(None),
            Err(error) => return Err(error),
        };
        // A record at the expected filename can still be stale or replaced.
        // Keep the local closure rooted unless every selected-generation field
        // matches the exact Turso row we are collecting.
        if receipt.validate_selected(selected).is_err() {
            return Ok(None);
        }
        match validate_receipt_membership(store, &receipt) {
            Ok(()) => {}
            Err(PublicationError::Receipt) => return Ok(None),
            Err(error) => return Err(error),
        }
        Ok(Some(receipt))
    }

    pub(super) fn verified_remote_segments_from_s3(
        &self,
        store: &FileStore,
        selected: &backend_extension_turso::SelectedGeneration,
    ) -> Result<Option<VerifiedRemoteSegmentSet>, PublicationError> {
        let Some(receipt) =
            self.durable_selected_receipt(store, RemoteClosureSelection::from_selected(selected))?
        else {
            return Ok(None);
        };
        let reopened = backend_extension_turso::reopen_selected_compiler_metadata(store, selected)
            .map_err(|_| PublicationError::Store)?;
        let Some(planes) = reopened.metadata().versioned_planes() else {
            return Ok(None);
        };
        let expected_count = planes.segment_count();
        if expected_count == 0 || expected_count > MAX_RECEIPT_OBJECTS {
            return Ok(None);
        }
        let mut object_ids = Vec::new();
        object_ids
            .try_reserve_exact(expected_count)
            .map_err(|_| PublicationError::Receipt)?;
        for artifact in planes.artifacts() {
            for member in artifact.members() {
                let object_id = *member.object_id();
                if object_id == [0; 32]
                    || member.byte_length() == 0
                    || member.byte_length()
                        > backend_semantic::ir::MAX_SEMANTIC_SEGMENT_BYTES as u64
                    || receipt.object_ids.binary_search(&object_id).is_err()
                {
                    return Ok(None);
                }
                object_ids.push(object_id);
            }
        }
        if object_ids.len() != expected_count {
            return Ok(None);
        }
        object_ids.sort_unstable();
        object_ids.dedup();
        if object_ids.is_empty() || object_ids.len() > expected_count {
            return Ok(None);
        }
        let closure = store
            .open_closure_claim(ArtifactClosureClaim::from_bytes(receipt.closure))
            .map_err(|_| PublicationError::Store)?
            .id();
        Ok(Some(VerifiedRemoteSegmentSet {
            closure,
            object_ids,
        }))
    }
}

impl PublicationFence {
    pub(in crate::builtin) fn for_attempt(
        attempt: &backend_extension_turso::CandidateAttempt,
        assignment: Option<backend_engine::cluster_transport::AssignmentScope>,
    ) -> Result<Self, PublicationError> {
        let fence = Self {
            namespace_id: attempt.namespace().namespace_id(),
            attempt_id: *attempt.attempt_id(),
            epoch: attempt.epoch(),
            attempt_fence: attempt.fence_bytes(),
            input_digest: *attempt.input_digest(),
            base_generation: attempt.base_generation(),
            base_root: attempt.base_root(),
            assignment,
        };
        fence.validate()?;
        Ok(fence)
    }

    pub(super) fn validate(self) -> Result<(), PublicationError> {
        if self.namespace_id == [0; 16]
            || self.attempt_id == [0; 16]
            || self.epoch == 0
            || self.attempt_fence == [0; 32]
            || self.input_digest == [0; 32]
            || self.base_root.is_some_and(|root| root == [0; 32])
        {
            return Err(PublicationError::Receipt);
        }
        if let Some(assignment) = self.assignment {
            assignment
                .validate()
                .map_err(|_| PublicationError::Receipt)?;
            if assignment.namespace_id() != self.namespace_id
                || assignment.attempt() != self.epoch
                || assignment.fence() != self.attempt_fence
            {
                return Err(PublicationError::Receipt);
            }
        }
        Ok(())
    }

    pub(super) fn storage_fence(
        self,
        closure_root: [u8; 32],
    ) -> Result<WorkFence, PublicationError> {
        self.validate()?;
        if closure_root == [0; 32] {
            return Err(PublicationError::Receipt);
        }
        let (work_id, attempt) = self
            .assignment
            .map_or((self.attempt_id, self.epoch), |assignment| {
                (assignment.work_id(), assignment.attempt())
            });
        Ok(WorkFence {
            namespace_id: self.namespace_id,
            work_id,
            attempt,
            fence: self.attempt_fence,
            closure_root,
        })
    }

    pub(in crate::builtin) const fn assignment_scope(
        self,
    ) -> Option<backend_engine::cluster_transport::AssignmentScope> {
        self.assignment
    }

    fn encode_into(self, output: &mut Vec<u8>) {
        output.extend_from_slice(&self.namespace_id);
        output.extend_from_slice(&self.attempt_id);
        output.extend_from_slice(&self.epoch.to_be_bytes());
        output.extend_from_slice(&self.attempt_fence);
        output.extend_from_slice(&self.input_digest);
        output.extend_from_slice(&self.base_generation.to_be_bytes());
        match self.base_root {
            Some(root) => {
                output.push(1);
                output.extend_from_slice(&root);
            }
            None => {
                output.push(0);
                output.extend_from_slice(&[0; 32]);
            }
        }
        match self.assignment {
            Some(assignment) => {
                output.push(1);
                output.extend_from_slice(&assignment.namespace_id());
                output.extend_from_slice(&assignment.work_id());
                output.extend_from_slice(&assignment.attempt().to_be_bytes());
                output.extend_from_slice(&assignment.fence());
            }
            None => {
                output.push(0);
                output.extend_from_slice(&[0; 72]);
            }
        }
    }

    fn decode_from(reader: &mut ReceiptReader<'_>) -> Result<Self, PublicationError> {
        let namespace_id = reader.array16()?;
        let attempt_id = reader.array16()?;
        let epoch = reader.u64()?;
        let attempt_fence = reader.array32()?;
        let input_digest = reader.array32()?;
        let base_generation = reader.u64()?;
        let base_root_present = reader.u8()?;
        let base_root_bytes = reader.array32()?;
        let base_root = match base_root_present {
            0 if base_root_bytes == [0; 32] => None,
            1 if base_root_bytes != [0; 32] => Some(base_root_bytes),
            _ => return Err(PublicationError::Receipt),
        };
        let assignment_present = reader.u8()?;
        let assignment_namespace = reader.array16()?;
        let assignment_work = reader.array16()?;
        let assignment_attempt = reader.u64()?;
        let assignment_fence = reader.array32()?;
        let assignment = match assignment_present {
            0 if assignment_namespace == [0; 16]
                && assignment_work == [0; 16]
                && assignment_attempt == 0
                && assignment_fence == [0; 32] =>
            {
                None
            }
            1 => Some(
                backend_engine::cluster_transport::AssignmentScope::new(
                    assignment_namespace,
                    assignment_work,
                    assignment_attempt,
                    assignment_fence,
                )
                .map_err(|_| PublicationError::Receipt)?,
            ),
            _ => return Err(PublicationError::Receipt),
        };
        let fence = Self {
            namespace_id,
            attempt_id,
            epoch,
            attempt_fence,
            input_digest,
            base_generation,
            base_root,
            assignment,
        };
        fence.validate()?;
        Ok(fence)
    }
}

impl ExactS3ClosureReceipt {
    pub(in crate::builtin) fn validate_for_selection(
        &self,
        expected_closure: [u8; 32],
        expected_target_root: [u8; 32],
        expected_fence: PublicationFence,
        expected_count: u64,
    ) -> Result<(), PublicationError> {
        self.validate(
            expected_closure,
            expected_target_root,
            expected_fence,
            expected_count,
        )
    }

    pub(super) fn validate_selected(
        &self,
        selected: RemoteClosureSelection,
    ) -> Result<(), PublicationError> {
        self.validate(
            selected.closure,
            selected.target_root,
            self.publication_fence,
            self.object_count,
        )?;
        if self.publication_fence.namespace_id != selected.namespace_id
            || self.publication_fence.attempt_id != selected.attempt_id
            || self.publication_fence.epoch != selected.epoch
            || self.publication_fence.attempt_fence != selected.attempt_fence
            || self.publication_fence.input_digest != selected.input_digest
            || self.publication_fence.base_generation.checked_add(1) != Some(selected.generation)
            || self
                .object_ids
                .binary_search(&selected.candidate_id)
                .is_err()
        {
            return Err(PublicationError::Receipt);
        }
        Ok(())
    }

    pub(super) fn validate(
        &self,
        expected_closure: [u8; 32],
        expected_target_root: [u8; 32],
        expected_fence: PublicationFence,
        expected_count: u64,
    ) -> Result<(), PublicationError> {
        let storage_fence = expected_fence.storage_fence(expected_closure)?;
        let pack_objects = self.packs.iter().try_fold(0_u64, |total, pack| {
            total.checked_add(u64::from(pack.object_count))
        });
        if self.closure != expected_closure
            || self.target_root != expected_target_root
            || self.target_root == [0; 32]
            || self.publication_fence != expected_fence
            || self.fence != storage_fence
            || self.fence.closure_root != self.closure
            || self.object_count != expected_count
            || self.object_count == 0
            || usize::try_from(self.object_count).ok() != Some(self.object_ids.len())
            || self.object_ids.len() > MAX_RECEIPT_OBJECTS
            || self.packs.is_empty()
            || self.packs.len() > MAX_RECEIPT_PACKS
            || pack_objects != Some(self.object_count)
            || self.payload_bytes == 0
            || self.membership_digest != closure_membership_digest(self.closure, &self.object_ids)
            || self
                .object_ids
                .windows(2)
                .any(|pair| pair[0].cmp(&pair[1]).is_ge())
        {
            return Err(PublicationError::Receipt);
        }
        let mut first = 0_usize;
        for pack in &self.packs {
            let count =
                usize::try_from(pack.object_count).map_err(|_| PublicationError::Receipt)?;
            let end = first.checked_add(count).ok_or(PublicationError::Receipt)?;
            let members = self
                .object_ids
                .get(first..end)
                .ok_or(PublicationError::Receipt)?;
            if count == 0
                || count > MAX_PACK_OBJECTS
                || pack.pack_id == [0; 32]
                || pack.layout_id == [0; 32]
                || pack.pack_bytes == 0
                || pack.pack_bytes > MAX_PACK_BYTES
                || pack.manifest_bytes == 0
                || u64::from(pack.manifest_bytes) > pack.pack_bytes
                || pack.sha256 == [0; 32]
                || pack.manifest_sha256 == [0; 32]
                || pack.member_digest
                    != pack_membership_digest(pack.pack_id, members.iter().copied(), members.len())
            {
                return Err(PublicationError::Receipt);
            }
            first = end;
        }
        if first != self.object_ids.len() {
            return Err(PublicationError::Receipt);
        }
        Ok(())
    }

    pub(super) fn encode(&self) -> Result<Vec<u8>, PublicationError> {
        if self.packs.is_empty()
            || self.packs.len() > MAX_RECEIPT_PACKS
            || self.object_ids.len() > MAX_RECEIPT_OBJECTS
        {
            return Err(PublicationError::Receipt);
        }
        let capacity = 700_usize
            .checked_add(
                self.object_ids
                    .len()
                    .checked_mul(32)
                    .ok_or(PublicationError::Receipt)?,
            )
            .and_then(|bytes| bytes.checked_add(self.packs.len().checked_mul(176)?))
            .ok_or(PublicationError::Receipt)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(capacity)
            .map_err(|_| PublicationError::Receipt)?;
        bytes.extend_from_slice(RECEIPT_MAGIC);
        bytes.extend_from_slice(&RECEIPT_VERSION.to_be_bytes());
        bytes.extend_from_slice(&self.closure);
        bytes.extend_from_slice(&self.target_root);
        self.publication_fence.encode_into(&mut bytes);
        bytes.extend_from_slice(&self.fence.namespace_id);
        bytes.extend_from_slice(&self.fence.work_id);
        bytes.extend_from_slice(&self.fence.attempt.to_be_bytes());
        bytes.extend_from_slice(&self.fence.fence);
        bytes.extend_from_slice(&self.fence.closure_root);
        bytes.extend_from_slice(&self.object_count.to_be_bytes());
        bytes.extend_from_slice(&self.payload_bytes.to_be_bytes());
        bytes.extend_from_slice(&self.membership_digest);
        bytes.extend_from_slice(
            &u32::try_from(self.object_ids.len())
                .map_err(|_| PublicationError::Receipt)?
                .to_be_bytes(),
        );
        for object_id in &self.object_ids {
            bytes.extend_from_slice(object_id);
        }
        bytes.extend_from_slice(
            &u32::try_from(self.packs.len())
                .map_err(|_| PublicationError::Receipt)?
                .to_be_bytes(),
        );
        for pack in &self.packs {
            bytes.extend_from_slice(&pack.pack_id);
            bytes.extend_from_slice(&pack.layout_id);
            bytes.extend_from_slice(&pack.pack_bytes.to_be_bytes());
            bytes.extend_from_slice(&pack.manifest_bytes.to_be_bytes());
            bytes.extend_from_slice(&pack.object_count.to_be_bytes());
            bytes.extend_from_slice(&pack.sha256);
            bytes.extend_from_slice(&pack.manifest_sha256);
            bytes.extend_from_slice(&pack.member_digest);
        }
        let checksum = Sha256::digest(&bytes);
        bytes.extend_from_slice(&checksum);
        if bytes.len() > MAX_RECEIPT_BYTES {
            return Err(PublicationError::Receipt);
        }
        Ok(bytes)
    }

    pub(super) fn decode(bytes: &[u8]) -> Result<Self, PublicationError> {
        if bytes.len() < 600 || bytes.len() > MAX_RECEIPT_BYTES {
            return Err(PublicationError::Receipt);
        }
        let checksum_offset = bytes.len() - 32;
        if Sha256::digest(&bytes[..checksum_offset]).as_slice() != &bytes[checksum_offset..] {
            return Err(PublicationError::Receipt);
        }
        let mut reader = ReceiptReader::new(&bytes[..checksum_offset]);
        if reader.take(8)? != RECEIPT_MAGIC || reader.u16()? != RECEIPT_VERSION {
            return Err(PublicationError::Receipt);
        }
        let closure = reader.array32()?;
        let target_root = reader.array32()?;
        let publication_fence = PublicationFence::decode_from(&mut reader)?;
        let namespace_id = reader.array16()?;
        let work_id = reader.array16()?;
        let attempt = reader.u64()?;
        let fence_bytes = reader.array32()?;
        let closure_root = reader.array32()?;
        let object_count = reader.u64()?;
        let payload_bytes = reader.u64()?;
        let membership_digest = reader.array32()?;
        let object_id_count =
            usize::try_from(reader.u32()?).map_err(|_| PublicationError::Receipt)?;
        if object_id_count == 0 || object_id_count > MAX_RECEIPT_OBJECTS {
            return Err(PublicationError::Receipt);
        }
        let mut object_ids = Vec::new();
        object_ids
            .try_reserve_exact(object_id_count)
            .map_err(|_| PublicationError::Receipt)?;
        for _ in 0..object_id_count {
            object_ids.push(reader.array32()?);
        }
        let pack_count = usize::try_from(reader.u32()?).map_err(|_| PublicationError::Receipt)?;
        if pack_count == 0 || pack_count > MAX_RECEIPT_PACKS {
            return Err(PublicationError::Receipt);
        }
        let mut packs = Vec::new();
        packs
            .try_reserve_exact(pack_count)
            .map_err(|_| PublicationError::Receipt)?;
        for _ in 0..pack_count {
            packs.push(PackReceiptSummary {
                pack_id: reader.array32()?,
                layout_id: reader.array32()?,
                pack_bytes: reader.u64()?,
                manifest_bytes: reader.u32()?,
                object_count: reader.u32()?,
                sha256: reader.array32()?,
                manifest_sha256: reader.array32()?,
                member_digest: reader.array32()?,
            });
        }
        reader.finish()?;
        let receipt = Self {
            closure,
            target_root,
            publication_fence,
            fence: WorkFence {
                namespace_id,
                work_id,
                attempt,
                fence: fence_bytes,
                closure_root,
            },
            object_count,
            payload_bytes,
            membership_digest,
            object_ids,
            packs,
        };
        receipt.validate(
            receipt.closure,
            receipt.target_root,
            receipt.publication_fence,
            receipt.object_count,
        )?;
        if receipt.encode()?.as_slice() != bytes {
            return Err(PublicationError::Receipt);
        }
        Ok(receipt)
    }
}

impl PackReceiptSummary {
    pub(super) fn from_pack_and_stored(
        pack: &ImmutableS3Pack,
        receipt: StoredPackReceipt,
        members: &[ObjectId],
    ) -> Self {
        Self {
            pack_id: *receipt.pack_id().as_bytes(),
            layout_id: *receipt.layout_id().as_bytes(),
            pack_bytes: receipt.pack_bytes(),
            manifest_bytes: pack.manifest_bytes(),
            object_count: receipt.object_count(),
            sha256: *receipt.sha256(),
            manifest_sha256: pack.manifest_sha256(),
            member_digest: pack_membership_digest(
                *receipt.pack_id().as_bytes(),
                members.iter().map(|id| *id.as_bytes()),
                members.len(),
            ),
        }
    }
}

pub(super) fn closure_membership_digest(closure: [u8; 32], members: &[[u8; 32]]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend-store-s3.exact-closure-members.v2\0");
    hasher.update(&closure);
    for id in members {
        hasher.update(id);
    }
    *hasher.finalize().as_bytes()
}

pub(super) fn validate_receipt_membership(
    store: &FileStore,
    receipt: &ExactS3ClosureReceipt,
) -> Result<(), PublicationError> {
    let index = store
        .open_closure_claim(ArtifactClosureClaim::from_bytes(receipt.closure))
        .map_err(|_| PublicationError::Store)?;
    if index.object_count() != receipt.object_count {
        return Err(PublicationError::Receipt);
    }
    let mut member_index = 0_usize;
    let mut after = None;
    loop {
        let page = index
            .page_ids(after, CLOSURE_PAGE_IDS)
            .map_err(|_| PublicationError::Store)?;
        if page.object_ids().is_empty() {
            break;
        }
        for object_id in page.object_ids() {
            if receipt.object_ids.get(member_index) != Some(object_id.as_bytes()) {
                return Err(PublicationError::Receipt);
            }
            member_index = member_index
                .checked_add(1)
                .ok_or(PublicationError::Receipt)?;
        }
        after = page.next();
        if after.is_none() {
            break;
        }
    }
    if member_index != receipt.object_ids.len() {
        return Err(PublicationError::Receipt);
    }
    Ok(())
}

pub(super) fn pack_membership_digest(
    pack_id: [u8; 32],
    members: impl IntoIterator<Item = [u8; 32]>,
    member_count: usize,
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend-store-s3.exact-pack-members.v1\0");
    hasher.update(&pack_id);
    hasher.update(&(member_count as u64).to_be_bytes());
    for id in members {
        hasher.update(&id);
    }
    *hasher.finalize().as_bytes()
}

struct ReceiptReader<'a> {
    bytes: &'a [u8],
    cursor: usize,
}

impl<'a> ReceiptReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, cursor: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], PublicationError> {
        let end = self
            .cursor
            .checked_add(length)
            .ok_or(PublicationError::Receipt)?;
        let bytes = self
            .bytes
            .get(self.cursor..end)
            .ok_or(PublicationError::Receipt)?;
        self.cursor = end;
        Ok(bytes)
    }

    fn u16(&mut self) -> Result<u16, PublicationError> {
        Ok(u16::from_be_bytes(
            self.take(2)?
                .try_into()
                .map_err(|_| PublicationError::Receipt)?,
        ))
    }

    fn u8(&mut self) -> Result<u8, PublicationError> {
        self.take(1)?
            .first()
            .copied()
            .ok_or(PublicationError::Receipt)
    }

    fn u32(&mut self) -> Result<u32, PublicationError> {
        Ok(u32::from_be_bytes(
            self.take(4)?
                .try_into()
                .map_err(|_| PublicationError::Receipt)?,
        ))
    }

    fn u64(&mut self) -> Result<u64, PublicationError> {
        Ok(u64::from_be_bytes(
            self.take(8)?
                .try_into()
                .map_err(|_| PublicationError::Receipt)?,
        ))
    }

    fn array16(&mut self) -> Result<[u8; 16], PublicationError> {
        self.take(16)?
            .try_into()
            .map_err(|_| PublicationError::Receipt)
    }

    fn array32(&mut self) -> Result<[u8; 32], PublicationError> {
        self.take(32)?
            .try_into()
            .map_err(|_| PublicationError::Receipt)
    }

    fn finish(self) -> Result<(), PublicationError> {
        if self.cursor == self.bytes.len() {
            Ok(())
        } else {
            Err(PublicationError::Receipt)
        }
    }
}

pub(super) fn read_receipt_file(path: &Path) -> Result<Vec<u8>, PublicationError> {
    read_receipt_file_if_exists(path)?.ok_or(PublicationError::ReceiptIo)
}

pub(super) fn read_receipt_file_if_exists(
    path: &Path,
) -> Result<Option<Vec<u8>>, PublicationError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(PublicationError::ReceiptIo),
    };
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > MAX_RECEIPT_BYTES as u64
    {
        return Err(PublicationError::ReceiptIo);
    }
    let mut file = File::open(path).map_err(|_| PublicationError::ReceiptIo)?;
    let mut bytes = Vec::new();
    file.take((MAX_RECEIPT_BYTES as u64) + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| PublicationError::ReceiptIo)?;
    if bytes.len() > MAX_RECEIPT_BYTES {
        return Err(PublicationError::ReceiptIo);
    }
    Ok(Some(bytes))
}
