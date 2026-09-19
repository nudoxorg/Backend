//! Validated and untrusted representations of authority session frames.

use crate::errors::FrameError;
use crate::{InputManifestId, SessionId};

/// Current authority-session protocol version.
pub const PROTOCOL_VERSION: u16 = 1;
/// Maximum encoded session-frame size.
pub const MAX_FRAME_BYTES: usize = 6 + 8 + 32 + 8;
const MAGIC: [u8; 3] = *b"BCF";
const HEADER_BYTES: usize = 6;
const HELLO_KIND: u8 = 1;
const REQUEST_KIND: u8 = 2;
const CANCEL_KIND: u8 = 3;
const RESET_KIND: u8 = 4;
const RESPONSE_KIND: u8 = 5;

/// The kind of a validated authority session frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameKind {
    /// Handshake frame.
    Hello,
    /// Extraction request frame.
    Request,
    /// Cooperative cancellation frame.
    Cancel,
    /// Explicit state-reset frame.
    Reset,
    /// Extraction response frame.
    Response,
}

/// The fixed protocol version carried by a validated frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionFrameVersion(u16);

impl SessionFrameVersion {
    /// Returns the protocol version.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FrameBody {
    Hello {
        session: SessionId,
    },
    Request {
        sequence: u64,
        manifest: InputManifestId,
        revision: u64,
    },
    Cancel {
        sequence: u64,
    },
    Reset,
    Response {
        sequence: u64,
        manifest: InputManifestId,
        revision: u64,
    },
}

/// An opaque, validated authority-session frame.
///
/// Frames can only be created through the constructors below or admitted by
/// [`UntrustedSessionFrame::admit`].  The wire version and kind tags are fixed
/// for every in-memory frame, while request/response roots remain explicit
/// and auditable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionFrame {
    body: FrameBody,
}

impl SessionFrame {
    /// Creates the protocol handshake for `session`.
    #[must_use]
    pub const fn hello(session: SessionId) -> Self {
        Self {
            body: FrameBody::Hello { session },
        }
    }

    /// Creates an extraction request bound to a manifest and revision.
    #[must_use]
    pub const fn request(sequence: u64, manifest: InputManifestId, revision: u64) -> Self {
        Self {
            body: FrameBody::Request {
                sequence,
                manifest,
                revision,
            },
        }
    }

    /// Creates a cooperative cancellation request.
    #[must_use]
    pub const fn cancel(sequence: u64) -> Self {
        Self {
            body: FrameBody::Cancel { sequence },
        }
    }

    /// Creates an explicit session reset request.
    #[must_use]
    pub const fn reset() -> Self {
        Self {
            body: FrameBody::Reset,
        }
    }

    /// Creates an extraction response bound to its request.
    #[must_use]
    pub const fn response(sequence: u64, manifest: InputManifestId, revision: u64) -> Self {
        Self {
            body: FrameBody::Response {
                sequence,
                manifest,
                revision,
            },
        }
    }

    /// Returns the protocol version carried by this frame.
    #[must_use]
    pub const fn version(&self) -> SessionFrameVersion {
        SessionFrameVersion(PROTOCOL_VERSION)
    }

    /// Returns this frame's validated kind.
    #[must_use]
    pub const fn kind(&self) -> FrameKind {
        match self.body {
            FrameBody::Hello { .. } => FrameKind::Hello,
            FrameBody::Request { .. } => FrameKind::Request,
            FrameBody::Cancel { .. } => FrameKind::Cancel,
            FrameBody::Reset => FrameKind::Reset,
            FrameBody::Response { .. } => FrameKind::Response,
        }
    }

    /// Returns the handshake session identity, if this is a hello frame.
    #[must_use]
    pub const fn hello_session(&self) -> Option<SessionId> {
        match self.body {
            FrameBody::Hello { session } => Some(session),
            _ => None,
        }
    }

    /// Returns the sequence number, if this frame carries one.
    #[must_use]
    pub const fn sequence(&self) -> Option<u64> {
        match self.body {
            FrameBody::Request { sequence, .. }
            | FrameBody::Cancel { sequence }
            | FrameBody::Response { sequence, .. } => Some(sequence),
            FrameBody::Hello { .. } | FrameBody::Reset => None,
        }
    }

    /// Returns the manifest root, if this frame carries one.
    #[must_use]
    pub const fn manifest(&self) -> Option<InputManifestId> {
        match self.body {
            FrameBody::Request { manifest, .. } | FrameBody::Response { manifest, .. } => {
                Some(manifest)
            }
            FrameBody::Hello { .. } | FrameBody::Cancel { .. } | FrameBody::Reset => None,
        }
    }

    /// Returns the discovery revision, if this frame carries one.
    #[must_use]
    pub const fn revision(&self) -> Option<u64> {
        match self.body {
            FrameBody::Request { revision, .. } | FrameBody::Response { revision, .. } => {
                Some(revision)
            }
            FrameBody::Hello { .. } | FrameBody::Cancel { .. } | FrameBody::Reset => None,
        }
    }

