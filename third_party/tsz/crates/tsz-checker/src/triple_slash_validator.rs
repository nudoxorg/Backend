//! Triple-slash reference directive validation
//!
//! Validates /// <reference path="..." /> directives in TypeScript files.

use std::path::Path;
use tsz_common::{ExecutionCheckpoint, ProjectExecutionStop};
use tsz_scanner::scanner_impl::{is_line_break, is_white_space_single_line};

/// Borrowed attribute bytes and their exact source position.
#[derive(Clone, Copy, Debug)]
pub struct ReferenceValue<'source> {
    pub text: &'source str,
    pub offset: usize,
}

/// One native triple-slash directive. Values borrow the admitted source bytes.
#[derive(Debug)]
pub struct ReferenceDirective<'source> {
    pub offset: usize,
    pub length: usize,
    pub path: Option<ReferenceValue<'source>>,
    pub types: Option<ReferenceValue<'source>>,
    pub lib: Option<ReferenceValue<'source>>,
    pub resolution_mode: Option<ReferenceValue<'source>>,
    pub no_default_lib: Option<ReferenceValue<'source>>,
}

/// Stream leading reference directives using TSZ's tag/attribute interpretation
/// and scanner character classes. Even a single large comment observes the
/// shared work/cancellation checkpoint; non-directive comments allocate nothing.
pub fn reference_directives<'source, 'checkpoint>(
    source: &'source str,
    checkpoint: &'checkpoint dyn ExecutionCheckpoint,
) -> impl Iterator<Item = Result<ReferenceDirective<'source>, ProjectExecutionStop>> + 'checkpoint
where
    'source: 'checkpoint,
{
    reference_directives_with_poll(source, move |work| checkpoint.checkpoint(work))
}

/// Same native stream for compiler stages whose existing poll carries a typed
/// admission error. The callback is borrowed; no ambient checkpoint is installed.
pub fn reference_directives_with_poll<'source, 'poll, E: 'poll>(
    source: &'source str,
    mut poll: impl FnMut(u64) -> Result<(), E> + 'poll,
) -> impl Iterator<Item = Result<ReferenceDirective<'source>, E>> + 'poll
where
    'source: 'poll,
{
    let mut position = 0usize;
    let mut pending = 0u64;
    let mut initial = Some(poll(0));
    let mut done = false;
    std::iter::from_fn(move || {
        if done {
            return None;
        }
        let result = (|| {
            if let Some(initial) = initial.take() {
                initial?;
            }
            let mut tick = |work| {
                pending += work;
                if pending >= 64 {
                    poll(std::mem::take(&mut pending))
                } else {
                    Ok(())
                }
            };
            while position < source.len() {
                let ch = source[position..].chars().next().unwrap();
                tick(ch.len_utf8() as u64)?;
                if is_white_space_single_line(ch as u32) || is_line_break(ch as u32) {
                    position += ch.len_utf8();
                    continue;
                }
                let start = position;
                let rest = &source[start..];
                let block = rest.starts_with("/*");
                let line = rest.starts_with("//");
                if !block && !line && !is_initial_shebang(start, rest) {
                    return Ok(None);
                }
                position += 2;
                while position < source.len() {
                    let ch = source[position..].chars().next().unwrap();
                    tick(ch.len_utf8() as u64)?;
                    if block && source[position..].starts_with("*/") {
                        position += 2;
                        break;
                    }
                    if !block && is_line_break(ch as u32) {
                        break;
                    }
                    position += ch.len_utf8();
                }
                let comment = &source[start..position];
                if line
                    && is_triple_slash_directive_tag_checked(comment, "reference", &mut || tick(1))?
                {
                    let mut attribute = |name| {
                        extract_quoted_attr_borrowed_with_offset(comment, name, &mut || tick(1))
                            .map(|value| {
                                value.map(|(text, offset)| ReferenceValue {
                                    text,
                                    offset: start + offset,
                                })
                            })
                    };
                    return Ok(Some(ReferenceDirective {
                        offset: start,
                        length: comment.len(),
                        path: attribute("path")?,
                        types: attribute("types")?,
                        lib: attribute("lib")?,
                        resolution_mode: attribute("resolution-mode")?,
                        no_default_lib: attribute("no-default-lib")?,
                    }));
                }
            }
            Ok(None)
        })();
        match result {
            Ok(Some(directive)) => Some(Ok(directive)),
            Ok(None) => {
                done = true;
                poll(std::mem::take(&mut pending)).err().map(Err)
            }
            Err(stop) => {
                done = true;
                Some(Err(stop))
            }
        }
    })
}

