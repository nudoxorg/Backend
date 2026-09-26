//! Leased subscription response codec.
//!
//! Request bytes stay in the parent. These functions encode and decode the
//! opened, resumed, batch, reset, acked, renewed, cancelled, and page replies.

use super::{
    RESET_BRANCH_DISCARDED, RESET_GAP, RESET_PRUNED, RESET_ROOT_MISMATCH, RESET_SCHEMA_MISMATCH,
    SUBSCRIPTION_ACKED, SUBSCRIPTION_BATCH, SUBSCRIPTION_CANCELLED, SUBSCRIPTION_OPENED,
    SUBSCRIPTION_PAGE, SUBSCRIPTION_RENEWED, SUBSCRIPTION_RESET, SUBSCRIPTION_RESUMED,
    append_cursor_header, read_lease, read_u32, read_u64, size_error, validate_credit,
    validate_credit_and_lease, validate_lease,
};
use crate::transport::{
    LocalControlError, LocalControlLimits, LocalSubscriptionResetReason, LocalSubscriptionResponse,
};

#[allow(
    clippy::too_many_lines,
    reason = "subscription grammar is kept contiguous"
)]
pub(in crate::transport::local::local_codec) fn encode_subscription_response(
    response: &LocalSubscriptionResponse,
    limits: LocalControlLimits,
) -> Result<Vec<u8>, LocalControlError> {
    let mut output = Vec::new();
    match response {
        LocalSubscriptionResponse::Opened {
            lease,
            cursor,
            credit,
            lease_ms,
            ..
        } => {
            validate_lease(*lease)?;
            validate_response_cursor(cursor, limits)?;
            validate_credit_and_lease(*credit, *lease_ms)?;
            output.push(SUBSCRIPTION_OPENED);
            output.extend_from_slice(&lease.as_bytes());
            append_cursor_header(&mut output, cursor, limits, false)?;
            output.extend_from_slice(
                &u64::try_from(*credit)
                    .map_err(|_| LocalControlError::FrameTooLarge)?
                    .to_be_bytes(),
            );
            output.extend_from_slice(&lease_ms.to_be_bytes());
            output.extend_from_slice(cursor);
        }
        LocalSubscriptionResponse::Resumed {
            lease,
            cursor,
            credit,
            lease_ms,
            ..
        } => {
            validate_lease(*lease)?;
            validate_response_cursor(cursor, limits)?;
            validate_credit_and_lease(*credit, *lease_ms)?;
            output.push(SUBSCRIPTION_RESUMED);
            output.extend_from_slice(&lease.as_bytes());
            append_cursor_header(&mut output, cursor, limits, false)?;
            output.extend_from_slice(
                &u64::try_from(*credit)
                    .map_err(|_| LocalControlError::FrameTooLarge)?
                    .to_be_bytes(),
            );
            output.extend_from_slice(&lease_ms.to_be_bytes());
            output.extend_from_slice(cursor);
        }
        LocalSubscriptionResponse::Batch {
            lease,
            previous,
            cursor,
            credit,
            payload,
            ..
        } => {
            validate_lease(*lease)?;
            validate_response_cursor(previous, limits)?;
            validate_response_cursor(cursor, limits)?;
            validate_credit(*credit)?;
            validate_subscription_payload(payload, limits)?;
            output.push(SUBSCRIPTION_BATCH);
            output.extend_from_slice(&lease.as_bytes());
            append_cursor_header(&mut output, previous, limits, false)?;
            append_cursor_header(&mut output, cursor, limits, false)?;
            output.extend_from_slice(
                &u64::try_from(*credit)
                    .map_err(|_| LocalControlError::FrameTooLarge)?
                    .to_be_bytes(),
            );
            output.extend_from_slice(previous);
            output.extend_from_slice(cursor);
            output.extend_from_slice(payload);
        }
        LocalSubscriptionResponse::ResetWithRoot {
            lease,
            cursor,
            credit,
            reason,
            payload,
            ..
        } => {
            validate_lease(*lease)?;
            validate_response_cursor(cursor, limits)?;
            validate_credit(*credit)?;
            validate_subscription_payload(payload, limits)?;
            output.push(SUBSCRIPTION_RESET);
            output.extend_from_slice(&lease.as_bytes());
            append_cursor_header(&mut output, cursor, limits, false)?;
            output.extend_from_slice(
                &u64::try_from(*credit)
                    .map_err(|_| LocalControlError::FrameTooLarge)?
                    .to_be_bytes(),
            );
            output.push(reset_reason_byte(*reason));
            output.extend_from_slice(cursor);
            output.extend_from_slice(payload);
        }
        LocalSubscriptionResponse::Acked { lease, cursor, .. } => {
            validate_lease(*lease)?;
            validate_response_cursor(cursor, limits)?;
            output.push(SUBSCRIPTION_ACKED);
            output.extend_from_slice(&lease.as_bytes());
            append_cursor_header(&mut output, cursor, limits, false)?;
            output.extend_from_slice(cursor);
        }
        LocalSubscriptionResponse::Renewed {
            lease,
            cursor,
            credit,
            lease_ms,
            ..
        } => {
            validate_lease(*lease)?;
            validate_response_cursor(cursor, limits)?;
            validate_credit_and_lease(*credit, *lease_ms)?;
            output.push(SUBSCRIPTION_RENEWED);
            output.extend_from_slice(&lease.as_bytes());
            append_cursor_header(&mut output, cursor, limits, false)?;
            output.extend_from_slice(
                &u64::try_from(*credit)
                    .map_err(|_| LocalControlError::FrameTooLarge)?
                    .to_be_bytes(),
            );
            output.extend_from_slice(&lease_ms.to_be_bytes());
            output.extend_from_slice(cursor);
        }
        LocalSubscriptionResponse::Cancelled { lease, .. } => {
            validate_lease(*lease)?;
            output.push(SUBSCRIPTION_CANCELLED);
            output.extend_from_slice(&lease.as_bytes());
        }
        LocalSubscriptionResponse::SnapshotPage {
            lease,
            page,
            next,
            credit,
            payload,
            ..
        } => {
            validate_lease(*lease)?;
            validate_credit(*credit)?;
            validate_subscription_payload(payload, limits)?;
            if page.len() > limits.max_cursor {
                return Err(LocalControlError::Invalid(
                    "subscription page cursor bounds",
                ));
            }
            if next
                .as_deref()
                .is_some_and(|next| next.len() > limits.max_cursor)
            {
                return Err(LocalControlError::Invalid(
                    "subscription next page cursor bounds",
                ));
            }
            output.push(SUBSCRIPTION_PAGE);
            output.extend_from_slice(&lease.as_bytes());
            append_cursor_header(&mut output, page, limits, true)?;
            output.extend_from_slice(
                &u32::try_from(next.as_deref().map_or(0, <[u8]>::len))
                    .map_err(|_| LocalControlError::FrameTooLarge)?
                    .to_be_bytes(),
            );
            output.extend_from_slice(
                &u64::try_from(*credit)
                    .map_err(|_| LocalControlError::FrameTooLarge)?
                    .to_be_bytes(),
            );
            output.extend_from_slice(page);
            if let Some(next) = next {
                output.extend_from_slice(next);
            }
            output.extend_from_slice(payload);
        }
    }
    if output.len() > limits.max_frame {
        return Err(LocalControlError::FrameTooLarge);
    }
    Ok(output)
}

