//! Conventional documentation sections, read line by line.
//!
//! Every language marks the parts of a declaration's documentation a reader
//! looks for first (how it fails, what it returns, how to call it) with a
//! convention rather than with syntax: a Markdown heading in Rust
//! (`# Errors`), a block tag in `JSDoc`, `Javadoc`, and `Doxygen` (`@throws`), a
//! header in Python docstrings (`Raises:`, or a `NumPy` header underlined with
//! dashes), a paragraph in Go (`Deprecated:`). One generic model covers them
//! all: a line either opens a [`SectionKind`], opens one entry inside a
//! section (`@throws IOException …`, `ValueError: …`), or is prose.
//!
//! The reader is a pure function of a language and the documentation's own
//! lines, so every surface derives the same structure from the same bytes
//! and nothing extra is stored or hashed.

use crate::language::Language;

/// What one conventional documentation section is about.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SectionKind {
    /// How it reports failure: Rust `# Errors`, `@throws`, `Raises:`,
    /// C# `<exception>`.
    Errors,
    /// When it panics or aborts instead: Rust `# Panics`.
    Panics,
    /// What a caller must uphold: Rust `# Safety`.
    Safety,
    /// Worked examples: `# Examples`, `@example`, `Examples:`.
    Examples,
    /// What it gives back: `@returns`, `Returns:`, C# `<returns>`.
    Returns,
    /// What each parameter means: `@param`, `Args:`, `Parameters`.
    Parameters,
    /// Why not to use it: `@deprecated`, Go's `Deprecated:` paragraph.
    Deprecated,
    /// Any other titled section the author wrote.
    Other,
}

impl SectionKind {
    /// Returns the stable lowercase name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Errors => "errors",
            Self::Panics => "panics",
            Self::Safety => "safety",
            Self::Examples => "examples",
            Self::Returns => "returns",
            Self::Parameters => "parameters",
            Self::Deprecated => "deprecated",
            Self::Other => "other",
        }
    }

    /// Whether the section is a list of named entries (`@throws T`,
    /// `x: int`) rather than running prose.
    #[must_use]
    pub const fn has_entries(self) -> bool {
        matches!(self, Self::Errors | Self::Parameters)
    }
}

/// How one documentation line reads.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LineRole {
    /// Ordinary prose, or the continuation of the current section.
    Prose,
    /// The whole line is a section heading; its text is not prose.
    Heading {
        /// What the section is about.
        kind: SectionKind,
        /// The title as written (`Errors`, `Raises`).
        title: String,
    },
    /// The line opens a section and, when the convention names one, an
    /// entry in it; the prose that follows starts `consumed` bytes in.
    Tag {
        /// What the section is about.
        kind: SectionKind,
        /// The title as written (`throws`, `returns`).
        title: String,
        /// The entry the tag names (`IOException`, `x`), when it names one.
        subject: Option<String>,
        /// Leading bytes of the line that spell the tag and subject.
        consumed: usize,
    },
    /// The line opens one entry of the current section; its prose starts
    /// `consumed` bytes in.
    Entry {
        /// The named entry (`ValueError`, `x`).
        subject: String,
        /// Leading bytes of the line that spell the subject.
        consumed: usize,
    },
    /// A `NumPy` underline (`------`) that belongs to the heading above it.
    Underline,
}

/// Reads one declaration's documentation lines in order, remembering the
/// section each line falls in.
#[derive(Clone, Debug)]
pub struct SectionReader {
    language: Language,
    current: Option<SectionKind>,
    /// Indentation of the header that opened a Python section, so an entry
    /// is recognized only one level inside it.
    header_indent: usize,
    previous_blank: bool,
}

impl SectionReader {
    /// Starts reading the documentation of one declaration in `language`.
    #[must_use]
    pub const fn new(language: Language) -> Self {
        Self {
            language,
            current: None,
            header_indent: 0,
            previous_blank: true,
        }
    }

    /// The section the last line left open, when any.
    #[must_use]
    pub const fn current(&self) -> Option<SectionKind> {
        self.current
    }

    /// Reads one line; `next` is the line after it, when there is one (a
    /// `NumPy` header is recognized by its underline).
    pub fn line(&mut self, line: &str, next: Option<&str>) -> LineRole {
        let role = self.classify(line, next);
        match &role {
            LineRole::Heading { kind, .. } | LineRole::Tag { kind, .. } => {
                self.current = Some(*kind);
                self.header_indent = indent(line);
            }
            LineRole::Prose | LineRole::Entry { .. } | LineRole::Underline => {}
        }
        self.previous_blank = line.trim().is_empty();
        role
    }

