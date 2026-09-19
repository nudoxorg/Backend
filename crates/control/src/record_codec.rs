//! Canonical codec for immutable control relation values.

use backend_version::{ObjectVersion, Schema};

use super::{
    AttemptData, AttemptOutcome, Authority, CustodyReceipt, CustodyStage, CustodyVerdict,
    DecisionReceipt, DurableReceipt, EvaluationReceipt, MAX_RECEIPT_BYTES, MAX_WAITERS,
    ReviewReceipt, WorkRecord, WorkStatus,
};
use crate::ControlError;
use crate::ids::{
    DecisionReceiptSchema, EvaluationReceiptSchema, FenceSchema, Identity, IdentityBytes,
    OwnerSchema, ReceiptSchema, ReviewReceiptSchema, WorkKeySchema, append_field,
    append_field_unchecked, append_identity, append_identity_unchecked, append_version,
};
use crate::spec::{Effort, WorkDependency, WorkSpec};

impl WorkRecord {
    /// Returns the bounded canonical row encoding used by the relation root.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError::Bounds`] if the retained row exceeds its
    /// canonical wire limit.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ControlError> {
        let mut output = Vec::new();
        output.push(2); // WorkRecord wire version.
        let spec = self.spec.canonical_bytes()?;
        append_field(&mut output, &spec)?;
        output.push(self.status.tag());
        output.extend_from_slice(&self.attempts.to_be_bytes());
        output.extend_from_slice(&self.epoch.to_be_bytes());
        output.extend_from_slice(&self.waiters.to_be_bytes());
        match &self.attempt {
            Some(attempt) => {
                output.push(1);
                append_identity::<OwnerSchema>(&mut output, attempt.owner_identity())?;
                output.extend_from_slice(&attempt.epoch().to_be_bytes());
                append_identity::<FenceSchema>(&mut output, attempt.fence_identity())?;
                output.extend_from_slice(&attempt.expires_at().to_be_bytes());
                output.extend_from_slice(&attempt.attempt().to_be_bytes());
            }
            None => output.push(0),
        }
        append_receipt(&mut output, self.receipt.as_ref())?;
        append_custody(&mut output, self.evaluation.as_ref())?;
        append_custody(&mut output, self.review.as_ref())?;
        append_custody(&mut output, self.decision.as_ref())?;
        if output.len() > MAX_RECEIPT_BYTES {
            return Err(ControlError::Bounds);
        }
        Ok(output)
    }

    pub(crate) fn append_canonical_unchecked(&self, output: &mut Vec<u8>) {
        output.push(2);
        let mut spec = Vec::new();
        self.spec.append_canonical_unchecked(&mut spec);
        append_field_unchecked(output, &spec);
        output.push(self.status.tag());
        output.extend_from_slice(&self.attempts.to_be_bytes());
        output.extend_from_slice(&self.epoch.to_be_bytes());
        output.extend_from_slice(&self.waiters.to_be_bytes());
        match &self.attempt {
            Some(attempt) => {
                output.push(1);
                append_identity_unchecked::<OwnerSchema>(output, attempt.owner_identity());
                output.extend_from_slice(&attempt.epoch().to_be_bytes());
                append_identity_unchecked::<FenceSchema>(output, attempt.fence_identity());
                output.extend_from_slice(&attempt.expires_at().to_be_bytes());
                output.extend_from_slice(&attempt.attempt().to_be_bytes());
            }
            None => output.push(0),
        }
        append_receipt_unchecked(output, self.receipt.as_ref());
        append_custody_unchecked(output, self.evaluation.as_ref());
        append_custody_unchecked(output, self.review.as_ref());
        append_custody_unchecked(output, self.decision.as_ref());
    }

    /// Decodes one canonical relation value and re-admits every typed field.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError::Corrupt`] for malformed bytes, invalid typed
    /// identities, or lifecycle invariants that do not hold.
    pub fn decode(bytes: &[u8]) -> Result<Self, ControlError> {
        if bytes.len() > MAX_RECEIPT_BYTES {
            return Err(ControlError::Bounds);
        }
        let record = decode_record(bytes)?;
        if record.canonical_bytes()?.as_slice() != bytes {
            return Err(ControlError::Corrupt);
        }
        Ok(record)
    }
}

struct DecodedRecord {
    spec: WorkSpec,
    status: WorkStatus,
    attempts: u16,
    epoch: u64,
    waiters: u16,
    attempt: Option<AttemptData>,
    receipt: Option<DurableReceipt>,
    evaluation: Option<EvaluationReceipt>,
    review: Option<ReviewReceipt>,
    decision: Option<DecisionReceipt>,
}

