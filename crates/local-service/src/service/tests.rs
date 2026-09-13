//! Local daemon service framing tests.

#![allow(clippy::expect_used, clippy::panic)]

use super::*;
use crate::protocol::{EngineRequest, EngineStatus, FrameLimits, frame, unframe};
use std::io::Cursor;

#[derive(Debug, Default)]
struct FakeOwner {
    commands: usize,
    engines: usize,
    closed: bool,
}

impl OwnerService for FakeOwner {
    fn command(&mut self, body: &[u8]) -> Result<Vec<u8>, ProtocolError> {
        self.commands += 1;
        Ok(body.to_vec())
    }

    fn engine(
        &mut self,
        _request_id: u64,
        _request: EngineRequest,
    ) -> Result<EngineStatus, ProtocolError> {
        self.engines += 1;
        Ok(EngineStatus::Accepted)
    }

    fn serve_one(&mut self) -> bool {
        false
    }

    fn close(&mut self) {
        self.closed = true;
    }
}

#[test]
fn one_owner_handles_command_and_control_payloads() {
    let limits = FrameLimits {
        max_frame: 1024,
        max_cursor: 128,
        max_frames_per_connection: 4,
        transport: backend_engine::TransportLimits {
            max_frame: 1024,
            max_chunk: 1024,
            ..backend_engine::TransportLimits::default()
        },
    };
    let mut service = LocaldService::new(FakeOwner::default(), limits).expect("service");
    let command = service.handle_payload(b"{\"version\":1}").expect("command");
    assert_eq!(command, b"{\"version\":1}");
    let control = crate::protocol::encode_engine_request(
        4,
        &EngineRequest::Subscribe {
            cursor: Box::from(b"c".as_slice()),
            credit: 1,
        },
        limits,
    )
    .expect("control");
    let response = service.handle_payload(&control).expect("response");
    assert!(matches!(
        unframe(&frame(&response, limits).expect("frame"), limits).expect("unframe"),
        payload if !payload.is_empty()
    ));
    assert_eq!(service.owner().commands, 1);
    assert_eq!(service.owner().engines, 1);
}

#[test]
fn stream_service_preserves_outer_frame_boundaries() {
    let limits = FrameLimits {
        max_frame: 1024,
        max_cursor: 128,
        max_frames_per_connection: 2,
        transport: backend_engine::TransportLimits {
            max_frame: 1024,
            max_chunk: 1024,
            ..backend_engine::TransportLimits::default()
        },
    };
    let input = frame(b"abc", limits).expect("frame");
    let mut stream = Cursor::new(input);
    let mut service = LocaldService::new(FakeOwner::default(), limits).expect("service");
    assert_eq!(service.serve_stream(&mut stream).expect("serve"), 1);
    assert_eq!(service.owner().commands, 1);
}

#[test]
fn malformed_command_errors_preserve_the_shared_reply_version_and_correlation() {
    let limits = FrameLimits {
        max_frame: 1024,
        max_cursor: 128,
        max_frames_per_connection: 2,
        transport: backend_engine::TransportLimits {
            max_frame: 1024,
            max_chunk: 1024,
            ..backend_engine::TransportLimits::default()
        },
    };
    let correlation = RequestCorrelation::from_payload(
        br#"{"version":2,"request_id":47,"command":{"kind":"unknown"}}"#,
    );
    assert_eq!(correlation, RequestCorrelation::Command(47));
    let payload = error_payload(
        correlation,
        &ProtocolError::InvalidCommand("malformed command".to_owned()),
        limits,
    );
    let reply = backend_engine::decode_reply_dto(&payload).expect("shared command error reply");
    assert_eq!(reply.version(), backend_engine::DTO_VERSION);
    assert_eq!(reply.request_id, 47);
    assert!(matches!(
        reply.reply,
        backend_engine::CommandReply::Error(_)
    ));
}