    fn uses(&self, family: Family) -> bool {
        match self.language {
            Language::Rust => family == Family::Markdown,
            Language::TypeScript | Language::Java | Language::C | Language::Cxx => {
                family == Family::Tags
            }
            Language::CSharp => family == Family::Tags,
            Language::Python => matches!(family, Family::Docstring | Family::Tags),
            Language::Go => family == Family::GoParagraph,
            Language::Unknown => true,
        }
    }

    fn classify(&self, line: &str, next: Option<&str>) -> LineRole {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return LineRole::Prose;
        }
        if self.uses(Family::Markdown)
            && let Some(role) = markdown_heading(trimmed)
        {
            return role;
        }
        if self.uses(Family::Tags)
            && let Some(role) = block_tag(line)
        {
            return role;
        }
        if self.uses(Family::GoParagraph)
            && self.previous_blank
            && trimmed.starts_with("Deprecated:")
        {
            let consumed = line.len() - line.trim_start().len() + "Deprecated:".len();
            return LineRole::Tag {
                kind: SectionKind::Deprecated,
                title: "Deprecated".to_owned(),
                subject: None,
                consumed,
            };
        }
        if self.uses(Family::Docstring) {
            if let Some(role) = docstring_header(trimmed, next) {
                return role;
            }
            if is_underline(trimmed) {
                return LineRole::Underline;
            }
            if let Some(kind) = self.current
                && kind.has_entries()
                && indent(line) > self.header_indent
                && let Some(role) = docstring_entry(line)
            {
                return role;
            }
            if let Some(kind) = self.current
                && kind.has_entries()
                && indent(line) == self.header_indent
                && next.is_some_and(|next| indent(next) > indent(line))
                && is_identifier_path(trimmed)
            {
                // A `NumPy` entry names its subject alone on a line and
                // indents its description below.
                return LineRole::Entry {
                    subject: trimmed.to_owned(),
                    consumed: line.len(),
                };
            }
        }
        LineRole::Prose
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Family {
    Markdown,
    Tags,
    Docstring,
    GoParagraph,
}

fn indent(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

fn kind_of_title(title: &str) -> SectionKind {
    match title.trim().to_ascii_lowercase().as_str() {
        "errors" | "error" | "raises" | "raise" | "throws" | "throw" | "exceptions"
        | "exception" => SectionKind::Errors,
        "panics" | "panic" => SectionKind::Panics,
        "safety" => SectionKind::Safety,
        "examples" | "example" => SectionKind::Examples,
        "returns" | "return" | "yields" | "yield" => SectionKind::Returns,
        "args" | "arguments" | "parameters" | "params" | "param" | "keyword args"
        | "keyword arguments" | "other parameters" => SectionKind::Parameters,
        "deprecated" => SectionKind::Deprecated,
        _ => SectionKind::Other,
    }
}

/// A Markdown ATX heading of level one to three: `# Errors`.
fn markdown_heading(trimmed: &str) -> Option<LineRole> {
    let hashes = trimmed.bytes().take_while(|byte| *byte == b'#').count();
    if !(1..=3).contains(&hashes) {
        return None;
    }
    let title = trimmed[hashes..].strip_prefix(' ')?.trim().trim_end_matches('#').trim();
    if title.is_empty() {
        return None;
    }
    Some(LineRole::Heading {
        kind: kind_of_title(title),
        title: title.to_owned(),
    })
}

/// A block tag at the start of a line: `@throws {TypeError} when…`,
/// `@param x the input`, `\returns …`, `:raises ValueError: …`.
fn block_tag(line: &str) -> Option<LineRole> {
    let lead = indent(line);
    let rest = &line[lead..];
    if let Some(field) = rest.strip_prefix(':') {
        return sphinx_field(line, lead, field);
    }
    let sigil = rest.chars().next().filter(|c| matches!(c, '@' | '\\'))?;
    let body = &rest[sigil.len_utf8()..];
    let name_end = body
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(body.len());
    let name = &body[..name_end];
    let kind = match name {
        "throws" | "throw" | "exception" | "raises" => SectionKind::Errors,
        "returns" | "return" | "retval" | "yields" => SectionKind::Returns,
        "param" | "arg" | "argument" | "tparam" => SectionKind::Parameters,
        "example" | "examples" => SectionKind::Examples,
        "deprecated" => SectionKind::Deprecated,
        _ => return None,
    };
    let after = &body[name_end..];
    if !(after.is_empty() || after.starts_with(char::is_whitespace) || after.starts_with('[')) {
        return None;
    }
    let mut consumed = lead + sigil.len_utf8() + name_end;
    // `@param[in] x` direction marks belong to the tag.
    let mut after = after;
    if let Some(close) = after.strip_prefix('[').and_then(|inner| inner.find(']')) {
        consumed += close + 2;
        after = &after[close + 2..];
    }
    let subject = if kind.has_entries() {
        let (subject, length) = tag_subject(kind, after)?;
        consumed += length;
        (!subject.is_empty()).then_some(subject)
    } else {
        None
    };
    Some(LineRole::Tag {
        kind,
        title: name.to_owned(),
        subject,
        consumed,
    })
}

/// The entry a tag names and the bytes that spell it, whitespace before it
/// included: `{TypeError}` (a `JSDoc` type), `{string} name` (a typed
/// parameter), or a bare word.
fn tag_subject(kind: SectionKind, after: &str) -> Option<(String, usize)> {
    let skipped = after.len() - after.trim_start().len();
    let word = &after[skipped..];
    let (subject, length) = if let Some(inner) = word.strip_prefix('{') {
        let close = inner.find('}')?;
        let ty = inner[..close].trim().to_owned();
        let tail = &inner[close + 1..];
        let tail_skip = tail.len() - tail.trim_start().len();
        if kind == SectionKind::Parameters {
            let name_len = tail[tail_skip..]
                .find(char::is_whitespace)
                .unwrap_or(tail.len() - tail_skip);
            let name = &tail[tail_skip..tail_skip + name_len];
            (
                if name.is_empty() { ty } else { name.to_owned() },
                close + 2 + tail_skip + name_len,
            )
        } else {
            (ty, close + 2)
        }
    } else {
        let length = word.find(char::is_whitespace).unwrap_or(word.len());
        (word[..length].to_owned(), length)
    };
    Some((subject, skipped + length))
}

/// A Sphinx field list line: `:raises ValueError: when`, `:param x: the`,
/// `:returns: the`.
fn sphinx_field(line: &str, lead: usize, field: &str) -> Option<LineRole> {
    let close = field.find(':')?;
    let head = &field[..close];
    let mut words = head.split_whitespace();
    let name = words.next()?;
    let kind = match name {
        "raises" | "raise" | "except" | "exception" | "throws" => SectionKind::Errors,
        "returns" | "return" | "rtype" | "yields" => SectionKind::Returns,
        "param" | "parameter" | "arg" | "argument" | "key" | "keyword" => {
            SectionKind::Parameters
        }
        _ => return None,
    };
    let subject = words.last().map(str::to_owned);
    let consumed = lead + 1 + close + 1;
    (consumed <= line.len()).then(|| LineRole::Tag {
        kind,
        title: name.to_owned(),
        subject,
        consumed,
    })
}

/// A Google header (`Raises:`) or a `NumPy` header (`Raises` over dashes).
fn docstring_header(trimmed: &str, next: Option<&str>) -> Option<LineRole> {
    let title = if let Some(title) = trimmed.strip_suffix(':') {
        title
    } else if next.is_some_and(|next| is_underline(next.trim())) {
        trimmed
    } else {
        return None;
    };
    if title.is_empty() || title.len() > 24 || !title.chars().all(|c| c.is_alphabetic() || c == ' ')
    {
        return None;
    }
    let kind = kind_of_title(title);
    (kind != SectionKind::Other || next.is_some_and(|next| is_underline(next.trim()))).then(|| {
        LineRole::Heading {
            kind,
            title: title.to_owned(),
        }
    })
}

fn is_underline(trimmed: &str) -> bool {
    trimmed.len() >= 3 && trimmed.bytes().all(|byte| byte == b'-' || byte == b'=')
}

fn is_identifier_path(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '[' | ']' | ','))
}