fn decode_record(bytes: &[u8]) -> Result<WorkRecord, ControlError> {
    let mut reader = Reader::new(bytes);
    if reader.byte()? != 2 {
        return Err(ControlError::Corrupt);
    }
    let spec = decode_spec(reader.field()?)?;
    let status = WorkStatus::from_tag(reader.byte()?)?;
    let attempts = reader.u16()?;
    let epoch = reader.u64()?;
    let waiters = reader.u16()?;
    let attempt = match reader.byte()? {
        0 => None,
        1 => {
            let owner = reader.identity::<OwnerSchema>()?;
            let epoch = reader.u64()?;
            let fence = reader.identity::<FenceSchema>()?;
            let expires_at = reader.u64()?;
            let attempt = reader.u16()?;
            Some(AttemptData::new(owner, epoch, fence, expires_at, attempt))
        }
        _ => return Err(ControlError::Corrupt),
    };
    let receipt = decode_receipt(&mut reader, &spec)?;
    let evaluation = decode_custody::<ReceiptSchema, EvaluationReceiptSchema>(
        &mut reader,
        &spec,
        receipt.as_ref().map(DurableReceipt::id),
    )?;
    let review = decode_custody::<EvaluationReceiptSchema, ReviewReceiptSchema>(
        &mut reader,
        &spec,
        evaluation.as_ref().map(EvaluationReceipt::id),
    )?;
    let decision = decode_custody::<ReviewReceiptSchema, DecisionReceiptSchema>(
        &mut reader,
        &spec,
        review.as_ref().map(ReviewReceipt::id),
    )?;
    reader.finish()?;
    let decoded = DecodedRecord {
        spec,
        status,
        attempts,
        epoch,
        waiters,
        attempt,
        receipt,
        evaluation,
        review,
        decision,
    };
    validate_record(&decoded)?;
    Ok(WorkRecord {
        spec: decoded.spec,
        status: decoded.status,
        attempts: decoded.attempts,
        epoch: decoded.epoch,
        waiters: decoded.waiters,
        attempt: decoded.attempt,
        receipt: decoded.receipt,
        evaluation: decoded.evaluation,
        review: decoded.review,
        decision: decoded.decision,
    })
}

fn validate_record(record: &DecodedRecord) -> Result<(), ControlError> {
    let spec = &record.spec;
    let status = record.status;
    let attempts = record.attempts;
    let epoch = record.epoch;
    let waiters = record.waiters;
    let attempt = record.attempt.as_ref();
    let receipt = record.receipt.as_ref();
    let evaluation = record.evaluation.as_ref();
    let review = record.review.as_ref();
    let decision = record.decision.as_ref();
    let retained_lease = status.has_expiring_lease();
    let retained_fence = matches!(status, WorkStatus::Failed | WorkStatus::Expired);
    let receipt_state = matches!(
        status,
        WorkStatus::Frozen
            | WorkStatus::Evaluated
            | WorkStatus::Reviewed
            | WorkStatus::Completed
            | WorkStatus::Rejected
            | WorkStatus::Quarantined
    );
    let attempt_matches_row = attempt
        .as_ref()
        .is_none_or(|attempt| attempt.epoch() == epoch && attempt.attempt() == attempts);
    let receipt_matches_attempt = match (&attempt, &receipt) {
        (Some(attempt), Some(receipt)) if retained_lease => {
            receipt.fence_identity() == attempt.fence_identity()
        }
        _ => true,
    };
    let custody_matches = custody_chain_matches(receipt, evaluation, review, decision);
    let controller_receipt =
        receipt.is_none_or(|receipt| receipt.authority() == Authority::Controller);
    if waiters > MAX_WAITERS
        || (attempts == 0) != (epoch == 0)
        || (retained_lease && attempt.is_none())
        || (attempt.is_some() && !retained_lease && !retained_fence)
        || (receipt.is_some() && !receipt_state)
        || (receipt_state && receipt.is_none())
        || !status_matches_custody(status, evaluation, review, decision)
        || !attempt_matches_row
        || !receipt_matches_attempt
        || !custody_matches
        || !controller_receipt
        || receipt.is_some_and(|receipt| receipt.outcome() != AttemptOutcome::Succeeded)
    {
        return Err(ControlError::Corrupt);
    }
    if let Some(attempt) = attempt {
        let expected_fence =
            crate::ledger::fence_identity(spec.key(), attempt.owner_identity(), attempt.epoch())?;
        if expected_fence != attempt.fence_identity().clone() {
            return Err(ControlError::Corrupt);
        }
    }
    if let Some(receipt) = receipt {
        let expected_fence =
            crate::ledger::fence_identity(spec.key(), receipt.owner_identity(), epoch)?;
        if expected_fence != receipt.fence_identity().clone() {
            return Err(ControlError::Corrupt);
        }
    }
    Ok(())
}

