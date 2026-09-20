//! Streaming scan, checkpoint validation, and tail repair.

use super::frame::{read_frame_at_path, validate_header, validate_payload};
use super::{
    ChainHash, File, HEADER_BYTES, JournalCheckpoint, JournalCodec, JournalDomain, JournalError,
    JournalFrame, JournalFrameRef, JournalLimits, JournalRecovery, JournalScan, MAX_PAYLOAD,
    OpenOptions, Path, Read, RecordId, Seek, SeekFrom, containing_directory,
};

pub(super) fn collect_recovery<D: JournalCodec>(
    path: &Path,
    limits: JournalLimits,
    checkpoint: Option<JournalCheckpoint<D>>,
) -> Result<(JournalRecovery<D>, JournalScan<D>), JournalError> {
    let mut frames = Vec::new();
    let scan = scan_path(path, limits, checkpoint, |frame| {
        frames.push(JournalFrame {
            sequence: frame.sequence,
            previous: frame.previous,
            record: frame.record,
            payload: frame.payload.to_vec().into_boxed_slice(),
            chain: frame.chain,
            end_offset: frame.end_offset,
        });
        Ok(())
    })?;
    let recovery = JournalRecovery {
        last_sequence: scan.last_sequence,
        frames,
        chain: scan.chain,
        truncated_tail: scan.truncated_tail,
    };
    Ok((recovery, scan))
}

pub(super) fn repair_tail<D: JournalDomain>(
    path: &Path,
    scan: &JournalScan<D>,
) -> Result<(), JournalError> {
    if !scan.truncated_tail {
        return Ok(());
    }
    let repair = OpenOptions::new().write(true).open(path)?;
    repair.set_len(scan.valid_offset)?;
    repair.sync_all()?;
    if repair.metadata()?.len() != scan.valid_offset {
        return Err(JournalError::Corrupt("tail repair"));
    }
    File::open(containing_directory(path))?.sync_all()?;
    Ok(())
}

