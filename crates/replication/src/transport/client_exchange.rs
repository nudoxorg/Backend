//! Resumable one-request local-control exchange state machine.

use super::super::{LocalControlRequest, encode_request, frame};
use super::{LocalControlClient, LocalControlError, LocalControlResponse, decode_response};
use std::io::{ErrorKind, Read, Write};
use std::time::{Duration, Instant};

/// Phase of one bounded local-control exchange.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalControlExchangePhase {
    /// Writing the one encoded request frame.
    Sending,
    /// Flushing the already-written request frame.
    Flushing,
    /// Receiving the four-byte frame length.
    ReadingHeader,
    /// Receiving the validated response body.
    ReadingBody,
}

/// Exact bounded progress for one request/correlation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LocalControlExchangeProgress {
    /// Request identity whose one frame is in flight.
    pub request_id: u64,
    /// Current write, flush, header, or body phase.
    pub phase: LocalControlExchangePhase,
    /// Bytes written from the sole encoded request frame.
    pub write_offset: usize,
    /// Bytes received from the four-byte response length.
    pub header_offset: usize,
    /// Validated expected response body length, if the full header arrived.
    pub body_len: Option<usize>,
    /// Bytes received from the validated response body.
    pub body_offset: usize,
    /// Fixed absolute deadline for this one request.
    pub deadline: Instant,
    /// Elapsed time since this exchange began.
    pub elapsed: Duration,
}

/// Terminal result of the resumable exchange driver.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalControlExchangeFailure {
    /// The fixed whole-exchange deadline elapsed. The request may have been
    /// admitted; callers must not replay it on a new request ID.
    Stalled,
    /// The caller requested cancellation between I/O attempts.
    Cancelled,
    /// The peer closed or reset the stream, possibly mid-frame.
    Closed,
    /// A complete peer frame violated the bounded local-control grammar.
    Protocol(LocalControlError),
    /// The stream failed for an I/O reason other than a readiness timeout.
    Io(ErrorKind),
}

/// A terminal exchange classification together with the exact last progress.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LocalControlExchangeError {
    /// Terminal reason for this one request.
    pub failure: LocalControlExchangeFailure,
    /// Phase and offsets when the exchange stopped.
    pub progress: LocalControlExchangeProgress,
}

impl LocalControlExchangeError {
    /// Whether this terminal exchange stopped before touching the stream.
    ///
    /// A typed stall or cancellation in the initial sending phase with zero
    /// request and response offsets leaves the connection at its previous
    /// frame boundary. Every other terminal state may have admitted a request
    /// or consumed only part of a response and must retire the stream.
    #[must_use]
    pub const fn request_was_not_sent(self) -> bool {
        matches!(
            self.failure,
            LocalControlExchangeFailure::Stalled | LocalControlExchangeFailure::Cancelled
        ) && matches!(self.progress.phase, LocalControlExchangePhase::Sending)
            && self.progress.write_offset == 0
            && self.progress.header_offset == 0
            && self.progress.body_len.is_none()
            && self.progress.body_offset == 0
    }
}

impl std::fmt::Display for LocalControlExchangeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.failure {
            LocalControlExchangeFailure::Stalled => write!(
                formatter,
                "local control request {} stalled in {:?}",
                self.progress.request_id, self.progress.phase
            ),
            LocalControlExchangeFailure::Cancelled => write!(
                formatter,
                "local control request {} was cancelled in {:?}",
                self.progress.request_id, self.progress.phase
            ),
            LocalControlExchangeFailure::Closed => write!(
                formatter,
                "local control peer closed in {:?} for request {}",
                self.progress.phase, self.progress.request_id
            ),
            LocalControlExchangeFailure::Protocol(error) => write!(
                formatter,
                "local control protocol failed in {:?} for request {}: {error}",
                self.progress.phase, self.progress.request_id
            ),
            LocalControlExchangeFailure::Io(kind) => write!(
                formatter,
                "local control I/O failed in {:?} for request {}: {kind:?}",
                self.progress.phase, self.progress.request_id
            ),
        }
    }
}

impl std::error::Error for LocalControlExchangeError {}

/// Decision made by the caller's short cancellation/freshness tick.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalControlExchangeDecision {
    /// Continue the same request at its current byte offsets.
    Continue,
    /// Stop this exchange without replay. The stream remains usable only if
    /// the terminal progress proves that no bytes touched it.
    Cancel,
}

/// An exclusively borrowed, single in-flight local-control request.
///
/// The mutable borrow prevents another request from sharing the stream until
/// this exchange completes or is dropped. An incomplete drop poisons the
/// underlying client unless a typed stop proves that no bytes touched it.
#[must_use = "dropping an incomplete exchange may retire its client stream"]
pub struct PendingLocalControlExchange<'a, S> {
    client: &'a mut LocalControlClient<S>,
    request_id: u64,
    request_frame: Vec<u8>,
    write_offset: usize,
    header: [u8; 4],
    header_offset: usize,
    body_len: Option<usize>,
    body_offset: usize,
    phase: LocalControlExchangePhase,
    started_at: Instant,
    deadline: Instant,
    complete: bool,
    terminal: Option<LocalControlExchangeError>,
}

