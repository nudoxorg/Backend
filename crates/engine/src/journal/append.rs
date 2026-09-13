//! Append, retry, and uncertain-outcome reconciliation.

use super::frame::{encode_frame, read_frame_at_path};
use std::io::Write;

use super::{
    ChainHash, HEADER_BYTES, HashChainJournal, JournalCodec, JournalError, JournalReceipt,
    JournalState, MAX_PAYLOAD, Path, RecordId, chain_digest,
};

fn encoded_payload<D: JournalCodec, F: FnOnce(&mut Vec<u8>)>(
    encode: F,
) -> Result<Vec<u8>, JournalError> {
    let mut payload = Vec::new();
    encode(&mut payload);
    if payload.len() > MAX_PAYLOAD {
        return Err(JournalError::Bounds);
    }
    D::validate(&payload)?;
    Ok(payload)
}

fn append_locked<D: JournalCodec>(
    path: &Path,
    state: &mut JournalState<D>,
    payload: &[u8],
) -> Result<JournalReceipt<D>, JournalError> {
    let sequence = state.next_sequence;
    let next_sequence = sequence.checked_add(1).ok_or(JournalError::Bounds)?;
    let previous = *state.chain.as_bytes();
    let digest = chain_digest::<D>(sequence, &previous, payload);
    let record = RecordId::from_payload(payload);
    let frame = encode_frame::<D>(sequence, &previous, &digest, payload)?;
    let start = state.next_offset;
    let end = start
        .checked_add(u64::try_from(frame.len()).map_err(|_| JournalError::Bounds)?)
        .ok_or(JournalError::Bounds)?;
    let candidate = AppendCandidate {
        start,
        end,
        sequence,
        previous,
        digest,
        record,
        payload,
        next_sequence,
    };
    let actual_start = state.file.metadata()?.len();
    if actual_start != start {
        return resolve_position(path, state, &candidate, actual_start);
    }
    write_candidate(path, state, &candidate, &frame)
}

fn resolve_position<D: JournalCodec>(
    path: &Path,
    state: &mut JournalState<D>,
    candidate: &AppendCandidate<'_, D>,
    actual_start: u64,
) -> Result<JournalReceipt<D>, JournalError> {
    if actual_start == candidate.end {
        match existing_append_matches(path, candidate) {
            Ok(true) if state.file.sync_data().is_ok() => {
                return Ok(adopt_append(
                    state,
                    candidate.start,
                    candidate.sequence,
                    candidate.next_sequence,
                    candidate.end,
                    candidate.digest,
                    candidate.record,
                ));
            }
            Ok(true) => {
                return Err(uncertain_append(
                    candidate.start,
                    candidate.sequence,
                    candidate.digest,
                    candidate.record,
                ));
            }
            Ok(false) => {}
            Err(JournalError::Io(_)) => {
                state.unusable = true;
                return Err(uncertain_append(
                    candidate.start,
                    candidate.sequence,
                    candidate.digest,
                    candidate.record,
                ));
            }
            Err(error) => {
                state.unusable = true;
                return Err(error);
            }
        }
    }
    if actual_start > candidate.start && actual_start < candidate.end {
        return resolve_append_failure(
            path,
            state,
            candidate,
            JournalError::Corrupt("journal append position"),
        );
    }
    state.unusable = true;
    Err(JournalError::Corrupt("journal append position"))
}

fn write_candidate<D: JournalCodec>(
    path: &Path,
    state: &mut JournalState<D>,
    candidate: &AppendCandidate<'_, D>,
    frame: &[u8],
) -> Result<JournalReceipt<D>, JournalError> {
    if let Err(error) = state.file.write_all(frame) {
        return resolve_append_failure(path, state, candidate, JournalError::Io(error));
    }
    if let Err(error) = state.file.sync_data() {
        return resolve_append_failure(path, state, candidate, JournalError::Io(error));
    }
    let actual_end = match state.file.metadata() {
        Ok(metadata) => metadata.len(),
        Err(error) => {
            return resolve_append_failure(path, state, candidate, JournalError::Io(error));
        }
    };
    if actual_end != candidate.end {
        return resolve_append_failure(
            path,
            state,
            candidate,
            JournalError::Corrupt("journal append position"),
        );
    }
    Ok(adopt_append(
        state,
        candidate.start,
        candidate.sequence,
        candidate.next_sequence,
        candidate.end,
        candidate.digest,
        candidate.record,
    ))
}

