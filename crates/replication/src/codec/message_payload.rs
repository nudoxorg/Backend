//! Typed payload codec facade for the replication message families.
//!
//! This module owns the one bounded field grammar below the RPL2 envelope.
//! Family implementations are split into focused modules; the envelope and
//! tag dispatch remain centralized in `message.rs`.

use super::{
    HEADER_BYTES, TAG_CANCEL_ATTEMPT, TAG_CAPABILITIES, TAG_CHUNK, TAG_CLOSURE_NEED_REQUEST,
    TAG_CLOSURE_PAGE_REQUEST, TAG_CLOSURE_PAGE_RESPONSE, TAG_CLOSURE_ROOT_ACK,
    TAG_CLOSURE_ROOT_OFFER, TAG_NODE_REQUEST, TAG_PACK, TAG_RANGE_REQUEST, TAG_RECIPE_REQUEST,
    TAG_RECIPE_RESULT, TAG_RESUME_REQUEST, TAG_ROOT_SUMMARY,
};
use crate::codec::primitives::{Reader, Writer};
use crate::{ReplicationError, TransportLimits, TransportMessage};

mod closure_payload;
mod execution_payload;
mod pack_payload;
mod summary_payload;
mod transfer_payload;
pub(super) fn encode_payload(
    message: &TransportMessage,
    limits: TransportLimits,
) -> Result<(u8, Vec<u8>), ReplicationError> {
    let mut writer = Writer::new(limits.max_frame.saturating_sub(HEADER_BYTES));
    let tag = match message {
        TransportMessage::WirePack(value) => {
            pack_payload::write_pack(&mut writer, value)?;
            TAG_PACK
        }
        TransportMessage::Capabilities(value) => {
            pack_payload::write_capabilities(&mut writer, value)?;
            TAG_CAPABILITIES
        }
        TransportMessage::RootSummary(value) => {
            let wire = value.to_wire()?;
            summary_payload::write_root_summary(&mut writer, &wire)?;
            TAG_ROOT_SUMMARY
        }
        TransportMessage::WireRootSummary(value) => {
            summary_payload::write_root_summary(&mut writer, value)?;
            TAG_ROOT_SUMMARY
        }
        TransportMessage::NodeRequest(value) => {
            let wire = value.to_wire()?;
            transfer_payload::write_node_request(&mut writer, &wire)?;
            TAG_NODE_REQUEST
        }
        TransportMessage::WireNodeRequest(value) => {
            transfer_payload::write_node_request(&mut writer, value)?;
            TAG_NODE_REQUEST
        }
        TransportMessage::RangeRequest(value) => {
            let wire = value.to_wire()?;
            transfer_payload::write_range_request(&mut writer, &wire)?;
            TAG_RANGE_REQUEST
        }
        TransportMessage::WireRangeRequest(value) => {
            transfer_payload::write_range_request(&mut writer, value)?;
            TAG_RANGE_REQUEST
        }
        TransportMessage::Chunk(value) => {
            transfer_payload::write_frame(&mut writer, value)?;
            TAG_CHUNK
        }
        TransportMessage::Resume(value) => {
            let wire = value.to_wire()?;
            transfer_payload::write_resume_request(&mut writer, &wire)?;
            TAG_RESUME_REQUEST
        }
        TransportMessage::WireResumeRequest(value) => {
            transfer_payload::write_resume_request(&mut writer, value)?;
            TAG_RESUME_REQUEST
        }
        TransportMessage::WireRecipeRequest(value) => {
            execution_payload::write_recipe_request(&mut writer, value)?;
            TAG_RECIPE_REQUEST
        }
        TransportMessage::WireRecipeResult(value) => {
            execution_payload::write_recipe_result(&mut writer, value)?;
            TAG_RECIPE_RESULT
        }
        TransportMessage::CancelAttempt(value) => {
            execution_payload::write_cancel_attempt(&mut writer, value)?;
            TAG_CANCEL_ATTEMPT
        }
        TransportMessage::ClosurePageRequest(value) => {
            closure_payload::write_closure_page_request(&mut writer, value)?;
            TAG_CLOSURE_PAGE_REQUEST
        }
        TransportMessage::ClosurePageResponse(value) => {
            closure_payload::write_closure_page_response(&mut writer, value)?;
            TAG_CLOSURE_PAGE_RESPONSE
        }
        TransportMessage::ClosureRootOffer(value) => {
            closure_payload::write_closure_root_offer(&mut writer, value)?;
            TAG_CLOSURE_ROOT_OFFER
        }
        TransportMessage::ClosureRootAck(value) => {
            closure_payload::write_closure_root_ack(&mut writer, value)?;
            TAG_CLOSURE_ROOT_ACK
        }
        TransportMessage::ClosureNeedRequest(value) => {
            closure_payload::write_closure_need_request(&mut writer, value)?;
            TAG_CLOSURE_NEED_REQUEST
        }
    };
    Ok((tag, writer.finish()))
}

pub(super) fn decode_payload(
    tag: u8,
    reader: &mut Reader<'_>,
    limits: TransportLimits,
) -> Result<TransportMessage, ReplicationError> {
    match tag {
        TAG_PACK => Ok(TransportMessage::WirePack(pack_payload::read_pack(
            reader, limits,
        )?)),
        TAG_CAPABILITIES => Ok(TransportMessage::Capabilities(
            pack_payload::read_capabilities(reader, limits)?,
        )),
        TAG_ROOT_SUMMARY => Ok(TransportMessage::WireRootSummary(
            summary_payload::read_root_summary(reader, limits)?,
        )),
        TAG_NODE_REQUEST => Ok(TransportMessage::WireNodeRequest(
            transfer_payload::read_node_request(reader, limits)?,
        )),
        TAG_RANGE_REQUEST => Ok(TransportMessage::WireRangeRequest(
            transfer_payload::read_range_request(reader, limits)?,
        )),
        TAG_CHUNK => Ok(TransportMessage::Chunk(transfer_payload::read_frame(
            reader, limits,
        )?)),
        TAG_RESUME_REQUEST => Ok(TransportMessage::WireResumeRequest(
            transfer_payload::read_resume_request(reader, limits)?,
        )),
        TAG_RECIPE_REQUEST => Ok(TransportMessage::WireRecipeRequest(
            execution_payload::read_recipe_request(reader, limits)?,
        )),
        TAG_RECIPE_RESULT => Ok(TransportMessage::WireRecipeResult(Box::new(
            execution_payload::read_recipe_result(reader, limits)?,
        ))),
        TAG_CANCEL_ATTEMPT => Ok(TransportMessage::CancelAttempt(
            execution_payload::read_cancel_attempt(reader)?,
        )),
        TAG_CLOSURE_PAGE_REQUEST => Ok(TransportMessage::ClosurePageRequest(
            closure_payload::read_closure_page_request(reader, limits)?,
        )),
        TAG_CLOSURE_PAGE_RESPONSE => Ok(TransportMessage::ClosurePageResponse(
            closure_payload::read_closure_page_response(reader, limits)?,
        )),
        TAG_CLOSURE_ROOT_OFFER => Ok(TransportMessage::ClosureRootOffer(
            closure_payload::read_closure_root_offer(reader, limits)?,
        )),
        TAG_CLOSURE_ROOT_ACK => Ok(TransportMessage::ClosureRootAck(
            closure_payload::read_closure_root_ack(reader, limits)?,
        )),
        TAG_CLOSURE_NEED_REQUEST => Ok(TransportMessage::ClosureNeedRequest(
            closure_payload::read_closure_need_request(reader)?,
        )),
        _ => Err(ReplicationError::UnknownMessage),
    }
}