impl<'a, S: Read + Write> PendingLocalControlExchange<'a, S> {
    /// Creates one empty-offset exchange inside the module that owns its state.
    pub(super) fn new(
        client: &'a mut LocalControlClient<S>,
        request: &LocalControlRequest,
        deadline: Instant,
    ) -> Result<Self, LocalControlError> {
        let started_at = Instant::now();
        client.ensure_usable()?;
        let request_id = request.request_id();
        if client.pending.contains_key(&request_id) {
            return Err(LocalControlError::Invalid("duplicate pending request id"));
        }
        let payload = encode_request(request, client.limits)?;
        let request_frame = frame(&payload, client.limits)?;
        client.receive.clear();
        Ok(Self {
            client,
            request_id,
            request_frame,
            write_offset: 0,
            header: [0; 4],
            header_offset: 0,
            body_len: None,
            body_offset: 0,
            phase: LocalControlExchangePhase::Sending,
            started_at,
            deadline,
            complete: false,
            terminal: None,
        })
    }

    /// Drives the same request across readiness timeouts until its exact
    /// correlated response arrives or its fixed deadline/cancellation ends it.
    ///
    /// The callback runs before every potentially blocking stream operation
    /// and after each `WouldBlock`/`TimedOut` retry. It must be short and
    /// nonblocking. Socket timeout granularity, scheduling, and callback time
    /// can overshoot the absolute deadline; this does not claim syscall
    /// preemption. Partial writes, flushes, frame headers, and bodies retain
    /// their offsets and are never restarted.
    ///
    /// # Errors
    ///
    /// Returns a typed stall, cancellation, peer closure, I/O, or protocol
    /// outcome with the exact last phase and byte offsets.
    pub fn wait_with(
        &mut self,
        tick: impl FnMut(LocalControlExchangeProgress) -> LocalControlExchangeDecision,
    ) -> Result<LocalControlResponse, LocalControlExchangeError> {
        self.wait_with_io_policy(tick, |_, _| Ok(()), false)
    }

    /// Drives the same frame while bounding each native I/O attempt by the
    /// remaining absolute deadline. Unrelated frames are rejected immediately,
    /// never queued. Configuration must not perform frame I/O.
    ///
    /// # Errors
    /// Returns the exact exchange failure, including timeout configuration.
    pub fn wait_exact_with_io_deadline(
        &mut self,
        tick: impl FnMut(LocalControlExchangeProgress) -> LocalControlExchangeDecision,
        configure: impl FnMut(&S, Duration) -> std::io::Result<()>,
    ) -> Result<LocalControlResponse, LocalControlExchangeError> {
        self.wait_with_io_policy(tick, configure, true)
    }

