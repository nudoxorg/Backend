//! Shared bounded framing for local process/control endpoints.
//!
//! The local daemon, desktop subscription client, CLI, and MCP client use the
//! same four-byte outer frame and `LDC2` control envelope.  This module owns
//! the byte grammar and all length/version checks.  Higher layers convert the
//! raw control payloads into their typed request/status enums and map this
//! small error algebra into their own transport errors.

/// Current local control envelope version.
pub const LOCAL_CONTROL_VERSION: u8 = 1;
/// Default maximum body bytes in one local length-prefixed frame.
/// Local control frames carry independently checkable relation preimages.
/// Four MiB keeps complete small/medium project snapshots single-copy while
/// larger views still use the paged subscription protocol.
pub const LOCAL_CONTROL_MAX_FRAME: usize = 4 * 1024 * 1024;
/// Bytes occupied by the fixed local control header.
pub const LOCAL_CONTROL_HEADER_BYTES: usize = 4 + 1 + 1 + 8;
/// Default maximum cursor bytes accepted by a local control request.
pub const LOCAL_CONTROL_MAX_CURSOR: usize = 8 * 1024;
/// Default maximum UTF-8 diagnostic bytes returned by a local control reply.
pub const LOCAL_CONTROL_MAX_ERROR: usize = 8 * 1024;
/// Maximum number of out-of-order responses retained by one local client.
///
/// A peer may answer several independent requests before the caller asks for
/// one particular correlation ID.  Retaining a bounded set makes that useful
/// for multiplexed clients without turning response correlation into an
/// unbounded allocation attack.
pub const LOCAL_CONTROL_MAX_PENDING: usize = 256;
/// Maximum encoded response bytes retained by one local client mailbox.
///
/// The count cap alone is insufficient because one response may approach the
/// frame limit. This independent byte cap keeps retention bounded when a peer
/// sends large out-of-order subscription pages.
pub const LOCAL_CONTROL_MAX_PENDING_BYTES: usize = 8 * 1024 * 1024;

const TAG_REPLICATE: u8 = 1;
const TAG_COMPLETE: u8 = 2;
const TAG_SUBSCRIBE: u8 = 3;
const TAG_SUBSCRIPTION_OPEN: u8 = 4;
const TAG_SUBSCRIPTION_RESUME: u8 = 5;
const TAG_SUBSCRIPTION_CREDIT: u8 = 6;
const TAG_SUBSCRIPTION_ACK: u8 = 7;
const TAG_SUBSCRIPTION_RENEW: u8 = 8;
const TAG_SUBSCRIPTION_CANCEL: u8 = 9;
const TAG_SUBSCRIPTION_PAGE: u8 = 10;
const STATUS_ACCEPTED: u8 = 0;
const STATUS_REJECTED: u8 = 1;
const STATUS_QUEUED: u8 = 2;
const STATUS_SUBSCRIPTION: u8 = 3;
const SUBSCRIPTION_OPENED: u8 = 1;
const SUBSCRIPTION_RESUMED: u8 = 2;
const SUBSCRIPTION_BATCH: u8 = 3;
const SUBSCRIPTION_RESET: u8 = 4;
const SUBSCRIPTION_ACKED: u8 = 5;
const SUBSCRIPTION_RENEWED: u8 = 6;
const SUBSCRIPTION_CANCELLED: u8 = 7;
const SUBSCRIPTION_PAGE: u8 = 8;
const RESET_GAP: u8 = 0;
const RESET_BRANCH_DISCARDED: u8 = 1;
const RESET_PRUNED: u8 = 2;
const RESET_SCHEMA_MISMATCH: u8 = 3;
const RESET_ROOT_MISMATCH: u8 = 4;
const REPLICATE_PREFIX_BYTES: usize = LOCAL_CONTROL_HEADER_BYTES + 4;
const SUBSCRIBE_PREFIX_BYTES: usize = LOCAL_CONTROL_HEADER_BYTES + 4 + 8;
const COMPLETE_BYTES: usize = LOCAL_CONTROL_HEADER_BYTES + 32 + 32 + 4 + 32;
const SUBSCRIPTION_ID_BYTES: usize = 16;
const SUBSCRIPTION_OPEN_PREFIX_BYTES: usize = LOCAL_CONTROL_HEADER_BYTES + 4 + 8 + 8;
const SUBSCRIPTION_RESUME_PREFIX_BYTES: usize =
    LOCAL_CONTROL_HEADER_BYTES + SUBSCRIPTION_ID_BYTES + 4 + 8 + 8;
