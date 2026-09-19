//! Typestate and erased state machines for persistent authority sessions.

use crate::{FrameKind, InputManifestId, ProcessError, SessionFrame, SessionId};
use std::marker::PhantomData;

/// Cold session phase.  No request can be emitted until the handshake starts.
#[derive(Debug)]
pub struct Cold {
    _private: (),
}

/// Handshaking session phase.  Only the matching protocol hello can advance it.
#[derive(Debug)]
pub struct Handshaking {
    _private: (),
}

/// Ready session phase.  It has no request in flight.
#[derive(Debug)]
pub struct Ready {
    _private: (),
}

/// Pending session phase.  Exactly one request must be answered or cancelled.
#[derive(Debug)]
pub struct Pending {
    _private: (),
}

/// Broken session phase requiring a fresh cold fallback.
#[derive(Debug)]
pub struct Broken {
    _private: (),
}

/// A request token that can only be answered with its exact roots once.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct RequestToken {
    sequence: u64,
    manifest: InputManifestId,
    revision: u64,
}

impl std::fmt::Debug for RequestToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestToken")
            .field("sequence", &self.sequence)
            .field("manifest", &self.manifest)
            .field("revision", &self.revision)
            .finish()
    }
}

impl RequestToken {
    /// Returns the request sequence.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Returns the exact input manifest bound to the request.
    #[must_use]
    pub const fn manifest(&self) -> InputManifestId {
        self.manifest
    }

    /// Returns the discovery revision bound to the request.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }
}

/// Typestate persistent authority session.
///
/// The phase parameter controls which transitions are available.  In
/// particular, a [`PersistentSession<Ready>`] cannot emit a second request
/// while the first request is pending; that state is represented by
/// [`PersistentSession<Pending>`].
pub struct PersistentSession<P> {
    key: SessionId,
    next_sequence: u64,
    pending: Option<RequestToken>,
    _phase: PhantomData<fn() -> P>,
}

impl<P> std::fmt::Debug for PersistentSession<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PersistentSession")
            .field("key", &self.key)
            .field("next_sequence", &self.next_sequence)
            .field("request_pending", &self.pending.is_some())
            .finish_non_exhaustive()
    }
}

impl PersistentSession<Cold> {
    /// Starts a cold session.
    #[must_use]
    pub const fn new(key: SessionId) -> Self {
        Self {
            key,
            next_sequence: 0,
            pending: None,
            _phase: PhantomData,
        }
    }

    /// Begins the protocol handshake.
    #[must_use]
    pub fn hello(self) -> (PersistentSession<Handshaking>, SessionFrame) {
        let key = self.key;
        (
            PersistentSession {
                key,
                next_sequence: 0,
                pending: None,
                _phase: PhantomData,
            },
            SessionFrame::hello(key),
        )
    }
}

impl PersistentSession<Handshaking> {
    /// Completes the handshake with a matching hello frame.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::Protocol`] when `frame` is not a hello for the
    /// session's key.
    pub fn ready(self, frame: SessionFrame) -> Result<PersistentSession<Ready>, ProcessError> {
        if frame.kind() == FrameKind::Hello && frame.hello_session() == Some(self.key) {
            Ok(PersistentSession {
                key: self.key,
                next_sequence: 0,
                pending: None,
                _phase: PhantomData,
            })
        } else {
            Err(ProcessError::Protocol)
        }
    }

    /// Marks a handshaking session broken after an invalid peer frame.
    #[must_use]
    pub fn into_broken(self) -> PersistentSession<Broken> {
        PersistentSession {
            key: self.key,
            next_sequence: 0,
            pending: None,
            _phase: PhantomData,
        }
    }
}

impl PersistentSession<Ready> {
    /// Creates one bounded in-flight request token and enters pending phase.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::Protocol`] when the session is not ready to
    /// accept a request.
    pub fn request(
        self,
        manifest: InputManifestId,
        revision: u64,
    ) -> Result<(PersistentSession<Pending>, RequestToken, SessionFrame), ProcessError> {
        let sequence = self.next_sequence;
        let token = RequestToken {
            sequence,
            manifest,
            revision,
        };
        let pending = RequestToken {
            sequence,
            manifest,
            revision,
        };
        Ok((
            PersistentSession {
                key: self.key,
                next_sequence: self.next_sequence,
                pending: Some(pending),
                _phase: PhantomData,
            },
            token,
            SessionFrame::request(sequence, manifest, revision),
        ))
    }
}