#[allow(
    clippy::too_many_lines,
    reason = "subscription grammar is kept contiguous"
)]
pub(in crate::transport::local::local_codec) fn decode_subscription_response(
    payload: &[u8],
    request_id: u64,
    limits: LocalControlLimits,
) -> Result<LocalSubscriptionResponse, LocalControlError> {
    let tag = *payload.first().ok_or(LocalControlError::Truncated)?;
    match tag {
        SUBSCRIPTION_OPENED | SUBSCRIPTION_RESUMED | SUBSCRIPTION_RENEWED => {
            if payload.len() < 37 {
                return Err(LocalControlError::Truncated);
            }
            let lease = read_lease(payload, 1)?;
            validate_lease(lease)?;
            let cursor_len = read_u32(payload, 17, "subscription cursor length")?;
            let credit = usize::try_from(read_u64(payload, 21, "subscription credit")?)
                .map_err(|_| LocalControlError::FrameTooLarge)?;
            let lease_ms = read_u64(payload, 29, "subscription lease")?;
            validate_credit_and_lease(credit, lease_ms)?;
            let expected = 37usize
                .checked_add(cursor_len)
                .ok_or(LocalControlError::FrameTooLarge)?;
            if payload.len() != expected {
                return Err(size_error(payload.len(), expected));
            }
            let cursor = payload[37..].to_vec().into_boxed_slice();
            validate_response_cursor(&cursor, limits)?;
            match tag {
                SUBSCRIPTION_OPENED => Ok(LocalSubscriptionResponse::Opened {
                    request_id,
                    lease,
                    cursor,
                    credit,
                    lease_ms,
                }),
                SUBSCRIPTION_RESUMED => Ok(LocalSubscriptionResponse::Resumed {
                    request_id,
                    lease,
                    cursor,
                    credit,
                    lease_ms,
                }),
                SUBSCRIPTION_RENEWED => Ok(LocalSubscriptionResponse::Renewed {
                    request_id,
                    lease,
                    cursor,
                    credit,
                    lease_ms,
                }),
                _ => Err(LocalControlError::Invalid("subscription response tag")),
            }
        }
        SUBSCRIPTION_BATCH => {
            if payload.len() < 33 {
                return Err(LocalControlError::Truncated);
            }
            let lease = read_lease(payload, 1)?;
            validate_lease(lease)?;
            let previous_len = read_u32(payload, 17, "subscription previous cursor length")?;
            let cursor_len = read_u32(payload, 21, "subscription cursor length")?;
            let credit = usize::try_from(read_u64(payload, 25, "subscription credit")?)
                .map_err(|_| LocalControlError::FrameTooLarge)?;
            validate_credit(credit)?;
            let data_start = 33usize
                .checked_add(previous_len)
                .and_then(|value| value.checked_add(cursor_len))
                .ok_or(LocalControlError::FrameTooLarge)?;
            if previous_len == 0
                || previous_len > limits.max_cursor
                || cursor_len == 0
                || cursor_len > limits.max_cursor
            {
                return Err(LocalControlError::Invalid("subscription cursor bounds"));
            }
            if payload.len() <= data_start {
                return Err(LocalControlError::Invalid("subscription batch payload"));
            }
            let previous_end = 33 + previous_len;
            let cursor_end = previous_end + cursor_len;
            let previous = payload[33..previous_end].to_vec().into_boxed_slice();
            let cursor = payload[previous_end..cursor_end]
                .to_vec()
                .into_boxed_slice();
            let body = payload[cursor_end..].to_vec().into_boxed_slice();
            Ok(LocalSubscriptionResponse::Batch {
                request_id,
                lease,
                previous,
                cursor,
                credit,
                payload: body,
            })
        }
        SUBSCRIPTION_RESET => {
            if payload.len() < 30 {
                return Err(LocalControlError::Truncated);
            }
            let lease = read_lease(payload, 1)?;
            validate_lease(lease)?;
            let cursor_len = read_u32(payload, 17, "subscription cursor length")?;
            let credit = usize::try_from(read_u64(payload, 21, "subscription credit")?)
                .map_err(|_| LocalControlError::FrameTooLarge)?;
            validate_credit(credit)?;
            let reason = reset_reason(payload[29])?;
            let cursor_end = 30usize
                .checked_add(cursor_len)
                .ok_or(LocalControlError::FrameTooLarge)?;
            if cursor_len == 0 || cursor_len > limits.max_cursor {
                return Err(LocalControlError::Invalid("subscription cursor bounds"));
            }
            if payload.len() <= cursor_end {
                return Err(LocalControlError::Invalid("subscription reset payload"));
            }
            let cursor = payload[30..cursor_end].to_vec().into_boxed_slice();
            let body = payload[cursor_end..].to_vec().into_boxed_slice();
            Ok(LocalSubscriptionResponse::ResetWithRoot {
                request_id,
                lease,
                cursor,
                credit,
                reason,
                payload: body,
            })
        }
        SUBSCRIPTION_ACKED => {
            if payload.len() < 21 {
                return Err(LocalControlError::Truncated);
            }
            let lease = read_lease(payload, 1)?;
            validate_lease(lease)?;
            let cursor_len = read_u32(payload, 17, "subscription cursor length")?;
            let expected = 21usize
                .checked_add(cursor_len)
                .ok_or(LocalControlError::FrameTooLarge)?;
            if cursor_len == 0 || cursor_len > limits.max_cursor {
                return Err(LocalControlError::Invalid("subscription cursor bounds"));
            }
            if payload.len() != expected {
                return Err(size_error(payload.len(), expected));
            }
            Ok(LocalSubscriptionResponse::Acked {
                request_id,
                lease,
                cursor: payload[21..].to_vec().into_boxed_slice(),
            })
        }
        SUBSCRIPTION_CANCELLED => {
            if payload.len() != 17 {
                return Err(size_error(payload.len(), 17));
            }
            let lease = read_lease(payload, 1)?;
            validate_lease(lease)?;
            Ok(LocalSubscriptionResponse::Cancelled { request_id, lease })
        }
        SUBSCRIPTION_PAGE => {
            if payload.len() < 33 {
                return Err(LocalControlError::Truncated);
            }
            let lease = read_lease(payload, 1)?;
            validate_lease(lease)?;
            let page_len = read_u32(payload, 17, "subscription page cursor length")?;
            let next_len = read_u32(payload, 21, "subscription next page cursor length")?;
            let credit = usize::try_from(read_u64(payload, 25, "subscription credit")?)
                .map_err(|_| LocalControlError::FrameTooLarge)?;
            validate_credit(credit)?;
            if page_len > limits.max_cursor || next_len > limits.max_cursor {
                return Err(LocalControlError::Invalid(
                    "subscription page cursor bounds",
                ));
            }
            let data_start = 33usize
                .checked_add(page_len)
                .and_then(|value| value.checked_add(next_len))
                .ok_or(LocalControlError::FrameTooLarge)?;
            if payload.len() <= data_start {
                return Err(LocalControlError::Invalid("subscription page payload"));
            }
            let page_end = 33 + page_len;
            let next_end = page_end + next_len;
            let page = payload[33..page_end].to_vec().into_boxed_slice();
            let next =
                (next_len != 0).then(|| payload[page_end..next_end].to_vec().into_boxed_slice());
            let body = payload[next_end..].to_vec().into_boxed_slice();
            Ok(LocalSubscriptionResponse::SnapshotPage {
                request_id,
                lease,
                page,
                next,
                credit,
                payload: body,
            })
        }
        _ => Err(LocalControlError::Invalid("subscription response tag")),
    }
}

