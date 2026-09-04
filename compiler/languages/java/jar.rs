//! A bounded, borrowing reader for the ZIP subset used by Maven JARs.
//!
//! ZIP64, encrypted entries, and data descriptors are intentionally outside this
//! adapter's contract. Extraction is caller-owned: deflate output is written into
//! the supplied `Vec`, and no checksum is obtained from Maven here.

use flate2::{Decompress, FlushDecompress, Status};
use thiserror::Error;

const EOCD: u32 = 0x0605_4b50;
const CENTRAL: u32 = 0x0201_4b50;
const LOCAL: u32 = 0x0403_4b50;
const MAX_COMMENT: usize = 65_535;
const MAX_ENTRY: u64 = 512 * 1024 * 1024;

/// A JAR borrowing its complete source bytes.
#[derive(Debug)]
pub struct Jar<'bytes> {
    bytes: &'bytes [u8],
    directory: usize,
    count: usize,
}

/// A central-directory entry borrowing its name and owning no file bytes.
#[derive(Clone, Copy, Debug)]
pub struct JarEntry<'jar> {
    jar: &'jar Jar<'jar>,
    offset: usize,
    name: &'jar [u8],
    method: u16,
    crc: u32,
    packed: u64,
    unpacked: u64,
    local: u64,
}

/// Bytes returned by extraction: either archive-borrowed or caller-buffered.
#[derive(Debug, PartialEq, Eq)]
pub enum EntryData<'bytes, 'output> {
    Borrowed(&'bytes [u8]),
    Buffered(&'output [u8]),
}
impl<'bytes, 'output> AsRef<[u8]> for EntryData<'bytes, 'output> {
    fn as_ref(&self) -> &[u8] {
        match self {
            Self::Borrowed(bytes) => bytes,
            Self::Buffered(bytes) => bytes,
        }
    }
}
impl<'bytes, 'output> std::ops::Deref for EntryData<'bytes, 'output> {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        self.as_ref()
    }
}

/// A malformed archive or rejected extraction request.
#[derive(Debug, Error)]
pub enum JarError {
    #[error("EOCD missing near offset {offset}, raw value {raw:#x}")]
    EocdMissing { offset: usize, raw: u32 },
    #[error("central directory truncated at offset {offset}, raw value {raw:#x}")]
    CentralDirectoryTruncated { offset: usize, raw: u32 },
    #[error("unknown compression method {method} at offset {offset}")]
    UnknownCompression { offset: usize, method: u16 },
    #[error("local header mismatch at offset {offset}: expected {expected}, actual {actual}")]
    LocalHeaderMismatch {
        offset: usize,
        expected: u64,
        actual: u64,
    },
    #[error("size mismatch at offset {offset}: expected {expected}, actual {actual}")]
    SizeMismatch {
        offset: usize,
        expected: u64,
        actual: u64,
    },
    #[error("size overflow at offset {offset}, raw value {raw}")]
    SizeOverflow { offset: usize, raw: u64 },
    #[error("CRC mismatch at offset {offset}: expected {expected:#x}, actual {actual:#x}")]
    CrcMismatch {
        offset: usize,
        expected: u32,
        actual: u32,
    },
    #[error("entry at offset {offset} is too large: {size}")]
    EntryTooLarge { offset: usize, size: u64 },
    #[error("deflate failed at offset {offset}: {source}")]
    Deflate {
        offset: usize,
        source: flate2::DecompressError,
    },
    #[error(
        "output buffer is not large enough at offset {offset}: expected {expected}, actual {actual}"
    )]
    OutputTooSmall {
        offset: usize,
        expected: u64,
        actual: usize,
    },
}

