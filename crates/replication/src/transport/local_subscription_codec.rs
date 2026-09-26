//! Leased subscription request/response codec.

use super::super::{
    RESET_BRANCH_DISCARDED, RESET_GAP, RESET_PRUNED, RESET_ROOT_MISMATCH, RESET_SCHEMA_MISMATCH,
    SUBSCRIPTION_ACK_PREFIX_BYTES, SUBSCRIPTION_ACKED, SUBSCRIPTION_BATCH,
    SUBSCRIPTION_CANCEL_BYTES, SUBSCRIPTION_CANCELLED, SUBSCRIPTION_CREDIT_BYTES,
    SUBSCRIPTION_ID_BYTES, SUBSCRIPTION_OPEN_PREFIX_BYTES, SUBSCRIPTION_OPENED, SUBSCRIPTION_PAGE,
    SUBSCRIPTION_PAGE_PREFIX_BYTES, SUBSCRIPTION_RENEW_PREFIX_BYTES, SUBSCRIPTION_RENEWED,
    SUBSCRIPTION_RESET, SUBSCRIPTION_RESUME_PREFIX_BYTES, SUBSCRIPTION_RESUMED,
    TAG_SUBSCRIPTION_ACK, TAG_SUBSCRIPTION_CANCEL, TAG_SUBSCRIPTION_CREDIT, TAG_SUBSCRIPTION_OPEN,
    TAG_SUBSCRIPTION_PAGE, TAG_SUBSCRIPTION_RENEW, TAG_SUBSCRIPTION_RESUME,
};
use super::{read_u32, read_u64, size_error};
use crate::transport::{
    LocalControlError, LocalControlLimits, LocalControlRequest, LocalSubscriptionId,
    LocalSubscriptionOperation, LocalSubscriptionRequest,
};

#[path = "local_subscription_codec/response.rs"]
mod response;

pub(super) use response::{decode_subscription_response, encode_subscription_response};