/// Extract triple-slash reference paths from source text
///
/// Returns a vector of (path, `line_number`, `quote_offset`) tuples for each reference directive found.
/// `quote_offset` is the byte offset of the value start (after the opening quote) within the original (untrimmed) line.
pub fn extract_reference_paths(source: &str) -> Vec<(String, usize, usize)> {
    let mut references = Vec::new();
    let mut in_block_comment = false;
    let mut past_prologue = false;

    for (line_num, line) in source.lines().enumerate() {
        let trimmed = line.trim();

        // Track block comment state: skip lines inside /* ... */ comments.
        if in_block_comment {
            if let Some(end_idx) = trimmed.find("*/") {
                // Block comment ends on this line; check the remainder
                let remainder = trimmed[end_idx + 2..].trim();
                in_block_comment = false;
                // If the remainder starts with ///, it could be a directive,
                // but that's extremely unusual; skip for simplicity.
                if remainder.is_empty() || !remainder.starts_with("///") {
                    continue;
                }
            } else {
                continue;
            }
        }

        // Check for block comment start on this line (before any directive)
        if trimmed.starts_with("/*") {
            if !trimmed.contains("*/") {
                in_block_comment = true;
            }
            continue;
        }

        if is_initial_shebang(line_num, trimmed) {
            continue;
        }

        // Triple-slash directives are only valid before any executable statement.
        // A non-empty line that is not a comment ends the directive prologue.
        if !trimmed.is_empty() && !trimmed.starts_with("//") {
            past_prologue = true;
        }
        if past_prologue {
            continue;
        }

        // Check if it is an exact <reference> tag with a path attribute.
        if !is_triple_slash_directive_tag(trimmed, "reference") {
            continue;
        }

        // Extract the path value between quotes, with offset in the original line
        if let Some((path, offset)) = extract_quoted_path_with_offset(line) {
            references.push((path, line_num, offset));
        }
    }

    references
}

/// Extract `/// <reference types="..." />` directives from source text.
///
/// Returns a vector of (`type_name`, `resolution_mode`, `types_value_byte_offset`, `types_value_length`) tuples.
/// `resolution_mode` is `Some(value)` when a `resolution-mode` attribute is present.
/// `types_value_byte_offset` and `types_value_length` locate the `types` attribute value in the
/// source text (used for TS1453 diagnostics, which tsc anchors at the `types` value span).
pub fn extract_reference_types(source: &str) -> Vec<(String, Option<String>, usize, usize)> {
    let mut references = Vec::new();
    let mut in_block_comment = false;
    let mut byte_offset = 0usize;
    let mut past_prologue = false;

    for line in source.lines() {
        let trimmed = line.trim();

        // Track block comment state
        if in_block_comment {
            if trimmed.contains("*/") {
                in_block_comment = false;
            }
            // +1 for the newline character
            byte_offset += line.len() + 1;
            continue;
        }
        if trimmed.starts_with("/*") {
            if !trimmed.contains("*/") {
                in_block_comment = true;
            }
            byte_offset += line.len() + 1;
            continue;
        }

        if is_initial_shebang(byte_offset, trimmed) {
            byte_offset += line.len() + 1;
            continue;
        }

        // Triple-slash directives are only valid before any executable statement.
        if !trimmed.is_empty() && !trimmed.starts_with("//") {
            past_prologue = true;
        }

        if !past_prologue
            && is_triple_slash_directive_tag(trimmed, "reference")
            && let Some((name, value_offset_in_line)) =
                extract_quoted_attr_with_offset(line, "types")
        {
            let resolution_mode = extract_quoted_attr(trimmed, "resolution-mode");
            let types_byte_offset = byte_offset + value_offset_in_line;
            let types_value_length = name.len();
            references.push((name, resolution_mode, types_byte_offset, types_value_length));
        }

        byte_offset += line.len() + 1;
    }

    references
}

