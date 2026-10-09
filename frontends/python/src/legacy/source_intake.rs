//! Finite, lossless byte-source intake. Unsupported codecs never become replacement text.

use crate::legacy::Span;

/// A source-specific decoding refusal in original raw-byte coordinates.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PythonSourceDecodeFault {
    /// Raw source cannot be represented by the admitted byte-coordinate format.
    #[error("Python source has {actual} bytes outside the 32-bit coordinate contract")]
    SourceExtent {
        /// Exact captured raw byte count.
        actual: usize,
    },
    /// The declared codec is outside this producer's UTF-8/ASCII intake contract.
    #[error("unsupported declared Python source codec {codec:?} at {span:?}")]
    UnsupportedCodec {
        /// Original declared codec spelling.
        codec: Box<str>,
        /// Original raw-byte range of the codec spelling.
        span: Span,
    },
    /// A UTF-8 BOM conflicts with the declared codec.
    #[error("UTF-8 BOM conflicts with Python source codec {codec:?} at {span:?}")]
    ConflictingBom {
        /// Original declared codec spelling.
        codec: Box<str>,
        /// Original raw-byte range of the conflicting codec spelling.
        span: Span,
    },
    /// The admitted UTF-8 codec rejected these exact raw bytes.
    #[error("invalid UTF-8 Python source bytes at {span:?}")]
    InvalidUtf8 {
        /// Exact raw-byte range rejected by UTF-8 validation.
        span: Span,
    },
    /// ASCII was explicitly selected but a non-ASCII byte was present.
    #[error("non-ASCII Python source byte at {span:?}")]
    InvalidAscii {
        /// The first non-ASCII raw byte.
        span: Span,
    },
}

/// Borrows exact UTF-8 source without copying or transcoding it.
///
/// UTF-8 (including its BOM) and ASCII are supported. Other declared codecs
/// remain explicit per-file unavailable facts; this is not general PEP 263 decoding.
///
/// # Errors
/// Returns a typed fault with original raw-byte coordinates for an unsupported
/// codec, conflicting BOM, invalid bytes or an unrepresentable source extent.
pub fn decode_python_source(bytes: &[u8]) -> Result<&str, PythonSourceDecodeFault> {
    if u32::try_from(bytes.len()).is_err() {
        return Err(PythonSourceDecodeFault::SourceExtent {
            actual: bytes.len(),
        });
    }
    let bom = bytes.starts_with(b"\xef\xbb\xbf");
    if let Some((codec, span)) = coding_cookie(bytes, usize::from(bom) * 3) {
        let normalized = codec.to_ascii_lowercase().replace('_', "-");
        let utf8 = matches!(normalized.as_str(), "utf-8" | "utf8");
        let ascii = matches!(normalized.as_str(), "ascii" | "us-ascii");
        if bom && !utf8 {
            return Err(PythonSourceDecodeFault::ConflictingBom {
                codec: codec.into(),
                span,
            });
        }
        if !utf8 && !ascii {
            return Err(PythonSourceDecodeFault::UnsupportedCodec {
                codec: codec.into(),
                span,
            });
        }
        if ascii && let Some(at) = bytes.iter().position(|byte| !byte.is_ascii()) {
            return Err(PythonSourceDecodeFault::InvalidAscii {
                span: Span {
                    start: at as u32,
                    end: at as u32 + 1,
                },
            });
        }
    }
    std::str::from_utf8(bytes).map_err(|error| {
        let start = error.valid_up_to();
        let end = start + error.error_len().unwrap_or(bytes.len() - start);
        PythonSourceDecodeFault::InvalidUtf8 {
            span: Span {
                start: start as u32,
                end: end as u32,
            },
        }
    })
}

