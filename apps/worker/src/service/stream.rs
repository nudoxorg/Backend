//! Bounded worker stream protocol loop.
//!
//! This module owns socket polling, cancellation control while a pure recipe is
//! running, and canonical response framing. Execution state remains in the
//! parent service and its lifecycle module.

use backend_engine::{
    PureRecipeExecutor, Relation, TransportMessage, WireRecipeRequest, WorkerAttestationSigner,
    WorkerError,
};
use std::io::{self, Read, Write};
use std::sync::mpsc::TryRecvError;
use std::time::Duration;

use super::{JobAdmission, RunningJob, WorkerFramed, WorkerProtocolError, WorkerService};

/// Socket capability used by listeners to shorten the read interval only while
/// a recipe is executing. The initial request and capability handshake retain
/// the configured peer deadline; a running job must poll for cancellation
/// without inheriting a thirty-second idle read.
pub(crate) trait PollableWorkerStream {
    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()>;
}

#[cfg(any(unix, windows))]
impl PollableWorkerStream for backend_platform::LocalStream {
    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        backend_platform::LocalStream::set_read_timeout(self, timeout)
    }
}

impl PollableWorkerStream for std::net::TcpStream {
    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        std::net::TcpStream::set_read_timeout(self, timeout)
    }
}

impl PollableWorkerStream for backend_engine::AuthenticatedTcpStream<std::net::TcpStream> {
    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.inner().set_read_timeout(timeout)
    }
}

