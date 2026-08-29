pub(super) const ONE_SECTION_BODY: &[u8] = &[0xa5];
pub(super) const ONE_SECTION_CORPUS: [u8; 41] = [
    b'N', b'D', b'X', b'1', 1, 0, 40, 0, 41, 0, 0, 0, 1, 0, 0, 0, 1, 0, 1, 0, 0, 0, 0, 0, 3, 0, 0,
    0, 40, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 0xa5,
];
pub(super) const TWO_SECTION_CORPUS: [u8; 65] = [
    b'N', b'D', b'X', b'1', 1, 0, 56, 0, 65, 0, 0, 0, 2, 0, 0, 0, 1, 0, 1, 0, 0, 0, 0, 0, 1, 0, 0,
    0, 56, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 64, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 0x11,
    0, 0, 0, 0, 0, 0, 0, 0x22,
];

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("corpus frame did not contain {needed} bytes at {offset}")]
pub(super) enum CorpusError {
    MissingBytes { offset: usize, needed: usize },
}

pub(super) fn input_byte(input: &[u8], offset: usize) -> u8 {
    match input.get(offset) {
        Some(value) => *value,
        None => 0,
    }
}

pub(super) fn replace_byte(bytes: &mut [u8], offset: usize, raw: u8) -> Result<(), CorpusError> {
    let slot = bytes
        .get_mut(offset)
        .ok_or(CorpusError::MissingBytes { offset, needed: 1 })?;
    *slot = if raw == *slot {
        raw.wrapping_add(1)
    } else {
        raw
    };
    Ok(())
}

pub(super) fn read_byte(bytes: &[u8], offset: usize) -> Result<u8, CorpusError> {
    bytes
        .get(offset)
        .copied()
        .ok_or(CorpusError::MissingBytes { offset, needed: 1 })
}

pub(super) fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, CorpusError> {
    let Some([first, second]) = bytes.get(offset..).and_then(|tail| tail.get(..2)) else {
        return Err(CorpusError::MissingBytes { offset, needed: 2 });
    };
    Ok(u16::from_le_bytes([*first, *second]))
}

pub(super) fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, CorpusError> {
    let Some([first, second, third, fourth]) = bytes.get(offset..).and_then(|tail| tail.get(..4))
    else {
        return Err(CorpusError::MissingBytes { offset, needed: 4 });
    };
    Ok(u32::from_le_bytes([*first, *second, *third, *fourth]))
}

pub(super) fn write_u16(bytes: &mut [u8], offset: usize, value: u16) -> Result<(), CorpusError> {
    let Some(target) = bytes.get_mut(offset..).and_then(|tail| tail.get_mut(..2)) else {
        return Err(CorpusError::MissingBytes { offset, needed: 2 });
    };
    target.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

pub(super) fn write_u32(bytes: &mut [u8], offset: usize, value: u32) -> Result<(), CorpusError> {
    let Some(target) = bytes.get_mut(offset..).and_then(|tail| tail.get_mut(..4)) else {
        return Err(CorpusError::MissingBytes { offset, needed: 4 });
    };
    target.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

#[allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    reason = "the test corpus targets the project's supported 32/64-bit platforms, where every u32 fits usize"
)]
pub(super) const fn raw_usize(value: u32) -> usize {
    value as usize
}
