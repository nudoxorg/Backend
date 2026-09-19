//! Typed admission boundary for worker control and recipe requests.
//!
//! The stream protocol delegates every executable request to this owner supplied
//! boundary. Keeping it separate from the byte loop makes it difficult to
//! accidentally turn a wire identity into executable material.

use super::WorkerJob;
use backend_engine::{Relation, TransportMessage, WireRecipeRequest, WorkerError};

/// Owner admission seam for one exact worker request.
pub trait JobAdmission<R: Relation> {
    /// Admits the untrusted request and returns typed execution material.
    ///
    /// # Errors
    /// Returns an error when the request is not authorized for this worker.
    fn admit(&mut self, request: &WireRecipeRequest) -> Result<WorkerJob<R>, WorkerError>;

    /// Admits one control prelude before execution. The default rejects no
    /// state and emits no response, allowing small fixture admissions to keep
    /// the ordinary capability handshake without implementing closure sync.
    ///
    /// # Errors
    /// Returns an error when the control message is not authorized.
    fn admit_control(
        &mut self,
        _message: TransportMessage,
    ) -> Result<Option<TransportMessage>, WorkerError> {
        Ok(None)
    }

    /// Admits one correlated Merkle page request. Implementations may use a
    /// durable relation index and return the page response on the same
    /// connection; the default keeps fixture workers page-less.
    ///
    /// # Errors
    /// Returns an error when the page request is not authorized.
    fn admit_page(
        &mut self,
        _request: backend_engine::ClosurePageRequest,
    ) -> Result<Option<TransportMessage>, WorkerError> {
        Ok(None)
    }

    /// Admits one correlated page returned for a worker-issued request.
    ///
    /// # Errors
    /// Returns an error when the page response is not authorized.
    fn admit_page_response(
        &mut self,
        _response: backend_engine::ClosurePageResponse,
    ) -> Result<Option<TransportMessage>, WorkerError> {
        Ok(None)
    }
}

/// Explicit admission default that refuses every execution request.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoJobAdmission;

impl<R: Relation> JobAdmission<R> for NoJobAdmission {
    fn admit(&mut self, _request: &WireRecipeRequest) -> Result<WorkerJob<R>, WorkerError> {
        Err(WorkerError::Capability)
    }
}

impl<R, F> JobAdmission<R> for F
where
    R: Relation,
    F: FnMut(&WireRecipeRequest) -> Result<WorkerJob<R>, WorkerError>,
{
    fn admit(&mut self, request: &WireRecipeRequest) -> Result<WorkerJob<R>, WorkerError> {
        self(request)
    }
}
