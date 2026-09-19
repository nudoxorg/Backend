use std::fmt;
#[cfg(unix)]
use std::fs::File;
use std::io::{self, Read, Write};

use super::{
    AuthenticatedTcpStream, MAGIC, NONCE_BYTES, STATUS_OK, STATUS_REJECTED, TOKEN_BYTES, VERSION,
    constant_time_eq, session_for_client, session_for_server, token,
};

/// Errors raised before a TCP stream enters the replication protocol.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TcpHandshakeError {
    /// The peer closed or sent an incomplete handshake.
    Truncated,
    /// The peer used an unsupported handshake envelope.
    InvalidEnvelope,
    /// The peer did not prove possession of the configured credential.
    AuthenticationFailed,
    /// The host cannot provide a bounded nonce source.
    NonceUnavailable,
    /// The stream failed while exchanging the handshake.
    Io(io::ErrorKind),
    /// The authenticated record allocation bound is invalid.
    InvalidRecordLimit,
}

impl fmt::Display for TcpHandshakeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => formatter.write_str("truncated TCP authority handshake"),
            Self::InvalidEnvelope => formatter.write_str("invalid TCP authority handshake"),
            Self::AuthenticationFailed => {
                formatter.write_str("TCP authority handshake authentication failed")
            }
            Self::NonceUnavailable => formatter.write_str("TCP handshake nonce source unavailable"),
            Self::Io(kind) => write!(formatter, "TCP handshake I/O failed: {kind:?}"),
            Self::InvalidRecordLimit => {
                formatter.write_str("invalid authenticated TCP record limit")
            }
        }
    }
}

impl std::error::Error for TcpHandshakeError {}

/// Owner credential used to authenticate a cross-host worker stream.
///
/// The key is copied into the handshake helper and never serialized.  Keep
/// this value in the owner process only; the peer receives challenge tokens
/// derived from it and cannot recover the credential from the wire.
#[derive(Clone)]
pub struct TcpAuthority {
    secret: [u8; 32],
}

impl PartialEq for TcpAuthority {
    fn eq(&self, other: &Self) -> bool {
        self.secret == other.secret
    }
}

impl Eq for TcpAuthority {}

impl Drop for TcpAuthority {
    fn drop(&mut self) {
        self.secret = [0_u8; 32];
    }
}

impl fmt::Debug for TcpAuthority {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("TcpAuthority(..)")
    }
}

impl TcpAuthority {
    /// Creates a handshake authority from an owner-supplied 32-byte secret.
    #[must_use]
    pub fn new(secret: [u8; 32]) -> Self {
        Self { secret }
    }