impl<'bytes> Jar<'bytes> {
    /// Parses the EOCD and validates every central-directory record boundary.
    pub fn parse(bytes: &'bytes [u8]) -> Result<Self, JarError> {
        let start = bytes.len().saturating_sub(MAX_COMMENT + 22);
        let mut eocd = None;
        for offset in (start..=bytes.len().saturating_sub(4)).rev() {
            if read_u32(bytes, offset) == Some(EOCD) {
                eocd = Some(offset);
                break;
            }
        }
        let eocd = eocd.ok_or_else(|| JarError::EocdMissing {
            offset: start,
            raw: bytes
                .get(start..start.saturating_add(4))
                .map(read_raw)
                .unwrap_or(0),
        })?;
        let disk = read_u16(bytes, eocd + 4).ok_or_else(|| trunc(eocd + 4))?;
        let directory_disk = read_u16(bytes, eocd + 6).ok_or_else(|| trunc(eocd + 6))?;
        let count = read_u16(bytes, eocd + 10).ok_or_else(|| trunc(eocd + 10))? as usize;
        let size = read_u32(bytes, eocd + 12).ok_or_else(|| trunc(eocd + 12))? as usize;
        let directory = read_u32(bytes, eocd + 16).ok_or_else(|| trunc(eocd + 16))? as usize;
        let comment = read_u16(bytes, eocd + 20).ok_or_else(|| trunc(eocd + 20))? as usize;
        if disk != 0
            || directory_disk != 0
            || eocd.checked_add(22 + comment).is_none()
            || eocd + 22 + comment > bytes.len()
            || directory.checked_add(size).is_none()
            || directory + size > bytes.len()
        {
            return Err(trunc(eocd));
        }
        let jar = Self {
            bytes,
            directory,
            count,
        };
        let mut cursor = directory;
        for _ in 0..count {
            let entry = jar.entry_at(cursor)?;
            cursor = entry.offset + central_len(entry.name.len(), bytes, entry.offset)?;
        }
        if cursor != directory + size {
            return Err(JarError::SizeMismatch {
                offset: eocd + 12,
                expected: size as u64,
                actual: (cursor - directory) as u64,
            });
        }
        Ok(jar)
    }

    /// Walks central-directory entries without copying names.
    pub fn entries(&self) -> impl Iterator<Item = Result<JarEntry<'_>, JarError>> {
        EntryIter {
            jar: self,
            cursor: self.directory,
            left: self.count,
        }
    }

    fn entry_at(&self, offset: usize) -> Result<JarEntry<'_>, JarError> {
        if read_u32(self.bytes, offset) != Some(CENTRAL) {
            return Err(trunc(offset));
        }
        let name_len =
            read_u16(self.bytes, offset + 28).ok_or_else(|| trunc(offset + 28))? as usize;
        let extra_len =
            read_u16(self.bytes, offset + 30).ok_or_else(|| trunc(offset + 30))? as usize;
        let comment_len =
            read_u16(self.bytes, offset + 32).ok_or_else(|| trunc(offset + 32))? as usize;
        let end = offset
            .checked_add(46)
            .and_then(|n| n.checked_add(name_len + extra_len + comment_len))
            .ok_or(JarError::SizeOverflow {
                offset,
                raw: u64::MAX,
            })?;
        if end > self.bytes.len() {
            return Err(trunc(offset));
        }
        let method = read_u16(self.bytes, offset + 10).ok_or_else(|| trunc(offset + 10))?;
        if method != 0 && method != 8 {
            return Err(JarError::UnknownCompression { offset, method });
        }
        let packed = read_u32(self.bytes, offset + 20).ok_or_else(|| trunc(offset + 20))? as u64;
        let unpacked = read_u32(self.bytes, offset + 24).ok_or_else(|| trunc(offset + 24))? as u64;
        if unpacked > MAX_ENTRY {
            return Err(JarError::EntryTooLarge {
                offset,
                size: unpacked,
            });
        }
        Ok(JarEntry {
            jar: self,
            offset,
            name: &self.bytes[offset + 46..offset + 46 + name_len],
            method,
            crc: read_u32(self.bytes, offset + 16).ok_or_else(|| trunc(offset + 16))?,
            packed,
            unpacked,
            local: read_u32(self.bytes, offset + 42).ok_or_else(|| trunc(offset + 42))? as u64,
        })
    }
}

struct EntryIter<'jar> {
    jar: &'jar Jar<'jar>,
    cursor: usize,
    left: usize,
}
impl<'jar> Iterator for EntryIter<'jar> {
    type Item = Result<JarEntry<'jar>, JarError>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.left == 0 {
            return None;
        }
        let result = self.jar.entry_at(self.cursor);
        self.left -= 1;
        if let Ok(entry) = result {
            self.cursor = entry.offset
                + 46
                + entry.name.len()
                + read_u16(self.jar.bytes, entry.offset + 30).unwrap_or(0) as usize
                + read_u16(self.jar.bytes, entry.offset + 32).unwrap_or(0) as usize;
        }
        Some(result)
    }
}