pub(super) fn encode_subscription_request(
    output: &mut Vec<u8>,
    request: &LocalSubscriptionRequest,
    limits: LocalControlLimits,
) -> Result<(), LocalControlError> {
    output.extend_from_slice(&request.request_id.to_be_bytes());
    match &request.operation {
        LocalSubscriptionOperation::Open {
            cursor,
            credit,
            lease_ms,
        } => {
            output.insert(5, TAG_SUBSCRIPTION_OPEN);
            validate_credit_and_lease(*credit, *lease_ms)?;
            append_cursor(output, cursor, limits, true)?;
            output.extend_from_slice(
                &u64::try_from(*credit)
                    .map_err(|_| LocalControlError::FrameTooLarge)?
                    .to_be_bytes(),
            );
            output.extend_from_slice(&lease_ms.to_be_bytes());
            output.extend_from_slice(cursor);
        }
        LocalSubscriptionOperation::Resume {
            lease,
            cursor,
            credit,
            lease_ms,
        } => {
            output.insert(5, TAG_SUBSCRIPTION_RESUME);
            validate_lease(*lease)?;
            validate_credit_and_lease(*credit, *lease_ms)?;
            output.extend_from_slice(&lease.as_bytes());
            append_cursor_header(output, cursor, limits, false)?;
            output.extend_from_slice(
                &u64::try_from(*credit)
                    .map_err(|_| LocalControlError::FrameTooLarge)?
                    .to_be_bytes(),
            );
            output.extend_from_slice(&lease_ms.to_be_bytes());
            output.extend_from_slice(cursor);
        }
        LocalSubscriptionOperation::Credit { lease, credit } => {
            output.insert(5, TAG_SUBSCRIPTION_CREDIT);
            validate_lease(*lease)?;
            validate_credit(*credit)?;
            output.extend_from_slice(&lease.as_bytes());
            output.extend_from_slice(
                &u64::try_from(*credit)
                    .map_err(|_| LocalControlError::FrameTooLarge)?
                    .to_be_bytes(),
            );
        }
        LocalSubscriptionOperation::Ack { lease, cursor } => {
            output.insert(5, TAG_SUBSCRIPTION_ACK);
            validate_lease(*lease)?;
            output.extend_from_slice(&lease.as_bytes());
            append_cursor_header(output, cursor, limits, false)?;
            output.extend_from_slice(cursor);
        }
        LocalSubscriptionOperation::Renew {
            lease,
            cursor,
            credit,
            lease_ms,
        } => {
            output.insert(5, TAG_SUBSCRIPTION_RENEW);
            validate_lease(*lease)?;
            validate_credit_and_lease(*credit, *lease_ms)?;
            output.extend_from_slice(&lease.as_bytes());
            append_cursor_header(output, cursor, limits, false)?;
            output.extend_from_slice(
                &u64::try_from(*credit)
                    .map_err(|_| LocalControlError::FrameTooLarge)?
                    .to_be_bytes(),
            );
            output.extend_from_slice(&lease_ms.to_be_bytes());
            output.extend_from_slice(cursor);
        }
        LocalSubscriptionOperation::Cancel { lease } => {
            output.insert(5, TAG_SUBSCRIPTION_CANCEL);
            validate_lease(*lease)?;
            output.extend_from_slice(&lease.as_bytes());
        }
        LocalSubscriptionOperation::Page {
            lease,
            page,
            credit,
        } => {
            output.insert(5, TAG_SUBSCRIPTION_PAGE);
            validate_lease(*lease)?;
            validate_credit(*credit)?;
            output.extend_from_slice(&lease.as_bytes());
            append_cursor_header(output, page, limits, true)?;
            output.extend_from_slice(
                &u64::try_from(*credit)
                    .map_err(|_| LocalControlError::FrameTooLarge)?
                    .to_be_bytes(),
            );
            output.extend_from_slice(page);
        }
    }
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "subscription grammar is kept contiguous"
)]
pub(super) fn decode_subscription_request(
    payload: &[u8],
    request_id: u64,
    limits: LocalControlLimits,
) -> Result<LocalControlRequest, LocalControlError> {
    let operation = match payload[5] {
        TAG_SUBSCRIPTION_OPEN => {
            if payload.len() < SUBSCRIPTION_OPEN_PREFIX_BYTES {
                return Err(LocalControlError::Truncated);
            }
            let cursor_len = read_u32(payload, 14, "subscription cursor length")?;
            let credit = usize::try_from(read_u64(payload, 18, "subscription credit")?)
                .map_err(|_| LocalControlError::FrameTooLarge)?;
            let lease_ms = read_u64(payload, 26, "subscription lease")?;
            validate_credit_and_lease(credit, lease_ms)?;
            let expected = SUBSCRIPTION_OPEN_PREFIX_BYTES
                .checked_add(cursor_len)
                .ok_or(LocalControlError::FrameTooLarge)?;
            if cursor_len > limits.max_cursor {
                return Err(LocalControlError::Invalid("subscription cursor bounds"));
            }
            if payload.len() != expected {
                return Err(size_error(payload.len(), expected));
            }
            LocalSubscriptionOperation::Open {
                cursor: payload[34..].to_vec().into_boxed_slice(),
                credit,
                lease_ms,
            }
        }
        TAG_SUBSCRIPTION_RESUME => {
            if payload.len() < SUBSCRIPTION_RESUME_PREFIX_BYTES {
                return Err(LocalControlError::Truncated);
            }
            let lease = read_lease(payload, 14)?;
            validate_lease(lease)?;
            let cursor_len = read_u32(payload, 30, "subscription cursor length")?;
            let credit = usize::try_from(read_u64(payload, 34, "subscription credit")?)
                .map_err(|_| LocalControlError::FrameTooLarge)?;
            let lease_ms = read_u64(payload, 42, "subscription lease")?;
            validate_credit_and_lease(credit, lease_ms)?;
            let expected = SUBSCRIPTION_RESUME_PREFIX_BYTES
                .checked_add(cursor_len)
                .ok_or(LocalControlError::FrameTooLarge)?;
            if cursor_len == 0 || cursor_len > limits.max_cursor {
                return Err(LocalControlError::Invalid("subscription cursor bounds"));
            }
            if payload.len() != expected {
                return Err(size_error(payload.len(), expected));
            }
            LocalSubscriptionOperation::Resume {
                lease,
                cursor: payload[50..].to_vec().into_boxed_slice(),
                credit,
                lease_ms,
            }
        }
        TAG_SUBSCRIPTION_CREDIT => {
            if payload.len() != SUBSCRIPTION_CREDIT_BYTES {
                return Err(size_error(payload.len(), SUBSCRIPTION_CREDIT_BYTES));
            }
            let lease = read_lease(payload, 14)?;
            validate_lease(lease)?;
            let credit = usize::try_from(read_u64(payload, 30, "subscription credit")?)
                .map_err(|_| LocalControlError::FrameTooLarge)?;
            validate_credit(credit)?;
            LocalSubscriptionOperation::Credit { lease, credit }
        }
        TAG_SUBSCRIPTION_ACK => {
            if payload.len() < SUBSCRIPTION_ACK_PREFIX_BYTES {
                return Err(LocalControlError::Truncated);
            }
            let lease = read_lease(payload, 14)?;
            validate_lease(lease)?;
            let cursor_len = read_u32(payload, 30, "subscription cursor length")?;
            let expected = SUBSCRIPTION_ACK_PREFIX_BYTES
                .checked_add(cursor_len)
                .ok_or(LocalControlError::FrameTooLarge)?;
            if cursor_len == 0 || cursor_len > limits.max_cursor {
                return Err(LocalControlError::Invalid("subscription cursor bounds"));
            }
            if payload.len() != expected {
                return Err(size_error(payload.len(), expected));
            }
            LocalSubscriptionOperation::Ack {
                lease,
                cursor: payload[34..].to_vec().into_boxed_slice(),
            }
        }
        TAG_SUBSCRIPTION_RENEW => {
            if payload.len() < SUBSCRIPTION_RENEW_PREFIX_BYTES {
                return Err(LocalControlError::Truncated);
            }
            let lease = read_lease(payload, 14)?;
            validate_lease(lease)?;
            let cursor_len = read_u32(payload, 30, "subscription cursor length")?;
            let credit = usize::try_from(read_u64(payload, 34, "subscription credit")?)
                .map_err(|_| LocalControlError::FrameTooLarge)?;
            let lease_ms = read_u64(payload, 42, "subscription lease")?;
            validate_credit_and_lease(credit, lease_ms)?;
            let expected = SUBSCRIPTION_RENEW_PREFIX_BYTES
                .checked_add(cursor_len)
                .ok_or(LocalControlError::FrameTooLarge)?;
            if cursor_len == 0 || cursor_len > limits.max_cursor {
                return Err(LocalControlError::Invalid("subscription cursor bounds"));
            }
            if payload.len() != expected {
                return Err(size_error(payload.len(), expected));
            }
            LocalSubscriptionOperation::Renew {
                lease,
                cursor: payload[50..].to_vec().into_boxed_slice(),
                credit,
                lease_ms,
            }
        }
        TAG_SUBSCRIPTION_CANCEL => {
            if payload.len() != SUBSCRIPTION_CANCEL_BYTES {
                return Err(size_error(payload.len(), SUBSCRIPTION_CANCEL_BYTES));
            }
            let lease = read_lease(payload, 14)?;
            validate_lease(lease)?;
            LocalSubscriptionOperation::Cancel { lease }
        }
        TAG_SUBSCRIPTION_PAGE => {
            if payload.len() < SUBSCRIPTION_PAGE_PREFIX_BYTES {
                return Err(LocalControlError::Truncated);
            }
            let lease = read_lease(payload, 14)?;
            validate_lease(lease)?;
            let page_len = read_u32(payload, 30, "subscription page cursor length")?;
            let credit = usize::try_from(read_u64(payload, 34, "subscription credit")?)
                .map_err(|_| LocalControlError::FrameTooLarge)?;
            validate_credit(credit)?;
            let expected = SUBSCRIPTION_PAGE_PREFIX_BYTES
                .checked_add(page_len)
                .ok_or(LocalControlError::FrameTooLarge)?;
            if page_len > limits.max_cursor {
                return Err(LocalControlError::Invalid(
                    "subscription page cursor bounds",
                ));
            }
            if payload.len() != expected {
                return Err(size_error(payload.len(), expected));
            }
            LocalSubscriptionOperation::Page {
                lease,
                page: payload[42..].to_vec().into_boxed_slice(),
                credit,
            }
        }
        _ => return Err(LocalControlError::Invalid("subscription operation tag")),
    };
    Ok(LocalControlRequest::Subscription(
        LocalSubscriptionRequest {
            request_id,
            operation,
        },
    ))
}