/// Extract `/// <amd-module name="..." />` directives from source text.
///
/// Returns a vector of (`module_name`, `line_number`) tuples for each amd-module directive found.
/// Used to detect multiple AMD module name assignments (TS2458).
pub fn extract_amd_module_names(source: &str) -> Vec<(String, usize)> {
    let mut amd_modules = Vec::new();
    let mut in_block_comment = false;
    let mut past_prologue = false;

    for (line_num, line) in source.lines().enumerate() {
        let trimmed = line.trim();

        // Track block comment state
        if in_block_comment {
            if trimmed.contains("*/") {
                in_block_comment = false;
            }
            continue;
        }
        if trimmed.starts_with("/*") {
            if !trimmed.contains("*/") {
                in_block_comment = true;
            }
            continue;
        }

        if is_initial_shebang(line_num, trimmed) {
            continue;
        }

        // Triple-slash directives are only valid before any executable statement.
        if !trimmed.is_empty() && !trimmed.starts_with("//") {
            past_prologue = true;
        }
        if past_prologue {
            continue;
        }

        // Check if it is an exact <amd-module> tag with a name attribute.
        if !is_triple_slash_directive_tag(trimmed, "amd-module") {
            continue;
        }

        // Extract the name value between quotes
        if let Some(name) = extract_quoted_attr(trimmed, "name") {
            amd_modules.push((name, line_num));
        }
    }

    amd_modules
}

/// Find triple-slash lines that look like reference directives but have invalid syntax.
///
/// Returns `(line_number, line_byte_offset)` for each malformed directive.
/// A directive is malformed if it starts with `/// <reference` but doesn't have
/// properly quoted attributes (path, types, lib, or no-default-lib).
pub fn find_malformed_reference_directives(source: &str) -> Vec<(usize, usize)> {
    let mut malformed = Vec::new();
    let mut in_block_comment = false;
    let mut byte_offset = 0usize;
    let mut past_prologue = false;

    for (line_num, line) in source.lines().enumerate() {
        let trimmed = line.trim();

        // Track block comment state
        if in_block_comment {
            if trimmed.contains("*/") {
                in_block_comment = false;
            }
            byte_offset += line.len() + 1;
            continue;
        }
        if trimmed.starts_with("/*") {
            if !trimmed.contains("*/") {
                in_block_comment = true;
            }
            byte_offset += line.len() + 1;
            continue;
        }

        if is_initial_shebang(byte_offset, trimmed) {
            byte_offset += line.len() + 1;
            continue;
        }

        // Triple-slash directives are only valid before any executable statement.
        if !trimmed.is_empty() && !trimmed.starts_with("//") {
            past_prologue = true;
        }

        // Must be an exact <reference> tag and still be in the prologue.
        if !past_prologue && is_triple_slash_directive_tag(trimmed, "reference") {
            // Check if it's a well-formed directive:
            // Valid forms: path="...", types="...", lib="...", no-default-lib="true"
            let has_valid_path = extract_quoted_attr(trimmed, "path").is_some();
            let has_valid_types = extract_quoted_attr(trimmed, "types").is_some();
            let has_valid_lib = extract_quoted_attr(trimmed, "lib").is_some();
            let has_valid_no_default = extract_quoted_attr(trimmed, "no-default-lib").is_some();

            if !has_valid_path && !has_valid_types && !has_valid_lib && !has_valid_no_default {
                // It looks like a reference directive but has invalid syntax
                // Calculate the offset of the `///` within the line
                let triple_slash_offset = line.find("///").unwrap_or(0);
                malformed.push((line_num, byte_offset + triple_slash_offset));
            }
        }

        byte_offset += line.len() + 1;
    }

    malformed
}

fn is_initial_shebang(position: usize, trimmed: &str) -> bool {
    position == 0 && trimmed.starts_with("#!")
}

fn is_triple_slash_directive_tag(trimmed: &str, tag: &str) -> bool {
    is_triple_slash_directive_tag_checked(trimmed, tag, &mut || Ok::<_, ProjectExecutionStop>(()))
        .unwrap()
}

fn is_triple_slash_directive_tag_checked<E>(
    trimmed: &str,
    tag: &str,
    tick: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(after_slashes) = trimmed.strip_prefix("///") else {
        return Ok(false);
    };
    let mut pos = 0;
    for ch in after_slashes.chars() {
        tick()?;
        if !ch.is_whitespace() {
            break;
        }
        pos += ch.len_utf8();
    }
    let Some(after_open) = after_slashes[pos..].strip_prefix('<') else {
        return Ok(false);
    };
    let Some(rest) = after_open.strip_prefix(tag) else {
        return Ok(false);
    };

    if rest.is_empty() {
        return Ok(true);
    }

    let bytes = rest.as_bytes();
    Ok(match bytes[0] {
        b'>' => true,
        b'/' => bytes.len() == 1 || bytes.get(1) == Some(&b'>'),
        ch => ch.is_ascii_whitespace(),
    })
}

/// Extract the value of a named attribute from a reference directive line.
fn extract_quoted_attr(line: &str, attr: &str) -> Option<String> {
    extract_quoted_attr_with_offset(line, attr).map(|(text, _)| text)
}

