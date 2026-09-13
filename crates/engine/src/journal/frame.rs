//! Canonical frame encoding and validation.

use super::{
    ChainHash, File, HEADER_BYTES, JOURNAL_FORMAT, JOURNAL_MAGIC, JournalCodec, JournalDomain,
    JournalError, JournalFrame, MAX_PAYLOAD, Path, Read, RecordId, Seek, SeekFrom, chain_digest,
    io,
};

pub(super) fn encode_frame<D: JournalDomain>(
    sequence: u64,
    previous: &[u8; 32],
    digest: &[u8; 32],
    payload: &[u8],
) -> Result<Vec<u8>, JournalError> {
    let payload_len = u64::try_from(payload.len()).map_err(|_| JournalError::Bounds)?;
    let mut frame = Vec::with_capacity(
        HEADER_BYTES
            .checked_add(payload.len())
            .ok_or(JournalError::Bounds)?,
    );
    frame.extend_from_slice(&JOURNAL_MAGIC);
    frame.extend_from_slice(&JOURNAL_FORMAT.to_be_bytes());
    frame.push(D::DOMAIN);
    frame.extend_from_slice(&D::TYPE.to_be_bytes());
    frame.push(D::VERSION);
    frame.extend_from_slice(&sequence.to_be_bytes());
    frame.extend_from_slice(previous);
    frame.extend_from_slice(&payload_len.to_be_bytes());
    frame.extend_from_slice(digest);
    frame.extend_from_slice(payload);
    Ok(frame)
}

pub(crate) fn read_frame_at_path<D: JournalCodec>(
    path: &Path,
    offset: u64,
) -> Result<JournalFrame<D>, JournalError> {
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut header = [0u8; HEADER_BYTES];
    read_exact_frame(&mut file, &mut header, "frame header")?;
    validate_header::<D>(&header)?;
    let sequence = u64::from_be_bytes(
        header[14..22]
            .try_into()
            .map_err(|_| JournalError::Corrupt("sequence"))?,
    );
    let mut previous_bytes = [0; 32];
    previous_bytes.copy_from_slice(&header[22..54]);
    let declared_length = u64::from_be_bytes(
        header[54..62]
            .try_into()
            .map_err(|_| JournalError::Corrupt("length"))?,
    );
    let payload_start = offset
        .checked_add(u64::try_from(HEADER_BYTES).map_err(|_| JournalError::Bounds)?)
        .ok_or(JournalError::Bounds)?;
    let file_len = file.metadata()?.len();
    if payload_start > file_len || declared_length > file_len - payload_start {
        return Err(JournalError::Corrupt("frame payload"));
    }
    let end_offset = payload_start
        .checked_add(declared_length)
        .ok_or(JournalError::Bounds)?;
    let length = usize::try_from(declared_length).map_err(|_| JournalError::Bounds)?;
    if length > MAX_PAYLOAD {
        return Err(JournalError::Bounds);
    }
    let mut payload = vec![0u8; length];
    read_exact_frame(&mut file, &mut payload, "frame payload")?;
    let mut digest = [0; 32];
    digest.copy_from_slice(&header[62..94]);
    validate_payload::<D>(sequence, &previous_bytes, &digest, &payload)?;
    Ok(JournalFrame {
        sequence,
        previous: ChainHash::from_bytes(previous_bytes),
        record: RecordId::from_payload(&payload),
        payload: payload.into_boxed_slice(),
        chain: ChainHash::from_bytes(digest),
        end_offset,
    })
}

/// Ensures a journal file's cursor is at its end after external inspection.
/// # Errors
///
/// Returns an error when validation, persistence, or admission of the
/// supplied value fails.
pub fn sync_to_end(file: &mut File) -> io::Result<()> {
    file.seek(SeekFrom::End(0)).map(|_| ())
}

pub(super) fn validate_header<D: JournalDomain>(header: &[u8]) -> Result<(), JournalError> {
    if header.len() != HEADER_BYTES {
        return Err(JournalError::Corrupt("header"));
    }
    if header[..8] != JOURNAL_MAGIC {
        return Err(JournalError::Corrupt("magic"));
    }
    let format = u16::from_be_bytes(
        header[8..10]
            .try_into()
            .map_err(|_| JournalError::Corrupt("format"))?,
    );
    let domain = header[10];
    let ty = u16::from_be_bytes(
        header[11..13]
            .try_into()
            .map_err(|_| JournalError::Corrupt("type"))?,
    );
    let version = header[13];
    if format != JOURNAL_FORMAT || domain != D::DOMAIN || ty != D::TYPE || version != D::VERSION {
        return Err(
            if domain != D::DOMAIN || ty != D::TYPE || version != D::VERSION {
                JournalError::DomainMismatch
            } else {
                JournalError::Corrupt("format/version")
            },
        );
    }
    Ok(())
}

pub(super) fn validate_payload<D: JournalCodec>(
    sequence: u64,
    previous: &[u8; 32],
    digest: &[u8; 32],
    payload: &[u8],
) -> Result<(), JournalError> {
    if chain_digest::<D>(sequence, previous, payload) != *digest {
        return Err(JournalError::Corrupt("hash"));
    }
    D::validate(payload)
}

pub(super) fn read_exact_frame<R: Read>(
    reader: &mut R,
    buffer: &mut [u8],
    reason: &'static str,
) -> Result<(), JournalError> {
    match reader.read_exact(buffer) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
            Err(JournalError::Corrupt(reason))
        }
        Err(error) => Err(JournalError::Io(error)),
    }
}