fn retry_locked<D: JournalCodec>(
    path: &Path,
    state: &mut JournalState<D>,
    receipt: JournalReceipt<D>,
    payload: &[u8],
) -> Result<JournalReceipt<D>, JournalError> {
    let next_sequence = receipt
        .sequence
        .checked_add(1)
        .ok_or(JournalError::Bounds)?;
    let record = RecordId::from_payload(payload);
    let previous = *state.chain.as_bytes();
    let digest = chain_digest::<D>(receipt.sequence, &previous, payload);
    let candidate_end = receipt
        .offset
        .checked_add(u64::try_from(HEADER_BYTES).map_err(|_| JournalError::Bounds)?)
        .and_then(|end| end.checked_add(u64::try_from(payload.len()).ok()?))
        .ok_or(JournalError::Bounds)?;
    let cursor_matches = state.next_offset == receipt.offset
        && state.next_sequence == receipt.sequence
        && digest == *receipt.chain.as_bytes()
        && record == receipt.record;
    let Ok(metadata) = state.file.metadata() else {
        state.unusable = true;
        return Err(uncertain_append(
            receipt.offset,
            receipt.sequence,
            *receipt.chain.as_bytes(),
            receipt.record,
        ));
    };
    let physical_len = metadata.len();
    let frame = match read_frame_at_path::<D>(path, receipt.offset) {
        Ok(frame) => frame,
        Err(error) => {
            return retry_missing_frame(
                path,
                state,
                receipt,
                payload,
                &RetryFrameContext {
                    candidate_end,
                    physical_len,
                    cursor_matches,
                },
                error,
            );
        }
    };
    finish_retry(
        state,
        receipt,
        record,
        payload,
        next_sequence,
        physical_len,
        &frame,
    )
}

fn retry_missing_frame<D: JournalCodec>(
    path: &Path,
    state: &mut JournalState<D>,
    receipt: JournalReceipt<D>,
    payload: &[u8],
    context: &RetryFrameContext,
    error: JournalError,
) -> Result<JournalReceipt<D>, JournalError> {
    let RetryFrameContext {
        candidate_end,
        physical_len,
        cursor_matches,
    } = *context;
    if !cursor_matches {
        state.unusable = true;
        return if physical_len >= candidate_end {
            Err(uncertain_append(
                receipt.offset,
                receipt.sequence,
                *receipt.chain.as_bytes(),
                receipt.record,
            ))
        } else {
            Err(JournalError::Corrupt("append receipt position"))
        };
    }
    match error {
        JournalError::Corrupt("frame header" | "frame payload")
            if physical_len <= candidate_end =>
        {
            if physical_len > receipt.offset && rollback_append(state, receipt.offset).is_err() {
                return Err(uncertain_append(
                    receipt.offset,
                    receipt.sequence,
                    *receipt.chain.as_bytes(),
                    receipt.record,
                ));
            }
            let retried = append_locked(path, state, payload)?;
            if retried != receipt {
                return Err(JournalError::Corrupt("append receipt"));
            }
            Ok(retried)
        }
        JournalError::Io(_) => {
            state.unusable = true;
            Err(uncertain_append(
                receipt.offset,
                receipt.sequence,
                *receipt.chain.as_bytes(),
                receipt.record,
            ))
        }
        error => {
            state.unusable = true;
            if physical_len > candidate_end {
                Err(uncertain_append(
                    receipt.offset,
                    receipt.sequence,
                    *receipt.chain.as_bytes(),
                    receipt.record,
                ))
            } else {
                Err(error)
            }
        }
    }
}

fn finish_retry<D: JournalCodec>(
    state: &mut JournalState<D>,
    receipt: JournalReceipt<D>,
    record: RecordId<D>,
    payload: &[u8],
    next_sequence: u64,
    physical_len: u64,
    frame: &super::JournalFrame<D>,
) -> Result<JournalReceipt<D>, JournalError> {
    if frame.start_offset() != receipt.offset
        || frame.sequence != receipt.sequence
        || frame.chain != receipt.chain
        || frame.record != receipt.record
        || frame.record != record
        || frame.payload.as_ref() != payload
    {
        state.unusable = true;
        return Err(JournalError::Corrupt("append receipt"));
    }
    let end = frame.end_offset;
    if physical_len != end {
        state.unusable = true;
        return Err(uncertain_append(
            receipt.offset,
            receipt.sequence,
            *receipt.chain.as_bytes(),
            receipt.record,
        ));
    }
    if state.next_offset == end
        && state.next_sequence == next_sequence
        && state.chain == receipt.chain
    {
        if state.file.sync_data().is_err() {
            return Err(uncertain_append(
                receipt.offset,
                receipt.sequence,
                *receipt.chain.as_bytes(),
                receipt.record,
            ));
        }
        return Ok(receipt);
    }
    if state.next_offset != receipt.offset
        || state.next_sequence != receipt.sequence
        || state.chain != frame.previous
    {
        state.unusable = true;
        return Err(JournalError::Corrupt("append receipt position"));
    }
    if state.file.sync_data().is_err() {
        return Err(uncertain_append(
            receipt.offset,
            receipt.sequence,
            *receipt.chain.as_bytes(),
            receipt.record,
        ));
    }
    Ok(adopt_append(
        state,
        receipt.offset,
        receipt.sequence,
        next_sequence,
        end,
        *receipt.chain.as_bytes(),
        receipt.record,
    ))
}

