use super::*;

impl S3ClosurePublisher {
    /// Stores every object listed by the checked closure index and persists its exact pack set.
    pub(super) fn publish_closure_inner(
        &self,
        store: &FileStore,
        closure: ClosureId,
        target_root: [u8; 32],
        expected_count: u64,
        budget: backend_store::ArtifactBudget,
        publication_fence: PublicationFence,
    ) -> Result<ExactS3ClosureReceipt, PublicationError> {
        publication_fence.validate()?;
        let fence = publication_fence.storage_fence(*closure.as_bytes())?;
        if expected_count == 0
            || usize::try_from(expected_count).ok().is_none_or(|count| {
                count > MAX_RECEIPT_OBJECTS
                    || 700_usize
                        .checked_add(count.saturating_mul(32))
                        .and_then(|bytes| {
                            bytes.checked_add(count.min(MAX_RECEIPT_PACKS).saturating_mul(176))
                        })
                        .is_none_or(|bytes| bytes > MAX_RECEIPT_BYTES)
            })
        {
            return Err(PublicationError::Receipt);
        }

        let index = store
            .read_closure_index(closure)
            .map_err(|_| PublicationError::Store)?;
        let actual_count = index.object_count();
        if actual_count != expected_count || actual_count == 0 {
            return Err(PublicationError::Receipt);
        }
        let mut builder = self.new_builder(store)?;
        let mut current_ids = Vec::<ObjectId>::new();
        let mut current_payload_bytes = 0_u64;
        let mut receipts = Vec::new();
        let mut total_objects = 0_u64;
        let mut total_payload_bytes = 0_u64;
        let mut object_ids = Vec::new();
        let mut membership = blake3::Hasher::new();
        membership.update(b"backend-store-s3.exact-closure-members.v2\0");
        membership.update(closure.as_bytes());
        let mut after = None;

        loop {
            let page = index
                .page_ids(after, CLOSURE_PAGE_IDS)
                .map_err(|_| PublicationError::Store)?;
            if page.object_ids().is_empty() {
                break;
            }
            for id in page.object_ids() {
                let mut reader = store
                    .artifact_sink(budget)
                    .open_object(UntrustedObjectId::from_bytes(*id.as_bytes()))
                    .map_err(|_| PublicationError::Store)?
                    .ok_or(PublicationError::Store)?;
                if reader.id() != *id {
                    return Err(PublicationError::Receipt);
                }
                let payload_len = reader.payload_len();
                let next_payload_bytes = current_payload_bytes
                    .checked_add(payload_len)
                    .ok_or(PublicationError::Receipt)?;
                let current_pack_full = current_ids.len() == MAX_PACK_OBJECTS;
                if !current_ids.is_empty()
                    && (next_payload_bytes > PACK_DATA_BUDGET || current_pack_full)
                {
                    self.finish_pack(builder, &current_ids, fence, &mut receipts)?;
                    builder = self.new_builder(store)?;
                    current_ids.clear();
                    current_payload_bytes = 0;
                }
                if payload_len > PACK_DATA_BUDGET {
                    return Err(PublicationError::Remote);
                }
                let claim = ArtifactObjectClaim::new(
                    reader.schema(),
                    *reader.key(),
                    *reader.version(),
                    payload_len,
                )
                .with_object_id(UntrustedObjectId::from_bytes(*id.as_bytes()));
                let mut payload = PayloadReader::new(&mut reader);
                builder
                    .push_streamed(claim, &mut payload)
                    .map_err(|_| PublicationError::Remote)?;
                current_payload_bytes = current_payload_bytes
                    .checked_add(payload_len)
                    .ok_or(PublicationError::Receipt)?;
                if object_ids.len() >= MAX_RECEIPT_OBJECTS {
                    return Err(PublicationError::Receipt);
                }
                current_ids.push(*id);
                object_ids.push(*id.as_bytes());
                membership.update(id.as_bytes());
                total_objects = total_objects
                    .checked_add(1)
                    .ok_or(PublicationError::Receipt)?;
                total_payload_bytes = total_payload_bytes
                    .checked_add(payload_len)
                    .ok_or(PublicationError::Receipt)?;
            }
            after = page.next();
            if after.is_none() {
                break;
            }
        }
        if !current_ids.is_empty() {
            self.finish_pack(builder, &current_ids, fence, &mut receipts)?;
        }
        let membership_digest = *membership.finalize().as_bytes();
        let receipt = ExactS3ClosureReceipt {
            closure: *closure.as_bytes(),
            target_root,
            publication_fence,
            fence,
            object_count: total_objects,
            payload_bytes: total_payload_bytes,
            membership_digest,
            object_ids,
            packs: receipts,
        };
        receipt.validate(
            *closure.as_bytes(),
            target_root,
            publication_fence,
            expected_count,
        )?;
        self.persist_receipt(&receipt)?;
        Ok(receipt)
    }

