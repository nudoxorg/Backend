//! Canonical local control payload codec.

use super::{
    COMPLETE_BYTES, LOCAL_CONTROL_HEADER_BYTES, LOCAL_CONTROL_MAGIC, LOCAL_CONTROL_VERSION,
    LocalControlError, LocalControlLimits, LocalControlRequest, LocalControlResponse,
    REPLICATE_PREFIX_BYTES, STATUS_ACCEPTED, STATUS_QUEUED, STATUS_REJECTED, STATUS_SUBSCRIPTION,
    SUBSCRIBE_PREFIX_BYTES, TAG_COMPLETE, TAG_REPLICATE, TAG_SUBSCRIBE, TAG_SUBSCRIPTION_ACK,
    TAG_SUBSCRIPTION_CANCEL, TAG_SUBSCRIPTION_CREDIT, TAG_SUBSCRIPTION_OPEN, TAG_SUBSCRIPTION_PAGE,
    TAG_SUBSCRIPTION_RENEW, TAG_SUBSCRIPTION_RESUME, is_control,
};
use std::borrow::Cow;
#[path = "../transport/local_subscription_codec.rs"]
mod subscription_codec;
use subscription_codec::{
    decode_subscription_request, decode_subscription_response, encode_subscription_request,
    encode_subscription_response,
};

/// Encodes one raw local control request.
///
/// # Errors
///
/// Returns an error when the request tag, cursor, payload, or enclosing frame
/// exceeds the supplied bounds.
pub fn encode_request(
    request: &LocalControlRequest,
    limits: LocalControlLimits,
) -> Result<Vec<u8>, LocalControlError> {
    limits.validate()?;
    let mut output = Vec::new();
    output.extend_from_slice(&LOCAL_CONTROL_MAGIC);
    output.push(LOCAL_CONTROL_VERSION);
    match request {
        LocalControlRequest::Replicate {
            request_id,
            payload,
        } => {
            output.push(TAG_REPLICATE);
            output.extend_from_slice(&request_id.to_be_bytes());
            let length =
                u32::try_from(payload.len()).map_err(|_| LocalControlError::FrameTooLarge)?;
            output.extend_from_slice(&length.to_be_bytes());
            output.extend_from_slice(payload);
        }
        LocalControlRequest::Complete {
            request_id,
            work_key,
            output: output_identity,
            ordinal,
            fence,
        } => {
            output.push(TAG_COMPLETE);
            output.extend_from_slice(&request_id.to_be_bytes());
            output.extend_from_slice(work_key);
            output.extend_from_slice(output_identity);
            output.extend_from_slice(&ordinal.to_be_bytes());
            output.extend_from_slice(fence);
        }
        LocalControlRequest::Subscribe {
            request_id,
            cursor,
            credit,
        } => {
            if *credit == 0 || cursor.len() > limits.max_cursor {
                return Err(LocalControlError::Invalid("subscription bounds"));
            }
            output.push(TAG_SUBSCRIBE);
            output.extend_from_slice(&request_id.to_be_bytes());
            let length =
                u32::try_from(cursor.len()).map_err(|_| LocalControlError::FrameTooLarge)?;
            output.extend_from_slice(&length.to_be_bytes());
            output.extend_from_slice(
                &u64::try_from(*credit)
                    .map_err(|_| LocalControlError::FrameTooLarge)?
                    .to_be_bytes(),
            );
            output.extend_from_slice(cursor);
        }
        LocalControlRequest::Subscription(request) => {
            encode_subscription_request(&mut output, request, limits)?;
        }
    }
    if output.len() > limits.max_frame {
        return Err(LocalControlError::FrameTooLarge);
    }
    Ok(output)
}