fn coding_cookie(bytes: &[u8], skip: usize) -> Option<(&str, Span)> {
    let mut offset = skip;
    for _ in 0..2 {
        let remainder = &bytes[offset..];
        if remainder.is_empty() {
            break;
        }
        let end = remainder
            .iter()
            .position(|byte| matches!(byte, b'\r' | b'\n'))
            .map_or(remainder.len(), |at| {
                at + if remainder[at] == b'\r' && remainder.get(at + 1) == Some(&b'\n') {
                    2
                } else {
                    1
                }
            });
        let line = &remainder[..end];
        let first = line
            .iter()
            .position(|byte| !matches!(byte, b' ' | b'\t' | b'\x0c'))
            .unwrap_or(line.len());
        let stripped = &line[first..];
        if !stripped.iter().all(|byte| matches!(byte, b'\r' | b'\n')) && !stripped.starts_with(b"#")
        {
            return None;
        }
        if stripped.starts_with(b"#") {
            for at in 0..line.len().saturating_sub(6) {
                if &line[at..at + 6] != b"coding" || !matches!(line[at + 6], b':' | b'=') {
                    continue;
                }
                let mut start = at + 7;
                while line
                    .get(start)
                    .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
                {
                    start += 1;
                }
                let mut end = start;
                while line.get(end).is_some_and(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')
                }) {
                    end += 1;
                }
                if end > start {
                    return Some((
                        std::str::from_utf8(&line[start..end]).ok()?,
                        Span {
                            start: (offset + start) as u32,
                            end: (offset + end) as u32,
                        },
                    ));
                }
            }
        }
        offset += line.len();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_intake_borrows_utf8_and_ascii_without_changing_offsets() {
        for bytes in [
            b"value = 'caf\xc3\xa9'\n".as_slice(),
            b"# coding: utf_8\nvalue = 1\n",
            b"\xef\xbb\xbf# coding: utf-8\nvalue = 1\n",
            b"# coding: ascii\nvalue = 1\n",
        ] {
            let text = decode_python_source(bytes).expect("supported source codec");
            assert_eq!(text.as_ptr(), bytes.as_ptr());
            assert_eq!(text.as_bytes(), bytes);
        }
    }

    #[test]
    fn source_intake_reports_raw_fault_extent_and_cookie_policy() {
        assert_eq!(
            decode_python_source(b"x = '\xff'\n"),
            Err(PythonSourceDecodeFault::InvalidUtf8 {
                span: Span { start: 5, end: 6 }
            })
        );
        assert!(
            matches!(decode_python_source(b"# coding: latin-1\nx = '\xfa'\n"), Err(PythonSourceDecodeFault::UnsupportedCodec { codec, span: Span { start: 10, end: 17 } }) if codec.as_ref() == "latin-1")
        );
        assert!(matches!(
            decode_python_source(b"\xef\xbb\xbf# coding: latin-1\n"),
            Err(PythonSourceDecodeFault::ConflictingBom {
                span: Span { start: 13, end: 20 },
                ..
            })
        ));
        assert_eq!(
            decode_python_source(b"# coding: ascii\nx = '\xc3\xa9'\n"),
            Err(PythonSourceDecodeFault::InvalidAscii {
                span: Span { start: 21, end: 22 }
            })
        );
        // A second-line cookie is allowed only after a blank or comment line.
        assert!(matches!(
            decode_python_source(b"#!/usr/bin/python\n# coding: cp1252\n"),
            Err(PythonSourceDecodeFault::UnsupportedCodec { .. })
        ));
        assert_eq!(
            decode_python_source(b"x = 1\n# coding: latin-1\n")
                .expect("cookie after code is ignored"),
            "x = 1\n# coding: latin-1\n"
        );
        for bytes in [
            b"# comment\r# coding: cp1252\r".as_slice(),
            b"# comment\r\n# coding: cp1252\r\n".as_slice(),
        ] {
            assert!(matches!(
                decode_python_source(bytes),
                Err(PythonSourceDecodeFault::UnsupportedCodec { codec, .. }) if codec.as_ref() == "cp1252"
            ));
        }
    }
}