pub(super) fn scan_path<D: JournalCodec, F>(
    path: &Path,
    limits: JournalLimits,
    checkpoint: Option<JournalCheckpoint<D>>,
    mut visitor: F,
) -> Result<JournalScan<D>, JournalError>
where
    F: FnMut(JournalFrameRef<'_, D>) -> Result<(), JournalError>,
{
    let mut file = File::open(path)?;
    let (start_offset, mut expected_sequence, mut previous, initial_sequence) =
        scan_start(path, checkpoint)?;
    file.seek(SeekFrom::Start(start_offset))?;
    let mut offset = start_offset;
    let mut frames_scanned = 0usize;
    let mut bytes_scanned = 0u64;
    let mut peak_payload_bytes = 0usize;
    let mut payload = Vec::new();
    let mut truncated_tail = false;
    let max_bytes = u64::try_from(limits.max_bytes).map_err(|_| JournalError::Bounds)?;

    loop {
        if offset == file.metadata()?.len() {
            break;
        }
        if frames_scanned >= limits.max_frames || bytes_scanned == max_bytes {
            return Err(JournalError::Bounds);
        }
        let header = match read_scan_header(
            &mut file,
            offset,
            bytes_scanned,
            limits.max_bytes,
            max_bytes,
        )? {
            HeaderRead::End => break,
            HeaderRead::Truncated {
                bytes_scanned: bytes,
            } => {
                bytes_scanned = bytes;
                truncated_tail = true;
                break;
            }
            HeaderRead::Ready {
                header,
                bytes_scanned: bytes,
            } => {
                bytes_scanned = bytes;
                header
            }
        };
        let Some(progress) = scan_frame(
            &mut file,
            &mut payload,
            &ScanFrameInput {
                offset,
                expected_sequence,
                previous,
                bytes_scanned,
                max_bytes,
                header,
            },
            &mut visitor,
        )?
        else {
            truncated_tail = true;
            break;
        };
        bytes_scanned = progress.bytes_scanned;
        frames_scanned = frames_scanned.checked_add(1).ok_or(JournalError::Bounds)?;
        peak_payload_bytes = peak_payload_bytes.max(progress.length);
        previous = progress.digest;
        expected_sequence = progress
            .sequence
            .checked_add(1)
            .ok_or(JournalError::Bounds)?;
        offset = progress.end_offset;
    }
    Ok(JournalScan {
        frames_scanned,
        bytes_scanned,
        peak_payload_bytes,
        valid_offset: offset,
        last_sequence: if frames_scanned > 0 {
            Some(expected_sequence.saturating_sub(1))
        } else {
            initial_sequence
        },
        chain: previous,
        truncated_tail,
    })
}

fn scan_start<D: JournalCodec>(
    path: &Path,
    checkpoint: Option<JournalCheckpoint<D>>,
) -> Result<(u64, u64, ChainHash<D>, Option<u64>), JournalError> {
    let Some(checkpoint) = checkpoint else {
        return Ok((0, 0, ChainHash::genesis(), None));
    };
    let frame = read_frame_at_path::<D>(path, checkpoint.offset)?;
    if frame.start_offset() != checkpoint.offset
        || frame.sequence != checkpoint.sequence
        || frame.chain != checkpoint.chain
        || frame.record != checkpoint.record
    {
        return Err(JournalError::Corrupt("checkpoint receipt"));
    }
    if checkpoint.sequence == 0 && frame.previous != ChainHash::genesis() {
        return Err(JournalError::Corrupt("checkpoint predecessor"));
    }
    Ok((
        frame.end_offset,
        checkpoint
            .sequence
            .checked_add(1)
            .ok_or(JournalError::Bounds)?,
        checkpoint.chain,
        Some(checkpoint.sequence),
    ))
}

enum HeaderRead {
    End,
    Truncated {
        bytes_scanned: u64,
    },
    Ready {
        header: [u8; HEADER_BYTES],
        bytes_scanned: u64,
    },
}

fn read_scan_header(
    file: &mut File,
    offset: u64,
    bytes_scanned: u64,
    max_bytes: usize,
    max_bytes_u64: u64,
) -> Result<HeaderRead, JournalError> {
    let mut header = [0u8; HEADER_BYTES];
    let bytes_before = bytes_scanned;
    let (read, limited) = read_limited(file, &mut header, bytes_scanned, max_bytes)?;
    if read == 0 {
        return Ok(HeaderRead::End);
    }
    let bytes_scanned = add_read_bytes(bytes_scanned, read, max_bytes)?;
    if limited {
        let budget_end = offset
            .checked_add(
                max_bytes_u64
                    .checked_sub(bytes_before)
                    .ok_or(JournalError::Bounds)?,
            )
            .ok_or(JournalError::Bounds)?;
        if file.metadata()?.len() > budget_end {
            return Err(JournalError::Bounds);
        }
        return Ok(HeaderRead::Truncated { bytes_scanned });
    }
    if read < HEADER_BYTES {
        return Ok(HeaderRead::Truncated { bytes_scanned });
    }
    Ok(HeaderRead::Ready {
        header,
        bytes_scanned,
    })
}

struct FrameProgress<D: JournalDomain> {
    sequence: u64,
    digest: ChainHash<D>,
    end_offset: u64,
    length: usize,
    bytes_scanned: u64,
}

struct ScanFrameInput<D: JournalDomain> {
    offset: u64,
    expected_sequence: u64,
    previous: ChainHash<D>,
    bytes_scanned: u64,
    max_bytes: u64,
    header: [u8; HEADER_BYTES],
}

fn scan_frame<D: JournalCodec, F>(
    file: &mut File,
    payload: &mut Vec<u8>,
    input: &ScanFrameInput<D>,
    visitor: &mut F,
) -> Result<Option<FrameProgress<D>>, JournalError>
where
    F: FnMut(JournalFrameRef<'_, D>) -> Result<(), JournalError>,
{
    let offset = input.offset;
    let expected_sequence = input.expected_sequence;
    let previous = input.previous;
    let bytes_scanned = input.bytes_scanned;
    let max_bytes = input.max_bytes;
    let header = input.header;
    validate_header::<D>(&header)?;
    let sequence = u64::from_be_bytes(
        header[14..22]
            .try_into()
            .map_err(|_| JournalError::Corrupt("sequence"))?,
    );
    if sequence != expected_sequence {
        return Err(JournalError::Corrupt("sequence"));
    }
    let mut previous_bytes = [0; 32];
    previous_bytes.copy_from_slice(&header[22..54]);
    if previous_bytes != *previous.as_bytes() {
        return Err(JournalError::Corrupt("predecessor"));
    }
    let declared_length = u64::from_be_bytes(
        header[54..62]
            .try_into()
            .map_err(|_| JournalError::Corrupt("length"))?,
    );
    let remaining = max_bytes
        .checked_sub(bytes_scanned)
        .ok_or(JournalError::Bounds)?;
    let payload_start = offset
        .checked_add(u64::try_from(HEADER_BYTES).map_err(|_| JournalError::Bounds)?)
        .ok_or(JournalError::Bounds)?;
    let file_len = file.metadata()?.len();
    if declared_length > u64::try_from(MAX_PAYLOAD).map_err(|_| JournalError::Bounds)? {
        if payload_start > file_len || declared_length > file_len - payload_start {
            return Ok(None);
        }
        return Err(JournalError::Bounds);
    }
    let declared_end = payload_start
        .checked_add(declared_length)
        .ok_or(JournalError::Bounds)?;
    if file_len < declared_end {
        return Ok(None);
    }
    let length = usize::try_from(declared_length).map_err(|_| JournalError::Bounds)?;
    if u64::try_from(length).map_err(|_| JournalError::Bounds)? > remaining {
        return Err(JournalError::Bounds);
    }
    payload.clear();
    payload.resize(length, 0);
    let (read, limited) = read_limited(
        file,
        payload,
        bytes_scanned,
        usize::try_from(max_bytes).map_err(|_| JournalError::Bounds)?,
    )?;
    let bytes_scanned = add_read_bytes(
        bytes_scanned,
        read,
        usize::try_from(max_bytes).map_err(|_| JournalError::Bounds)?,
    )?;
    if limited {
        return if file.metadata()?.len() < declared_end {
            Ok(None)
        } else {
            Err(JournalError::Bounds)
        };
    }
    if read < length {
        return Ok(None);
    }
    let mut digest = [0; 32];
    digest.copy_from_slice(&header[62..94]);
    validate_payload::<D>(sequence, &previous_bytes, &digest, payload)?;
    let end_offset = offset
        .checked_add(u64::try_from(HEADER_BYTES).map_err(|_| JournalError::Bounds)?)
        .and_then(|end| end.checked_add(u64::try_from(length).ok()?))
        .ok_or(JournalError::Bounds)?;
    visitor(JournalFrameRef {
        sequence,
        previous: ChainHash::from_bytes(previous_bytes),
        record: RecordId::from_payload(payload),
        payload,
        chain: ChainHash::from_bytes(digest),
        offset,
        end_offset,
    })?;
    Ok(Some(FrameProgress {
        sequence,
        digest: ChainHash::from_bytes(digest),
        end_offset,
        length,
        bytes_scanned,
    }))
}

fn read_limited(
    file: &mut File,
    buffer: &mut [u8],
    consumed: u64,
    max_bytes: usize,
) -> Result<(usize, bool), JournalError> {
    let consumed = usize::try_from(consumed).map_err(|_| JournalError::Bounds)?;
    let remaining = max_bytes
        .checked_sub(consumed)
        .ok_or(JournalError::Bounds)?;
    let read_len = buffer.len().min(remaining);
    let read = file.read(&mut buffer[..read_len])?;
    Ok((read, read_len < buffer.len()))
}

fn add_read_bytes(current: u64, read: usize, max_bytes: usize) -> Result<u64, JournalError> {
    let next = current
        .checked_add(u64::try_from(read).map_err(|_| JournalError::Bounds)?)
        .ok_or(JournalError::Bounds)?;
    if next > u64::try_from(max_bytes).map_err(|_| JournalError::Bounds)? {
        return Err(JournalError::Bounds);
    }
    Ok(next)
}
