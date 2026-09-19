//! Canonical sparse input transfer lifecycle.

use super::{
    AdmittedChunk, AuthorityClaim, Frame, InputCas, MAX_ACTIVE_SESSIONS, ObjectKey, ObjectRequest,
    ObjectVersion, ReceivingCas, ReceivingCheckpoint, ReplicationError, Schema, TransferId,
};

impl<T: Schema> InputCas<T> {
    /// Starts a canonical sparse transfer.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn begin(
        &mut self,
        request: ObjectRequest<T>,
        authority: AuthorityClaim,
    ) -> Result<(), ReplicationError> {
        let transfer = request.transfer;
        if !self.active.contains_key(&transfer) && self.active.len() >= MAX_ACTIVE_SESSIONS {
            return Err(ReplicationError::Backpressure);
        }
        let session = ReceivingCas::new(
            request,
            authority,
            self.limits,
            self.max_extents,
            &mut self.sink,
        )?;
        self.active.insert(transfer, session);
        Ok(())
    }

    /// Admits a complete one-frame object when the object itself is the proof
    /// preimage. This is used for the small product relation-root input; large
    /// values use [`Self::begin`], [`Self::stage`], and a durable checkpoint.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn ingest_complete_frame(
        &mut self,
        frame: Frame<T>,
        authority: AuthorityClaim,
    ) -> Result<ObjectVersion<T>, ReplicationError>
    where
        T: Schema<Value = [u8]>,
    {
        frame.validate(self.limits)?;
        if frame.offset != 0 || frame.object_len != frame.payload.len() as u64 {
            return Err(ReplicationError::Range);
        }
        let transfer = frame.transfer;
        let key = ObjectKey::<T>::admit_value(frame.key.into_untrusted(), &frame.payload)
            .map_err(|_| ReplicationError::IdentityMismatch)?;
        let version =
            ObjectVersion::<T>::admit_value(frame.version.into_untrusted(), &frame.payload)
                .map_err(|_| ReplicationError::IdentityMismatch)?;
        let request = ObjectRequest::whole(
            frame.transfer,
            key,
            version,
            frame.object_len,
            self.limits.max_ranges,
        )?;
        self.begin(request, authority)?;
        let admitted = frame.admit(self.limits)?;
        self.stage(admitted)?;
        self.finish(transfer)
    }

    /// Stages one bounded frame against an already authenticated object
    /// summary. This is the multi-frame sibling of
    /// [`Self::ingest_complete_frame`]: no bytes are concatenated while a
    /// transfer is in flight, and completion remains gated by the canonical
    /// receiving typestate.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn ingest_frame(
        &mut self,
        frame: Frame<T>,
        key: ObjectKey<T>,
        version: ObjectVersion<T>,
        authority: AuthorityClaim,
    ) -> Result<Option<ObjectVersion<T>>, ReplicationError> {
        frame.validate(self.limits)?;
        if frame.key
            != backend_engine::claim_schema_object_key(key).map_err(ReplicationError::from)?
            || frame.version
                != backend_engine::claim_schema_object_version(version)
                    .map_err(ReplicationError::from)?
            || frame.object_len == 0
        {
            return Err(ReplicationError::IdentityMismatch);
        }
        let transfer = frame.transfer;
        let object_len = frame.object_len;
        if !self.active.contains_key(&frame.transfer) {
            if self.active.len() >= MAX_ACTIVE_SESSIONS {
                return Err(ReplicationError::Backpressure);
            }
            let request = ObjectRequest::whole(
                frame.transfer,
                key,
                version,
                frame.object_len,
                self.limits.max_ranges,
            )?;
            self.begin(request, authority)?;
        }
        self.stage(frame.admit(self.limits)?)?;
        if !self.checkpoint(transfer)?.coverage.is_complete(object_len) {
            return Ok(None);
        }
        match self.finish(transfer) {
            Ok(version) => Ok(Some(version)),
            Err(error) => Err(error),
        }
    }

    /// Admits and stages a wire chunk against the retained typed request.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn stage(&mut self, chunk: AdmittedChunk<T>) -> Result<(), ReplicationError> {
        let transfer = chunk.transfer();
        let session = self
            .active
            .get_mut(&transfer)
            .ok_or(ReplicationError::Disconnected)?;
        session.stage(&mut self.sink, chunk).map(|_| ())
    }

    /// Reopens a checkpoint recovered from the owner journal after process
    /// restart. The canonical checkpoint validator rechecks every identity,
    /// authority, extent, and coverage claim before any bytes are exposed.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn resume(
        &mut self,
        request: ObjectRequest<T>,
        authority: AuthorityClaim,
        checkpoint: ReceivingCheckpoint<T>,
    ) -> Result<(), ReplicationError> {
        let transfer = request.transfer;
        if self.active.contains_key(&transfer) {
            return Err(ReplicationError::ReplayConflict);
        }
        let session = ReceivingCas::resume(
            request,
            authority,
            self.limits,
            self.max_extents,
            checkpoint,
            &mut self.sink,
        )?;
        self.active.insert(transfer, session);
        Ok(())
    }

    /// Returns a byte-free durable checkpoint for reconnect/restart handoff.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn checkpoint(
        &self,
        transfer: TransferId,
    ) -> Result<ReceivingCheckpoint<T>, ReplicationError> {
        self.active
            .get(&transfer)
            .ok_or(ReplicationError::Disconnected)?
            .checkpoint()
    }

    /// Finishes a transfer and atomically publishes its canonical object.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn finish(&mut self, transfer: TransferId) -> Result<ObjectVersion<T>, ReplicationError> {
        let session = self
            .active
            .remove(&transfer)
            .ok_or(ReplicationError::Disconnected)?;
        session.finish(&mut self.sink)
    }

    /// Aborts an unpublished transfer while retaining committed objects.
    pub fn abort(&mut self, transfer: TransferId) {
        if let Some(session) = self.active.remove(&transfer) {
            session.abort(&mut self.sink);
        }
    }
}
