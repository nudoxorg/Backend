//! In use: the real statement a caller writes to use a symbol.
//!
//! Given a caller's source file and its span, [`mine`] skips the caller's
//! signature (everything up to the first line that opens a brace), finds the
//! first body line that names the symbol (`\bname\b`, or `.name`/`::name`
//! for a member), and takes the statement from there until it closes (a
//! line ending in `;`, `{`, `}` or `,`), at most three lines, dedented. The
//! hit is reported as a byte range inside its line so the page can
//! underline exactly the name.
//!
//! This is `page.js`'s `inUse` search, pure and independent of how the
//! source text was fetched.

use std::ops::Range;

/// What to look for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Needle {
    /// The symbol's name.
    pub name: String,
    /// A member: only `.name` or `::name` count (so a method `new` does not
    /// match every `new`).
    pub member: bool,
}

/// A statement that uses the symbol.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Excerpt {
    /// The 1-based line of the hit in the file.
    pub line: u32,
    /// The statement's lines, dedented by their common indent.
    pub lines: Vec<String>,
    /// Which of `lines` holds the hit.
    pub hit: usize,
    /// The name's byte range inside `lines[hit]`.
    pub mark: Range<usize>,
}

impl Excerpt {
    /// The statement as one string.
    #[must_use]
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }
}

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// The byte range of the first match of `needle` in `line` (the name only).
#[must_use]
pub fn find(line: &str, needle: &Needle) -> Option<Range<usize>> {
    let bytes = line.as_bytes();
    let name = needle.name.as_str();
    if name.is_empty() {
        return None;
    }
    let mut from = 0;
    while let Some(off) = line[from..].find(name) {
        let start = from + off;
        let end = start + name.len();
        let before_ok = if needle.member {
            (start >= 1 && bytes[start - 1] == b'.') || (start >= 2 && &bytes[start - 2..start] == b"::")
        } else {
            start == 0 || !is_word(bytes[start - 1])
        };
        let after_ok = end >= bytes.len() || !is_word(bytes[end]);
        if before_ok && after_ok {
            return Some(start..end);
        }
        from = start + 1;
        while from < line.len() && !line.is_char_boundary(from) {
            from += 1;
        }
    }
    None
}

/// Lines the search never takes: comments, attributes and nested item
/// headers.
fn skipped(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("//") || t.starts_with("#[") || t.starts_with("pub fn") || t.starts_with("fn ") || t.starts_with("impl ")
}

fn closes(line: &str) -> bool {
    matches!(line.trim_end().chars().last(), Some(';' | '{' | '}' | ','))
}

/// The statement in `source` that uses `needle`, searched inside the caller
/// spanning lines `start..=end` (1-based, inclusive).
#[must_use]
pub fn mine(source: &str, start: u32, end: u32, needle: &Needle) -> Option<Excerpt> {
    let lines: Vec<&str> = source.split('\n').collect();
    let a = (start.max(1) - 1) as usize;
    let b = (end as usize).min(lines.len());
    if a >= b {
        return None;
    }
    // Skip the signature: it ends at the first line that opens a brace.
    let mut body = a;
    while body + 1 < b && !lines[body].trim_end().ends_with('{') {
        body += 1;
    }
    let at = (body + 1..b).find(|&k| !skipped(lines[k]) && find(lines[k], needle).is_some())?;
    // A tail expression runs into the caller's closing brace; the brace is
    // not part of the statement.
    let last = if b - 1 > at && lines[b - 1].trim() == "}" { b - 2 } else { b - 1 };
    let mut to = at;
    while to < last.min(at + 2) && !closes(lines[to]) {
        to += 1;
    }
    let indent = lines[at..=to]
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    let taken: Vec<String> = lines[at..=to]
        .iter()
        .map(|l| l.get(indent..).unwrap_or_else(|| l.trim_start()).trim_end().to_owned())
        .collect();
    let mark = find(&taken[0], needle)?;
    Some(Excerpt { line: u32::try_from(at + 1).unwrap_or(u32::MAX), lines: taken, hit: 0, mark })
}
