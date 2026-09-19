//! Framed transport facade for the local owner loop.

use super::runtime::{RequestCorrelation, ServiceError, error_payload};
use crate::protocol::{
    EngineRequest, EngineStatus, FrameLimits, ProtocolError, RequestFrame, ResponseFrame,
    decode_request, encode_response, read_frame, write_frame,
};
use std::fmt;
use std::io::{Read, Write};

/// A process-owned adapter around the one durable engine owner.
pub trait OwnerService {
    /// Handles one CLI/MCP/library command body. The body is still a wire
    /// claim; implementations must decode it against owner-supplied expected
    /// values before returning a typed reply encoding.
    ///
    /// # Errors
    ///
    /// Returns an error when the command is malformed or rejected by the owner.
    fn command(&mut self, body: &[u8]) -> Result<Vec<u8>, ProtocolError>;

    /// Handles one engine operation on the owner loop.
    ///
    /// # Errors
    ///
    /// Returns an error when the operation is rejected by the owner.
    fn engine(
        &mut self,
        request_id: u64,
        request: EngineRequest,
    ) -> Result<EngineStatus, ProtocolError>;

    /// Runs one fair owner-loop operation. Returning `true` means work was
    /// processed. The listener uses this between client reads to prevent a
    /// busy or slow connection from starving another lane.
    fn serve_one(&mut self) -> bool;

    /// Closes request lanes and releases external transport references.
    fn close(&mut self);
}

/// A bounded local endpoint over one owner service.
pub struct LocaldService<O> {
    owner: O,
    limits: FrameLimits,
    closed: bool,
    lifecycle: Option<crate::listener::ListenerShutdown>,
}

impl<O: fmt::Debug> fmt::Debug for LocaldService<O> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocaldService")
            .field("owner", &self.owner)
            .field("limits", &self.limits)
            .field("closed", &self.closed)
            .field("lifecycle", &self.lifecycle)
            .finish()
    }
}

impl<O: OwnerService> LocaldService<O> {
    /// Creates a service around an already-open owner.
    ///
    /// # Errors
    ///
    /// Returns an error when the supplied frame limits are invalid.
    pub fn new(owner: O, limits: FrameLimits) -> Result<Self, ProtocolError> {
        limits.validate()?;
        Ok(Self {
            owner,
            limits,
            closed: false,
            lifecycle: None,
        })
    }

    /// Installs the lifecycle capability answering a wire shutdown request.
    ///
    /// A listener calls this at bind time. Until it does, a shutdown frame is
    /// rejected with a bounded diagnostic rather than silently accepted: a
    /// service with no listener has nothing to stop, and a client must be able
    /// to tell "stopped" from "ignored".
    pub fn attach_lifecycle(&mut self, lifecycle: crate::listener::ListenerShutdown) {
        self.lifecycle = Some(lifecycle);
    }

    /// Answers a wire shutdown request through the listener capability.
    fn request_shutdown(&self) -> EngineStatus {
        self.lifecycle.as_ref().map_or_else(
            || {
                EngineStatus::Rejected(
                    "locald lifecycle control is unavailable on this service".to_owned(),
                )
            },
            |lifecycle| {
                lifecycle.request();
                EngineStatus::Accepted
            },
        )
    }

    /// Returns the configured frame and replication limits.
    #[must_use]
    pub const fn limits(&self) -> FrameLimits {
        self.limits
    }

    /// Returns a shared reference to the owner adapter.
    #[must_use]
    pub const fn owner(&self) -> &O {
        &self.owner
    }

    /// Returns a mutable reference to the owner adapter for controlled
    /// composition or diagnostics.
    #[must_use]
    pub const fn owner_mut(&mut self) -> &mut O {
        &mut self.owner
    }