impl PersistentSession<Pending> {
    /// Accepts a matching response and returns to ready phase.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::Protocol`] for a mismatched response or
    /// [`ProcessError::SequenceExhausted`] when the sequence cannot advance.
    pub fn response(
        mut self,
        token: RequestToken,
        frame: SessionFrame,
    ) -> Result<PersistentSession<Ready>, ProcessError> {
        let Some(expected) = self.pending.take() else {
            return Err(ProcessError::Protocol);
        };
        if frame.kind() != FrameKind::Response
            || frame.sequence() != Some(token.sequence)
            || frame.manifest() != Some(token.manifest)
            || frame.revision() != Some(token.revision)
            || expected.sequence != token.sequence
            || expected.manifest != token.manifest
            || expected.revision != token.revision
        {
            return Err(ProcessError::Protocol);
        }
        let next_sequence = token
            .sequence
            .checked_add(1)
            .ok_or(ProcessError::SequenceExhausted)?;
        Ok(PersistentSession {
            key: self.key,
            next_sequence,
            pending: None,
            _phase: PhantomData,
        })
    }

    /// Emits a matching cancellation frame and returns to ready phase.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::Protocol`] when no request is pending.
    pub fn cancel(mut self) -> Result<(PersistentSession<Ready>, SessionFrame), ProcessError> {
        let Some(token) = self.pending.take() else {
            return Err(ProcessError::Protocol);
        };
        Ok((
            PersistentSession {
                key: self.key,
                next_sequence: self.next_sequence,
                pending: None,
                _phase: PhantomData,
            },
            SessionFrame::cancel(token.sequence),
        ))
    }

    /// Discards a pending session after a process crash or protocol fault.
    #[must_use]
    pub fn into_broken(self) -> PersistentSession<Broken> {
        PersistentSession {
            key: self.key,
            next_sequence: 0,
            pending: None,
            _phase: PhantomData,
        }
    }
}

impl PersistentSession<Broken> {
    /// Explicitly falls back to a fresh cold session.
    #[must_use]
    pub fn fallback(self) -> PersistentSession<Cold> {
        PersistentSession::new(self.key)
    }
}

impl<P> PersistentSession<P> {
    /// Returns the session identity shared by every typestate phase.
    #[must_use]
    pub const fn key(&self) -> SessionId {
        self.key
    }
}

/// Erased session lifecycle used by process supervisors receiving dynamic
/// frames from a peer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ErasedSession {
    key: SessionId,
    state: SessionState,
    next_sequence: u64,
    pending: Option<(u64, InputManifestId, u64)>,
}

impl ErasedSession {
    /// Starts a cold session requiring a handshake.
    #[must_use]
    pub const fn new(key: SessionId) -> Self {
        Self {
            key,
            state: SessionState::Cold,
            next_sequence: 0,
            pending: None,
        }
    }

    /// Returns the current dynamic lifecycle state.
    #[must_use]
    pub const fn state(&self) -> SessionState {
        self.state
    }

    /// Emits the handshake and enters handshaking state.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::Protocol`] when the session is not cold.
    pub fn hello(&mut self) -> Result<SessionFrame, ProcessError> {
        if self.state != SessionState::Cold {
            return Err(ProcessError::Protocol);
        }
        self.state = SessionState::Handshaking;
        Ok(SessionFrame::hello(self.key))
    }

    /// Queues one request after a successful handshake.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::Protocol`] when the session is not ready or
    /// already has a pending request. The caller is responsible for pairing
    /// the manifest with its session key.
    pub fn request(
        &mut self,
        manifest: InputManifestId,
        revision: u64,
    ) -> Result<SessionFrame, ProcessError> {
        if self.state != SessionState::Ready || self.pending.is_some() {
            return Err(ProcessError::Protocol);
        }
        let sequence = self.next_sequence;
        self.pending = Some((sequence, manifest, revision));
        Ok(SessionFrame::request(sequence, manifest, revision))
    }