    fn new_builder(&self, store: &FileStore) -> Result<S3PackBuilder, PublicationError> {
        S3PackBuilder::new_with_registry(MAX_PACK_BYTES, store.relation_registry().clone())
            .map_err(|_| PublicationError::Remote)
    }

    fn finish_pack(
        &self,
        builder: S3PackBuilder,
        expected_ids: &[ObjectId],
        fence: WorkFence,
        receipts: &mut Vec<PackReceiptSummary>,
    ) -> Result<(), PublicationError> {
        if expected_ids.is_empty() || receipts.len() >= MAX_RECEIPT_PACKS {
            return Err(PublicationError::Receipt);
        }
        let pack = builder.finish().map_err(|_| PublicationError::Remote)?;
        validate_pack_members(&pack, expected_ids)?;
        let (upload, readback) = self.capabilities(&pack, fence)?;
        let stored = self
            .route
            .put_pack(&pack, &upload, fence, Some(&readback))
            .map_err(PublicationError::RemoteStore)?;
        validate_stored_pack(&pack, &stored, expected_ids.len(), fence)?;
        receipts.push(PackReceiptSummary::from_pack_and_stored(
            &pack,
            stored,
            expected_ids,
        ));
        Ok(())
    }

    fn capabilities(
        &self,
        pack: &ImmutableS3Pack,
        fence: WorkFence,
    ) -> Result<(PackUploadCapability, PackReadCapability), PublicationError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| PublicationError::Configuration)?
            .as_secs();
        let expires = now
            .checked_add(MAX_PRESIGN_SECONDS)
            .ok_or(PublicationError::Configuration)?;
        let key = format!("{}{}", self.prefix, hex(pack.pack_id().as_bytes()));
        let path = format!("/{}/{}", self.bucket, key);
        let date = aws_timestamp(now);
        let short_date = &date[..8];
        let scope = format!("{short_date}/{}/s3/aws4_request", self.region);
        let credential = format!("{}/{scope}", self.access_key);
        let checksum = BASE64.encode(pack.sha256());

        let upload_url = self.presign(
            "PUT",
            &path,
            &date,
            MAX_PRESIGN_SECONDS,
            &credential,
            &scope,
            &[
                ("host", "".to_owned()),
                ("if-none-match", "*".to_owned()),
                ("x-amz-checksum-sha256", checksum),
            ],
            "host;if-none-match;x-amz-checksum-sha256",
        )?;
        let read_url = self.presign(
            "GET",
            &path,
            &date,
            MAX_PRESIGN_SECONDS,
            &credential,
            &scope,
            &[("host", String::new())],
            "host",
        )?;
        Ok((
            PackUploadCapability::new(
                upload_url,
                pack.pack_id(),
                pack.layout_id(),
                pack.len(),
                pack.manifest_bytes(),
                *pack.sha256(),
                pack.manifest_sha256(),
                expires,
                fence,
                true,
                true,
            ),
            PackReadCapability::new(
                read_url,
                pack.pack_id(),
                pack.layout_id(),
                pack.len(),
                pack.manifest_bytes(),
                *pack.sha256(),
                pack.manifest_sha256(),
                expires,
                fence,
                true,
            ),
        ))
    }
}

