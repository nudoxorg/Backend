//! Canonical little-endian encoding helpers.
//!
//! Every hash preimage in the IR-VCS design is built from these primitives so
//! that the same logical value always yields the same bytes regardless of
//! platform endianness or `serde` representation. Strings are always
//! length-prefixed (`u32le` byte length, then UTF-8); there are **no** bare,
//! unframed strings or floats in any hash preimage (design Appendix A §3).

/// A byte consumer for a hash preimage.
///
/// [`Vec<u8>`] keeps the bytes. [`HasherSink`] folds them into BLAKE3 and
/// drops them, so a storage hash does not allocate the payload it digests.
pub trait ByteSink {
    /// Append one byte.
    fn push(&mut self, byte: u8);
    /// Append a slice.
    fn extend_from_slice(&mut self, bytes: &[u8]);
}

impl ByteSink for Vec<u8> {
    #[inline]
    fn push(&mut self, byte: u8) {
        Vec::push(self, byte);
    }

    #[inline]
    fn extend_from_slice(&mut self, bytes: &[u8]) {
        Vec::extend_from_slice(self, bytes);
    }
}

/// Folds preimage bytes into a BLAKE3 hasher. A small buffer keeps one-byte
/// `push` calls from calling into the hasher individually.
pub struct HasherSink<'a> {
    hasher: &'a mut blake3::Hasher,
    buf: [u8; 128],
    filled: usize,
}

impl<'a> HasherSink<'a> {
    /// Hash into `hasher`. Drop flushes the tail.
    #[must_use]
    pub fn new(hasher: &'a mut blake3::Hasher) -> Self {
        Self {
            hasher,
            buf: [0; 128],
            filled: 0,
        }
    }

    fn flush(&mut self) {
        if self.filled == 0 {
            return;
        }
        self.hasher.update(&self.buf[..self.filled]);
        self.filled = 0;
    }
}

impl Drop for HasherSink<'_> {
    fn drop(&mut self) {
        self.flush();
    }
}

impl ByteSink for HasherSink<'_> {
    #[inline]
    fn push(&mut self, byte: u8) {
        self.buf[self.filled] = byte;
        self.filled += 1;
        if self.filled == self.buf.len() {
            self.flush();
        }
    }

    fn extend_from_slice(&mut self, bytes: &[u8]) {
        let mut rest = bytes;
        while !rest.is_empty() {
            let room = self.buf.len() - self.filled;
            let take = room.min(rest.len());
            self.buf[self.filled..self.filled + take].copy_from_slice(&rest[..take]);
            self.filled += take;
            rest = &rest[take..];
            if self.filled == self.buf.len() {
                self.flush();
            }
        }
    }
}

/// Append a little-endian `u16`.
#[inline]
pub fn write_u16le(out: &mut impl ByteSink, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// Append a little-endian `u32`.
#[inline]
pub fn write_u32le(out: &mut impl ByteSink, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// Append a little-endian `u64`.
#[inline]
pub fn write_u64le(out: &mut impl ByteSink, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// `encode_str(s) = u32le(byte_len) || utf8_bytes` (design IntroId bootstrap).
///
/// Panics only in the impossible case that a string exceeds `u32::MAX` bytes.
#[inline]
pub fn encode_str(out: &mut impl ByteSink, s: &str) {
    let bytes = s.as_bytes();
    write_u32le(
        out,
        u32::try_from(bytes.len()).expect("string longer than u32::MAX bytes"),
    );
    out.extend_from_slice(bytes);
}

/// `encode_segments(ss) = u32le(count) || encode_str(each, root->leaf)`.
#[inline]
pub fn encode_segments<S: AsRef<str>>(out: &mut impl ByteSink, segments: &[S]) {
    write_u32le(
        out,
        u32::try_from(segments.len()).expect("too many segments"),
    );
    for seg in segments {
        encode_str(out, seg.as_ref());
    }
}