    fn wait_with_io_policy(
        &mut self,
        mut tick: impl FnMut(LocalControlExchangeProgress) -> LocalControlExchangeDecision,
        mut configure: impl FnMut(&S, Duration) -> std::io::Result<()>,
        strict: bool,
    ) -> Result<LocalControlResponse, LocalControlExchangeError> {
        if let Some(error) = self.terminal {
            return Err(error);
        }
        if self.complete {
            return Err(self.stop(LocalControlExchangeFailure::Protocol(
                LocalControlError::Invalid("exchange already completed"),
            )));
        }
        loop {
            if Instant::now() >= self.deadline {
                return Err(self.stop(LocalControlExchangeFailure::Stalled));
            }
            if tick(self.progress()) == LocalControlExchangeDecision::Cancel {
                return Err(self.stop(LocalControlExchangeFailure::Cancelled));
            }
            if Instant::now() >= self.deadline {
                return Err(self.stop(LocalControlExchangeFailure::Stalled));
            }
            let remaining = self.deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(self.stop(LocalControlExchangeFailure::Stalled));
            }
            if let Err(error) = configure(&self.client.stream, remaining) {
                return Err(self.stop(classify_io(error.kind())));
            }
            if Instant::now() >= self.deadline {
                return Err(self.stop(LocalControlExchangeFailure::Stalled));
            }
            match self.phase {
                LocalControlExchangePhase::Sending => {
                    match self
                        .client
                        .stream
                        .write(&self.request_frame[self.write_offset..])
                    {
                        Ok(0) => return Err(self.stop(LocalControlExchangeFailure::Closed)),
                        Ok(written) => {
                            self.write_offset += written;
                            if self.write_offset == self.request_frame.len() {
                                self.phase = LocalControlExchangePhase::Flushing;
                            }
                        }
                        Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                        Err(error) if is_retryable_wait(error.kind()) => {
                            pause_after_readiness(error.kind());
                            continue;
                        }
                        Err(error) => return Err(self.stop(classify_io(error.kind()))),
                    }
                }
                LocalControlExchangePhase::Flushing => match self.client.stream.flush() {
                    Ok(()) => self.phase = LocalControlExchangePhase::ReadingHeader,
                    Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                    Err(error) if is_retryable_wait(error.kind()) => {
                        pause_after_readiness(error.kind());
                        continue;
                    }
                    Err(error) => return Err(self.stop(classify_io(error.kind()))),
                },
                LocalControlExchangePhase::ReadingHeader
                | LocalControlExchangePhase::ReadingBody => {
                    if let Some(response) = self.receive_step(strict)? {
                        self.complete = true;
                        return Ok(response);
                    }
                }
            }
        }
    }

    fn receive_step(
        &mut self,
        strict: bool,
    ) -> Result<Option<LocalControlResponse>, LocalControlExchangeError> {
        match self.phase {
            LocalControlExchangePhase::ReadingHeader => {
                match self
                    .client
                    .stream
                    .read(&mut self.header[self.header_offset..])
                {
                    Ok(0) => return Err(self.stop(LocalControlExchangeFailure::Closed)),
                    Ok(read) => {
                        self.header_offset += read;
                        if self.header_offset == self.header.len() {
                            let length =
                                usize::try_from(u32::from_be_bytes(self.header)).map_err(|_| {
                                    self.stop(LocalControlExchangeFailure::Protocol(
                                        LocalControlError::FrameTooLarge,
                                    ))
                                })?;
                            if length > self.client.limits.max_frame {
                                return Err(self.stop(LocalControlExchangeFailure::Protocol(
                                    LocalControlError::FrameTooLarge,
                                )));
                            }
                            self.client.receive.resize(length, 0);
                            self.body_len = Some(length);
                            if length == 0 {
                                if let Some(response) = self.decode_current(strict)? {
                                    return Ok(Some(response));
                                }
                            } else {
                                self.phase = LocalControlExchangePhase::ReadingBody;
                            }
                        }
                    }
                    Err(error) if error.kind() == ErrorKind::Interrupted => return Ok(None),
                    Err(error) if is_retryable_wait(error.kind()) => {
                        pause_after_readiness(error.kind());
                        return Ok(None);
                    }
                    Err(error) => return Err(self.stop(classify_io(error.kind()))),
                }
            }
            LocalControlExchangePhase::ReadingBody => {
                let Some(body_len) = self.body_len else {
                    return Err(self.stop(LocalControlExchangeFailure::Protocol(
                        LocalControlError::Truncated,
                    )));
                };
                match self
                    .client
                    .stream
                    .read(&mut self.client.receive[self.body_offset..body_len])
                {
                    Ok(0) => return Err(self.stop(LocalControlExchangeFailure::Closed)),
                    Ok(read) => {
                        self.body_offset += read;
                        if self.body_offset == body_len {
                            if let Some(response) = self.decode_current(strict)? {
                                return Ok(Some(response));
                            }
                        }
                    }
                    Err(error) if error.kind() == ErrorKind::Interrupted => return Ok(None),
                    Err(error) if is_retryable_wait(error.kind()) => {
                        pause_after_readiness(error.kind());
                        return Ok(None);
                    }
                    Err(error) => return Err(self.stop(classify_io(error.kind()))),
                }
            }
            _ => {
                return Err(self.stop(LocalControlExchangeFailure::Protocol(
                    LocalControlError::Invalid("response is not pending"),
                )));
            }
        }
        Ok(None)
    }

    /// Consumes an abandoned fully sent exchange, discarding only its exact
    /// correlated response. No response escapes to publication admission.
    /// Partial sends and ambiguous frames remain poisoned on drop. The cleanup
    /// deadline is independent of, and never extends, the operation allowance.
    pub fn discard_response_until(
        mut self,
        deadline: Instant,
        mut configure_read: impl FnMut(&S, Duration) -> std::io::Result<()>,
        expected: impl FnOnce(&LocalControlResponse) -> bool,
    ) -> Result<(), LocalControlExchangeError> {
        if !self.terminal.is_some_and(|error| {
            matches!(
                error.failure,
                LocalControlExchangeFailure::Cancelled | LocalControlExchangeFailure::Stalled
            )
        }) || self.write_offset != self.request_frame.len()
            || !matches!(
                self.phase,
                LocalControlExchangePhase::ReadingHeader | LocalControlExchangePhase::ReadingBody
            )
        {
            return Err(self.stop(LocalControlExchangeFailure::Protocol(
                LocalControlError::Invalid("exchange cannot discard a response"),
            )));
        }
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(self.stop(LocalControlExchangeFailure::Stalled));
            }
            if let Err(error) = configure_read(&self.client.stream, remaining) {
                return Err(self.stop(classify_io(error.kind())));
            }
            if Instant::now() >= deadline {
                return Err(self.stop(LocalControlExchangeFailure::Stalled));
            }
            if let Some(response) = self.receive_step(true)? {
                if Instant::now() >= deadline {
                    return Err(self.stop(LocalControlExchangeFailure::Stalled));
                }
                if !expected(&response) {
                    return Err(self.stop(LocalControlExchangeFailure::Protocol(
                        LocalControlError::Invalid(
                            "discarded response is not the outstanding operation",
                        ),
                    )));
                }
                if Instant::now() >= deadline {
                    return Err(self.stop(LocalControlExchangeFailure::Stalled));
                }
                self.complete = true;
                return Ok(());
            }
        }
    }

    fn decode_current(
        &mut self,
        strict: bool,
    ) -> Result<Option<LocalControlResponse>, LocalControlExchangeError> {
        let response = match decode_response(&self.client.receive, self.client.limits) {
            Ok(response) => response,
            Err(error) => {
                return Err(self.stop(LocalControlExchangeFailure::Protocol(error)));
            }
        };
        if response.request_id() == self.request_id {
            return Ok(Some(response));
        }
        if strict {
            return Err(self.stop(LocalControlExchangeFailure::Protocol(
                LocalControlError::Invalid("discarded response correlation mismatch"),
            )));
        }
        if let Err(error) = self.client.retain_pending(response) {
            return Err(self.stop(LocalControlExchangeFailure::Protocol(error)));
        }
        self.client.receive.clear();
        self.header = [0; 4];
        self.header_offset = 0;
        self.body_len = None;
        self.body_offset = 0;
        self.phase = LocalControlExchangePhase::ReadingHeader;
        Ok(None)
    }

    fn progress(&self) -> LocalControlExchangeProgress {
        LocalControlExchangeProgress {
            request_id: self.request_id,
            phase: self.phase,
            write_offset: self.write_offset,
            header_offset: self.header_offset,
            body_len: self.body_len,
            body_offset: self.body_offset,
            deadline: self.deadline,
            elapsed: self.started_at.elapsed(),
        }
    }

    fn stop(&mut self, failure: LocalControlExchangeFailure) -> LocalControlExchangeError {
        let error = LocalControlExchangeError {
            failure,
            progress: self.progress(),
        };
        self.terminal = Some(error);
        error
    }
}