    /// Handles one complete payload and returns the response payload.
    ///
    /// # Errors
    ///
    /// Returns an error when decoding, owner admission, or response encoding fails.
    pub fn handle_payload(&mut self, payload: &[u8]) -> Result<Vec<u8>, ProtocolError> {
        if self.closed {
            return Err(ProtocolError::Closed);
        }
        let request = match decode_request(payload, self.limits) {
            Ok(request) => request,
            Err(error) => {
                let correlation = RequestCorrelation::from_payload(payload);
                return match correlation.request_id() {
                    Some(request_id) => encode_response(
                        &ResponseFrame::Engine {
                            request_id,
                            status: EngineStatus::Rejected(error.to_string()),
                        },
                        self.limits,
                    ),
                    None => Err(error),
                };
            }
        };
        let response = match request {
            RequestFrame::Command(body) => {
                let reply = self.owner.command(&body)?;
                ResponseFrame::Command(reply.into_boxed_slice())
            }
            // Shutdown is a listener lifecycle operation, not workspace state,
            // so it never reaches the owner adapter.
            RequestFrame::Engine {
                request_id,
                request,
            } if matches!(*request, EngineRequest::Shutdown) => ResponseFrame::Engine {
                request_id,
                status: self.request_shutdown(),
            },
            RequestFrame::Engine {
                request_id,
                request,
            } => ResponseFrame::Engine {
                request_id,
                status: self.owner.engine(request_id, *request)?,
            },
        };
        encode_response(&response, self.limits)
    }

    /// Handles one already-connected byte stream until EOF, protocol error,
    /// shutdown, or the per-connection frame limit is reached.
    ///
    /// # Errors
    ///
    /// Returns an error when a frame or owner operation is rejected.
    pub fn serve_stream<S: Read + Write>(&mut self, stream: &mut S) -> Result<usize, ServiceError> {
        if self.closed {
            return Err(ServiceError::Protocol(ProtocolError::Closed));
        }
        let mut frames = 0usize;
        while frames < self.limits.max_frames_per_connection {
            // Give every engine lane a chance before blocking on another
            // client frame. This is a nonblocking tick and does not alter the
            // sole-owner rule.
            let _ = self.owner.serve_one();
            let payload = match read_frame(stream, self.limits) {
                Ok(payload) => payload,
                Err(ProtocolError::Closed) => return Ok(frames),
                Err(error) => {
                    self.write_error(stream, RequestCorrelation::Unknown, &error)?;
                    return Err(ServiceError::Protocol(error));
                }
            };
            let response = match self.handle_payload(&payload) {
                Ok(response) => response,
                Err(error) => {
                    self.write_error(stream, RequestCorrelation::from_payload(&payload), &error)?;
                    return Err(ServiceError::Protocol(error));
                }
            };
            write_frame(stream, &response, self.limits).map_err(ServiceError::Protocol)?;
            frames += 1;
            // Process all immediately available owner work before accepting
            // the next client frame. No connection can run owner state itself.
            while self.owner.serve_one() {}
        }
        Ok(frames)
    }

    /// Processes one connection payload and writes one response. This helper
    /// is useful for `UnixStream` pair tests and event-loop integrations.
    ///
    /// # Errors
    ///
    /// Returns an error when reading, processing, or writing the frame fails.
    pub fn serve_one_frame<S: Read + Write>(&mut self, stream: &mut S) -> Result<(), ServiceError> {
        let payload = read_frame(stream, self.limits).map_err(ServiceError::Protocol)?;
        let response = match self.handle_payload(&payload) {
            Ok(response) => response,
            Err(error) => {
                self.write_error(stream, RequestCorrelation::from_payload(&payload), &error)?;
                return Err(ServiceError::Protocol(error));
            }
        };
        write_frame(stream, &response, self.limits).map_err(ServiceError::Protocol)
    }

    fn write_error<S: Write>(
        &self,
        stream: &mut S,
        correlation: RequestCorrelation,
        error: &ProtocolError,
    ) -> Result<(), ServiceError> {
        let response = error_payload(correlation, error, self.limits);
        write_frame(stream, &response, self.limits).map_err(ServiceError::Protocol)
    }

    /// Closes this service and its owner.
    pub fn close(&mut self) {
        if !self.closed {
            self.closed = true;
            self.owner.close();
        }
    }

    /// Returns whether the service has been closed.
    #[must_use]
    pub const fn is_closed(&self) -> bool {
        self.closed
    }

    /// Consumes the service and returns the owner after shutdown state has
    /// been observed by the service.
    #[must_use]
    pub fn into_owner(self) -> O {
        self.owner
    }
}