    /// Receives and validates the response for the pending request.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::Protocol`] for a missing or mismatched pending
    /// response, or [`ProcessError::SequenceExhausted`] when the sequence
    /// cannot advance.
    pub fn response(&mut self, frame: SessionFrame) -> Result<(), ProcessError> {
        let Some((sequence, manifest, revision)) = self.pending else {
            self.state = SessionState::Broken;
            return Err(ProcessError::Protocol);
        };
        if self.state != SessionState::Ready
            || frame.kind() != FrameKind::Response
            || frame.sequence() != Some(sequence)
            || frame.manifest() != Some(manifest)
            || frame.revision() != Some(revision)
        {
            self.state = SessionState::Broken;
            return Err(ProcessError::Protocol);
        }
        self.pending = None;
        let Some(next_sequence) = self.next_sequence.checked_add(1) else {
            self.state = SessionState::Broken;
            return Err(ProcessError::SequenceExhausted);
        };
        self.next_sequence = next_sequence;
        Ok(())
    }

    /// Accepts a validated peer frame and advances the dynamic lifecycle.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::Protocol`] when the frame is not legal in the
    /// current lifecycle state.
    pub fn accept(&mut self, frame: SessionFrame) -> Result<(), ProcessError> {
        match frame.kind() {
            FrameKind::Hello
                if self.state == SessionState::Handshaking
                    && frame.hello_session() == Some(self.key) =>
            {
                self.state = SessionState::Ready;
                Ok(())
            }
            FrameKind::Response => self.response(frame),
            FrameKind::Reset => {
                self.state = SessionState::Cold;
                self.pending = None;
                self.next_sequence = 0;
                Ok(())
            }
            FrameKind::Cancel
                if self.state == SessionState::Ready
                    && self
                        .pending
                        .is_some_and(|pending| frame.sequence() == Some(pending.0)) =>
            {
                self.pending = None;
                self.state = SessionState::Ready;
                Ok(())
            }
            _ => {
                self.state = SessionState::Broken;
                Err(ProcessError::Protocol)
            }
        }
    }

    /// Decodes and admits one wire frame against this session's expected
    /// identities before applying the dynamic transition.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::Protocol`] when the wire bytes are malformed,
    /// identities do not match, or the lifecycle transition is invalid.
    pub fn accept_wire(&mut self, bytes: &[u8]) -> Result<(), ProcessError> {
        let expected_session = (self.state == SessionState::Handshaking).then_some(self.key);
        let expected_manifest = self.pending.map(|pending| pending.1);
        let Ok(frame) = SessionFrame::decode(bytes)
            .and_then(|wire| wire.admit(expected_session, expected_manifest))
        else {
            self.state = SessionState::Broken;
            return Err(ProcessError::Protocol);
        };
        self.accept(frame)
    }

    /// Drops a broken session's pending request and returns to cold fallback.
    pub fn fallback_to_cold(&mut self) {
        self.state = SessionState::Cold;
        self.pending = None;
        self.next_sequence = 0;
    }

    /// Returns whether a cold executor should be used after a crash or fault.
    #[must_use]
    pub const fn requires_cold_fallback(&self) -> bool {
        matches!(self.state, SessionState::Cold | SessionState::Broken)
    }

    /// Returns the session identity.
    #[must_use]
    pub const fn key(&self) -> SessionId {
        self.key
    }

    /// Returns whether a request is currently awaiting a response.
    #[must_use]
    pub const fn has_pending_request(&self) -> bool {
        self.pending.is_some()
    }
}

/// Dynamic lifecycle states for an erased session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionState {
    /// No handshake has been sent.
    Cold,
    /// A handshake has been sent and is awaiting admission.
    Handshaking,
    /// The peer is admitted and no response is currently pending.
    Ready,
    /// A protocol or process failure requires cold fallback.
    Broken,
}