/// Decodes one raw local control request after strict length/version checks.
///
/// # Errors
///
/// Returns an error for a wrong magic/version/tag, malformed nested lengths,
/// or an oversized payload.
pub fn decode_request(
    payload: &[u8],
    limits: LocalControlLimits,
) -> Result<LocalControlRequest, LocalControlError> {
    limits.validate()?;
    if payload.len() < LOCAL_CONTROL_HEADER_BYTES || !is_control(payload) {
        return Err(LocalControlError::Invalid("request magic"));
    }
    if payload[4] != LOCAL_CONTROL_VERSION {
        return Err(LocalControlError::Invalid("request version"));
    }
    let request_id = u64::from_be_bytes(
        payload[6..14]
            .try_into()
            .map_err(|_| LocalControlError::Truncated)?,
    );
    match payload[5] {
        TAG_REPLICATE => {
            if payload.len() < REPLICATE_PREFIX_BYTES {
                return Err(LocalControlError::Truncated);
            }
            let length = read_u32(payload, 14, "replication length")?;
            if length > limits.max_frame {
                return Err(LocalControlError::FrameTooLarge);
            }
            let expected = REPLICATE_PREFIX_BYTES
                .checked_add(length)
                .ok_or(LocalControlError::FrameTooLarge)?;
            if payload.len() != expected {
                return Err(size_error(payload.len(), expected));
            }
            Ok(LocalControlRequest::Replicate {
                request_id,
                payload: payload[18..].to_vec().into_boxed_slice(),
            })
        }
        TAG_COMPLETE => {
            if payload.len() != COMPLETE_BYTES {
                return Err(size_error(payload.len(), COMPLETE_BYTES));
            }
            Ok(LocalControlRequest::Complete {
                request_id,
                work_key: payload[14..46]
                    .try_into()
                    .map_err(|_| LocalControlError::Truncated)?,
                output: payload[46..78]
                    .try_into()
                    .map_err(|_| LocalControlError::Truncated)?,
                ordinal: u32::from_be_bytes(
                    payload[78..82]
                        .try_into()
                        .map_err(|_| LocalControlError::Truncated)?,
                ),
                fence: payload[82..114]
                    .try_into()
                    .map_err(|_| LocalControlError::Truncated)?,
            })
        }
        TAG_SUBSCRIBE => {
            if payload.len() < SUBSCRIBE_PREFIX_BYTES {
                return Err(LocalControlError::Truncated);
            }
            let cursor_len = read_u32(payload, 14, "cursor length")?;
            let credit = usize::try_from(read_u64(payload, 18, "subscription credit")?)
                .map_err(|_| LocalControlError::FrameTooLarge)?;
            if credit == 0 || cursor_len > limits.max_cursor {
                return Err(LocalControlError::Invalid("subscription bounds"));
            }
            let expected = SUBSCRIBE_PREFIX_BYTES
                .checked_add(cursor_len)
                .ok_or(LocalControlError::FrameTooLarge)?;
            if payload.len() != expected {
                return Err(size_error(payload.len(), expected));
            }
            Ok(LocalControlRequest::Subscribe {
                request_id,
                cursor: payload[26..].to_vec().into_boxed_slice(),
                credit,
            })
        }
        TAG_SUBSCRIPTION_OPEN
        | TAG_SUBSCRIPTION_RESUME
        | TAG_SUBSCRIPTION_CREDIT
        | TAG_SUBSCRIPTION_ACK
        | TAG_SUBSCRIPTION_RENEW
        | TAG_SUBSCRIPTION_CANCEL
        | TAG_SUBSCRIPTION_PAGE => decode_subscription_request(payload, request_id, limits),
        _ => Err(LocalControlError::Invalid("request operation tag")),
    }
}

/// Encodes one raw local control response.
///
/// # Errors
///
/// Returns an error when a payload, diagnostic, queue count, or enclosing
/// response exceeds the supplied bounds.
pub fn encode_response(
    response: &LocalControlResponse,
    limits: LocalControlLimits,
) -> Result<Vec<u8>, LocalControlError> {
    limits.validate()?;
    let mut output = Vec::with_capacity(LOCAL_CONTROL_HEADER_BYTES + 4 + 8);
    output.extend_from_slice(&LOCAL_CONTROL_MAGIC);
    output.push(LOCAL_CONTROL_VERSION);
    let (status, queued_bytes, payload) = match response {
        LocalControlResponse::Accepted { .. } => (STATUS_ACCEPTED, None, Cow::Borrowed(&[][..])),
        LocalControlResponse::AcceptedPayload { payload, .. } => {
            (STATUS_ACCEPTED, None, Cow::Borrowed(payload.as_ref()))
        }
        LocalControlResponse::Queued { bytes, .. } => (
            STATUS_QUEUED,
            Some(u64::try_from(*bytes).map_err(|_| LocalControlError::FrameTooLarge)?),
            Cow::Borrowed(&[][..]),
        ),
        LocalControlResponse::Rejected { message, .. } => {
            if message.len() > limits.max_error {
                return Err(LocalControlError::FrameTooLarge);
            }
            (STATUS_REJECTED, None, Cow::Borrowed(message.as_bytes()))
        }
        LocalControlResponse::Subscription(response) => (
            STATUS_SUBSCRIPTION,
            None,
            Cow::Owned(encode_subscription_response(response, limits)?),
        ),
    };
    output.push(status);
    output.extend_from_slice(&response.request_id().to_be_bytes());
    let length = u32::try_from(payload.len()).map_err(|_| LocalControlError::FrameTooLarge)?;
    output.extend_from_slice(&length.to_be_bytes());
    if let Some(bytes) = queued_bytes {
        output.extend_from_slice(&bytes.to_be_bytes());
    }
    output.extend_from_slice(payload.as_ref());
    if output.len() > limits.max_frame {
        return Err(LocalControlError::FrameTooLarge);
    }
    Ok(output)
}

