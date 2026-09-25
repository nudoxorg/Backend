//! Percent-encoding for a single URL path segment.
//!
//! Unreserved bytes (`A-Za-z0-9-_.~`) pass through. Every other byte becomes
//! `%HH`. Callers that need dots as separators (a Maven group id) split
//! first, then encode each piece.

/// Encode `raw` as one URL path segment.
#[must_use]
pub fn path_segment(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::path_segment;

    #[test]
    fn unreserved_bytes_are_identity_and_the_rest_are_percent() {
        assert_eq!(path_segment("AZaz09-_.~"), "AZaz09-_.~");
        assert_eq!(path_segment(""), "");
        assert_eq!(path_segment("a b+c%"), "a%20b%2Bc%25");
        assert_eq!(path_segment("é"), "%C3%A9");
    }
}