fn validate_response_cursor(
    cursor: &[u8],
    limits: LocalControlLimits,
) -> Result<(), LocalControlError> {
    if cursor.is_empty() || cursor.len() > limits.max_cursor {
        return Err(LocalControlError::Invalid("subscription cursor bounds"));
    }
    Ok(())
}

fn validate_subscription_payload(
    payload: &[u8],
    limits: LocalControlLimits,
) -> Result<(), LocalControlError> {
    if payload.is_empty() {
        return Err(LocalControlError::Invalid("subscription payload"));
    }
    if payload.len() > limits.max_frame {
        return Err(LocalControlError::FrameTooLarge);
    }
    Ok(())
}

const fn reset_reason_byte(reason: LocalSubscriptionResetReason) -> u8 {
    match reason {
        LocalSubscriptionResetReason::Gap => RESET_GAP,
        LocalSubscriptionResetReason::BranchDiscarded => RESET_BRANCH_DISCARDED,
        LocalSubscriptionResetReason::Pruned => RESET_PRUNED,
        LocalSubscriptionResetReason::SchemaMismatch => RESET_SCHEMA_MISMATCH,
        LocalSubscriptionResetReason::RootMismatch => RESET_ROOT_MISMATCH,
    }
}

fn reset_reason(byte: u8) -> Result<LocalSubscriptionResetReason, LocalControlError> {
    match byte {
        RESET_GAP => Ok(LocalSubscriptionResetReason::Gap),
        RESET_BRANCH_DISCARDED => Ok(LocalSubscriptionResetReason::BranchDiscarded),
        RESET_PRUNED => Ok(LocalSubscriptionResetReason::Pruned),
        RESET_SCHEMA_MISMATCH => Ok(LocalSubscriptionResetReason::SchemaMismatch),
        RESET_ROOT_MISMATCH => Ok(LocalSubscriptionResetReason::RootMismatch),
        _ => Err(LocalControlError::Invalid("subscription reset reason")),
    }
}