/// Decodes one raw local control response.
///
/// # Errors
///
/// Returns an error for a wrong magic/version/status, malformed lengths,
/// invalid UTF-8 diagnostics, or a response outside the configured bounds.
pub fn decode_response(
    payload: &[u8],
    limits: LocalControlLimits,
) -> Result<LocalControlResponse, LocalControlError> {
    limits.validate()?;
    if payload.len() < LOCAL_CONTROL_HEADER_BYTES {
        return Err(LocalControlError::Truncated);
    }
    if !is_control(payload) {
        return Err(LocalControlError::Invalid("response magic"));
    }
    if payload[4] != LOCAL_CONTROL_VERSION {
        return Err(LocalControlError::Invalid("response version"));
    }
    if payload.len() < LOCAL_CONTROL_HEADER_BYTES + 4 {
        return Err(LocalControlError::Truncated);
    }
    let request_id = u64::from_be_bytes(
        payload[6..14]
            .try_into()
            .map_err(|_| LocalControlError::Truncated)?,
    );
    let length = read_u32(payload, 14, "response length")?;
    if length > limits.max_frame {
        return Err(LocalControlError::FrameTooLarge);
    }
    let queued_prefix = match payload[5] {
        STATUS_ACCEPTED | STATUS_REJECTED | STATUS_SUBSCRIPTION => 0,
        STATUS_QUEUED => 8,
        _ => return Err(LocalControlError::Invalid("response status")),
    };
    let expected = LOCAL_CONTROL_HEADER_BYTES
        .checked_add(4)
        .and_then(|value| value.checked_add(queued_prefix))
        .and_then(|value| value.checked_add(length))
        .ok_or(LocalControlError::FrameTooLarge)?;
    if payload.len() != expected {
        return Err(size_error(payload.len(), expected));
    }
    match payload[5] {
        STATUS_ACCEPTED => {
            let start = LOCAL_CONTROL_HEADER_BYTES + 4;
            Ok(if length == 0 {
                LocalControlResponse::Accepted { request_id }
            } else {
                LocalControlResponse::AcceptedPayload {
                    request_id,
                    payload: payload[start..].to_vec().into_boxed_slice(),
                }
            })
        }
        STATUS_REJECTED => {
            if length > limits.max_error {
                return Err(LocalControlError::FrameTooLarge);
            }
            let start = LOCAL_CONTROL_HEADER_BYTES + 4;
            let message = std::str::from_utf8(&payload[start..])
                .map_err(|_| LocalControlError::InvalidUtf8)?
                .to_owned();
            Ok(LocalControlResponse::Rejected {
                request_id,
                message,
            })
        }
        STATUS_QUEUED => {
            if length != 0 {
                return Err(LocalControlError::Invalid("queued response payload"));
            }
            let bytes = usize::try_from(read_u64(payload, 18, "queued response bytes")?)
                .map_err(|_| LocalControlError::FrameTooLarge)?;
            Ok(LocalControlResponse::Queued { request_id, bytes })
        }
        STATUS_SUBSCRIPTION => {
            let start = LOCAL_CONTROL_HEADER_BYTES + 4;
            let response = decode_subscription_response(&payload[start..], request_id, limits)?;
            Ok(LocalControlResponse::Subscription(response))
        }
        _ => Err(LocalControlError::Invalid("response status")),
    }
}

fn read_u32(payload: &[u8], start: usize, field: &'static str) -> Result<usize, LocalControlError> {
    let end = start
        .checked_add(4)
        .ok_or(LocalControlError::FrameTooLarge)?;
    let bytes = payload
        .get(start..end)
        .ok_or(LocalControlError::Truncated)?;
    usize::try_from(u32::from_be_bytes(
        bytes.try_into().map_err(|_| LocalControlError::Truncated)?,
    ))
    .map_err(|_| LocalControlError::Invalid(field))
}

fn read_u64(payload: &[u8], start: usize, field: &'static str) -> Result<u64, LocalControlError> {
    let end = start
        .checked_add(8)
        .ok_or(LocalControlError::FrameTooLarge)?;
    let bytes = payload
        .get(start..end)
        .ok_or(LocalControlError::Truncated)?;
    let bytes: [u8; 8] = bytes
        .try_into()
        .map_err(|_| LocalControlError::Invalid(field))?;
    Ok(u64::from_be_bytes(bytes))
}

fn size_error(actual: usize, expected: usize) -> LocalControlError {
    if actual < expected {
        LocalControlError::Truncated
    } else {
        LocalControlError::Trailing
    }
}