fn custody_chain_matches(
    receipt: Option<&DurableReceipt>,
    evaluation: Option<&EvaluationReceipt>,
    review: Option<&ReviewReceipt>,
    decision: Option<&DecisionReceipt>,
) -> bool {
    let Some(receipt) = receipt else {
        return evaluation.is_none() && review.is_none() && decision.is_none();
    };
    let evaluation_ok = evaluation.is_none_or(|item| {
        item.key() == receipt.key()
            && item.subject() == receipt.id()
            && item.actor() != receipt.owner()
    });
    let review_ok = review.is_none_or(|item| {
        evaluation.is_some_and(|previous| {
            item.key() == receipt.key()
                && item.subject() == previous.id()
                && item.actor() != receipt.owner()
                && item.actor() != previous.actor()
        })
    });
    let decision_ok = decision.is_none_or(|item| {
        review.is_some_and(|previous| {
            item.key() == receipt.key()
                && item.subject() == previous.id()
                && item.actor() != receipt.owner()
                && evaluation.is_some_and(|evaluation| item.actor() != evaluation.actor())
                && item.actor() != previous.actor()
        })
    });
    evaluation_ok && review_ok && decision_ok
}

fn status_matches_custody(
    status: WorkStatus,
    evaluation: Option<&EvaluationReceipt>,
    review: Option<&ReviewReceipt>,
    decision: Option<&DecisionReceipt>,
) -> bool {
    let accepted = CustodyVerdict::Accepted;
    match status {
        WorkStatus::Evaluated => {
            evaluation.is_some_and(|item| item.verdict() == accepted)
                && review.is_none()
                && decision.is_none()
        }
        WorkStatus::Reviewed => {
            evaluation.is_some_and(|item| item.verdict() == accepted)
                && review.is_some_and(|item| item.verdict() == accepted)
                && decision.is_none()
        }
        WorkStatus::Completed => {
            evaluation.is_some_and(|item| item.verdict() == accepted)
                && review.is_some_and(|item| item.verdict() == accepted)
                && decision.is_some_and(|item| item.verdict() == accepted)
        }
        WorkStatus::Rejected | WorkStatus::Quarantined => {
            let expected = if status == WorkStatus::Rejected {
                CustodyVerdict::Rejected
            } else {
                CustodyVerdict::Quarantined
            };
            (evaluation.is_some_and(|item| item.verdict() == expected)
                && review.is_none()
                && decision.is_none())
                || (evaluation.is_some_and(|item| item.verdict() == accepted)
                    && review.is_some_and(|item| item.verdict() == expected)
                    && decision.is_none())
                || (evaluation.is_some_and(|item| item.verdict() == accepted)
                    && review.is_some_and(|item| item.verdict() == accepted)
                    && decision.is_some_and(|item| item.verdict() == expected))
                || (status == WorkStatus::Quarantined
                    && evaluation.is_none()
                    && review.is_none()
                    && decision.is_none())
        }
        _ => evaluation.is_none() && review.is_none() && decision.is_none(),
    }
}

fn append_receipt(
    output: &mut Vec<u8>,
    receipt: Option<&DurableReceipt>,
) -> Result<(), ControlError> {
    match receipt {
        Some(receipt) => {
            output.push(1);
            append_version::<WorkKeySchema>(output, receipt.key());
            append_identity::<OwnerSchema>(output, receipt.owner_identity())?;
            append_identity::<FenceSchema>(output, receipt.fence_identity())?;
            output.push(receipt.authority().tag());
            append_identity::<crate::ids::OutputSchema>(output, receipt.output_identity())?;
            append_identity::<crate::ids::EvidenceSchema>(output, receipt.evidence_identity())?;
            output.push(receipt.outcome().tag());
            append_identity::<ReceiptSchema>(output, receipt.receipt_identity())?;
        }
        None => output.push(0),
    }
    Ok(())
}