impl<'jar> JarEntry<'jar> {
    /// The raw, borrowed entry name.
    pub fn name(&self) -> &'jar [u8] {
        self.name
    }
    /// Whether this name cannot escape a caller-selected extraction root.
    pub fn is_safe_relative_path(&self) -> bool {
        if self.name.is_empty()
            || self.name.contains(&b'\\')
            || self.name[0] == b'/'
            || self.name[0] == b'\\'
        {
            return false;
        }
        if self.name.len() >= 2 && self.name[1] == b':' {
            return false;
        }
        !self
            .name
            .split(|&b| b == b'/')
            .any(|part| part == b".." || part.is_empty())
    }
    /// Returns stored bytes directly, or decodes deflate into `output`.
    pub fn data<'out>(&self, output: &'out mut Vec<u8>) -> Result<EntryData<'jar, 'out>, JarError> {
        let local = usize::try_from(self.local).map_err(|_| JarError::SizeOverflow {
            offset: self.offset,
            raw: self.local,
        })?;
        if read_u32(self.jar.bytes, local) != Some(LOCAL) {
            return Err(JarError::LocalHeaderMismatch {
                offset: local,
                expected: LOCAL as u64,
                actual: read_u32(self.jar.bytes, local).unwrap_or(0) as u64,
            });
        }
        let local_name =
            read_u16(self.jar.bytes, local + 26).ok_or_else(|| trunc(local + 26))? as usize;
        let extra = read_u16(self.jar.bytes, local + 28).ok_or_else(|| trunc(local + 28))? as usize;
        let name_end = local
            .checked_add(30 + local_name)
            .ok_or(JarError::SizeOverflow {
                offset: local,
                raw: self.packed,
            })?;
        if name_end > self.jar.bytes.len() || &self.jar.bytes[local + 30..name_end] != self.name {
            return Err(JarError::LocalHeaderMismatch {
                offset: local,
                expected: self.name.len() as u64,
                actual: local_name as u64,
            });
        }
        let begin = name_end.checked_add(extra).ok_or(JarError::SizeOverflow {
            offset: local,
            raw: self.packed,
        })?;
        let end = begin
            .checked_add(
                usize::try_from(self.packed).map_err(|_| JarError::SizeOverflow {
                    offset: local,
                    raw: self.packed,
                })?,
            )
            .ok_or(JarError::SizeOverflow {
                offset: local,
                raw: self.packed,
            })?;
        if end > self.jar.bytes.len() {
            return Err(trunc(begin));
        }
        if self.method == 0 {
            if self.packed != self.unpacked {
                return Err(JarError::SizeMismatch {
                    offset: self.offset,
                    expected: self.unpacked,
                    actual: self.packed,
                });
            }
            let data = &self.jar.bytes[begin..end];
            return crc_result(self.offset, self.crc, data).map(|_| EntryData::Borrowed(data));
        }
        if self.unpacked > MAX_ENTRY {
            return Err(JarError::EntryTooLarge {
                offset: self.offset,
                size: self.unpacked,
            });
        }
        let target = usize::try_from(self.unpacked).map_err(|_| JarError::SizeOverflow {
            offset: self.offset,
            raw: self.unpacked,
        })?;
        output.clear();
        output.resize(target, 0);
        let mut decoder = Decompress::new(false);
        let status = decoder
            .decompress(&self.jar.bytes[begin..end], output, FlushDecompress::Finish)
            .map_err(|source| JarError::Deflate {
                offset: self.offset,
                source,
            })?;
        let written = decoder.total_out() as usize;
        if status != Status::StreamEnd || written != target {
            return Err(JarError::SizeMismatch {
                offset: self.offset,
                expected: self.unpacked,
                actual: written as u64,
            });
        }
        crc_result(self.offset, self.crc, &output[..written])
            .map(|_| EntryData::Buffered(&output[..written]))
    }
}

fn crc_result(offset: usize, expected: u32, bytes: &[u8]) -> Result<(), JarError> {
    let actual = crc32(bytes);
    if actual == expected {
        Ok(())
    } else {
        Err(JarError::CrcMismatch {
            offset,
            expected,
            actual,
        })
    }
}
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in bytes {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}
fn read_u16(bytes: &[u8], at: usize) -> Option<u16> {
    bytes
        .get(at..at + 2)
        .map(|v| u16::from_le_bytes([v[0], v[1]]))
}
fn read_u32(bytes: &[u8], at: usize) -> Option<u32> {
    bytes
        .get(at..at + 4)
        .map(|v| u32::from_le_bytes([v[0], v[1], v[2], v[3]]))
}
fn read_raw(bytes: &[u8]) -> u32 {
    let mut raw = [0; 4];
    raw[..bytes.len().min(4)].copy_from_slice(&bytes[..bytes.len().min(4)]);
    u32::from_le_bytes(raw)
}
fn trunc(offset: usize) -> JarError {
    JarError::CentralDirectoryTruncated { offset, raw: 0 }
}
fn central_len(name: usize, bytes: &[u8], offset: usize) -> Result<usize, JarError> {
    let extra = read_u16(bytes, offset + 30).ok_or_else(|| trunc(offset))? as usize;
    let comment = read_u16(bytes, offset + 32).ok_or_else(|| trunc(offset))? as usize;
    46usize
        .checked_add(name)
        .and_then(|n| n.checked_add(extra + comment))
        .ok_or(JarError::SizeOverflow {
            offset,
            raw: u64::MAX,
        })
}
