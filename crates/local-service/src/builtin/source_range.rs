//! Byte-preserving reads from source bytes checked against an indexed file fact.
//!
//! The caller must first select the exact project, revision and complete file
//! facts. Native extents require selected compiler identity and provenance;
//! reopened host bytes must match the indexed source and UTF-8 bounds.

use backend_compile::SourceBindingCapture;
use backend_version::{ContentId, SourceFactDomain};

/// A borrowed page relative to an exact declaration owner extent.
#[derive(Debug, Eq, PartialEq)]
pub(super) struct SourceRange<'a> {
    pub(super) text: &'a str,
    pub(super) start: u32,
    pub(super) end: u32,
    pub(super) total: u32,
    pub(super) owner_start_line: Option<u32>,
}

/// An owned page that may cross the owner transport boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct OwnedSourceRange {
    pub(super) text: String,
    pub(super) start: u32,
    pub(super) end: u32,
    pub(super) total: u32,
    pub(super) source_identity: [u8; 32],
    pub(super) owner_start_line: Option<u32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SourceRangeError {
    Unavailable,
    Stale,
    InvalidExtent,
    InvalidOffset,
    InvalidLimit,
}

/// Reads a native compiler-selected host extent after revalidating the host.
/// The caller must prove the selected publication, identity, path and extent.
pub(super) fn native_page<'a>(
    host: &'a [u8],
    expected: ContentId<SourceFactDomain>,
    expected_input: [u8; 32],
    extent: (u32, u32),
    offset: u32,
    limit: u32,
) -> Result<SourceRange<'a>, SourceRangeError> {
    if !(4..=4096).contains(&limit) {
        return Err(SourceRangeError::InvalidLimit);
    }
    let verified =
        backend_compile::VerifiedSourceText::admit_stored(host, expected_input, expected).map_err(
            |error| match error {
                backend_compile::SourceVerificationError::InputRevisionMismatch
                | backend_compile::SourceVerificationError::SourceIdentityMismatch
                | backend_compile::SourceVerificationError::NonUtf8 => SourceRangeError::Stale,
                _ => SourceRangeError::InvalidExtent,
            },
        )?;
    let owner = verified
        .extent(extent.0 as usize..extent.1 as usize)
        .map_err(|_| SourceRangeError::InvalidExtent)?;
    let start = usize::try_from(offset).map_err(|_| SourceRangeError::InvalidOffset)?;
    if start > owner.text().len() || !owner.text().is_char_boundary(start) {
        return Err(SourceRangeError::InvalidOffset);
    }
    let mut end = start.saturating_add(limit as usize).min(owner.text().len());
    while !owner.text().is_char_boundary(end) {
        end -= 1;
    }
    Ok(SourceRange {
        text: owner
            .subregion(start..end)
            .map_err(|_| SourceRangeError::InvalidOffset)?
            .text(),
        start: offset,
        end: u32::try_from(end).map_err(|_| SourceRangeError::InvalidExtent)?,
        total: u32::try_from(owner.text().len()).map_err(|_| SourceRangeError::InvalidExtent)?,
        owner_start_line: observed_start_line(host, extent.0 as usize, owner.text()),
    })
}

/// Declaration context that precedes a native extent.
///
/// Compilers keep modifiers such as `export` and the leading comment outside the
/// declaration they report, so a page that starts at the extent drops exactly the
/// words a reader wants. This is the rest of the extent's first line plus the
/// contiguous comment or decorator lines directly above it.
#[derive(Debug, Eq, PartialEq)]
pub(super) struct NativeContextHead<'a> {
    pub(super) text: &'a str,
    /// Number of line breaks inside `text`; the extent starts that many lines later.
    pub(super) lines: u32,
}

const CONTEXT_LINE_LIMIT: usize = 8;
const CONTEXT_BYTE_LIMIT: usize = 1024;
const SAME_LINE_LIMIT: usize = 128;

fn is_context_line(trimmed: &str) -> bool {
    if trimmed.is_empty() {
        return false;
    }
    trimmed.starts_with("//")
        || trimmed.starts_with("/*")
        || trimmed == "*"
        || trimmed.starts_with("* ")
        || trimmed.starts_with("*/")
        || trimmed.starts_with("--")
        || trimmed.starts_with('@')
        || trimmed.starts_with("#[")
        || trimmed.starts_with("# ")
        || trimmed == "#"
        || trimmed.starts_with("##")
}