    /// Performs only the client side of the mutual possession handshake.
    ///
    /// This method proves authority possession but leaves the byte stream
    /// unauthenticated. Call [`Self::client_handshake_authenticated`] before
    /// sending or accepting replication frames.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn client_handshake<S: Read + Write>(
        &self,
        stream: &mut S,
    ) -> Result<(), TcpHandshakeError> {
        let nonce = fresh_nonce()?;
        self.client_handshake_with_nonce(stream, nonce)
    }

    /// Performs the client handshake and returns a stream that authenticates
    /// every post-handshake record.
    ///
    /// `max_record` bounds the complete byte sequence supplied by one
    /// replication outer frame (including its four-byte length prefix). The
    /// caller should derive it from the negotiated transport frame budget.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn client_handshake_authenticated<S: Read + Write>(
        &self,
        mut stream: S,
        max_record: usize,
    ) -> Result<AuthenticatedTcpStream<S>, TcpHandshakeError> {
        let client_nonce = fresh_nonce()?;
        let server_nonce = self.client_handshake_session(&mut stream, client_nonce)?;
        let session = session_for_client(&self.secret, &server_nonce, &client_nonce);
        AuthenticatedTcpStream::new(stream, &session, max_record)
    }

    /// Performs only the server side of the mutual possession handshake.
    ///
    /// This method proves authority possession but leaves the byte stream
    /// unauthenticated. Call [`Self::server_handshake_authenticated`] before
    /// sending or accepting replication frames.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn server_handshake<S: Read + Write>(
        &self,
        stream: &mut S,
    ) -> Result<(), TcpHandshakeError> {
        let server_nonce = fresh_nonce()?;
        stream
            .write_all(&MAGIC)
            .and_then(|()| stream.write_all(&[VERSION]))
            .and_then(|()| stream.write_all(&server_nonce))
            .and_then(|()| stream.flush())
            .map_err(|error| map_io(&error))?;
        self.server_handshake_after_challenge(stream, server_nonce)
    }

    /// Performs the server handshake and returns a stream that authenticates
    /// every post-handshake record.
    ///
    /// `max_record` bounds the complete byte sequence supplied by one
    /// replication outer frame (including its four-byte length prefix).
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn server_handshake_authenticated<S: Read + Write>(
        &self,
        mut stream: S,
        max_record: usize,
    ) -> Result<AuthenticatedTcpStream<S>, TcpHandshakeError> {
        let server_nonce = fresh_nonce()?;
        stream
            .write_all(&MAGIC)
            .and_then(|()| stream.write_all(&[VERSION]))
            .and_then(|()| stream.write_all(&server_nonce))
            .and_then(|()| stream.flush())
            .map_err(|error| map_io(&error))?;
        let client_nonce = self.server_handshake_session(&mut stream, server_nonce)?;
        let session = session_for_server(&self.secret, &server_nonce, &client_nonce);
        AuthenticatedTcpStream::new(stream, &session, max_record)
    }

    pub(super) fn server_handshake_after_challenge<S: Read + Write>(
        &self,
        stream: &mut S,
        server_nonce: [u8; NONCE_BYTES],
    ) -> Result<(), TcpHandshakeError> {
        let _ = self.server_handshake_session(stream, server_nonce)?;
        Ok(())
    }

    fn server_handshake_session<S: Read + Write>(
        &self,
        stream: &mut S,
        server_nonce: [u8; NONCE_BYTES],
    ) -> Result<[u8; NONCE_BYTES], TcpHandshakeError> {
        let mut envelope = [0_u8; MAGIC.len() + 1 + (NONCE_BYTES * 2) + TOKEN_BYTES];
        read_exact(stream, &mut envelope)?;
        if envelope[..MAGIC.len()] != MAGIC || envelope[MAGIC.len()] != VERSION {
            reject(stream);
            return Err(TcpHandshakeError::InvalidEnvelope);
        }
        let nonce_start = MAGIC.len() + 1;
        let client_nonce_start = nonce_start + NONCE_BYTES;
        let token_start = client_nonce_start + NONCE_BYTES;
        if envelope[nonce_start..client_nonce_start] != server_nonce {
            reject(stream);
            return Err(TcpHandshakeError::AuthenticationFailed);
        }
        let mut client_nonce = [0_u8; NONCE_BYTES];
        client_nonce.copy_from_slice(&envelope[client_nonce_start..token_start]);
        let mut claimed = [0_u8; TOKEN_BYTES];
        claimed.copy_from_slice(&envelope[token_start..]);
        if !constant_time_eq(
            &claimed,
            &token(&self.secret, &server_nonce, &client_nonce, b"client"),
        ) {
            reject(stream);
            return Err(TcpHandshakeError::AuthenticationFailed);
        }
        stream
            .write_all(&[STATUS_OK])
            .and_then(|()| {
                stream.write_all(&token(
                    &self.secret,
                    &server_nonce,
                    &client_nonce,
                    b"server",
                ))
            })
            .and_then(|()| stream.flush())
            .map_err(|error| map_io(&error))?;
        Ok(client_nonce)
    }

    pub(super) fn client_handshake_with_nonce<S: Read + Write>(
        &self,
        stream: &mut S,
        client_nonce: [u8; NONCE_BYTES],
    ) -> Result<(), TcpHandshakeError> {
        let _ = self.client_handshake_session(stream, client_nonce)?;
        Ok(())
    }

    fn client_handshake_session<S: Read + Write>(
        &self,
        stream: &mut S,
        client_nonce: [u8; NONCE_BYTES],
    ) -> Result<[u8; NONCE_BYTES], TcpHandshakeError> {
        let mut challenge = [0_u8; MAGIC.len() + 1 + NONCE_BYTES];
        read_exact(stream, &mut challenge)?;
        if challenge[..MAGIC.len()] != MAGIC || challenge[MAGIC.len()] != VERSION {
            return Err(TcpHandshakeError::InvalidEnvelope);
        }
        let mut server_nonce = [0_u8; NONCE_BYTES];
        server_nonce.copy_from_slice(&challenge[MAGIC.len() + 1..]);
        stream
            .write_all(&MAGIC)
            .and_then(|()| stream.write_all(&[VERSION]))
            .and_then(|()| stream.write_all(&server_nonce))
            .and_then(|()| stream.write_all(&client_nonce))
            .and_then(|()| {
                stream.write_all(&token(
                    &self.secret,
                    &server_nonce,
                    &client_nonce,
                    b"client",
                ))
            })
            .and_then(|()| stream.flush())
            .map_err(|error| map_io(&error))?;
        let mut status = [0_u8; 1];
        read_exact(stream, &mut status)?;
        if status[0] != STATUS_OK {
            return Err(TcpHandshakeError::AuthenticationFailed);
        }
        let mut response = [0_u8; TOKEN_BYTES];
        read_exact(stream, &mut response)?;
        if !constant_time_eq(
            &response,
            &token(&self.secret, &server_nonce, &client_nonce, b"server"),
        ) {
            return Err(TcpHandshakeError::AuthenticationFailed);
        }
        Ok(server_nonce)
    }
}

fn read_exact(reader: &mut impl Read, bytes: &mut [u8]) -> Result<(), TcpHandshakeError> {
    reader.read_exact(bytes).map_err(|error| {
        if error.kind() == io::ErrorKind::UnexpectedEof {
            TcpHandshakeError::Truncated
        } else {
            TcpHandshakeError::Io(error.kind())
        }
    })
}

fn map_io(error: &io::Error) -> TcpHandshakeError {
    TcpHandshakeError::Io(error.kind())
}

fn reject(stream: &mut impl Write) {
    let _ = stream.write_all(&[STATUS_REJECTED]);
    let _ = stream.flush();
}

#[cfg(unix)]
fn fresh_nonce() -> Result<[u8; NONCE_BYTES], TcpHandshakeError> {
    let mut nonce = [0_u8; NONCE_BYTES];
    File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut nonce))
        .map_err(|_| TcpHandshakeError::NonceUnavailable)?;
    Ok(nonce)
}

#[cfg(windows)]
fn fresh_nonce() -> Result<[u8; NONCE_BYTES], TcpHandshakeError> {
    let mut nonce = [0_u8; NONCE_BYTES];
    backend_platform::win32::random::fill(&mut nonce)
        .map_err(|_| TcpHandshakeError::NonceUnavailable)?;
    Ok(nonce)
}

#[cfg(not(any(unix, windows)))]
fn fresh_nonce() -> Result<[u8; NONCE_BYTES], TcpHandshakeError> {
    Err(TcpHandshakeError::NonceUnavailable)
}
