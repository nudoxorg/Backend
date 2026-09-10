//! Defines hex behavior for `interface-identity`, whose purpose is to spell, parse, and abbreviate every identity a person or agent can name.
//! This module owns the hex invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Allocation-free lower-hex rendering and strict parsing of compact identity payloads.

use core::fmt::{self, Write as _};

/// Compact identity payload width shared by declaration families and variant fingerprints.
pub const KEY_HEX_BYTES: usize = 16;

/// Exact rejection of a hex spelling.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HexParseError {
    /// The text was not exactly twice the payload width.
    Width {
        /// Observed UTF-8 length.
        observed: usize,
        /// Required UTF-8 length.
        expected: usize,
    },
    /// A byte was not a lower-case hexadecimal digit.
    Digit {
        /// Byte offset of the offending character.
        offset: usize,
    },
}

/// Renders one nibble arithmetically, so no lookup table has to be indexed.
const fn nibble(value: u8) -> u8 {
    let low = value & 0x0f;
    if low < 10 { b'0' + low } else { b'a' + (low - 10) }
}

pub(crate) fn write_lower_hex(formatter: &mut fmt::Formatter<'_>, bytes: &[u8]) -> fmt::Result {
    for byte in bytes {
        formatter.write_char(char::from(nibble(byte >> 4)))?;
        formatter.write_char(char::from(nibble(*byte)))?;
    }
    Ok(())
}

pub(crate) fn parse_lower_hex<const WIDTH: usize>(
    text: &str,
) -> Result<[u8; WIDTH], HexParseError> {
    let bytes = text.as_bytes();
    if bytes.len() != WIDTH * 2 {
        return Err(HexParseError::Width {
            observed: bytes.len(),
            expected: WIDTH * 2,
        });
    }
    let mut output = [0_u8; WIDTH];
    for (index, slot) in output.iter_mut().enumerate() {
        let high = digit(bytes, index * 2)?;
        let low = digit(bytes, index * 2 + 1)?;
        *slot = (high << 4) | low;
    }
    Ok(output)
}

fn digit(bytes: &[u8], offset: usize) -> Result<u8, HexParseError> {
    match bytes.get(offset) {
        Some(byte @ b'0'..=b'9') => Ok(byte - b'0'),
        Some(byte @ b'a'..=b'f') => Ok(byte - b'a' + 10),
        _ => Err(HexParseError::Digit { offset }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Hex<'bytes>(&'bytes [u8]);

    impl fmt::Display for Hex<'_> {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            write_lower_hex(formatter, self.0)
        }
    }

    #[test]
    fn renders_and_parses_exactly() {
        let bytes = [0x0f_u8, 0xa0, 0xff, 0x00];
        let text = Hex(&bytes).to_string();
        assert_eq!(text, "0fa0ff00");
        assert_eq!(parse_lower_hex::<4>(&text), Ok(bytes));
        assert_eq!(
            parse_lower_hex::<4>("0FA0FF00"),
            Err(HexParseError::Digit { offset: 1 })
        );
        assert_eq!(
            parse_lower_hex::<4>("0fa0ff"),
            Err(HexParseError::Width {
                observed: 6,
                expected: 8
            })
        );
    }
}
