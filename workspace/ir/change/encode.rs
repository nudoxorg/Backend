//! Canonical little-endian encoding helpers.
//!
//! Every hash preimage in the IR-VCS design is built from these primitives so
//! that the same logical value always yields the same bytes regardless of
//! platform endianness or `serde` representation. Strings are always
//! length-prefixed (`u32le` byte length, then UTF-8); there are **no** bare,
//! unframed strings or floats in any hash preimage (design Appendix A §3).

/// Append a little-endian `u16`.
#[inline]
pub fn write_u16le(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// Append a little-endian `u32`.
#[inline]
pub fn write_u32le(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// Append a little-endian `u64`.
#[inline]
pub fn write_u64le(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// `encode_str(s) = u32le(byte_len) || utf8_bytes` (design IntroId bootstrap).
///
/// Panics only in the impossible case that a string exceeds `u32::MAX` bytes.
#[inline]
pub fn encode_str(out: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    write_u32le(out, u32::try_from(bytes.len()).expect("string longer than u32::MAX bytes"));
    out.extend_from_slice(bytes);
}

/// `encode_segments(ss) = u32le(count) || encode_str(each, root->leaf)`.
#[inline]
pub fn encode_segments<S: AsRef<str>>(out: &mut Vec<u8>, segments: &[S]) {
    write_u32le(out, u32::try_from(segments.len()).expect("too many segments"));
    for seg in segments {
        encode_str(out, seg.as_ref());
    }
}