impl<D: JournalCodec> HashChainJournal<D> {
    /// Appends and syncs one canonical record.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn append(&self, record: &D::Record) -> Result<JournalReceipt<D>, JournalError> {
        self.append_encoded(|payload| D::encode(record, payload))
    }

    /// Appends a canonical payload produced directly into the journal frame
    /// buffer. This is the borrowed boundary used by the workspace owner: an
    /// Arc-backed prepared transition can stream its immutable slices once
    /// without first constructing a second `WorkspaceRecord` allocation.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn append_encoded<F>(&self, encode: F) -> Result<JournalReceipt<D>, JournalError>
    where
        F: FnOnce(&mut Vec<u8>),
    {
        let payload = encoded_payload::<D, F>(encode)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| JournalError::Corrupt("poisoned journal"))?;
        if state.unusable {
            return Err(JournalError::Corrupt("journal append state"));
        }
        append_locked(&self.path, &mut state, &payload)
    }

    /// Reconciles an append whose outcome was returned as
    /// [`JournalError::AppendUncertain`].
    ///
    /// The receipt identifies the exact frame candidate and the closure must
    /// produce the same canonical payload.  This is the restart-safe retry
    /// boundary: when the complete candidate is already the physical tail it
    /// is synced and adopted, while a missing or partial candidate is removed
    /// with a verified rollback and retried as the same deterministic append.
    /// A suffix after the candidate is never silently ignored.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn retry_encoded<F>(
        &self,
        receipt: JournalReceipt<D>,
        encode: F,
    ) -> Result<JournalReceipt<D>, JournalError>
    where
        F: FnOnce(&mut Vec<u8>),
    {
        let payload = encoded_payload::<D, F>(encode)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| JournalError::Corrupt("poisoned journal"))?;
        if state.unusable {
            return Err(JournalError::Corrupt("journal append state"));
        }
        retry_locked(&self.path, &mut state, receipt, &payload)
    }
}

/// Fixed candidate identity shared by the append and uncertainty paths.
///
/// Grouping the fields makes it impossible for a retry check to accidentally
/// compare a digest from one payload with the offset of another candidate.
pub(super) struct AppendCandidate<'a, D: JournalCodec> {
    pub(super) start: u64,
    pub(super) end: u64,
    pub(super) sequence: u64,
    pub(super) previous: [u8; 32],
    pub(super) digest: [u8; 32],
    pub(super) record: RecordId<D>,
    pub(super) payload: &'a [u8],
    pub(super) next_sequence: u64,
}

#[derive(Clone, Copy)]
struct RetryFrameContext {
    candidate_end: u64,
    physical_len: u64,
    cursor_matches: bool,
}

fn existing_append_matches<D: JournalCodec>(
    path: &Path,
    candidate: &AppendCandidate<'_, D>,
) -> Result<bool, JournalError> {
    let frame = read_frame_at_path(path, candidate.start)?;
    Ok(frame.start_offset() == candidate.start
        && frame.end_offset == candidate.end
        && frame.sequence == candidate.sequence
        && frame.previous == ChainHash::from_bytes(candidate.previous)
        && frame.chain == ChainHash::from_bytes(candidate.digest)
        && frame.record == candidate.record
        && frame.payload.as_ref() == candidate.payload)
}

fn uncertain_append<D: JournalCodec>(
    offset: u64,
    sequence: u64,
    chain: [u8; 32],
    record: RecordId<D>,
) -> JournalError {
    JournalError::AppendUncertain {
        offset,
        sequence,
        chain,
        record: *record.as_bytes(),
    }
}