/// Words and punctuation that may precede a declaration name on its own line
/// (`export default `, `pub(crate) `, `public static `, indentation).
fn is_modifier_prefix(prefix: &str) -> bool {
    prefix.len() <= SAME_LINE_LIMIT
        && !prefix.contains(['{', '}', ';', '=', ',', '`', '"', '\''])
}

/// Splits the verified bytes before `extent_start` into the leading context head.
pub(super) fn native_context_head<'a>(
    host: &'a [u8],
    expected: ContentId<SourceFactDomain>,
    expected_input: [u8; 32],
    extent: (u32, u32),
) -> Result<NativeContextHead<'a>, SourceRangeError> {
    let verified =
        backend_compile::VerifiedSourceText::admit_stored(host, expected_input, expected).map_err(
            |error| match error {
                backend_compile::SourceVerificationError::InputRevisionMismatch
                | backend_compile::SourceVerificationError::SourceIdentityMismatch
                | backend_compile::SourceVerificationError::NonUtf8 => SourceRangeError::Stale,
                _ => SourceRangeError::InvalidExtent,
            },
        )?;
    let prefix = verified
        .extent(0..extent.0 as usize)
        .map_err(|_| SourceRangeError::InvalidExtent)?
        .text();
    Ok(context_head(prefix))
}

fn context_head(prefix: &str) -> NativeContextHead<'_> {
    let empty = NativeContextHead { text: &prefix[prefix.len()..], lines: 0 };
    let line_start = prefix.rfind('\n').map_or(0, |at| at + 1);
    if !is_modifier_prefix(&prefix[line_start..]) {
        return empty;
    }
    let mut cursor = line_start;
    let mut taken = 0;
    while cursor > 0 && taken < CONTEXT_LINE_LIMIT {
        let line_end = cursor - 1;
        let line_start = prefix[..line_end].rfind('\n').map_or(0, |at| at + 1);
        let line = &prefix[line_start..line_end];
        if !is_context_line(line.trim())
            || prefix.len() - line_start > CONTEXT_BYTE_LIMIT + SAME_LINE_LIMIT
        {
            break;
        }
        cursor = line_start;
        taken += 1;
    }
    let text = &prefix[cursor..];
    let lines = u32::try_from(text.bytes().filter(|byte| *byte == b'\n').count()).unwrap_or(0);
    NativeContextHead { text, lines }
}

// The renderer numbers lines with Rust's LF/CRLF convention. Do not assign a
// site when a bare CR or a Unicode line separator would make those numbers
// misleading. `native_page` calls this only after verifying the entire host
// and admitting the selected extent on that same byte buffer.
fn observed_start_line(host: &[u8], owner_start: usize, owner: &str) -> Option<u32> {
    let prefix = std::str::from_utf8(host.get(..owner_start)?).ok()?;
    let representable = |text: &str| {
        !text.contains('\u{2028}') && !text.contains('\u{2029}')
            && text.as_bytes().iter().enumerate().all(|(at, byte)| {
                *byte != b'\r' || text.as_bytes().get(at + 1) == Some(&b'\n')
            })
    };
    if !representable(prefix) || !representable(owner) {
        return None;
    }
    u32::try_from(prefix.bytes().filter(|byte| *byte == b'\n').count())
        .ok()?
        .checked_add(1)
}