impl<S> Drop for PendingLocalControlExchange<'_, S> {
    fn drop(&mut self) {
        if !self.complete
            && !self
                .terminal
                .is_some_and(LocalControlExchangeError::request_was_not_sent)
        {
            self.client.poisoned = true;
        }
    }
}

const fn is_retryable_wait(kind: ErrorKind) -> bool {
    matches!(kind, ErrorKind::WouldBlock | ErrorKind::TimedOut)
}

fn pause_after_readiness(kind: ErrorKind) {
    if kind == ErrorKind::WouldBlock {
        std::thread::park_timeout(Duration::from_millis(1));
    }
}

const fn classify_io(kind: ErrorKind) -> LocalControlExchangeFailure {
    if matches!(
        kind,
        ErrorKind::ConnectionReset
            | ErrorKind::ConnectionAborted
            | ErrorKind::BrokenPipe
            | ErrorKind::NotConnected
            | ErrorKind::UnexpectedEof
    ) {
        LocalControlExchangeFailure::Closed
    } else {
        LocalControlExchangeFailure::Io(kind)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod exchange_tests {
    use super::super::super::frame;
    use super::super::{LocalControlLimits, LocalControlRequest, encode_request};
    use super::*;
    use std::io::{self, Cursor};
    use std::time::Duration;

    fn limits() -> LocalControlLimits {
        LocalControlLimits {
            max_frame: 4096,
            max_cursor: 512,
            max_error: 128,
        }
    }

    fn make_request(request_id: u64) -> LocalControlRequest {
        LocalControlRequest::Subscribe {
            request_id,
            cursor: Box::from(*b"cursor-v1"),
            credit: 4,
        }
    }

    fn response(request_id: u64) -> LocalControlResponse {
        LocalControlResponse::Accepted { request_id }
    }

    fn wire_response(request_id: u64) -> Vec<u8> {
        let body = super::super::super::encode_response(&response(request_id), limits())
            .expect("encode response");
        frame(&body, limits()).expect("frame response")
    }

    #[derive(Debug)]
    struct PausingStream {
        input: Cursor<Vec<u8>>,
        writes: Vec<u8>,
        chunk: usize,
        read_calls: usize,
        write_calls: usize,
        flush_calls: usize,
        read_stall_after: Option<u64>,
        write_stall_after: Option<usize>,
    }

    impl PausingStream {
        fn new(input: Vec<u8>) -> Self {
            Self {
                input: Cursor::new(input),
                writes: Vec::new(),
                chunk: 1,
                read_calls: 0,
                write_calls: 0,
                flush_calls: 0,
                read_stall_after: None,
                write_stall_after: None,
            }
        }
    }

    impl Read for PausingStream {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            self.read_calls += 1;
            if self
                .read_stall_after
                .is_some_and(|limit| self.input.position() >= limit)
            {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            if self.read_calls % 2 == 1 {
                return Err(io::ErrorKind::TimedOut.into());
            }
            let mut amount = output.len().min(self.chunk.max(1));
            if let Some(limit) = self.read_stall_after {
                amount = amount.min(
                    usize::try_from(limit - self.input.position())
                        .expect("read stall remains ahead"),
                );
            }
            self.input.read(&mut output[..amount])
        }
    }

    impl Write for PausingStream {
        fn write(&mut self, input: &[u8]) -> io::Result<usize> {
            self.write_calls += 1;
            if self
                .write_stall_after
                .is_some_and(|limit| self.writes.len() >= limit)
            {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            if self.write_calls % 2 == 1 {
                return Err(io::ErrorKind::TimedOut.into());
            }
            let mut amount = input.len().min(self.chunk.max(1));
            if let Some(limit) = self.write_stall_after {
                amount = amount.min(limit.saturating_sub(self.writes.len()));
            }
            self.writes.extend_from_slice(&input[..amount]);
            Ok(amount)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flush_calls += 1;
            if self.flush_calls == 1 {
                Err(io::ErrorKind::WouldBlock.into())
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn partial_request_and_reply_offsets_survive_readiness_timeouts_once() {
        let mut input = wire_response(77);
        input.extend_from_slice(&wire_response(8));
        let stream = PausingStream::new(input);
        let mut client = LocalControlClient::new(stream, limits());
        let request = make_request(8);
        let expected_write = frame(
            &encode_request(&request, limits()).expect("encode request"),
            limits(),
        )
        .expect("frame request");
        let mut ticks = 0;
        let mut last = LocalControlExchangeProgress {
            request_id: 8,
            phase: LocalControlExchangePhase::Sending,
            write_offset: 0,
            header_offset: 0,
            body_len: None,
            body_offset: 0,
            deadline: Instant::now() + Duration::from_secs(2),
            elapsed: Duration::ZERO,
        };
        let mut exchange = client
            .begin_exchange(&request, Instant::now() + Duration::from_secs(2))
            .expect("begin one exchange");
        let observed = exchange
            .wait_with(|progress| {
                assert_eq!(progress.request_id, 8);
                assert!(progress.write_offset >= last.write_offset);
                if progress.phase == last.phase {
                    assert!(progress.header_offset >= last.header_offset);
                    assert!(progress.body_offset >= last.body_offset);
                }
                last = progress;
                ticks += 1;
                LocalControlExchangeDecision::Continue
            })
            .expect("correlated response after fragments");
        drop(exchange);
        assert_eq!(observed, response(8));
        assert!(ticks > 12, "the tick spans multiple partial I/O operations");
        assert_eq!(
            client.stream().writes,
            expected_write,
            "request frame written once"
        );
        assert_eq!(
            client.stream().flush_calls,
            2,
            "flush resumes without resending"
        );
        assert_eq!(client.pending_len(), 1);
        assert_eq!(
            client.response_for(77).expect("retain other correlation"),
            response(77)
        );
    }

    #[test]
    fn whole_exchange_deadline_reports_partial_write_and_retires_client() {
        let mut stream = PausingStream::new(Vec::new());
        stream.write_stall_after = Some(2);
        let mut client = LocalControlClient::new(stream, limits());
        let request = make_request(3);
        let mut exchange = client
            .begin_exchange(&request, Instant::now() + Duration::from_millis(100))
            .expect("begin exchange");
        let error = exchange
            .wait_with(|_| LocalControlExchangeDecision::Continue)
            .expect_err("request never completes");
        assert_eq!(error.failure, LocalControlExchangeFailure::Stalled);
        assert_eq!(error.progress.phase, LocalControlExchangePhase::Sending);
        assert_eq!(error.progress.write_offset, 2);
        drop(exchange);
        assert!(client.requires_reconnect());
        let before = client.stream().writes.len();
        assert!(matches!(
            client.request(&request),
            Err(LocalControlError::Invalid(
                "local control client retired after incomplete exchange"
            ))
        ));
        assert_eq!(
            client.stream().writes.len(),
            before,
            "no second request is sent"
        );
    }

    #[test]
    fn elapsed_deadline_is_a_typed_zero_offset_stall_that_leaves_the_stream_usable() {
        let stream = PausingStream::new(wire_response(19));
        let mut client = LocalControlClient::new(stream, limits());
        let request = make_request(19);
        let mut exchange = client
            .begin_exchange(&request, Instant::now() - Duration::from_millis(1))
            .expect("begin expired exchange without sending");
        let error = exchange
            .wait_with(|_| panic!("expired exchange does not tick or write"))
            .expect_err("expired fixed deadline");
        assert_eq!(error.failure, LocalControlExchangeFailure::Stalled);
        assert_eq!(error.progress.write_offset, 0);
        assert!(error.request_was_not_sent());
        drop(exchange);
        assert!(client.stream().writes.is_empty());
        assert!(!client.requires_reconnect());
        let mut retry = client
            .begin_exchange(&request, Instant::now() + Duration::from_secs(1))
            .expect("begin request at untouched boundary");
        assert_eq!(
            retry
                .wait_with(|_| LocalControlExchangeDecision::Continue)
                .expect("request succeeds on preserved stream"),
            response(19)
        );
        drop(retry);
        assert!(!client.requires_reconnect());
    }

    #[test]
    fn cancellation_after_a_partial_request_still_poisons_the_stream() {
        let mut stream = PausingStream::new(wire_response(20));
        stream.chunk = 2;
        stream.write_stall_after = Some(2);
        let mut client = LocalControlClient::new(stream, limits());
        let request = make_request(20);
        let mut exchange = client
            .begin_exchange(&request, Instant::now() + Duration::from_secs(1))
            .expect("begin exchange");
        let error = exchange
            .wait_with(|progress| {
                if progress.write_offset == 0 {
                    LocalControlExchangeDecision::Continue
                } else {
                    LocalControlExchangeDecision::Cancel
                }
            })
            .expect_err("cancel after the first request fragment");
        assert_eq!(error.failure, LocalControlExchangeFailure::Cancelled);
        assert_eq!(error.progress.phase, LocalControlExchangePhase::Sending);
        assert_eq!(error.progress.write_offset, 2);
        assert!(!error.request_was_not_sent());
        drop(exchange);
        assert!(client.requires_reconnect());
        let written = client.stream().writes.len();
        assert!(matches!(
            client.request(&request),
            Err(LocalControlError::Invalid(
                "local control client retired after incomplete exchange"
            ))
        ));
        assert_eq!(
            client.stream().writes.len(),
            written,
            "no second frame is sent"
        );
    }

    #[test]
    fn body_deadline_reports_exact_partial_body_and_cancellation_sends_nothing() {
        let mut stream = PausingStream::new(wire_response(4));
        stream.read_stall_after = Some(6);
        let mut client = LocalControlClient::new(stream, limits());
        let request = make_request(4);
        let mut exchange = client
            .begin_exchange(&request, Instant::now() + Duration::from_millis(100))
            .expect("begin exchange");
        let error = exchange
            .wait_with(|_| LocalControlExchangeDecision::Continue)
            .expect_err("response body remains partial");
        assert_eq!(error.failure, LocalControlExchangeFailure::Stalled);
        assert_eq!(error.progress.phase, LocalControlExchangePhase::ReadingBody);
        assert_eq!(error.progress.header_offset, 4);
        assert_eq!(error.progress.body_offset, 2);
        drop(exchange);

        let mut client = LocalControlClient::new(PausingStream::new(wire_response(5)), limits());
        let request = make_request(5);
        let mut exchange = client
            .begin_exchange(&request, Instant::now() + Duration::from_secs(1))
            .expect("begin exchange");
        let error = exchange
            .wait_with(|progress| {
                assert_eq!(progress.write_offset, 0);
                LocalControlExchangeDecision::Cancel
            })
            .expect_err("caller cancelled before writing");
        assert_eq!(error.failure, LocalControlExchangeFailure::Cancelled);
        assert_eq!(error.progress.write_offset, 0);
    }

    #[test]
    fn peer_close_and_bad_frame_are_not_reported_as_stalls() {
        let mut client = LocalControlClient::new(PausingStream::new(Vec::new()), limits());
        let request = make_request(10);
        let mut exchange = client
            .begin_exchange(&request, Instant::now() + Duration::from_secs(1))
            .expect("begin exchange");
        let error = exchange
            .wait_with(|_| LocalControlExchangeDecision::Continue)
            .expect_err("closed response stream");
        assert_eq!(error.failure, LocalControlExchangeFailure::Closed);
        assert_eq!(
            error.progress.phase,
            LocalControlExchangePhase::ReadingHeader
        );
        drop(exchange);

        let mut client = LocalControlClient::new(PausingStream::new(vec![0, 0]), limits());
        let request = make_request(12);
        let mut exchange = client
            .begin_exchange(&request, Instant::now() + Duration::from_secs(1))
            .expect("begin exchange");
        let error = exchange
            .wait_with(|_| LocalControlExchangeDecision::Continue)
            .expect_err("peer closed a partial header");
        assert_eq!(error.failure, LocalControlExchangeFailure::Closed);
        assert_eq!(
            error.progress.phase,
            LocalControlExchangePhase::ReadingHeader
        );
        assert_eq!(error.progress.header_offset, 2);
        drop(exchange);

        let mut partial_body = wire_response(13);
        partial_body.pop().expect("response contains a body");
        let mut client = LocalControlClient::new(PausingStream::new(partial_body), limits());
        let request = make_request(13);
        let mut exchange = client
            .begin_exchange(&request, Instant::now() + Duration::from_secs(1))
            .expect("begin exchange");
        let error = exchange
            .wait_with(|_| LocalControlExchangeDecision::Continue)
            .expect_err("peer closed a partial response body");
        assert_eq!(error.failure, LocalControlExchangeFailure::Closed);
        assert_eq!(error.progress.phase, LocalControlExchangePhase::ReadingBody);
        assert!(error.progress.body_offset > 0);
        drop(exchange);

        let stream = PausingStream::new(u32::MAX.to_be_bytes().to_vec());
        let mut client = LocalControlClient::new(stream, limits());
        let request = make_request(11);
        let mut exchange = client
            .begin_exchange(&request, Instant::now() + Duration::from_secs(1))
            .expect("begin exchange");
        let error = exchange
            .wait_with(|_| LocalControlExchangeDecision::Continue)
            .expect_err("oversized response header");
        assert_eq!(
            error.failure,
            LocalControlExchangeFailure::Protocol(LocalControlError::FrameTooLarge)
        );
        assert_eq!(
            error.progress.phase,
            LocalControlExchangePhase::ReadingHeader
        );
        drop(exchange);

        let malformed = frame(b"bad", limits()).expect("frame malformed body");
        let mut client = LocalControlClient::new(PausingStream::new(malformed), limits());
        let request = make_request(14);
        let mut exchange = client
            .begin_exchange(&request, Instant::now() + Duration::from_secs(1))
            .expect("begin exchange");
        let error = exchange
            .wait_with(|_| LocalControlExchangeDecision::Continue)
            .expect_err("malformed complete control body");
        assert_eq!(
            error.failure,
            LocalControlExchangeFailure::Protocol(LocalControlError::Truncated)
        );
        assert_eq!(error.progress.phase, LocalControlExchangePhase::ReadingBody);
    }
    #[test]
    fn affine_discard_resumes_partial_header_and_body_without_replaying() {
        for (phase, offset) in [
            (LocalControlExchangePhase::ReadingHeader, 2),
            (LocalControlExchangePhase::ReadingBody, 1),
        ] {
            let first = wire_response(42);
            let mut input = first.clone();
            input.extend(wire_response(43));
            let mut client = LocalControlClient::new(PausingStream::new(input), limits());
            let mut exchange = client
                .begin_exchange(&make_request(42), Instant::now() + Duration::from_secs(1))
                .expect("exchange");
            let original = exchange
                .wait_with(|progress| {
                    let at = if phase == LocalControlExchangePhase::ReadingHeader {
                        progress.header_offset
                    } else {
                        progress.body_offset
                    };
                    if progress.phase == phase && at >= offset {
                        LocalControlExchangeDecision::Cancel
                    } else {
                        LocalControlExchangeDecision::Continue
                    }
                })
                .expect_err("abandoned inside exact frame");
            assert_eq!(original.progress.phase, phase);
            let deadline = Instant::now() + Duration::from_millis(50);
            exchange
                .discard_response_until(
                    deadline,
                    |_, remaining| {
                        assert!(!remaining.is_zero() && remaining <= Duration::from_millis(50));
                        Ok(())
                    },
                    |_| true,
                )
                .expect("discard exact response");
            assert!(!client.requires_reconnect());
            assert_eq!(client.stream().input.position(), first.len() as u64);
            assert_eq!(client.pending_bytes(), 0);
            let first_request = frame(
                &encode_request(&make_request(42), limits()).expect("encode"),
                limits(),
            )
            .expect("frame");
            assert_eq!(
                client.stream().writes,
                first_request,
                "no replay or extra request"
            );
            assert_eq!(
                client
                    .begin_exchange(&make_request(43), Instant::now() + Duration::from_secs(1))
                    .expect("same client")
                    .wait_with(|_| LocalControlExchangeDecision::Continue)
                    .expect("next exact reply"),
                response(43)
            );
        }
    }

    #[test]
    fn affine_discard_rejects_unrelated_response_without_queueing_or_reading_successor() {
        let wrong = wire_response(77);
        let mut input = wrong.clone();
        input.extend(wire_response(42));
        let mut client = LocalControlClient::new(PausingStream::new(input), limits());
        let mut exchange = client
            .begin_exchange(&make_request(42), Instant::now() + Duration::from_secs(1))
            .expect("exchange");
        exchange
            .wait_with(|p| {
                if p.phase == LocalControlExchangePhase::ReadingHeader {
                    LocalControlExchangeDecision::Cancel
                } else {
                    LocalControlExchangeDecision::Continue
                }
            })
            .expect_err("stop before response");
        let error = exchange
            .discard_response_until(
                Instant::now() + Duration::from_millis(50),
                |_, _| Ok(()),
                |_| true,
            )
            .expect_err("wrong correlation");
        assert!(matches!(
            error.failure,
            LocalControlExchangeFailure::Protocol(_)
        ));
        assert!(client.requires_reconnect());
        assert_eq!(client.pending_bytes(), 0);
        assert_eq!(client.stream().input.position(), wrong.len() as u64);
    }

    #[test]
    fn affine_discard_missing_or_partial_payload_expires_and_retires() {
        for cut in [0, 2, 5] {
            let mut stream = PausingStream::new(wire_response(42));
            stream.read_stall_after = Some(cut);
            let mut client = LocalControlClient::new(stream, limits());
            let mut exchange = client
                .begin_exchange(&make_request(42), Instant::now() + Duration::from_secs(1))
                .expect("exchange");
            exchange
                .wait_with(|p| {
                    if p.phase == LocalControlExchangePhase::ReadingHeader {
                        LocalControlExchangeDecision::Cancel
                    } else {
                        LocalControlExchangeDecision::Continue
                    }
                })
                .expect_err("stop before response");
            let started = Instant::now();
            let error = exchange
                .discard_response_until(
                    started + Duration::from_millis(20),
                    |_, _| Ok(()),
                    |_| true,
                )
                .expect_err("cleanup expired");
            assert_eq!(error.failure, LocalControlExchangeFailure::Stalled);
            assert!(client.requires_reconnect());
            assert!(
                started.elapsed() < Duration::from_millis(250),
                "bounded native-free cleanup"
            );
        }
    }

    #[test]
    fn affine_discard_refuses_partial_send_and_invalid_or_oversized_payloads() {
        let mut stream = PausingStream::new(wire_response(42));
        stream.write_stall_after = Some(2);
        let mut client = LocalControlClient::new(stream, limits());
        let mut exchange = client
            .begin_exchange(&make_request(42), Instant::now() + Duration::from_secs(1))
            .expect("exchange");
        exchange
            .wait_with(|p| {
                if p.write_offset == 2 {
                    LocalControlExchangeDecision::Cancel
                } else {
                    LocalControlExchangeDecision::Continue
                }
            })
            .expect_err("partial send");
        exchange
            .discard_response_until(
                Instant::now() + Duration::from_millis(50),
                |_, _| Ok(()),
                |_| true,
            )
            .expect_err("not fully flushed");
        assert!(client.requires_reconnect());
        assert_eq!(client.stream().writes.len(), 2);
        assert_eq!(client.stream().read_calls, 0);
        for input in [u32::MAX.to_be_bytes().to_vec(), vec![0, 0, 0, 1, 0]] {
            let mut client = LocalControlClient::new(PausingStream::new(input), limits());
            let mut exchange = client
                .begin_exchange(&make_request(42), Instant::now() + Duration::from_secs(1))
                .expect("exchange");
            exchange
                .wait_with(|p| {
                    if p.phase == LocalControlExchangePhase::ReadingHeader {
                        LocalControlExchangeDecision::Cancel
                    } else {
                        LocalControlExchangeDecision::Continue
                    }
                })
                .expect_err("stop");
            let error = exchange
                .discard_response_until(
                    Instant::now() + Duration::from_millis(50),
                    |_, _| Ok(()),
                    |_| true,
                )
                .expect_err("invalid bounded response");
            assert!(matches!(
                error.failure,
                LocalControlExchangeFailure::Protocol(_)
            ));
            assert!(client.requires_reconnect());
        }
    }

    #[test]
    fn affine_discard_timeout_configuration_failure_never_preserves_a_socket() {
        let mut client = LocalControlClient::new(PausingStream::new(wire_response(42)), limits());
        let mut exchange = client
            .begin_exchange(&make_request(42), Instant::now() + Duration::from_secs(1))
            .expect("exchange");
        exchange
            .wait_with(|p| {
                if p.phase == LocalControlExchangePhase::ReadingHeader {
                    LocalControlExchangeDecision::Cancel
                } else {
                    LocalControlExchangeDecision::Continue
                }
            })
            .expect_err("stop");
        let error = exchange
            .discard_response_until(
                Instant::now() + Duration::from_millis(50),
                |_, _| Err(ErrorKind::PermissionDenied.into()),
                |_| true,
            )
            .expect_err("configuration failed");
        assert_eq!(
            error.failure,
            LocalControlExchangeFailure::Io(ErrorKind::PermissionDenied)
        );
        assert!(client.requires_reconnect());
        assert_eq!(client.stream().read_calls, 0);
    }
}