const SUBSCRIPTION_CREDIT_BYTES: usize = LOCAL_CONTROL_HEADER_BYTES + SUBSCRIPTION_ID_BYTES + 8;
const SUBSCRIPTION_ACK_PREFIX_BYTES: usize = LOCAL_CONTROL_HEADER_BYTES + SUBSCRIPTION_ID_BYTES + 4;
const SUBSCRIPTION_RENEW_PREFIX_BYTES: usize =
    LOCAL_CONTROL_HEADER_BYTES + SUBSCRIPTION_ID_BYTES + 4 + 8 + 8;
const SUBSCRIPTION_CANCEL_BYTES: usize = LOCAL_CONTROL_HEADER_BYTES + SUBSCRIPTION_ID_BYTES;
const SUBSCRIPTION_PAGE_PREFIX_BYTES: usize =
    LOCAL_CONTROL_HEADER_BYTES + SUBSCRIPTION_ID_BYTES + 4 + 8;

#[path = "../transport/local_types.rs"]
mod local_types;
pub use local_types::{
    LocalControlError, LocalControlLimits, LocalControlRequest, LocalControlResponse,
    LocalSubscriptionId, LocalSubscriptionOperation, LocalSubscriptionRequest,
    LocalSubscriptionResetReason, LocalSubscriptionResponse,
};

#[path = "../transport/client.rs"]
mod client;
pub use client::LocalControlClient;

#[path = "../transport/local_codec.rs"]
mod local_codec;
pub use local_codec::{decode_request, decode_response, encode_request, encode_response};