    /// Encodes this validated frame in the bounded wire format.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        encode_body(self.kind(), &self.body)
    }

    /// Decodes one frame envelope without guessing the meaning of raw IDs.
    ///
    /// The returned [`UntrustedSessionFrame`] retains identity bytes as
    /// untrusted claims.  A receiver must call [`UntrustedSessionFrame::admit`]
    /// with the typed session/manifest identities it already expects before a
    /// [`SessionFrame`] can be created.
    ///
    /// # Errors
    ///
    /// Returns [`FrameError`] when the bytes are truncated, malformed, or
    /// carry an unsupported frame kind or version.
    pub fn decode(bytes: &[u8]) -> Result<UntrustedSessionFrame, FrameError> {
        decode_untrusted(bytes)
    }

    /// Decodes and admits a frame against caller-owned expected identities.
    ///
    /// # Errors
    ///
    /// Returns [`FrameError`] when decoding fails or an identity claim does
    /// not match the supplied expectation.
    pub fn decode_for(
        bytes: &[u8],
        expected_session: Option<SessionId>,
        expected_manifest: Option<InputManifestId>,
    ) -> Result<Self, FrameError> {
        Self::decode(bytes)?.admit(expected_session, expected_manifest)
    }
}

/// A structurally valid frame whose identity claims have not yet been admitted.
///
/// This type is produced by [`SessionFrame::decode`].  It cannot be passed to
/// session state transitions until its IDs are compared with caller-owned
/// expected identities.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UntrustedSessionFrame {
    body: UntrustedBody,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum UntrustedBody {
    Hello {
        session: [u8; 32],
    },
    Request {
        sequence: u64,
        manifest: [u8; 32],
        revision: u64,
    },
    Cancel {
        sequence: u64,
    },
    Reset,
    Response {
        sequence: u64,
        manifest: [u8; 32],
        revision: u64,
    },
}

impl UntrustedSessionFrame {
    /// Returns the structurally validated frame kind.
    #[must_use]
    pub const fn kind(&self) -> FrameKind {
        match self.body {
            UntrustedBody::Hello { .. } => FrameKind::Hello,
            UntrustedBody::Request { .. } => FrameKind::Request,
            UntrustedBody::Cancel { .. } => FrameKind::Cancel,
            UntrustedBody::Reset => FrameKind::Reset,
            UntrustedBody::Response { .. } => FrameKind::Response,
        }
    }

    /// Returns a sequence claim, if the frame carries one.
    #[must_use]
    pub const fn sequence(&self) -> Option<u64> {
        match self.body {
            UntrustedBody::Request { sequence, .. }
            | UntrustedBody::Cancel { sequence }
            | UntrustedBody::Response { sequence, .. } => Some(sequence),
            UntrustedBody::Hello { .. } | UntrustedBody::Reset => None,
        }
    }

    /// Returns the raw hello identity claim, if present.
    #[must_use]
    pub const fn session_bytes(&self) -> Option<&[u8; 32]> {
        match &self.body {
            UntrustedBody::Hello { session } => Some(session),
            _ => None,
        }
    }

    /// Returns the raw manifest identity claim, if present.
    #[must_use]
    pub const fn manifest_bytes(&self) -> Option<&[u8; 32]> {
        match &self.body {
            UntrustedBody::Request { manifest, .. } | UntrustedBody::Response { manifest, .. } => {
                Some(manifest)
            }
            _ => None,
        }
    }

    /// Returns a discovery revision claim, if present.
    #[must_use]
    pub const fn revision(&self) -> Option<u64> {
        match self.body {
            UntrustedBody::Request { revision, .. } | UntrustedBody::Response { revision, .. } => {
                Some(revision)
            }
            UntrustedBody::Hello { .. } | UntrustedBody::Cancel { .. } | UntrustedBody::Reset => {
                None
            }
        }
    }

    /// Compares every identity claim with caller-owned expected IDs.
    ///
    /// # Errors
    ///
    /// Returns [`FrameError::InvalidIdentity`] when a required expected ID is
    /// absent or differs from the frame claim.
    pub fn admit(
        self,
        expected_session: Option<SessionId>,
        expected_manifest: Option<InputManifestId>,
    ) -> Result<SessionFrame, FrameError> {
        match self.body {
            UntrustedBody::Hello { session } => {
                let expected = expected_session.ok_or(FrameError::InvalidIdentity)?;
                if expected.to_bytes() != session {
                    return Err(FrameError::InvalidIdentity);
                }
                Ok(SessionFrame::hello(expected))
            }
            UntrustedBody::Request {
                sequence,
                manifest,
                revision,
            } => {
                let expected = expected_manifest.ok_or(FrameError::InvalidIdentity)?;
                if expected.to_bytes() != manifest {
                    return Err(FrameError::InvalidIdentity);
                }
                Ok(SessionFrame::request(sequence, expected, revision))
            }
            UntrustedBody::Cancel { sequence } => Ok(SessionFrame::cancel(sequence)),
            UntrustedBody::Reset => Ok(SessionFrame::reset()),
            UntrustedBody::Response {
                sequence,
                manifest,
                revision,
            } => {
                let expected = expected_manifest.ok_or(FrameError::InvalidIdentity)?;
                if expected.to_bytes() != manifest {
                    return Err(FrameError::InvalidIdentity);
                }
                Ok(SessionFrame::response(sequence, expected, revision))
            }
        }
    }
}

