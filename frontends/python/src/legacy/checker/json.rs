//! Decodes one pyrefly JSON transcript into diagnostic rows.
use super::*;

/// Decodes the diagnostic rows of one pyrefly JSON transcript.
///
/// pyrefly appends human-readable summary lines after the JSON document on
/// stdout, so the decoder reads exactly one balanced top-level value and
/// ignores everything after it.
pub(super) fn decode_diagnostics(transcript: &[u8]) -> Result<Vec<RawDiagnostic>, CheckerError> {
    let start = transcript
        .iter()
        .position(|byte| *byte == b'{')
        .ok_or_else(|| CheckerError::Decode {
            message: "no JSON document".to_owned(),
            transcript: head_text(transcript),
        })?;
    let mut parser = JsonParser {
        bytes: transcript,
        cursor: start,
    };
    let mut rows = Vec::new();
    parser
        .read_document(|parser| parser.read_error_row(&mut rows))
        .map_err(|message| CheckerError::Decode {
            message,
            transcript: head_text(transcript),
        })?;
    Ok(rows)
}

/// A minimal JSON reader over the pyrefly transcript schema.
struct JsonParser<'a> {
    bytes: &'a [u8],
    cursor: usize,
}

impl JsonParser<'_> {
    /// Skips whitespace.
    fn skip_whitespace(&mut self) {
        while matches!(
            self.bytes.get(self.cursor),
            Some(b' ') | Some(b'\t') | Some(b'\n') | Some(b'\r')
        ) {
            self.cursor += 1;
        }
    }

    /// Consumes the next byte or fails.
    fn take(&mut self, expected: u8) -> Result<(), String> {
        self.skip_whitespace();
        if self.bytes.get(self.cursor) == Some(&expected) {
            self.cursor += 1;
            Ok(())
        } else {
            Err(format!("expected `{}`", char::from(expected)))
        }
    }

    /// Consumes one byte when present.
    fn try_take(&mut self, expected: u8) -> bool {
        self.skip_whitespace();
        if self.bytes.get(self.cursor) == Some(&expected) {
            self.cursor += 1;
            true
        } else {
            false
        }
    }

    /// Reads one balanced top-level object, visiting each diagnostic entry of
    /// its `errors` array through `visit`.
    fn read_document(
        &mut self,
        mut visit: impl FnMut(&mut Self) -> Result<(), String>,
    ) -> Result<(), String> {
        self.take(b'{')?;
        if self.try_take(b'}') {
            return Ok(());
        }
        loop {
            let key = self.read_string()?;
            if key == "errors" {
                self.take(b':')?;
                self.take(b'[')?;
                if !self.try_take(b']') {
                    loop {
                        visit(self)?;
                        if self.try_take(b',') {
                            continue;
                        }
                        self.take(b']')?;
                        break;
                    }
                }
            } else {
                self.take(b':')?;
                self.skip_value()?;
            }
            if self.try_take(b',') {
                continue;
            }
            self.take(b'}')?;
            return Ok(());
        }
    }

    /// Reads one diagnostic object into a [`RawDiagnostic`].
    fn read_error_row(&mut self, rows: &mut Vec<RawDiagnostic>) -> Result<(), String> {
        self.take(b'{')?;
        let mut row = RawDiagnostic {
            name: String::new(),
            line: 0,
            column: 0,
            stop_line: 0,
            stop_column: 0,
            description: None,
        };
        if !self.try_take(b'}') {
            loop {
                let key = self.read_string()?;
                self.take(b':')?;
                match key.as_str() {
                    "name" => row.name = self.read_string()?,
                    "line" => row.line = self.read_u32()?,
                    "column" => row.column = self.read_u32()?,
                    "stop_line" => row.stop_line = self.read_u32()?,
                    "stop_column" => row.stop_column = self.read_u32()?,
                    "description" => row.description = Some(self.read_string()?),
                    _ => self.skip_value()?,
                }
                if self.try_take(b',') {
                    continue;
                }
                self.take(b'}')?;
                break;
            }
        }
        rows.push(row);
        Ok(())
    }

    /// Reads one JSON string with its escapes decoded.
    fn read_string(&mut self) -> Result<String, String> {
        self.take(b'"')?;
        let mut out = String::new();
        loop {
            let byte = *self.bytes.get(self.cursor).ok_or("unterminated string")?;
            self.cursor += 1;
            match byte {
                b'"' => return Ok(out),
                b'\\' => out.push(self.read_escape()?),
                _ => {
                    let width = utf8_width(byte);
                    let start = self.cursor - 1;
                    let end = start + width;
                    let slice = self
                        .bytes
                        .get(start..end)
                        .ok_or("truncated UTF-8 sequence")?;
                    let text = core::str::from_utf8(slice).map_err(|_| "invalid UTF-8")?;
                    out.push_str(text);
                    self.cursor = end;
                }
            }
        }
    }

    /// Reads one escape sequence after its backslash.
    fn read_escape(&mut self) -> Result<char, String> {
        let escape = *self.bytes.get(self.cursor).ok_or("unterminated escape")?;
        self.cursor += 1;
        match escape {
            b'"' => Ok('"'),
            b'\\' => Ok('\\'),
            b'/' => Ok('/'),
            b'b' => Ok('\u{0008}'),
            b'f' => Ok('\u{000C}'),
            b'n' => Ok('\n'),
            b'r' => Ok('\r'),
            b't' => Ok('\t'),
            b'u' => self.read_unicode_escape(),
            _ => Err("unknown escape".to_owned()),
        }
    }

    /// Reads one `\uXXXX` escape, joining UTF-16 surrogate pairs.
    fn read_unicode_escape(&mut self) -> Result<char, String> {
        let high = self.read_hex4()?;
        if (0xD800..0xDC00).contains(&high) {
            if self.bytes.get(self.cursor) == Some(&b'\\')
                && self.bytes.get(self.cursor + 1) == Some(&b'u')
            {
                self.cursor += 2;
                let low = self.read_hex4()?;
                if (0xDC00..0xE000).contains(&low) {
                    let combined =
                        0x10000 + ((u32::from(high) - 0xD800) << 10) + (u32::from(low) - 0xDC00);
                    return char::from_u32(combined).ok_or_else(|| "invalid surrogate".to_owned());
                }
            }
            return Err("unpaired surrogate".to_owned());
        }
        char::from_u32(u32::from(high)).ok_or_else(|| "invalid code point".to_owned())
    }

    /// Reads exactly four hexadecimal digits.
    fn read_hex4(&mut self) -> Result<u16, String> {
        let slice = self
            .bytes
            .get(self.cursor..self.cursor + 4)
            .ok_or("truncated unicode escape")?;
        let text = core::str::from_utf8(slice).map_err(|_| "invalid escape".to_owned())?;
        let value = u16::from_str_radix(text, 16).map_err(|_| "invalid hex digits".to_owned())?;
        self.cursor += 4;
        Ok(value)
    }

    /// Reads one unsigned integer cell.
    fn read_u32(&mut self) -> Result<u32, String> {
        self.skip_whitespace();
        let start = self.cursor;
        while self
            .bytes
            .get(self.cursor)
            .is_some_and(|byte| byte.is_ascii_digit())
        {
            self.cursor += 1;
        }
        let text = self
            .bytes
            .get(start..self.cursor)
            .ok_or("truncated number")?;
        let text = core::str::from_utf8(text).map_err(|_| "invalid number".to_owned())?;
        if text.is_empty() {
            return Err("empty number".to_owned());
        }
        text.parse::<u32>()
            .map_err(|_| format!("number `{text}` exceeds the u32 cell"))
    }

    /// Skips one balanced JSON value of any kind.
    fn skip_value(&mut self) -> Result<(), String> {
        self.skip_whitespace();
        match self.bytes.get(self.cursor) {
            Some(b'{') => {
                self.cursor += 1;
                if !self.try_take(b'}') {
                    loop {
                        drop(self.read_string()?);
                        self.take(b':')?;
                        self.skip_value()?;
                        if self.try_take(b',') {
                            continue;
                        }
                        self.take(b'}')?;
                        break;
                    }
                }
                Ok(())
            }
            Some(b'[') => {
                self.cursor += 1;
                if !self.try_take(b']') {
                    loop {
                        self.skip_value()?;
                        if self.try_take(b',') {
                            continue;
                        }
                        self.take(b']')?;
                        break;
                    }
                }
                Ok(())
            }
            Some(b'"') => {
                drop(self.read_string()?);
                Ok(())
            }
            Some(b't') => self.skip_literal(b"true"),
            Some(b'f') => self.skip_literal(b"false"),
            Some(b'n') => self.skip_literal(b"null"),
            Some(byte) if byte.is_ascii_digit() || *byte == b'-' => {
                while self.bytes.get(self.cursor).is_some_and(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+' | b'e' | b'E')
                }) {
                    self.cursor += 1;
                }
                Ok(())
            }
            _ => Err("unexpected value".to_owned()),
        }
    }

    /// Skips one keyword literal.
    fn skip_literal(&mut self, keyword: &[u8]) -> Result<(), String> {
        if self.bytes.get(self.cursor..self.cursor + keyword.len()) == Some(keyword) {
            self.cursor += keyword.len();
            Ok(())
        } else {
            Err("invalid literal".to_owned())
        }
    }
}

/// The UTF-8 sequence width named by a leading byte.
const fn utf8_width(first: u8) -> usize {
    match first {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}
