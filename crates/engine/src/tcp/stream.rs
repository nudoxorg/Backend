use std::fmt;
use std::io::{self, Read, Write};

use super::handshake::TcpHandshakeError;
use super::{
    RECORD_HEADER_BYTES, RECORD_MAC_BYTES, RECORD_MAGIC, RECORD_VERSION, TcpSession,
    constant_time_eq,
};

const RECORD_SCRATCH_BYTES: usize = 8 * 1024;

/// A byte stream with authenticated, ordered records derived from the
/// authority handshake transcript.
///
/// The wrapper deliberately presents a normal Read and Write stream
/// to the canonical replication framing layer. Writes are accumulated until
/// `Write::flush` and then emitted as one authenticated record. Reads
/// verify one complete record before exposing any of its bytes.
pub struct AuthenticatedTcpStream<S> {
    stream: S,
    reader: ReaderState,
    writer: WriterState,
}

impl<S: fmt::Debug> fmt::Debug for AuthenticatedTcpStream<S> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthenticatedTcpStream")
            .field("max_record", &self.writer.max_record)
            .field("send_sequence", &self.writer.send_sequence)
            .field("recv_sequence", &self.reader.recv_sequence)
            .finish_non_exhaustive()
    }
}

impl<S> AuthenticatedTcpStream<S> {
    pub(super) fn new(
        stream: S,
        session: &TcpSession,
        max_record: usize,
    ) -> Result<Self, TcpHandshakeError> {
        if max_record == 0 || max_record > u32::MAX as usize {
            return Err(TcpHandshakeError::InvalidRecordLimit);
        }
        Ok(Self {
            stream,
            reader: ReaderState::from_session(session, max_record),
            writer: WriterState::from_session(session, max_record),
        })
    }

    /// Returns a mutable reference to the underlying socket for timeout and
    /// shutdown configuration.
    pub fn inner_mut(&mut self) -> &mut S {
        &mut self.stream
    }

    /// Returns a shared reference to the underlying socket.
    pub fn inner(&self) -> &S {
        &self.stream
    }

    /// Returns the underlying socket after the authenticated stream is
    /// closed. Pending writes must be flushed by the caller first.
    #[must_use]
    pub fn into_inner(self) -> S {
        self.stream
    }

    /// Consumes this stream and gives its read and write directions to two
    /// distinct owners.
    ///
    /// The closure is given a shared reference only so a socket type such as
    /// `std::net::TcpStream` can create two OS handles. The authenticated
    /// state itself is moved into the two halves, so a second writer cannot be
    /// minted with a copied sequence counter.
    ///
    /// ```compile_fail
    /// use backend_engine::AuthenticatedTcpStream;
    /// # use std::io;
    /// # fn split<S>(stream: AuthenticatedTcpStream<S>) -> io::Result<()> {
    /// let _first = stream.try_split_with(|_| {
    ///     Err(io::Error::other("example"))
    /// });
    /// let _second = stream.try_split_with(|_| {
    ///     Err(io::Error::other("the stream was moved"))
    /// });
    /// # Ok(()) }
    /// ```
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn try_split_with<SR, SW, F>(
        self,
        split: F,
    ) -> io::Result<(AuthenticatedReadHalf<SR>, AuthenticatedWriteHalf<SW>)>
    where
        F: FnOnce(&S) -> io::Result<(SR, SW)>,
    {
        if self.reader.has_buffered_bytes()
            || self.writer.has_buffered_bytes()
            || self.reader.failed
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "authenticated TCP stream has buffered data and cannot be split",
            ));
        }
        let Self {
            stream,
            reader,
            writer,
        } = self;
        let (read_stream, write_stream) = split(&stream)?;
        drop(stream);
        Ok((
            AuthenticatedReadHalf {
                stream: read_stream,
                state: reader,
            },
            AuthenticatedWriteHalf {
                stream: write_stream,
                state: writer,
            },
        ))
    }
}

impl<S: Read> Read for AuthenticatedTcpStream<S> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.reader.read_from(&mut self.stream, bytes)
    }
}

impl<S: Write> Write for AuthenticatedTcpStream<S> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.writer.write_to(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush_to(&mut self.stream)
    }
}

/// The read direction of an authenticated TCP stream.
///
/// This type implements Read only. It cannot be cloned, and consuming an
/// `AuthenticatedTcpStream` is the only way to create it.
///
/// ```compile_fail
/// use backend_engine::AuthenticatedReadHalf;
/// # fn only_reads<S>(mut half: AuthenticatedReadHalf<S>) {
/// let _: &mut dyn std::io::Write = &mut half;
/// # }
/// ```
pub struct AuthenticatedReadHalf<S> {
    stream: S,
    state: ReaderState,
}

impl<S: fmt::Debug> fmt::Debug for AuthenticatedReadHalf<S> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthenticatedReadHalf")
            .field("max_record", &self.state.max_record)
            .field("recv_sequence", &self.state.recv_sequence)
            .finish_non_exhaustive()
    }
}