/// Checks the host against the selected source identity and binding evidence.
/// Offsets are relative to the owner and never rounded at the start boundary.
pub(super) fn page<'a>(
    host: &'a [u8],
    expected: ContentId<SourceFactDomain>,
    expected_input: [u8; 32],
    binding: SourceBindingCapture,
    name: &str,
    offset: u32,
    limit: u32,
) -> Result<SourceRange<'a>, SourceRangeError> {
    const MAX_BYTES: u32 = 4096;
    if !(4..=MAX_BYTES).contains(&limit) {
        return Err(SourceRangeError::InvalidLimit);
    }
    let verified =
        backend_compile::VerifiedSourceText::admit_stored(host, expected_input, expected).map_err(
            |error| match error {
                backend_compile::SourceVerificationError::InputRevisionMismatch
                | backend_compile::SourceVerificationError::SourceIdentityMismatch
                | backend_compile::SourceVerificationError::NonUtf8 => SourceRangeError::Stale,
                _ => SourceRangeError::InvalidExtent,
            },
        )?;
    let verified_binding = verified
        .binding(binding)
        .map_err(|_| SourceRangeError::InvalidExtent)?;
    if verified_binding.name().text() != name {
        return Err(SourceRangeError::InvalidExtent);
    }
    let owner = verified_binding.owner().text();
    let start = usize::try_from(offset).map_err(|_| SourceRangeError::InvalidOffset)?;
    if start > owner.len() || !owner.is_char_boundary(start) {
        return Err(SourceRangeError::InvalidOffset);
    }
    let mut end = start.saturating_add(limit as usize).min(owner.len());
    while !owner.is_char_boundary(end) {
        end -= 1;
    }
    Ok(SourceRange {
        text: verified_binding
            .owner()
            .subregion(start..end)
            .map_err(|_| SourceRangeError::InvalidOffset)?
            .text(),
        start: offset,
        end: u32::try_from(end).map_err(|_| SourceRangeError::InvalidExtent)?,
        total: u32::try_from(owner.len()).map_err(|_| SourceRangeError::InvalidExtent)?,
        owner_start_line: observed_start_line(host, binding.owner().0 as usize, owner),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_owner_line_uses_verified_host_and_refuses_unrenderable_breaks() {
        for (prefix, body, expected) in [
            ("// é\r\n// 漢\n", "function f() {\r\n  return 1;\r\n}", Some(3)),
            ("// old\rline\n", "function f() {}", None),
            ("// old\u{2028}line\n", "function f() {}", None),
            ("// old\u{2029}line\n", "function f() {}", None),
            ("// before\n", "function f() {\rreturn 1;}", None),
            ("// before\n", "function f() {\u{2028}return 1;}", None),
        ] {
            let host = format!("{prefix}{body}");
            let source = ContentId::<SourceFactDomain>::from_canonical_bytes(host.as_bytes());
            let input = backend_compile::typed_of::<backend_compile::InputContentSchema>(host.as_bytes()).to_bytes();
            let extent = (prefix.len() as u32, host.len() as u32);
            let page = native_page(host.as_bytes(), source, input, extent, 0, 4096).expect("admitted extent");
            assert_eq!(page.owner_start_line, expected);
            assert_eq!(page.text, body);
            let changed = host.replace('f', "g");
            assert_eq!(native_page(changed.as_bytes(), source, input, extent, 0, 4096), Err(SourceRangeError::Stale));
        }
    }

    #[test]
    fn native_extent_pages_preserve_bytes_and_reject_stale_and_invalid_offsets() {
        let body = format!(
            "function ferris() {{\r\n{}\r\n}}\r\n",
            "  const value = \"é漢🦀\";\r\n".repeat(200)
        );
        let host = format!("// before\r\n{body}// after");
        let start = host.find("function ferris").expect("owner");
        let extent = (start as u32, (start + body.len()) as u32);
        let source = ContentId::<SourceFactDomain>::from_canonical_bytes(host.as_bytes());
        let input =
            backend_compile::typed_of::<backend_compile::InputContentSchema>(host.as_bytes())
                .to_bytes();
        let mut offset = 0;
        let mut result = String::new();
        loop {
            let page = native_page(host.as_bytes(), source, input, extent, offset, 2048)
                .expect("native page");
            result.push_str(page.text);
            offset = page.end;
            if page.end == page.total {
                break;
            }
        }
        assert_eq!(result.as_bytes(), body.as_bytes());
        assert_eq!(
            native_page(host.as_bytes(), source, input, extent, offset, 2048)
                .expect("EOF")
                .text,
            ""
        );
        let multibyte = body.find('é').expect("multibyte") as u32;
        assert_eq!(
            native_page(host.as_bytes(), source, input, extent, multibyte + 1, 2048),
            Err(SourceRangeError::InvalidOffset)
        );
        let changed = host.replace("value", "other");
        assert_eq!(
            native_page(changed.as_bytes(), source, input, extent, 0, 2048),
            Err(SourceRangeError::Stale)
        );
        assert_eq!(
            native_page(
                host.as_bytes(),
                source,
                input,
                (extent.0, host.len() as u32 + 1),
                0,
                2048
            ),
            Err(SourceRangeError::InvalidExtent)
        );
    }

    #[test]
    fn unicode_pages_preserve_crlf_and_final_newline_and_reject_stale_bytes() {
        let body = format!(
            "function ferris() {{\r\n{}\r\n}}\r\n",
            "  const value = \"é漢🦀\";\r\n".repeat(200)
        );
        let host = format!("// prefix\n{body}// suffix");
        let start = host.find("function ferris").expect("owner");
        let name = host.find("ferris").expect("name");
        let capture = SourceBindingCapture::new(
            start as u32,
            (start + body.len()) as u32,
            name as u32,
            (name + 6) as u32,
        )
        .expect("binding");
        let identity = ContentId::<SourceFactDomain>::from_canonical_bytes(host.as_bytes());
        let input =
            backend_compile::typed_of::<backend_compile::InputContentSchema>(host.as_bytes())
                .to_bytes();
        let mut offset = 0;
        let mut result = String::new();
        loop {
            let page = page(
                host.as_bytes(),
                identity,
                input,
                capture,
                "ferris",
                offset,
                37,
            )
            .expect("page");
            result.push_str(page.text);
            if page.end == page.total {
                break;
            }
            assert!(page.end > offset);
            offset = page.end;
        }
        assert_eq!(result.as_bytes(), body.as_bytes());
        assert_eq!(
            page(
                host.replace("prefix", "PREFIX").as_bytes(),
                identity,
                input,
                capture,
                "ferris",
                0,
                37
            ),
            Err(SourceRangeError::Stale)
        );
        assert_eq!(
            page(host.as_bytes(), identity, [0; 32], capture, "ferris", 0, 37),
            Err(SourceRangeError::Stale)
        );
        let wrong_source = ContentId::<SourceFactDomain>::from_canonical_bytes(b"other source");
        assert_eq!(
            page(
                host.as_bytes(),
                wrong_source,
                input,
                capture,
                "ferris",
                0,
                37
            ),
            Err(SourceRangeError::Stale)
        );
        let mut malformed = host.as_bytes().to_vec();
        malformed[0] = 0xff;
        assert_eq!(
            page(&malformed, identity, input, capture, "ferris", 0, 37),
            Err(SourceRangeError::Stale)
        );
        let mid = body.find('é').expect("multibyte") as u32 + 1;
        assert_eq!(
            page(host.as_bytes(), identity, input, capture, "ferris", mid, 37),
            Err(SourceRangeError::InvalidOffset)
        );
        assert_eq!(
            page(host.as_bytes(), identity, input, capture, "other", 0, 37),
            Err(SourceRangeError::InvalidExtent)
        );
        let eof = page(
            host.as_bytes(),
            identity,
            input,
            capture,
            "ferris",
            body.len() as u32,
            37,
        )
        .expect("EOF");
        assert_eq!(eof.text, "");
    }
    #[test]
    fn one_long_multibyte_line_pages_without_gaps_or_oversize() {
        let host = format!("const ferris = \"{}\";", "🦀".repeat(3000));
        let name = host.find("ferris").expect("name");
        let capture =
            SourceBindingCapture::new(0, host.len() as u32, name as u32, (name + 6) as u32)
                .expect("binding");
        let identity = ContentId::<SourceFactDomain>::from_canonical_bytes(host.as_bytes());
        let input =
            backend_compile::typed_of::<backend_compile::InputContentSchema>(host.as_bytes())
                .to_bytes();
        let mut offset = 0;
        let mut result = String::new();
        loop {
            let next = page(
                host.as_bytes(),
                identity,
                input,
                capture,
                "ferris",
                offset,
                4095,
            )
            .expect("page");
            assert!(next.text.len() <= 4095);
            result.push_str(next.text);
            if next.end == next.total {
                break;
            }
            assert!(next.end > offset);
            offset = next.end;
        }
        assert_eq!(result, host);
    }
}

#[cfg(test)]
mod context_head_tests {
    use super::context_head;

    #[test]
    fn modifiers_on_the_extent_line_are_kept() {
        let prefix = "import a from 'a';\n\nexport ";
        let head = context_head(prefix);
        assert_eq!(head.text, "export ");
        assert_eq!(head.lines, 0);
    }

    #[test]
    fn contiguous_leading_comments_and_decorators_are_kept_but_not_code() {
        let prefix = "const x = 1;\n// why this exists\n/** doc */\n@Injectable()\nexport default ";
        let head = context_head(prefix);
        assert_eq!(head.text, "// why this exists\n/** doc */\n@Injectable()\nexport default ");
        assert_eq!(head.lines, 3);
    }

    #[test]
    fn a_blank_line_ends_the_context() {
        let head = context_head("// detached\n\nexport ");
        assert_eq!(head.text, "export ");
    }

    #[test]
    fn code_before_the_extent_on_the_same_line_is_not_context() {
        let head = context_head("// note\nconst a = 1; ");
        assert_eq!(head.text, "");
        assert_eq!(head.lines, 0);
    }

    #[test]
    fn an_extent_at_the_start_of_the_file_has_no_head() {
        assert_eq!(context_head("").text, "");
    }

    #[test]
    fn the_context_is_bounded() {
        let prefix = "// c\n".repeat(50) + "export ";
        let head = context_head(&prefix);
        assert_eq!(head.lines, 8);
        assert!(head.text.ends_with("export "));
    }
}