impl<E: PureRecipeExecutor, S: WorkerAttestationSigner> WorkerService<E, S> {
    /// Serves a bounded canonical worker stream until EOF, protocol error,
    /// shutdown, or the per-connection frame limit.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn serve_stream<R: Relation + Send, A: JobAdmission<R>, T: Read + Write>(
        &mut self,
        stream: &mut T,
        admission: &mut A,
    ) -> Result<usize, WorkerProtocolError> {
        self.serve_stream_internal(stream, admission, None, |_, _| Ok(()))
    }

    /// Serves a socket stream while polling for cancellation at a short,
    /// bounded interval during recipe execution. The socket's configured
    /// handshake/idle timeout is restored before the next request, and the
    /// generic compatibility method above remains available for non-socket
    /// test streams.
    pub(crate) fn serve_stream_socket<
        R: Relation + Send,
        A: JobAdmission<R>,
        T: Read + Write + PollableWorkerStream,
    >(
        &mut self,
        stream: &mut T,
        admission: &mut A,
    ) -> Result<usize, WorkerProtocolError> {
        let active_timeout = self.limits.io_timeout.min(Duration::from_millis(25));
        self.serve_stream_internal(
            stream,
            admission,
            Some(active_timeout),
            |stream, timeout| {
                stream
                    .set_read_timeout(Some(timeout))
                    .map_err(|error| WorkerProtocolError::Io(error.kind()))
            },
        )
    }

    fn serve_stream_internal<
        R: Relation + Send,
        A: JobAdmission<R>,
        T: Read + Write,
        F: Fn(&T, Duration) -> Result<(), WorkerProtocolError>,
    >(
        &mut self,
        stream: &mut T,
        admission: &mut A,
        active_timeout: Option<Duration>,
        set_active_timeout: F,
    ) -> Result<usize, WorkerProtocolError> {
        if self.closed {
            return Err(WorkerProtocolError::Closed);
        }
        let mut stream = WorkerFramed::new(&mut *stream, self.limits)?;
        let mut frames = 0usize;
        while frames < self.limits.max_frames_per_connection {
            let message = match stream.read_message() {
                Ok(message) => message,
                Err(WorkerProtocolError::Closed) => return Ok(frames),
                Err(error) => return Err(error),
            };
            match message {
                TransportMessage::WireRecipeRequest(request) => {
                    if let Some(timeout) = active_timeout {
                        set_active_timeout(&**stream.inner(), timeout)?;
                    }
                    let keep_stream =
                        self.serve_recipe_stream(&mut stream, request, admission, &mut frames)?;
                    if !keep_stream {
                        return Ok(frames);
                    }
                    if active_timeout.is_some() {
                        set_active_timeout(&**stream.inner(), self.limits.io_timeout)?;
                    }
                }
                TransportMessage::Chunk(frame) => {
                    if let Some(response) =
                        admission.admit_control(TransportMessage::Chunk(frame))?
                    {
                        stream.write_message(&response)?;
                    }
                    frames = frames.saturating_add(1);
                }
                TransportMessage::ClosurePageRequest(request) => {
                    let response = admission.admit_page(request)?.ok_or_else(|| {
                        WorkerProtocolError::Rejected(
                            "worker Merkle page admission is not configured".to_owned(),
                        )
                    })?;
                    stream.write_message(&response)?;
                    frames = frames.saturating_add(1);
                }
                TransportMessage::ClosurePageResponse(response) => {
                    let reply = admission.admit_page_response(response)?.ok_or_else(|| {
                        WorkerProtocolError::Rejected(
                            "worker Merkle page response admission is not configured".to_owned(),
                        )
                    })?;
                    stream.write_message(&reply)?;
                    frames = frames.saturating_add(1);
                }
                TransportMessage::ClosureRootOffer(offer) => {
                    let response = admission
                        .admit_control(TransportMessage::ClosureRootOffer(offer))?
                        .ok_or_else(|| {
                            WorkerProtocolError::Rejected(
                                "worker closure-root admission is not configured".to_owned(),
                            )
                        })?;
                    stream.write_message(&response)?;
                    frames = frames.saturating_add(1);
                }
                TransportMessage::ClosureNeedRequest(request) => {
                    let response = admission
                        .admit_control(TransportMessage::ClosureNeedRequest(request))?
                        .ok_or_else(|| {
                            WorkerProtocolError::Rejected(
                                "worker closure-need admission is not configured".to_owned(),
                            )
                        })?;
                    stream.write_message(&response)?;
                    frames = frames.saturating_add(1);
                }
                TransportMessage::CancelAttempt(cancel) => {
                    self.admit_completed_cancel(cancel)?;
                    frames = frames.saturating_add(1);
                }
                message => {
                    let response = self.handle_message(message, admission)?;
                    stream.write_message(&response)?;
                    frames = frames.saturating_add(1);
                }
            }
        }
        Ok(frames)
    }

    #[allow(clippy::too_many_lines)]
    fn serve_recipe_stream<R: Relation + Send, A: JobAdmission<R>, T: Read + Write>(
        &mut self,
        stream: &mut WorkerFramed<&mut T>,
        request: WireRecipeRequest,
        admission: &mut A,
        frames: &mut usize,
    ) -> Result<bool, WorkerProtocolError> {
        let RunningJob {
            key,
            handle,
            worker,
            completed: receiver,
        } = self.begin_job(request, admission)?;

        let mut cancellation_requested = false;
        let completed = loop {
            match receiver.try_recv() {
                Ok(completed) => break Some(completed),
                Err(TryRecvError::Disconnected) => break None,
                Err(TryRecvError::Empty) => {}
            }
            match stream.read_message() {
                Ok(TransportMessage::Capabilities(peer)) => {
                    let response = match self
                        .handle_message(TransportMessage::Capabilities(peer), admission)
                    {
                        Ok(response) => response,
                        Err(error) => {
                            self.cancel_and_reap(key, &handle, worker)?;
                            return Err(error);
                        }
                    };
                    if let Err(error) = stream.write_message(&response) {
                        self.cancel_and_reap(key, &handle, worker)?;
                        return Err(error);
                    }
                    *frames = frames.saturating_add(1);
                }
                Ok(TransportMessage::WireRecipeRequest(_)) => {
                    self.cancel_and_reap(key, &handle, worker)?;
                    return Err(WorkerProtocolError::Backpressure);
                }
                Ok(TransportMessage::CancelAttempt(cancel)) => {
                    if let Err(error) = self.admit_cancel_attempt(key, cancel) {
                        self.cancel_and_reap(key, &handle, worker)?;
                        return Err(error);
                    }
                    cancellation_requested = true;
                    *frames = frames.saturating_add(1);
                }
                Ok(TransportMessage::Chunk(frame)) => {
                    if let Some(response) =
                        admission.admit_control(TransportMessage::Chunk(frame))?
                    {
                        stream.write_message(&response)?;
                    }
                    *frames = frames.saturating_add(1);
                }
                Ok(TransportMessage::ClosurePageRequest(request)) => {
                    let Some(response) = admission.admit_page(request)? else {
                        self.cancel_and_reap(key, &handle, worker)?;
                        return Err(WorkerProtocolError::Rejected(
                            "worker Merkle page admission is not configured".to_owned(),
                        ));
                    };
                    stream.write_message(&response)?;
                    *frames = frames.saturating_add(1);
                }
                Ok(TransportMessage::ClosurePageResponse(response)) => {
                    let Some(reply) = admission.admit_page_response(response)? else {
                        self.cancel_and_reap(key, &handle, worker)?;
                        return Err(WorkerProtocolError::Rejected(
                            "worker Merkle page response admission is not configured".to_owned(),
                        ));
                    };
                    stream.write_message(&reply)?;
                    *frames = frames.saturating_add(1);
                }
                Ok(TransportMessage::ClosureRootOffer(offer)) => {
                    let Some(response) =
                        admission.admit_control(TransportMessage::ClosureRootOffer(offer))?
                    else {
                        self.cancel_and_reap(key, &handle, worker)?;
                        return Err(WorkerProtocolError::Rejected(
                            "worker closure-root admission is not configured".to_owned(),
                        ));
                    };
                    stream.write_message(&response)?;
                    *frames = frames.saturating_add(1);
                }
                Ok(TransportMessage::ClosureNeedRequest(request)) => {
                    let Some(response) =
                        admission.admit_control(TransportMessage::ClosureNeedRequest(request))?
                    else {
                        self.cancel_and_reap(key, &handle, worker)?;
                        return Err(WorkerProtocolError::Rejected(
                            "worker closure-need admission is not configured".to_owned(),
                        ));
                    };
                    stream.write_message(&response)?;
                    *frames = frames.saturating_add(1);
                }
                Ok(_) => {
                    // The canonical worker stream has no untrusted error
                    // response. Close after cancelling so a peer cannot send
                    // a second operation into an in-flight execution lane.
                    self.cancel_and_reap(key, &handle, worker)?;
                    return Err(WorkerProtocolError::Rejected(
                        "only capabilities or an authenticated cancellation may interrupt a job"
                            .to_owned(),
                    ));
                }
                Err(WorkerProtocolError::Timeout) => {}
                Err(WorkerProtocolError::Truncated | WorkerProtocolError::Closed) => {
                    self.cancel_and_reap(key, &handle, worker)?;
                    return Ok(false);
                }
                Err(error) => {
                    self.cancel_and_reap(key, &handle, worker)?;
                    return Err(error);
                }
            }
        };

        let Some(completed) = completed else {
            self.active.remove(&key);
            let _ = worker.join();
            return Err(WorkerProtocolError::Rejected(
                "worker execution thread ended without a result".to_owned(),
            ));
        };
        let cancellation = self
            .active
            .remove(&completed.key)
            .ok_or_else(|| {
                WorkerProtocolError::Rejected(
                    "worker completion did not match an active attempt".to_owned(),
                )
            })?
            .cancellation;
        let join_result = worker.join();
        if join_result.is_err() {
            return Err(WorkerProtocolError::Rejected(
                "worker execution thread panicked".to_owned(),
            ));
        }
        let result = completed.result;
        if cancellation_requested {
            // There is no unauthenticated error variant in the canonical
            // replication grammar. Once an exact cancel has been admitted,
            // consume the cooperative result without publishing it. The
            // active entry is already removed, so a reconnect can make a
            // fresh attempt without retaining capacity for the cancelled job.
            // Keep this stream's owner loop alive as well: the peer may send
            // another control frame after observing that cancellation was
            // admitted.
            return match result {
                Ok(_) | Err(WorkerError::Cancelled) => Ok(true),
                Err(error) => Err(WorkerProtocolError::Rejected(error.to_string())),
            };
        }
        let result = result.map_err(|error| WorkerProtocolError::Rejected(error.to_string()))?;
        stream.write_message(&TransportMessage::WireRecipeResult(Box::new(result)))?;
        self.completed_cancellation = Some(cancellation);
        *frames = frames.saturating_add(1);
        Ok(true)
    }
}