fn append_receipt_unchecked(output: &mut Vec<u8>, receipt: Option<&DurableReceipt>) {
    match receipt {
        Some(receipt) => {
            output.push(1);
            append_version::<WorkKeySchema>(output, receipt.key());
            append_identity_unchecked::<OwnerSchema>(output, receipt.owner_identity());
            append_identity_unchecked::<FenceSchema>(output, receipt.fence_identity());
            output.push(receipt.authority().tag());
            append_identity_unchecked::<crate::ids::OutputSchema>(
                output,
                receipt.output_identity(),
            );
            append_identity_unchecked::<crate::ids::EvidenceSchema>(
                output,
                receipt.evidence_identity(),
            );
            output.push(receipt.outcome().tag());
            append_identity_unchecked::<ReceiptSchema>(output, receipt.receipt_identity());
        }
        None => output.push(0),
    }
}

fn decode_receipt(
    reader: &mut Reader<'_>,
    spec: &WorkSpec,
) -> Result<Option<DurableReceipt>, ControlError> {
    match reader.byte()? {
        0 => Ok(None),
        1 => {
            let key = reader.raw_version::<WorkKeySchema>()?;
            if key != spec.key().to_bytes() {
                return Err(ControlError::Corrupt);
            }
            let owner = reader.identity::<OwnerSchema>()?;
            let fence = reader.identity::<FenceSchema>()?;
            let authority = Authority::from_tag(reader.byte()?)?;
            let output = reader.identity::<crate::ids::OutputSchema>()?;
            let evidence = reader.identity::<crate::ids::EvidenceSchema>()?;
            let outcome = AttemptOutcome::from_tag(reader.byte()?)?;
            let id = reader.identity::<ReceiptSchema>()?;
            let receipt = DurableReceipt::new(
                spec.key(),
                owner,
                fence,
                authority,
                output,
                evidence,
                outcome,
            )?;
            if receipt.id() != id.id() {
                return Err(ControlError::Corrupt);
            }
            Ok(Some(receipt))
        }
        _ => Err(ControlError::Corrupt),
    }
}

fn append_custody<P, S>(
    output: &mut Vec<u8>,
    receipt: Option<&CustodyReceipt<P, S>>,
) -> Result<(), ControlError>
where
    P: Schema<Value = IdentityBytes>,
    S: CustodyStage,
{
    match receipt {
        Some(receipt) => {
            output.push(1);
            append_version::<WorkKeySchema>(output, receipt.key());
            append_version::<P>(output, receipt.subject());
            append_identity::<OwnerSchema>(output, receipt.actor_identity())?;
            output.push(receipt.verdict().tag());
            append_identity::<crate::ids::EvidenceSchema>(output, receipt.evidence_identity())?;
            append_identity::<S>(output, receipt.receipt_identity())?;
        }
        None => output.push(0),
    }
    Ok(())
}

fn append_custody_unchecked<P, S>(output: &mut Vec<u8>, receipt: Option<&CustodyReceipt<P, S>>)
where
    P: Schema<Value = IdentityBytes>,
    S: CustodyStage,
{
    match receipt {
        Some(receipt) => {
            output.push(1);
            append_version::<WorkKeySchema>(output, receipt.key());
            append_version::<P>(output, receipt.subject());
            append_identity_unchecked::<OwnerSchema>(output, receipt.actor_identity());
            output.push(receipt.verdict().tag());
            append_identity_unchecked::<crate::ids::EvidenceSchema>(
                output,
                receipt.evidence_identity(),
            );
            append_identity_unchecked::<S>(output, receipt.receipt_identity());
        }
        None => output.push(0),
    }
}

fn decode_custody<P, S>(
    reader: &mut Reader<'_>,
    spec: &WorkSpec,
    expected_subject: Option<ObjectVersion<P>>,
) -> Result<Option<CustodyReceipt<P, S>>, ControlError>
where
    P: Schema<Value = IdentityBytes>,
    S: CustodyStage,
{
    match reader.byte()? {
        0 => Ok(None),
        1 => {
            if reader.raw_version::<WorkKeySchema>()? != spec.key().to_bytes() {
                return Err(ControlError::Corrupt);
            }
            let subject = expected_subject.ok_or(ControlError::Corrupt)?;
            if reader.raw_version::<P>()? != subject.to_bytes() {
                return Err(ControlError::Corrupt);
            }
            let actor = reader.identity::<OwnerSchema>()?;
            let verdict = CustodyVerdict::from_tag(reader.byte()?)?;
            let evidence = reader.identity::<crate::ids::EvidenceSchema>()?;
            let id = reader.identity::<S>()?;
            let receipt = CustodyReceipt::new(spec.key(), subject, actor, verdict, evidence)?;
            if receipt.id() != id.id() {
                return Err(ControlError::Corrupt);
            }
            Ok(Some(receipt))
        }
        _ => Err(ControlError::Corrupt),
    }
}