/// Extract path with byte offset of the value start within the line.
fn extract_quoted_path_with_offset(line: &str) -> Option<(String, usize)> {
    extract_quoted_attr_with_offset(line, "path")
}

fn extract_quoted_attr_with_offset(line: &str, attr: &str) -> Option<(String, usize)> {
    extract_quoted_attr_borrowed_with_offset(line, attr, &mut || Ok::<_, ProjectExecutionStop>(()))
        .ok()
        .flatten()
        .map(|(text, offset)| (text.to_owned(), offset))
}

/// The existing extractors and captured discovery share exact attribute-name
/// boundaries, quote handling and offsets. The captured caller meters each step.
fn extract_quoted_attr_borrowed_with_offset<'source, E>(
    line: &'source str,
    attr: &str,
    tick: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<(&'source str, usize)>, E> {
    let mut previous = None;
    let mut quote = None;
    for (idx, ch) in line.char_indices() {
        tick()?;
        if let Some(delimiter) = quote {
            if ch == delimiter {
                quote = None;
            }
            previous = Some(ch);
            continue;
        }
        if matches!(ch, '\'' | '"') {
            quote = Some(ch);
            previous = Some(ch);
            continue;
        }
        let boundary = !previous.is_some_and(is_directive_attr_name_char);
        previous = Some(ch);
        if !boundary || !line[idx..].starts_with(attr) {
            continue;
        }
        let mut pos = idx + attr.len();
        for ch in line[pos..].chars() {
            tick()?;
            if !ch.is_whitespace() {
                break;
            }
            pos += ch.len_utf8();
        }
        if !line[pos..].starts_with('=') {
            continue;
        }
        pos += 1;
        for ch in line[pos..].chars() {
            tick()?;
            if !ch.is_whitespace() {
                break;
            }
            pos += ch.len_utf8();
        }
        let Some(quote @ ('\'' | '"')) = line[pos..].chars().next() else {
            return Ok(None);
        };
        let offset = pos + 1;
        for (end, ch) in line[offset..].char_indices() {
            tick()?;
            if ch == quote {
                return Ok(Some((&line[offset..offset + end], offset)));
            }
        }
        return Ok(None);
    }
    Ok(None)
}

const fn is_directive_attr_name_char(ch: char) -> bool {
    ch == '-' || ch == '_' || ch.is_ascii_alphanumeric()
}

/// Check if a referenced file exists relative to the source file
///
/// Returns true if the file exists, false otherwise.
/// Follows TypeScript's resolution strategy:
/// 1. Try exact path first
/// 2. If no extension or not found, try source extensions supported by options.
pub fn validate_reference_path(source_file: &Path, reference_path: &str, allow_js: bool) -> bool {
    resolve_reference_path_with(source_file, reference_path, allow_js, |path| {
        Ok::<_, std::convert::Infallible>(path.exists())
    })
    .unwrap()
    .is_some()
}

/// Native path-reference probe order, with caller-owned filesystem authority.
/// A missing result is distinct from a provider/cancellation error.
pub fn resolve_reference_path_with<E>(
    source_file: &Path,
    reference_path: &str,
    allow_js: bool,
    mut is_file: impl FnMut(&Path) -> Result<bool, E>,
) -> Result<Option<std::path::PathBuf>, E> {
    if reference_path.is_empty() {
        return Ok(None);
    }

    if let Some(parent) = source_file.parent() {
        let base_path = parent.join(reference_path);

        // Try exact path first
        if is_file(&base_path)? {
            return Ok(Some(base_path));
        }

        // If the filename already has an extension, don't try others.
        // Check the filename part (after last /), not the whole path,
        // since paths like "./idx" contain dots in directory components.
        let has_extension = Path::new(reference_path)
            .file_name()
            .and_then(|f| f.to_str())
            .is_some_and(|f| f.contains('.'));
        if has_extension {
            return Ok(None);
        }

        for ext in reference_path_probe_extensions(allow_js) {
            let path_with_ext = parent.join(format!("{reference_path}{ext}"));
            if is_file(&path_with_ext)? {
                return Ok(Some(path_with_ext));
            }
        }

        Ok(None)
    } else {
        Ok(None)
    }
}

pub const fn reference_path_probe_extensions(allow_js: bool) -> &'static [&'static str] {
    if allow_js {
        &[".ts", ".tsx", ".d.ts", ".js", ".jsx"]
    } else {
        &[".ts", ".tsx", ".d.ts"]
    }
}

#[cfg(test)]
#[path = "../tests/triple_slash_validator.rs"]
mod tests;