fn append_cursor(
    output: &mut Vec<u8>,
    cursor: &[u8],
    limits: LocalControlLimits,
    allow_empty: bool,
) -> Result<(), LocalControlError> {
    append_cursor_header(output, cursor, limits, allow_empty)
}

fn append_cursor_header(
    output: &mut Vec<u8>,
    cursor: &[u8],
    limits: LocalControlLimits,
    allow_empty: bool,
) -> Result<(), LocalControlError> {
    if (!allow_empty && cursor.is_empty()) || cursor.len() > limits.max_cursor {
        return Err(LocalControlError::Invalid("subscription cursor bounds"));
    }
    output.extend_from_slice(
        &u32::try_from(cursor.len())
            .map_err(|_| LocalControlError::FrameTooLarge)?
            .to_be_bytes(),
    );
    Ok(())
}

fn validate_credit(credit: usize) -> Result<(), LocalControlError> {
    if credit == 0 {
        return Err(LocalControlError::Invalid("subscription credit"));
    }
    Ok(())
}

fn validate_credit_and_lease(credit: usize, lease_ms: u64) -> Result<(), LocalControlError> {
    validate_credit(credit)?;
    if lease_ms == 0 {
        return Err(LocalControlError::Invalid("subscription lease"));
    }
    Ok(())
}

fn validate_lease(lease: LocalSubscriptionId) -> Result<(), LocalControlError> {
    if lease.is_zero() {
        return Err(LocalControlError::Invalid("subscription lease id"));
    }
    Ok(())
}

fn read_lease(payload: &[u8], start: usize) -> Result<LocalSubscriptionId, LocalControlError> {
    let end = start
        .checked_add(SUBSCRIPTION_ID_BYTES)
        .ok_or(LocalControlError::FrameTooLarge)?;
    let bytes = payload
        .get(start..end)
        .ok_or(LocalControlError::Truncated)?;
    Ok(LocalSubscriptionId::from_bytes(
        bytes.try_into().map_err(|_| LocalControlError::Truncated)?,
    ))
}
