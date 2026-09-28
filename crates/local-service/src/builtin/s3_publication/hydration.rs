use super::receipt::{read_receipt_file, validate_receipt_membership};
use super::signing::{aws_timestamp, hex};
use super::*;

impl S3ClosurePublisher {
    fn read_capability(
        &self,
        pack: &PackReceiptSummary,
        fence: WorkFence,
    ) -> Result<PackReadCapability, PublicationError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| PublicationError::Configuration)?
            .as_secs();
        let expires = now
            .checked_add(MAX_PRESIGN_SECONDS)
            .ok_or(PublicationError::Configuration)?;
        let key = format!("{}{}", self.prefix, hex(&pack.pack_id));
        let path = format!("/{}/{}", self.bucket, key);
        let date = aws_timestamp(now);
        let short_date = &date[..8];
        let scope = format!("{short_date}/{}/s3/aws4_request", self.region);
        let credential = format!("{}/{scope}", self.access_key);
        let url = self.presign(
            "GET",
            &path,
            &date,
            MAX_PRESIGN_SECONDS,
            &credential,
            &scope,
            &[("host", String::new())],
            "host",
        )?;
        Ok(PackReadCapability::new(
            url,
            S3PackId::from_bytes(pack.pack_id),
            S3LayoutId::from_bytes(pack.layout_id),
            pack.pack_bytes,
            pack.manifest_bytes,
            pack.sha256,
            pack.manifest_sha256,
            expires,
            fence,
            false,
        ))
    }

    fn selected_receipt(
        &self,
        store: &FileStore,
        selected: RemoteClosureSelection,
    ) -> Result<Arc<ExactS3ClosureReceipt>, PublicationError> {
        let key = ReceiptCacheKey {
            closure: selected.closure,
            attempt_id: selected.attempt_id,
            epoch: selected.epoch,
            attempt_fence: selected.attempt_fence,
        };
        if let Some(receipt) = self
            .receipt_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .filter(|(cached_key, _)| *cached_key == key)
            .map(|(_, receipt)| Arc::clone(receipt))
        {
            receipt.validate_selected(selected)?;
            return Ok(receipt);
        }
        let path = self.receipt_root.join(format!(
            "v2-{}-{}-{}-{}.receipt",
            hex(&selected.closure),
            hex(&selected.attempt_id),
            selected.epoch,
            hex(&selected.attempt_fence),
        ));
        let encoded = read_receipt_file(&path)?;
        let receipt = Arc::new(ExactS3ClosureReceipt::decode(&encoded)?);
        receipt.validate_selected(selected)?;
        validate_receipt_membership(store, &receipt)?;
        *self
            .receipt_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((key, Arc::clone(&receipt)));
        Ok(receipt)
    }

    pub(super) fn hydrate_object_from_s3(
        &self,
        store: &FileStore,
        selected: RemoteClosureSelection,
        object_id: UntrustedObjectId,
        expected_schema: backend_version::SchemaIdentity,
        expected_payload_len: u64,
    ) -> Result<Vec<u8>, PublicationError> {
        if expected_payload_len > 1024 * 1024 {
            return Err(PublicationError::Receipt);
        }
        let receipt = self.selected_receipt(store, selected)?;
        let object_index = receipt
            .object_ids
            .binary_search(object_id.as_bytes())
            .map_err(|_| PublicationError::Receipt)?;
        let object_index = u64::try_from(object_index).map_err(|_| PublicationError::Receipt)?;
        let mut first_object = 0_u64;
        let pack = receipt
            .packs
            .iter()
            .find(|pack| {
                let next = first_object.saturating_add(u64::from(pack.object_count));
                let contains = object_index >= first_object && object_index < next;
                first_object = next;
                contains
            })
            .ok_or(PublicationError::Receipt)?;
        let capability = self.read_capability(pack, receipt.fence)?;
        let remote = self
            .route
            .open_pack(&capability, receipt.fence)
            .map_err(|source| PublicationError::RemoteHydration {
                operation: RemoteHydrationOperation::OpenPack,
                source: Some(source),
            })?;
        let packed = self
            .route
            .get_object_claim(&remote, object_id, receipt.fence, store.relation_registry())
            .map_err(|source| PublicationError::RemoteHydration {
                operation: RemoteHydrationOperation::FetchObject,
                source: Some(source),
            })?;
        if packed.id().as_bytes() != object_id.as_bytes()
            || packed.fence() != receipt.fence
            || packed.pack_id().as_bytes() != &pack.pack_id
            || packed.layout_id().as_bytes() != &pack.layout_id
        {
            return Err(PublicationError::Receipt);
        }
        let envelope_bytes = packed.envelope_bytes();
        let maximum_envelope_bytes = expected_payload_len
            .checked_add(128)
            .ok_or(PublicationError::Receipt)?;
        if envelope_bytes > maximum_envelope_bytes {
            return Err(PublicationError::Receipt);
        }
        let maximum = usize::try_from(envelope_bytes).map_err(|_| PublicationError::Receipt)?;
        let mut envelope =
            packed
                .open_envelope()
                .map_err(|source| PublicationError::RemoteHydration {
                    operation: RemoteHydrationOperation::OpenEnvelope,
                    source: Some(source),
                })?;
        let verified = backend_store::admit_object_envelope(
            &mut envelope,
            maximum,
            store.relation_registry(),
            object_id,
        )
        .map_err(|_| PublicationError::Receipt)?;
        if verified.id().as_bytes() != object_id.as_bytes()
            || verified.schema() != expected_schema
            || verified.payload_len() != expected_payload_len
        {
            return Err(PublicationError::Receipt);
        }
        let payload_offset = envelope_bytes
            .checked_sub(verified.payload_len())
            .ok_or(PublicationError::Receipt)?;
        envelope
            .seek(SeekFrom::Start(payload_offset))
            .map_err(|_| PublicationError::RemoteHydration {
                operation: RemoteHydrationOperation::SeekPayload,
                source: None,
            })?;
        let payload_len =
            usize::try_from(expected_payload_len).map_err(|_| PublicationError::Receipt)?;
        let mut payload = Vec::new();
        payload
            .try_reserve_exact(payload_len)
            .map_err(|_| PublicationError::Store)?;
        payload.resize(payload_len, 0);
        envelope
            .read_exact(&mut payload)
            .map_err(|_| PublicationError::RemoteHydration {
                operation: RemoteHydrationOperation::ReadPayload,
                source: None,
            })?;
        Ok(payload)
    }
}