#[path = "../transport/local_frame.rs"]
mod local_frame;
pub use local_frame::{
    LOCAL_CONTROL_MAGIC, control_request_id, frame, is_control, read_frame, read_frame_into,
    unframe, write_frame,
};

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{self, Cursor, Read, Write};

    fn limits() -> LocalControlLimits {
        LocalControlLimits {
            max_frame: 4096,
            max_cursor: 512,
            max_error: 128,
        }
    }

    fn lease() -> LocalSubscriptionId {
        LocalSubscriptionId::from_bytes([0x7a; SUBSCRIPTION_ID_BYTES])
    }

    fn cursor() -> Box<[u8]> {
        Box::from(*b"cursor-v1")
    }

    #[derive(Debug)]
    struct ChunkedScript {
        input: Cursor<Vec<u8>>,
        writes: Vec<u8>,
        chunk: usize,
    }

    impl Read for ChunkedScript {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            let amount = output.len().min(self.chunk.max(1));
            self.input.read(&mut output[..amount])
        }
    }

    impl Write for ChunkedScript {
        fn write(&mut self, input: &[u8]) -> io::Result<usize> {
            self.writes.extend_from_slice(input);
            Ok(input.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn every_surface_uses_one_canonical_frame_and_control_bytes() {
        let request = LocalControlRequest::Subscribe {
            request_id: 7,
            cursor: Box::from(*b"cursor"),
            credit: 3,
        };
        let payload = encode_request(&request, limits()).expect("request");
        assert_eq!(&payload[..4], b"LDC2");
        assert_eq!(control_request_id(&payload), Some(7));
        assert_eq!(decode_request(&payload, limits()).expect("decode"), request);
        let framed = frame(&payload, limits()).expect("frame");
        assert_eq!(unframe(&framed, limits()).expect("unframe"), payload);
        let mut reader = Cursor::new(framed);
        assert_eq!(read_frame(&mut reader, limits()).expect("read"), payload);
        let mut reused = vec![0_u8; 128];
        let mut reader = Cursor::new(frame(b"next", limits()).expect("frame"));
        read_frame_into(&mut reader, &mut reused, limits()).expect("read into reused");
        assert_eq!(reused, b"next");
    }

    #[test]
    fn malformed_lengths_versions_and_statuses_fail_before_slicing() {
        let limits = limits();
        assert_eq!(
            unframe(&[0, 0, 0, 5, b'h'], limits),
            Err(LocalControlError::Truncated)
        );
        assert_eq!(
            unframe(&[0, 0, 0, 1, b'h', b'x'], limits),
            Err(LocalControlError::Trailing)
        );
        let mut request = encode_request(
            &LocalControlRequest::Subscribe {
                request_id: 1,
                cursor: Box::new([]),
                credit: 1,
            },
            limits,
        )
        .expect("request");
        request[4] = LOCAL_CONTROL_VERSION + 1;
        assert_eq!(control_request_id(&request), Some(1));
        assert_eq!(
            decode_request(&request, limits),
            Err(LocalControlError::Invalid("request version"))
        );
        request[4] = LOCAL_CONTROL_VERSION;
        request[5] = 0xff;
        assert_eq!(
            decode_request(&request, limits),
            Err(LocalControlError::Invalid("request operation tag"))
        );
        let mut oversized_request = vec![0_u8; LOCAL_CONTROL_HEADER_BYTES + 4];
        oversized_request[..4].copy_from_slice(&LOCAL_CONTROL_MAGIC);
        oversized_request[4] = LOCAL_CONTROL_VERSION;
        oversized_request[5] = TAG_REPLICATE;
        oversized_request[14..18].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(
            decode_request(&oversized_request, limits),
            Err(LocalControlError::FrameTooLarge)
        );
        let response = encode_response(
            &LocalControlResponse::Queued {
                request_id: 1,
                bytes: 7,
            },
            limits,
        )
        .expect("response");
        assert_eq!(
            decode_response(&response, limits).expect("queued"),
            LocalControlResponse::Queued {
                request_id: 1,
                bytes: 7
            }
        );
        let mut wrong_status = response.clone();
        wrong_status[5] = 0x99;
        assert_eq!(
            decode_response(&wrong_status, limits),
            Err(LocalControlError::Invalid("response status"))
        );
        let mut oversized_response = response;
        oversized_response[5] = STATUS_ACCEPTED;
        oversized_response[14..18].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(
            decode_response(&oversized_response, limits),
            Err(LocalControlError::FrameTooLarge)
        );
        let invalid_utf8 = LocalControlResponse::Rejected {
            request_id: 1,
            message: "diagnostic".to_owned(),
        };
        let mut invalid_utf8 = encode_response(&invalid_utf8, limits).expect("diagnostic");
        invalid_utf8[18] = 0xff;
        assert_eq!(
            decode_response(&invalid_utf8, limits),
            Err(LocalControlError::InvalidUtf8)
        );
    }

    #[test]
    fn leased_subscription_operations_and_responses_have_one_canonical_codec() {
        let limits = limits();
        let operations = [
            LocalSubscriptionOperation::Open {
                cursor: Box::new([]),
                credit: 4,
                lease_ms: 30_000,
            },
            LocalSubscriptionOperation::Resume {
                lease: lease(),
                cursor: cursor(),
                credit: 4,
                lease_ms: 30_000,
            },
            LocalSubscriptionOperation::Credit {
                lease: lease(),
                credit: 2,
            },
            LocalSubscriptionOperation::Ack {
                lease: lease(),
                cursor: cursor(),
            },
            LocalSubscriptionOperation::Renew {
                lease: lease(),
                cursor: cursor(),
                credit: 4,
                lease_ms: 60_000,
            },
            LocalSubscriptionOperation::Cancel { lease: lease() },
            LocalSubscriptionOperation::Page {
                lease: lease(),
                page: Box::from(*b"page-v1"),
                credit: 128,
            },
        ];
        for (index, operation) in operations.into_iter().enumerate() {
            let request = LocalControlRequest::Subscription(LocalSubscriptionRequest {
                request_id: index as u64 + 1,
                operation,
            });
            let encoded = encode_request(&request, limits).expect("encode lease operation");
            assert_eq!(
                decode_request(&encoded, limits).expect("decode lease operation"),
                request
            );
        }

        let responses = [
            LocalSubscriptionResponse::Opened {
                request_id: 1,
                lease: lease(),
                cursor: cursor(),
                credit: 4,
                lease_ms: 30_000,
            },
            LocalSubscriptionResponse::Resumed {
                request_id: 2,
                lease: lease(),
                cursor: cursor(),
                credit: 4,
                lease_ms: 30_000,
            },
            LocalSubscriptionResponse::Batch {
                request_id: 3,
                lease: lease(),
                previous: cursor(),
                cursor: Box::from(*b"cursor-v2"),
                credit: 2,
                payload: Box::from(*b"events"),
            },
            LocalSubscriptionResponse::ResetWithRoot {
                request_id: 4,
                lease: lease(),
                cursor: Box::from(*b"cursor-root"),
                credit: 4,
                reason: LocalSubscriptionResetReason::Pruned,
                payload: Box::from(*b"reset-page"),
            },
            LocalSubscriptionResponse::Acked {
                request_id: 5,
                lease: lease(),
                cursor: cursor(),
            },
            LocalSubscriptionResponse::Renewed {
                request_id: 6,
                lease: lease(),
                cursor: cursor(),
                credit: 4,
                lease_ms: 30_000,
            },
            LocalSubscriptionResponse::Cancelled {
                request_id: 7,
                lease: lease(),
            },
            LocalSubscriptionResponse::SnapshotPage {
                request_id: 8,
                lease: lease(),
                page: Box::from(*b"page-v1"),
                next: Some(Box::from(*b"page-v2")),
                credit: 128,
                payload: Box::from(*b"descriptor-and-rows"),
            },
        ];
        for response in responses {
            let raw = LocalControlResponse::Subscription(response.clone());
            let encoded = encode_response(&raw, limits).expect("encode lease response");
            assert_eq!(
                decode_response(&encoded, limits).expect("decode lease response"),
                raw
            );
        }
    }

    #[test]
    fn leased_client_retains_interleaved_replies_across_fragmented_reads_and_reconnects() {
        let limits = limits();
        let first = encode_response(
            &LocalControlResponse::Subscription(LocalSubscriptionResponse::Batch {
                request_id: 9,
                lease: lease(),
                previous: cursor(),
                cursor: Box::from(*b"cursor-v2"),
                credit: 1,
                payload: Box::from(*b"nine"),
            }),
            limits,
        )
        .expect("first response");
        let second = encode_response(
            &LocalControlResponse::Subscription(LocalSubscriptionResponse::Acked {
                request_id: 8,
                lease: lease(),
                cursor: cursor(),
            }),
            limits,
        )
        .expect("second response");
        let mut input = frame(&first, limits).expect("first frame");
        input.extend_from_slice(&frame(&second, limits).expect("second frame"));
        let stream = ChunkedScript {
            input: Cursor::new(input),
            writes: Vec::new(),
            chunk: 1,
        };
        let mut client = LocalControlClient::new(stream, limits);
        let request = LocalControlRequest::Subscription(LocalSubscriptionRequest {
            request_id: 8,
            operation: LocalSubscriptionOperation::Ack {
                lease: lease(),
                cursor: cursor(),
            },
        });
        let response = client.request(&request).expect("correlated response");
        assert_eq!(response.request_id(), 8);
        assert_eq!(client.pending_len(), 1);
        assert!(client.pending_bytes() > 0);
        assert!(client.pending_bytes() <= LOCAL_CONTROL_MAX_PENDING_BYTES);
        assert_eq!(
            client
                .response_for(9)
                .expect("interleaved response")
                .request_id(),
            9
        );

        let reconnect = LocalControlRequest::Subscription(LocalSubscriptionRequest {
            request_id: 10,
            operation: LocalSubscriptionOperation::Resume {
                lease: lease(),
                cursor: cursor(),
                credit: 4,
                lease_ms: 30_000,
            },
        });
        let reconnect_bytes = encode_request(&reconnect, limits).expect("resume request");
        assert_eq!(
            decode_request(&reconnect_bytes, limits).expect("resume decode"),
            reconnect
        );
        assert!(!client.into_inner().writes.is_empty());
    }

    #[test]
    fn leased_codec_rejects_zero_leases_bad_cursors_and_reset_reason_tampering() {
        let limits = limits();
        let request = LocalControlRequest::Subscription(LocalSubscriptionRequest {
            request_id: 1,
            operation: LocalSubscriptionOperation::Resume {
                lease: lease(),
                cursor: cursor(),
                credit: 1,
                lease_ms: 1,
            },
        });
        let mut encoded = encode_request(&request, limits).expect("request");
        encoded[14..30].fill(0);
        assert_eq!(
            decode_request(&encoded, limits),
            Err(LocalControlError::Invalid("subscription lease id"))
        );

        let response =
            LocalControlResponse::Subscription(LocalSubscriptionResponse::ResetWithRoot {
                request_id: 2,
                lease: lease(),
                cursor: cursor(),
                credit: 1,
                reason: LocalSubscriptionResetReason::Gap,
                payload: Box::from(*b"root-page"),
            });
        let mut encoded = encode_response(&response, limits).expect("reset response");
        let payload_start = LOCAL_CONTROL_HEADER_BYTES + 4;
        encoded[payload_start + 29] = 0xff;
        assert_eq!(
            decode_response(&encoded, limits),
            Err(LocalControlError::Invalid("subscription reset reason"))
        );
        let mut truncated = encode_response(&response, limits).expect("reset response");
        truncated.pop();
        assert_eq!(
            decode_response(&truncated, limits),
            Err(LocalControlError::Truncated)
        );

        let zero_batch = LocalControlResponse::Subscription(LocalSubscriptionResponse::Batch {
            request_id: 3,
            lease: lease(),
            previous: cursor(),
            cursor: Box::from(*b"cursor-v2"),
            credit: 0,
            payload: Box::from(*b"events"),
        });
        assert_eq!(
            encode_response(&zero_batch, limits),
            Err(LocalControlError::Invalid("subscription credit"))
        );

        let zero_page =
            LocalControlResponse::Subscription(LocalSubscriptionResponse::SnapshotPage {
                request_id: 4,
                lease: lease(),
                page: Box::new([]),
                next: None,
                credit: 0,
                payload: Box::from(*b"page"),
            });
        assert_eq!(
            encode_response(&zero_page, limits),
            Err(LocalControlError::Invalid("subscription credit"))
        );
    }
}