impl<S> AuthenticatedReadHalf<S> {
    /// Returns the underlying socket after consuming the authenticated read
    /// half.
    #[must_use]
    pub fn into_inner(self) -> S {
        self.stream
    }
}

impl<S: Read> Read for AuthenticatedReadHalf<S> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.state.read_from(&mut self.stream, bytes)
    }
}

/// The write direction of an authenticated TCP stream.
///
/// This type implements Write only. It cannot be cloned, and consuming an
/// `AuthenticatedTcpStream` is the only way to create it.
pub struct AuthenticatedWriteHalf<S> {
    stream: S,
    state: WriterState,
}

impl<S: fmt::Debug> fmt::Debug for AuthenticatedWriteHalf<S> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthenticatedWriteHalf")
            .field("max_record", &self.state.max_record)
            .field("send_sequence", &self.state.send_sequence)
            .finish_non_exhaustive()
    }
}

impl<S> AuthenticatedWriteHalf<S> {
    /// Returns the underlying socket after consuming the authenticated write
    /// half.
    #[must_use]
    pub fn into_inner(self) -> S {
        self.stream
    }
}

impl<S: Write> Write for AuthenticatedWriteHalf<S> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.state.write_to(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.state.flush_to(&mut self.stream)
    }
}

struct ReaderState {
    recv_key: [u8; 32],
    recv_direction: u8,
    recv_sequence: u64,
    max_record: usize,
    wire_buffer: Vec<u8>,
    read_buffer: Vec<u8>,
    read_offset: usize,
    failed: bool,
}

impl ReaderState {
    fn from_session(session: &TcpSession, max_record: usize) -> Self {
        Self {
            recv_key: session.recv_key,
            recv_direction: session.recv_direction,
            recv_sequence: session.recv_sequence,
            max_record,
            wire_buffer: Vec::new(),
            read_buffer: Vec::new(),
            read_offset: 0,
            failed: false,
        }
    }

    fn has_buffered_bytes(&self) -> bool {
        !self.wire_buffer.is_empty() || self.read_offset != self.read_buffer.len()
    }

    fn read_from<S: Read>(&mut self, stream: &mut S, bytes: &mut [u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        if self.read_offset > self.read_buffer.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "authenticated TCP read cursor is invalid",
            ));
        }
        if self.read_offset == self.read_buffer.len() && !self.read_record(stream)? {
            return Ok(0);
        }
        let available = self
            .read_buffer
            .len()
            .checked_sub(self.read_offset)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "authenticated TCP read cursor")
            })?;
        let count = available.min(bytes.len());
        let end = self.read_offset.checked_add(count).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "authenticated TCP read cursor overflow",
            )
        })?;
        bytes[..count].copy_from_slice(&self.read_buffer[self.read_offset..end]);
        self.read_offset = end;
        Ok(count)
    }

    fn read_record<S: Read>(&mut self, stream: &mut S) -> io::Result<bool> {
        if self.failed {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "authenticated TCP stream is closed after a protocol failure",
            ));
        }
        let result = self.read_record_inner(stream);
        if let Err(error) = &result
            && matches!(
                error.kind(),
                io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof
            )
        {
            self.failed = true;
        }
        result
    }

    fn read_record_inner<S: Read>(&mut self, stream: &mut S) -> io::Result<bool> {
        let mut scratch = [0_u8; RECORD_SCRATCH_BYTES];
        let Some(header) = self.read_record_header(stream, &mut scratch)? else {
            return Ok(false);
        };
        self.validate_record_header(&header)?;
        let length =
            usize::try_from(u32::from_be_bytes(header[14..18].try_into().map_err(
                |_| io::Error::new(io::ErrorKind::InvalidData, "invalid length"),
            )?))
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid record length"))?;
        if length == 0 || length > self.max_record {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "authenticated TCP record exceeds its bound",
            ));
        }
        let payload_end = RECORD_HEADER_BYTES.checked_add(length).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "record payload length overflow")
        })?;
        let total = payload_end
            .checked_add(RECORD_MAC_BYTES)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "record length overflow"))?;
        self.read_record_body(stream, &mut scratch, total)?;
        let payload = &self.wire_buffer[RECORD_HEADER_BYTES..payload_end];
        let claimed: [u8; RECORD_MAC_BYTES] = self.wire_buffer[payload_end..total]
            .try_into()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid record MAC"))?;
        let expected = record_mac(&self.recv_key, &header, payload);
        if !constant_time_eq(&claimed, &expected) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "authenticated TCP record MAC mismatch",
            ));
        }
        self.recv_sequence = self.recv_sequence.checked_add(1).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "record sequence exhausted")
        })?;
        self.read_buffer.clear();
        self.read_buffer.extend_from_slice(payload);
        self.read_offset = 0;
        self.wire_buffer.drain(..total);
        Ok(true)
    }

    fn read_record_header<S: Read>(
        &mut self,
        stream: &mut S,
        scratch: &mut [u8; RECORD_SCRATCH_BYTES],
    ) -> io::Result<Option<[u8; RECORD_HEADER_BYTES]>> {
        if self.wire_buffer.is_empty() {
            match stream.read(&mut scratch[..1]) {
                Ok(0) => return Ok(None),
                Ok(1) => self.wire_buffer.push(scratch[0]),
                Ok(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "authenticated TCP record returned an invalid prefix",
                    ));
                }
                Err(error) => return Err(error),
            }
        }
        while self.wire_buffer.len() < RECORD_HEADER_BYTES {
            let remaining = RECORD_HEADER_BYTES
                .checked_sub(self.wire_buffer.len())
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "authenticated TCP header size")
                })?;
            let read = stream.read(&mut scratch[..remaining])?;
            if read == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "truncated authenticated TCP record header",
                ));
            }
            if read > remaining {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "authenticated TCP reader returned too many bytes",
                ));
            }
            self.wire_buffer.extend_from_slice(&scratch[..read]);
        }
        let header: [u8; RECORD_HEADER_BYTES] = self.wire_buffer[..RECORD_HEADER_BYTES]
            .try_into()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid record header"))?;
        Ok(Some(header))
    }

    fn validate_record_header(&self, header: &[u8; RECORD_HEADER_BYTES]) -> io::Result<()> {
        if header[..4] != RECORD_MAGIC || header[4] != RECORD_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid authenticated TCP record envelope",
            ));
        }
        if header[5] != self.recv_direction {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "authenticated TCP record direction mismatch",
            ));
        }
        let sequence = u64::from_be_bytes(
            header[6..14]
                .try_into()
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid sequence"))?,
        );
        if sequence != self.recv_sequence {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "authenticated TCP record replay or reorder",
            ));
        }
        Ok(())
    }

    fn read_record_body<S: Read>(
        &mut self,
        stream: &mut S,
        scratch: &mut [u8; RECORD_SCRATCH_BYTES],
        total: usize,
    ) -> io::Result<()> {
        while self.wire_buffer.len() < total {
            let remaining = total.checked_sub(self.wire_buffer.len()).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "authenticated TCP record size")
            })?;
            let chunk_size = remaining.min(RECORD_SCRATCH_BYTES);
            let read = stream.read(&mut scratch[..chunk_size])?;
            if read == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "truncated authenticated TCP record body",
                ));
            }
            if read > chunk_size {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "authenticated TCP reader returned too many bytes",
                ));
            }
            self.wire_buffer.extend_from_slice(&scratch[..read]);
        }
        Ok(())
    }
}