/// A Google entry: `ValueError: when empty.`, `x (int): the input.`
fn docstring_entry(line: &str) -> Option<LineRole> {
    let lead = indent(line);
    let rest = &line[lead..];
    let colon = rest.find(':')?;
    let head = rest[..colon].trim_end();
    let subject = head.split(" (").next().unwrap_or(head).trim();
    if subject.is_empty() || !is_identifier_path(subject) {
        return None;
    }
    Some(LineRole::Entry {
        subject: subject.to_owned(),
        consumed: lead + colon + 1,
    })
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    /// Renders every line's role the way the assertions read it.
    fn read(language: Language, text: &str) -> Vec<String> {
        let lines: Vec<&str> = text.lines().collect();
        let mut reader = SectionReader::new(language);
        lines
            .iter()
            .enumerate()
            .map(|(at, line)| match reader.line(line, lines.get(at + 1).copied()) {
                LineRole::Prose => format!("prose {:?}", line.trim()),
                LineRole::Heading { kind, title } => format!("heading {} {title}", kind.name()),
                LineRole::Tag {
                    kind,
                    title,
                    subject,
                    consumed,
                } => format!(
                    "tag {} {title} {subject:?} body {:?}",
                    kind.name(),
                    line[consumed..].trim()
                ),
                LineRole::Entry { subject, consumed } => {
                    format!("entry {subject} body {:?}", line[consumed..].trim())
                }
                LineRole::Underline => "underline".to_owned(),
            })
            .collect()
    }

    #[test]
    fn rust_markdown_headings_open_their_sections() {
        assert_eq!(
            read(
                Language::Rust,
                "Parses one.\n\n# Errors\nFails when empty.\n\n# Panics\nNever.\n## Notes\n#[derive]"
            ),
            [
                r#"prose "Parses one.""#,
                r#"prose """#,
                "heading errors Errors",
                r#"prose "Fails when empty.""#,
                r#"prose """#,
                "heading panics Panics",
                r#"prose "Never.""#,
                "heading other Notes",
                r##"prose "#[derive]""##,
            ]
        );
    }

    #[test]
    fn block_tags_name_their_section_and_entry() {
        assert_eq!(
            read(
                Language::TypeScript,
                "Builds one.\n@param {string} name the name\n@returns the thing\n@throws {TypeError} when bad\n@see other"
            ),
            [
                r#"prose "Builds one.""#,
                r#"tag parameters param Some("name") body "the name""#,
                r#"tag returns returns None body "the thing""#,
                r#"tag errors throws Some("TypeError") body "when bad""#,
                r#"prose "@see other""#,
            ]
        );
        assert_eq!(
            read(Language::Java, "@exception IOException if closed\n@deprecated use {@link Fresh}"),
            [
                r#"tag errors exception Some("IOException") body "if closed""#,
                r#"tag deprecated deprecated None body "use {@link Fresh}""#,
            ]
        );
        assert_eq!(
            read(Language::Cxx, "\\throws std::bad_alloc on exhaustion"),
            [r#"tag errors throws Some("std::bad_alloc") body "on exhaustion""#]
        );
        assert_eq!(
            read(Language::CSharp, "Makes one.\n@throws System.ArgumentNullException When nothing is given."),
            [
                r#"prose "Makes one.""#,
                r#"tag errors throws Some("System.ArgumentNullException") body "When nothing is given.""#,
            ]
        );
    }

    #[test]
    fn python_google_numpy_and_sphinx_sections_are_read() {
        assert_eq!(
            read(
                Language::Python,
                "Makes one.\n\nArgs:\n    name (str): the name.\n\nRaises:\n    ValueError: when empty.\n        It says why.\n\nReturns:\n    The thing."
            ),
            [
                r#"prose "Makes one.""#,
                r#"prose """#,
                "heading parameters Args",
                r#"entry name body "the name.""#,
                r#"prose """#,
                "heading errors Raises",
                r#"entry ValueError body "when empty.""#,
                r#"prose "It says why.""#,
                r#"prose """#,
                "heading returns Returns",
                r#"prose "The thing.""#,
            ]
        );
        assert_eq!(
            read(Language::Python, "Raises\n------\nValueError\n    when empty."),
            [
                "heading errors Raises",
                "underline",
                r#"entry ValueError body """#,
                r#"prose "when empty.""#,
            ]
        );
        assert_eq!(
            read(Language::Python, ":raises ValueError: when empty.\n:returns: the thing."),
            [
                r#"tag errors raises Some("ValueError") body "when empty.""#,
                r#"tag returns returns None body "the thing.""#,
            ]
        );
    }

    #[test]
    fn go_reads_only_its_deprecated_paragraph() {
        assert_eq!(
            read(Language::Go, "Old makes one.\n\nDeprecated: use New.\nReturns: never a section."),
            [
                r#"prose "Old makes one.""#,
                r#"prose """#,
                r#"tag deprecated Deprecated None body "use New.""#,
                r#"prose "Returns: never a section.""#,
            ]
        );
    }
}