fn decode_spec(bytes: &[u8]) -> Result<WorkSpec, ControlError> {
    let mut reader = Reader::new(bytes);
    if reader.byte()? != 1 {
        return Err(ControlError::Corrupt);
    }
    let cell = reader.identity::<crate::ids::CellSchema>()?;
    let role = reader.identity::<crate::ids::RoleSchema>()?;
    let model = reader.identity::<crate::ids::ModelSchema>()?;
    let effort = Effort::from_tag(reader.byte()?)?;
    let toolchain = reader.identity::<crate::ids::ToolchainSchema>()?;
    let input = reader.identity::<crate::ids::InputSchema>()?;
    let claimed_dependency_root = reader.identity::<crate::ids::DependencySchema>()?;
    let count = usize::try_from(reader.u32()?).map_err(|_| ControlError::Bounds)?;
    if count > crate::spec::MAX_DEPENDENCIES {
        return Err(ControlError::Bounds);
    }
    let mut dependencies = Vec::with_capacity(count);
    for _ in 0..count {
        let claimed_key = reader.raw_version::<WorkKeySchema>()?;
        let material = reader.field()?.to_vec();
        let dependency = WorkDependency::from_material(material)?;
        if dependency.key().to_bytes() != claimed_key {
            return Err(ControlError::Corrupt);
        }
        dependencies.push(dependency);
    }
    reader.finish()?;
    let spec = WorkSpec::new(cell, role, model, effort, toolchain, input, dependencies)?;
    if spec.dependency_root() != claimed_dependency_root.id() {
        return Err(ControlError::Corrupt);
    }
    Ok(spec)
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], ControlError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(ControlError::Bounds)?;
        let bytes = self
            .bytes
            .get(self.offset..end)
            .ok_or(ControlError::Corrupt)?;
        self.offset = end;
        Ok(bytes)
    }

    fn byte(&mut self) -> Result<u8, ControlError> {
        self.take(1)?.first().copied().ok_or(ControlError::Corrupt)
    }

    fn u16(&mut self) -> Result<u16, ControlError> {
        Ok(u16::from_be_bytes(
            self.take(2)?
                .try_into()
                .map_err(|_| ControlError::Corrupt)?,
        ))
    }

    fn u32(&mut self) -> Result<u32, ControlError> {
        Ok(u32::from_be_bytes(
            self.take(4)?
                .try_into()
                .map_err(|_| ControlError::Corrupt)?,
        ))
    }

    fn u64(&mut self) -> Result<u64, ControlError> {
        Ok(u64::from_be_bytes(
            self.take(8)?
                .try_into()
                .map_err(|_| ControlError::Corrupt)?,
        ))
    }

    fn field(&mut self) -> Result<&'a [u8], ControlError> {
        let length = usize::try_from(self.u64()?).map_err(|_| ControlError::Bounds)?;
        self.take(length)
    }

    fn raw_version<T: Schema>(&mut self) -> Result<[u8; backend_version::ID_BYTES], ControlError> {
        let domain = self.byte()?;
        let ty = self.u16()?;
        let version = self.byte()?;
        if (domain, ty, version) != (T::DOMAIN, T::TYPE, T::VERSION) {
            return Err(ControlError::Corrupt);
        }
        self.take(backend_version::ID_BYTES)?
            .try_into()
            .map_err(|_| ControlError::Corrupt)
    }

    fn identity<T: Schema<Value = IdentityBytes>>(&mut self) -> Result<Identity<T>, ControlError> {
        let domain = self.byte()?;
        let ty = self.u16()?;
        let version = self.byte()?;
        if (domain, ty, version) != (T::DOMAIN, T::TYPE, T::VERSION) {
            return Err(ControlError::Corrupt);
        }
        let bytes = self.take(backend_version::ID_BYTES)?;
        let value = self.field()?.to_vec();
        Identity::from_wire(bytes, value)
    }

    fn finish(self) -> Result<(), ControlError> {
        if self.offset == self.bytes.len() {
            Ok(())
        } else {
            Err(ControlError::Corrupt)
        }
    }
}