impl TryFrom<&[u8]> for UntrustedSessionFrame {
    type Error = FrameError;

    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        SessionFrame::decode(bytes)
    }
}

fn encode_body(kind: FrameKind, body: &FrameBody) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(MAX_FRAME_BYTES);
    encoded.extend_from_slice(&MAGIC);
    encoded.extend_from_slice(&PROTOCOL_VERSION.to_be_bytes());
    encoded.push(kind_tag(kind));
    match body {
        FrameBody::Hello { session } => encoded.extend_from_slice(session.as_bytes()),
        FrameBody::Request {
            sequence,
            manifest,
            revision,
        }
        | FrameBody::Response {
            sequence,
            manifest,
            revision,
        } => {
            encoded.extend_from_slice(&sequence.to_be_bytes());
            encoded.extend_from_slice(manifest.as_bytes());
            encoded.extend_from_slice(&revision.to_be_bytes());
        }
        FrameBody::Cancel { sequence } => encoded.extend_from_slice(&sequence.to_be_bytes()),
        FrameBody::Reset => {}
    }
    encoded
}

fn decode_untrusted(bytes: &[u8]) -> Result<UntrustedSessionFrame, FrameError> {
    if bytes.len() < HEADER_BYTES {
        return Err(FrameError::Truncated);
    }
    if bytes[..MAGIC.len()] != MAGIC {
        return Err(FrameError::InvalidMagic);
    }
    let version = u16::from_be_bytes(bytes[3..5].try_into().map_err(|_| FrameError::Truncated)?);
    if version != PROTOCOL_VERSION {
        return Err(FrameError::UnsupportedVersion(version));
    }
    let kind = bytes[5];
    let expected = expected_len(kind)?;
    if bytes.len() != expected {
        return Err(FrameError::InvalidLength {
            expected,
            actual: bytes.len(),
        });
    }
    let payload = &bytes[HEADER_BYTES..];
    let body = match kind {
        HELLO_KIND => UntrustedBody::Hello {
            session: payload.try_into().map_err(|_| FrameError::Truncated)?,
        },
        REQUEST_KIND => {
            let (sequence, manifest, revision) = decode_request(payload)?;
            UntrustedBody::Request {
                sequence,
                manifest,
                revision,
            }
        }
        CANCEL_KIND => UntrustedBody::Cancel {
            sequence: read_u64(payload)?,
        },
        RESET_KIND => UntrustedBody::Reset,
        RESPONSE_KIND => {
            let (sequence, manifest, revision) = decode_request(payload)?;
            UntrustedBody::Response {
                sequence,
                manifest,
                revision,
            }
        }
        unknown => return Err(FrameError::UnknownKind(unknown)),
    };
    Ok(UntrustedSessionFrame { body })
}

const fn kind_tag(kind: FrameKind) -> u8 {
    match kind {
        FrameKind::Hello => HELLO_KIND,
        FrameKind::Request => REQUEST_KIND,
        FrameKind::Cancel => CANCEL_KIND,
        FrameKind::Reset => RESET_KIND,
        FrameKind::Response => RESPONSE_KIND,
    }
}

fn expected_len(kind: u8) -> Result<usize, FrameError> {
    match kind {
        HELLO_KIND => Ok(HEADER_BYTES + 32),
        REQUEST_KIND | RESPONSE_KIND => Ok(HEADER_BYTES + 8 + 32 + 8),
        CANCEL_KIND => Ok(HEADER_BYTES + 8),
        RESET_KIND => Ok(HEADER_BYTES),
        unknown => Err(FrameError::UnknownKind(unknown)),
    }
}

fn read_u64(bytes: &[u8]) -> Result<u64, FrameError> {
    let raw = bytes.get(..8).ok_or(FrameError::Truncated)?;
    Ok(u64::from_be_bytes(
        raw.try_into().map_err(|_| FrameError::Truncated)?,
    ))
}

fn decode_request(bytes: &[u8]) -> Result<(u64, [u8; 32], u64), FrameError> {
    let sequence = read_u64(bytes)?;
    let manifest = bytes
        .get(8..40)
        .ok_or(FrameError::Truncated)?
        .try_into()
        .map_err(|_| FrameError::Truncated)?;
    let revision = read_u64(bytes.get(40..48).ok_or(FrameError::Truncated)?)?;
    Ok((sequence, manifest, revision))
}