impl SelectedClosurePublisher for S3ClosurePublisher {
    fn publish_closure(
        &self,
        store: &FileStore,
        closure: ClosureId,
        target_root: [u8; 32],
        expected_count: u64,
        budget: backend_store::ArtifactBudget,
        publication_fence: PublicationFence,
    ) -> Result<ExactS3ClosureReceipt, PublicationError> {
        S3ClosurePublisher::publish_closure_inner(
            self,
            store,
            closure,
            target_root,
            expected_count,
            budget,
            publication_fence,
        )
    }

    fn hydrate_object(
        &self,
        store: &FileStore,
        selected: RemoteClosureSelection,
        object_id: UntrustedObjectId,
        expected_schema: backend_version::SchemaIdentity,
        expected_payload_len: u64,
    ) -> Result<Vec<u8>, PublicationError> {
        S3ClosurePublisher::hydrate_object_from_s3(
            self,
            store,
            selected,
            object_id,
            expected_schema,
            expected_payload_len,
        )
    }

    fn hydrate_object_range(
        &self,
        store: &FileStore,
        selected: RemoteClosureSelection,
        object_id: UntrustedObjectId,
        expected_schema: backend_version::SchemaIdentity,
        expected_payload_len: u64,
        offset: u64,
        length: u64,
    ) -> Result<Vec<u8>, PublicationError> {
        self.hydrate_object_range_from_s3(
            store,
            selected,
            object_id,
            expected_schema,
            expected_payload_len,
            offset,
            length,
        )
    }

    fn has_durable_selected_closure(
        &self,
        store: &FileStore,
        selected: RemoteClosureSelection,
    ) -> Result<bool, PublicationError> {
        self.durable_selected_receipt(store, selected)
            .map(|receipt| receipt.is_some())
    }

    fn verified_remote_segments(
        &self,
        store: &FileStore,
        selected: &backend_extension_turso::SelectedGeneration,
    ) -> Result<Option<VerifiedRemoteSegmentSet>, PublicationError> {
        S3ClosurePublisher::verified_remote_segments_from_s3(self, store, selected)
    }
}

fn validate_pack_members(
    pack: &ImmutableS3Pack,
    expected: &[ObjectId],
) -> Result<(), PublicationError> {
    let extents = pack.manifest().extents();
    if extents.len() != expected.len()
        || extents
            .iter()
            .zip(expected)
            .any(|(extent, id)| extent.object_id_bytes() != id.as_bytes())
    {
        return Err(PublicationError::Receipt);
    }
    Ok(())
}

fn validate_stored_pack(
    pack: &ImmutableS3Pack,
    receipt: &StoredPackReceipt,
    expected_count: usize,
    fence: WorkFence,
) -> Result<(), PublicationError> {
    if receipt.pack_id() != pack.pack_id()
        || receipt.layout_id() != pack.layout_id()
        || receipt.pack_bytes() != pack.len()
        || usize::try_from(receipt.object_count()).ok() != Some(expected_count)
        || receipt.sha256() != pack.sha256()
        || receipt.fence() != fence
    {
        return Err(PublicationError::Receipt);
    }
    Ok(())
}

struct PayloadReader<'a> {
    reader: &'a mut ArtifactObjectReader,
    offset: u64,
}

impl<'a> PayloadReader<'a> {
    fn new(reader: &'a mut ArtifactObjectReader) -> Self {
        Self { reader, offset: 0 }
    }
}

impl Read for PayloadReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self
            .reader
            .read_payload_range(self.offset, buffer)
            .map_err(|_| io::Error::other("read checked object payload"))?;
        self.offset = self
            .offset
            .checked_add(u64::try_from(read).map_err(io::Error::other)?)
            .ok_or_else(|| io::Error::other("object payload offset overflow"))?;
        Ok(read)
    }
}