fn adopt_append<D: JournalCodec>(
    state: &mut JournalState<D>,
    offset: u64,
    sequence: u64,
    next_sequence: u64,
    end: u64,
    digest: [u8; 32],
    record: RecordId<D>,
) -> JournalReceipt<D> {
    state.chain = ChainHash::from_bytes(digest);
    state.next_sequence = next_sequence;
    state.next_offset = end;
    JournalReceipt {
        offset,
        sequence,
        chain: state.chain,
        record,
    }
}

fn resolve_complete_append<D: JournalCodec>(
    path: &Path,
    state: &mut JournalState<D>,
    candidate: &AppendCandidate<'_, D>,
) -> Result<JournalReceipt<D>, JournalError> {
    match existing_append_matches(path, candidate) {
        Ok(true) if state.file.sync_data().is_ok() => Ok(adopt_append(
            state,
            candidate.start,
            candidate.sequence,
            candidate.next_sequence,
            candidate.end,
            candidate.digest,
            candidate.record,
        )),
        Ok(true) => Err(uncertain_append(
            candidate.start,
            candidate.sequence,
            candidate.digest,
            candidate.record,
        )),
        Ok(false) => {
            state.unusable = true;
            Err(JournalError::Corrupt("journal append conflict"))
        }
        Err(JournalError::Io(_)) => {
            state.unusable = true;
            Err(uncertain_append(
                candidate.start,
                candidate.sequence,
                candidate.digest,
                candidate.record,
            ))
        }
        Err(error) => {
            state.unusable = true;
            Err(error)
        }
    }
}

fn resolve_short_append<D: JournalCodec>(
    path: &Path,
    state: &mut JournalState<D>,
    candidate: &AppendCandidate<'_, D>,
    error: JournalError,
    physical_len: u64,
) -> Result<JournalReceipt<D>, JournalError> {
    if physical_len > candidate.start {
        match read_frame_at_path::<D>(path, candidate.start) {
            Ok(frame) => {
                state.unusable = true;
                let same = frame.sequence == candidate.sequence
                    && frame.previous == ChainHash::from_bytes(candidate.previous)
                    && frame.chain == ChainHash::from_bytes(candidate.digest)
                    && frame.record == candidate.record
                    && frame.payload.as_ref() == candidate.payload
                    && frame.end_offset == candidate.end;
                return if same {
                    Err(uncertain_append(
                        candidate.start,
                        candidate.sequence,
                        candidate.digest,
                        candidate.record,
                    ))
                } else {
                    Err(JournalError::Corrupt("journal append conflict"))
                };
            }
            Err(JournalError::Corrupt("frame header" | "frame payload")) => {}
            Err(JournalError::Io(_)) => {
                state.unusable = true;
                return Err(uncertain_append(
                    candidate.start,
                    candidate.sequence,
                    candidate.digest,
                    candidate.record,
                ));
            }
            Err(error) => {
                state.unusable = true;
                return Err(error);
            }
        }
    }
    if rollback_append(state, candidate.start).is_err() {
        return Err(uncertain_append(
            candidate.start,
            candidate.sequence,
            candidate.digest,
            candidate.record,
        ));
    }
    Err(error)
}

pub(super) fn resolve_append_failure<D: JournalCodec>(
    path: &Path,
    state: &mut JournalState<D>,
    candidate: &AppendCandidate<'_, D>,
    error: JournalError,
) -> Result<JournalReceipt<D>, JournalError> {
    match state.file.metadata() {
        Ok(metadata) if metadata.len() == candidate.end => {
            resolve_complete_append(path, state, candidate)
        }
        Ok(metadata) if metadata.len() < candidate.end => {
            resolve_short_append(path, state, candidate, error, metadata.len())
        }
        Ok(_) => {
            // The file is longer than the candidate end.  The candidate may
            // be durable with an unvalidated suffix, so an ordinary error
            // would conflate absence with a committed frame.
            state.unusable = true;
            Err(uncertain_append(
                candidate.start,
                candidate.sequence,
                candidate.digest,
                candidate.record,
            ))
        }
        Err(_) => {
            state.unusable = true;
            Err(uncertain_append(
                candidate.start,
                candidate.sequence,
                candidate.digest,
                candidate.record,
            ))
        }
    }
}

fn rollback_append<D: JournalCodec>(
    state: &mut JournalState<D>,
    start: u64,
) -> Result<(), JournalError> {
    if state.file.set_len(start).is_err()
        || state.file.sync_all().is_err()
        || !matches!(
            state.file.metadata(),
            Ok(metadata) if metadata.len() == start
        )
    {
        state.unusable = true;
        return Err(JournalError::Corrupt("append rollback"));
    }
    Ok(())
}