impl Drop for ReaderState {
    fn drop(&mut self) {
        self.recv_key = [0_u8; 32];
    }
}

struct WriterState {
    send_key: [u8; 32],
    send_direction: u8,
    send_sequence: u64,
    max_record: usize,
    write_buffer: Vec<u8>,
}

impl WriterState {
    fn from_session(session: &TcpSession, max_record: usize) -> Self {
        Self {
            send_key: session.send_key,
            send_direction: session.send_direction,
            send_sequence: session.send_sequence,
            max_record,
            write_buffer: Vec::new(),
        }
    }

    fn has_buffered_bytes(&self) -> bool {
        !self.write_buffer.is_empty()
    }

    fn write_to(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let new_len = self
            .write_buffer
            .len()
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "record length overflow"))?;
        if new_len > self.max_record {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "authenticated TCP record exceeds its bound",
            ));
        }
        self.write_buffer.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush_to<S: Write>(&mut self, stream: &mut S) -> io::Result<()> {
        if self.write_buffer.is_empty() {
            return stream.flush();
        }
        let length = u32::try_from(self.write_buffer.len()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "authenticated TCP record length overflows",
            )
        })?;
        let sequence = self.send_sequence;
        let mut header = [0_u8; RECORD_HEADER_BYTES];
        header[..4].copy_from_slice(&RECORD_MAGIC);
        header[4] = RECORD_VERSION;
        header[5] = self.send_direction;
        header[6..14].copy_from_slice(&sequence.to_be_bytes());
        header[14..18].copy_from_slice(&length.to_be_bytes());
        let mac = record_mac(&self.send_key, &header, &self.write_buffer);
        stream.write_all(&header)?;
        stream.write_all(&self.write_buffer)?;
        stream.write_all(&mac)?;
        stream.flush()?;
        self.send_sequence = self.send_sequence.checked_add(1).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "record sequence exhausted")
        })?;
        self.write_buffer.clear();
        Ok(())
    }
}

impl Drop for WriterState {
    fn drop(&mut self) {
        self.send_key = [0_u8; 32];
    }
}

fn record_mac(key: &[u8; 32], header: &[u8; RECORD_HEADER_BYTES], payload: &[u8]) -> [u8; 32] {
    let mut material = Vec::with_capacity(header.len() + payload.len() + 32);
    material.extend_from_slice(b"backend.tcp.record.v1\0");
    material.extend_from_slice(header);
    material.extend_from_slice(payload);
    *blake3::keyed_hash(key, &material).as_bytes()
}
